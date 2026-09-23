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

/// 平台内部码走的是同一条白名单：`platform_unavailable`，或"结果不明"时的 `outcome_unknown`。
///
/// 这些码由平台自己产生（不经渠道），因此必须逐个钉住——它们是"平台自己的 bug/运维缺口"
/// 唯一对外的说法。
#[test]
fn platform_internal_codes_stay_inside_the_public_error_whitelist() {
    // (平台内部码, 类别, `retry_safety`)——与各写入点的取值一致。
    let rows = [
        (
            "adapter_rejected",
            ProviderFailureKind::PlatformInternal,
            RetrySafety::NotRetryable,
        ),
        (
            "credential_unavailable",
            ProviderFailureKind::PlatformInternal,
            RetrySafety::NotRetryable,
        ),
        (
            "adapter_configuration_failed",
            ProviderFailureKind::PlatformInternal,
            RetrySafety::NotRetryable,
        ),
        (
            "result_delivery_failed",
            ProviderFailureKind::PlatformInternal,
            RetrySafety::AcceptanceUnknown,
        ),
        (
            "worker_lease_expired",
            ProviderFailureKind::PlatformInternal,
            RetrySafety::AcceptanceUnknown,
        ),
        (
            "reconciliation_refunded",
            ProviderFailureKind::PlatformInternal,
            RetrySafety::NotRetryable,
        ),
    ];
    for (code, kind, retry_safety) in rows {
        let public = public_error_code(kind, retry_safety);
        assert!(
            PublicErrorCode::parse(public.as_str()).is_some(),
            "{code} 落到了白名单外：{public:?}"
        );
        assert_ne!(
            public,
            PublicErrorCode::ContentRejected,
            "{code} 是平台自己的问题，不许说成消费者内容被拒"
        );
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

/// 渠道原始码留在内部，对客码独立派生；失败件同样带着成本事实（来源可辨）。
#[test]
fn adapter_failures_keep_the_channel_code_internal() {
    let snapshot = offering().price_snapshot;
    let failure = failure_from_adapter(
        &snapshot,
        AdapterError::Provider(ProviderCallError {
            code: "payment_required".to_owned(),
            message: "account balance is insufficient".to_owned(),
            trace_id: Some("trace-payment".to_owned()),
            retry_safety: RetrySafety::NotRetryable,
            kind: ProviderFailureKind::PlatformFunding,
            // 渠道用错误响应回话，没给金额：失败件落 `unavailable` 进缺口清单，
            // 而不是留 NULL 让这笔成本两头都看不见。
            provider_cost: None,
        }),
    );
    assert_eq!(failure.provider_code, "payment_required");
    assert_eq!(failure.public_code, PublicErrorCode::PlatformUnavailable);
    assert_eq!(failure.kind, ProviderFailureKind::PlatformFunding);
    assert_eq!(failure.target_state, JobState::Failed);
    assert_eq!(failure.hold_disposition, HoldDisposition::Release);
    assert_eq!(
        failure.provider_cost,
        Some(ProviderCostFact {
            source: ProviderCostSource::Unavailable,
            amount_microusd: None,
            currency: None,
            cny_microusd: None,
        }),
        "没有成本事实的失败件按缺口落：来源可辨，不进 NULL"
    );

    let unknown = failure_from_adapter(
        &snapshot,
        AdapterError::Provider(ProviderCallError {
            code: "idempotency_result_indeterminate".to_owned(),
            message: "the request may already be accepted".to_owned(),
            trace_id: None,
            retry_safety: RetrySafety::AcceptanceUnknown,
            kind: ProviderFailureKind::UpstreamRejected,
            provider_cost: None,
        }),
    );
    assert_eq!(unknown.public_code, PublicErrorCode::OutcomeUnknown);
    assert_eq!(unknown.target_state, JobState::ReconciliationRequired);
    assert_eq!(
        unknown.hold_disposition,
        HoldDisposition::RetainForReconciliation
    );

    // 可证明未受理（渠道按第一方依据明确"这次请求没执行"）与确定性拒绝走同一处置：
    // 失败并释放预授权——区别只留在 Attempt 的渠道原始码上。
    let unaccepted = failure_from_adapter(
        &snapshot,
        AdapterError::Provider(ProviderCallError {
            code: "429".to_owned(),
            message: "rate_limit_error".to_owned(),
            trace_id: None,
            retry_safety: RetrySafety::SafeBeforeAcceptance,
            kind: ProviderFailureKind::UpstreamRateLimited,
            provider_cost: None,
        }),
    );
    assert_eq!(unaccepted.provider_code, "429");
    assert_eq!(unaccepted.public_code, PublicErrorCode::PlatformUnavailable);
    assert_eq!(unaccepted.target_state, JobState::Failed);
    assert_eq!(unaccepted.hold_disposition, HoldDisposition::Release);
}

/// 适配器在终态之后判定失败时把已经读到的金额带回来：失败件的成本事实与成功件同源同形，
/// 按**声明值**落库（不是自算值），币种取渠道声明的那一份。
#[test]
fn a_failure_carries_the_cost_the_adapter_already_read() {
    let snapshot = offering().price_snapshot;
    let declared = failure_from_adapter(
        &snapshot,
        AdapterError::Provider(ProviderCallError {
            code: "provider_result_missing".to_owned(),
            message: "completed task carried no image url".to_owned(),
            trace_id: Some("task-declared".to_owned()),
            retry_safety: RetrySafety::AcceptanceUnknown,
            kind: ProviderFailureKind::Unknown,
            provider_cost: Some(ProviderCost::Declared(seeai_adapter_sdk::DeclaredCost {
                amount_microusd: 11_354,
                currency: snapshot
                    .cost_currency()
                    .expect("这条快照带成本币种")
                    .to_owned(),
            })),
        }),
    );
    assert_eq!(
        declared.provider_cost,
        Some(ProviderCostFact {
            source: ProviderCostSource::Declared,
            amount_microusd: Some(11_354),
            currency: Some(
                snapshot
                    .cost_currency()
                    .expect("这条快照带成本币种")
                    .to_owned(),
            ),
            cny_microusd: None,
        }),
        "上游声明的金额直接取它：没有结果图不代表这笔钱没花"
    );
    assert_eq!(
        declared.target_state,
        JobState::ReconciliationRequired,
        "失败件的处置口径不变"
    );
    assert_eq!(declared.trace_id.as_deref(), Some("task-declared"));
}
