//! 直接执行用例对着**真 PostgreSQL** 与**假上游**验（Spec 0005 §3–§5，RFC 0017 §3、§5）。
//!
//! 覆盖：成功路径先结算再返回、账本只扣一次；可证明未受理的失败保留原 Job 可重投；接受状态
//! 不明转对账并保留占用；上游已受理但句柄未入库交付晚到事实且绝不重提；同键重放按 Spec §4
//! 投影成 request_in_progress / result_not_retained / 原错误 / outcome_unknown。
//!
//! 用例从 HTTP_CONTRACT_DATABASE_URL 派生一次性库、跑完整迁移，再发一份真修订；跑完删库。
//! 假上游在内存里控制返回，不发起任何外部调用。

use std::collections::BTreeMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Duration as ChronoDuration;
use seeai_adapter_sdk::{
    AcceptedHandle, AccountingFacts, AdapterDescriptor, AdapterError, DeclaredCost,
    ExecutionContext, GatewayAdapter, GatewayInput, GeneratedImage, ImageAdapter,
    ProviderCallError, ProviderCost, ProviderCredential, QueryAccountingCapability,
    ResponsePayload, RetrySafety,
};
use seeai_application::{
    AdapterFactory, AdmitExecution, AdmitOutcome, ApplicationError, BeginSubmission,
    ClaimedLateFact, CredentialProvider, DirectExecutionCall, DirectExecutionError,
    DirectExecutionLimits, DirectExecutionRequest, DirectExecutionService, ExecutionFinalization,
    ExecutionRepository, FailOrReconcileExecution, FingerprintKeys, LateFacts, LateFactsOutcome,
    OfferingDraft, PricePlanDraft, ProviderFailureKind, PublishRuntimeCommand, RecordAcceptance,
    RequestTimeoutPolicy, RetryPolicy, RuntimeService, SettleExecution, SubmissionStarted,
    TakenOverExecution,
};
use seeai_domain::{
    AccountId, AttemptId, ConsumerRatesCny, FencingToken, JobId, ProviderCostFact, TokenUsage,
};
use seeai_persistence::PgHubRepository;
use serde_json::{Value, json};
use sqlx::{AssertSqlSafe, PgPool};
use uuid::Uuid;

const ADAPTER_KEY: &str = "fake-gateway";

async fn isolated_database_url() -> (String, String) {
    let base = std::env::var("HTTP_CONTRACT_DATABASE_URL")
        .expect("HTTP_CONTRACT_DATABASE_URL is required for the ignored direct execution test");
    let admin = PgPool::connect(&base)
        .await
        .expect("connect to the provided contract database");
    let name = format!("seeai_direct_{}", Uuid::new_v4().simple());
    sqlx::query(AssertSqlSafe(format!("CREATE DATABASE \"{name}\"")))
        .execute(&admin)
        .await
        .expect("create an isolated direct execution database");
    admin.close().await;
    let url = match base.rfind('/') {
        Some(index) => format!("{}/{}", &base[..index], name),
        None => panic!("HTTP_CONTRACT_DATABASE_URL must include a database name"),
    };
    (url, name)
}

async fn drop_isolated_database(name: &str) {
    let base = std::env::var("HTTP_CONTRACT_DATABASE_URL").expect("the contract database url");
    let Ok(admin) = PgPool::connect(&base).await else {
        return;
    };
    let _ = sqlx::query(AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"
    )))
    .execute(&admin)
    .await;
    admin.close().await;
}

#[derive(Debug, Clone, Copy)]
enum FakeBehavior {
    Success,
    Provider {
        retry_safety: RetrySafety,
        kind: ProviderFailureKind,
    },
    AcceptedUnpersisted,
    /// 第一次调用报可证明未受理，之后成功：验证请求内重投会换新 Attempt 并成功。
    SafeBeforeAcceptanceOnce,
    /// 睡这么多毫秒再成功：把总期限推到结算之后，验证交付超时。
    DelaySuccess(u64),
}

