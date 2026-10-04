//! 异常对账用例对着**真 PostgreSQL** 与**假上游只读查询**验（Spec 0005 A2、A5-A8、A10；
//! RFC 0017 §3、§5、§6）。
//!
//! 覆盖：接管后有可信句柄只读查询并幂等结算一次、无句柄只建案保留占用、终态但证据缺失建成本缺口、
//! 晚到事实领取消费与超时重领、旧 Worker 领不到 v1 记录、孤儿 admitted 回收。
//!
//! 用例从 HTTP_CONTRACT_DATABASE_URL 派生一次性库、跑完整迁移，再用最小事实直接受理与提交；
//! 假上游只回答只读查询，任何 execute 调用都会被计数（生成请求绝不许在 Worker 上重发）。
//! 跑完删库。

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use seeai_adapter_sdk::{
    AccountingFacts, AccountingQuery, AdapterDescriptor, AdapterError, Deadline, DeclaredCost,
    ExecutionContext, GatewayAdapter, GatewayInput, ImageAdapter, ProviderCost, ProviderCredential,
    ProviderOutput, ProviderTaskState, QueryAccountingCapability,
};
use seeai_application::{
    AdapterFactory, AdmitExecution, AdmitOffering, AdmitOutcome, ApplicationError, BeginSubmission,
    CredentialProvider, ExecutionReconciliationService, ExecutionRepository,
    FailOrReconcileExecution, FailureDisposition, HubRepository, LateFacts, ProviderFailureKind,
    ReconciliationPolicy, RecordAcceptance, RetryPolicy, RoutingDecision, SettleExecution,
};
use seeai_domain::{
    AccountId, AttemptId, ChannelId, ChargeFacts, FencingToken, ImageBranch, JobId,
    MeteringEvidence, OfferingId, PriceSnapshot, ProviderCostFact, ProviderCostSource,
    RuntimeRevisionId, TokenUsage, VendorModelId,
};
use seeai_persistence::PgHubRepository;
use serde_json::json;
use sqlx::{AssertSqlSafe, PgPool, Row};
use uuid::Uuid;

const ADAPTER_KEY: &str = "fake";

async fn isolated_database_url() -> (String, String) {
    let base = std::env::var("HTTP_CONTRACT_DATABASE_URL")
        .expect("HTTP_CONTRACT_DATABASE_URL is required for the ignored reconciliation test");
    let admin = PgPool::connect(&base)
        .await
        .expect("connect to the provided contract database");
    let name = format!("seeai_recon_{}", Uuid::new_v4().simple());
    sqlx::query(AssertSqlSafe(format!("CREATE DATABASE \"{name}\"")))
        .execute(&admin)
        .await
        .expect("create an isolated reconciliation database");
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

/// 只读查询的假行为。生成执行永不在这条路径上发生。
#[derive(Clone)]
enum FakeQuery {
    Terminal(AccountingFacts),
    TerminalWithoutFacts,
    NotTerminal,
    Untrusted,
    Failed(AccountingFacts),
    Cancelled(AccountingFacts),
    Unsupported,
}

struct FakeFactory {
    behavior: Arc<Mutex<FakeQuery>>,
    queries: Arc<AtomicUsize>,
    executes: Arc<AtomicUsize>,
}

impl FakeFactory {
    fn new() -> Self {
        Self {
            behavior: Arc::new(Mutex::new(FakeQuery::Terminal(terminal_facts()))),
            queries: Arc::new(AtomicUsize::new(0)),
            executes: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn set(&self, behavior: FakeQuery) {
        *self.behavior.lock().expect("the fake behavior lock") = behavior;
    }
}

struct FakeGateway {
    behavior: Arc<Mutex<FakeQuery>>,
    queries: Arc<AtomicUsize>,
    executes: Arc<AtomicUsize>,
}

#[async_trait]
impl GatewayAdapter for FakeGateway {
    fn key(&self) -> &'static str {
        ADAPTER_KEY
    }

    fn query_accounting_capability(&self) -> QueryAccountingCapability {
        match *self.behavior.lock().expect("the fake behavior lock") {
            FakeQuery::Unsupported => QueryAccountingCapability::Unsupported,
            _ => QueryAccountingCapability::Supported,
        }
    }

    async fn execute(
        &self,
        _input: Arc<GatewayInput>,
        _context: &dyn ExecutionContext,
        _credential: &ProviderCredential,
    ) -> Result<ProviderOutput, AdapterError> {
        self.executes.fetch_add(1, Ordering::SeqCst);
        Err(AdapterError::Configuration(
            "the reconciliation must never submit a generation request".to_owned(),
        ))
    }

    async fn query_accounting(
        &self,
        _handle: &seeai_adapter_sdk::AcceptedHandle,
        _cost_currency: &str,
        _deadline: Deadline,
        _credential: &ProviderCredential,
    ) -> Result<AccountingQuery, AdapterError> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        match self
            .behavior
            .lock()
            .expect("the fake behavior lock")
            .clone()
        {
            FakeQuery::Terminal(facts) => Ok(AccountingQuery {
                state: ProviderTaskState::Succeeded,
                accounting_facts: Some(facts),
            }),
            FakeQuery::TerminalWithoutFacts => Ok(AccountingQuery {
                state: ProviderTaskState::Succeeded,
                accounting_facts: None,
            }),
            FakeQuery::NotTerminal => Ok(AccountingQuery {
                state: ProviderTaskState::Pending,
                accounting_facts: None,
            }),
            // 渠道状态不可信（未列出的状态，或响应无法与句柄关联）：既不结算也不释放。
            FakeQuery::Untrusted => Ok(AccountingQuery {
                state: ProviderTaskState::Unknown,
                accounting_facts: None,
            }),
            // 失败与取消也可能带回用量和成本：状态决定收尾，不能被"有事实"翻成成功。
            FakeQuery::Failed(facts) => Ok(AccountingQuery {
                state: ProviderTaskState::Failed,
                accounting_facts: Some(facts),
            }),
            FakeQuery::Cancelled(facts) => Ok(AccountingQuery {
                state: ProviderTaskState::Cancelled,
                accounting_facts: Some(facts),
            }),
            FakeQuery::Unsupported => Err(AdapterError::QueryAccountingUnsupported),
        }
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
            "the reconciliation never runs the legacy adapter".to_owned(),
        ))
    }
}

