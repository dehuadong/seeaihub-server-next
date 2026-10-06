//! settle / fail_or_reconcile / read_finalization 的账务与容量要对着**真库**验：
//! 行锁内的所有权与 fencing、只落一次账、capture 与每日合计、渠道槽位释放，以及冲突证据建案
//! 与重复失败的幂等，都不是内存替身能模拟的（Spec 0005 §3、§5；RFC 0017 §3、§5、§6）。
//!
//! 用例从 HTTP_CONTRACT_DATABASE_URL 派生一次性库，跑完整迁移后在真库上走受理 → 提交 → 收尾；
//! 覆盖正常结算、重复 settle、冲突证据、确定失败、不确定失败与 read_finalization 两态。
//! 跑完删掉这个库，不动基库。

use chrono::{Duration as ChronoDuration, Utc};
use seeai_application::{
    AdmitExecution, AdmitOffering, AdmitOutcome, ApplicationError, BeginSubmission,
    ExecutionRepository, FailOrReconcileExecution, FailureDisposition, ProviderFailureKind,
    RecordAcceptance, RoutingDecision, SettleExecution,
};
use seeai_domain::{
    AccountId, AttemptId, ChannelId, ExecutionStage, FencingToken, ImageBranch, JobId,
    MeteringEvidence, OfferingId, PriceSnapshot, ProviderCostFact, ProviderCostSource,
    ProviderTaskHandle, ProviderTraceId, RuntimeRevisionId, TokenUsage, VendorModelId,
};
use seeai_persistence::PgHubRepository;
use serde_json::json;
use sqlx::{AssertSqlSafe, PgPool, Row};
use uuid::Uuid;

