//! API 直接执行的 Supervisor：执行所有权、本机许可、一次性结果通道与有界发送（RFC 0017 §5、§6）。
//!
//! 旧路径（建 Job、Worker 领取、轮询结果）不动；这里的每一件事都只服务新协议的一次同步执行。
//! 一个执行任务的全部资源——执行 slot、内存字节预算、响应发送 permit——由本模块持有，并且
//! **活到响应发送完成**：执行 slot 与内存预算不能一结束就释放，慢客户端占用的这几种许可因此
//! 始终计入上限。Handler 断开只会让 one-shot 结果无处投递，图片随结果一起立即释放；后台任务
//! 不因调用方离开而无限保留结果。
//!
//! 期限分两层：执行侧总期限 D（GENERATION_SYNC_WAIT_SECONDS）由应用层按 D-R 交给 Adapter，
//! 本模块只用 D 作为 Handler 等待上界；客户端发送另有独立的有界期限，由响应 body 自己执行。
//! 停机时先置取消标志，给在飞任务有限收尾时间，残余按应用层的对账路径处置。

use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use axum::body::Body;
use bytes::Bytes;
use chrono::Duration as ChronoDuration;
use futures_util::Stream;
use seeai_adapter_sdk::DispatchGate;
use seeai_application::{
    ApplicationError, DirectExecutionCall, DirectExecutionError, DirectExecutionRequest,
    DirectExecutionService, DirectExecutionSuccess, ExecutionOwnershipRegistrar,
    ExecutionRepository,
};
use seeai_domain::{AccountId, FencingToken, JobId};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, oneshot};
use tokio::time::Instant;
use uuid::Uuid;

#[cfg(target_os = "linux")]
mod connection;
#[cfg(target_os = "linux")]
mod monitor;

#[cfg(target_os = "linux")]
pub use connection::{TransportConfig, TransportObservability, serve};
#[cfg(target_os = "linux")]
pub use monitor::MonitorConfig;

/// 连接对端地址，作为请求扩展挂到每个请求上。
///
/// 这是**连接层**能给的最权威的客户端地址：`X-Forwarded-For` 之类的头由调用方自带，不能直接当
/// 来源。生产在反向代理之后时，由 `apps/api` 按运维配置决定采信哪个受信头，否则退回这里。
/// 类型定义在平台无关的模块里，非 Linux 构建也能编译（那种构建会在启动时明确拒绝服务）。
#[derive(Debug, Clone)]
pub struct ClientAddr(pub Option<std::net::SocketAddr>);

/// 执行所有权续约的装配：续约端口与租约时长。没有它就不起续约任务。
#[derive(Clone)]
pub struct OwnershipRenewalConfig {
    /// 续约走的执行事实端口。
    pub executions: Arc<dyn ExecutionRepository>,
    /// 租约时长；续约间隔是它的三分之一，begin_submission 也按它落 lease_expires_at。
    pub lease: ChronoDuration,
}

/// 本机直接执行的容量与期限配置。字段直接就是对应环境变量的取值，由 main 从环境读出。
#[derive(Clone)]
pub struct SupervisorConfig {
    /// 本机同时在执行的生成任务数。
    pub execution_slots: usize,
    /// 本机在飞执行可预占的内存总量（字节）。
    pub max_memory_bytes: usize,
    /// 单次执行要预留的字节：由各 Driver 声明的字节上限算出的最坏占用。
    pub execution_memory_bytes: usize,
    /// 本机同时可持有的响应发送名额。
    pub send_slots: usize,
    /// 本机同时在读请求正文的准入名额。
    pub read_slots: usize,
    /// 正文从开始接收到读完的上限。
    pub slow_read_timeout: Duration,
    /// 停机的有限排空宽限期。
    pub shutdown_grace: Duration,
    /// Handler 等待执行结果的总期限 D。
    pub total_deadline: Duration,
    /// 收尾宽限：D 到点后，应用层还要有限次确认结算/提交结果并把结论交回。Handler 等到
    /// D 加这一档才把"事实未知"当兜底，好让应用层自己的分类先返回。
    pub finalization_grace: Duration,
    /// 收到结果后把响应发给客户端的独立有界期限。
    pub send_window: Duration,
    /// 执行所有权续约；None 时不续约（测试或不启用直接执行）。
    pub ownership: Option<OwnershipRenewalConfig>,
}