impl AdapterFactory for FakeFactory {
    fn descriptor(&self, _adapter_key: &str) -> Option<AdapterDescriptor> {
        None
    }

    fn validate_publication(
        &self,
        _adapter_key: &str,
        _carrier_schema: &serde_json::Value,
        _restrictions: &serde_json::Value,
    ) -> Result<(), String> {
        Ok(())
    }

    fn create(
        &self,
        _adapter_key: &str,
        _base_url: &str,
        _timeout: Duration,
    ) -> Result<Arc<dyn ImageAdapter>, ApplicationError> {
        Ok(Arc::new(FakeLegacyAdapter))
    }

    fn create_gateway(
        &self,
        _adapter_key: &str,
        _base_url: &str,
        _timeout: Duration,
    ) -> Result<Arc<dyn GatewayAdapter>, ApplicationError> {
        Ok(Arc::new(FakeGateway {
            behavior: self.behavior.clone(),
            queries: self.queries.clone(),
            executes: self.executes.clone(),
        }))
    }
}

struct FakeCredentials;

impl CredentialProvider for FakeCredentials {
    fn resolve(&self, _reference: &str) -> Result<ProviderCredential, ApplicationError> {
        ProviderCredential::new("fake-credential".to_owned())
            .map_err(|error| ApplicationError::Configuration(error.to_string()))
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

fn terminal_facts() -> AccountingFacts {
    AccountingFacts {
        usage: Some(usage()),
        provider_cost: ProviderCost::Declared(DeclaredCost {
            amount_microusd: 2_000,
            currency: "USD".to_owned(),
        }),
        image_count: 1,
        response_digest: "resp-recon".to_owned(),
        provider_trace_id: Some("trace-recon".to_owned()),
    }
}

struct Fixture {
    account_id: AccountId,
    channel_id: ChannelId,
    vendor_model_id: VendorModelId,
    offering_id: OfferingId,
    runtime_revision_id: RuntimeRevisionId,
}

async fn seed_fixture(pool: &PgPool) -> Fixture {
    let fixture = Fixture {
        account_id: AccountId::new(),
        channel_id: ChannelId::new(),
        vendor_model_id: VendorModelId::new(),
        offering_id: OfferingId::new(),
        runtime_revision_id: RuntimeRevisionId::new(),
    };
    sqlx::query(
        "INSERT INTO ledger.accounts (id, balance_microusd, held_microusd, version, kind, name)
         VALUES ($1, 1000000, 0, 0, 'consumer', 'reconciliation test account')",
    )
    .bind(fixture.account_id.0)
    .execute(pool)
    .await
    .expect("seed account");
    sqlx::query(
        "INSERT INTO supply.channels (id, provider_kind, base_url, credential_env)
         VALUES ($1, 'Fake', 'http://127.0.0.1:9', 'FAKE_PROVIDER_KEY')",
    )
    .bind(fixture.channel_id.0)
    .execute(pool)
    .await
    .expect("seed channel");
    sqlx::query(
        "INSERT INTO catalog.vendor_models
             (id, vendor_id, native_model_id, native_revision, capability_schema)
         VALUES ($1, 'fake-vendor', 'fake-model', 'v1', '{}'::jsonb)",
    )
    .bind(fixture.vendor_model_id.0)
    .execute(pool)
    .await
    .expect("seed vendor model");
    sqlx::query(
        "INSERT INTO supply.offerings
             (id, vendor_model_id, channel_id, adapter_key, provider_model_id,
              carrier_schema, parameter_mapping)
         VALUES ($1, $2, $3, 'fake', 'fake-model', '{}'::jsonb, '{}'::jsonb)",
    )
    .bind(fixture.offering_id.0)
    .bind(fixture.vendor_model_id.0)
    .bind(fixture.channel_id.0)
    .execute(pool)
    .await
    .expect("seed offering");
    sqlx::query(
        "INSERT INTO publication.runtime_revisions
             (id, snapshot, published_by, gateway_model, vendor_model_id)
         VALUES ($1, '{}'::jsonb, 'recon-test', 'gw', $2)",
    )
    .bind(fixture.runtime_revision_id.0)
    .bind(fixture.vendor_model_id.0)
    .execute(pool)
    .await
    .expect("seed runtime revision");
    fixture
}

fn snapshot() -> PriceSnapshot {
    serde_json::from_value(json!({
        "captured_at": "2026-10-03T00:00:00Z",
        "hold_microusd": 1000,
        "hold_source": "platform_default",
        "formula": "token_rates",
        "cost_currency": "USD",
        "rates": {
            "currency": "USD",
            "text_input_microusd_per_million": 5000000,
            "image_input_microusd_per_million": 8000000,
            "text_output_microusd_per_million": 10000000,
            "image_output_microusd_per_million": 30000000
        }
    }))
    .expect("a frozen price snapshot")
}

async fn admit_one(repository: &PgHubRepository, fixture: &Fixture, key: &str) -> JobId {
    admit_with_snapshot(repository, fixture, key, snapshot()).await
}

async fn admit_with_snapshot(
    repository: &PgHubRepository,
    fixture: &Fixture,
    key: &str,
    price_snapshot: PriceSnapshot,
) -> JobId {
    let outcome = repository
        .admit(AdmitExecution {
            account_id: fixture.account_id,
            branch: ImageBranch::PromptOnly,
            offering: AdmitOffering {
                runtime_revision_id: fixture.runtime_revision_id,
                vendor_model_id: fixture.vendor_model_id,
                offering_id: fixture.offering_id,
                channel_id: fixture.channel_id,
                gateway_model: "gw".to_owned(),
                adapter_key: ADAPTER_KEY.to_owned(),
                provider_model_id: "fake-model".to_owned(),
                base_url: "http://127.0.0.1:9".to_owned(),
                credential_env: "FAKE_PROVIDER_KEY".to_owned(),
            },
            price_snapshot,
            routing: RoutingDecision {
                runtime_revision_id: fixture.runtime_revision_id,
                chosen_offering_id: fixture.offering_id,
                considered: Vec::new(),
            },
            idempotency_key_digest: format!("digest-{key}"),
            request_digest: format!("request-{key}"),
            request_digest_key_version: 1,
            max_cost_microusd: 1000,
            max_account_in_flight: 8,
            max_channel_in_flight: 8,
        })
        .await
        .expect("the admit");
    let AdmitOutcome::Admitted { job, .. } = outcome else {
        panic!("a fresh key must be admitted");
    };
    job.job_id
}

async fn begin(repository: &PgHubRepository, job_id: JobId) -> AttemptId {
    repository
        .begin_submission(BeginSubmission {
            job_id,
            execution_owner: "supervisor-a".to_owned(),
            fencing_token: FencingToken::new(0),
            deadline: Utc::now() + ChronoDuration::minutes(5),
            lease: ChronoDuration::minutes(5),
        })
        .await
        .expect("begin_submission")
        .attempt_id
}

async fn accept(repository: &PgHubRepository, job_id: JobId, attempt_id: AttemptId, handle: &str) {
    repository
        .record_acceptance(RecordAcceptance {
            job_id,
            attempt_id,
            execution_owner: "supervisor-a".to_owned(),
            fencing_token: FencingToken::new(0),
            provider_task_handle: Some(handle.to_owned()),
            provider_trace_id: Some("trace-accepted".to_owned()),
        })
        .await
        .expect("record_acceptance");
}

async fn expire_lease(pool: &PgPool, job_id: JobId) {
    sqlx::query(
        "UPDATE generation.jobs SET lease_expires_at = now() - interval '1 second' WHERE id = $1",
    )
    .bind(job_id.0)
    .execute(pool)
    .await
    .expect("expire the lease");
}

fn test_policy() -> ReconciliationPolicy {
    ReconciliationPolicy {
        batch_limit: 16,
        orphan_max_age: ChronoDuration::minutes(15),
        late_fact_claim_ttl: ChronoDuration::minutes(5),
        query_timeout: Duration::from_secs(5),
        query_max_attempts: 5,
        query_backoff_base: Duration::from_secs(1),
        query_backoff_max: Duration::from_secs(60),
        // 用例不跑慢周期账务核对：每 100 轮才轮一次，单轮用例因此不会因为它多读账户。
        ledger_audit_every_rounds: 100,
        ledger_audit_limit: 100,
        ledger_audit_window: Duration::from_secs(3600),
    }
}

fn reconciliation(
    repository: &Arc<PgHubRepository>,
    factory: Arc<FakeFactory>,
) -> ExecutionReconciliationService {
    reconciliation_with_policy(repository, factory, test_policy())
}

fn reconciliation_with_policy(
    repository: &Arc<PgHubRepository>,
    factory: Arc<FakeFactory>,
    policy: ReconciliationPolicy,
) -> ExecutionReconciliationService {
    ExecutionReconciliationService::new(
        repository.clone(),
        repository.clone(),
        factory,
        Arc::new(FakeCredentials),
        "recon-worker".to_owned(),
        takeover_lease(),
    )
    .with_policy(policy)
    .with_retry_policy(RetryPolicy {
        max_attempts: 2,
        backoff_base: Duration::from_millis(1),
    })
}

async fn connect() -> (Arc<PgHubRepository>, String) {
    let (database_url, database_name) = isolated_database_url().await;
    let repository = Arc::new(
        PgHubRepository::connect(&database_url, 4)
            .await
            .expect("the isolated database"),
    );
    repository.migrate().await.expect("the migrations apply");
    (repository, database_name)
}

async fn state(pool: &PgPool, job_id: JobId) -> String {
    sqlx::query_scalar("SELECT state FROM generation.jobs WHERE id = $1")
        .bind(job_id.0)
        .fetch_one(pool)
        .await
        .expect("the job state")
}

async fn attempt_state(pool: &PgPool, attempt_id: AttemptId) -> String {
    sqlx::query_scalar("SELECT state FROM generation.attempts WHERE id = $1")
        .bind(attempt_id.0)
        .fetch_one(pool)
        .await
        .expect("the attempt state")
}

async fn held(pool: &PgPool, account_id: AccountId) -> i64 {
    sqlx::query_scalar("SELECT held_microusd FROM ledger.accounts WHERE id = $1")
        .bind(account_id.0)
        .fetch_one(pool)
        .await
        .expect("the held amount")
}

async fn captures(pool: &PgPool, job_id: JobId) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM ledger.entries WHERE job_id = $1 AND kind = 'capture'")
        .bind(job_id.0)
        .fetch_one(pool)
        .await
        .expect("the capture count")
}

async fn cases(pool: &PgPool, job_id: JobId) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM operations.reconciliation_cases WHERE job_id = $1")
        .bind(job_id.0)
        .fetch_one(pool)
        .await
        .expect("the reconciliation case count")
}

