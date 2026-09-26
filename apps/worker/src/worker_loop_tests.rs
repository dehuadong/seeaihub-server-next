use super::*;
use async_trait::async_trait;
use seeai_adapter_sdk::{AdapterDescriptor, ImageAdapter};
use seeai_application::*;
use seeai_domain::*;
use serde_json::Value;
use std::{future::Future, pin::Pin};
use uuid::Uuid;

/// 一个**空队列**的仓储：`claim_next_job` 先按住一会儿再回 `None`，把"一轮"拉长成一个可观察的
/// 窗口。两个标志让用例能精确地把停机请求投在"这一轮已经开始、还没跑完"的那一刻。
///
/// 其余方法一律 `unimplemented!()`：这条用例只走"领取下一轮"这一条路，别的路径真被调到就是用例
/// 写错了，不该悄悄返回一个假值让它看起来跑通了。
#[derive(Default)]
struct EmptyQueueRepository {
    iteration_started: Arc<std::sync::atomic::AtomicBool>,
    iteration_finished: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
impl HubRepository for EmptyQueueRepository {
    async fn claim_next_job(
        &self,
        _worker_id: &str,
        _lease_duration: ChronoDuration,
    ) -> Result<Option<ClaimedJob>, ApplicationError> {
        self.iteration_started
            .store(true, std::sync::atomic::Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(200)).await;
        self.iteration_finished
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(None)
    }

