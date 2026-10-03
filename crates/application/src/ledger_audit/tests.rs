//! 账实核对按账户的编排用例：发现不符之后**做什么**、一致时**什么都不做**、以及"已经有未结案
//! 案例"时**不重复告警**。
//!
//! 这里用一个只记账的假仓库：真库那侧的语义（比对口径、部分唯一索引挡住重复建案）由端到端的
//! 合同用例在真 PostgreSQL 上验。

use super::*;
use crate::{
    AcceptanceProbe, AccountSummary, ActiveOfferingChannel, AlertSink, ApiKeyView, AttemptFailure,
    BalanceChange, ClaimedJob, CompleteJob, CustomerAccountTarget, CustomerBillingQuery,
    CustomerBillingSummary, CustomerLedgerQuery, CustomerUsageQuery, CustomerUsageView,
    CustomerView, GatewayModelView, JobView, LeaseRecovery, LedgerEntry, LedgerPage, NewFxRate,
    ProviderCostGapView, ProviderFailureQuery, ProviderFailureView, PublishRuntimeRequest,
    ReconciliationCaseView, ReferencedOffering, RefundReconciliationCommand, RoutingDecision,
    SelectableOfferingView, UnacceptedAttempt,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use seeai_domain::{
    AccountId, AttemptId, ChannelId, CreateImageGeneration, FxRate, GenerationJob, ImageBranch,
    JobId, OfferingCandidate, OfferingId, PublishedModel, PublishedOffering, PublishedRevision,
    RoutePolicy,
};
use serde_json::Value;
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

/// 一个只服务账实核对的假仓库：它给一个账户的"对不上"事实（或没有），并记下每次建案的请求。
struct AuditRepository {
    mismatch: Option<LedgerMismatch>,
    /// 建案这次是否**真的插进去了**。已有未结案案例时真库会把插入挡掉，那时这里是 `false`。
    case_opened: bool,
    opened: Mutex<Vec<OpenLedgerCaseCommand>>,
}

impl AuditRepository {
    fn new(mismatch: Option<LedgerMismatch>, case_opened: bool) -> Self {
        Self {
            mismatch,
            case_opened,
            opened: Mutex::new(Vec::new()),
        }
    }

    fn opened(&self) -> Vec<OpenLedgerCaseCommand> {
        self.opened.lock().expect("opened lock").clone()
    }
}

fn unused_repository<T>() -> Result<T, ApplicationError> {
    Err(ApplicationError::Persistence(
        "unused repository operation in ledger audit test".to_owned(),
    ))
}

/// 同 [`unused_repository`]，给 `Option` / `Vec` 这类推不出类型参数的返回用。
fn unused_option<T>() -> Result<Option<T>, ApplicationError> {
    unused_repository()
}

#[async_trait]
impl HubRepository for AuditRepository {
    // 身份与会话那一组端口与本用例无关：账实核对只读账本与余额。真被调到就是用例写错了。
    async fn customer_usage(
        &self,
        _account_id: AccountId,
        _query: CustomerUsageQuery,
    ) -> Result<Vec<CustomerUsageView>, ApplicationError> {
        unused_repository()
    }

    async fn customer_ledger(
        &self,
        _account_id: AccountId,
        _query: CustomerLedgerQuery,
    ) -> Result<LedgerPage, ApplicationError> {
        unused_repository()
    }

    async fn customer_billing(
        &self,
        _account_id: AccountId,
        _query: CustomerBillingQuery,
    ) -> Result<CustomerBillingSummary, ApplicationError> {
        unused_repository()
    }
    async fn list_api_keys(
        &self,
        _account_id: AccountId,
    ) -> Result<Vec<ApiKeyView>, ApplicationError> {
        unused_repository()
    }

    async fn revoke_api_key_of_account(
        &self,
        _account_id: AccountId,
        _key_id: Uuid,
        _actor: &str,
    ) -> Result<bool, ApplicationError> {
        unused_repository()
    }

    async fn current_fx_rates(
        &self,
    ) -> Result<Vec<(String, u64, DateTime<Utc>)>, ApplicationError> {
        unused_repository()
    }

    async fn record_audit(
        &self,
        _actor: &str,
        _action: &str,
        _subject_type: &str,
        _subject_id: &str,
    ) -> Result<(), ApplicationError> {
        unused_repository()
    }

    async fn set_admin_password(
        &self,
        _admin_id: Uuid,
        _password_hash: &str,

        _actor: &str,
    ) -> Result<bool, ApplicationError> {
        unused_repository()
    }

    async fn find_admin_password(
        &self,
        _admin_id: Uuid,
    ) -> Result<Option<String>, ApplicationError> {
        unused_option()
    }

    async fn create_password_reset(
        &self,
        _subject_kind: &str,
        _subject_id: Uuid,
        _token_hash: &str,
        _expires_at: DateTime<Utc>,
    ) -> Result<Uuid, ApplicationError> {
        unused_repository()
    }

    async fn find_password_reset(
        &self,
        _token_hash: &str,
    ) -> Result<Option<(String, Uuid, DateTime<Utc>, Option<DateTime<Utc>>)>, ApplicationError>
    {
        unused_option()
    }

    async fn redeem_password_reset(&self, _token_hash: &str) -> Result<bool, ApplicationError> {
        unused_repository()
    }

    async fn find_customer_password(
        &self,
        _customer_id: Uuid,
    ) -> Result<Option<String>, ApplicationError> {
        unused_option()
    }

    async fn set_customer_password(
        &self,
        _customer_id: Uuid,
        _password_hash: &str,

        _actor: &str,
    ) -> Result<bool, ApplicationError> {
        unused_repository()
    }

    async fn open_customer_account(
        &self,
        _email: &str,
        _password_hash: &str,
        _target: CustomerAccountTarget,
    ) -> Result<(Uuid, Uuid), ApplicationError> {
        unused_repository()
    }

    async fn find_customer_view(
        &self,
        _email: &str,
    ) -> Result<Option<CustomerView>, ApplicationError> {
        unused_option()
    }

    async fn find_customer_view_by_id(
        &self,
        _customer_id: Uuid,
    ) -> Result<Option<CustomerView>, ApplicationError> {
        unused_option()
    }

    async fn list_customers(&self, _limit: u32) -> Result<Vec<CustomerView>, ApplicationError> {
        unused_repository()
    }

    async fn find_admin_by_email(
        &self,
        _email: &str,
    ) -> Result<Option<(Uuid, String)>, ApplicationError> {
        unused_repository()
    }

    async fn ensure_admin_account(
        &self,
        _email: &str,
        _password_hash: &str,
    ) -> Result<(Uuid, bool), ApplicationError> {
        unused_repository()
    }

    async fn upsert_admin_password(
        &self,
        _email: &str,
        _password_hash: &str,
    ) -> Result<Uuid, ApplicationError> {
        unused_repository()
    }

    async fn record_admin_login(&self, _admin_id: Uuid) -> Result<(), ApplicationError> {
        unused_repository()
    }

    async fn create_admin_session(
        &self,
        _admin_id: Uuid,
        _token_hash: &str,
        _expires_at: DateTime<Utc>,
    ) -> Result<Uuid, ApplicationError> {
        unused_repository()
    }

    async fn find_admin_session(
        &self,
        _token_hash: &str,
    ) -> Result<Option<(Uuid, String, DateTime<Utc>)>, ApplicationError> {
        unused_repository()
    }

    async fn delete_admin_session(&self, _token_hash: &str) -> Result<(), ApplicationError> {
        unused_repository()
    }

    async fn create_customer(
        &self,
        _account_id: AccountId,
        _account_name: &str,
        _email: &str,
        _password_hash: &str,
    ) -> Result<Uuid, ApplicationError> {
        unused_repository()
    }

    async fn find_customer_by_email(
        &self,
        _email: &str,
    ) -> Result<Option<(Uuid, Uuid, String)>, ApplicationError> {
        unused_repository()
    }

    async fn find_customer_by_account(
        &self,
        _account_id: Uuid,
    ) -> Result<Option<Uuid>, ApplicationError> {
        unused_option()
    }

    async fn find_customer_account(
        &self,
        _customer_id: Uuid,
    ) -> Result<Option<Uuid>, ApplicationError> {
        unused_repository()
    }

    async fn record_customer_login(&self, _customer_id: Uuid) -> Result<(), ApplicationError> {
        unused_repository()
    }

    async fn create_customer_session(
        &self,
        _customer_id: Uuid,
        _token_hash: &str,
        _expires_at: DateTime<Utc>,
    ) -> Result<Uuid, ApplicationError> {
        unused_repository()
    }

    async fn find_customer_session(
        &self,
        _token_hash: &str,
    ) -> Result<Option<(Uuid, Uuid, DateTime<Utc>)>, ApplicationError> {
        unused_repository()
    }

    async fn delete_customer_session(&self, _token_hash: &str) -> Result<(), ApplicationError> {
        unused_repository()
    }

    async fn publish_runtime(
        &self,
        _request: PublishRuntimeRequest,
    ) -> Result<PublishedRevision, ApplicationError> {
        unused_repository()
    }

    async fn active_offering(
        &self,
        _native_model_id: &str,
    ) -> Result<Vec<OfferingCandidate>, ApplicationError> {
        unused_repository()
    }

    async fn active_offering_channels(
        &self,
        _gateway_model: &str,
    ) -> Result<Vec<ActiveOfferingChannel>, ApplicationError> {
        unused_repository()
    }

    async fn offerings_by_id(
        &self,
        _offering_ids: &[OfferingId],
    ) -> Result<Vec<ReferencedOffering>, ApplicationError> {
        unused_repository()
    }

    async fn enabled_offerings(
        &self,
        _offering_ids: &[OfferingId],
    ) -> Result<HashSet<OfferingId>, ApplicationError> {
        unused_repository()
    }

    async fn selectable_offerings(&self) -> Result<Vec<SelectableOfferingView>, ApplicationError> {
        unused_repository()
    }

    async fn published_models(&self) -> Result<Vec<PublishedModel>, ApplicationError> {
        unused_repository()
    }

    async fn gateway_models(&self) -> Result<Vec<GatewayModelView>, ApplicationError> {
        unused_repository()
    }

    async fn set_gateway_model_enabled(
        &self,
        _gateway_model: &str,
        _enabled: bool,
        _actor: &str,
    ) -> Result<(), ApplicationError> {
        unused_repository()
    }

    async fn set_offering_enabled(
        &self,
        _offering_id: OfferingId,
        _enabled: bool,
        _actor: &str,
    ) -> Result<Vec<String>, ApplicationError> {
        unused_repository()
    }

    async fn set_channel_enabled(
        &self,
        _channel_id: ChannelId,
        _enabled: bool,
        _actor: &str,
    ) -> Result<Vec<String>, ApplicationError> {
        unused_repository()
    }

    async fn upsert_fx_rate(&self, _rate: NewFxRate, _actor: &str) -> Result<(), ApplicationError> {
        unused_repository()
    }

    async fn effective_fx_rate(&self, _currency: &str) -> Result<Option<FxRate>, ApplicationError> {
        unused_repository()
    }

    async fn provider_cost_gaps(
        &self,
        _limit: u32,
    ) -> Result<Vec<ProviderCostGapView>, ApplicationError> {
        unused_repository()
    }

    async fn acceptance_probe(
        &self,
        _gateway_model: &str,
        _account_id: AccountId,
        _idempotency_key: &str,
    ) -> Result<AcceptanceProbe, ApplicationError> {
        unused_repository()
    }

    async fn accounts_updated_within(
        &self,
        _window: Duration,
    ) -> Result<Vec<BalanceChange>, ApplicationError> {
        unused_repository()
    }

    async fn insert_audit_event(
        &self,
        _actor: &str,
        _action: &str,
        _subject_type: &str,
        _subject_id: &str,
        _payload: Value,
    ) -> Result<(), ApplicationError> {
        unused_repository()
    }

    async fn create_account(
        &self,
        _account_id: AccountId,
        _name: &str,
        _tag: Option<&str>,
        _initial_credit_microusd: u64,
        _actor: &str,
    ) -> Result<BalanceChange, ApplicationError> {
        unused_repository()
    }

    async fn set_account_name(
        &self,
        _account_id: AccountId,
        _name: &str,
        _actor: &str,
    ) -> Result<(), ApplicationError> {
        unused_repository()
    }

    async fn credit_account(
        &self,
        _account_id: AccountId,
        _amount_microusd: u64,
        _business_key: &str,
        _actor: &str,
    ) -> Result<BalanceChange, ApplicationError> {
        unused_repository()
    }

    async fn read_account_balance(
        &self,
        _account_id: AccountId,
    ) -> Result<BalanceChange, ApplicationError> {
        unused_repository()
    }

    async fn list_accounts(
        &self,
        _email: Option<&str>,
        _tag: Option<&str>,
        _name: Option<&str>,
        _limit: u32,
    ) -> Result<Vec<AccountSummary>, ApplicationError> {
        unused_repository()
    }

    async fn find_account_summary(
        &self,
        _account_id: AccountId,
    ) -> Result<Option<AccountSummary>, ApplicationError> {
        unused_option()
    }

    async fn read_ledger_entries(
        &self,
        _account_id: AccountId,
        _since: Option<chrono::DateTime<chrono::Utc>>,
        _until: Option<chrono::DateTime<chrono::Utc>>,
        _kind: Option<&str>,
        _offset: u32,
        _limit: u32,
    ) -> Result<Vec<LedgerEntry>, ApplicationError> {
        unused_repository()
    }

    async fn count_ledger_entries(
        &self,
        _account_id: AccountId,
        _since: Option<chrono::DateTime<chrono::Utc>>,
        _until: Option<chrono::DateTime<chrono::Utc>>,
        _kind: Option<&str>,
    ) -> Result<u64, ApplicationError> {
        unused_repository()
    }

    async fn held_microusd(&self, _account_id: AccountId) -> Result<i64, ApplicationError> {
        unused_repository()
    }

    async fn probe(&self) -> Result<(), ApplicationError> {
        unused_repository()
    }

    async fn account_tag(
        &self,
        _account_id: AccountId,
    ) -> Result<Option<String>, ApplicationError> {
        unused_repository()
    }

    async fn set_account_tag(
        &self,
        _account_id: AccountId,
        _tag: Option<&str>,
        _actor: &str,
    ) -> Result<(), ApplicationError> {
        unused_repository()
    }

    async fn route_policy(
        &self,
        _gateway_model: &str,
    ) -> Result<Option<RoutePolicy>, ApplicationError> {
        unused_repository()
    }

    async fn upsert_route_policy(
        &self,
        _policy: &RoutePolicy,
        _actor: &str,
    ) -> Result<(), ApplicationError> {
        unused_repository()
    }

    async fn route_policies(&self) -> Result<Vec<RoutePolicy>, ApplicationError> {
        unused_repository()
    }

    async fn create_api_key(
        &self,
        _account_id: AccountId,
        _label: &str,
        _key_hash: &str,
        _actor: &str,
    ) -> Result<Uuid, ApplicationError> {
        unused_repository()
    }

    async fn revoke_api_key(&self, _key_id: Uuid, _actor: &str) -> Result<(), ApplicationError> {
        unused_repository()
    }

    async fn api_key_identity(
        &self,
        _key_hash: &str,
    ) -> Result<(Uuid, AccountId), ApplicationError> {
        unused_repository()
    }

    async fn create_job(
        &self,
        _command: CreateImageGeneration,
        _branch: ImageBranch,
        _offering: PublishedOffering,
        _request_hash: String,
        _routing: RoutingDecision,
    ) -> Result<(GenerationJob, BalanceChange), ApplicationError> {
        unused_repository()
    }

    async fn get_job(
        &self,
        _account_id: AccountId,
        _job_id: JobId,
    ) -> Result<JobView, ApplicationError> {
        unused_repository()
    }

    async fn claim_next_job(
        &self,
        _worker_id: &str,
        _lease_duration: chrono::Duration,
    ) -> Result<Option<ClaimedJob>, ApplicationError> {
        unused_repository()
    }

    async fn recover_expired_leases(&self) -> Result<LeaseRecovery, ApplicationError> {
        unused_repository()
    }

    async fn begin_attempt(
        &self,
        _job_id: JobId,
        _worker_id: &str,
        _attempt_id: AttemptId,
        _request_digest: &str,
    ) -> Result<u32, ApplicationError> {
        unused_repository()
    }

    async fn requeue_after_unaccepted(
        &self,
        _command: UnacceptedAttempt,
    ) -> Result<(), ApplicationError> {
        unused_repository()
    }

    async fn renew_lease(
        &self,
        _job_id: JobId,
        _worker_id: &str,
        _lease_duration: chrono::Duration,
    ) -> Result<(), ApplicationError> {
        unused_repository()
    }

    async fn complete_job(
        &self,
        _completion: CompleteJob,
    ) -> Result<BalanceChange, ApplicationError> {
        unused_repository()
    }

    async fn fail_job(
        &self,
        _job_id: JobId,
        _worker_id: &str,
        _attempt_id: Option<AttemptId>,
        _failure: AttemptFailure,
    ) -> Result<BalanceChange, ApplicationError> {
        unused_repository()
    }

    async fn count_in_flight_jobs(
        &self,
        _account_id: AccountId,
        _except_idempotency_key: &str,
    ) -> Result<u64, ApplicationError> {
        unused_repository()
    }

    async fn daily_spend_microusd(&self, _account_id: AccountId) -> Result<u64, ApplicationError> {
        unused_repository()
    }

    async fn list_open_reconciliation_cases(
        &self,
    ) -> Result<Vec<ReconciliationCaseView>, ApplicationError> {
        unused_repository()
    }

    async fn provider_failures(
        &self,
        _query: ProviderFailureQuery,
    ) -> Result<Vec<ProviderFailureView>, ApplicationError> {
        unused_repository()
    }

    async fn consecutive_offering_failures(
        &self,
        _offering_id: OfferingId,
        _window: u32,
    ) -> Result<u64, ApplicationError> {
        unused_repository()
    }

    async fn refund_reconciliation(
        &self,
        _command: RefundReconciliationCommand,
    ) -> Result<BalanceChange, ApplicationError> {
        unused_repository()
    }

    async fn account_ledger_mismatch(
        &self,
        _account_id: AccountId,
    ) -> Result<Option<LedgerMismatch>, ApplicationError> {
        Ok(self.mismatch)
    }

    async fn open_ledger_reconciliation_case(
        &self,
        command: OpenLedgerCaseCommand,
    ) -> Result<bool, ApplicationError> {
        self.opened
            .lock()
            .map_err(|error| ApplicationError::Persistence(error.to_string()))?
            .push(command);
        Ok(self.case_opened)
    }
}

/// 一个只记账、不真发信的告警出口。
struct RecordingSink {
    sent: Mutex<Vec<PlatformAlert>>,
}

#[async_trait]
impl AlertSink for RecordingSink {
    async fn send(&self, alert: &PlatformAlert) -> Result<(), ApplicationError> {
        self.sent.lock().expect("sent lock").push(alert.clone());
        Ok(())
    }
}

fn mismatch(account_id: AccountId) -> LedgerMismatch {
    LedgerMismatch {
        account_id,
        ledger_total_microusd: 1_000_000,
        balance_microusd: 999_999,
        holds_total_microusd: 0,
        held_microusd: 0,
    }
}

fn held_mismatch(account_id: AccountId) -> LedgerMismatch {
    LedgerMismatch {
        account_id,
        ledger_total_microusd: 1_000_000,
        balance_microusd: 1_000_000,
        holds_total_microusd: 30_000,
        held_microusd: 29_000,
    }
}

fn auditor(
    repository: Arc<AuditRepository>,
) -> (LedgerAuditor, Arc<RecordingSink>, Arc<AuditRepository>) {
    let sink = Arc::new(RecordingSink {
        sent: Mutex::new(Vec::new()),
    });
    let auditor = LedgerAuditor::new(repository.clone())
        .with_alerts(Arc::new(PlatformAlerter::new(sink.clone())));
    (auditor, sink, repository)
}

/// 余额对不上：建一条案例，并外发一条**账实不符**的告警——载荷带上账户与两组数，一个数不改。
#[tokio::test]
async fn a_balance_mismatch_opens_a_case_and_alerts_the_platform() {
    let account_id = AccountId::new();
    let repository = Arc::new(AuditRepository::new(Some(mismatch(account_id)), true));
    let (auditor, sink, repository) = auditor(repository);

    let report = auditor
        .audit_account(account_id)
        .await
        .expect("this check converges");

    assert_eq!(
        report,
        LedgerAuditReport {
            mismatch_found: true,
            case_opened: true
        }
    );
    let opened = repository.opened();
    assert_eq!(opened.len(), 1, "对不上就要建一条案例");
    assert_eq!(opened[0].account_id, account_id);
    assert!(
        opened[0].reason.contains("1000000") && opened[0].reason.contains("999999"),
        "案例要写清哪条等式断了与两边的数：{}",
        opened[0].reason
    );

    let sent = sink.sent.lock().expect("sent lock").clone();
    assert_eq!(sent.len(), 1, "新建一条案例就外发一条告警");
    let PlatformAlert::LedgerMismatch(alert) = &sent[0] else {
        panic!("账实不符外发的是它自己那种形态");
    };
    assert_eq!(alert.account_id, account_id);
    assert_eq!(alert.ledger_total_microusd, 1_000_000);
    assert_eq!(alert.balance_microusd, 999_999);
    assert_eq!(alert.holds_total_microusd, 0);
    assert_eq!(alert.held_microusd, 0);
}

/// 占用合计对不上：同样建案告警，理由写的是占用那条等式。
#[tokio::test]
async fn a_held_mismatch_opens_a_case_and_alerts_the_platform() {
    let account_id = AccountId::new();
    let repository = Arc::new(AuditRepository::new(Some(held_mismatch(account_id)), true));
    let (auditor, sink, repository) = auditor(repository);

    let report = auditor
        .audit_account(account_id)
        .await
        .expect("this check converges");

    assert_eq!(
        report,
        LedgerAuditReport {
            mismatch_found: true,
            case_opened: true
        }
    );
    let opened = repository.opened();
    assert_eq!(opened.len(), 1);
    assert!(
        opened[0].reason.contains("30000") && opened[0].reason.contains("29000"),
        "占用对不上要写清 active holds 与 held：{}",
        opened[0].reason
    );
    let sent = sink.sent.lock().expect("sent lock").clone();
    let PlatformAlert::LedgerMismatch(alert) = &sent[0] else {
        panic!("账实不符外发的是它自己那种形态");
    };
    assert_eq!(alert.holds_total_microusd, 30_000);
    assert_eq!(alert.held_microusd, 29_000);
}

/// 两条等式都断：理由把两句话都写上，案例只建一条。
#[tokio::test]
async fn both_equations_breaking_are_reported_in_one_case() {
    let account_id = AccountId::new();
    let both = LedgerMismatch {
        account_id,
        ledger_total_microusd: 1_000_000,
        balance_microusd: 999_999,
        holds_total_microusd: 30_000,
        held_microusd: 29_000,
    };
    let repository = Arc::new(AuditRepository::new(Some(both), true));
    let (auditor, _sink, repository) = auditor(repository);

    let report = auditor
        .audit_account(account_id)
        .await
        .expect("this check converges");

    assert_eq!(
        report,
        LedgerAuditReport {
            mismatch_found: true,
            case_opened: true
        }
    );
    let opened = repository.opened();
    assert_eq!(opened.len(), 1, "两条都断也只建一条案例");
    assert!(opened[0].reason.contains("balance"));
    assert!(opened[0].reason.contains("held"));
}

/// 账实一致：**什么都不做**——不建案、不告警。
#[tokio::test]
async fn a_consistent_ledger_produces_no_action_at_all() {
    let repository = Arc::new(AuditRepository::new(None, true));
    let (auditor, sink, repository) = auditor(repository);

    let report = auditor
        .audit_account(AccountId::new())
        .await
        .expect("this check converges");

    assert_eq!(report, LedgerAuditReport::default());
    assert!(repository.opened().is_empty(), "一致时不建案");
    assert!(
        sink.sent.lock().expect("sent lock").is_empty(),
        "一致时不告警"
    );
}

/// 案例还开着的时候**不重复外发**：发现照记（报告里看得出来），但收告警的人不该反复收到同一条
/// ——新建一条案例才是那一条平台侧事件。
#[tokio::test]
async fn an_already_open_case_is_not_alerted_again() {
    let account_id = AccountId::new();
    let repository = Arc::new(AuditRepository::new(Some(mismatch(account_id)), false));
    let (auditor, sink, repository) = auditor(repository);

    let report = auditor
        .audit_account(account_id)
        .await
        .expect("this check converges");

    assert_eq!(
        report,
        LedgerAuditReport {
            mismatch_found: true,
            case_opened: false
        },
        "照样发现了不符，只是没有新建案例"
    );
    assert_eq!(
        repository.opened().len(),
        1,
        "该账户的建案请求照发（真库那边由唯一索引挡掉）"
    );
    assert!(
        sink.sent.lock().expect("sent lock").is_empty(),
        "没有新案例就不外发"
    );
}
