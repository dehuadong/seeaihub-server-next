use super::*;

#[test]
fn a_failure_disposition_is_derived_by_the_single_public_code_rule() {
    // 结果未知回 outcome_unknown；确定失败与可重试的中间失败都不是未知。
    assert_eq!(
        public_error_code_for_disposition(
            ProviderFailureKind::UpstreamUnavailable,
            FailureDisposition::Unknown
        ),
        PublicErrorCode::OutcomeUnknown
    );
    assert_eq!(
        public_error_code_for_disposition(
            ProviderFailureKind::UpstreamUnavailable,
            FailureDisposition::DeterminedFailure
        ),
        PublicErrorCode::PlatformUnavailable
    );
    assert_eq!(
        public_error_code_for_disposition(
            ProviderFailureKind::Unknown,
            FailureDisposition::SafeRetry
        ),
        PublicErrorCode::PlatformUnavailable
    );
    // 消费者内容被拒优先于重试安全性——与既有 public_error_code 同一条规则，不另立优先级。
    assert_eq!(
        public_error_code_for_disposition(
            ProviderFailureKind::ConsumerContent,
            FailureDisposition::Unknown
        ),
        PublicErrorCode::ContentRejected
    );
}
