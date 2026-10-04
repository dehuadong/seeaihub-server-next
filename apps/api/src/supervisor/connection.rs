//! 自管连接驱动：`TcpListener` accept loop + Hyper auto connection + 连接 owner registry。
//!
//! `axum::serve` 把连接生命周期封在内部，拿不到"这条响应还在不在发"与"连接什么时候真正销毁"，
//! 因此这里用公开接口自己驱动（RFC 0018 §8.1）：
//!
//! - accept 后先对一个 duplicate 的 CLOEXEC fd 做断开注册，**收到监视确认才启动 Hyper**（§8.2）；
//! - 图片响应在交给 Hyper 之前把 [`SendHold`]（发送许可 + 执行 slot 与字节预算 + 绝对期限）移进
//!   本连接的 owner registry。service future 返回 Response 只表示"交给 Hyper"，不释放任何许可；
//! - 同一连接内多个图片响应取**最早**期限，只前移不延长；到期先 `shutdown(Both)` 再销毁连接；
//! - HTTP/1 的图片响应加 `Connection: close`：这条连接写完就结束，许可随连接销毁释放，而不是
//!   随 body 的 `poll_next` 释放；
//! - 每连接一个自定义 [`hyper::rt::Executor`]，把 Hyper spawn 的 stream/service future 归入有界
//!   `JoinSet`；任务组满时拒绝新 task 并关闭整条连接，不排队；
//! - 关闭顺序固定为：closing fence → 取走 JoinSet → `abort_all` → 销毁 connection/IO → join 至空
//!   → 移除关闭监视 → 最后释放 registry 许可。只 abort 或只 drop 主 connection future 都不算
//!   资源归零。
//!
//! 图片路由不接受 `Upgrade`/`CONNECT`：升级后的 IO 会由独立任务持有，绕过这里的任务组跟踪。驱动
//! 不启用 Hyper 的 upgrade 支持，别的路由收到 `Upgrade` 也只当普通请求回一个普通响应，不切协议。
//!
//! 期限与断开都不改写账务事实：这里只销毁 transport 侧资源；连接被观察到断开时置取消这条连接上
//! 在飞执行的动作闸，已提交的 Provider 事实仍由受监督的执行有限收尾。

use std::{
    collections::HashMap,
    error::Error as StdError,
    future::Future,
    os::fd::OwnedFd,
    pin::Pin,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError, Weak,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    extract::{Request, State},
    http::{HeaderValue, Method, StatusCode, Version, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use hyper_util::{rt::TokioIo, server::conn::auto, service::TowerToHyperService};
use rustix::net::Shutdown;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::watch,
    task::JoinSet,
    time::Instant,
};

use super::{
    ConnectionScope, DisconnectSignal, SendHold,
    monitor::{DisconnectMonitor, MonitorConfig},
};

/// 连接层的观测计数（RFC 0018 §2.3）。
///
/// 连接数由 owner 的真实生命周期维护：owner 构造时加、`Drop` 时减，因此快照与 registry 的
/// 在册连接一致。拒绝次数是单调计数。只记数量，不记请求内容。
#[derive(Debug, Default)]
pub struct TransportObservability {
    connections: AtomicUsize,
    connection_rejections: AtomicUsize,
    task_rejections: AtomicUsize,
    monitor_rejections: AtomicUsize,
}

impl TransportObservability {
    /// 当前连接层快照。
    #[must_use]
    pub fn snapshot(&self) -> TransportSnapshot {
        TransportSnapshot {
            connections: self.connections.load(Ordering::Relaxed),
            connection_rejections: self.connection_rejections.load(Ordering::Relaxed),
            task_rejections: self.task_rejections.load(Ordering::Relaxed),
            monitor_rejections: self.monitor_rejections.load(Ordering::Relaxed),
        }
    }

    /// 把当前快照写进 tracing：有界记录，只记数量。
    pub fn record(&self) {
        let snapshot = self.snapshot();
        tracing::info!(
            connections = snapshot.connections,
            connection_rejections = snapshot.connection_rejections,
            task_rejections = snapshot.task_rejections,
            monitor_rejections = snapshot.monitor_rejections,
            "direct gateway transport"
        );
    }
}

/// 某一时刻的连接层快照，字段与 [`TransportObservability`] 的观测项一一对应。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransportSnapshot {
    /// 在册连接数。
    pub connections: usize,
    /// 因连接数达到上限被拒绝的次数。
    pub connection_rejections: usize,
    /// 因每连接任务组满员被拒绝并关闭连接的次数。
    pub task_rejections: usize,
    /// 因断开监视未确认（登记失败或 fd 复制失败）被拒绝的次数。
    pub monitor_rejections: usize,
}

