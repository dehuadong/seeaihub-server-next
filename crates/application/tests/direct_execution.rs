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
    AcceptedHandle, AccountingFacts, AdapterDescriptor, AdapterError, DeclaredCost, DispatchGate,
    ExecutionContext, GATEWAY_REQUEST_WIRE_BYTES, GatewayAdapter, GatewayByteLimits, GatewayInput,
    GeneratedImage, ImageAdapter, ProviderCallError, ProviderCost, ProviderCredential,
    ProviderTaskHandle, QueryAccountingCapability, ResponsePayload, RetrySafety,
    begin_generation_send,
};
use seeai_application::{
    AdapterFactory, AdmitExecution, AdmitOutcome, ApplicationError, BeginSubmission,
    ClaimedLateFact, CredentialProvider, DirectExecutionCall, DirectExecutionError,
    DirectExecutionLimits, DirectExecutionRequest, DirectExecutionService, ExecutionFinalization,
    ExecutionLookup, ExecutionRepository, FailOrReconcileExecution, HubRepository, LateFacts,
    LateFactsOutcome, OfferingDraft, PricePlanDraft, ProviderFailureKind, PublishRuntimeCommand,
    RecordAcceptance, RequestFingerprintKeys, RequestTimeoutPolicy, RetryPolicy, RuntimeService,
    SettleExecution, SubmissionStarted, TakenOverExecution,
};
use seeai_domain::{
    AccountId, AttemptId, ConsumerRatesCny, FencingToken, JobId, ProviderCostFact, ProviderTraceId,
    TokenUsage,
};
use seeai_persistence::PgHubRepository;
use serde_json::{Value, json};
use sqlx::{AssertSqlSafe, PgPool, Row};
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
    /// 取消落在生成发送之前：可证明没有提交生成。
    CancelledBeforeSend,
    /// 取消发生在等待上游结果的阶段：不能证明是否已受理。
    CancelledAfterAcceptance,
    /// 第一次调用报可证明未受理，之后成功：验证请求内重投会换新 Attempt 并成功。
    SafeBeforeAcceptanceOnce,
    /// 睡这么多毫秒再成功：把总期限推到结算之后，验证交付超时。
    DelaySuccess(u64),
}

struct FakeGateway {
    send_gate: Arc<Mutex<Option<Arc<DispatchGate>>>>,
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
        context: &dyn ExecutionContext,
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
                    trace_id: Some(ProviderTraceId::parse("trace-1").expect("test trace")),
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
                    trace_id: Some(ProviderTraceId::parse("trace-1").expect("test trace")),
                    retry_safety,
                    kind,
                    provider_cost: Some(ProviderCost::Unavailable),
                }))
            }
            FakeBehavior::AcceptedUnpersisted => Err(AdapterError::AcceptedUnpersisted {
                handle: AcceptedHandle {
                    task_id: ProviderTaskHandle::parse("task-1".to_owned()).expect("test handle"),
                    trace_id: Some(ProviderTraceId::parse("trace-1").expect("test trace")),
                },
                reason: "the fake could not persist the handle".to_owned(),
            }),
            FakeBehavior::CancelledBeforeSend => {
                // 取消恰好落在发送前：先关闸，再问发送资格——真实竞态的等价模型。
                if let Some(gate) = self.send_gate.lock().expect("the send gate lock").clone() {
                    gate.cancel();
                }
                begin_generation_send(context)?;
                Err(AdapterError::Configuration(
                    "the generation gate unexpectedly allowed a send in this test".to_owned(),
                ))
            }
            FakeBehavior::CancelledAfterAcceptance => Err(AdapterError::Cancelled),
        }
    }
}

struct FakeFactory {
    behavior: Arc<Mutex<FakeBehavior>>,
    calls: Arc<AtomicUsize>,
    /// 让用例把真实取消闸交给假渠道：`CancelledBeforeSend` 会在发送资格之前先关闸，
    /// 模拟"取消恰好落在发送前"这一竞态（而不是在执行之前就取消）。
    send_gate: Arc<Mutex<Option<Arc<DispatchGate>>>>,
}

impl FakeFactory {
    fn new() -> Self {
        Self {
            behavior: Arc::new(Mutex::new(FakeBehavior::Success)),
            calls: Arc::new(AtomicUsize::new(0)),
            send_gate: Arc::new(Mutex::new(None)),
        }
    }