struct FakeGateway {
    behavior: Arc<Mutex<FakeBehavior>>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl GatewayAdapter for FakeGateway {
    fn key(&self) -> &'static str {
        ADAPTER_KEY
    }

    fn query_accounting_capability(&self) -> QueryAccountingCapability {
        QueryAccountingCapability::Unsupported
    }

    async fn execute(
        &self,
        _input: Arc<GatewayInput>,
        _context: &dyn ExecutionContext,
        _credential: &ProviderCredential,
    ) -> Result<seeai_adapter_sdk::ProviderOutput, AdapterError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let behavior = *self.behavior.lock().expect("the fake behavior lock");
        if let FakeBehavior::DelaySuccess(millis) = behavior {
            tokio::time::sleep(Duration::from_millis(millis)).await;
        }
        match behavior {
            FakeBehavior::Success | FakeBehavior::DelaySuccess(_) => Ok(success_output()),
            FakeBehavior::SafeBeforeAcceptanceOnce if call == 1 => {
                Err(AdapterError::Provider(ProviderCallError {
                    code: "fake-provider".to_owned(),
                    message: "the fake provider failed before acceptance".to_owned(),
                    trace_id: Some("trace-1".to_owned()),
                    retry_safety: RetrySafety::SafeBeforeAcceptance,
                    kind: ProviderFailureKind::UpstreamUnavailable,
                    provider_cost: Some(ProviderCost::Unavailable),
                }))
            }
            FakeBehavior::SafeBeforeAcceptanceOnce => Ok(success_output()),
            FakeBehavior::Provider { retry_safety, kind } => {
                Err(AdapterError::Provider(ProviderCallError {
                    code: "fake-provider".to_owned(),
                    message: "the fake provider failed".to_owned(),
                    trace_id: Some("trace-1".to_owned()),
                    retry_safety,
                    kind,
                    provider_cost: Some(ProviderCost::Unavailable),
                }))
            }
            FakeBehavior::AcceptedUnpersisted => Err(AdapterError::AcceptedUnpersisted {
                handle: AcceptedHandle {
                    task_id: "task-1".to_owned(),
                    trace_id: Some("trace-1".to_owned()),
                },
                reason: "the fake could not persist the handle".to_owned(),
            }),
        }
    }
}

struct FakeFactory {
    behavior: Arc<Mutex<FakeBehavior>>,
    calls: Arc<AtomicUsize>,
}