/// 执行内存的字节预算：按预留量增减，超载返回 None，不排队。
struct ByteBudget {
    total: usize,
    used: AtomicUsize,
}

impl ByteBudget {
    fn new(total: usize) -> Arc<Self> {
        Arc::new(Self {
            total,
            used: AtomicUsize::new(0),
        })
    }

    fn try_acquire(self: &Arc<Self>, bytes: usize) -> Option<ByteBudgetPermit> {
        let mut current = self.used.load(Ordering::Relaxed);
        loop {
            let next = current.checked_add(bytes)?;
            if next > self.total {
                return None;
            }
            match self.used.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Some(ByteBudgetPermit {
                        budget: self.clone(),
                        bytes,
                    });
                }
                Err(actual) => current = actual,
            }
        }
    }
}

struct ByteBudgetPermit {
    budget: Arc<ByteBudget>,
    bytes: usize,
}

impl Drop for ByteBudgetPermit {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

/// 本机直接执行的预算观测计数（RFC 0018 §2.3）。
///
/// 每个"持有量"都由真实许可的生命周期维护：许可构造时加、许可 `Drop` 时减，因此快照与真实
/// 持有量一致，没有单独的记账路径可以漂移。拒绝次数是单调计数。只记数量，不记图片或参数。
#[derive(Debug, Default)]
pub struct ExecutionObservability {
    buffered_bytes: AtomicUsize,
    active_reads: AtomicUsize,
    active_executions: AtomicUsize,
    active_sends: AtomicUsize,
    read_rejections: AtomicUsize,
    send_rejections: AtomicUsize,
    execution_rejections: AtomicUsize,
    memory_rejections: AtomicUsize,
}

/// 某一时刻的执行预算快照，字段与 [`ExecutionObservability`] 的观测项一一对应。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExecutionBudgetSnapshot {
    /// 已预留字节：与字节预算的当前占用同源。
    pub reserved_bytes: usize,
    /// 实际缓冲字节：连接层仍持有的响应正文之和。
    pub buffered_bytes: usize,
    /// 活跃的正文读取名额。
    pub active_reads: usize,
    /// 活跃的执行 slot（含已交回结果、仍由发送许可持有的那些）。
    pub active_executions: usize,
    /// 活跃的响应发送名额。
    pub active_sends: usize,
    /// 因读取名额耗尽被拒绝的次数。
    pub read_rejections: usize,
    /// 因发送名额耗尽被拒绝的次数。
    pub send_rejections: usize,
    /// 因执行 slot 耗尽被拒绝的次数。
    pub execution_rejections: usize,
    /// 因内存预算不足被拒绝的次数。
    pub memory_rejections: usize,
}

/// 一次执行占用的 slot 与内存预算。它随结果从执行任务交回 Handler，再随响应 body 存活到发送完成。
pub struct ExecutionLease {
    _slot: OwnedSemaphorePermit,
    _memory: ByteBudgetPermit,
    observability: Arc<ExecutionObservability>,
}

impl Drop for ExecutionLease {
    fn drop(&mut self) {
        self.observability
            .active_executions
            .fetch_sub(1, Ordering::AcqRel);
    }
}

/// 一个响应发送名额。
pub struct SendLease {
    _permit: OwnedSemaphorePermit,
    observability: Arc<ExecutionObservability>,
}

impl Drop for SendLease {
    fn drop(&mut self) {
        self.observability
            .active_sends
            .fetch_sub(1, Ordering::AcqRel);
    }
}

/// 一个本机正文读取名额。
pub struct ReadLease {
    _permit: OwnedSemaphorePermit,
    observability: Arc<ExecutionObservability>,
}

impl Drop for ReadLease {
    fn drop(&mut self) {
        self.observability
            .active_reads
            .fetch_sub(1, Ordering::AcqRel);
    }
}

