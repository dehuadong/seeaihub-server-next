use super::*;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use seeai_adapter_sdk::AdapterDescriptor;
use seeai_application::*;
use seeai_domain::*;
use serde_json::Value;
use std::{future::Future, pin::Pin};
use uuid::Uuid;

/// 一个**没有对账工作**的仓储：这一层只服务"停机时序"用例，真被调到就是用例写错了。
///
/// 其余方法一律 `unimplemented!()`：别的路径真被调到就是用例写错了，不该悄悄返回一个假值让它
/// 看起来跑通了。
#[derive(Default)]
struct EmptyQueueRepository;

#[async_trait]
impl HubRepository for EmptyQueueRepository {
    // 身份、会话与折算率那一组端口与"领任务 / 停机"这条用例无关，真被调到就是用例写错了。
    async fn customer_usage(
        &self,
        _account_id: AccountId,
        _query: CustomerUsageQuery,
    ) -> Result<Vec<CustomerUsageView>, ApplicationError> {
        unimplemented!()
    }

    async fn customer_ledger(
        &self,
        _account_id: AccountId,
        _query: CustomerLedgerQuery,
    ) -> Result<LedgerPage, ApplicationError> {
        unimplemented!()
    }

    async fn customer_billing(
        &self,
        _account_id: AccountId,
        _query: CustomerBillingQuery,
    ) -> Result<CustomerBillingSummary, ApplicationError> {
        unimplemented!()
    }

    async fn model_document_material(
        &self,
        _vendor_id: &str,
        _native_model_id: &str,
        _native_revision: &str,
    ) -> Result<Option<serde_json::Value>, ApplicationError> {
        unimplemented!()
    }

    async fn current_model_document(
        &self,
        _gateway_model: &str,
    ) -> Result<Option<String>, ApplicationError> {
        unimplemented!()
    }

    async fn model_document_by_version(
        &self,
        _gateway_model: &str,
        _version: uuid::Uuid,
    ) -> Result<Option<String>, ApplicationError> {
        unimplemented!()
    }

    async fn current_models_missing_documents(
        &self,
    ) -> Result<Vec<MissingModelDocument>, ApplicationError> {
        unimplemented!()
    }

    async fn insert_model_document(
        &self,
        _runtime_revision_id: RuntimeRevisionId,
        _gateway_model: &str,
        _vendor_model_id: VendorModelId,
        _body: &str,
    ) -> Result<(), ApplicationError> {
        unimplemented!()
    }
    async fn list_api_keys(
        &self,
        _account_id: AccountId,
    ) -> Result<Vec<ApiKeyView>, ApplicationError> {
        unimplemented!()
    }

    async fn revoke_api_key_of_account(
        &self,
        _account_id: AccountId,
        _key_id: Uuid,
        _actor: &str,
    ) -> Result<bool, ApplicationError> {
        unimplemented!()
    }

    async fn current_fx_rates(
        &self,
    ) -> Result<Vec<(String, u64, DateTime<Utc>)>, ApplicationError> {
        unimplemented!()
    }

    async fn find_admin_by_email(
        &self,
        _email: &str,
    ) -> Result<Option<(Uuid, String)>, ApplicationError> {
        unimplemented!()
    }

    async fn ensure_admin_account(
        &self,
        _email: &str,
        _password_hash: &str,
    ) -> Result<(Uuid, bool), ApplicationError> {
        unimplemented!()
    }

    async fn upsert_admin_password(
        &self,
        _email: &str,
        _password_hash: &str,
    ) -> Result<Uuid, ApplicationError> {
        unimplemented!()
    }

    async fn record_admin_login(&self, _admin_id: Uuid) -> Result<(), ApplicationError> {
        unimplemented!()
    }

    async fn create_admin_session(
        &self,
        _admin_id: Uuid,
        _token_hash: &str,
        _expires_at: DateTime<Utc>,
    ) -> Result<Uuid, ApplicationError> {
        unimplemented!()
    }

    async fn find_admin_session(
        &self,
        _token_hash: &str,
    ) -> Result<Option<(Uuid, String, DateTime<Utc>)>, ApplicationError> {
        unimplemented!()
    }

    async fn delete_admin_session(&self, _token_hash: &str) -> Result<(), ApplicationError> {
        unimplemented!()
    }