impl FakeFactory {
    fn new() -> Self {
        Self {
            behavior: Arc::new(Mutex::new(FakeBehavior::Success)),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn set(&self, behavior: FakeBehavior) {
        *self.behavior.lock().expect("the fake behavior lock") = behavior;
    }
}

impl AdapterFactory for FakeFactory {
    fn descriptor(&self, adapter_key: &str) -> Option<AdapterDescriptor> {
        (adapter_key == ADAPTER_KEY).then_some(AdapterDescriptor {
            key: ADAPTER_KEY,
            supported_top_level_parameters: &["model", "prompt", "image", "mask", "n"],
            supported_extra_parameters: &[],
            supported_branches: &[
                seeai_domain::ImageBranch::PromptOnly,
                seeai_domain::ImageBranch::ImageConditioned,
                seeai_domain::ImageBranch::Masked,
            ],
            max_reference_images: 1,
            declares_cost: true,
        })
    }

    fn validate_publication(
        &self,
        _adapter_key: &str,
        _carrier_schema: &Value,
        _restrictions: &Value,
    ) -> Result<(), String> {
        Ok(())
    }

    fn create(
        &self,
        adapter_key: &str,
        _base_url: &str,
        _timeout: Duration,
    ) -> Result<Arc<dyn ImageAdapter>, ApplicationError> {
        if adapter_key != ADAPTER_KEY {
            return Err(ApplicationError::Configuration(format!(
                "the fake has no legacy adapter for {adapter_key}"
            )));
        }
        // 发布期会装配一次旧接口的 Driver 以确认能构造；直接执行路径不用它。
        Ok(Arc::new(FakeLegacyAdapter))
    }

    fn create_gateway(
        &self,
        adapter_key: &str,
        _base_url: &str,
        _timeout: Duration,
    ) -> Result<Arc<dyn GatewayAdapter>, ApplicationError> {
        if adapter_key != ADAPTER_KEY {
            return Err(ApplicationError::Configuration(format!(
                "the fake has no gateway adapter for {adapter_key}"
            )));
        }
        Ok(Arc::new(FakeGateway {
            behavior: self.behavior.clone(),
            calls: self.calls.clone(),
        }))
    }
}

struct FakeLegacyAdapter;

#[async_trait]
impl ImageAdapter for FakeLegacyAdapter {
    fn key(&self) -> &'static str {
        ADAPTER_KEY
    }

    async fn execute(
        &self,
        _request: seeai_adapter_sdk::PreparedImageRequest,
        _credential: &ProviderCredential,
    ) -> Result<seeai_adapter_sdk::ProviderSuccess, AdapterError> {
        Err(AdapterError::Configuration(
            "the fake legacy adapter is unused".to_owned(),
        ))
    }
}

struct FakeCredentials;

impl CredentialProvider for FakeCredentials {
    fn resolve(&self, _reference: &str) -> Result<ProviderCredential, ApplicationError> {
        ProviderCredential::new("fake-credential".to_owned())
            .map_err(|error| ApplicationError::Configuration(error.to_string()))
    }
}

fn success_output() -> seeai_adapter_sdk::ProviderOutput {
    seeai_adapter_sdk::ProviderOutput {
        response_payload: ResponsePayload {
            created: Some(1),
            images: vec![GeneratedImage::Url("https://img.example/x.png".to_owned())],
        },
        accounting_facts: AccountingFacts {
            usage: Some(usage()),
            provider_cost: ProviderCost::Declared(DeclaredCost {
                amount_microusd: 2_000,
                currency: "USD".to_owned(),
            }),
            image_count: 1,
            response_digest: "digest-1".to_owned(),
            provider_trace_id: Some("trace-1".to_owned()),
        },
    }
}

fn usage() -> TokenUsage {
    TokenUsage {
        input_tokens: 10,
        input_text_tokens: 6,
        input_image_tokens: 4,
        output_tokens: 5,
        output_text_tokens: 2,
        output_image_tokens: 3,
        total_tokens: 15,
    }
}

fn test_keys() -> FingerprintKeys {
    let mut request_keys = BTreeMap::new();
    request_keys.insert(1, vec![7_u8; 32]);
    FingerprintKeys::new(vec![9_u8; 32], 1, request_keys, 1).expect("the test fingerprint keys")
}

fn test_timeouts() -> RequestTimeoutPolicy {
    RequestTimeoutPolicy {
        base: Duration::from_secs(1),
        included_images: 1,
        per_image: Duration::from_secs(1),
        provider_timeout: Duration::from_secs(10),
        worker_lease: Duration::from_secs(10),
        sync_wait: Duration::from_secs(30),
        max_output_images: 4,
    }
}

fn surface() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": {
            "model": {"const": "gw"},
            "prompt": {"type": "string"},
            "image": {"type": "string"},
            "mask": {"type": "string"},
            "n": {"type": "integer", "minimum": 1, "maximum": 4}
        }
    })
}