/// 一个图片响应的发送许可与它实际占用的执行许可，外加绝对发送期限。
///
/// 它由 handler 在结果就绪时构造，随响应扩展交给连接层；连接 owner registry 是它唯一的长期持有者，
/// 只有 transport 任务与缓冲确实销毁之后才释放（RFC 0018 §8.1、§8.3）。放进响应扩展要求可克隆，
/// 因此内部用 `Arc`。
#[derive(Clone)]
pub struct SendHold(Arc<SendHoldInner>);

struct SendHoldInner {
    deadline: Instant,
    /// 这份许可代表的实际缓冲字节（编码后的响应正文长度）：随许可一起被观测与释放。
    buffered_bytes: usize,
    observability: Arc<ExecutionObservability>,
    _lease: ExecutionLease,
    _send: SendLease,
}

impl Drop for SendHoldInner {
    fn drop(&mut self) {
        self.observability
            .buffered_bytes
            .fetch_sub(self.buffered_bytes, Ordering::AcqRel);
    }
}

impl SendHold {
    /// `deadline` 是绝对时刻：不因部分写成功、body 被 poll 或流量波动重置。
    ///
    /// `buffered_bytes` 是这份许可背后实际驻留的响应正文字节数，记进观测而不参与预算许可本身。
    #[must_use]
    pub fn new(
        deadline: Instant,
        lease: ExecutionLease,
        send: SendLease,
        buffered_bytes: usize,
    ) -> Self {
        let observability = Arc::clone(&lease.observability);
        observability
            .buffered_bytes
            .fetch_add(buffered_bytes, Ordering::AcqRel);
        Self(Arc::new(SendHoldInner {
            deadline,
            buffered_bytes,
            observability,
            _lease: lease,
            _send: send,
        }))
    }

    #[must_use]
    pub fn deadline(&self) -> Instant {
        self.0.deadline
    }
}

/// 一次执行的结果接收端与它的外部动作闸。
///
/// 闸交给连接层跟踪：客户端断开被可靠观察到时置 `client_gone`，停止这次执行尚未开始的外部动作；
/// 已经开始的生成发送按"可能已提交"继续有限收尾，不因断开改写账务（Spec 0005 §5）。
pub struct ExecutionHandle {
    pub outcome: oneshot::Receiver<ExecutionOutcome>,
    pub gate: Arc<DispatchGate>,
}

/// 一条连接的取消信号：置位并通知，连接任务据此销毁整条连接。
///
/// 监视线程是同步线程，`cancel` 只做原子写与同步唤醒。
pub struct DisconnectSignal {
    cancelled: AtomicBool,
    notify: Notify,
}

impl DisconnectSignal {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            cancelled: AtomicBool::new(false),
            notify: Notify::new(),
        })
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// 等到取消发生；已经取消时立即返回。
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        // 先注册等待再复查，避免复查与等待之间漏掉一次通知。
        let notified = self.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }
}

/// 一次请求所属连接的作用域：放进 request extension，handler 用它登记这次执行的动作闸。
///
/// 断开是**连接级**事实：观测到客户端关闭时，这条连接上所有在飞执行的闸一起置 `client_gone`。
pub struct ConnectionScope {
    signal: Arc<DisconnectSignal>,
    gates: Mutex<GateTracker>,
}

struct GateTracker {
    next: u64,
    live: HashMap<u64, Weak<DispatchGate>>,
}

impl ConnectionScope {
    #[must_use]
    pub fn new(signal: Arc<DisconnectSignal>) -> Arc<Self> {
        Arc::new(Self {
            signal,
            gates: Mutex::new(GateTracker {
                next: 0,
                live: HashMap::new(),
            }),
        })
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.signal.is_cancelled()
    }

    /// 登记一次执行的动作闸；返回的 guard 在 handler 结束时撤销登记。
    ///
    /// 连接已经被观察到断开时当场置 `client_gone`：登记与置位之间不能留下"断开早于登记"的窗口。
    #[must_use]
    pub fn track(self: &Arc<Self>, gate: &Arc<DispatchGate>) -> ConnectionGateGuard {
        let id = {
            let mut gates = self.gates.lock().unwrap_or_else(|error| error.into_inner());
            gates.next += 1;
            let id = gates.next;
            gates.live.retain(|_, gate| gate.strong_count() > 0);
            gates.live.insert(id, Arc::downgrade(gate));
            id
        };
        if self.is_cancelled() {
            gate.client_gone();
        }
        ConnectionGateGuard {
            scope: Arc::clone(self),
            id,
        }
    }