    async fn record_audit(
        &self,
        _actor: &str,
        _action: &str,
        _subject_type: &str,
        _subject_id: &str,
    ) -> Result<(), ApplicationError> {
        unimplemented!()
    }

    async fn set_admin_password(
        &self,
        _admin_id: Uuid,
        _password_hash: &str,

        _actor: &str,
    ) -> Result<bool, ApplicationError> {
        unimplemented!()
    }

    async fn find_admin_password(
        &self,
        _admin_id: Uuid,
    ) -> Result<Option<String>, ApplicationError> {
        unimplemented!()
    }

    async fn create_password_reset(
        &self,
        _subject_kind: &str,
        _subject_id: Uuid,
        _token_hash: &str,
        _expires_at: DateTime<Utc>,
    ) -> Result<Uuid, ApplicationError> {
        unimplemented!()
    }

    async fn find_password_reset(
        &self,
        _token_hash: &str,
    ) -> Result<Option<(String, Uuid, DateTime<Utc>, Option<DateTime<Utc>>)>, ApplicationError>
    {
        unimplemented!()
    }

    async fn redeem_password_reset(&self, _token_hash: &str) -> Result<bool, ApplicationError> {
        unimplemented!()
    }

    async fn create_customer(
        &self,
        _account_id: AccountId,
        _account_name: &str,
        _email: &str,
        _password_hash: &str,
    ) -> Result<Uuid, ApplicationError> {
        unimplemented!()
    }

    async fn find_customer_by_email(
        &self,
        _email: &str,
    ) -> Result<Option<(Uuid, Uuid, String)>, ApplicationError> {
        unimplemented!()
    }

    async fn find_customer_by_account(
        &self,
        _account_id: Uuid,
    ) -> Result<Option<Uuid>, ApplicationError> {
        unimplemented!()
    }

    async fn find_customer_account(
        &self,
        _customer_id: Uuid,
    ) -> Result<Option<Uuid>, ApplicationError> {
        unimplemented!()
    }

    async fn record_customer_login(&self, _customer_id: Uuid) -> Result<(), ApplicationError> {
        unimplemented!()
    }

    async fn create_customer_session(
        &self,
        _customer_id: Uuid,
        _token_hash: &str,
        _expires_at: DateTime<Utc>,
    ) -> Result<Uuid, ApplicationError> {
        unimplemented!()
    }

    async fn find_customer_session(
        &self,
        _token_hash: &str,
    ) -> Result<Option<(Uuid, Uuid, DateTime<Utc>)>, ApplicationError> {
        unimplemented!()
    }

    async fn delete_customer_session(&self, _token_hash: &str) -> Result<(), ApplicationError> {
        unimplemented!()
    }

    async fn find_customer_password(
        &self,
        _customer_id: Uuid,
    ) -> Result<Option<String>, ApplicationError> {
        unimplemented!()
    }

    async fn set_customer_password(
        &self,
        _customer_id: Uuid,
        _password_hash: &str,

        _actor: &str,
    ) -> Result<bool, ApplicationError> {
        unimplemented!()
    }

    async fn open_customer_account(
        &self,
        _email: &str,
        _password_hash: &str,
        _target: CustomerAccountTarget,
    ) -> Result<(Uuid, Uuid), ApplicationError> {
        unimplemented!()
    }

    async fn find_customer_view(
        &self,
        _email: &str,
    ) -> Result<Option<CustomerView>, ApplicationError> {
        unimplemented!()
    }

    async fn find_customer_view_by_id(
        &self,
        _customer_id: Uuid,
    ) -> Result<Option<CustomerView>, ApplicationError> {
        unimplemented!()
    }

    async fn list_customers(&self, _limit: u32) -> Result<Vec<CustomerView>, ApplicationError> {
        unimplemented!()
    }

