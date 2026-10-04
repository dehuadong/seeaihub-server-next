use super::*;
use crate::DomainError;

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

#[test]
fn a_receipt_credential_is_bounded_and_never_printed() {
    let value = "a".repeat(RECEIPT_CREDENTIAL_HEX_LEN);
    let credential = ReceiptCredential::parse(&value).expect("a full hex credential");
    assert_eq!(credential.expose(), value);
    assert_eq!(
        format!("{credential:?}"),
        "ReceiptCredential([REDACTED])",
        "the original value must not reach logs"
    );

    for rejected in [
        String::new(),
        "A".repeat(RECEIPT_CREDENTIAL_HEX_LEN),
        "a".repeat(RECEIPT_CREDENTIAL_HEX_LEN - 1),
        "a".repeat(RECEIPT_CREDENTIAL_HEX_LEN + 1),
        "g".repeat(RECEIPT_CREDENTIAL_HEX_LEN),
    ] {
        assert!(
            ReceiptCredential::parse(&rejected).is_none(),
            "{rejected:?} is not a bounded credential"
        );
    }
}

#[test]
fn a_trace_id_is_bounded_and_never_printed() {
    let trace = ProviderTraceId::parse("req_1a-2B.3:4").expect("a bounded trace id");
    assert_eq!(trace.as_str(), "req_1a-2B.3:4");
    assert_eq!(trace.to_string(), "req_1a-2B.3:4");
    assert_eq!(trace.clone().into_string(), "req_1a-2B.3:4");
    assert_eq!(
        format!("{trace:?}"),
        "ProviderTraceId(13 bytes)",
        "the original value must not reach logs"
    );
}

#[test]
fn a_trace_id_rejects_urls_payloads_controls_and_over_long_values() {
    for rejected in [
        "",
        "https://example.invalid/task/1",
        "data:image/png;base64,AAAA",
        "req id with spaces",
        "line\nbreak",
        "trace\u{7f}",
        "任务标识",
    ] {
        assert!(
            ProviderTraceId::parse(rejected).is_none(),
            "{rejected:?} must not pass as a trace id"
        );
    }
    assert!(
        ProviderTraceId::parse(&"a".repeat(MAX_PROVIDER_IDENTIFIER_BYTES + 1)).is_none(),
        "an over-long value is not a trace id"
    );
    assert!(
        ProviderTraceId::parse(&"a".repeat(MAX_PROVIDER_IDENTIFIER_BYTES)).is_some(),
        "the length limit is inclusive"
    );
}

#[test]
fn a_task_handle_is_the_same_bounded_identifier() {
    let handle =
        ProviderTaskHandle::parse("task_abc-123.4:x".to_owned()).expect("a bounded handle");
    assert_eq!(handle.as_str(), "task_abc-123.4:x");
    assert_eq!(handle.into_string(), "task_abc-123.4:x");

    for rejected in [
        "",
        "https://example.invalid/task/1",
        "data:image/png;base64,AAAA",
        "task\u{0}id",
    ] {
        assert!(
            ProviderTaskHandle::parse(rejected.to_owned()).is_err(),
            "{rejected:?} must not pass as a task handle"
        );
    }
    assert!(
        ProviderTaskHandle::parse("a".repeat(MAX_PROVIDER_IDENTIFIER_BYTES + 1)).is_err(),
        "an over-long value is not a task handle"
    );
}
