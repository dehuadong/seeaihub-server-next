use super::*;

/// 渠道侧的失败一律说成平台侧故障；只有消费者内容被拒才是消费者的错。
#[test]
fn channel_failures_are_reported_as_platform_problems() {
    // (场景, 类别, `retry_safety`, 对客码)
    let rows = [
        (
            "渠道账户欠费：AIHubMix 403 insufficient_user_quota / APIMart 402 payment_required",
            ProviderFailureKind::PlatformFunding,
            RetrySafety::NotRetryable,
            PublicErrorCode::PlatformUnavailable,
        ),
        (
            "渠道侧凭证、权限或渠道被禁用：AIHubMix 403 的其余分支",
            ProviderFailureKind::PlatformCredential,
            RetrySafety::NotRetryable,
            PublicErrorCode::PlatformUnavailable,
        ),
        (
            "渠道用 500 承载的参数错误：APIMart 500 build_request_failed",
            ProviderFailureKind::UpstreamRejected,
            RetrySafety::NotRetryable,
            PublicErrorCode::PlatformUnavailable,
        ),
        (
            "渠道限流",
            ProviderFailureKind::UpstreamRateLimited,
            RetrySafety::SafeBeforeAcceptance,
            PublicErrorCode::PlatformUnavailable,
        ),
        (
            "渠道 5xx、网络中断或响应形状不可用",
            ProviderFailureKind::UpstreamUnavailable,
            RetrySafety::SafeBeforeAcceptance,
            PublicErrorCode::PlatformUnavailable,
        ),
        (
            "拿不准的渠道失败",
            ProviderFailureKind::Unknown,
            RetrySafety::NotRetryable,
            PublicErrorCode::PlatformUnavailable,
        ),
        (
            "受理状态不明：APIMart 409 idempotency_result_indeterminate",
            ProviderFailureKind::Unknown,
            RetrySafety::AcceptanceUnknown,
            PublicErrorCode::OutcomeUnknown,
        ),
        (
            "渠道按幂等子类拒了平台的请求：APIMart 409 idempotency_in_progress / key_reused",
            ProviderFailureKind::UpstreamRejected,
            RetrySafety::AcceptanceUnknown,
            PublicErrorCode::OutcomeUnknown,
        ),
        (
            "渠道不可用且拿不准是否已受理",
            ProviderFailureKind::UpstreamUnavailable,
            RetrySafety::AcceptanceUnknown,
            PublicErrorCode::OutcomeUnknown,
        ),
        (
            "消费者内容被渠道拒绝",
            ProviderFailureKind::ConsumerContent,
            RetrySafety::NotRetryable,
            PublicErrorCode::ContentRejected,
        ),
        (
            "平台自己的问题：租约过期、结果交付失败、配置错误",
            ProviderFailureKind::PlatformInternal,
            RetrySafety::AcceptanceUnknown,
            PublicErrorCode::OutcomeUnknown,
        ),
    ];
    for (scenario, kind, retry_safety, expected) in rows {
        assert_eq!(
            public_error_code(kind, retry_safety),
            expected,
            "场景的对客码不符：{scenario}"
        );
    }
}

/// 对客码只有三个取值：新增类别或新增 `retry_safety` 都不能让第四个值溜出去。
#[test]
fn every_failure_kind_stays_inside_the_public_error_whitelist() {
    let safeties = [
        RetrySafety::SafeBeforeAcceptance,
        RetrySafety::NotRetryable,
        RetrySafety::AcceptanceUnknown,
    ];
    for kind in ProviderFailureKind::ALL {
        for retry_safety in safeties {
            let code = public_error_code(kind, retry_safety);
            assert!(
                PublicErrorCode::parse(code.as_str()).is_some(),
                "{kind:?} 与 {retry_safety:?} 组合出了白名单外的对客码"
            );
            assert!(!code.default_message().is_empty());
        }
    }
}

/// 不传类别时只列平台侧事件：渠道不可用、被限流、消费者内容被拒都不在内。
#[test]
fn the_default_failure_list_covers_only_platform_side_events() {
    assert_eq!(
        default_failure_kinds(),
        vec![
            ProviderFailureKind::PlatformFunding,
            ProviderFailureKind::PlatformCredential,
            ProviderFailureKind::PlatformInternal,
            ProviderFailureKind::UpstreamRejected,
            ProviderFailureKind::Unknown,
        ]
    );
}

/// `content_rejected` 专指消费者的内容被拒：别的类别都不许用它。
#[test]
fn only_consumer_content_becomes_a_consumer_error() {
    let safeties = [
        RetrySafety::SafeBeforeAcceptance,
        RetrySafety::NotRetryable,
        RetrySafety::AcceptanceUnknown,
    ];
    for retry_safety in safeties {
        for kind in ProviderFailureKind::ALL {
            let code = public_error_code(kind, retry_safety);
            if kind == ProviderFailureKind::ConsumerContent {
                assert_eq!(code, PublicErrorCode::ContentRejected);
            } else {
                assert_ne!(code, PublicErrorCode::ContentRejected);
            }
        }
    }
}

/// 落库值与类型必须一一对应：数据库的 CHECK 约束用的是同一批字符串。
#[test]
fn stored_failure_kinds_and_public_codes_round_trip() {
    for kind in ProviderFailureKind::ALL {
        assert_eq!(ProviderFailureKind::parse(kind.as_str()), Some(kind));
    }
    assert_eq!(ProviderFailureKind::parse("provider_error"), None);
    for code in [
        PublicErrorCode::PlatformUnavailable,
        PublicErrorCode::OutcomeUnknown,
        PublicErrorCode::ContentRejected,
    ] {
        assert_eq!(PublicErrorCode::parse(code.as_str()), Some(code));
    }
    assert_eq!(PublicErrorCode::parse("402"), None);
}