async fn capacity_state(pool: &PgPool, job_id: JobId) -> String {
    sqlx::query_scalar("SELECT state FROM generation.execution_capacity WHERE job_id = $1")
        .bind(job_id.0)
        .fetch_one(pool)
        .await
        .expect("the channel slot state")
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_taken_over_handle_is_queried_read_only_and_settled_once() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "recon-settle").await;
    let attempt_id = begin(&repository, job_id).await;
    accept(&repository, job_id, attempt_id, "task-recon").await;
    expire_lease(&pool, job_id).await;

    let factory = Arc::new(FakeFactory::new());
    let service = reconciliation(&repository, factory.clone());
    let report = service.run_once().await.expect("the reconciliation round");

    assert_eq!(report.taken_over, 1);
    assert_eq!(report.queried, 1);
    assert_eq!(report.settled, 1);
    assert_eq!(report.reconciled, 0);
    assert_eq!(state(&pool, job_id).await, "succeeded");
    assert_eq!(attempt_state(&pool, attempt_id).await, "terminal");
    assert_eq!(captures(&pool, job_id).await, 1, "the ledger captures once");
    assert_eq!(held(&pool, fixture.account_id).await, 0, "the hold settles");
    assert_eq!(capacity_state(&pool, job_id).await, "released");
    assert_eq!(factory.queries.load(Ordering::SeqCst), 1);
    assert_eq!(
        factory.executes.load(Ordering::SeqCst),
        0,
        "the worker never resubmits a generation request"
    );

    // 再跑一轮：已终结的执行不再被接管、不再查询、不再扣费。
    let second = service.run_once().await.expect("the second round");
    assert_eq!(second.taken_over, 0);
    assert_eq!(factory.queries.load(Ordering::SeqCst), 1);
    assert_eq!(captures(&pool, job_id).await, 1);

    drop(pool);
    drop(service);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

