//! 晚到事实收件要对着**真库**验：只写最小收件行、不改所有权与终态、同内容幂等、异内容建案。
//! 这些都是行内的库层判据（Spec 0005 §2、§5；RFC 0017 §3）。
//!
//! 用例从 HTTP_CONTRACT_DATABASE_URL 派生一次性库，跑完整迁移后受理、提交，再交付晚到句柄或
//! 账务事实；跑完删库，不动基库。

use chrono::{Duration as ChronoDuration, Utc};
use seeai_application::{
    AdmitExecution, AdmitOffering, AdmitOutcome, BeginSubmission, ExecutionRepository,
    LateFactKind, LateFacts, LateFactsOutcome, RoutingDecision,
};
use seeai_domain::{
    AccountId, AttemptId, ChannelId, FencingToken, ImageBranch, JobId, MeteringEvidence,
    OfferingId, PriceSnapshot, ProviderCostFact, ProviderCostSource, ProviderTaskHandle,
    ProviderTraceId, ReceiptCredential, RuntimeRevisionId, TokenUsage, VendorModelId,
};
use seeai_persistence::PgHubRepository;
use serde_json::json;
use sqlx::{AssertSqlSafe, PgPool, Row};
use uuid::Uuid;

async fn isolated_database_url() -> (String, String) {
    let base = std::env::var("HTTP_CONTRACT_DATABASE_URL")
        .expect("HTTP_CONTRACT_DATABASE_URL is required for the ignored late-facts test");
    let admin = PgPool::connect(&base)
        .await
        .expect("connect to the provided contract database");
    let name = format!("seeai_late_{}", Uuid::new_v4().simple());
    sqlx::query(AssertSqlSafe(format!("CREATE DATABASE \"{name}\"")))
        .execute(&admin)
        .await
        .expect("create an isolated late-facts database");
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

async fn connect() -> (PgHubRepository, String) {
    let (database_url, database_name) = isolated_database_url().await;
    let repository = PgHubRepository::connect(&database_url, 4)
        .await
        .expect("the isolated database");
    repository.migrate().await.expect("the migrations apply");
    (repository, database_name)
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
         VALUES ($1, 100000, 0, 0, 'consumer', 'late facts account')",
    )
    .bind(fixture.account_id.0)
    .execute(pool)
    .await
    .expect("seed account");
    sqlx::query(
        "INSERT INTO supply.channels (id, provider_kind, base_url, credential_env)
         VALUES ($1, 'fake', 'http://127.0.0.1:9', 'FAKE_PROVIDER_KEY')",
    )
    .bind(fixture.channel_id.0)
    .execute(pool)
    .await
    .expect("seed channel");
    sqlx::query(
        "INSERT INTO catalog.vendor_models
             (id, vendor_id, native_model_id, native_revision, model_type, capability_schema)
         VALUES ($1, 'fake-vendor', 'fake-model', 'v1', 'image', '{}'::jsonb)",
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
         VALUES ($1, '{}'::jsonb, 'late-test', 'fake-gateway', $2)",
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
        "formula": "token_rates"
    }))
    .expect("a frozen price snapshot")
}

