use super::*;
use crate::{HoldDisposition, PublicErrorCode};
use std::sync::Mutex;

/// 一个只记账、不真发信的出口：用例要验的是"失败怎么收口"，不是 HTTP。
struct RecordingSink {
    outcome: Result<(), String>,
    sent: Mutex<Vec<PlatformAlert>>,
}

impl RecordingSink {
    fn accepting() -> Self {
        Self {
            outcome: Ok(()),
            sent: Mutex::new(Vec::new()),
        }
    }

    fn refusing() -> Self {
        Self {
            outcome: Err("the alert receiver answered 500".to_owned()),
            sent: Mutex::new(Vec::new()),
        }
    }

    fn sent(&self) -> Vec<PlatformAlert> {
        self.sent.lock().expect("sent lock").clone()
    }
}

#[async_trait]
impl AlertSink for RecordingSink {
    async fn send(&self, alert: &PlatformAlert) -> Result<(), ApplicationError> {
        self.sent.lock().expect("sent lock").push(alert.clone());
        match &self.outcome {
            Ok(()) => Ok(()),
            Err(message) => Err(ApplicationError::Persistence(message.clone())),
        }
    }
}

fn failure(kind: ProviderFailureKind, target_state: JobState) -> AttemptFailure {
    AttemptFailure {
        provider_code: "channel_code".to_owned(),
        public_code: PublicErrorCode::PlatformUnavailable,
        message: "channel message".to_owned(),
        trace_id: None,
        kind,
        target_state,
        hold_disposition: HoldDisposition::Release,
        provider_cost: None,
    }
}

fn alert() -> PlatformAlert {
    PlatformAlert::Execution(ExecutionAlert {
        job_id: JobId::new(),
        provider_kind: "AIHubMix".to_owned(),
        failure_kind: ProviderFailureKind::PlatformFunding,
        occurred_at: Utc::now(),
    })
}

/// 平台欠费与凭证类是**既有的失败类别**，不需要另立一套判据；这两类之外只有"这次失败把 Job
/// 推成对账"才算平台侧事件——渠道限流、消费者内容被拒都不是运维要去修的事。
#[test]
fn only_platform_events_are_alerted_by_the_failure_itself() {
    for kind in [
        ProviderFailureKind::PlatformFunding,
        ProviderFailureKind::PlatformCredential,
    ] {
        assert!(
            is_platform_event(&failure(kind, JobState::Failed)),
            "{kind:?} 是平台侧事件"
        );
    }
    for kind in [
        ProviderFailureKind::UpstreamRejected,
        ProviderFailureKind::UpstreamUnavailable,
        ProviderFailureKind::UpstreamRateLimited,
        ProviderFailureKind::ConsumerContent,
        ProviderFailureKind::Unknown,
    ] {
        assert!(
            !is_platform_event(&failure(kind, JobState::Failed)),
            "{kind:?} 自己不是平台侧事件"
        );
    }
    // 进对账与类别无关：它就是"新建了一条对账案例"的那条路径。
    assert!(is_platform_event(&failure(
        ProviderFailureKind::UpstreamUnavailable,
        JobState::ReconciliationRequired
    )));
}

/// 发送失败**收口在出口里**：调用点拿到的是 `()`，所以它没有机会把一次外发失败变成对主流程的
/// 影响；失败只进日志与计数。
#[tokio::test]
async fn a_refused_delivery_is_counted_and_never_returned_to_the_caller() {
    let refusing = Arc::new(RecordingSink::refusing());
    let alerter = PlatformAlerter::new(refusing.clone());
    alerter.notify(alert()).await;
    assert_eq!(
        alerter.counters(),
        AlertCounters {
            delivered: 0,
            failed: 1
        }
    );
    // 交付本身还是要发生的：被拒的是"送不出去"，不是"没尝试"。
    assert_eq!(refusing.sent().len(), 1);

    let accepting = Arc::new(RecordingSink::accepting());
    let alerter = PlatformAlerter::new(accepting.clone());
    let sent = alert();
    alerter.notify(sent.clone()).await;
    assert_eq!(
        alerter.counters(),
        AlertCounters {
            delivered: 1,
            failed: 0
        }
    );
    assert_eq!(accepting.sent(), vec![sent]);
}

/// 载荷**逐字**就是这四个字段：多一个键就意味着一次外发多带一份仓库里的东西出去。
///
/// 这里同时钉住"不带凭证与对客内容"——那两样根本不在这个结构里，所以序列化结果里也不可能有。
#[test]
fn the_payload_is_exactly_the_four_locating_fields() {
    let alert = alert();
    let PlatformAlert::Execution(execution) = &alert else {
        panic!("这条夹具是执行告警");
    };
    let rendered = serde_json::to_value(&alert).expect("the alert serializes");
    let object = rendered.as_object().expect("the alert is a JSON object");
    let mut keys: Vec<String> = object.keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        ["failure_kind", "job_id", "occurred_at", "provider_kind"]
            .map(str::to_owned)
            .to_vec()
    );
    assert_eq!(
        object["job_id"],
        serde_json::json!(execution.job_id.0.to_string())
    );
    assert_eq!(object["provider_kind"], serde_json::json!("AIHubMix"));
    assert_eq!(
        object["failure_kind"],
        serde_json::json!("platform_funding")
    );
    assert_eq!(
        object["occurred_at"],
        serde_json::json!(execution.occurred_at)
    );
}

/// 账实不符那一种形态的载荷是它自己的四个字段：**没有** `job_id`、`provider_kind`、
/// `failure_kind`——那条余额对不上不属于任何一次执行，收到告警的人也不该去查一个不存在的 Job。
#[test]
fn the_ledger_mismatch_payload_is_its_own_locating_fields() {
    let account_id = AccountId::new();
    let alert = PlatformAlert::ledger_mismatch(account_id, 1_000_000, 999_999);
    let rendered = serde_json::to_value(&alert).expect("the alert serializes");
    let object = rendered.as_object().expect("the alert is a JSON object");
    let mut keys: Vec<String> = object.keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "account_id",
            "balance_microusd",
            "ledger_total_microusd",
            "occurred_at"
        ]
        .map(str::to_owned)
        .to_vec()
    );
    assert_eq!(
        object["account_id"],
        serde_json::json!(account_id.0.to_string())
    );
    assert_eq!(
        object["ledger_total_microusd"],
        serde_json::json!(1_000_000)
    );
    assert_eq!(object["balance_microusd"], serde_json::json!(999_999));
}

/// 阈值是配置项：往回看多少次由它决定，而它不接受 0（0 会把"每一次失败"都当成长度足够的连续段）。
#[test]
fn the_streak_threshold_is_the_configured_one() {
    let alerter = Arc::new(PlatformAlerter::new(Arc::new(RecordingSink::accepting())));
    let exit = PlatformAlertExit::new(alerter, NonZeroU64::new(4).expect("non-zero"));
    assert_eq!(exit.threshold(), 4);
    assert_eq!(exit.window(), 4);
}