async fn cost_source(pool: &PgPool, attempt_id: AttemptId) -> Option<String> {
    sqlx::query_scalar("SELECT provider_cost_source FROM generation.attempts WHERE id = $1")
        .bind(attempt_id.0)
        .fetch_one(pool)
        .await
        .expect("the attempt cost source")
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_taken_over_failed_task_releases_the_hold_without_charging() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "recon-failed").await;
    let attempt_id = begin(&repository, job_id).await;
    accept(&repository, job_id, attempt_id, "task-failed").await;
    expire_lease(&pool, job_id).await;

    let factory = Arc::new(FakeFactory::new());
    factory.set(FakeQuery::Failed(terminal_facts()));
    let service = reconciliation(&repository, factory.clone());
    let report = service.run_once().await.expect("the reconciliation round");

    assert_eq!(report.failed, 1);
    assert_eq!(
        report.settled, 0,
        "an upstream failure is never settled as a success, even with usage attached"
    );
    assert_eq!(
        report.reconciled, 0,
        "a confirmed failure is not an unknown outcome"
    );
    assert_eq!(state(&pool, job_id).await, "failed");
    assert_eq!(attempt_state(&pool, attempt_id).await, "terminal");
    assert_eq!(
        captures(&pool, job_id).await,
        0,
        "the consumer is not charged"
    );
    assert_eq!(
        held(&pool, fixture.account_id).await,
        0,
        "the hold is released"
    );
    assert_eq!(capacity_state(&pool, job_id).await, "released");
    assert_eq!(
        cost_source(&pool, attempt_id).await.as_deref(),
        Some("declared"),
        "the upstream cost the failure carried is still recorded"
    );
    assert_eq!(
        factory.executes.load(Ordering::SeqCst),
        0,
        "the worker never resubmits a generation request"
    );

    drop(pool);
    drop(service);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_taken_over_cancelled_task_releases_the_hold_without_charging() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "recon-cancelled").await;
    let attempt_id = begin(&repository, job_id).await;
    accept(&repository, job_id, attempt_id, "task-cancelled").await;
    expire_lease(&pool, job_id).await;

    let factory = Arc::new(FakeFactory::new());
    factory.set(FakeQuery::Cancelled(terminal_facts()));
    let service = reconciliation(&repository, factory.clone());
    let report = service.run_once().await.expect("the reconciliation round");

    assert_eq!(report.failed, 1);
    assert_eq!(report.settled, 0);
    assert_eq!(state(&pool, job_id).await, "failed");
    assert_eq!(
        captures(&pool, job_id).await,
        0,
        "the consumer is not charged"
    );
    assert_eq!(
        held(&pool, fixture.account_id).await,
        0,
        "the hold is released"
    );
    assert_eq!(capacity_state(&pool, job_id).await, "released");

    drop(pool);
    drop(service);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn an_untrusted_task_state_opens_a_case_and_keeps_the_hold() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "recon-untrusted").await;
    let attempt_id = begin(&repository, job_id).await;
    accept(&repository, job_id, attempt_id, "task-untrusted").await;
    expire_lease(&pool, job_id).await;

    let factory = Arc::new(FakeFactory::new());
    factory.set(FakeQuery::Untrusted);
    let service = reconciliation(&repository, factory);
    let report = service.run_once().await.expect("the reconciliation round");

    assert_eq!(
        report.reconciled, 1,
        "a state that cannot be tied to the handle must enter reconciliation, not settle"
    );
    assert_eq!(report.settled, 0);
    assert_eq!(
        report.failed, 0,
        "an untrusted state is not a confirmed failure"
    );
    assert_eq!(state(&pool, job_id).await, "reconciliation_required");
    assert_eq!(attempt_state(&pool, attempt_id).await, "unknown");
    assert_eq!(cases(&pool, job_id).await, 1, "a case is opened");
    assert_eq!(
        held(&pool, fixture.account_id).await,
        1_000,
        "the hold is retained"
    );
    assert_eq!(capacity_state(&pool, job_id).await, "held");
    assert_eq!(captures(&pool, job_id).await, 0);

    drop(pool);
    drop(service);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_taken_over_execution_without_a_handle_opens_a_case_and_keeps_the_hold() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "recon-no-handle").await;
    let attempt_id = begin(&repository, job_id).await;
    expire_lease(&pool, job_id).await;

    let factory = Arc::new(FakeFactory::new());
    let service = reconciliation(&repository, factory.clone());
    let report = service.run_once().await.expect("the reconciliation round");

    assert_eq!(report.reconciled, 1);
    assert_eq!(report.settled, 0);
    assert_eq!(state(&pool, job_id).await, "reconciliation_required");
    assert_eq!(attempt_state(&pool, attempt_id).await, "unknown");
    assert_eq!(cases(&pool, job_id).await, 1, "a case is opened");
    assert_eq!(
        held(&pool, fixture.account_id).await,
        1_000,
        "the hold is retained"
    );
    assert_eq!(capacity_state(&pool, job_id).await, "held");
    assert_eq!(captures(&pool, job_id).await, 0);
    assert_eq!(
        factory.queries.load(Ordering::SeqCst),
        0,
        "no handle means no read-only query"
    );
    assert_eq!(factory.executes.load(Ordering::SeqCst), 0);

    drop(pool);
    drop(service);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_terminal_query_without_evidence_records_a_cost_gap() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "recon-gap").await;
    let attempt_id = begin(&repository, job_id).await;
    accept(&repository, job_id, attempt_id, "task-gap").await;
    expire_lease(&pool, job_id).await;

    let factory = Arc::new(FakeFactory::new());
    factory.set(FakeQuery::TerminalWithoutFacts);
    let service = reconciliation(&repository, factory);
    let report = service.run_once().await.expect("the reconciliation round");

    assert_eq!(report.reconciled, 1);
    assert_eq!(report.settled, 0);
    assert_eq!(state(&pool, job_id).await, "reconciliation_required");
    assert_eq!(attempt_state(&pool, attempt_id).await, "unknown");
    let source: String =
        sqlx::query_scalar("SELECT provider_cost_source FROM generation.attempts WHERE id = $1")
            .bind(attempt_id.0)
            .fetch_one(&pool)
            .await
            .expect("the attempt cost source");
    assert_eq!(
        source, "unavailable",
        "missing evidence lands as a cost gap"
    );
    let gaps = repository
        .provider_cost_gaps(10)
        .await
        .expect("the provider cost gaps");
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].job_id, job_id);
    assert_eq!(held(&pool, fixture.account_id).await, 1_000);
    assert_eq!(captures(&pool, job_id).await, 0);

    drop(pool);
    drop(service);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_channel_without_read_only_query_only_opens_a_case() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "recon-unsupported").await;
    let attempt_id = begin(&repository, job_id).await;
    accept(&repository, job_id, attempt_id, "task-unsupported").await;
    expire_lease(&pool, job_id).await;

    let factory = Arc::new(FakeFactory::new());
    factory.set(FakeQuery::Unsupported);
    let service = reconciliation(&repository, factory.clone());
    let report = service.run_once().await.expect("the reconciliation round");

    assert_eq!(report.reconciled, 1);
    assert_eq!(
        report.queried, 0,
        "an unsupported capability is rejected before any read-only query"
    );
    assert_eq!(state(&pool, job_id).await, "reconciliation_required");
    assert_eq!(attempt_state(&pool, attempt_id).await, "unknown");
    assert_eq!(
        cases(&pool, job_id).await,
        1,
        "an unsupported channel only opens a case"
    );
    assert_eq!(held(&pool, fixture.account_id).await, 1_000);
    assert_eq!(factory.queries.load(Ordering::SeqCst), 0);
    assert_eq!(factory.executes.load(Ordering::SeqCst), 0);

    drop(pool);
    drop(service);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_late_accounting_fact_settles_and_is_consumed() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "recon-late-accounting").await;
    let attempt_id = begin(&repository, job_id).await;
    repository
        .offer_late_facts(LateFacts {
            job_id,
            attempt_id,
            provider_task_handle: None,
            provider_trace_id: Some("trace-late".to_owned()),
            image_count: None,
            evidence: Some(MeteringEvidence {
                attempt_id,
                provider_response_digest: "resp-late".to_owned(),
                usage: usage(),
            }),
            provider_state: Some(ProviderTaskState::Succeeded),
            provider_cost: Some(ProviderCostFact {
                source: ProviderCostSource::Declared,
                amount_microusd: Some(200),
                currency: Some("USD".to_owned()),
                cny_microusd: None,
            }),
        })
        .await
        .expect("offer the late accounting fact");
    expire_lease(&pool, job_id).await;

    let factory = Arc::new(FakeFactory::new());
    let service = reconciliation(&repository, factory);
    let report = service.run_once().await.expect("the reconciliation round");

    assert_eq!(report.late_facts_consumed, 1);
    assert_eq!(report.settled, 1);
    assert_eq!(state(&pool, job_id).await, "succeeded");
    assert_eq!(captures(&pool, job_id).await, 1);
    assert_eq!(held(&pool, fixture.account_id).await, 0);
    let consumed: Option<chrono::DateTime<Utc>> =
        sqlx::query_scalar("SELECT consumed_at FROM generation.late_facts WHERE job_id = $1")
            .bind(job_id.0)
            .fetch_one(&pool)
            .await
            .expect("the consumed timestamp");
    assert!(consumed.is_some(), "a settled late fact is consumed");

    drop(pool);
    drop(service);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn an_unfinished_late_handle_is_not_consumed_and_is_reclaimable_after_the_ttl() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "recon-late-handle").await;
    let attempt_id = begin(&repository, job_id).await;
    repository
        .offer_late_facts(LateFacts {
            job_id,
            attempt_id,
            provider_task_handle: Some("task-unfinished".to_owned()),
            provider_trace_id: Some("trace-unfinished".to_owned()),
            image_count: None,
            evidence: None,
            provider_cost: None,
            provider_state: None,
        })
        .await
        .expect("offer the late task handle");
    expire_lease(&pool, job_id).await;

    let factory = Arc::new(FakeFactory::new());
    factory.set(FakeQuery::NotTerminal);
    let service = reconciliation(&repository, factory);
    let report = service.run_once().await.expect("the reconciliation round");

    assert_eq!(
        report.late_facts_consumed, 0,
        "a handle whose task is not terminal must not be marked consumed"
    );
    let row =
        sqlx::query("SELECT claimed_by, consumed_at FROM generation.late_facts WHERE job_id = $1")
            .bind(job_id.0)
            .fetch_one(&pool)
            .await
            .expect("the late fact row");
    assert_eq!(
        row.try_get::<Option<String>, _>("claimed_by")
            .expect("claimed_by")
            .as_deref(),
        Some("recon-worker"),
        "the round claims the fact but does not consume it"
    );
    assert!(
        row.try_get::<Option<chrono::DateTime<Utc>>, _>("consumed_at")
            .expect("consumed_at")
            .is_none()
    );
    // 领取 TTL 到期后可被另一个领取者重领（零 TTL 表示立即视为过期）。
    let reclaimed = repository
        .claim_unconsumed_late_facts("worker-2", 10, ChronoDuration::zero())
        .await
        .expect("reclaim");
    assert_eq!(reclaimed.len(), 1);

    drop(pool);
    drop(service);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_late_handle_whose_task_failed_releases_the_hold_without_charging() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "recon-late-failed").await;
    let attempt_id = begin(&repository, job_id).await;
    repository
        .offer_late_facts(LateFacts {
            job_id,
            attempt_id,
            provider_task_handle: Some("task-failed".to_owned()),
            provider_trace_id: Some("trace-failed".to_owned()),
            image_count: None,
            evidence: None,
            provider_cost: None,
            provider_state: None,
        })
        .await
        .expect("offer the late task handle");
    expire_lease(&pool, job_id).await;

    let factory = Arc::new(FakeFactory::new());
    factory.set(FakeQuery::Failed(terminal_facts()));
    let service = reconciliation(&repository, factory);
    let report = service.run_once().await.expect("the reconciliation round");

    assert_eq!(report.failed, 1);
    assert_eq!(report.settled, 0, "a failed task is never charged");
    assert_eq!(
        report.late_facts_consumed, 1,
        "once the outcome is known the handle fact is consumed"
    );
    assert_eq!(state(&pool, job_id).await, "failed");
    assert_eq!(
        captures(&pool, job_id).await,
        0,
        "the consumer is not charged"
    );
    assert_eq!(
        held(&pool, fixture.account_id).await,
        0,
        "the hold is released"
    );

    drop(pool);
    drop(service);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn an_orphan_admission_is_reaped_and_releases_hold_and_slot() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "recon-orphan").await;
    sqlx::query("UPDATE generation.jobs SET created_at = now() - interval '1 hour' WHERE id = $1")
        .bind(job_id.0)
        .execute(&pool)
        .await
        .expect("age the admission");

    let factory = Arc::new(FakeFactory::new());
    let service = reconciliation(&repository, factory);
    let report = service.run_once().await.expect("the reconciliation round");

    assert_eq!(report.reaped_orphans, 1);
    assert_eq!(state(&pool, job_id).await, "failed");
    assert_eq!(
        held(&pool, fixture.account_id).await,
        0,
        "the hold is released"
    );
    assert_eq!(capacity_state(&pool, job_id).await, "released");

    drop(pool);
    drop(service);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