    /// 连接被观察到断开：这条连接上所有在飞执行只置 `client_gone`。
    ///
    /// 断开是 transport 观察到的事实，不是所有权失效：本进程仍是这些执行的所有者，因此它不碰
    /// `ownership_lost`，也不放弃"本地未发送"的证明（RFC 0018 §4.1）。先置连接信号再逐个
    /// 置闸：此后才登记的闸由 [`Self::track`] 当场补上，不留"断开早于登记"的窗口。
    pub fn mark_client_gone(&self) {
        self.signal.cancel();
        let gates: Vec<Arc<DispatchGate>> = self
            .gates
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .live
            .values()
            .filter_map(Weak::upgrade)
            .collect();
        for gate in gates {
            gate.client_gone();
        }
    }
}

/// 一次执行的闸登记；Drop 即撤销。
pub struct ConnectionGateGuard {
    scope: Arc<ConnectionScope>,
    id: u64,
}

impl Drop for ConnectionGateGuard {
    fn drop(&mut self) {
        self.scope
            .gates
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .live
            .remove(&self.id);
    }
}

/// 执行任务交回 Handler 的结果：载荷或错误，外加必须继续持有的执行许可。
pub struct ExecutionOutcome {
    pub result: Result<DirectExecutionSuccess, DirectExecutionError>,
    pub lease: ExecutionLease,
}

/// 认证后的账户与这次请求的读取名额；由入口中间件放进 request extension。
///
/// 名额包裹成 Arc 是因为 request extension 的值必须可克隆，而信号量 permit 不可克隆。
#[derive(Clone)]
pub struct AuthenticatedAccount {
    pub account_id: AccountId,
    /// 收到请求头的时刻：总期限 D 从这里起算，handler 与执行侧共用同一个起点。
    pub received_at: tokio::time::Instant,
    _read: Arc<ReadLease>,
}

impl AuthenticatedAccount {
    #[must_use]
    pub fn new(account_id: AccountId, received_at: tokio::time::Instant, read: ReadLease) -> Self {
        Self {
            account_id,
            received_at,
            _read: Arc::new(read),
        }
    }
}

/// 正文读取是否触发过有界读超时：由 [`DeadlineBody`] 在超时时置位，handler 据此把
/// 提取器的读错误翻成 408，而不是当成请求格式错误。
#[derive(Clone, Default)]
pub struct SlowRead {
    timed_out: Arc<AtomicBool>,
}

impl SlowRead {
    #[must_use]
    pub fn timed_out(&self) -> bool {
        self.timed_out.load(Ordering::Relaxed)
    }
}

/// 直接执行的 Supervisor。
pub struct Supervisor {
    owner_id: String,
    execution_slots: Arc<Semaphore>,
    send_slots: Arc<Semaphore>,
    read_slots: Arc<Semaphore>,
    memory: Arc<ByteBudget>,
    /// 单次执行预留的字节数（构造时校验过的取值）。
    execution_memory_bytes: usize,
    /// 停机取消的根闸；每次执行从它派生自己的取消/发送闸。
    gate: Arc<DispatchGate>,
    /// 每次执行各自的取消/发送闸；续约失败只置它自己那一份。Weak 不延命执行任务。
    live_executions: Arc<Mutex<HashMap<u64, Weak<DispatchGate>>>>,
    next_execution_id: AtomicU64,
    ownership: Option<OwnershipRenewalConfig>,
    active: Arc<AtomicUsize>,
    idle: Arc<Notify>,
    slow_read_timeout: Duration,
    shutdown_grace: Duration,
    total_deadline: Duration,
    finalization_grace: Duration,
    send_window: Duration,
    /// 预算观测计数：每次取/放许可都在这里留数（RFC 0018 §2.3）。
    observability: Arc<ExecutionObservability>,
}