/// 传输层容量与期限：每一项都是明确上限，没有"按需增长"的默认值。
#[derive(Debug, Clone, Copy)]
pub struct TransportConfig {
    /// 同时在册连接数上限；达到上限时新连接直接关闭。
    pub max_connections: usize,
    /// 每连接可同时存在的 Hyper 子任务数上限。
    pub max_connection_tasks: usize,
    /// 断开监视的容量与批量。
    pub monitor: MonitorConfig,
    /// 等待监视注册确认的上限。
    pub registration_timeout: Duration,
    /// 等待监视释放确认的上限。
    pub release_timeout: Duration,
    /// 停机时留给在飞连接的优雅收尾时间。
    pub shutdown_grace: Duration,
    /// HTTP/1 每次请求的最大 header 条数。
    pub http1_max_headers: usize,
    /// HTTP/1 解析缓冲上限（同时限制流水线缓冲）。
    pub http1_max_buf_size: usize,
    /// HTTP/2 每连接最大并发流。
    pub http2_max_concurrent_streams: u32,
    /// HTTP/2 发送缓冲上限。
    pub http2_max_send_buf_size: usize,
    /// HTTP/2 header 列表上限。
    pub http2_max_header_list_size: u32,
}

/// 连接 future 的错误类型。
type ConnectionResult = Result<(), Box<dyn StdError + Send + Sync>>;

/// 起 accept loop，直到 `shutdown` 完成。
///
/// 停机分两段：先停止受理并让在飞连接优雅收尾（Hyper 的 `graceful_shutdown`），到 `shutdown_grace`
/// 仍有连接没结束时强制 `shutdown(Both)` 收尾，最后停掉断开监视线程。
///
/// `observability` 由调用方持有：连接数与拒绝次数写到同一份计数里，供周期性观测读取（RFC 0018 §2.3）。
pub async fn serve(
    listener: TcpListener,
    router: Router,
    config: TransportConfig,
    observability: Arc<TransportObservability>,
    shutdown: impl Future<Output = ()>,
) -> anyhow::Result<()> {
    let monitor = DisconnectMonitor::start(config.monitor).map_err(anyhow::Error::msg)?;
    let registry = Arc::new(Registry::new(monitor, config, observability));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut connections: JoinSet<()> = JoinSet::new();
    let mut shutdown = std::pin::pin!(shutdown);
    let mut next_id: u64 = 1;
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, peer)) => {
                        if registry.live_count() >= config.max_connections {
                            let rejections = registry.observability.connection_rejections
                                .fetch_add(1, Ordering::AcqRel) + 1;
                            tracing::warn!(
                                %peer,
                                connection_rejections = rejections,
                                "refusing a connection: the transport is at its connection limit"
                            );
                            drop(stream);
                            continue;
                        }
                        let id = next_id;
                        next_id += 1;
                        let registry = Arc::clone(&registry);
                        let router = router.clone();
                        let shutdown = shutdown_rx.clone();
                        connections.spawn(async move {
                            run_connection(id, stream, router, registry, shutdown).await;
                        });
                    }
                    Err(error) => {
                        tracing::warn!(error = %error, "accept failed");
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                }
            }
            () = shutdown.as_mut() => break,
            joined = connections.join_next(), if !connections.is_empty() => {
                if let Some(Err(error)) = joined {
                    tracing::warn!(error = %error, "a connection task panicked");
                }
            }
        }
    }
    let _ = shutdown_tx.send(true);
    registry.forget_dead();
    let drained = tokio::time::timeout(config.shutdown_grace, async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    if drained.is_err() {
        let remaining = registry.live_count();
        tracing::warn!(
            remaining,
            "the shutdown grace expired with connections still open; closing them"
        );
        registry.force_close_all();
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            while connections.join_next().await.is_some() {}
        })
        .await;
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    }
    registry.monitor.shutdown();
    Ok(())
}

/// 在册连接。
struct Registry {
    monitor: Arc<DisconnectMonitor>,
    config: TransportConfig,
    observability: Arc<TransportObservability>,
    live: Mutex<HashMap<u64, Weak<ConnectionOwner>>>,
}

impl Registry {
    fn new(
        monitor: Arc<DisconnectMonitor>,
        config: TransportConfig,
        observability: Arc<TransportObservability>,
    ) -> Self {
        Self {
            monitor,
            config,
            observability,
            live: Mutex::new(HashMap::new()),
        }
    }