async fn publish(repository: &Arc<PgHubRepository>, factory: Arc<dyn AdapterFactory>) {
    let runtime = RuntimeService::new(repository.clone(), factory);
    runtime
        .publish(PublishRuntimeCommand {
            vendor_id: Some("fake-vendor".to_owned()),
            native_model_id: Some("gw".to_owned()),
            gateway_model: Some("gw".to_owned()),
            native_revision: Some("v1".to_owned()),
            capability_schema: Some(surface()),
            offerings: Some(vec![OfferingDraft {
                offering_id: None,
                provider_kind: Some("Fake".to_owned()),
                adapter_key: Some(ADAPTER_KEY.to_owned()),
                provider_model_id: "fake-model".to_owned(),
                base_url: Some("http://127.0.0.1:9".to_owned()),
                credential_env: Some("FAKE_PROVIDER_KEY".to_owned()),
                routing_priority: None,
                weight: None,
                restrictions: json!({
                    "allowed_branches": ["prompt_only", "image_conditioned", "masked"],
                    "max_reference_images": 1
                }),
                carrier_schema: Some(surface()),
                parameter_mapping: json!({}),
                capability_schema: None,
                formula: Some("token_rates".to_owned()),
                price_plan: Some(PricePlanDraft {
                    currency: "USD".to_owned(),
                    text_input_microusd_per_million: 5_000_000,
                    image_input_microusd_per_million: 8_000_000,
                    text_output_microusd_per_million: 10_000_000,
                    image_output_microusd_per_million: 30_000_000,
                    source_url: "https://example.invalid/price".to_owned(),
                }),
                cost_unit_price_microusd: None,
                cost_currency: None,
                reference_cost_microusd: None,
                consumer_rates_cny: Some(ConsumerRatesCny {
                    text_input_micros_per_million: 7_000_000,
                    image_input_micros_per_million: 9_000_000,
                    text_output_micros_per_million: 11_000_000,
                    image_output_micros_per_million: 40_000_000,
                }),
                consumer_formula: None,
                cost_basis: None,
                tier_prices: None,
                floor_amounts: None,
            }]),
            references: None,
            markup_bps: None,
            actor: "direct-execution-test".to_owned(),
        })
        .await
        .expect("the runtime publication");
}

/// 只在 `settle` 上做手脚的端口包装：第一次调用**先让真实仓库提交**，再谎报提交结果未知，
/// 用来验证应用层"先 read_finalization 确认、不先假定失败"（RFC 0017 §3）。
struct FlakySettleRepository {
    inner: Arc<PgHubRepository>,
    fail_next_settle: Arc<AtomicBool>,
}

#[async_trait]
impl ExecutionRepository for FlakySettleRepository {
    async fn admit(&self, command: AdmitExecution) -> Result<AdmitOutcome, ApplicationError> {
        self.inner.admit(command).await
    }

    async fn begin_submission(
        &self,
        command: BeginSubmission,
    ) -> Result<SubmissionStarted, ApplicationError> {
        self.inner.begin_submission(command).await
    }

    async fn record_acceptance(&self, command: RecordAcceptance) -> Result<(), ApplicationError> {
        self.inner.record_acceptance(command).await
    }

    async fn settle(
        &self,
        command: SettleExecution,
    ) -> Result<ExecutionFinalization, ApplicationError> {
        let result = self.inner.settle(command).await;
        if self.fail_next_settle.swap(false, Ordering::SeqCst) && result.is_ok() {
            return Err(ApplicationError::Persistence(
                "simulated lost commit acknowledgement".to_owned(),
            ));
        }
        result
    }

    async fn fail_or_reconcile(
        &self,
        command: FailOrReconcileExecution,
    ) -> Result<ExecutionFinalization, ApplicationError> {
        self.inner.fail_or_reconcile(command).await
    }

    async fn read_finalization(
        &self,
        job_id: JobId,
        attempt_id: AttemptId,
    ) -> Result<Option<ExecutionFinalization>, ApplicationError> {
        self.inner.read_finalization(job_id, attempt_id).await
    }

    async fn offer_late_facts(
        &self,
        facts: LateFacts,
    ) -> Result<LateFactsOutcome, ApplicationError> {
        self.inner.offer_late_facts(facts).await
    }

