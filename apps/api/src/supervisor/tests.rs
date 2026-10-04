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

/// 发送期限只能在这一层"产出下一段之前"生效：到点必须让 body 出错而不是照常交付。
///
/// 底层 socket 写阻塞无法从这里取消，这条用例只固定能闭合的那一段（见 [`GuardedResponseStream`]）。
#[tokio::test]
async fn a_response_past_its_send_window_fails_before_delivering() {
    use futures_util::StreamExt;

    let supervisor =
        Supervisor::new(config(4, EXECUTION_MEMORY_BYTES * 4)).expect("the supervisor");
    let lease = supervisor
        .try_reserve_execution()
        .expect("an execution lease");
    let send = supervisor.try_reserve_send().expect("a send lease");
    let mut stream =
        GuardedResponseStream::new(Bytes::from_static(b"{}"), lease, send, Duration::ZERO);
    let item = stream
        .next()
        .await
        .expect("the stream yields one terminal item");
    assert!(
        item.is_err(),
        "a send window already gone must fail, not deliver the payload"
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