    fn insert(&self, owner: &Arc<ConnectionOwner>) {
        let mut live = lock(&self.live);
        live.retain(|_, owner| owner.strong_count() > 0);
        live.insert(owner.id, Arc::downgrade(owner));
    }

    fn remove(&self, id: u64) {
        lock(&self.live).remove(&id);
    }

    fn forget_dead(&self) {
        lock(&self.live).retain(|_, owner| owner.strong_count() > 0);
    }

    fn live_count(&self) -> usize {
        let mut live = lock(&self.live);
        live.retain(|_, owner| owner.strong_count() > 0);
        live.len()
    }

    fn force_close_all(&self) {
        let owners: Vec<Arc<ConnectionOwner>> = lock(&self.live)
            .values()
            .filter_map(Weak::upgrade)
            .collect();
        for owner in owners {
            owner.force_close();
        }
    }
}

/// 一条连接的 owner：发送许可、任务组与用于 `shutdown` 的 duplicate fd 都归它。
struct ConnectionOwner {
    id: u64,
    fd: Arc<OwnedFd>,
    signal: Arc<DisconnectSignal>,
    tasks: Mutex<TaskGroup>,
    holds: Mutex<Holds>,
    deadline_moved: tokio::sync::Notify,
    /// 连接数观测：owner 构造时加、`Drop` 时减，与 registry 的在册连接一一对应。
    observability: Arc<TransportObservability>,
}

impl Drop for ConnectionOwner {
    fn drop(&mut self) {
        self.observability
            .connections
            .fetch_sub(1, Ordering::AcqRel);
    }
}

struct TaskGroup {
    set: JoinSet<()>,
    closing: bool,
    capacity: usize,
}

struct Holds {
    /// 关闭闸：置位之后新到的响应不再进入 registry（连接已经在收尾）。
    closed: bool,
    holds: Vec<SendHold>,
    /// 连接级发送期限：取所有在册响应的最早值，只前移不延长。
    deadline: Option<Instant>,
}

impl ConnectionOwner {
    fn new(
        id: u64,
        fd: Arc<OwnedFd>,
        signal: Arc<DisconnectSignal>,
        capacity: usize,
        observability: Arc<TransportObservability>,
    ) -> Self {
        observability.connections.fetch_add(1, Ordering::AcqRel);
        Self {
            id,
            fd,
            signal,
            tasks: Mutex::new(TaskGroup {
                set: JoinSet::new(),
                closing: false,
                capacity,
            }),
            holds: Mutex::new(Holds {
                closed: false,
                holds: Vec::new(),
                deadline: None,
            }),
            deadline_moved: tokio::sync::Notify::new(),
            observability,
        }
    }

    /// 图片响应进入发送阶段：许可与期限交给连接 owner。
    ///
    /// 连接已经在收尾时不再收编：许可立即释放，不能留在一个没人再处理的 registry 里。
    fn register_send(&self, hold: SendHold) {
        let deadline = hold.deadline();
        let moved = {
            let mut holds = lock(&self.holds);
            if holds.closed {
                drop(holds);
                drop(hold);
                return;
            }
            holds.holds.push(hold);
            match holds.deadline {
                Some(current) if current <= deadline => false,
                _ => {
                    holds.deadline = Some(deadline);
                    true
                }
            }
        };
        if moved {
            self.deadline_moved.notify_one();
        }
    }

    fn effective_deadline(&self) -> Option<Instant> {
        lock(&self.holds).deadline
    }

    /// 有界任务组：满员或已关闸时拒绝，不排队。
    fn spawn_task<F>(&self, future: F) -> bool
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let mut tasks = lock(&self.tasks);
        if tasks.closing {
            return false;
        }
        // 先回收已完成项再判容量：完成但没回收的句柄不该顶掉一个名额。
        while tasks.set.try_join_next().is_some() {}
        if tasks.set.len() >= tasks.capacity {
            return false;
        }
        tasks.set.spawn(future);
        true
    }

    /// 关闸并取走任务组；此后 `spawn_task` 一律拒绝。
    fn take_tasks(&self) -> JoinSet<()> {
        let mut tasks = lock(&self.tasks);
        tasks.closing = true;
        std::mem::take(&mut tasks.set)
    }

    fn close(&self) {
        lock(&self.tasks).closing = true;
    }

    /// 强制关闭：先 `shutdown(Both)` 唤醒阻塞的 IO，再置取消让连接任务进入收尾。
    fn force_close(&self) {
        let _ = rustix::net::shutdown(&*self.fd, Shutdown::Both);
        self.signal.cancel();
    }

    /// 最后一步：清空 registry 里的许可与缓冲。
    fn release_holds(&self) {
        let mut holds = lock(&self.holds);
        holds.closed = true;
        holds.holds.clear();
        holds.deadline = None;
    }
}