    async fn renew_execution_ownership(
        &self,
        job_id: JobId,
        execution_owner: &str,
        fencing_token: FencingToken,
        lease: ChronoDuration,
    ) -> Result<(), ApplicationError> {
        self.inner
            .renew_execution_ownership(job_id, execution_owner, fencing_token, lease)
            .await
    }

    async fn takeover_expired_executions(
        &self,
        worker_id: &str,
        lease: ChronoDuration,
        limit: u32,
        max_query_attempts: u32,
    ) -> Result<Vec<TakenOverExecution>, ApplicationError> {
        self.inner
            .takeover_expired_executions(worker_id, lease, limit, max_query_attempts)
            .await
    }

    async fn reap_unsubmitted_admissions(
        &self,
        max_age: ChronoDuration,
        limit: u32,
    ) -> Result<u64, ApplicationError> {
        self.inner.reap_unsubmitted_admissions(max_age, limit).await
    }

    async fn claim_unconsumed_late_facts(
        &self,
        worker_id: &str,
        limit: u32,
        claim_ttl: ChronoDuration,
    ) -> Result<Vec<ClaimedLateFact>, ApplicationError> {
        self.inner
            .claim_unconsumed_late_facts(worker_id, limit, claim_ttl)
            .await
    }

    async fn mark_late_fact_consumed(&self, id: Uuid) -> Result<bool, ApplicationError> {
        self.inner.mark_late_fact_consumed(id).await
    }

    async fn record_reconciliation_query_attempt(
        &self,
        job_id: JobId,
        backoff: ChronoDuration,
    ) -> Result<Option<u32>, ApplicationError> {
        self.inner
            .record_reconciliation_query_attempt(job_id, backoff)
            .await
    }

    async fn record_terminal_provider_cost(
        &self,
        job_id: JobId,
        attempt_id: AttemptId,
        cost: &ProviderCostFact,
    ) -> Result<bool, ApplicationError> {
        self.inner
            .record_terminal_provider_cost(job_id, attempt_id, cost)
            .await
    }
}

struct Fixture {
    repository: Arc<PgHubRepository>,
    database_name: String,
    account_id: AccountId,
    service: DirectExecutionService,
    factory: Arc<FakeFactory>,
    calls: Arc<AtomicUsize>,
    owner: String,
}

async fn setup() -> Fixture {
    setup_with(None).await
}

async fn setup_with(flaky_settle: Option<Arc<AtomicBool>>) -> Fixture {
    let (database_url, database_name) = isolated_database_url().await;
    let repository = Arc::new(
        PgHubRepository::connect(&database_url, 4)
            .await
            .expect("the isolated database"),
    );
    repository.migrate().await.expect("the migrations apply");
    let pool = repository.pool().clone();
    let account_id = AccountId::new();
    sqlx::query(
        "INSERT INTO ledger.accounts (id, balance_microusd, held_microusd, version, kind, name)
         VALUES ($1, 1000000, 0, 0, 'consumer', 'direct execution test account')",
    )
    .bind(account_id.0)
    .execute(&pool)
    .await
    .expect("seed account");
    sqlx::query(
        "INSERT INTO pricing.fx_rates (id, currency, rate_micros, effective_at, created_by)
         VALUES (gen_random_uuid(), 'USD', 7100000, now(), 'direct-execution-test')",
    )
    .execute(&pool)
    .await
    .expect("seed fx rate");

    let factory = Arc::new(FakeFactory::new());
    publish(&repository, factory.clone()).await;
    let calls = factory.calls.clone();
    let executions: Arc<dyn ExecutionRepository> = match &flaky_settle {
        Some(flag) => Arc::new(FlakySettleRepository {
            inner: repository.clone(),
            fail_next_settle: flag.clone(),
        }),
        None => repository.clone(),
    };
    let service = DirectExecutionService::new(
        repository.clone(),
        executions,
        factory.clone(),
        Arc::new(FakeCredentials),
        test_keys(),
        test_timeouts(),
        DirectExecutionLimits {
            max_account_in_flight: 8,
            max_channel_in_flight: 8,
            default_hold_microusd: 1_000,
        },
    )
    .with_settle_reserve(Duration::from_secs(1))
    // 用例把退避压到 1ms：重投逻辑要看，但不该让每条用例多等几秒。
    .with_retry_policy(RetryPolicy {
        max_attempts: 3,
        backoff_base: Duration::from_millis(1),
    });
    Fixture {
        repository,
        database_name,
        account_id,
        service,
        factory,
        calls,
        owner: "supervisor-a".to_owned(),
    }
}