async fn isolated_database_url() -> (String, String) {
    let base = std::env::var("HTTP_CONTRACT_DATABASE_URL")
        .expect("HTTP_CONTRACT_DATABASE_URL is required for the ignored finalization test");
    let admin = PgPool::connect(&base)
        .await
        .expect("connect to the provided contract database");
    let name = format!("seeai_settle_{}", Uuid::new_v4().simple());
    sqlx::query(AssertSqlSafe(format!("CREATE DATABASE \"{name}\"")))
        .execute(&admin)
        .await
        .expect("create an isolated finalization database");
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

/// 收尾需要的那几行外键目标：账户、渠道、厂商模型、供给与修订。
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
         VALUES ($1, 1000000, 0, 0, 'consumer', 'finalization test account')",
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
         VALUES ($1, '{}'::jsonb, 'finalization-test', 'fake-gateway', $2)",
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

fn admit_command(fixture: &Fixture, key: &str) -> AdmitExecution {
    AdmitExecution {
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
    }
}

async fn admit_one(repository: &PgHubRepository, fixture: &Fixture, key: &str) -> JobId {
    let outcome = repository
        .admit(admit_command(fixture, key))
        .await
        .expect("the admit");
    let AdmitOutcome::Admitted { job, .. } = outcome else {
        panic!("a fresh key must be admitted");
    };
    job.job_id
}

/// 受理后写下提交声明并接受，返回本次 Attempt——与真实执行路径同形。
async fn start_and_accept(
    repository: &PgHubRepository,
    job_id: JobId,
) -> seeai_application::SubmissionStarted {
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
    repository
        .record_acceptance(RecordAcceptance {
            job_id,
            attempt_id: started.attempt_id,
            execution_owner: "supervisor-a".to_owned(),
            fencing_token: FencingToken::new(0),
            provider_task_handle: Some(
                ProviderTaskHandle::parse("task-1".to_owned()).expect("test handle"),
            ),
            provider_trace_id: Some(ProviderTraceId::parse("trace-1").expect("test trace")),
        })
        .await
        .expect("record_acceptance");
    started
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

fn evidence(attempt_id: AttemptId, digest: &str) -> MeteringEvidence {
    MeteringEvidence {
        attempt_id,
        provider_response_digest: digest.to_owned(),
        usage: Some(usage()),
    }
}

fn settle_command(
    job_id: JobId,
    attempt_id: AttemptId,
    digest: &str,
    charge: u64,
) -> SettleExecution {
    SettleExecution {
        job_id,
        attempt_id,
        execution_owner: "supervisor-a".to_owned(),
        fencing_token: FencingToken::new(0),
        evidence: evidence(attempt_id, digest),
        provider_cost: ProviderCostFact {
            source: ProviderCostSource::Declared,
            amount_microusd: Some(2000),
            currency: Some("USD".to_owned()),
            cny_microusd: Some(1500),
        },
        charge_microusd: charge,
        image_count: Some(1),
        provider_trace_id: Some(ProviderTraceId::parse("trace-1").expect("test trace")),
    }
}

fn failure_command(
    job_id: JobId,
    attempt_id: AttemptId,
    disposition: FailureDisposition,
    provider_cost: Option<ProviderCostFact>,
) -> FailOrReconcileExecution {
    FailOrReconcileExecution::for_failure(
        job_id,
        attempt_id,
        "supervisor-a".to_owned(),
        FencingToken::new(0),
        ProviderFailureKind::PlatformInternal,
        disposition,
        provider_cost,
        Some(ProviderTraceId::parse("trace-1").expect("test trace")),
    )
}

async fn scalar(pool: &PgPool, sql: &'static str, id: Uuid) -> i64 {
    sqlx::query_scalar(sql)
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("count")
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn settle_commits_the_ledger_once_and_releases_the_channel_slot() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "settle").await;
    let started = start_and_accept(&repository, job_id).await;

    let finalized = repository
        .settle(settle_command(job_id, started.attempt_id, "resp-1", 800))
        .await
        .expect("the first settle");
    assert_eq!(finalized.stage, ExecutionStage::Succeeded);
    assert_eq!(finalized.charge_microusd, 800);

    // Attempt 落 terminal 与计量证据/成本事实。
    let attempt = sqlx::query(
        "SELECT state, response_digest, metering_evidence, provider_trace_id,
                provider_cost_source, provider_cost_microusd, provider_cost_currency,
                provider_cost_cny_microusd, completed_at
         FROM generation.attempts WHERE id = $1",
    )
    .bind(started.attempt_id.0)
    .fetch_one(&pool)
    .await
    .expect("the settled attempt");
    assert_eq!(
        attempt.try_get::<String, _>("state").expect("state"),
        "terminal"
    );
    assert_eq!(
        attempt
            .try_get::<Option<String>, _>("response_digest")
            .expect("digest")
            .as_deref(),
        Some("resp-1")
    );
    assert!(
        attempt
            .try_get::<Option<serde_json::Value>, _>("metering_evidence")
            .expect("evidence")
            .is_some(),
        "the metering evidence is persisted"
    );
    assert_eq!(
        attempt
            .try_get::<Option<String>, _>("provider_cost_source")
            .expect("source")
            .as_deref(),
        Some("declared")
    );
    assert_eq!(
        attempt
            .try_get::<Option<i64>, _>("provider_cost_cny_microusd")
            .expect("cny"),
        Some(1500)
    );
    assert!(
        attempt
            .try_get::<Option<chrono::DateTime<Utc>>, _>("completed_at")
            .expect("completed")
            .is_some()
    );

    // Job 落 succeeded 并盖 terminal_at，产出张数落在 image_count（用量与账单的分母）。
    let job = sqlx::query(
        "SELECT state, terminal_at, error_code, image_count FROM generation.jobs WHERE id = $1",
    )
    .bind(job_id.0)
    .fetch_one(&pool)
    .await
    .expect("the settled job");
    assert_eq!(
        job.try_get::<String, _>("state").expect("state"),
        "succeeded"
    );
    assert_eq!(
        job.try_get::<Option<i32>, _>("image_count")
            .expect("image count"),
        Some(1),
        "结算写入本次实际产出张数"
    );
    assert!(
        job.try_get::<Option<chrono::DateTime<Utc>>, _>("terminal_at")
            .expect("terminal")
            .is_some(),
        "success stamps terminal_at"
    );
    assert_eq!(
        job.try_get::<Option<String>, _>("error_code")
            .expect("error"),
        None
    );

    // 账户：按实收减少余额、按预授权额去掉占用。
    let account =
        sqlx::query("SELECT balance_microusd, held_microusd FROM ledger.accounts WHERE id = $1")
            .bind(fixture.account_id.0)
            .fetch_one(&pool)
            .await
            .expect("the settled account");
    assert_eq!(
        account
            .try_get::<i64, _>("balance_microusd")
            .expect("balance"),
        999_200
    );
    assert_eq!(account.try_get::<i64, _>("held_microusd").expect("held"), 0);
    assert_eq!(
        scalar(
            &pool,
            "SELECT count(*) FROM ledger.holds WHERE job_id = $1 AND status = 'captured'",
            job_id.0
        )
        .await,
        1,
        "the hold is captured"
    );
    // 真实收支才写 capture，且业务键按执行唯一。
    let capture = sqlx::query(
        "SELECT amount_microusd, business_key FROM ledger.entries
         WHERE job_id = $1 AND kind = 'capture'",
    )
    .bind(job_id.0)
    .fetch_one(&pool)
    .await
    .expect("the capture entry");
    assert_eq!(
        capture
            .try_get::<i64, _>("amount_microusd")
            .expect("amount"),
        -800
    );
    assert_eq!(
        capture.try_get::<String, _>("business_key").expect("key"),
        format!("job:{job_id}:capture")
    );
    let daily: i64 = sqlx::query_scalar(
        "SELECT settled_microusd FROM ledger.daily_spend
         WHERE account_id = $1 AND day = (now() AT TIME ZONE 'UTC')::date",
    )
    .bind(fixture.account_id.0)
    .fetch_one(&pool)
    .await
    .expect("the daily total");
    assert_eq!(daily, 800, "the capture accumulates into the daily total");

    // 渠道容量槽位随终态释放。
    let slot = sqlx::query(
        "SELECT state, released_at FROM generation.execution_capacity WHERE job_id = $1",
    )
    .bind(job_id.0)
    .fetch_one(&pool)
    .await
    .expect("the released slot");
    assert_eq!(
        slot.try_get::<String, _>("state").expect("state"),
        "released"
    );
    assert!(
        slot.try_get::<Option<chrono::DateTime<Utc>>, _>("released_at")
            .expect("released_at")
            .is_some()
    );

    // read_finalization：已提交。
    let read = repository
        .read_finalization(job_id, started.attempt_id)
        .await
        .expect("read_finalization");
    let read = read.expect("a committed finalization");
    assert_eq!(read.stage, ExecutionStage::Succeeded);
    assert_eq!(read.charge_microusd, 800);

    // 同 Attempt 同事实重复调用：返回已提交结果，不重复扣费。
    let repeated = repository
        .settle(settle_command(job_id, started.attempt_id, "resp-1", 800))
        .await
        .expect("the repeated settle");
    assert_eq!(repeated.charge_microusd, 800);
    let balance_after: i64 =
        sqlx::query_scalar("SELECT balance_microusd FROM ledger.accounts WHERE id = $1")
            .bind(fixture.account_id.0)
            .fetch_one(&pool)
            .await
            .expect("balance");
    assert_eq!(balance_after, 999_200, "a repeat must not charge twice");
    assert_eq!(
        scalar(
            &pool,
            "SELECT count(*) FROM ledger.entries WHERE job_id = $1 AND kind = 'capture'",
            job_id.0
        )
        .await,
        1,
        "a repeat writes no second capture"
    );
    assert_eq!(
        scalar(
            &pool,
            "SELECT count(*) FROM operations.reconciliation_cases WHERE job_id = $1",
            job_id.0
        )
        .await,
        0,
        "an identical repeat is not a conflict"
    );

    // 同 Attempt 冲突证据：建对账案例、不覆盖原结果，也不重复扣费。
    let conflict = repository
        .settle(settle_command(job_id, started.attempt_id, "resp-2", 999))
        .await
        .expect("the conflicting settle still reports the committed result");
    assert_eq!(
        conflict.charge_microusd, 800,
        "the original committed charge is returned"
    );
    assert_eq!(
        scalar(
            &pool,
            "SELECT count(*) FROM operations.reconciliation_cases WHERE job_id = $1 AND status = 'open'",
            job_id.0
        )
        .await,
        1,
        "conflicting evidence opens a case"
    );
    let unchanged: i64 =
        sqlx::query_scalar("SELECT balance_microusd FROM ledger.accounts WHERE id = $1")
            .bind(fixture.account_id.0)
            .fetch_one(&pool)
            .await
            .expect("balance");
    assert_eq!(unchanged, 999_200, "the conflict must not charge again");
    let stored_digest: Option<String> =
        sqlx::query_scalar("SELECT response_digest FROM generation.attempts WHERE id = $1")
            .bind(started.attempt_id.0)
            .fetch_one(&pool)
            .await
            .expect("digest");
    assert_eq!(
        stored_digest.as_deref(),
        Some("resp-1"),
        "the conflict must not overwrite the stored evidence"
    );

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_determined_failure_releases_the_hold_and_the_channel_slot() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "determined").await;
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

    let gap = ProviderCostFact {
        source: ProviderCostSource::Unavailable,
        amount_microusd: None,
        currency: None,
        cny_microusd: None,
    };
    let finalized = repository
        .fail_or_reconcile(failure_command(
            job_id,
            started.attempt_id,
            FailureDisposition::DeterminedFailure,
            Some(gap),
        ))
        .await
        .expect("the determined failure");
    assert_eq!(finalized.stage, ExecutionStage::Failed);
    assert_eq!(finalized.charge_microusd, 0);

    let job = sqlx::query(
        "SELECT state, error_code, failure_kind, terminal_at
         FROM generation.jobs WHERE id = $1",
    )
    .bind(job_id.0)
    .fetch_one(&pool)
    .await
    .expect("the failed job");
    assert_eq!(job.try_get::<String, _>("state").expect("state"), "failed");
    assert_eq!(
        job.try_get::<Option<String>, _>("error_code")
            .expect("error")
            .as_deref(),
        Some("platform_unavailable")
    );
    assert_eq!(
        job.try_get::<Option<String>, _>("failure_kind")
            .expect("kind")
            .as_deref(),
        Some("platform_internal")
    );
    assert!(
        job.try_get::<Option<chrono::DateTime<Utc>>, _>("terminal_at")
            .expect("terminal")
            .is_some(),
        "a determined failure is terminal"
    );
    // Attempt 落 terminal 并把成本缺口标出来，且不保存渠道原文。
    let attempt = sqlx::query(
        "SELECT state, provider_cost_source, provider_cost_microusd, provider_error_code,
                provider_error_message, completed_at
         FROM generation.attempts WHERE id = $1",
    )
    .bind(started.attempt_id.0)
    .fetch_one(&pool)
    .await
    .expect("the failed attempt");
    assert_eq!(
        attempt.try_get::<String, _>("state").expect("state"),
        "terminal"
    );
    assert_eq!(
        attempt
            .try_get::<Option<String>, _>("provider_cost_source")
            .expect("source")
            .as_deref(),
        Some("unavailable")
    );
    assert_eq!(
        attempt
            .try_get::<Option<String>, _>("provider_error_code")
            .expect("code"),
        None,
        "the bounded classifier stores no provider raw code"
    );
    assert_eq!(
        attempt
            .try_get::<Option<String>, _>("provider_error_message")
            .expect("message"),
        None
    );
    assert!(
        attempt
            .try_get::<Option<chrono::DateTime<Utc>>, _>("completed_at")
            .expect("completed")
            .is_some()
    );

    // 释放 Hold 与渠道槽位，余额不动、不写对客流水、不建对账案例。
    let account =
        sqlx::query("SELECT balance_microusd, held_microusd FROM ledger.accounts WHERE id = $1")
            .bind(fixture.account_id.0)
            .fetch_one(&pool)
            .await
            .expect("the account");
    assert_eq!(
        account
            .try_get::<i64, _>("balance_microusd")
            .expect("balance"),
        1_000_000
    );
    assert_eq!(account.try_get::<i64, _>("held_microusd").expect("held"), 0);
    assert_eq!(
        scalar(
            &pool,
            "SELECT count(*) FROM ledger.holds WHERE job_id = $1 AND status = 'released'",
            job_id.0
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &pool,
            "SELECT count(*) FROM ledger.entries WHERE job_id = $1 AND kind = 'capture'",
            job_id.0
        )
        .await,
        0
    );
    assert_eq!(
        scalar(
            &pool,
            "SELECT count(*) FROM generation.execution_capacity WHERE job_id = $1 AND state = 'released'",
            job_id.0
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &pool,
            "SELECT count(*) FROM operations.reconciliation_cases WHERE job_id = $1",
            job_id.0
        )
        .await,
        0
    );

    // 重复调用幂等：不再释放、不再改余额。
    let repeated = repository
        .fail_or_reconcile(failure_command(
            job_id,
            started.attempt_id,
            FailureDisposition::DeterminedFailure,
            Some(ProviderCostFact {
                source: ProviderCostSource::Unavailable,
                amount_microusd: None,
                currency: None,
                cny_microusd: None,
            }),
        ))
        .await
        .expect("the repeated failure is idempotent");
    assert_eq!(repeated.stage, ExecutionStage::Failed);
    assert_eq!(
        scalar(
            &pool,
            "SELECT count(*) FROM ledger.holds WHERE job_id = $1",
            job_id.0
        )
        .await,
        1,
        "a repeat does not touch the hold again"
    );

    // 换了处置：冲突，原终态不被改写。
    let conflicting = repository
        .fail_or_reconcile(failure_command(
            job_id,
            started.attempt_id,
            FailureDisposition::Unknown,
            None,
        ))
        .await;
    assert!(
        matches!(conflicting, Err(ApplicationError::Conflict(_))),
        "a different disposition on a finalized job must conflict, got {conflicting:?}"
    );

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn an_uncertain_failure_retains_the_hold_and_the_slot_and_opens_a_case() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "uncertain").await;
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

    let cost = ProviderCostFact {
        source: ProviderCostSource::Declared,
        amount_microusd: Some(400),
        currency: Some("USD".to_owned()),
        cny_microusd: Some(300),
    };
    let finalized = repository
        .fail_or_reconcile(failure_command(
            job_id,
            started.attempt_id,
            FailureDisposition::Unknown,
            Some(cost),
        ))
        .await
        .expect("the uncertain failure");
    assert_eq!(finalized.stage, ExecutionStage::ReconciliationRequired);
    assert_eq!(finalized.charge_microusd, 0);

    let job = sqlx::query(
        "SELECT state, terminal_at, error_code, failure_kind FROM generation.jobs WHERE id = $1",
    )
    .bind(job_id.0)
    .fetch_one(&pool)
    .await
    .expect("the reconciling job");
    assert_eq!(
        job.try_get::<String, _>("state").expect("state"),
        "reconciliation_required"
    );
    assert_eq!(
        job.try_get::<Option<chrono::DateTime<Utc>>, _>("terminal_at")
            .expect("terminal"),
        None,
        "reconciliation is not a terminal state"
    );
    assert_eq!(
        job.try_get::<Option<String>, _>("error_code")
            .expect("error")
            .as_deref(),
        Some("outcome_unknown")
    );
    let attempt = sqlx::query("SELECT state FROM generation.attempts WHERE id = $1")
        .bind(started.attempt_id.0)
        .fetch_one(&pool)
        .await
        .expect("the unknown attempt");
    assert_eq!(
        attempt.try_get::<String, _>("state").expect("state"),
        "unknown"
    );

    // 占用与槽位保留，账户余额与 held 都不动。
    let account =
        sqlx::query("SELECT balance_microusd, held_microusd FROM ledger.accounts WHERE id = $1")
            .bind(fixture.account_id.0)
            .fetch_one(&pool)
            .await
            .expect("the account");
    assert_eq!(
        account
            .try_get::<i64, _>("balance_microusd")
            .expect("balance"),
        1_000_000
    );
    assert_eq!(
        account.try_get::<i64, _>("held_microusd").expect("held"),
        1000
    );
    assert_eq!(
        scalar(
            &pool,
            "SELECT count(*) FROM ledger.holds WHERE job_id = $1 AND status = 'active'",
            job_id.0
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &pool,
            "SELECT count(*) FROM generation.execution_capacity WHERE job_id = $1 AND state = 'held'",
            job_id.0
        )
        .await,
        1,
        "an unknown outcome keeps the channel slot"
    );
    let case =
        sqlx::query("SELECT reason, status FROM operations.reconciliation_cases WHERE job_id = $1")
            .bind(job_id.0)
            .fetch_one(&pool)
            .await
            .expect("the reconciliation case");
    assert_eq!(case.try_get::<String, _>("status").expect("status"), "open");
    let reason: String = case.try_get("reason").expect("reason");
    assert!(
        reason.contains("platform_internal"),
        "the bounded reason names the failure kind, got {reason}"
    );

    // 平台自担的成本进账本：平台账户减、cost 分录按执行唯一。
    let platform: i64 =
        sqlx::query_scalar("SELECT balance_microusd FROM ledger.accounts WHERE kind = 'platform'")
            .fetch_one(&pool)
            .await
            .expect("the platform account");
    assert_eq!(platform, -300, "the platform bears the declared cost");
    assert_eq!(
        scalar(
            &pool,
            "SELECT count(*) FROM ledger.entries WHERE job_id = $1 AND kind = 'cost'",
            job_id.0
        )
        .await,
        1
    );

    // 重复调用幂等：不重复建案、不重复记成本、槽位仍保留。
    let repeated = repository
        .fail_or_reconcile(failure_command(
            job_id,
            started.attempt_id,
            FailureDisposition::Unknown,
            Some(ProviderCostFact {
                source: ProviderCostSource::Declared,
                amount_microusd: Some(400),
                currency: Some("USD".to_owned()),
                cny_microusd: Some(300),
            }),
        ))
        .await
        .expect("the repeated uncertain failure is idempotent");
    assert_eq!(repeated.stage, ExecutionStage::ReconciliationRequired);
    assert_eq!(
        scalar(
            &pool,
            "SELECT count(*) FROM operations.reconciliation_cases WHERE job_id = $1",
            job_id.0
        )
        .await,
        1,
        "a repeat does not open a second case"
    );
    let platform_after: i64 =
        sqlx::query_scalar("SELECT balance_microusd FROM ledger.accounts WHERE kind = 'platform'")
            .fetch_one(&pool)
            .await
            .expect("the platform account");
    assert_eq!(
        platform_after, -300,
        "a repeat does not record the cost twice"
    );
    assert_eq!(
        scalar(
            &pool,
            "SELECT count(*) FROM generation.execution_capacity WHERE job_id = $1 AND state = 'held'",
            job_id.0
        )
        .await,
        1
    );

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn read_finalization_reports_committed_and_uncommitted() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "read").await;
    let started = start_and_accept(&repository, job_id).await;

    // 提交声明已写、尚未收尾：未提交。
    assert!(
        repository
            .read_finalization(job_id, started.attempt_id)
            .await
            .expect("read before settle")
            .is_none(),
        "an executing job has no committed finalization"
    );
    // 不存在的 Attempt：未提交。
    assert!(
        repository
            .read_finalization(job_id, AttemptId::new())
            .await
            .expect("read an unknown attempt")
            .is_none()
    );

    repository
        .settle(settle_command(job_id, started.attempt_id, "resp-read", 250))
        .await
        .expect("settle");

    let committed = repository
        .read_finalization(job_id, started.attempt_id)
        .await
        .expect("read after settle")
        .expect("a committed finalization");
    assert_eq!(committed.stage, ExecutionStage::Succeeded);
    assert_eq!(committed.charge_microusd, 250);

    // 失败件也已提交：状态 failed、实收 0。
    let failed_job = admit_one(&repository, &fixture, "read-failed").await;
    let failed_start = repository
        .begin_submission(BeginSubmission {
            job_id: failed_job,
            execution_owner: "supervisor-a".to_owned(),
            fencing_token: FencingToken::new(0),
            deadline: Utc::now() + ChronoDuration::minutes(5),
            lease: ChronoDuration::minutes(5),
        })
        .await
        .expect("begin_submission");
    repository
        .fail_or_reconcile(failure_command(
            failed_job,
            failed_start.attempt_id,
            FailureDisposition::DeterminedFailure,
            None,
        ))
        .await
        .expect("fail_or_reconcile");
    let committed = repository
        .read_finalization(failed_job, failed_start.attempt_id)
        .await
        .expect("read the failed job")
        .expect("a committed failure");
    assert_eq!(committed.stage, ExecutionStage::Failed);
    assert_eq!(committed.charge_microusd, 0);

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}
/// 对账态在确认“无需收费的失败”后可以收成 failed 并释放占用与槽位（RFC 0017 §5）。
#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_reconciliation_can_be_resolved_into_a_determined_failure() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "resolve-reconciliation").await;
    let started = start_and_accept(&repository, job_id).await;

    // 先转对账：保留占用与槽位。
    repository
        .fail_or_reconcile(failure_command(
            job_id,
            started.attempt_id,
            FailureDisposition::Unknown,
            None,
        ))
        .await
        .expect("retain for reconciliation");

    // 确认无需收费的失败：收成 failed，释放占用与槽位。
    let resolved = repository
        .fail_or_reconcile(failure_command(
            job_id,
            started.attempt_id,
            FailureDisposition::DeterminedFailure,
            None,
        ))
        .await
        .expect("resolve the reconciliation into a failure");
    assert_eq!(resolved.stage, ExecutionStage::Failed);

    let job = sqlx::query("SELECT state, terminal_at FROM generation.jobs WHERE id = $1")
        .bind(job_id.0)
        .fetch_one(&pool)
        .await
        .expect("the resolved job");
    assert_eq!(job.try_get::<String, _>("state").expect("state"), "failed");
    assert!(
        job.try_get::<Option<chrono::DateTime<Utc>>, _>("terminal_at")
            .expect("terminal_at")
            .is_some(),
        "a resolved failure is terminal"
    );
    let attempt_state: String =
        sqlx::query_scalar("SELECT state FROM generation.attempts WHERE id = $1")
            .bind(started.attempt_id.0)
            .fetch_one(&pool)
            .await
            .expect("attempt state");
    assert_eq!(attempt_state, "terminal");
    let hold_status: String =
        sqlx::query_scalar("SELECT status FROM ledger.holds WHERE job_id = $1")
            .bind(job_id.0)
            .fetch_one(&pool)
            .await
            .expect("hold status");
    assert_eq!(hold_status, "released");
    let held_slots = scalar(
        &pool,
        "SELECT count(*) FROM generation.execution_capacity WHERE job_id = $1 AND state = 'held'",
        job_id.0,
    )
    .await;
    assert_eq!(held_slots, 0, "the resolved failure releases the slot");

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