/// 自定义 Executor：把 Hyper spawn 的子任务收进本连接的有界任务组。
#[derive(Clone)]
struct ConnectionExec {
    owner: Arc<ConnectionOwner>,
}

impl<F> hyper::rt::Executor<F> for ConnectionExec
where
    F: Future<Output = ()> + Send + 'static,
{
    fn execute(&self, future: F) {
        if !self.owner.spawn_task(future) {
            // 任务组满或已关闸：不排队，直接关闭整条连接。
            let rejections = self
                .owner
                .observability
                .task_rejections
                .fetch_add(1, Ordering::AcqRel)
                + 1;
            tracing::warn!(
                connection_id = self.owner.id,
                task_rejections = rejections,
                "the connection task group refused a task; closing the connection"
            );
            self.owner.force_close();
        }
    }
}

/// 驱动一条连接：注册断开监视 → 启动 Hyper → 等结束、期限或断开 → 固定顺序收尾。
async fn run_connection(
    id: u64,
    stream: TcpStream,
    router: Router,
    registry: Arc<Registry>,
    shutdown: watch::Receiver<bool>,
) {
    let config = registry.config;
    let fd = match rustix::io::fcntl_dupfd_cloexec(&stream, 0) {
        Ok(fd) => Arc::new(fd),
        Err(error) => {
            let rejections = registry
                .observability
                .monitor_rejections
                .fetch_add(1, Ordering::AcqRel)
                + 1;
            tracing::warn!(
                connection_id = id,
                error = %error,
                monitor_rejections = rejections,
                "duplicating the socket for the disconnect monitor failed"
            );
            return;
        }
    };
    let signal = DisconnectSignal::new();
    let owner = Arc::new(ConnectionOwner::new(
        id,
        Arc::clone(&fd),
        Arc::clone(&signal),
        config.max_connection_tasks,
        Arc::clone(&registry.observability),
    ));
    registry.insert(&owner);
    if let Err(error) = registry
        .monitor
        .register(
            Arc::clone(&fd),
            id,
            Arc::clone(&signal),
            config.registration_timeout,
        )
        .await
    {
        // 得不到监视保证就不启动连接：这正是 §8.2 要求"收到注册确认后才启动"的原因。
        let rejections = registry
            .observability
            .monitor_rejections
            .fetch_add(1, Ordering::AcqRel)
            + 1;
        tracing::warn!(
            connection_id = id,
            error = %error,
            monitor_rejections = rejections,
            "refusing a connection: the disconnect monitor did not confirm the registration"
        );
        registry.remove(id);
        return;
    }

    let scope = ConnectionScope::new(Arc::clone(&signal));
    let guard_state = GuardState {
        owner: Arc::clone(&owner),
        scope: Arc::clone(&scope),
    };
    let service = TowerToHyperService::new(router.layer(axum::middleware::from_fn_with_state(
        guard_state,
        connection_guard,
    )));
    let mut builder = auto::Builder::new(ConnectionExec {
        owner: Arc::clone(&owner),
    });
    builder
        .http1()
        .max_headers(config.http1_max_headers)
        .max_buf_size(config.http1_max_buf_size);
    builder
        .http2()
        .max_concurrent_streams(config.http2_max_concurrent_streams)
        .max_send_buf_size(config.http2_max_send_buf_size)
        .max_header_list_size(config.http2_max_header_list_size);
    let mut connection = Some(Box::pin(
        builder.serve_connection(TokioIo::new(stream), service),
    ));
    let mut shutdown = shutdown;
    let mut shutting_down = *shutdown.borrow();
    if shutting_down {
        connection
            .as_mut()
            .expect("the connection is live until teardown")
            .as_mut()
            .graceful_shutdown();
    }
    let mut armed: Option<Instant> = None;
    let mut timer: Option<Pin<Box<tokio::time::Sleep>>> = None;
    let outcome = loop {
        let deadline = owner.effective_deadline();
        if deadline != armed {
            armed = deadline;
            timer = deadline.map(|deadline| Box::pin(tokio::time::sleep_until(deadline)));
        }
        let sleeping = async {
            match timer.as_mut() {
                Some(timer) => timer.as_mut().await,
                None => std::future::pending::<()>().await,
            }
        };
        let connection = connection
            .as_mut()
            .expect("the connection is live until teardown");
        tokio::select! {
            result = connection.as_mut() => break Outcome::Finished(result),
            () = signal.cancelled() => break Outcome::ClientGone,
            // HTTP/2 的响应登记发生在 Hyper 派生的 stream 任务里，连接 future 不会因此被唤醒；
            // 期限前移的通知必须自己能叫醒这个循环，否则计时器会停在旧（更晚）的期限上。
            () = owner.deadline_moved.notified() => {}
            () = sleeping => break Outcome::SendDeadline,
            changed = shutdown.changed(), if !shutting_down => {
                if changed.is_err() {
                    // 发送端消失：按停机处理，不再等待。
                    break Outcome::ClientGone;
                }
                shutting_down = true;
                connection.as_mut().graceful_shutdown();
            }
        }
    };
    match outcome {
        Outcome::SendDeadline => {
            // 期限到达先 shutdown(Both)，再销毁连接：让阻塞的读写在销毁前就失败。
            let _ = rustix::net::shutdown(&*owner.fd, Shutdown::Both);
            tracing::warn!(
                connection_id = id,
                "the client send deadline expired; destroying the connection"
            );
        }
        Outcome::ClientGone => {
            // 客户端走了：这条连接上在飞执行的闸只置 client_gone——本进程仍是它们的所有者。
            scope.mark_client_gone();
        }
        Outcome::Finished(Ok(())) => {}
        Outcome::Finished(Err(error)) => {
            tracing::debug!(connection_id = id, error = %error, "the connection ended with an error");
        }
    }
    finish_connection(&registry, &owner, &mut connection).await;
    registry.remove(id);
}