    async fn publish_runtime(
        &self,
        _request: PublishRuntimeRequest,
    ) -> Result<PublishedRevision, ApplicationError> {
        unimplemented!()
    }
    async fn active_offering(
        &self,
        _gateway_model: &str,
    ) -> Result<ActiveOfferings, ApplicationError> {
        unimplemented!()
    }
    async fn active_offering_channels(
        &self,
        _gateway_model: &str,
    ) -> Result<Vec<ActiveOfferingChannel>, ApplicationError> {
        unimplemented!()
    }
    async fn offerings_by_id(
        &self,
        _offering_ids: &[OfferingId],
    ) -> Result<Vec<ReferencedOffering>, ApplicationError> {
        unimplemented!()
    }
    async fn enabled_offerings(
        &self,
        _offering_ids: &[OfferingId],
    ) -> Result<std::collections::HashSet<OfferingId>, ApplicationError> {
        unimplemented!()
    }
    async fn selectable_offerings(&self) -> Result<Vec<SelectableOfferingView>, ApplicationError> {
        unimplemented!()
    }
    async fn published_models(&self) -> Result<Vec<PublishedModel>, ApplicationError> {
        unimplemented!()
    }
    async fn gateway_models(&self) -> Result<Vec<GatewayModelView>, ApplicationError> {
        unimplemented!()
    }
    async fn set_gateway_model_settings(
        &self,
        _gateway_model: &str,
        _enabled: Option<bool>,
        _max_concurrent_jobs: Option<Option<u32>>,
        _actor: &str,
    ) -> Result<(), ApplicationError> {
        unimplemented!()
    }
    async fn set_offering_enabled(
        &self,
        _offering_id: OfferingId,
        _enabled: bool,
        _actor: &str,
    ) -> Result<Vec<String>, ApplicationError> {
        unimplemented!()
    }
    async fn set_channel_enabled(
        &self,
        _channel_id: ChannelId,
        _enabled: bool,
        _actor: &str,
    ) -> Result<Vec<String>, ApplicationError> {
        unimplemented!()
    }
    async fn upsert_fx_rate(&self, _rate: NewFxRate, _actor: &str) -> Result<(), ApplicationError> {
        unimplemented!()
    }
    async fn effective_fx_rate(&self, _currency: &str) -> Result<Option<FxRate>, ApplicationError> {
        unimplemented!()
    }
    async fn provider_cost_gaps(
        &self,
        _limit: u32,
    ) -> Result<Vec<ProviderCostGapView>, ApplicationError> {
        unimplemented!()
    }
    async fn accounts_updated_within(
        &self,
        _window: Duration,
    ) -> Result<Vec<BalanceChange>, ApplicationError> {
        // 慢周期账务核对按增量窗口取账户：这条夹具没有任何账户，回空表示"没有要核对的"。
        Ok(Vec::new())
    }
    async fn insert_audit_event(
        &self,
        _actor: &str,
        _action: &str,
        _subject_type: &str,
        _subject_id: &str,
        _payload: Value,
    ) -> Result<(), ApplicationError> {
        unimplemented!()
    }
    async fn create_account(
        &self,
        _account_id: AccountId,
        _name: &str,
        _tag: Option<&str>,
        _initial_credit_microusd: u64,
        _actor: &str,
    ) -> Result<BalanceChange, ApplicationError> {
        unimplemented!()
    }
    async fn set_account_name(
        &self,
        _account_id: AccountId,
        _name: &str,
        _actor: &str,
    ) -> Result<(), ApplicationError> {
        unimplemented!()
    }