impl Fixture {
    async fn cleanup(self) {
        // 先放下 service 持有的连接池，再删库。
        drop(self.service);
        drop(self.repository);
        drop_isolated_database(&self.database_name).await;
    }

    fn pool(&self) -> &PgPool {
        self.repository.pool()
    }

    fn request(&self, key: &str) -> DirectExecutionRequest {
        DirectExecutionRequest {
            account_id: self.account_id,
            model: "gw".to_owned(),
            endpoint: "/v1/images/generations".to_owned(),
            native_parameters: json!({"prompt": "a red fox"}),
            reference_images: Vec::new(),
            mask: None,
            idempotency_key: key.to_owned(),
        }
    }

    fn call(&self) -> DirectExecutionCall {
        self.call_within(Duration::from_secs(30))
    }

    /// 总期限从"现在"起算 `budget`：直接执行用例自己给一个绝对时刻。
    fn call_within(&self, budget: Duration) -> DirectExecutionCall {
        DirectExecutionCall {
            execution_owner: self.owner.clone(),
            cancelled: Arc::new(AtomicBool::new(false)),
            total_deadline: tokio::time::Instant::now() + budget,
            ownership: None,
        }
    }

    async fn job_id(&self) -> Uuid {
        sqlx::query_scalar("SELECT id FROM generation.jobs WHERE account_id = $1")
            .bind(self.account_id.0)
            .fetch_one(self.pool())
            .await
            .expect("the job id")
    }

    async fn job_state(&self) -> String {
        sqlx::query_scalar("SELECT state FROM generation.jobs WHERE account_id = $1")
            .bind(self.account_id.0)
            .fetch_one(self.pool())
            .await
            .expect("the job state")
    }

    async fn held_microusd(&self) -> i64 {
        sqlx::query_scalar("SELECT held_microusd FROM ledger.accounts WHERE id = $1")
            .bind(self.account_id.0)
            .fetch_one(self.pool())
            .await
            .expect("the held amount")
    }

