//! 连接断开监视：进程唯一的 epoll 线程，在 Hyper 读取之前独立观察对端关闭（RFC 0018 §8.2）。
//!
//! 为什么不能用 service future 的 Drop 或 hyper 自己的 EOF 判定：hyper 1.11 的 HTTP/1 在手里还缓存着
//! 下一条请求的流水线字节时不会再去读 socket（`proto/h1/conn.rs` 的 `mid_message_detect_eof`），
//! 对端 FIN 与已关闭的写半边因此观察不到，慢执行会替一个已经离开的客户端继续跑。
//!
//! 本模块因此在 accept 之后、Hyper 读取之前对一个 **duplicate 的 CLOEXEC fd** 注册 epoll，只订阅
//! `RDHUP | HUP | ERR | ONESHOT`：不订阅 `IN`、不读任何 HTTP 字节，既不与 Hyper 抢读，也能在流水线
//! 字节仍在缓冲时收到关闭事件。eventfd 只用来在控制消息入队后唤醒 epoll 线程，它自己的 `IN` 订阅
//! 与客户端 fd 无关。
//!
//! 线程只做三件事：等 epoll、处理关闭事件、取控制队列。注册走有界控制队列并等待确认，连接收到确认
//! 才开始 Hyper；控制队列为 cleanup 预留容量。释放（DEL + 关闭 duplicate）走同一条队列，并在确认
//! 之前不会认为 duplicate 已经消失；队列满或线程已死时退回本线程直接 DEL，cleanup 因此不依赖队列
//! 容量与 monitor 存活。
//!
//! 事件数据里带的是单调连接 ID 而不是 fd：fd 会被复用，ID 不会。监视表按 ID 索引，旧 ID 的迟到事件
//! 在表里查不到，自然被丢弃。
//!
//! 监视线程致命故障（`epoll_wait` 返回非 `EINTR` 错误）时取消全部在册连接、清空监视表并停止受理新
//! 注册；这正是"关闭相关连接并暂停新准入"。

use std::{
    collections::HashMap,
    os::fd::{AsRawFd, OwnedFd},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{SyncSender, TrySendError, sync_channel},
    },
    thread::JoinHandle,
    time::Duration,
};

use rustix::{
    buffer::spare_capacity,
    event::{
        EventfdFlags,
        epoll::{self, EventData, EventFlags},
    },
    io::Errno,
};
use tokio::sync::oneshot;

use super::DisconnectSignal;

/// 控制队列为释放（cleanup）预留的槽位：注册只能用到其余容量，释放任何时候都进得去。
const RELEASE_RESERVE: usize = 64;

/// eventfd 在 epoll 事件数据里的固定标识；连接 ID 从 1 起，不冲突。
const EVENTFD_ID: u64 = 0;

/// 监视线程的启动配置。
#[derive(Debug, Clone, Copy)]
pub struct MonitorConfig {
    /// 同时在册的连接上限；达到上限后新注册被拒绝。
    pub max_connections: usize,
    /// 控制队列容量；注册可用容量是它减去为释放预留的部分。
    pub control_queue_capacity: usize,
    /// 每次 `epoll_wait` 固定取回的事件条数。
    pub event_batch: usize,
}

/// 监视器不可用的原因：每种都意味着不得启动这条连接。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorError {
    /// 控制队列满或监视表达到连接上限：此刻得不到监视保证。
    Unavailable,
    /// 监视线程已经致命故障：新准入暂停。
    Failed,
    /// 注册确认没有在期限内到达。
    Unresponsive,
}

impl std::fmt::Display for MonitorError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = match self {
            Self::Unavailable => "the disconnect monitor is at capacity",
            Self::Failed => "the disconnect monitor failed",
            Self::Unresponsive => "the disconnect monitor did not confirm the registration in time",
        };
        formatter.write_str(reason)
    }
}