/// 按张计价的冻结快照：成本单价 700 USD 微单位/张，对客形态缺省等于成本形态（per_image）。
fn per_image_snapshot() -> PriceSnapshot {
    serde_json::from_value(json!({
        "captured_at": "2026-10-03T00:00:00Z",
        "hold_microusd": 1000,
        "hold_source": "platform_default",
        "formula": "per_image",
        "cost_currency": "USD",
        "cost_unit_price_microusd": 700
    }))
    .expect("a per image price snapshot")
}

async fn query_attempts(pool: &PgPool, job_id: JobId) -> i32 {
    sqlx::query_scalar("SELECT attempts FROM operations.reconciliation_cases WHERE job_id = $1")
        .bind(job_id.0)
        .fetch_one(pool)
        .await
        .expect("the query attempts")
}

async fn next_query_at(pool: &PgPool, job_id: JobId) -> Option<chrono::DateTime<Utc>> {
    sqlx::query_scalar(
        "SELECT next_query_at FROM operations.reconciliation_cases WHERE job_id = $1",
    )
    .bind(job_id.0)
    .fetch_one(pool)
    .await
    .expect("the next query time")
}

async fn make_query_due(pool: &PgPool, job_id: JobId) {
    sqlx::query(
        "UPDATE operations.reconciliation_cases SET next_query_at = now() - interval '1 second' WHERE job_id = $1",
    )
    .bind(job_id.0)
    .execute(pool)
    .await
    .expect("make the next query due");
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn late_handle_queries_back_off_and_stop_at_the_retry_cap() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "recon-schedule").await;
    let attempt_id = begin(&repository, job_id).await;
    repository
        .offer_late_facts(LateFacts {
            job_id,
            attempt_id,
            provider_task_handle: Some("task-schedule".to_owned()),
            provider_trace_id: Some("trace-schedule".to_owned()),
            image_count: None,
            evidence: None,
            provider_cost: None,
            provider_state: None,
        })
        .await
        .expect("offer the late handle");

    let factory = Arc::new(FakeFactory::new());
    factory.set(FakeQuery::NotTerminal);
    let mut policy = test_policy();
    policy.query_max_attempts = 2;
    policy.query_backoff_base = Duration::from_secs(3600);
    policy.query_backoff_max = Duration::from_secs(3600);
    // 领取 TTL 归零，晚到事实每轮都可被同一 worker 重领，方便在一轮里推进查询排期。
    policy.late_fact_claim_ttl = ChronoDuration::zero();
    let service = reconciliation_with_policy(&repository, factory.clone(), policy);

    // 第一轮：接管、查询一次，并把下一次查询排到一个小时以后。
    expire_lease(&pool, job_id).await;
    let first = service.run_once().await.expect("the first round");
    assert_eq!(first.taken_over, 1);
    assert_eq!(first.queried, 1);
    assert_eq!(query_attempts(&pool, job_id).await, 1);
    assert!(next_query_at(&pool, job_id).await.is_some());

    // 第二轮：租约虽然过期，但还没到 next_query_at，本轮跳过、不再查询。
    expire_lease(&pool, job_id).await;
    let second = service.run_once().await.expect("the second round");
    assert_eq!(second.taken_over, 0, "a case that is not due is skipped");
    assert_eq!(factory.queries.load(Ordering::SeqCst), 1);

    // 到期后再查一次，正好用掉额度（上限 2）。
    make_query_due(&pool, job_id).await;
    expire_lease(&pool, job_id).await;
    let third = service.run_once().await.expect("the third round");
    assert_eq!(third.taken_over, 1);
    assert_eq!(query_attempts(&pool, job_id).await, 2);
    assert_eq!(factory.queries.load(Ordering::SeqCst), 2);

    // 额度用尽：即使到期也不再自动接管/查询，转人工。
    make_query_due(&pool, job_id).await;
    expire_lease(&pool, job_id).await;
    let fourth = service.run_once().await.expect("the fourth round");
    assert_eq!(fourth.taken_over, 0, "the query budget is exhausted");
    assert_eq!(factory.queries.load(Ordering::SeqCst), 2);

    drop(pool);
    drop(service);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_terminal_late_cost_lands_in_the_cost_gap_without_reopening_the_job() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "recon-terminal-cost").await;
    let attempt_id = begin(&repository, job_id).await;
    // 确定失败：请求根本没交到渠道，成本四列留空，Job 已收成 failed 终态。
    repository
        .fail_or_reconcile(FailOrReconcileExecution::for_failure(
            job_id,
            attempt_id,
            "supervisor-a".to_owned(),
            FencingToken::new(0),
            ProviderFailureKind::PlatformInternal,
            FailureDisposition::DeterminedFailure,
            None,
            None,
        ))
        .await
        .expect("the determined failure");
    assert_eq!(state(&pool, job_id).await, "failed");

    repository
        .offer_late_facts(LateFacts {
            job_id,
            attempt_id,
            provider_task_handle: None,
            provider_trace_id: Some("trace-terminal".to_owned()),
            image_count: None,
            evidence: None,
            provider_state: None,
            provider_cost: Some(ProviderCostFact {
                source: ProviderCostSource::Unavailable,
                amount_microusd: None,
                currency: None,
                cny_microusd: None,
            }),
        })
        .await
        .expect("offer the late cost gap");

    let factory = Arc::new(FakeFactory::new());
    let service = reconciliation(&repository, factory);
    let report = service.run_once().await.expect("the round");

    assert_eq!(report.late_facts_consumed, 1);
    assert_eq!(
        state(&pool, job_id).await,
        "failed",
        "a late cost never reopens the terminal state"
    );
    assert_eq!(attempt_state(&pool, attempt_id).await, "terminal");
    let source: Option<String> =
        sqlx::query_scalar("SELECT provider_cost_source FROM generation.attempts WHERE id = $1")
            .bind(attempt_id.0)
            .fetch_one(&pool)
            .await
            .expect("the cost source");
    assert_eq!(source.as_deref(), Some("unavailable"));
    let gaps = repository
        .provider_cost_gaps(10)
        .await
        .expect("the provider cost gaps");
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].job_id, job_id);

    drop(pool);
    drop(service);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_per_image_late_fact_uses_a_known_count_and_gaps_when_it_is_missing() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let factory = Arc::new(FakeFactory::new());
    // 查询停在非终态，让晚到事实去把执行转对账；本用例考的是按张计价而不是查询本身。
    factory.set(FakeQuery::NotTerminal);
    let service = reconciliation(&repository, factory);

    // 缺产出张数：按张计价算不出成本，落缺口、转对账，不按 0 结算。
    let gap_job = admit_with_snapshot(
        &repository,
        &fixture,
        "recon-per-image-gap",
        per_image_snapshot(),
    )
    .await;
    let gap_attempt = begin(&repository, gap_job).await;
    accept(&repository, gap_job, gap_attempt, "task-per-image-gap").await;
    repository
        .offer_late_facts(LateFacts {
            job_id: gap_job,
            attempt_id: gap_attempt,
            provider_task_handle: None,
            provider_trace_id: Some("trace-gap".to_owned()),
            image_count: None,
            evidence: Some(MeteringEvidence {
                attempt_id: gap_attempt,
                provider_response_digest: "resp-gap".to_owned(),
                usage: usage(),
            }),
            provider_cost: None,
            provider_state: None,
        })
        .await
        .expect("offer the gapped late fact");
    expire_lease(&pool, gap_job).await;
    let gap_report = service.run_once().await.expect("the gap round");
    assert_eq!(gap_report.late_facts_consumed, 1);
    assert_eq!(
        gap_report.settled, 0,
        "a per-image charge with no count must not settle"
    );
    assert_eq!(state(&pool, gap_job).await, "reconciliation_required");
    assert_eq!(captures(&pool, gap_job).await, 0);
    let gap_source: Option<String> =
        sqlx::query_scalar("SELECT provider_cost_source FROM generation.attempts WHERE id = $1")
            .bind(gap_attempt.0)
            .fetch_one(&pool)
            .await
            .expect("the gap source");
    assert_eq!(gap_source.as_deref(), Some("unavailable"));

    // 带产出张数：成本按张算得出（700 × 2），但仍然算不出对客实收（对客形态也是按张），
    // 所以仍转对账、不按 0 收费；成本来源是 computed，不是缺口。
    let count_job = admit_with_snapshot(
        &repository,
        &fixture,
        "recon-per-image-count",
        per_image_snapshot(),
    )
    .await;
    let count_attempt = begin(&repository, count_job).await;
    accept(
        &repository,
        count_job,
        count_attempt,
        "task-per-image-count",
    )
    .await;
    repository
        .offer_late_facts(LateFacts {
            job_id: count_job,
            attempt_id: count_attempt,
            provider_task_handle: None,
            provider_trace_id: Some("trace-count".to_owned()),
            image_count: Some(2),
            evidence: Some(MeteringEvidence {
                attempt_id: count_attempt,
                provider_response_digest: "resp-count".to_owned(),
                usage: usage(),
            }),
            provider_cost: None,
            provider_state: None,
        })
        .await
        .expect("offer the counted late fact");
    expire_lease(&pool, count_job).await;
    let count_report = service.run_once().await.expect("the count round");
    assert_eq!(count_report.late_facts_consumed, 1);
    assert_eq!(count_report.settled, 0);
    assert_eq!(captures(&pool, count_job).await, 0);
    let count_source: Option<String> =
        sqlx::query_scalar("SELECT provider_cost_source FROM generation.attempts WHERE id = $1")
            .bind(count_attempt.0)
            .fetch_one(&pool)
            .await
            .expect("the count source");
    assert_eq!(count_source.as_deref(), Some("computed"));
    let amount: Option<i64> =
        sqlx::query_scalar("SELECT provider_cost_microusd FROM generation.attempts WHERE id = $1")
            .bind(count_attempt.0)
            .fetch_one(&pool)
            .await
            .expect("the counted cost");
    assert_eq!(amount, Some(1_400), "700 per image times two images");

    drop(pool);
    drop(service);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn api_and_worker_finalizations_charge_at_most_once() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;

    // 竞争件：S3 直接执行的收尾入口与 Worker 的接管查询同一条 Attempt 竞争。
    let job_id = admit_one(&repository, &fixture, "recon-competition").await;
    let attempt_id = begin(&repository, job_id).await;
    accept(&repository, job_id, attempt_id, "task-competition").await;
    expire_lease(&pool, job_id).await;
    let api_command = SettleExecution {
        job_id,
        attempt_id,
        execution_owner: "supervisor-a".to_owned(),
        fencing_token: FencingToken::new(0),
        evidence: MeteringEvidence {
            attempt_id,
            provider_response_digest: "resp-api".to_owned(),
            usage: usage(),
        },
        provider_cost: ProviderCostFact {
            source: ProviderCostSource::Declared,
            amount_microusd: Some(2_000),
            currency: Some("USD".to_owned()),
            cny_microusd: None,
        },
        charge_microusd: snapshot()
            .charge_microusd(ChargeFacts {
                usage: &usage(),
                images: 1,
                declared_cost_microusd: Some(2_000),
            })
            .expect("the API charge"),
        image_count: Some(1),
        provider_trace_id: Some("trace-api".to_owned()),
    };

    let factory = Arc::new(FakeFactory::new());
    let service = reconciliation(&repository, factory);
    let (worker_result, api_result) =
        tokio::join!(service.run_once(), repository.settle(api_command.clone()));
    // 谁先提交都行，另一边要么被终态挡下、要么所有权冲突；账上只允许一条 capture。
    let _ = worker_result;
    let _ = api_result;
    assert_eq!(
        captures(&pool, job_id).await,
        1,
        "API/Worker competition captures exactly once"
    );
    assert_eq!(state(&pool, job_id).await, "succeeded");
    assert_eq!(held(&pool, fixture.account_id).await, 0);

    // 重复收尾：同一条已提交的 API 收尾再调一次，回原结果、不再扣费。
    let duplicate_job = admit_one(&repository, &fixture, "recon-duplicate").await;
    let duplicate_attempt = begin(&repository, duplicate_job).await;
    let mut duplicate = api_command.clone();
    duplicate.job_id = duplicate_job;
    duplicate.attempt_id = duplicate_attempt;
    duplicate.evidence.attempt_id = duplicate_attempt;
    repository
        .settle(duplicate.clone())
        .await
        .expect("the first finalization");
    repository
        .settle(duplicate)
        .await
        .expect("the repeated finalization returns the committed result");
    assert_eq!(captures(&pool, duplicate_job).await, 1);

    drop(pool);
    drop(service);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