impl Supervisor {
    /// 装配：容量与期限都必须有实际意义，配不出可用的执行容量时进程起不来。
    pub fn new(config: SupervisorConfig) -> Result<Arc<Self>, ApplicationError> {
        if config.execution_slots == 0 || config.send_slots == 0 || config.read_slots == 0 {
            return Err(ApplicationError::Configuration(
                "direct execution slot counts must be positive".to_owned(),
            ));
        }
        if config.execution_memory_bytes == 0 {
            return Err(ApplicationError::Configuration(
                "the per-execution memory reservation must be positive".to_owned(),
            ));
        }
        if config.max_memory_bytes < config.execution_memory_bytes {
            return Err(ApplicationError::Configuration(format!(
                "GENERATION_MAX_MEMORY_BYTES must cover at least one execution reservation of \
                 {} bytes",
                config.execution_memory_bytes
            )));
        }
        Ok(Arc::new(Self {
            owner_id: format!("api-{}-{}", std::process::id(), Uuid::new_v4()),
            execution_slots: Arc::new(Semaphore::new(config.execution_slots)),
            send_slots: Arc::new(Semaphore::new(config.send_slots)),
            read_slots: Arc::new(Semaphore::new(config.read_slots)),
            memory: ByteBudget::new(config.max_memory_bytes),
            execution_memory_bytes: config.execution_memory_bytes,
            gate: Arc::new(DispatchGate::new()),
            live_executions: Arc::new(Mutex::new(HashMap::new())),
            next_execution_id: AtomicU64::new(0),
            ownership: config.ownership,
            active: Arc::new(AtomicUsize::new(0)),
            idle: Arc::new(Notify::new()),
            slow_read_timeout: config.slow_read_timeout,
            shutdown_grace: config.shutdown_grace,
            total_deadline: config.total_deadline,
            finalization_grace: config.finalization_grace,
            send_window: config.send_window,
            observability: Arc::new(ExecutionObservability::default()),
        }))
    }

    #[must_use]
    pub fn total_deadline(&self) -> Duration {
        self.total_deadline
    }

    #[must_use]
    pub fn send_window(&self) -> Duration {
        self.send_window
    }

    /// 收尾宽限：Handler 在总期限 D 之后再等这么久，给应用层确认结算/提交结果留时间。
    #[must_use]
    pub fn finalization_grace(&self) -> Duration {
        self.finalization_grace
    }

    /// 单测观察口：还剩多少个发送名额。
    #[cfg(test)]
    pub(crate) fn send_slots_available(&self) -> usize {
        self.send_slots.available_permits()
    }

    /// 单测观察口：还剩多少个执行 slot。
    #[cfg(test)]
    pub(crate) fn execution_slots_available(&self) -> usize {
        self.execution_slots.available_permits()
    }

    /// 单测观察口：还剩多少可预占字节。
    #[cfg(test)]
    pub(crate) fn memory_bytes_available(&self) -> usize {
        self.memory.total - self.memory.used.load(Ordering::Relaxed)
    }

    /// 当前预算观测快照：每个持有量都取自真实许可（RFC 0018 §2.3）。
    #[must_use]
    pub fn observability(&self) -> ExecutionBudgetSnapshot {
        ExecutionBudgetSnapshot {
            reserved_bytes: self.memory.used.load(Ordering::Relaxed),
            buffered_bytes: self.observability.buffered_bytes.load(Ordering::Relaxed),
            active_reads: self.observability.active_reads.load(Ordering::Relaxed),
            active_executions: self.observability.active_executions.load(Ordering::Relaxed),
            active_sends: self.observability.active_sends.load(Ordering::Relaxed),
            read_rejections: self.observability.read_rejections.load(Ordering::Relaxed),
            send_rejections: self.observability.send_rejections.load(Ordering::Relaxed),
            execution_rejections: self
                .observability
                .execution_rejections
                .load(Ordering::Relaxed),
            memory_rejections: self.observability.memory_rejections.load(Ordering::Relaxed),
        }
    }