/// 监视线程与连接任务共享的监视表与 epoll 句柄。
struct Shared {
    epoll: OwnedFd,
    eventfd: OwnedFd,
    /// 在册连接，按单调连接 ID 索引；值持有 duplicate fd 的所有权。
    entries: Mutex<HashMap<u64, Entry>>,
    /// 已入队但未被线程取走的控制消息数；注册准入据此为释放留容量。
    queued: AtomicUsize,
    admission_limit: usize,
    max_connections: usize,
    event_batch: usize,
    failed: AtomicBool,
}

struct Entry {
    fd: Arc<OwnedFd>,
    signal: Arc<DisconnectSignal>,
}

enum Control {
    Register {
        fd: Arc<OwnedFd>,
        id: u64,
        signal: Arc<DisconnectSignal>,
        ack: oneshot::Sender<Result<(), MonitorError>>,
    },
    Release {
        id: u64,
        ack: oneshot::Sender<()>,
    },
    Shutdown,
}

/// 进程唯一的连接断开监视器。
pub struct DisconnectMonitor {
    shared: Arc<Shared>,
    control: SyncSender<Control>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl DisconnectMonitor {
    /// 建 epoll、eventfd 与专用线程。配置无意义时返回配置错误字符串，由调用方拒绝启动。
    pub fn start(config: MonitorConfig) -> Result<Arc<Self>, String> {
        if config.max_connections == 0 {
            return Err("the disconnect monitor needs a positive connection limit".to_owned());
        }
        if config.control_queue_capacity <= RELEASE_RESERVE {
            return Err(format!(
                "the disconnect monitor control queue must exceed the {RELEASE_RESERVE} slots \
                 reserved for cleanup"
            ));
        }
        if config.event_batch == 0 {
            return Err("the disconnect monitor needs a positive event batch".to_owned());
        }
        let epoll = epoll::create(epoll::CreateFlags::CLOEXEC)
            .map_err(|error| format!("epoll_create1 failed: {error}"))?;
        let wake = rustix::event::eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)
            .map_err(|error| format!("eventfd failed: {error}"))?;
        epoll::add(
            &epoll,
            &wake,
            EventData::new_u64(EVENTFD_ID),
            EventFlags::IN,
        )
        .map_err(|error| format!("registering the monitor wakeup failed: {error}"))?;
        let shared = Arc::new(Shared {
            epoll,
            eventfd: wake,
            entries: Mutex::new(HashMap::new()),
            queued: AtomicUsize::new(0),
            admission_limit: config.control_queue_capacity - RELEASE_RESERVE,
            max_connections: config.max_connections,
            event_batch: config.event_batch,
            failed: AtomicBool::new(false),
        });
        let (control, receiver) = sync_channel(config.control_queue_capacity);
        let monitor = Arc::new(Self {
            shared: shared.clone(),
            control,
            thread: Mutex::new(None),
        });
        let handle = std::thread::Builder::new()
            .name("api-disconnect-monitor".to_owned())
            .spawn(move || run(&shared, &receiver))
            .map_err(|error| format!("spawning the disconnect monitor thread failed: {error}"))?;
        if let Ok(mut slot) = monitor.thread.lock() {
            *slot = Some(handle);
        }
        Ok(monitor)
    }

    #[must_use]
    pub fn is_failed(&self) -> bool {
        self.shared.failed.load(Ordering::SeqCst)
    }