/// 关闭顺序：closing fence → 取走任务组 → abort → 销毁 connection/IO → join 至空 → 移除监视 →
/// 最后释放 registry 许可。
async fn finish_connection<C>(
    registry: &Arc<Registry>,
    owner: &Arc<ConnectionOwner>,
    connection: &mut Option<C>,
) {
    owner.close();
    let mut tasks = owner.take_tasks();
    tasks.abort_all();
    drop(connection.take());
    while tasks.join_next().await.is_some() {}
    drop(tasks);
    registry
        .monitor
        .release(owner.id, registry.config.release_timeout)
        .await;
    owner.release_holds();
}

enum Outcome {
    Finished(ConnectionResult),
    ClientGone,
    SendDeadline,
}

/// 连接中间件的共享状态：owner 收编发送许可，scope 交给 handler 跟踪在飞执行。
#[derive(Clone)]
struct GuardState {
    owner: Arc<ConnectionOwner>,
    scope: Arc<ConnectionScope>,
}

/// 连接级中间件：拒绝图片路由上的 Upgrade/CONNECT，收编图片响应的发送许可并设置 HTTP/1 关闭语义。
async fn connection_guard(
    State(state): State<GuardState>,
    mut request: Request,
    next: Next,
) -> Response {
    if let Some(rejection) = tunnel_rejection(&request) {
        return rejection;
    }
    let http1 = matches!(request.version(), Version::HTTP_11 | Version::HTTP_10);
    request.extensions_mut().insert(Arc::clone(&state.scope));
    let mut response = next.run(request).await;
    if let Some(hold) = response.extensions_mut().remove::<SendHold>() {
        state.owner.register_send(hold);
        if http1 {
            // 图片响应写完就结束这条连接：许可随连接销毁释放，而不是随 body 的 poll_next 释放。
            response
                .headers_mut()
                .insert(header::CONNECTION, HeaderValue::from_static("close"));
        }
    }
    response
}

/// 图片路由不接受隧道与协议升级：升级后的 IO 由独立任务持有，会绕过连接任务组跟踪。
fn tunnel_rejection(request: &Request) -> Option<Response> {
    let image_route = request.uri().path().starts_with("/v1/images/");
    if request.method() == Method::CONNECT {
        return Some(rejection(
            StatusCode::METHOD_NOT_ALLOWED,
            "connect_not_supported",
            "the gateway does not accept CONNECT tunnels",
        ));
    }
    if image_route && wants_upgrade(request) {
        return Some(rejection(
            StatusCode::BAD_REQUEST,
            "upgrade_not_supported",
            "image routes do not accept protocol upgrades",
        ));
    }
    None
}

fn wants_upgrade(request: &Request) -> bool {
    if request.headers().contains_key(header::UPGRADE) {
        return true;
    }
    request
        .headers()
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| value.to_ascii_lowercase().contains("upgrade"))
}

fn rejection(status: StatusCode, code: &'static str, message: &'static str) -> Response {
    (
        status,
        axum::Json(serde_json::json!({ "error": { "code": code, "message": message } })),
    )
        .into_response()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