    async fn captures(&self, job_id: Uuid) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM ledger.entries WHERE job_id = $1 AND kind = 'capture'",
        )
        .bind(job_id)
        .fetch_one(self.pool())
        .await
        .expect("the capture count")
    }

    async fn reconciliation_cases(&self, job_id: Uuid) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM operations.reconciliation_cases WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(self.pool())
            .await
            .expect("the reconciliation case count")
    }

    async fn attempt_states(&self) -> Vec<String> {
        let job_id = self.job_id().await;
        sqlx::query_scalar(
            "SELECT state FROM generation.attempts WHERE job_id = $1 ORDER BY attempt_no",
        )
        .bind(job_id)
        .fetch_all(self.pool())
        .await
        .expect("the attempt states")
    }
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn success_settles_before_returning_and_charges_once() {
    let fixture = setup().await;
    let request = fixture.request("request-success");
    let call = fixture.call();

    let success = fixture
        .service
        .execute(request, &call)
        .await
        .expect("the direct execution succeeds");
    assert_eq!(success.payload.images.len(), 1);
    let job_id = success.job_id;
    assert_eq!(fixture.job_state().await, "succeeded");
    assert_eq!(
        fixture.captures(job_id.0).await,
        1,
        "the ledger captures once"
    );
    assert_eq!(fixture.held_microusd().await, 0, "the hold is settled");
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);

    // 同键再来：已结算且结果不保留，不重新执行、不再扣费。
    let replay = fixture
        .service
        .execute(fixture.request("request-success"), &call)
        .await;
    assert!(matches!(
        replay,
        Err(DirectExecutionError::ResultNotRetained)
    ));
    assert_eq!(fixture.captures(job_id.0).await, 1, "no second capture");
    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        1,
        "no second provider call"
    );

    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn safe_before_acceptance_is_retried_in_request_then_released() {
    let fixture = setup().await;
    fixture.factory.set(FakeBehavior::Provider {
        retry_safety: RetrySafety::SafeBeforeAcceptance,
        kind: ProviderFailureKind::UpstreamUnavailable,
    });

    let result = fixture
        .service
        .execute(fixture.request("request-retry"), &fixture.call())
        .await;
    assert!(
        matches!(
            result,
            Err(DirectExecutionError::OriginalFailure {
                code: seeai_application::PublicErrorCode::PlatformUnavailable
            })
        ),
        "retry exhaustion is a determined failure, not a permanent in-progress"
    );
    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        3,
        "the request retries the same candidate up to the configured attempt limit"
    );
    assert_eq!(
        fixture.job_state().await,
        "failed",
        "retry exhaustion releases the hold and closes the job"
    );
    assert_eq!(fixture.held_microusd().await, 0, "the hold is released");
    assert_eq!(fixture.attempt_states().await, vec!["terminal"; 3]);

    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn safe_before_acceptance_then_success_settles_once() {
    let fixture = setup().await;
    fixture.factory.set(FakeBehavior::SafeBeforeAcceptanceOnce);

    let success = fixture
        .service
        .execute(fixture.request("request-retry-success"), &fixture.call())
        .await
        .expect("the retried request succeeds");
    assert_eq!(success.payload.images.len(), 1);
    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        2,
        "the first attempt is provably unaccepted, the second one runs"
    );
    assert_eq!(fixture.job_state().await, "succeeded");
    assert_eq!(fixture.captures(success.job_id.0).await, 1);
    assert_eq!(fixture.held_microusd().await, 0);
    assert_eq!(fixture.attempt_states().await, vec!["terminal", "terminal"]);

    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_deadline_before_the_provider_call_returns_request_timeout_and_releases() {
    let fixture = setup().await;
    fixture.factory.set(FakeBehavior::Success);

    // 结算预留 R 是 1s，这里给的总期限还不到 R：提交声明能落，但外部预算已到尾，一次上游都不发。
    let result = fixture
        .service
        .execute(
            fixture.request("request-timeout"),
            &fixture.call_within(Duration::from_millis(200)),
        )
        .await;
    assert!(matches!(result, Err(DirectExecutionError::RequestTimeout)));
    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        0,
        "a request whose external budget is gone does not call the provider"
    );
    assert_eq!(fixture.job_state().await, "failed");
    assert_eq!(fixture.held_microusd().await, 0);

    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_settle_after_the_total_deadline_returns_result_delivery_timeout() {
    let fixture = setup().await;
    // 上游慢过总期限：结算仍会提交（钱照收），但图片来不及准备返回。
    fixture.factory.set(FakeBehavior::DelaySuccess(1_300));

    let result = fixture
        .service
        .execute(
            fixture.request("request-delivery-timeout"),
            &fixture.call_within(Duration::from_millis(1_200)),
        )
        .await;
    assert!(matches!(
        result,
        Err(DirectExecutionError::ResultDeliveryTimeout)
    ));
    assert_eq!(fixture.job_state().await, "succeeded");
    assert_eq!(fixture.held_microusd().await, 0, "the charge is committed");
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);

    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn an_unknown_settle_commit_is_confirmed_instead_of_assumed_failed() {
    let fail_next = Arc::new(AtomicBool::new(true));
    let fixture = setup_with(Some(fail_next.clone())).await;

    let success = fixture
        .service
        .execute(fixture.request("request-settle-unknown"), &fixture.call())
        .await
        .expect("a committed settle with a lost acknowledgement is confirmed, not failed");
    assert_eq!(
        fixture.captures(success.job_id.0).await,
        1,
        "confirming the commit must not charge twice"
    );
    assert_eq!(fixture.job_state().await, "succeeded");
    assert!(
        !fail_next.load(Ordering::SeqCst),
        "the simulated lost commit acknowledgement was exercised"
    );

    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn an_unknown_acceptance_reconciles_and_keeps_the_hold() {
    let fixture = setup().await;
    fixture.factory.set(FakeBehavior::Provider {
        retry_safety: RetrySafety::AcceptanceUnknown,
        kind: ProviderFailureKind::UpstreamUnavailable,
    });

    let result = fixture
        .service
        .execute(fixture.request("request-unknown"), &fixture.call())
        .await;
    assert!(matches!(result, Err(DirectExecutionError::OutcomeUnknown)));
    let job_id = fixture.job_id().await;
    assert_eq!(fixture.job_state().await, "reconciliation_required");
    assert_eq!(fixture.reconciliation_cases(job_id).await, 1);
    assert_eq!(fixture.held_microusd().await, 1_000, "the hold is retained");
    assert_eq!(fixture.captures(job_id).await, 0);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);

    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn accepted_unpersisted_offers_late_facts_and_reconciles() {
    let fixture = setup().await;
    fixture.factory.set(FakeBehavior::AcceptedUnpersisted);

    let result = fixture
        .service
        .execute(fixture.request("request-accepted"), &fixture.call())
        .await;
    assert!(matches!(result, Err(DirectExecutionError::OutcomeUnknown)));
    let job_id = fixture.job_id().await;
    assert_eq!(fixture.job_state().await, "reconciliation_required");
    let facts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM generation.late_facts WHERE job_id = $1 AND kind = 'task_handle'",
    )
    .bind(job_id)
    .fetch_one(fixture.pool())
    .await
    .expect("the late fact count");
    assert_eq!(facts, 1, "the handle is offered as a late fact");
    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        1,
        "no second provider call"
    );

    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn the_same_key_replays_settled_failed_and_unknown_outcomes() {
    let fixture = setup().await;

    // 1) 已结算成功、结果不保留。
    fixture
        .service
        .execute(fixture.request("replay-settled"), &fixture.call())
        .await
        .expect("the settled request");
    let settled = fixture
        .service
        .execute(fixture.request("replay-settled"), &fixture.call())
        .await;
    assert!(matches!(
        settled,
        Err(DirectExecutionError::ResultNotRetained)
    ));

    // 3) 已确定失败：原平台错误码重放。
    fixture.factory.set(FakeBehavior::Provider {
        retry_safety: RetrySafety::NotRetryable,
        kind: ProviderFailureKind::ConsumerContent,
    });
    let _ = fixture
        .service
        .execute(fixture.request("replay-failed"), &fixture.call())
        .await;
    let failed = fixture
        .service
        .execute(fixture.request("replay-failed"), &fixture.call())
        .await;
    assert!(matches!(
        failed,
        Err(DirectExecutionError::OriginalFailure {
            code: seeai_application::PublicErrorCode::ContentRejected
        })
    ));

    // 4) 结果未知：保留占用，同键重发回 outcome_unknown。
    fixture.factory.set(FakeBehavior::Provider {
        retry_safety: RetrySafety::AcceptanceUnknown,
        kind: ProviderFailureKind::UpstreamUnavailable,
    });
    let _ = fixture
        .service
        .execute(fixture.request("replay-unknown"), &fixture.call())
        .await;
    let unknown = fixture
        .service
        .execute(fixture.request("replay-unknown"), &fixture.call())
        .await;
    assert!(matches!(unknown, Err(DirectExecutionError::OutcomeUnknown)));

    fixture.cleanup().await;
}