    /// duplicate fd 入册并等到确认；确认到手后调用方才可以启动这条连接。
    ///
    /// 失败一律意味着这条连接不得开始服务：得不到监视保证时启动连接就会重新落入 RFC 0018 §8.2
    /// 描述的盲区。
    pub async fn register(
        &self,
        fd: Arc<OwnedFd>,
        id: u64,
        signal: Arc<DisconnectSignal>,
        confirm_timeout: Duration,
    ) -> Result<(), MonitorError> {
        if self.is_failed() {
            return Err(MonitorError::Failed);
        }
        // 先计数再入队：入队成功后线程可能立刻取走并减计数，后加会让计数下溢。
        if self.shared.queued.fetch_add(1, Ordering::SeqCst) >= self.shared.admission_limit {
            self.shared.queued.fetch_sub(1, Ordering::SeqCst);
            return Err(MonitorError::Unavailable);
        }
        let (ack, confirmed) = oneshot::channel();
        match self.control.try_send(Control::Register {
            fd,
            id,
            signal,
            ack,
        }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.shared.queued.fetch_sub(1, Ordering::SeqCst);
                return Err(MonitorError::Unavailable);
            }
            Err(TrySendError::Disconnected(_)) => {
                self.shared.queued.fetch_sub(1, Ordering::SeqCst);
                return Err(MonitorError::Failed);
            }
        }
        self.wake();
        match tokio::time::timeout(confirm_timeout, confirmed).await {
            Ok(Ok(Ok(()))) => Ok(()),
            // 线程拒绝了注册，队列里的 fd 随消息一起被丢掉，不需要额外清理。
            Ok(Ok(Err(error))) => Err(error),
            // 线程没回话：注册消息可能还躺在队列里，必须补一次释放，否则队列持有的 duplicate
            // 会让这个 socket 一直活着。
            Ok(Err(_recv)) => {
                self.release(id, confirm_timeout).await;
                Err(MonitorError::Failed)
            }
            Err(_elapsed) => {
                self.release(id, confirm_timeout).await;
                Err(MonitorError::Unresponsive)
            }
        }
    }

    /// 释放监控：DEL duplicate 并确认它已经从监视表移除。
    ///
    /// 走有界控制队列（容量由 `RELEASE_RESERVE` 保证），并等到监视线程确认；队列不可用或线程不回话
    /// 时在本线程直接 DEL，cleanup 因此不会被队列容量或 monitor 存活挡住。
    pub async fn release(&self, id: u64, confirm_timeout: Duration) {
        let (ack, confirmed) = oneshot::channel();
        self.shared.queued.fetch_add(1, Ordering::SeqCst);
        match self.control.try_send(Control::Release { id, ack }) {
            Ok(()) => {
                self.wake();
                if tokio::time::timeout(confirm_timeout, confirmed)
                    .await
                    .is_ok_and(|result| result.is_ok())
                {
                    return;
                }
                // 消息已经入队，计数归线程在取走时处理；这里只在本线程补一次直接释放。
                self.release_direct(id);
            }
            Err(_) => {
                self.shared.queued.fetch_sub(1, Ordering::SeqCst);
                self.release_direct(id);
            }
        }
    }

    /// 停监视线程：取消全部在册连接并清空监视表。已注册连接由连接任务自己收尾。
    pub fn shutdown(&self) {
        self.shared.failed.store(true, Ordering::SeqCst);
        let _ = self.control.try_send(Control::Shutdown);
        self.wake();
        let handle = self.thread.lock().ok().and_then(|mut slot| slot.take());
        if let Some(handle) = handle {
            let _ = handle.join();
        }
        cancel_all_entries(&self.shared);
    }

    /// 在本线程移除监视：从监视表取走并 DEL + 关闭 duplicate。
    ///
    /// 与监视线程的 `epoll_wait` 并发是安全的：`epoll_ctl` 线程安全，表用互斥锁串行；迟到的关闭
    /// 事件按 ID 查不到表项即被丢弃。
    fn release_direct(&self, id: u64) {
        release_one(&self.shared, id);
    }

    fn wake(&self) {
        let _ = rustix::io::write(&self.shared.eventfd, &1u64.to_ne_bytes());
    }
}

/// 监视线程主循环：固定批量的 `epoll_wait` + 取控制队列，直到停机或致命故障。
fn run(shared: &Arc<Shared>, receiver: &std::sync::mpsc::Receiver<Control>) {
    let mut events: Vec<epoll::Event> = Vec::with_capacity(shared.event_batch);
    loop {
        match epoll::wait(&shared.epoll, spare_capacity(&mut events), None) {
            Ok(count) => {
                for event in events[..count].iter() {
                    let id = event.data.u64();
                    if id == EVENTFD_ID {
                        drain_wakeup(&shared.eventfd);
                        continue;
                    }
                    handle_close_event(shared, id);
                }
                events.clear();
                if !drain_control(shared, receiver) {
                    return;
                }
            }
            Err(Errno::INTR) => continue,
            Err(error) => {
                fail(shared, &error);
                return;
            }
        }
    }
}