async fn admit_one(repository: &PgHubRepository, fixture: &Fixture, key: &str) -> JobId {
    let outcome = repository
        .admit(AdmitExecution {
            account_id: fixture.account_id,
            branch: ImageBranch::PromptOnly,
            offering: AdmitOffering {
                runtime_revision_id: fixture.runtime_revision_id,
                vendor_model_id: fixture.vendor_model_id,
                offering_id: fixture.offering_id,
                channel_id: fixture.channel_id,
                gateway_model: "fake-gateway".to_owned(),
                adapter_key: "fake".to_owned(),
                provider_model_id: "fake-model".to_owned(),
                base_url: "http://127.0.0.1:9".to_owned(),
                credential_env: "FAKE_PROVIDER_KEY".to_owned(),
            },
            price_snapshot: snapshot(),
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

async fn begin(repository: &PgHubRepository, job_id: JobId) -> (AttemptId, ReceiptCredential) {
    let started = repository
        .begin_submission(BeginSubmission {
            job_id,
            execution_owner: "supervisor-a".to_owned(),
            fencing_token: FencingToken::new(0),
            deadline: Utc::now() + ChronoDuration::minutes(5),
            lease: ChronoDuration::minutes(5),
        })
        .await
        .expect("begin_submission");
    (started.attempt_id, started.receipt_credential)
}

fn facts(job_id: JobId, attempt_id: AttemptId, credential: &ReceiptCredential) -> LateFacts {
    LateFacts {
        job_id,
        attempt_id,
        receipt_credential: credential.clone(),
        provider_task_handle: None,
        provider_trace_id: Some(ProviderTraceId::parse("trace-late").expect("test trace")),
        image_count: None,
        evidence: None,
        provider_cost: None,
        provider_state: None,
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

async fn count_facts(pool: &PgPool, job_id: Uuid, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM generation.late_facts WHERE job_id = $1 AND kind = $2")
        .bind(job_id)
        .bind(kind)
        .fetch_one(pool)
        .await
        .expect("count late facts")
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_late_task_handle_is_received_once_and_keeps_ownership_untouched() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "late-handle").await;
    let (attempt_id, credential) = begin(&repository, job_id).await;

    let mut late = facts(job_id, attempt_id, &credential);
    late.provider_task_handle =
        Some(ProviderTaskHandle::parse("task-9".to_owned()).expect("test handle"));
    late.provider_trace_id = Some(ProviderTraceId::parse("trace-9").expect("test trace"));
    assert_eq!(
        repository
            .offer_late_facts(late.clone())
            .await
            .expect("offer"),
        LateFactsOutcome::Received
    );
    assert_eq!(count_facts(&pool, job_id.0, "task_handle").await, 1);

    // 所有权与状态不被收件改动。
    let job = sqlx::query(
        "SELECT state, execution_owner, terminal_at FROM generation.jobs WHERE id = $1",
    )
    .bind(job_id.0)
    .fetch_one(&pool)
    .await
    .expect("the job");
    assert_eq!(
        job.try_get::<String, _>("state").expect("state"),
        "executing"
    );
    assert_eq!(
        job.try_get::<Option<String>, _>("execution_owner")
            .expect("owner")
            .as_deref(),
        Some("supervisor-a")
    );
    assert!(
        job.try_get::<Option<chrono::DateTime<Utc>>, _>("terminal_at")
            .expect("terminal")
            .is_none()
    );

    // 同内容重复：幂等，不重复写。
    assert_eq!(
        repository.offer_late_facts(late).await.expect("repeat"),
        LateFactsOutcome::Received
    );
    assert_eq!(count_facts(&pool, job_id.0, "task_handle").await, 1);

    // 异内容：建案，不覆盖原收件。
    let mut different = facts(job_id, attempt_id, &credential);
    different.provider_task_handle =
        Some(ProviderTaskHandle::parse("task-other".to_owned()).expect("test handle"));
    assert_eq!(
        repository
            .offer_late_facts(different)
            .await
            .expect("conflict"),
        LateFactsOutcome::Conflicted
    );
    assert_eq!(count_facts(&pool, job_id.0, "task_handle").await, 1);
    let cases: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operations.reconciliation_cases WHERE job_id = $1",
    )
    .bind(job_id.0)
    .fetch_one(&pool)
    .await
    .expect("cases");
    assert_eq!(cases, 1, "a conflicting late fact opens a case");

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn late_accounting_facts_are_received_but_unrelated_ones_are_ignored() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "late-accounting").await;
    let (attempt_id, credential) = begin(&repository, job_id).await;

    let mut late = facts(job_id, attempt_id, &credential);
    late.evidence = Some(MeteringEvidence {
        attempt_id,
        provider_response_digest: "resp-late".to_owned(),
        usage: Some(usage()),
    });
    late.provider_cost = Some(ProviderCostFact {
        source: ProviderCostSource::Declared,
        amount_microusd: Some(200),
        currency: Some("USD".to_owned()),
        cny_microusd: Some(150),
    });
    assert_eq!(
        repository.offer_late_facts(late).await.expect("offer"),
        LateFactsOutcome::Received
    );
    assert_eq!(count_facts(&pool, job_id.0, "accounting").await, 1);

    // 证据 Attempt 对不上：忽略，不写。
    let mut mismatched = facts(job_id, attempt_id, &credential);
    mismatched.evidence = Some(MeteringEvidence {
        attempt_id: AttemptId::new(),
        provider_response_digest: "resp-other".to_owned(),
        usage: Some(usage()),
    });
    assert_eq!(
        repository
            .offer_late_facts(mismatched)
            .await
            .expect("mismatch"),
        LateFactsOutcome::Ignored
    );
    assert_eq!(count_facts(&pool, job_id.0, "accounting").await, 1);

    // 没有可收内容或 Job 关联不上：忽略。
    assert_eq!(
        repository
            .offer_late_facts(facts(job_id, attempt_id, &credential))
            .await
            .expect("empty"),
        LateFactsOutcome::Ignored
    );
    let mut unrelated = facts(JobId::new(), attempt_id, &credential);
    unrelated.provider_task_handle =
        Some(ProviderTaskHandle::parse("task-x".to_owned()).expect("test handle"));
    assert_eq!(
        repository
            .offer_late_facts(unrelated)
            .await
            .expect("unrelated"),
        LateFactsOutcome::Ignored
    );

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_late_task_handle_is_claimed_reclaimable_after_the_ttl_and_marked_consumed() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "late-claim").await;
    let (attempt_id, credential) = begin(&repository, job_id).await;

    let mut late = facts(job_id, attempt_id, &credential);
    late.provider_task_handle =
        Some(ProviderTaskHandle::parse("task-claim".to_owned()).expect("test handle"));
    late.provider_trace_id = Some(ProviderTraceId::parse("trace-claim").expect("test trace"));
    assert_eq!(
        repository.offer_late_facts(late).await.expect("offer"),
        LateFactsOutcome::Received
    );

    let claimed = repository
        .claim_unconsumed_late_facts("worker-1", 10, ChronoDuration::minutes(5))
        .await
        .expect("claim");
    assert_eq!(claimed.len(), 1);
    let fact = &claimed[0];
    assert_eq!(fact.job_id, job_id);
    assert_eq!(fact.attempt_id, attempt_id);
    assert_eq!(fact.kind, LateFactKind::TaskHandle);
    assert_eq!(
        fact.provider_task_handle
            .as_ref()
            .map(ProviderTaskHandle::as_str),
        Some("task-claim")
    );
    assert_eq!(
        fact.provider_trace_id.as_ref().map(ProviderTraceId::as_str),
        Some("trace-claim")
    );
    assert!(fact.evidence.is_none());
    assert!(fact.provider_cost.is_none());

    let claimed_by: Option<String> =
        sqlx::query_scalar("SELECT claimed_by FROM generation.late_facts WHERE id = $1")
            .bind(fact.id)
            .fetch_one(&pool)
            .await
            .expect("claimed_by");
    assert_eq!(claimed_by.as_deref(), Some("worker-1"));

    // 领取未超 TTL：另一个领取者拿不到同一行。
    assert!(
        repository
            .claim_unconsumed_late_facts("worker-2", 10, ChronoDuration::minutes(5))
            .await
            .expect("claim")
            .is_empty(),
        "a fresh claim must not be stolen before its TTL"
    );

    // 领取超时：可被重新领取并覆盖 claimed_by，consumed_at 仍为空（领取不是消费）。
    let reclaimed = repository
        .claim_unconsumed_late_facts("worker-2", 10, ChronoDuration::zero())
        .await
        .expect("reclaim");
    assert_eq!(reclaimed.len(), 1);
    let row =
        sqlx::query("SELECT claimed_by, consumed_at FROM generation.late_facts WHERE id = $1")
            .bind(fact.id)
            .fetch_one(&pool)
            .await
            .expect("the reclaimed row");
    assert_eq!(
        row.try_get::<Option<String>, _>("claimed_by")
            .expect("claimed_by")
            .as_deref(),
        Some("worker-2")
    );
    assert!(
        row.try_get::<Option<chrono::DateTime<Utc>>, _>("consumed_at")
            .expect("consumed_at")
            .is_none(),
        "claiming a fact does not consume it"
    );

    // 消费成功才 mark；重复 mark 返回 false，已消费的不再被领取。
    assert!(
        repository
            .mark_late_fact_consumed(fact.id)
            .await
            .expect("mark")
    );
    assert!(
        !repository
            .mark_late_fact_consumed(fact.id)
            .await
            .expect("repeat mark")
    );
    assert!(
        repository
            .claim_unconsumed_late_facts("worker-3", 10, ChronoDuration::zero())
            .await
            .expect("claim after mark")
            .is_empty()
    );

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn claimed_accounting_late_facts_carry_the_evidence_and_cost() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "late-accounting-claim").await;
    let (attempt_id, credential) = begin(&repository, job_id).await;

    let mut late = facts(job_id, attempt_id, &credential);
    late.evidence = Some(MeteringEvidence {
        attempt_id,
        provider_response_digest: "resp-claim".to_owned(),
        usage: Some(usage()),
    });
    late.provider_cost = Some(ProviderCostFact {
        source: ProviderCostSource::Declared,
        amount_microusd: Some(200),
        currency: Some("USD".to_owned()),
        cny_microusd: Some(150),
    });
    assert_eq!(
        repository.offer_late_facts(late).await.expect("offer"),
        LateFactsOutcome::Received
    );

    let claimed = repository
        .claim_unconsumed_late_facts("worker-1", 10, ChronoDuration::zero())
        .await
        .expect("claim");
    assert_eq!(claimed.len(), 1);
    let fact = &claimed[0];
    assert_eq!(fact.kind, LateFactKind::Accounting);
    assert_eq!(
        fact.evidence
            .as_ref()
            .map(|e| e.provider_response_digest.as_str()),
        Some("resp-claim")
    );
    assert_eq!(
        fact.evidence.as_ref().map(|e| e.attempt_id),
        Some(attempt_id)
    );
    let cost = fact.provider_cost.as_ref().expect("the cost fact");
    assert_eq!(cost.source, ProviderCostSource::Declared);
    assert_eq!(cost.amount_microusd, Some(200));
    assert_eq!(cost.currency.as_deref(), Some("USD"));
    assert_eq!(cost.cny_microusd, Some(150));

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

/// 不是有界标识的"句柄"在类型构造处就被拒绝，进不了收件端口；没有其它可收事实时不写空行。
#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_late_handle_that_is_not_an_identifier_is_not_stored() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "late-bad-handle").await;
    let (attempt_id, credential) = begin(&repository, job_id).await;

    assert!(
        ProviderTaskHandle::parse("data:image/png;base64,AAAA".to_owned()).is_err(),
        "a payload must not construct a task handle"
    );
    assert!(
        ProviderTraceId::parse("data:image/png;base64,AAAA").is_none(),
        "a payload must not construct a trace id"
    );

    // 只有句柄形态、没有其它事实：按无可收内容处理，不写空行。
    let late = facts(job_id, attempt_id, &credential);
    assert_eq!(
        repository.offer_late_facts(late).await.expect("offer"),
        LateFactsOutcome::Ignored
    );
    assert_eq!(count_facts(&pool, job_id.0, "task_handle").await, 0);

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

/// 收件凭据不匹配：不写任何收件行，也不改所有权与状态（RFC 0018 §5.2）。
#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_late_fact_with_a_mismatched_credential_is_ignored() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "late-bad-credential").await;
    let (attempt_id, credential) = begin(&repository, job_id).await;

    // 形状合法但不是这一份：只在库里存摘要，拿不到原值就伪造不出。
    let forged = ReceiptCredential::parse(&"a".repeat(64)).expect("a well-formed credential");
    let mut late = facts(job_id, attempt_id, &forged);
    late.provider_task_handle =
        Some(ProviderTaskHandle::parse("task-forged".to_owned()).expect("test handle"));
    assert_eq!(
        repository.offer_late_facts(late).await.expect("offer"),
        LateFactsOutcome::Ignored
    );
    assert_eq!(count_facts(&pool, job_id.0, "task_handle").await, 0);

    // 原凭据仍然有效：拒绝的是伪造那一份，不是把这条通路关掉。
    let mut valid = facts(job_id, attempt_id, &credential);
    valid.provider_task_handle =
        Some(ProviderTaskHandle::parse("task-valid".to_owned()).expect("test handle"));
    assert_eq!(
        repository.offer_late_facts(valid).await.expect("offer"),
        LateFactsOutcome::Received
    );
    assert_eq!(count_facts(&pool, job_id.0, "task_handle").await, 1);

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

/// 升级前已在飞的 Attempt 没有凭据摘要：不伪造身份，一律不收（RFC 0018 §5.2）。
#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_late_fact_for_an_attempt_without_a_credential_is_ignored() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "late-no-credential").await;
    let (attempt_id, credential) = begin(&repository, job_id).await;
    sqlx::query("UPDATE generation.attempts SET receipt_credential_digest = NULL WHERE id = $1")
        .bind(attempt_id.0)
        .execute(&pool)
        .await
        .expect("clear the credential digest");

    let mut late = facts(job_id, attempt_id, &credential);
    late.provider_task_handle =
        Some(ProviderTaskHandle::parse("task-legacy".to_owned()).expect("test handle"));
    assert_eq!(
        repository.offer_late_facts(late).await.expect("offer"),
        LateFactsOutcome::Ignored
    );
    assert_eq!(count_facts(&pool, job_id.0, "task_handle").await, 0);

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}