    async fn credit_account(
        &self,
        _account_id: AccountId,
        _amount_microusd: u64,
        _business_key: &str,
        _actor: &str,
    ) -> Result<BalanceChange, ApplicationError> {
        unimplemented!()
    }
    async fn read_account_balance(
        &self,
        _account_id: AccountId,
    ) -> Result<BalanceChange, ApplicationError> {
        unimplemented!()
    }
    async fn list_accounts(
        &self,
        _email: Option<&str>,
        _tag: Option<&str>,
        _name: Option<&str>,
        _limit: u32,
    ) -> Result<Vec<AccountSummary>, ApplicationError> {
        unimplemented!()
    }
    async fn find_account_summary(
        &self,
        _account_id: AccountId,
    ) -> Result<Option<AccountSummary>, ApplicationError> {
        unimplemented!()
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
        unimplemented!()
    }
    async fn count_ledger_entries(
        &self,
        _account_id: AccountId,
        _since: Option<chrono::DateTime<chrono::Utc>>,
        _until: Option<chrono::DateTime<chrono::Utc>>,
        _kind: Option<&str>,
    ) -> Result<u64, ApplicationError> {
        unimplemented!()
    }
    async fn held_microusd(&self, _account_id: AccountId) -> Result<i64, ApplicationError> {
        unimplemented!()
    }
    async fn probe(&self) -> Result<(), ApplicationError> {
        unimplemented!()
    }
    async fn account_tag(
        &self,
        _account_id: AccountId,
    ) -> Result<Option<String>, ApplicationError> {
        unimplemented!()
    }
    async fn set_account_tag(
        &self,
        _account_id: AccountId,
        _tag: Option<&str>,
        _actor: &str,
    ) -> Result<(), ApplicationError> {
        unimplemented!()
    }
    async fn route_policy(
        &self,
        _gateway_model: &str,
    ) -> Result<Option<RoutePolicy>, ApplicationError> {
        unimplemented!()
    }
    async fn upsert_route_policy(
        &self,
        _policy: &RoutePolicy,
        _actor: &str,
    ) -> Result<(), ApplicationError> {
        unimplemented!()
    }
    async fn route_policies(&self) -> Result<Vec<RoutePolicy>, ApplicationError> {
        unimplemented!()
    }
    async fn create_api_key(
        &self,
        _account_id: AccountId,
        _label: &str,
        _key_hash: &str,
        _actor: &str,
    ) -> Result<Uuid, ApplicationError> {
        unimplemented!()
    }
    async fn api_key_identity(
        &self,
        _key_hash: &str,
    ) -> Result<(Uuid, AccountId), ApplicationError> {
        unimplemented!()
    }
    async fn list_open_reconciliation_cases(
        &self,
    ) -> Result<Vec<ReconciliationCaseView>, ApplicationError> {
        unimplemented!()
    }
    async fn provider_failures(
        &self,
        _query: ProviderFailureQuery,
    ) -> Result<Vec<ProviderFailureView>, ApplicationError> {
        unimplemented!()
    }
    async fn refund_reconciliation(
        &self,
        _command: RefundReconciliationCommand,
    ) -> Result<BalanceChange, ApplicationError> {
        unimplemented!()
    }
    async fn account_ledger_mismatch(
        &self,
        _account_id: AccountId,
    ) -> Result<Option<LedgerMismatch>, ApplicationError> {
        unimplemented!()
    }
    async fn open_ledger_reconciliation_case(
        &self,
        _command: OpenLedgerCaseCommand,
    ) -> Result<bool, ApplicationError> {
        unimplemented!()
    }
}

/// 对账仓储替身：**第一件事是接管过期所有权**，这里就按住一会儿再回空，把"一轮"拉长成一个
/// 可观察的窗口。两个标志让用例能精确地把停机请求投在"这一轮已经开始、还没跑完"的那一刻。
///
/// 其余方法一律 `unimplemented!()`：一轮对账在接管之后还有孤儿回收与晚到事实消费，这条夹具
/// 只验停机时序，真被调到别的端口就是用例写错了。
#[derive(Default)]
struct BlockedReconciliationRepository {
    iteration_started: Arc<std::sync::atomic::AtomicBool>,
    iteration_finished: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
impl ExecutionRepository for BlockedReconciliationRepository {
    async fn takeover_expired_executions(
        &self,
        _worker_id: &str,
        _lease: ChronoDuration,
        _limit: u32,
        _max_query_attempts: u32,
    ) -> Result<Vec<TakenOverExecution>, ApplicationError> {
        self.iteration_started
            .store(true, std::sync::atomic::Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(200)).await;
        self.iteration_finished
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(Vec::new())
    }

    async fn reap_unsubmitted_admissions(
        &self,
        _max_age: ChronoDuration,
        _limit: u32,
    ) -> Result<u64, ApplicationError> {
        Ok(0)
    }

    async fn claim_unconsumed_late_facts(
        &self,
        _worker_id: &str,
        _limit: u32,
        _claim_ttl: ChronoDuration,
    ) -> Result<Vec<ClaimedLateFact>, ApplicationError> {
        Ok(Vec::new())
    }

    async fn admit(&self, _command: AdmitExecution) -> Result<AdmitOutcome, ApplicationError> {
        unimplemented!()
    }

    async fn lookup_execution(
        &self,
        _account_id: AccountId,
        _idempotency_key_digest: &str,
    ) -> Result<Option<ExecutionLookup>, ApplicationError> {
        unimplemented!()
    }

    async fn begin_submission(
        &self,
        _command: BeginSubmission,
    ) -> Result<SubmissionStarted, ApplicationError> {
        unimplemented!()
    }

    async fn record_acceptance(&self, _command: RecordAcceptance) -> Result<(), ApplicationError> {
        unimplemented!()
    }