/// 对账 Worker 的接管租约：服务的租赁时长，也用来把 `lease_expires_at` 反解成接管时刻。
fn takeover_lease() -> ChronoDuration {
    ChronoDuration::minutes(5)
}

/// 一组对账延迟样本的均值与 p50/p95（排序后取向上取整那一档，与性能基线同一口径）。
fn latency_summary(
    mut samples: Vec<ChronoDuration>,
) -> (ChronoDuration, ChronoDuration, ChronoDuration) {
    samples.sort();
    let count = i32::try_from(samples.len()).expect("the sample count fits");
    let mean = samples.iter().copied().sum::<ChronoDuration>() / count;
    let percentile = |p: f64| {
        let index = (samples.len() as f64 * p).ceil() as usize - 1;
        samples[index.min(samples.len() - 1)]
    };
    (mean, percentile(0.50), percentile(0.95))
}

/// A8：一次「Worker 接管到结算完成」的本机观测（对账延迟）。
///
/// 用现有接管夹具跑 [`ROUNDS`] 轮：每轮受理一条 v1 执行、记受理句柄、把租约置为过期，再跑一轮
/// `run_once`。接管把 `lease_expires_at` 写成「接管事务开始 + 租约」，结算把 `terminal_at` 写成
/// 结算事务开始，所以 `terminal_at − (lease_expires_at − 租约)` 就是接管到结算的数据库时钟差；
/// 结算不清 `lease_expires_at`，一轮里也只有一次接管。墙钟耗时是交叉核对，含 `run_once` 的外围
/// 扫描。只记录，不断言阈值。
#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_taken_over_execution_reports_settlement_latency() {
    const ROUNDS: usize = 20;

    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let factory = Arc::new(FakeFactory::new());
    let service = reconciliation(&repository, factory);

    let mut database_latencies = Vec::with_capacity(ROUNDS);
    let mut wall_latencies = Vec::with_capacity(ROUNDS);
    for index in 0..ROUNDS {
        let job_id = admit_one(&repository, &fixture, &format!("recon-latency-{index}")).await;
        let attempt_id = begin(&repository, job_id).await;
        accept(&repository, job_id, attempt_id, "task-latency").await;
        expire_lease(&pool, job_id).await;

        let began = std::time::Instant::now();
        let report = service.run_once().await.expect("the reconciliation round");
        wall_latencies.push(
            ChronoDuration::from_std(began.elapsed())
                .expect("the round finishes within the chrono range"),
        );
        assert_eq!(
            report.taken_over, 1,
            "round {index} takes over one execution"
        );
        assert_eq!(report.settled, 1, "round {index} settles the execution");

        let lease_expires_at: chrono::DateTime<Utc> =
            sqlx::query_scalar("SELECT lease_expires_at FROM generation.jobs WHERE id = $1")
                .bind(job_id.0)
                .fetch_one(&pool)
                .await
                .expect("the takeover lease");
        let terminal_at: chrono::DateTime<Utc> =
            sqlx::query_scalar("SELECT terminal_at FROM generation.jobs WHERE id = $1")
                .bind(job_id.0)
                .fetch_one(&pool)
                .await
                .expect("the settlement timestamp");
        database_latencies.push(terminal_at - (lease_expires_at - takeover_lease()));
    }

    let (mean, p50, p95) = latency_summary(database_latencies);
    let (wall_mean, _, _) = latency_summary(wall_latencies);
    println!(
        "reconciliation-takeover-to-settlement: n={ROUNDS} mean={:.1}ms p50={:.1}ms p95={:.1}ms wall_mean={:.1}ms",
        mean.as_seconds_f64() * 1_000.0,
        p50.as_seconds_f64() * 1_000.0,
        p95.as_seconds_f64() * 1_000.0,
        wall_mean.as_seconds_f64() * 1_000.0,
    );

    drop(pool);
    drop(service);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