/// 取空控制队列；返回 `false` 表示停机或通道关闭，线程应退出。
fn drain_control(shared: &Arc<Shared>, receiver: &std::sync::mpsc::Receiver<Control>) -> bool {
    loop {
        match receiver.try_recv() {
            Ok(Control::Register {
                fd,
                id,
                signal,
                ack,
            }) => {
                shared.queued.fetch_sub(1, Ordering::SeqCst);
                let result = register_one(shared, fd, id, signal);
                let _ = ack.send(result);
            }
            Ok(Control::Release { id, ack }) => {
                shared.queued.fetch_sub(1, Ordering::SeqCst);
                release_one(shared, id);
                let _ = ack.send(());
            }
            Ok(Control::Shutdown) => {
                shared.failed.store(true, Ordering::SeqCst);
                cancel_all_entries(shared);
                return false;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => return true,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                shared.failed.store(true, Ordering::SeqCst);
                cancel_all_entries(shared);
                return false;
            }
        }
    }
}

fn register_one(
    shared: &Arc<Shared>,
    fd: Arc<OwnedFd>,
    id: u64,
    signal: Arc<DisconnectSignal>,
) -> Result<(), MonitorError> {
    if shared.failed.load(Ordering::SeqCst) {
        return Err(MonitorError::Failed);
    }
    let mut entries = match shared.entries.lock() {
        Ok(entries) => entries,
        Err(_poisoned) => return Err(MonitorError::Failed),
    };
    if entries.len() >= shared.max_connections {
        return Err(MonitorError::Unavailable);
    }
    if epoll::add(
        &shared.epoll,
        &*fd,
        EventData::new_u64(id),
        close_event_flags(),
    )
    .is_err()
    {
        return Err(MonitorError::Unavailable);
    }
    entries.insert(id, Entry { fd, signal });
    Ok(())
}

fn release_one(shared: &Arc<Shared>, id: u64) {
    let entry = shared
        .entries
        .lock()
        .ok()
        .and_then(|mut entries| entries.remove(&id));
    if let Some(entry) = entry {
        delete_registration(&shared.epoll, &entry.fd);
    }
}

/// 关闭事件：先置取消并通知 owner，再 DEL/drop duplicate。
fn handle_close_event(shared: &Arc<Shared>, id: u64) {
    let entry = shared
        .entries
        .lock()
        .ok()
        .and_then(|mut entries| entries.remove(&id));
    let Some(entry) = entry else {
        return;
    };
    entry.signal.cancel();
    delete_registration(&shared.epoll, &entry.fd);
    let fd = entry.fd.as_raw_fd();
    drop(entry);
    tracing::debug!(connection_id = id, fd, "the client closed the connection");
}

/// 致命故障：取消全部在册连接并停止受理新注册。
fn fail(shared: &Arc<Shared>, error: &Errno) {
    shared.failed.store(true, Ordering::SeqCst);
    tracing::error!(
        error = %error,
        "the disconnect monitor failed; closing the watched connections and refusing new ones"
    );
    cancel_all_entries(shared);
}

fn cancel_all_entries(shared: &Arc<Shared>) {
    let drained: Vec<Entry> = shared
        .entries
        .lock()
        .map(|mut entries| entries.drain().map(|(_, entry)| entry).collect())
        .unwrap_or_default();
    for entry in &drained {
        entry.signal.cancel();
    }
    for entry in drained {
        delete_registration(&shared.epoll, &entry.fd);
    }
}

fn delete_registration(epoll: &OwnedFd, fd: &OwnedFd) {
    // ONESHOT 之后内核已经摘掉兴趣，DEL 返回 ENOENT 是正常路径，不是失败。
    let _ = epoll::delete(epoll, fd);
}

fn close_event_flags() -> EventFlags {
    EventFlags::RDHUP | EventFlags::HUP | EventFlags::ERR | EventFlags::ONESHOT
}

fn drain_wakeup(wake: &OwnedFd) {
    let mut counter = [0u8; 8];
    loop {
        match rustix::io::read(wake, &mut counter) {
            Ok(_) => continue,
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests;