    /// 把当前预算快照写进 tracing：有界记录，只记数量，不记图片或参数（RFC 0018 §2.3）。
    pub fn record_observability(&self) {
        let snapshot = self.observability();
        tracing::info!(
            reserved_bytes = snapshot.reserved_bytes,
            buffered_bytes = snapshot.buffered_bytes,
            active_reads = snapshot.active_reads,
            active_executions = snapshot.active_executions,
            active_sends = snapshot.active_sends,
            read_rejections = snapshot.read_rejections,
            send_rejections = snapshot.send_rejections,
            execution_rejections = snapshot.execution_rejections,
            memory_rejections = snapshot.memory_rejections,
            "direct execution budget"
        );
    }

    /// 取一个本机正文读取名额；满员时拒绝而不是排队。
    #[must_use]
    pub fn try_reserve_read(&self) -> Option<ReadLease> {
        match self.read_slots.clone().try_acquire_owned() {
            Ok(permit) => {
                self.observability
                    .active_reads
                    .fetch_add(1, Ordering::AcqRel);
                Some(ReadLease {
                    _permit: permit,
                    observability: Arc::clone(&self.observability),
                })
            }
            Err(_) => {
                let rejections = self
                    .observability
                    .read_rejections
                    .fetch_add(1, Ordering::AcqRel)
                    + 1;
                tracing::warn!(
                    read_rejections = rejections,
                    "refusing a request body read: the read slots are exhausted"
                );
                None
            }
        }
    }

    /// 取一个响应发送名额；在受理前取，取不到就不执行这次尚未发生费用的请求。
    #[must_use]
    pub fn try_reserve_send(&self) -> Option<SendLease> {
        match self.send_slots.clone().try_acquire_owned() {
            Ok(permit) => {
                self.observability
                    .active_sends
                    .fetch_add(1, Ordering::AcqRel);
                Some(SendLease {
                    _permit: permit,
                    observability: Arc::clone(&self.observability),
                })
            }
            Err(_) => {
                let rejections = self
                    .observability
                    .send_rejections
                    .fetch_add(1, Ordering::AcqRel)
                    + 1;
                tracing::warn!(
                    send_rejections = rejections,
                    "refusing a generation request: the response send slots are exhausted"
                );
                None
            }
        }
    }

    /// 取一次执行的 slot 与内存预算；两者任一不足都拒绝。
    #[must_use]
    pub fn try_reserve_execution(&self) -> Option<ExecutionLease> {
        let slot = match self.execution_slots.clone().try_acquire_owned() {
            Ok(slot) => slot,
            Err(_) => {
                let rejections = self
                    .observability
                    .execution_rejections
                    .fetch_add(1, Ordering::AcqRel)
                    + 1;
                tracing::warn!(
                    execution_rejections = rejections,
                    "refusing an execution: the execution slots are exhausted"
                );
                return None;
            }
        };
        let Some(memory) = self.memory.try_acquire(self.execution_memory_bytes) else {
            let rejections = self
                .observability
                .memory_rejections
                .fetch_add(1, Ordering::AcqRel)
                + 1;
            tracing::warn!(
                memory_rejections = rejections,
                execution_memory_bytes = self.execution_memory_bytes,
                "refusing an execution: the memory budget cannot cover one execution reservation"
            );
            return None;
        };
        self.observability
            .active_executions
            .fetch_add(1, Ordering::AcqRel);
        Some(ExecutionLease {
            _slot: slot,
            _memory: memory,
            observability: Arc::clone(&self.observability),
        })
    }

