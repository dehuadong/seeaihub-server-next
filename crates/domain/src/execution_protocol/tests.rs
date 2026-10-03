use super::*;
use crate::DomainError;

#[test]
fn protocol_round_trips_through_storage_text() {
    for protocol in [ExecutionProtocol::Legacy, ExecutionProtocol::V1] {
        assert_eq!(ExecutionProtocol::parse(protocol.as_str()), Some(protocol));
        assert_eq!(protocol.to_string(), protocol.as_str());
    }
    assert_eq!(ExecutionProtocol::parse("v2"), None);
}

#[test]
fn stage_round_trips_through_storage_text() {
    for stage in [
        ExecutionStage::Admitted,
        ExecutionStage::Executing,
        ExecutionStage::Succeeded,
        ExecutionStage::Failed,
        ExecutionStage::ReconciliationRequired,
    ] {
        assert_eq!(ExecutionStage::parse(stage.as_str()), Some(stage));
        assert_eq!(stage.to_string(), stage.as_str());
    }
    assert_eq!(ExecutionStage::parse("leased"), None);
}

#[test]
fn stage_walks_the_admitted_to_terminal_path() {
    let executing = ExecutionStage::Admitted
        .transition(ExecutionStage::Executing)
        .expect("admitted may start executing");
    assert_eq!(
        executing
            .transition(ExecutionStage::Succeeded)
            .expect("executing may succeed"),
        ExecutionStage::Succeeded
    );
    assert_eq!(
        ExecutionStage::Admitted
            .transition(ExecutionStage::Failed)
            .expect("an admitted attempt that never left may fail"),
        ExecutionStage::Failed
    );
}

#[test]
fn stage_rejects_skipping_execution_and_leaving_a_terminal_state() {
    assert_eq!(
        ExecutionStage::Admitted.transition(ExecutionStage::Succeeded),
        Err(DomainError::InvalidExecutionStageTransition {
            from: ExecutionStage::Admitted,
            to: ExecutionStage::Succeeded,
        })
    );
    assert!(
        ExecutionStage::Succeeded
            .transition(ExecutionStage::Failed)
            .is_err()
    );
    assert!(
        ExecutionStage::Admitted
            .transition(ExecutionStage::Admitted)
            .is_err()
    );
}

#[test]
fn reconciling_stage_is_not_terminal_and_may_still_settle() {
    let reconciling = ExecutionStage::Executing
        .transition(ExecutionStage::ReconciliationRequired)
        .expect("an unknown acceptance outcome goes to reconciliation");
    assert!(!reconciling.is_terminal());
    assert_eq!(
        reconciling
            .transition(ExecutionStage::Succeeded)
            .expect("late evidence may still settle"),
        ExecutionStage::Succeeded
    );
}

#[test]
fn attempt_stage_round_trips_and_only_terminal_is_terminal() {
    for stage in [
        AttemptStage::Prepared,
        AttemptStage::Submitting,
        AttemptStage::Accepted,
        AttemptStage::Terminal,
        AttemptStage::Unknown,
    ] {
        assert_eq!(AttemptStage::parse(stage.as_str()), Some(stage));
        assert_eq!(stage.to_string(), stage.as_str());
        assert_eq!(stage.is_terminal(), stage == AttemptStage::Terminal);
    }
    assert_eq!(AttemptStage::parse("succeeded"), None);
}

#[test]
fn fencing_token_increments_and_never_wraps() {
    let token = FencingToken::new(7);
    assert_eq!(token.next().expect("7 + 1 fits").get(), 8);
    let last = FencingToken::new(u64::MAX - 1).next().expect("max fits");
    assert_eq!(last.get(), u64::MAX);
    assert_eq!(last.next(), Err(DomainError::ArithmeticOverflow));
}