    async fn cancel_unsubmitted(
        &self,
        _command: CancelUnsubmitted,
    ) -> Result<ExecutionFinalization, ApplicationError> {
        unimplemented!()
    }

    async fn settle(
        &self,
        _command: SettleExecution,
    ) -> Result<ExecutionFinalization, ApplicationError> {
        unimplemented!()
    }

    async fn fail_or_reconcile(
        &self,
        _command: FailOrReconcileExecution,
    ) -> Result<ExecutionFinalization, ApplicationError> {
        unimplemented!()
    }

    async fn read_finalization(
        &self,
        _job_id: JobId,
        _attempt_id: AttemptId,
    ) -> Result<Option<ExecutionFinalization>, ApplicationError> {
        unimplemented!()
    }

    async fn offer_late_facts(
        &self,
        _facts: LateFacts,
    ) -> Result<LateFactsOutcome, ApplicationError> {
        unimplemented!()
    }

    async fn renew_execution_ownership(
        &self,
        _job_id: JobId,
        _execution_owner: &str,
        _fencing_token: FencingToken,
        _lease: ChronoDuration,
    ) -> Result<(), ApplicationError> {
        unimplemented!()
    }

    async fn mark_late_fact_consumed(&self, _id: Uuid) -> Result<bool, ApplicationError> {
        unimplemented!()
    }

    async fn record_reconciliation_query_attempt(
        &self,
        _job_id: JobId,
        _backoff: ChronoDuration,
    ) -> Result<Option<u32>, ApplicationError> {
        unimplemented!()
    }

    async fn record_terminal_provider_cost(
        &self,
        _job_id: JobId,
        _attempt_id: AttemptId,
        _cost: &ProviderCostFact,
    ) -> Result<bool, ApplicationError> {
        unimplemented!()
    }
}

/// 这条用例不该走到 Driver：`create` 直接报配置错误，而不是提供一个用不到的适配器替身。
struct NoAdapterFactory;

impl AdapterFactory for NoAdapterFactory {
    fn descriptor(&self, _adapter_key: &str) -> Option<AdapterDescriptor> {
        None
    }

    fn validate_publication(
        &self,
        _adapter_key: &str,
        _carrier_schema: &Value,
        _restrictions: &Value,
    ) -> Result<(), String> {
        unimplemented!()
    }
}

struct NoCredentials;

impl CredentialProvider for NoCredentials {
    fn resolve(&self, _reference: &str) -> Result<ProviderCredential, ApplicationError> {
        unimplemented!()
    }
}

/// 一条用例自己的终止信号：可以自己投递一次；[`Self::signal`] 给出主循环要的那份 future。
///
/// 形状与生产里的终止信号（SIGINT / SIGTERM）一致：**可重复轮询**，触发之后一直就绪。`watch` 的当前值
/// 让"再问一遍"始终给出触发结论；裸 `oneshot` 完成后再被轮询会 panic。
struct TestInterrupt {
    /// 触发开关：置位后唤醒 `signal` 的等待者，之后一直读到 `true`。
    trigger: tokio::sync::watch::Sender<bool>,
    /// 交给主循环的那份；`signal()` 从这里克隆，与 `trigger` 同源。
    receiver: tokio::sync::watch::Receiver<bool>,
}

impl Default for TestInterrupt {
    fn default() -> Self {
        let (trigger, receiver) = tokio::sync::watch::channel(false);
        Self { trigger, receiver }
    }
}

impl TestInterrupt {
    fn signal(&self) -> Pin<Box<dyn Future<Output = std::io::Result<()>> + Send>> {
        let mut receiver = self.receiver.clone();
        Box::pin(async move {
            loop {
                if *receiver.borrow() {
                    return Ok(());
                }
                // 发送端没了：没人再能要求停机，按已触发处理，不永远等下去。
                if receiver.changed().await.is_err() {
                    return Ok(());
                }
            }
        })
    }