    /// 起一个执行任务，返回一次性结果接收端。
    ///
    /// 任务自己持有执行许可；结果投递失败（Handler 已断开）时许可随结果一起被丢弃，图片立即释放。
    /// 任务数量受执行 slot 约束——调用方必须先拿到 ExecutionLease，这里不产生无界后台任务。
    pub fn spawn(
        &self,
        service: Arc<DirectExecutionService>,
        request: DirectExecutionRequest,
        lease: ExecutionLease,
        deadline: tokio::time::Instant,
    ) -> ExecutionHandle {
        let (sender, receiver) = oneshot::channel();
        // 每次执行一份自己的取消事实：续约冲突只置这一份的 ownership_lost，不波及其它在飞执行。
        let gate = Arc::new(DispatchGate::new());
        if self.gate.is_stopped() {
            // 停机不是"客户端断开"也不是一次可指名的续约失败：两位一起置，执行不声称本地未发送，
            // 结论交给当前所有者（RFC 0018 §4.1）。
            gate.stop_all();
        }
        let execution_id = self.next_execution_id.fetch_add(1, Ordering::Relaxed);
        let live = self.live_executions.clone();
        if let Ok(mut live_map) = live.lock() {
            live_map.retain(|_, flag| flag.strong_count() > 0);
            live_map.insert(execution_id, Arc::downgrade(&gate));
        }
        let stopped = Arc::new(AtomicBool::new(false));
        let ownership = self.ownership.as_ref().map(|renewal| {
            Arc::new(RenewingOwnership {
                renewal: Arc::new(RepositoryRenewal(renewal.executions.clone())),
                owner_id: self.owner_id.clone(),
                lease: renewal.lease,
                gate: gate.clone(),
                stopped: stopped.clone(),
            }) as Arc<dyn ExecutionOwnershipRegistrar>
        });
        let call = DirectExecutionCall {
            execution_owner: self.owner_id.clone(),
            gate: gate.clone(),
            total_deadline: deadline,
            ownership,
        };
        self.active.fetch_add(1, Ordering::SeqCst);
        let active = self.active.clone();
        let idle = self.idle.clone();
        tokio::spawn(async move {
            let result = service.execute(request, &call).await;
            // 执行结束：续约任务下一跳看到停止标志后自然退出，不再续约已收尾的 Job。
            stopped.store(true, Ordering::SeqCst);
            if let Ok(mut live_map) = live.lock() {
                live_map.remove(&execution_id);
            }
            // 接收端断开时 send 把结果与许可原样退回并丢弃：这正是"断开立即释放图片"。
            let _ = sender.send(ExecutionOutcome { result, lease });
            active.fetch_sub(1, Ordering::SeqCst);
            idle.notify_one();
        });
        ExecutionHandle {
            outcome: receiver,
            gate,
        }
    }

    /// 停机开始：停止新的外部副作用，交给在飞任务有限收尾。
    ///
    /// 停机不是一个可指名的取消原因：它在每次执行的闸上两位一起置（见 [`Supervisor::spawn`]），
    /// 因此停机期间在飞的执行不会声称"本地可证明未发送"。
    pub fn begin_drain(&self) {
        self.gate.stop_all();
        if let Ok(live) = self.live_executions.lock() {
            for flag in live.values() {
                if let Some(flag) = flag.upgrade() {
                    flag.stop_all();
                }
            }
        }
    }

    /// 等在飞任务收尾到宽限期上限；到点仍有残余时不再等——残余事实由应用层的对账路径处置。
    pub async fn drain(&self) {
        if self.active.load(Ordering::SeqCst) == 0 {
            return;
        }
        let active = self.active.clone();
        let idle = self.idle.clone();
        let _ = tokio::time::timeout(self.shutdown_grace, async move {
            while active.load(Ordering::SeqCst) > 0 {
                idle.notified().await;
            }
        })
        .await;
        let remaining = self.active.load(Ordering::SeqCst);
        if remaining > 0 {
            tracing::warn!(
                remaining,
                "the shutdown grace expired with executions still finishing; their facts are \
                 left to reconciliation"
            );
        }
    }

    /// 给请求正文套一个有界读完期限；超时置位共享标志，并把读取错误交给提取器。
    pub fn limit_slow_read(&self, body: Body) -> (Body, SlowRead) {
        let slow = SlowRead::default();
        let stream = DeadlineBody {
            inner: Box::pin(body.into_data_stream()),
            timer: Box::pin(tokio::time::sleep(self.slow_read_timeout)),
            timed_out: slow.timed_out.clone(),
            expired: false,
        };
        (Body::from_stream(stream), slow)
    }
}

/// 请求正文的有界读取流：整段正文必须在期限前读完。
///
/// 每次 poll 都先查计时器，**不只在 Pending 时查**：滴流客户端可以一直有字节可读，
/// 若只在等待分支判期限，读数就永远不到期（RFC 0017 §6）。
struct DeadlineBody {
    inner: Pin<Box<axum::body::BodyDataStream>>,
    timer: Pin<Box<tokio::time::Sleep>>,
    timed_out: Arc<AtomicBool>,
    expired: bool,
}