    async fn recover_expired_leases(&self) -> Result<LeaseRecovery, ApplicationError> {
        Ok(LeaseRecovery::default())
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
    ) -> Result<Vec<OfferingCandidate>, ApplicationError> {
        unimplemented!()
    }
    async fn enabled_offerings(
        &self,
        _offering_ids: &[OfferingId],
    ) -> Result<std::collections::HashSet<OfferingId>, ApplicationError> {
        unimplemented!()
    }
    async fn published_models(&self) -> Result<Vec<PublishedModel>, ApplicationError> {
        unimplemented!()
    }
    async fn gateway_models(&self) -> Result<Vec<GatewayModelView>, ApplicationError> {
        unimplemented!()
    }
    async fn set_gateway_model_enabled(
        &self,
        _gateway_model: &str,
        _enabled: bool,
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
    async fn acceptance_probe(
        &self,
        _gateway_model: &str,
        _account_id: AccountId,
        _idempotency_key: &str,
    ) -> Result<AcceptanceProbe, ApplicationError> {
        unimplemented!()
    }
    async fn accounts_updated_within(
        &self,
        _window: Duration,
    ) -> Result<Vec<BalanceChange>, ApplicationError> {
        unimplemented!()
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
        _initial_credit_microusd: u64,
        _actor: &str,
    ) -> Result<BalanceChange, ApplicationError> {
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
    async fn read_ledger_entries(
        &self,
        _account_id: AccountId,
        _since: Option<chrono::DateTime<chrono::Utc>>,
        _limit: u32,
    ) -> Result<Vec<LedgerEntry>, ApplicationError> {
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
    async fn revoke_api_key(&self, _key_id: Uuid, _actor: &str) -> Result<(), ApplicationError> {
        unimplemented!()
    }
    async fn api_key_identity(
        &self,
        _key_hash: &str,
    ) -> Result<(Uuid, AccountId), ApplicationError> {
        unimplemented!()
    }
    async fn create_job(
        &self,
        _command: CreateImageGeneration,
        _branch: ImageBranch,
        _offering: PublishedOffering,
        _request_hash: String,
        _routing: RoutingDecision,
    ) -> Result<(GenerationJob, BalanceChange), ApplicationError> {
        unimplemented!()
    }
    async fn get_job(
        &self,
        _account_id: AccountId,
        _job_id: JobId,
    ) -> Result<JobView, ApplicationError> {
        unimplemented!()
    }
    async fn begin_attempt(
        &self,
        _job_id: JobId,
        _worker_id: &str,
        _attempt_id: AttemptId,
        _request_digest: &str,
    ) -> Result<u32, ApplicationError> {
        unimplemented!()
    }
    async fn requeue_after_unaccepted(
        &self,
        _command: UnacceptedAttempt,
    ) -> Result<(), ApplicationError> {
        unimplemented!()
    }
    async fn renew_lease(
        &self,
        _job_id: JobId,
        _worker_id: &str,
        _lease_duration: ChronoDuration,
    ) -> Result<(), ApplicationError> {
        unimplemented!()
    }
    async fn complete_job(
        &self,
        _completion: CompleteJob,
    ) -> Result<BalanceChange, ApplicationError> {
        unimplemented!()
    }
    async fn fail_job(
        &self,
        _job_id: JobId,
        _worker_id: &str,
        _attempt_id: Option<AttemptId>,
        _failure: AttemptFailure,
    ) -> Result<BalanceChange, ApplicationError> {
        unimplemented!()
    }
    async fn count_in_flight_jobs(
        &self,
        _account_id: AccountId,
        _except_idempotency_key: &str,
    ) -> Result<u64, ApplicationError> {
        unimplemented!()
    }
    async fn daily_spend_microusd(&self, _account_id: AccountId) -> Result<u64, ApplicationError> {
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
    async fn consecutive_offering_failures(
        &self,
        _offering_id: OfferingId,
        _window: u32,
    ) -> Result<u64, ApplicationError> {
        unimplemented!()
    }
    async fn refund_reconciliation(
        &self,
        _command: RefundReconciliationCommand,
    ) -> Result<BalanceChange, ApplicationError> {
        unimplemented!()
    }
    async fn accounts_with_ledger_mismatch(
        &self,
    ) -> Result<Vec<LedgerBalanceMismatch>, ApplicationError> {
        unimplemented!()
    }
    async fn open_ledger_reconciliation_case(
        &self,
        _command: OpenLedgerCaseCommand,
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

    fn create(
        &self,
        adapter_key: &str,
        _base_url: &str,
        _timeout: Duration,
    ) -> Result<Arc<dyn ImageAdapter>, ApplicationError> {
        Err(ApplicationError::Configuration(format!(
            "the drain fixture has no driver for {adapter_key}"
        )))
    }
}

struct NoCredentials;

impl CredentialProvider for NoCredentials {
    fn resolve(&self, _reference: &str) -> Result<ProviderCredential, ApplicationError> {
        unimplemented!()
    }
}

/// 一条用例自己的终止信号：可以自己投递一次；`install` 给出主循环要的那份 future。
#[derive(Default)]
struct TestInterrupt {
    sender: Option<tokio::sync::oneshot::Sender<std::io::Result<()>>>,
}

impl TestInterrupt {
    fn install(&mut self) -> Pin<Box<dyn Future<Output = std::io::Result<()>> + Send>> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.sender = Some(sender);
        Box::pin(async move { receiver.await.expect("the interrupt is sent") })
    }

    fn send(&mut self) {
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(Ok(()));
        }
    }
}

fn drain_worker(repository: Arc<EmptyQueueRepository>) -> WorkerService {
    WorkerService::new(
        repository,
        Arc::new(NoAdapterFactory),
        Arc::new(NoCredentials),
        "drain-test-worker".to_owned(),
        ChronoDuration::seconds(30),
        // 只要一条自洽的链即可：这条用例不跑 Driver，超时取值不参与判定。
        RequestTimeoutPolicy {
            base: Duration::from_secs(1),
            included_images: 1,
            per_image: Duration::ZERO,
            provider_timeout: Duration::from_secs(1),
            worker_lease: Duration::from_secs(30),
            sync_wait: Duration::from_secs(1),
            max_output_images: 1,
        },
    )
    .expect("the fixture policy must be a consistent chain")
}

/// 一份"还没人要求停机"的输入：排空开关是关的，终止信号由用例自己决定什么时候投。
fn quiet_signals() -> (
    ShutdownSignals,
    tokio::sync::watch::Sender<bool>,
    TestInterrupt,
) {
    let (control, drain_control) = tokio::sync::watch::channel(false);
    let mut interrupt = TestInterrupt::default();
    let interrupt_future = interrupt.install();
    (
        ShutdownSignals {
            drain_control,
            interrupt: interrupt_future,
        },
        control,
        interrupt,
    )
}

/// 排空请求**不打断在飞的那一轮**：不再领下一轮，手上这一轮跑完才退。
///
/// 判据是"那一轮真的跑完了"：请求投在 `claim_next_job` 正按住的时候（这一轮已经在飞），
/// 循环必须等它返回之后才收工——而不是在半路把它丢掉。
#[tokio::test]
async fn a_drain_request_waits_for_the_in_flight_iteration() {
    let repository = Arc::new(EmptyQueueRepository::default());
    let finished_flag = repository.iteration_finished.clone();
    let worker = drain_worker(repository);
    let (signals, control, _interrupt) = quiet_signals();

    // 主循环先在"等停机"上停住，这一投递把它放行；随后那一轮要跑满 600ms。
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = control.send(true);
    });

    run_until_shutdown(&worker, signals, Duration::from_secs(3_600))
        .await
        .expect("draining must not fail");

    assert!(
        finished_flag.load(std::sync::atomic::Ordering::SeqCst),
        "在飞的那一轮必须跑完：排空不许在它结束之前返回"
    );
}

/// 终止信号同样等手上这一轮跑完才退：信号只决定"不再领下一轮"。
#[tokio::test]
async fn an_interrupt_waits_for_the_in_flight_iteration() {
    let repository = Arc::new(EmptyQueueRepository::default());
    let finished_flag = repository.iteration_finished.clone();
    let worker = drain_worker(repository);
    let (signals, _control, mut interrupt) = quiet_signals();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        interrupt.send();
    });

    run_until_shutdown(&worker, signals, Duration::from_secs(3_600))
        .await
        .expect("draining must not fail");

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
    let repository = Arc::new(EmptyQueueRepository::default());
    let started_flag = repository.iteration_started.clone();
    let worker = drain_worker(repository);
    let (signals, control, _interrupt) = quiet_signals();
    let _ = control.send(true);

    let outcome = tokio::time::timeout(
        Duration::from_millis(500),
        run_until_shutdown(&worker, signals, Duration::from_secs(3_600)),
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
    let repository = Arc::new(EmptyQueueRepository::default());
    let worker = drain_worker(repository);
    let (signals, _control, _interrupt) = quiet_signals();

    let outcome = tokio::time::timeout(
        Duration::from_millis(600),
        run_until_shutdown(&worker, signals, Duration::from_millis(100)),
    )
    .await;

    assert!(
        outcome.is_err(),
        "没有停机请求时循环不许返回（被超时截断才是对的）：{outcome:?}"
    );
}