    fn send(&self) {
        self.trigger.send_replace(true);
    }
}

fn drain_worker(
    executions: Arc<BlockedReconciliationRepository>,
) -> ExecutionReconciliationService {
    ExecutionReconciliationService::new(
        Arc::new(EmptyQueueRepository),
        executions,
        Arc::new(NoAdapterFactory),
        Arc::new(NoCredentials),
        "drain-test-worker".to_owned(),
        ChronoDuration::seconds(30),
    )
}

/// 一份"还没人要求停机"的输入：排空开关是关的，终止信号由用例自己决定什么时候投。
///
/// 返回的第三个值是**共享的**终止信号：用例留一份用来投递，给主循环的那份只是它的一个 future。
fn quiet_signals() -> (
    ShutdownSignals,
    tokio::sync::watch::Sender<bool>,
    Arc<TestInterrupt>,
) {
    let (control, drain_control) = tokio::sync::watch::channel(false);
    let interrupt = Arc::new(TestInterrupt::default());
    (
        ShutdownSignals {
            drain_control,
            interrupt: interrupt.signal(),
        },
        control,
        interrupt,
    )
}

/// 排空请求**不打断在飞的那一轮**：不再领下一轮，手上这一轮跑完才退。
///
/// 判据是"那一轮真的跑完了"：请求投在那一轮已经在飞的时候，循环必须等它返回之后才收工——
/// 而不是在半路把它丢掉。
#[tokio::test]
async fn a_drain_request_waits_for_the_in_flight_iteration() {
    let repository = Arc::new(BlockedReconciliationRepository::default());
    let finished_flag = repository.iteration_finished.clone();
    let service = drain_worker(repository);
    let (signals, control, _interrupt) = quiet_signals();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = control.send(true);
    });

    run_until_shutdown(Some(&service), signals, Duration::from_secs(3_600)).await;

    assert!(
        finished_flag.load(std::sync::atomic::Ordering::SeqCst),
        "在飞的那一轮必须跑完：排空不许在它结束之前返回"
    );
}

/// 终止信号同样等手上这一轮跑完才退：信号只决定"不再领下一轮"。
#[tokio::test]
async fn an_interrupt_waits_for_the_in_flight_iteration() {
    let repository = Arc::new(BlockedReconciliationRepository::default());
    let finished_flag = repository.iteration_finished.clone();
    let service = drain_worker(repository);
    let (signals, _control, interrupt) = quiet_signals();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        interrupt.send();
    });

    run_until_shutdown(Some(&service), signals, Duration::from_secs(3_600)).await;

    assert!(
        finished_flag.load(std::sync::atomic::Ordering::SeqCst),
        "在飞的那一轮必须跑完：收到终止信号也不许丢下它"
    );
}

/// 停机请求**在进程起来之前就成立**时，一轮都不领就收工。
///
/// 这是"重复的停机请求"必须具备的性质：不然每次都会被当成一条新的停机要求，再去领一轮。
#[tokio::test]
async fn an_already_draining_worker_does_not_claim_another_iteration() {
    let repository = Arc::new(BlockedReconciliationRepository::default());
    let started_flag = repository.iteration_started.clone();
    let service = drain_worker(repository);
    let (signals, control, _interrupt) = quiet_signals();
    let _ = control.send(true);

    let outcome = tokio::time::timeout(
        Duration::from_millis(500),
        run_until_shutdown(Some(&service), signals, Duration::from_secs(3_600)),
    )
    .await;

    assert!(
        outcome.is_ok(),
        "已经在排空状态时主循环该立刻收工，而不是再领一轮"
    );
    assert!(
        !started_flag.load(std::sync::atomic::Ordering::SeqCst),
        "排空态下不许领新任务，所以那一轮根本不该开始"
    );
}

/// 没有停机请求时循环**不退出**：它把手上那一轮跑完，然后继续等。
///
/// 断言只看"它没有返回"：用超时把循环截断，被截断才是对的——提前返回意味着"没活干"被当成了
/// "可以走了"。那一轮是否已经跑完**不作断言**：这条用例要钉的是停机判定，把时序卡进断言只会让它
/// 在慢机器上偶发（而它证明不了更多东西）。
#[tokio::test]
async fn the_loop_keeps_working_while_no_stop_is_requested() {
    let repository = Arc::new(BlockedReconciliationRepository::default());
    let service = drain_worker(repository);
    let (signals, _control, _interrupt) = quiet_signals();

    let outcome = tokio::time::timeout(
        Duration::from_millis(600),
        run_until_shutdown(Some(&service), signals, Duration::from_millis(100)),
    )
    .await;

    assert!(
        outcome.is_err(),
        "没有停机请求时循环不许返回（被超时截断才是对的）：{outcome:?}"
    );
}