impl Stream for DeadlineBody {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.expired {
            return Poll::Ready(None);
        }
        if self.timer.as_mut().poll(cx).is_ready() {
            self.expired = true;
            self.timed_out.store(true, Ordering::SeqCst);
            return Poll::Ready(Some(Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "the request body was not read within the configured limit",
            ))));
        }
        match self.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => Poll::Ready(Some(Ok(chunk))),
            Poll::Ready(Some(Err(error))) => {
                Poll::Ready(Some(Err(std::io::Error::other(error.to_string()))))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// 续约执行所有权的最小端口：只暴露"续约一次"，让续约失败策略可独立测试。
trait OwnershipRenewal: Send + Sync {
    fn renew(
        &self,
        job_id: JobId,
        owner: String,
        fencing_token: FencingToken,
        lease: ChronoDuration,
    ) -> Pin<Box<dyn Future<Output = Result<(), ApplicationError>> + Send>>;
}

/// 生产实现：直接调用 Repository 的续约端口。
struct RepositoryRenewal(Arc<dyn ExecutionRepository>);

impl OwnershipRenewal for RepositoryRenewal {
    fn renew(
        &self,
        job_id: JobId,
        owner: String,
        fencing_token: FencingToken,
        lease: ChronoDuration,
    ) -> Pin<Box<dyn Future<Output = Result<(), ApplicationError>> + Send>> {
        let executions = self.0.clone();
        Box::pin(async move {
            executions
                .renew_execution_ownership(job_id, &owner, fencing_token, lease)
                .await
        })
    }
}

/// 一次执行的所有权续约：按租约的三分之一周期在独立任务里续约。
///
/// **任何**续约失败或一次续约超过一个续约周期都立即置这次执行的 `ownership_lost` 并停止新的外部
/// 动作：所有权冲突、数据库不可用、超时都同样意味着"不能再证明还持有所有权"，继续生成就可能与
/// 接管方重复收费（RFC 0017 §5）。置的是 `ownership_lost` 而不是客户端断开：这类执行的收尾要按
/// "只能有限收尾或交还当前所有者"走，不能按旧 token 正式结算（RFC 0018 §4.1）。已到达的句柄或
/// 账务事实仍由执行路径有限收尾，不因取消而丢弃。
struct RenewingOwnership {
    renewal: Arc<dyn OwnershipRenewal>,
    owner_id: String,
    lease: ChronoDuration,
    gate: Arc<DispatchGate>,
    stopped: Arc<AtomicBool>,
}

impl ExecutionOwnershipRegistrar for RenewingOwnership {
    fn registered(&self, job_id: JobId, fencing_token: FencingToken) {
        let renewal = self.renewal.clone();
        let owner = self.owner_id.clone();
        let lease = self.lease;
        let gate = self.gate.clone();
        let stopped = self.stopped.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(renewal_interval(lease));
            // interval 的第一跳立即到点，那不是一次续约周期。
            ticker.tick().await;
            loop {
                ticker.tick().await;
                if stopped.load(Ordering::SeqCst) || gate.is_stopped() {
                    return;
                }
                let attempt = renewal.renew(job_id, owner.clone(), fencing_token, lease);
                match tokio::time::timeout(renewal_interval(lease), attempt).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        gate.ownership_lost();
                        tracing::warn!(
                            job_id = %job_id,
                            error = %error,
                            "execution ownership renewal failed; stopping new external actions"
                        );
                        return;
                    }
                    Err(_elapsed) => {
                        gate.ownership_lost();
                        tracing::warn!(
                            job_id = %job_id,
                            "execution ownership renewal did not finish within one renewal period; stopping new external actions"
                        );
                        return;
                    }
                }
            }
        });
    }
}

/// 续约间隔：租约的三分之一，至少 1ms（interval 不接受零周期）。
fn renewal_interval(lease: ChronoDuration) -> Duration {
    let millis = (lease.num_milliseconds() / 3).max(1);
    Duration::from_millis(u64::try_from(millis).unwrap_or(1))
}

#[cfg(test)]
mod tests;
