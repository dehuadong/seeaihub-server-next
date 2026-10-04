use super::*;

/// 用例里的单次执行预留：真实取值由调用方按 Driver 字节上限算（见 `apps/api/src/main.rs`）。
const EXECUTION_MEMORY_BYTES: usize = 32 * 1024 * 1024;

fn config(execution_slots: usize, max_memory_bytes: usize) -> SupervisorConfig {
    SupervisorConfig {
        execution_slots,
        max_memory_bytes,
        execution_memory_bytes: EXECUTION_MEMORY_BYTES,
        send_slots: 4,
        read_slots: 4,
        slow_read_timeout: Duration::from_secs(1),
        shutdown_grace: Duration::from_secs(1),
        total_deadline: Duration::from_secs(30),
        finalization_grace: Duration::from_secs(1),
        send_window: Duration::from_secs(5),
        ownership: None,
    }
}

#[test]
fn renewal_interval_is_a_third_of_the_lease_with_a_floor() {
    assert_eq!(
        renewal_interval(ChronoDuration::seconds(60)),
        Duration::from_secs(20)
    );
    assert_eq!(
        renewal_interval(ChronoDuration::seconds(1)),
        Duration::from_millis(333)
    );
    assert_eq!(
        renewal_interval(ChronoDuration::zero()),
        Duration::from_millis(1),
        "a zero period would make tokio::time::interval panic"
    );
}

#[test]
fn execution_slots_bound_the_number_of_leases() {
    let supervisor =
        Supervisor::new(config(1, EXECUTION_MEMORY_BYTES * 4)).expect("the supervisor");
    let first = supervisor.try_reserve_execution().expect("the first lease");
    assert!(
        supervisor.try_reserve_execution().is_none(),
        "a second execution must be refused while the single slot is held"
    );
    drop(first);
    assert!(
        supervisor.try_reserve_execution().is_some(),
        "the slot is released"
    );
}

#[test]
fn the_memory_budget_bounds_leases_before_the_slot_count_does() {
    let supervisor = Supervisor::new(config(8, EXECUTION_MEMORY_BYTES)).expect("the supervisor");
    let first = supervisor.try_reserve_execution().expect("the first lease");
    assert!(supervisor.try_reserve_execution().is_none());
    drop(first);
    assert!(supervisor.try_reserve_execution().is_some());
}

#[test]
fn a_budget_below_one_execution_is_a_configuration_error() {
    assert!(Supervisor::new(config(4, EXECUTION_MEMORY_BYTES - 1)).is_err());
}

#[test]
fn send_and_read_slots_are_refused_when_exhausted() {
    let supervisor =
        Supervisor::new(config(4, EXECUTION_MEMORY_BYTES * 4)).expect("the supervisor");
    let mut read_leases = Vec::new();
    for _ in 0..4 {
        read_leases.push(supervisor.try_reserve_read().expect("a read lease"));
    }
    assert!(
        supervisor.try_reserve_read().is_none(),
        "read slots are bounded"
    );
    let mut send_leases = Vec::new();
    for _ in 0..4 {
        send_leases.push(supervisor.try_reserve_send().expect("a send lease"));
    }
    assert!(
        supervisor.try_reserve_send().is_none(),
        "send slots are bounded"
    );
}

/// 观测计数与真实许可同源：许可释放之后每一项都回到零（RFC 0018 §2.3）。
#[test]
fn releasing_every_permit_returns_the_observation_to_zero() {
    let supervisor =
        Supervisor::new(config(4, EXECUTION_MEMORY_BYTES * 4)).expect("the supervisor");
    let read = supervisor.try_reserve_read().expect("a read lease");
    let lease = supervisor
        .try_reserve_execution()
        .expect("an execution lease");
    let send = supervisor.try_reserve_send().expect("a send lease");
    let hold = SendHold::new(
        tokio::time::Instant::now() + Duration::from_secs(5),
        lease,
        send,
        4096,
    );
    let observed = supervisor.observability();
    assert_eq!(observed.reserved_bytes, EXECUTION_MEMORY_BYTES);
    assert_eq!(observed.buffered_bytes, 4096);
    assert_eq!(observed.active_reads, 1);
    assert_eq!(observed.active_executions, 1);
    assert_eq!(observed.active_sends, 1);

    drop(read);
    drop(hold);
    assert_eq!(
        supervisor.observability(),
        ExecutionBudgetSnapshot::default(),
        "each permit's release path must subtract its own count"
    );
}

/// 容量拒绝每次都记数，并且按拒绝的那一段分开记。
#[test]
fn capacity_refusals_are_counted_per_stage() {
    let supervisor = Supervisor::new(config(1, EXECUTION_MEMORY_BYTES)).expect("the supervisor");
    let _lease = supervisor
        .try_reserve_execution()
        .expect("the single execution lease");
    assert!(supervisor.try_reserve_execution().is_none());
    let observed = supervisor.observability();
    assert_eq!(observed.execution_rejections, 1);
    assert_eq!(observed.memory_rejections, 0);

    let mut reads = Vec::new();
    for _ in 0..4 {
        reads.push(supervisor.try_reserve_read().expect("a read lease"));
    }
    assert!(supervisor.try_reserve_read().is_none());
    let mut sends = Vec::new();
    for _ in 0..4 {
        sends.push(supervisor.try_reserve_send().expect("a send lease"));
    }
    assert!(supervisor.try_reserve_send().is_none());
    let observed = supervisor.observability();
    assert_eq!(observed.read_rejections, 1);
    assert_eq!(observed.send_rejections, 1);
    assert_eq!(observed.active_reads, 4);
    assert_eq!(observed.active_sends, 4);
    assert_eq!(observed.active_executions, 1);
}