/// 不是标识的句柄在存储层就写不进去（0036 的 CHECK）：即使绕过应用直接 UPDATE 也会被拒。
///
/// 对账的读侧另有一道校验，服务的是**约束生效之前**写入的旧行；约束生效后新值不可能非法，
/// 因此这里断言的是数据库这一层，而不是同一条不可达的读路径。
#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_stored_handle_that_is_not_an_identifier_cannot_be_written() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "recon-bad-handle").await;
    let attempt_id = begin(&repository, job_id).await;
    accept(&repository, job_id, attempt_id, "task-legacy").await;

    for bad in [
        "https://example.invalid/a.png",
        "data:image/png;base64,AAAA",
        "task id with spaces",
    ] {
        let refused =
            sqlx::query("UPDATE generation.jobs SET provider_task_handle = $2 WHERE id = $1")
                .bind(job_id.0)
                .bind(bad)
                .execute(&pool)
                .await;
        assert!(refused.is_err(), "{bad:?} must not be storable as a handle");
    }
    let stored: Option<String> =
        sqlx::query_scalar("SELECT provider_task_handle FROM generation.jobs WHERE id = $1")
            .bind(job_id.0)
            .fetch_one(&pool)
            .await
            .expect("the stored handle");
    assert_eq!(
        stored.as_deref(),
        Some("task-legacy"),
        "a refused write leaves the original value untouched"
    );

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}