    /// 把这次执行的取消闸交给假渠道，让它在发送资格之前关闸。
    fn set_send_gate(&self, gate: Arc<DispatchGate>) {
        *self.send_gate.lock().expect("the send gate lock") = Some(gate);
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
            byte_limits: GatewayByteLimits {
                request_wire_bytes: GATEWAY_REQUEST_WIRE_BYTES,
                provider_response_bytes: 8 * 1024 * 1024,
            },
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
            send_gate: self.send_gate.clone(),
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
            provider_trace_id: Some(ProviderTraceId::parse("trace-1").expect("test trace")),
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

fn test_keys() -> RequestFingerprintKeys {
    let mut request_keys = BTreeMap::new();
    request_keys.insert(1, vec![7_u8; 32]);
    RequestFingerprintKeys::new(request_keys, 1).expect("the test fingerprint keys")
}

/// 轮换后的密钥：当前版本 v2，v1 仍保留——旧记录要用它比对（RFC 0017 §2）。
fn test_keys_rotated_to_v2() -> RequestFingerprintKeys {
    let mut request_keys = BTreeMap::new();
    request_keys.insert(1, vec![7_u8; 32]);
    request_keys.insert(2, vec![8_u8; 32]);
    RequestFingerprintKeys::new(request_keys, 2).expect("the rotated fingerprint keys")
}

/// 轮换后旧版本已从配置移除：记录的版本取不到密钥，无法安全比对。
fn test_keys_without_v1() -> RequestFingerprintKeys {
    let mut request_keys = BTreeMap::new();
    request_keys.insert(2, vec![8_u8; 32]);
    RequestFingerprintKeys::new(request_keys, 2).expect("the rotated fingerprint keys without v1")
}

fn test_timeouts() -> RequestTimeoutPolicy {
    RequestTimeoutPolicy {
        base: Duration::from_secs(1),
        included_images: 1,
        per_image: Duration::from_secs(1),
        provider_timeout: Duration::from_secs(10),
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

/// 只在收尾端口上做手脚的包装：第一次 `settle` **先让真实仓库提交**，再谎报提交结果未知，
/// 用来验证应用层"先 read_finalization 确认、不先假定失败"（RFC 0017 §3）。
///
/// `always_conflict` 则让 `settle` 与 `fail_or_reconcile` 一直报冲突、只读确认也读不到，
/// 模拟所有权已被接管或提交结果长期不明：验证已经取得的事实会在有限收尾预算内交回收件端口，
/// 而不是随返回值丢掉（RFC 0018 §5.1）。
struct FlakySettleRepository {
    inner: Arc<PgHubRepository>,
    fail_next_settle: Arc<AtomicBool>,
    always_conflict: Arc<AtomicBool>,
}

#[async_trait]
impl ExecutionRepository for FlakySettleRepository {
    async fn lookup_execution(
        &self,
        account_id: AccountId,
        idempotency_key_digest: &str,
    ) -> Result<Option<ExecutionLookup>, ApplicationError> {
        self.inner
            .lookup_execution(account_id, idempotency_key_digest)
            .await
    }

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
        if self.always_conflict.load(Ordering::SeqCst) {
            return Err(ApplicationError::Conflict(
                "simulated ownership conflict on settle".to_owned(),
            ));
        }
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
        if self.always_conflict.load(Ordering::SeqCst) {
            return Err(ApplicationError::Conflict(
                "simulated ownership conflict on failure finalization".to_owned(),
            ));
        }
        self.inner.fail_or_reconcile(command).await
    }

    async fn read_finalization(
        &self,
        job_id: JobId,
        attempt_id: AttemptId,
    ) -> Result<Option<ExecutionFinalization>, ApplicationError> {
        if self.always_conflict.load(Ordering::SeqCst) {
            // 提交结果长期不明：确认不到任何已提交的收尾。
            return Ok(None);
        }
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
    /// 这次执行共用一份取消/发送闸，测试可以直接在闸口前取消。
    gate: Arc<DispatchGate>,
}

async fn setup() -> Fixture {
    setup_with(None).await
}

async fn setup_with(flaky_settle: Option<Arc<AtomicBool>>) -> Fixture {
    setup_with_keys(flaky_settle, None, test_keys()).await
}

/// 收尾端口一直报冲突、只读确认也读不到：验证已经取得的事实会被交回收件端口。
async fn setup_with_conflicting_finalization() -> Fixture {
    setup_with_keys(None, Some(Arc::new(AtomicBool::new(true))), test_keys()).await
}

async fn setup_with_keys(
    flaky_settle: Option<Arc<AtomicBool>>,
    always_conflict: Option<Arc<AtomicBool>>,
    keys: RequestFingerprintKeys,
) -> Fixture {
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
    let executions: Arc<dyn ExecutionRepository> = match (&flaky_settle, &always_conflict) {
        (Some(flag), _) => Arc::new(FlakySettleRepository {
            inner: repository.clone(),
            fail_next_settle: flag.clone(),
            always_conflict: Arc::new(AtomicBool::new(false)),
        }),
        (None, Some(conflict)) => Arc::new(FlakySettleRepository {
            inner: repository.clone(),
            fail_next_settle: Arc::new(AtomicBool::new(false)),
            always_conflict: conflict.clone(),
        }),
        (None, None) => repository.clone(),
    };
    let service = build_service(repository.clone(), executions, factory.clone(), keys);
    Fixture {
        repository,
        database_name,
        account_id,
        service,
        factory,
        calls,
        owner: "supervisor-a".to_owned(),
        gate: Arc::new(DispatchGate::new()),
    }
}

/// 用同一组依赖与限制装配一个服务：换一套指纹密钥（轮换用例）时只动密钥，别处逐位一致。
fn build_service(
    repository: Arc<PgHubRepository>,
    executions: Arc<dyn ExecutionRepository>,
    factory: Arc<FakeFactory>,
    keys: RequestFingerprintKeys,
) -> DirectExecutionService {
    DirectExecutionService::new(
        repository,
        executions,
        factory,
        Arc::new(FakeCredentials),
        keys,
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
    })
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
            gate: self.gate.clone(),
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

    async fn job_count(&self) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM generation.jobs WHERE account_id = $1")
            .bind(self.account_id.0)
            .fetch_one(self.pool())
            .await
            .expect("the job count")
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
    let handed_off: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.late_facts WHERE job_id = $1")
            .bind(job_id.0)
            .fetch_one(fixture.pool())
            .await
            .expect("the late fact count");
    assert_eq!(
        handed_off, 0,
        "a formally settled success does not fill the inbox"
    );

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

/// 同步成功但当前所有权下结算不了：已经取得的成功事实在有限预算内交回收件端口，不丢。
#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_success_that_cannot_be_settled_hands_off_its_facts() {
    let fixture = setup_with_conflicting_finalization().await;

    let result = fixture
        .service
        .execute(fixture.request("request-handoff-success"), &fixture.call())
        .await;
    assert!(matches!(result, Err(DirectExecutionError::OutcomeUnknown)));
    let job_id = fixture.job_id().await;
    assert_eq!(
        fixture.captures(job_id).await,
        0,
        "no formal settlement: nothing is captured"
    );
    assert_eq!(
        fixture.held_microusd().await,
        1_000,
        "without a committed finalization the hold is retained"
    );
    let row = sqlx::query(
        "SELECT provider_state, image_count, metering_evidence IS NOT NULL AS has_evidence
         FROM generation.late_facts WHERE job_id = $1 AND kind = 'accounting'",
    )
    .bind(job_id)
    .fetch_one(fixture.pool())
    .await
    .expect("the handed-off success fact");
    assert_eq!(
        row.try_get::<Option<String>, _>("provider_state")
            .expect("provider_state")
            .as_deref(),
        Some("succeeded"),
        "the provider terminal state travels with the fact"
    );
    assert_eq!(
        row.try_get::<Option<i32>, _>("image_count")
            .expect("image_count"),
        Some(1)
    );
    assert!(
        row.try_get::<bool, _>("has_evidence").expect("evidence"),
        "the metering evidence travels with the fact"
    );

    fixture.cleanup().await;
}

/// 明确失败但当前所有权下收尾不了：失败与成本事实同样交回收件端口。
#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_determined_failure_that_cannot_be_finalized_hands_off_its_facts() {
    let fixture = setup_with_conflicting_finalization().await;
    fixture.factory.set(FakeBehavior::Provider {
        retry_safety: RetrySafety::NotRetryable,
        kind: ProviderFailureKind::UpstreamRejected,
    });

    let result = fixture
        .service
        .execute(fixture.request("request-handoff-failure"), &fixture.call())
        .await;
    assert!(matches!(result, Err(DirectExecutionError::OutcomeUnknown)));
    let job_id = fixture.job_id().await;
    assert_eq!(fixture.captures(job_id).await, 0);
    let row = sqlx::query(
        "SELECT provider_state, provider_cost_source
         FROM generation.late_facts WHERE job_id = $1 AND kind = 'accounting'",
    )
    .bind(job_id)
    .fetch_one(fixture.pool())
    .await
    .expect("the handed-off failure fact");
    assert_eq!(
        row.try_get::<Option<String>, _>("provider_state")
            .expect("provider_state")
            .as_deref(),
        Some("failed"),
        "an explicitly failed task is not recorded as success"
    );
    assert_eq!(
        row.try_get::<Option<String>, _>("provider_cost_source")
            .expect("cost source")
            .as_deref(),
        Some("unavailable"),
        "a failure without a reliable amount keeps the cost gap instead of faking zero"
    );

    fixture.cleanup().await;
}

/// 受理状态不明但收尾不了：未知终态如实交接，保留占用交对账。
#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn an_unknown_outcome_that_cannot_be_reconciled_hands_off_its_facts() {
    let fixture = setup_with_conflicting_finalization().await;
    fixture.factory.set(FakeBehavior::Provider {
        retry_safety: RetrySafety::AcceptanceUnknown,
        kind: ProviderFailureKind::UpstreamUnavailable,
    });

    let result = fixture
        .service
        .execute(fixture.request("request-handoff-unknown"), &fixture.call())
        .await;
    assert!(matches!(result, Err(DirectExecutionError::OutcomeUnknown)));
    let job_id = fixture.job_id().await;
    assert_eq!(fixture.captures(job_id).await, 0);
    assert_eq!(
        fixture.held_microusd().await,
        1_000,
        "an unknown acceptance keeps the hold"
    );
    let state: Option<String> = sqlx::query_scalar(
        "SELECT provider_state FROM generation.late_facts WHERE job_id = $1 AND kind = 'accounting'",
    )
    .bind(job_id)
    .fetch_one(fixture.pool())
    .await
    .expect("the handed-off unknown fact");
    assert_eq!(
        state.as_deref(),
        Some("unknown"),
        "an unproven acceptance is handed off as unknown, never as success"
    );

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
async fn a_cancellation_before_the_send_releases_the_hold() {
    let fixture = setup().await;
    fixture.factory.set(FakeBehavior::CancelledBeforeSend);
    // 取消先赢：假渠道在发送资格之前关闸，因此这次 Attempt 确实没有发出生成请求，
    // 但受理已经发生——这正是"已受理、未发送"的那一段。
    fixture.factory.set_send_gate(fixture.gate.clone());

    let result = fixture
        .service
        .execute(fixture.request("request-cancel-pre"), &fixture.call())
        .await;
    assert!(
        matches!(result, Err(DirectExecutionError::RequestTimeout)),
        "a cancellation before the send is a proven non-submission, reported as a timeout"
    );
    assert!(
        !fixture.gate.generation_started(),
        "the gate refused, so no generation was started"
    );
    let job_id = fixture.job_id().await;
    assert_eq!(fixture.job_state().await, "failed");
    assert_eq!(fixture.held_microusd().await, 0, "the hold is released");
    assert_eq!(fixture.captures(job_id).await, 0);
    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        1,
        "a proven non-submission is not retried"
    );

    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_cancellation_after_acceptance_keeps_the_hold() {
    let fixture = setup().await;
    fixture.factory.set(FakeBehavior::CancelledAfterAcceptance);

    let result = fixture
        .service
        .execute(fixture.request("request-cancel-post"), &fixture.call())
        .await;
    assert!(matches!(result, Err(DirectExecutionError::OutcomeUnknown)));
    let job_id = fixture.job_id().await;
    assert_eq!(fixture.job_state().await, "reconciliation_required");
    assert_eq!(fixture.reconciliation_cases(job_id).await, 1);
    assert_eq!(
        fixture.held_microusd().await,
        1_000,
        "a cancellation while waiting for the provider cannot prove non-acceptance"
    );
    assert_eq!(fixture.captures(job_id).await, 0);

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

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_rotated_fingerprint_key_replays_the_same_request() {
    let fixture = setup().await;
    // 第一次用 v1 受理并结算成功：记录按 v1 写下指纹与版本。
    fixture
        .service
        .execute(fixture.request("rotation-replay"), &fixture.call())
        .await
        .expect("the first request");
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);

    // 轮换：当前版本换成 v2，v1 仍配置。旧记录必须用它的 v1 重算，不能拿 v2 重新解释。
    let rotated = build_service(
        fixture.repository.clone(),
        fixture.repository.clone(),
        fixture.factory.clone(),
        test_keys_rotated_to_v2(),
    );
    let replay = rotated
        .execute(fixture.request("rotation-replay"), &fixture.call())
        .await;
    assert!(matches!(
        replay,
        Err(DirectExecutionError::ResultNotRetained)
    ));
    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        1,
        "rotation must not admit the same key as a new request"
    );
    assert_eq!(
        fixture.job_count().await,
        1,
        "rotation creates no second job"
    );

    drop(rotated);
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_replay_without_the_recorded_key_version_is_an_idempotency_conflict() {
    let fixture = setup().await;
    fixture
        .service
        .execute(fixture.request("rotation-missing"), &fixture.call())
        .await
        .expect("the first request");

    // 旧版本已从配置移除：无法安全比对，按 409 idempotency_conflict 拒绝，不新建、不执行。
    let rotated = build_service(
        fixture.repository.clone(),
        fixture.repository.clone(),
        fixture.factory.clone(),
        test_keys_without_v1(),
    );
    let replay = rotated
        .execute(fixture.request("rotation-missing"), &fixture.call())
        .await;
    assert!(matches!(
        replay,
        Err(DirectExecutionError::Application(
            ApplicationError::Conflict(_)
        ))
    ));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture.job_count().await,
        1,
        "a conflict creates no second job"
    );

    drop(rotated);
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_replay_survives_a_disabled_candidate() {
    let fixture = setup().await;
    let success = fixture
        .service
        .execute(fixture.request("replay-delisted"), &fixture.call())
        .await
        .expect("the original request");
    let offering_id: Uuid =
        sqlx::query_scalar("SELECT offering_id FROM generation.jobs WHERE id = $1")
            .bind(success.job_id.0)
            .fetch_one(fixture.pool())
            .await
            .expect("the frozen offering id");
    // 停用这条候选（型号下架同理会清空候选集）：此后选路取不到供给，但重放预查必须还按
    // 记录冻结的合同给出投影，而不是 404。
    sqlx::query("UPDATE supply.offerings SET enabled = false WHERE id = $1")
        .bind(offering_id)
        .execute(fixture.pool())
        .await
        .expect("disable the offering");
    assert!(fixture.job_count().await == 1);
    assert!(
        fixture
            .repository
            .active_offering("gw")
            .await
            .expect("the active candidates")
            .is_empty(),
        "a disabled candidate disappears from routing"
    );

    let replay = fixture
        .service
        .execute(fixture.request("replay-delisted"), &fixture.call())
        .await;
    assert!(matches!(
        replay,
        Err(DirectExecutionError::ResultNotRetained)
    ));
    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        1,
        "the replay never reaches the provider"
    );

    fixture.cleanup().await;
}

/// 受理之前就取消：不建 Job、不占 Hold 与渠道名额，也不调用 Provider。
///
/// 这比"建一条记录再立刻失败"更好：同键重试仍能作为一次正常的新请求处理，而不是命中一条
/// 被取消写死的失败记录。
#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_cancellation_before_admission_creates_nothing() {
    let fixture = setup().await;
    fixture.gate.cancel();

    let result = fixture
        .service
        .execute(fixture.request("request-cancel-pre-admit"), &fixture.call())
        .await;
    assert!(
        matches!(result, Err(DirectExecutionError::RequestTimeout)),
        "a cancellation before admission is a proven non-submission"
    );
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(fixture.pool())
        .await
        .expect("the job count");
    assert_eq!(jobs, 0, "no job record is created");
    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        0,
        "the provider is never called"
    );
    assert_eq!(fixture.held_microusd().await, 0, "no hold is taken");

    fixture.cleanup().await;
}