/// 内存不够时记的是内存拒绝，不是 slot 拒绝。
#[test]
fn a_memory_shortfall_is_counted_as_a_memory_refusal() {
    let supervisor = Supervisor::new(config(4, EXECUTION_MEMORY_BYTES)).expect("the supervisor");
    let _lease = supervisor
        .try_reserve_execution()
        .expect("the single reservable execution");
    assert!(supervisor.try_reserve_execution().is_none());
    let observed = supervisor.observability();
    assert_eq!(observed.execution_rejections, 0);
    assert_eq!(observed.memory_rejections, 1);
}

/// 发送许可与执行预算随 [`SendHold`] 一起被持有：只要还有句柄在，许可就不归零。
///
/// 期限本身由连接层执行；这一层只负责"谁持有、什么时候释放"（RFC 0018 §8.3）。
#[test]
fn a_send_hold_keeps_its_permits_until_the_last_handle_is_dropped() {
    let supervisor =
        Supervisor::new(config(4, EXECUTION_MEMORY_BYTES * 4)).expect("the supervisor");
    let lease = supervisor
        .try_reserve_execution()
        .expect("an execution lease");
    let send = supervisor.try_reserve_send().expect("a send lease");
    assert_eq!(supervisor.send_slots_available(), 3);
    assert_eq!(supervisor.execution_slots_available(), 3);
    let hold = SendHold::new(
        tokio::time::Instant::now() + Duration::from_secs(5),
        lease,
        send,
        4096,
    );
    let handle = hold.clone();
    drop(hold);
    assert_eq!(
        supervisor.send_slots_available(),
        3,
        "a live handle still holds the send permit"
    );
    drop(handle);
    assert_eq!(supervisor.send_slots_available(), 4);
    assert_eq!(supervisor.execution_slots_available(), 4);
    assert_eq!(
        supervisor.memory_bytes_available(),
        EXECUTION_MEMORY_BYTES * 4
    );
}

/// 续约端口固定返回一种错误，用来验证哪一类续约失败会停下新的外部动作。
#[derive(Clone, Copy)]
enum RenewalFailure {
    Conflict,
    Unavailable,
}

impl OwnershipRenewal for RenewalFailure {
    fn renew(
        &self,
        _job_id: JobId,
        _owner: String,
        _fencing_token: FencingToken,
        _lease: ChronoDuration,
    ) -> Pin<Box<dyn Future<Output = Result<(), ApplicationError>> + Send>> {
        let failure = *self;
        Box::pin(async move {
            Err(match failure {
                RenewalFailure::Conflict => {
                    ApplicationError::Conflict("the execution was taken over".to_owned())
                }
                RenewalFailure::Unavailable => {
                    ApplicationError::Configuration("the database is unavailable".to_owned())
                }
            })
        })
    }
}

/// 续约永远不返回，用来验证一次续约超过一个周期也会停下新的外部动作。
struct RenewalHanging;

impl OwnershipRenewal for RenewalHanging {
    fn renew(
        &self,
        _job_id: JobId,
        _owner: String,
        _fencing_token: FencingToken,
        _lease: ChronoDuration,
    ) -> Pin<Box<dyn Future<Output = Result<(), ApplicationError>> + Send>> {
        Box::pin(std::future::pending())
    }
}

/// 起一个续约任务并等到取消标志置位；超时未置位即失败。
async fn renewal_task_cancels(renewal: Arc<dyn OwnershipRenewal>) -> bool {
    let gate = Arc::new(DispatchGate::new());
    let ownership = RenewingOwnership {
        renewal,
        owner_id: "owner-under-test".to_owned(),
        lease: ChronoDuration::zero(),
        gate: gate.clone(),
        stopped: Arc::new(AtomicBool::new(false)),
    };
    ownership.registered(JobId::new(), FencingToken::new(1));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !gate.is_cancelled() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    gate.is_cancelled()
}

#[tokio::test]
async fn any_renewal_error_stops_new_external_actions() {
    let cancelled = renewal_task_cancels(Arc::new(RenewalFailure::Unavailable)).await;
    assert!(
        cancelled,
        "a non-conflict renewal failure must also stop new external actions"
    );
}

#[tokio::test]
async fn a_renewal_conflict_still_stops_new_external_actions() {
    let cancelled = renewal_task_cancels(Arc::new(RenewalFailure::Conflict)).await;
    assert!(cancelled);
}

#[tokio::test]
async fn a_stalled_renewal_stops_new_external_actions() {
    let cancelled = renewal_task_cancels(Arc::new(RenewalHanging)).await;
    assert!(
        cancelled,
        "a renewal that never returns must not keep the execution eligible to act"
    );
}
