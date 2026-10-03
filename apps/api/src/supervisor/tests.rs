use super::*;

fn config(execution_slots: usize, max_memory_bytes: usize) -> SupervisorConfig {
    SupervisorConfig {
        execution_slots,
        max_memory_bytes,
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