/// 可证明未受理的中间失败：记录本次 Attempt、保留占用与槽位、Job 保持 executing，
/// 随后可以再提交一次同一候选（Spec 0005 §5 的安全重试；RFC 0017 §3、§5）。
#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_safe_retry_failure_keeps_the_job_executing_and_allows_another_submission() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "safe-retry").await;
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

    let retryable = repository
        .fail_or_reconcile(failure_command(
            job_id,
            started.attempt_id,
            FailureDisposition::SafeRetry,
            None,
        ))
        .await
        .expect("a retryable failure is recorded");
    assert_eq!(retryable.stage, ExecutionStage::Executing);

    let job =
        sqlx::query("SELECT state, error_code, terminal_at FROM generation.jobs WHERE id = $1")
            .bind(job_id.0)
            .fetch_one(&pool)
            .await
            .expect("the job");
    assert_eq!(
        job.try_get::<String, _>("state").expect("state"),
        "executing"
    );
    assert!(
        job.try_get::<Option<String>, _>("error_code")
            .expect("code")
            .is_none(),
        "a retryable failure must not mark the job failed"
    );
    assert!(
        job.try_get::<Option<chrono::DateTime<Utc>>, _>("terminal_at")
            .expect("terminal")
            .is_none()
    );
    assert_eq!(
        scalar(
            &pool,
            "SELECT count(*) FROM ledger.holds WHERE job_id = $1 AND status = 'active'",
            job_id.0
        )
        .await,
        1,
        "the hold is retained for the retry"
    );
    assert_eq!(
        scalar(
            &pool,
            "SELECT count(*) FROM generation.execution_capacity WHERE job_id = $1 AND state = 'held'",
            job_id.0
        )
        .await,
        1,
        "the channel slot is retained for the retry"
    );
    assert_eq!(
        scalar(
            &pool,
            "SELECT count(*) FROM operations.reconciliation_cases WHERE job_id = $1",
            job_id.0
        )
        .await,
        0,
        "a retryable failure is not a reconciliation case"
    );
    let attempt_state: String =
        sqlx::query_scalar("SELECT state FROM generation.attempts WHERE id = $1")
            .bind(started.attempt_id.0)
            .fetch_one(&pool)
            .await
            .expect("attempt state");
    assert_eq!(attempt_state, "terminal", "the failed attempt is closed");

    let retried = repository
        .begin_submission(BeginSubmission {
            job_id,
            execution_owner: "supervisor-a".to_owned(),
            fencing_token: FencingToken::new(0),
            deadline: Utc::now() + ChronoDuration::minutes(5),
            lease: ChronoDuration::minutes(5),
        })
        .await
        .expect("the retry submission");
    assert_eq!(retried.attempt_no, 2, "the retry opens the next attempt");

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}
