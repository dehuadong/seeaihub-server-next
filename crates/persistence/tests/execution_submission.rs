//! begin_submission / record_acceptance 的提交声明、执行所有权与重复事实要对着**真库**验：
//! 未终结、所有权、fencing、总期限和当前 Attempt 都是行锁内的库层判据，重复事实幂等也是；
//! 这些无法用内存替身模拟（Spec 0005 §3、§5；RFC 0017 §3）。
//!
//! 用例从 HTTP_CONTRACT_DATABASE_URL 派生一次性库，跑完整迁移后受理一台 Job，再走提交声明与
//! 接受入库；分别覆盖正常落列、所有权/fencing/期限/对账态被拒、以及同一 Attempt 重复接受的
//! 幂等与冲突。跑完删掉这个库，不动基库。

use chrono::{Duration as ChronoDuration, Utc};
use seeai_application::{
    AdmitExecution, AdmitOffering, AdmitOutcome, ApplicationError, BeginSubmission,
    CancelUnsubmitted, ExecutionRepository, RecordAcceptance, RoutingDecision,
};
use seeai_domain::{
    AccountId, AttemptStage, ChannelId, ExecutionStage, FencingToken, ImageBranch, JobId,
    OfferingId, PriceSnapshot, ProviderTaskHandle, ProviderTraceId, RuntimeRevisionId,
    VendorModelId,
};
use seeai_persistence::PgHubRepository;
use serde_json::json;
use sqlx::{AssertSqlSafe, PgPool, Row};
use std::collections::BTreeSet;
use uuid::Uuid;

async fn isolated_database_url() -> (String, String) {
    let base = std::env::var("HTTP_CONTRACT_DATABASE_URL")
        .expect("HTTP_CONTRACT_DATABASE_URL is required for the ignored submission test");
    let admin = PgPool::connect(&base)
        .await
        .expect("connect to the provided contract database");
    let name = format!("seeai_submit_{}", Uuid::new_v4().simple());
    sqlx::query(AssertSqlSafe(format!("CREATE DATABASE \"{name}\"")))
        .execute(&admin)
        .await
        .expect("create an isolated submission database");
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

/// 受理需要的那几行外键目标：账户、渠道、厂商模型、供给与修订。
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
    // 余额刻意放大：一个用例里会有多台 Job 各占一份预授权，别让资金闸门挡住后面的受理。
    sqlx::query(
        "INSERT INTO ledger.accounts (id, balance_microusd, held_microusd, version, kind, name)
         VALUES ($1, 1000000, 0, 0, 'consumer', 'submission test account')",
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
         VALUES ($1, '{}'::jsonb, 'submission-test', 'fake-gateway', $2)",
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
        max_in_flight: 8,
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
    assert_eq!(job.stage, ExecutionStage::Admitted);
    job.job_id
}

fn begin(
    job_id: JobId,
    owner: &str,
    token: u64,
    deadline: chrono::DateTime<Utc>,
) -> BeginSubmission {
    BeginSubmission {
        job_id,
        execution_owner: owner.to_owned(),
        fencing_token: FencingToken::new(token),
        deadline,
        lease: ChronoDuration::minutes(5),
    }
}

fn cancel(job_id: JobId, owner: &str, token: u64) -> CancelUnsubmitted {
    CancelUnsubmitted {
        job_id,
        execution_owner: owner.to_owned(),
        fencing_token: FencingToken::new(token),
    }
}

async fn count(pool: &PgPool, sql: &'static str, id: Uuid) -> i64 {
    sqlx::query_scalar(sql)
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("count")
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn submission_declaration_then_acceptance_persist_the_minimal_facts() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "submit").await;

    let started = repository
        .begin_submission(begin(
            job_id,
            "supervisor-a",
            0,
            Utc::now() + ChronoDuration::minutes(5),
        ))
        .await
        .expect("begin_submission");
    assert_eq!(started.attempt_no, 1, "the first submission is attempt 1");

    let job = sqlx::query(
        "SELECT state, execution_owner, provider_task_handle, request_digest
         FROM generation.jobs WHERE id = $1",
    )
    .bind(job_id.0)
    .fetch_one(&pool)
    .await
    .expect("the job row");
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
    assert_eq!(
        job.try_get::<Option<String>, _>("provider_task_handle")
            .expect("handle"),
        None,
        "the task handle is not known before the provider accepts"
    );
    let job_digest: String = job.try_get("request_digest").expect("job digest");

    let attempt = sqlx::query(
        "SELECT state, attempt_no, provider_trace_id, request_digest
         FROM generation.attempts WHERE id = $1",
    )
    .bind(started.attempt_id.0)
    .fetch_one(&pool)
    .await
    .expect("the attempt row");
    assert_eq!(
        attempt
            .try_get::<String, _>("state")
            .expect("attempt state"),
        "submitting"
    );
    assert_eq!(attempt.try_get::<i32, _>("attempt_no").expect("no"), 1);
    assert_eq!(
        attempt
            .try_get::<Option<String>, _>("provider_trace_id")
            .expect("trace"),
        None
    );
    assert_eq!(
        attempt
            .try_get::<String, _>("request_digest")
            .expect("digest"),
        job_digest,
        "the v1 attempt reuses the job request fingerprint"
    );

    repository
        .record_acceptance(RecordAcceptance {
            job_id,
            attempt_id: started.attempt_id,
            execution_owner: "supervisor-a".to_owned(),
            fencing_token: FencingToken::new(0),
            provider_task_handle: Some(
                ProviderTaskHandle::parse("task-42".to_owned()).expect("test handle"),
            ),
            provider_trace_id: Some(ProviderTraceId::parse("trace-42").expect("test trace")),
        })
        .await
        .expect("record_acceptance");

    let job = sqlx::query("SELECT state, provider_task_handle FROM generation.jobs WHERE id = $1")
        .bind(job_id.0)
        .fetch_one(&pool)
        .await
        .expect("the job after acceptance");
    assert_eq!(
        job.try_get::<String, _>("state").expect("state"),
        "executing",
        "acceptance keeps the job executing"
    );
    assert_eq!(
        job.try_get::<Option<String>, _>("provider_task_handle")
            .expect("handle")
            .as_deref(),
        Some("task-42")
    );
    let attempt =
        sqlx::query("SELECT state, provider_trace_id FROM generation.attempts WHERE id = $1")
            .bind(started.attempt_id.0)
            .fetch_one(&pool)
            .await
            .expect("the attempt after acceptance");
    assert_eq!(
        attempt.try_get::<String, _>("state").expect("state"),
        "accepted"
    );
    assert_eq!(
        attempt
            .try_get::<Option<String>, _>("provider_trace_id")
            .expect("trace")
            .as_deref(),
        Some("trace-42")
    );

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn begin_submission_fences_ownership_tokens_deadlines_and_open_attempts() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;

    // 所有权已属别的调用方：认领失败，不写任何行。
    let owned = admit_one(&repository, &fixture, "owned").await;
    sqlx::query("UPDATE generation.jobs SET execution_owner = 'supervisor-b' WHERE id = $1")
        .bind(owned.0)
        .execute(&pool)
        .await
        .expect("plant another owner");
    let rejected = repository
        .begin_submission(begin(
            owned,
            "supervisor-a",
            0,
            Utc::now() + ChronoDuration::minutes(5),
        ))
        .await;
    assert!(
        matches!(rejected, Err(ApplicationError::Conflict(_))),
        "a foreign owner must conflict, got {rejected:?}"
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM generation.attempts WHERE job_id = $1",
            owned.0
        )
        .await,
        0,
        "a rejected submission writes no attempt"
    );

    // fencing token 不匹配：同样拒绝。
    sqlx::query(
        "UPDATE generation.jobs SET execution_owner = NULL, fencing_token = 9 WHERE id = $1",
    )
    .bind(owned.0)
    .execute(&pool)
    .await
    .expect("bump the token");
    let rejected = repository
        .begin_submission(begin(
            owned,
            "supervisor-a",
            0,
            Utc::now() + ChronoDuration::minutes(5),
        ))
        .await;
    assert!(
        matches!(rejected, Err(ApplicationError::Conflict(_))),
        "a stale fencing token must conflict, got {rejected:?}"
    );

    // 总期限已到：明确报告，且不写任何行、不改状态。
    sqlx::query("UPDATE generation.jobs SET fencing_token = 0 WHERE id = $1")
        .bind(owned.0)
        .execute(&pool)
        .await
        .expect("restore the token");
    let expired = repository
        .begin_submission(begin(
            owned,
            "supervisor-a",
            0,
            Utc::now() - ChronoDuration::seconds(1),
        ))
        .await;
    assert!(
        matches!(expired, Err(ApplicationError::ExecutionDeadlineExceeded)),
        "an expired deadline must be reported, got {expired:?}"
    );
    let state: String = sqlx::query_scalar("SELECT state FROM generation.jobs WHERE id = $1")
        .bind(owned.0)
        .fetch_one(&pool)
        .await
        .expect("state");
    assert_eq!(
        state, "admitted",
        "an expired deadline leaves the job admitted"
    );

    // 正常开始之后，同一台 Job 不能再来一次：上一个 Attempt 还没收尾。
    repository
        .begin_submission(begin(
            owned,
            "supervisor-a",
            0,
            Utc::now() + ChronoDuration::minutes(5),
        ))
        .await
        .expect("a fresh submission");
    let open = repository
        .begin_submission(begin(
            owned,
            "supervisor-a",
            0,
            Utc::now() + ChronoDuration::minutes(5),
        ))
        .await;
    assert!(
        matches!(open, Err(ApplicationError::Conflict(_))),
        "an unfinished attempt must block a second submission, got {open:?}"
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM generation.attempts WHERE job_id = $1",
            owned.0
        )
        .await,
        1,
        "only one attempt exists"
    );

    // 对账态禁止开始提交（Spec 0005 §5）。
    let reconciling = admit_one(&repository, &fixture, "reconciling").await;
    sqlx::query("UPDATE generation.jobs SET state = 'reconciliation_required' WHERE id = $1")
        .bind(reconciling.0)
        .execute(&pool)
        .await
        .expect("move to reconciliation");
    let rejected = repository
        .begin_submission(begin(
            reconciling,
            "supervisor-a",
            0,
            Utc::now() + ChronoDuration::minutes(5),
        ))
        .await;
    assert!(
        matches!(rejected, Err(ApplicationError::Conflict(_))),
        "a reconciliation job must not submit, got {rejected:?}"
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM generation.attempts WHERE job_id = $1",
            reconciling.0
        )
        .await,
        0
    );

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn acceptance_is_idempotent_for_same_facts_and_conflicts_on_different_ones() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "idempotent").await;
    let started = repository
        .begin_submission(begin(
            job_id,
            "supervisor-a",
            0,
            Utc::now() + ChronoDuration::minutes(5),
        ))
        .await
        .expect("begin_submission");

    let acceptance = |trace: &str| RecordAcceptance {
        job_id,
        attempt_id: started.attempt_id,
        execution_owner: "supervisor-a".to_owned(),
        fencing_token: FencingToken::new(0),
        provider_task_handle: Some(
            ProviderTaskHandle::parse("task-1".to_owned()).expect("test handle"),
        ),
        provider_trace_id: Some(ProviderTraceId::parse(trace).expect("test trace")),
    };

    repository
        .record_acceptance(acceptance("trace-1"))
        .await
        .expect("the first acceptance");
    repository
        .record_acceptance(acceptance("trace-1"))
        .await
        .expect("repeating the same facts is idempotent");
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM generation.attempts WHERE job_id = $1",
            job_id.0
        )
        .await,
        1,
        "an idempotent acceptance does not add a row"
    );

    // 同一 Attempt 换了事实：冲突，原事实不被覆盖。
    let conflict = repository.record_acceptance(acceptance("trace-2")).await;
    assert!(
        matches!(conflict, Err(ApplicationError::Conflict(_))),
        "different facts on the same attempt must conflict, got {conflict:?}"
    );
    let stored: Option<String> =
        sqlx::query_scalar("SELECT provider_trace_id FROM generation.attempts WHERE id = $1")
            .bind(started.attempt_id.0)
            .fetch_one(&pool)
            .await
            .expect("the stored trace");
    assert_eq!(
        stored.as_deref(),
        Some("trace-1"),
        "the conflict must not overwrite the stored fact"
    );

    // 旧 token 不能改写已经入库的接受事实。
    let stale = repository
        .record_acceptance(RecordAcceptance {
            fencing_token: FencingToken::new(5),
            ..acceptance("trace-1")
        })
        .await;
    assert!(
        matches!(stale, Err(ApplicationError::Conflict(_))),
        "a stale token must not record acceptance, got {stale:?}"
    );

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn ownership_renewal_extends_only_the_lease_and_conflicts_when_fenced() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "renew").await;
    repository
        .begin_submission(begin(
            job_id,
            "supervisor-a",
            0,
            Utc::now() + ChronoDuration::minutes(5),
        ))
        .await
        .expect("begin_submission");

    let before: chrono::DateTime<Utc> =
        sqlx::query_scalar("SELECT lease_expires_at FROM generation.jobs WHERE id = $1")
            .bind(job_id.0)
            .fetch_one(&pool)
            .await
            .expect("the initial lease");

    repository
        .renew_execution_ownership(
            job_id,
            "supervisor-a",
            FencingToken::new(0),
            ChronoDuration::minutes(30),
        )
        .await
        .expect("renew");

    let job = sqlx::query(
        "SELECT execution_owner, fencing_token, lease_expires_at FROM generation.jobs WHERE id = $1",
    )
    .bind(job_id.0)
    .fetch_one(&pool)
    .await
    .expect("the renewed job");
    assert_eq!(
        job.try_get::<Option<String>, _>("execution_owner")
            .expect("owner")
            .as_deref(),
        Some("supervisor-a"),
        "renewal keeps the owner"
    );
    assert_eq!(
        job.try_get::<i64, _>("fencing_token").expect("token"),
        0,
        "renewal never changes the fencing token"
    );
    let after: chrono::DateTime<Utc> = job.try_get("lease_expires_at").expect("lease");
    assert!(
        after >= before + ChronoDuration::minutes(20),
        "renewal must push the lease forward, before {before}, after {after}"
    );

    // 别的所有者不能续约。
    assert!(matches!(
        repository
            .renew_execution_ownership(
                job_id,
                "supervisor-b",
                FencingToken::new(0),
                ChronoDuration::minutes(5)
            )
            .await,
        Err(ApplicationError::Conflict(_))
    ));
    // 旧 token 不能续约。
    assert!(matches!(
        repository
            .renew_execution_ownership(
                job_id,
                "supervisor-a",
                FencingToken::new(1),
                ChronoDuration::minutes(5)
            )
            .await,
        Err(ApplicationError::Conflict(_))
    ));
    // 已终态不能续约。
    sqlx::query("UPDATE generation.jobs SET state = 'succeeded' WHERE id = $1")
        .bind(job_id.0)
        .execute(&pool)
        .await
        .expect("finish the job");
    assert!(matches!(
        repository
            .renew_execution_ownership(
                job_id,
                "supervisor-a",
                FencingToken::new(0),
                ChronoDuration::minutes(5)
            )
            .await,
        Err(ApplicationError::Conflict(_))
    ));
    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn takeover_swaps_ownership_and_increments_the_fencing_token() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "takeover").await;
    let started = repository
        .begin_submission(begin(
            job_id,
            "supervisor-a",
            0,
            Utc::now() + ChronoDuration::minutes(5),
        ))
        .await
        .expect("begin_submission");
    repository
        .record_acceptance(RecordAcceptance {
            job_id,
            attempt_id: started.attempt_id,
            execution_owner: "supervisor-a".to_owned(),
            fencing_token: FencingToken::new(0),
            provider_task_handle: Some(
                ProviderTaskHandle::parse("task-7".to_owned()).expect("test handle"),
            ),
            provider_trace_id: Some(ProviderTraceId::parse("trace-7").expect("test trace")),
        })
        .await
        .expect("record_acceptance");

    // 租约还没过期：不能被接管。
    assert!(
        repository
            .takeover_expired_executions("worker-1", ChronoDuration::minutes(5), 10, 5)
            .await
            .expect("takeover")
            .is_empty(),
        "a live lease must not be taken over"
    );

    sqlx::query(
        "UPDATE generation.jobs SET lease_expires_at = now() - interval '1 second' WHERE id = $1",
    )
    .bind(job_id.0)
    .execute(&pool)
    .await
    .expect("expire the lease");

    let taken = repository
        .takeover_expired_executions("worker-1", ChronoDuration::minutes(5), 10, 5)
        .await
        .expect("takeover");
    assert_eq!(taken.len(), 1, "exactly the expired job is taken over");
    let taken = &taken[0];
    assert_eq!(taken.job_id, job_id);
    assert_eq!(taken.account_id, fixture.account_id);
    assert_eq!(
        taken.fencing_token,
        FencingToken::new(1),
        "only takeover increments the token"
    );
    assert_eq!(taken.attempt_id, Some(started.attempt_id));
    assert_eq!(taken.attempt_state, Some(AttemptStage::Accepted));
    assert_eq!(
        taken
            .provider_task_handle
            .as_ref()
            .map(ProviderTaskHandle::as_str),
        Some("task-7")
    );
    assert_eq!(
        taken
            .provider_trace_id
            .as_ref()
            .map(ProviderTraceId::as_str),
        Some("trace-7")
    );
    assert_eq!(taken.adapter_key, "fake");
    assert_eq!(taken.base_url, "http://127.0.0.1:9");
    assert_eq!(taken.credential_env, "FAKE_PROVIDER_KEY");
    assert_eq!(taken.price_snapshot.hold_microusd, Some(1000));
    assert_eq!(taken.stage, ExecutionStage::Executing);
    assert_eq!(taken.provider_kind, "fake");

    let job =
        sqlx::query("SELECT execution_owner, fencing_token FROM generation.jobs WHERE id = $1")
            .bind(job_id.0)
            .fetch_one(&pool)
            .await
            .expect("the taken job");
    assert_eq!(
        job.try_get::<Option<String>, _>("execution_owner")
            .expect("owner")
            .as_deref(),
        Some("worker-1")
    );
    assert_eq!(job.try_get::<i64, _>("fencing_token").expect("token"), 1);

    // 旧 token 不再被接受；新 token 可以续约。
    assert!(matches!(
        repository
            .renew_execution_ownership(
                job_id,
                "worker-1",
                FencingToken::new(0),
                ChronoDuration::minutes(5)
            )
            .await,
        Err(ApplicationError::Conflict(_))
    ));
    repository
        .renew_execution_ownership(
            job_id,
            "worker-1",
            FencingToken::new(1),
            ChronoDuration::minutes(5),
        )
        .await
        .expect("renew with the taken token");

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

/// 多副本并发接管：不重复、不丢（Issue #62/#64，RFC 0017 §5）。
///
/// 前置：与文件内其它真库用例相同——HTTP_CONTRACT_DATABASE_URL 指向一个允许 CREATE DATABASE
/// 的 PostgreSQL，角色能跑迁移；用例自建一次性库并在结束删除。
///
/// 两个 repository 各自建池、各用一条连接，指向同一座一次性库，才谈得上并发接管。两次调用都给
/// limit = 4：任何一方都拿不满 8 台，只有两方各领一半，并集才凑得齐 8 台——断言因此必须覆盖
/// "两批不相交 + 并集等于 8"，而不是让一方一次领完。两侧用不同 worker_id：owner 落到库里之后
/// 能指认是哪一方写的，"owner 只被接管它的一方写"就直接从库里核对。
#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn concurrent_takeovers_partition_expired_executions_without_overlap_or_loss() {
    const JOBS: usize = 8;
    const PER_CALLER: u32 = 4;
    const ORIGINAL_TOKEN: i64 = 0;

    // 两个 repository 指向同一座一次性库，但各自建池、各用一条连接，才谈得上并发接管。
    let (database_url, database_name) = isolated_database_url().await;
    let repository_a = PgHubRepository::connect(&database_url, 1)
        .await
        .expect("the first independent connection");
    repository_a.migrate().await.expect("the migrations apply");
    let repository_b = PgHubRepository::connect(&database_url, 1)
        .await
        .expect("the second independent connection");
    let pool = repository_a.pool().clone();
    let fixture = seed_fixture(&pool).await;

    // 造 8 台 v1、executing、各带一个 open attempt 的执行，随后让 8 条租约一起过期。
    let mut expected = BTreeSet::new();
    for index in 0..JOBS {
        let job_id = admit_one(&repository_a, &fixture, &format!("takeover-race-{index}")).await;
        let started = repository_a
            .begin_submission(begin(
                job_id,
                "supervisor-a",
                ORIGINAL_TOKEN as u64,
                Utc::now() + ChronoDuration::minutes(5),
            ))
            .await
            .expect("begin_submission");
        repository_a
            .record_acceptance(RecordAcceptance {
                job_id,
                attempt_id: started.attempt_id,
                execution_owner: "supervisor-a".to_owned(),
                fencing_token: FencingToken::new(ORIGINAL_TOKEN as u64),
                provider_task_handle: Some(
                    ProviderTaskHandle::parse(format!("task-{index}"))
                        .expect("a bounded task handle"),
                ),
                provider_trace_id: Some(
                    ProviderTraceId::parse(&format!("trace-{index}")).expect("a bounded trace id"),
                ),
            })
            .await
            .expect("record_acceptance");
        expected.insert(job_id.0);
    }
    sqlx::query(
        "UPDATE generation.jobs
         SET lease_expires_at = now() - interval '1 second'
         WHERE state = 'executing'",
    )
    .execute(&pool)
    .await
    .expect("expire every lease at once");

    let (left, right) = tokio::join!(
        repository_a.takeover_expired_executions(
            "worker-a",
            ChronoDuration::minutes(5),
            PER_CALLER,
            5
        ),
        repository_b.takeover_expired_executions(
            "worker-b",
            ChronoDuration::minutes(5),
            PER_CALLER,
            5
        ),
    );
    let left = left.expect("the first concurrent takeover");
    let right = right.expect("the second concurrent takeover");

    let left_ids: BTreeSet<Uuid> = left.iter().map(|taken| taken.job_id.0).collect();
    let right_ids: BTreeSet<Uuid> = right.iter().map(|taken| taken.job_id.0).collect();

    // 不重复：两批没有交集；每台只在其中一批出现。
    assert!(
        left_ids.is_disjoint(&right_ids),
        "two concurrent takeovers must not both own the same job: {left_ids:?} vs {right_ids:?}"
    );
    // 不丢：并集恰好是全部 8 台，且两次调用各拿满 limit。
    let union: BTreeSet<Uuid> = left_ids.union(&right_ids).copied().collect();
    assert_eq!(
        union, expected,
        "every expired execution is taken over exactly once"
    );
    assert_eq!(
        left_ids.len(),
        PER_CALLER as usize,
        "the first caller fills its share"
    );
    assert_eq!(
        right_ids.len(),
        PER_CALLER as usize,
        "the second caller fills its share"
    );

    // 每台 fencing_token 恰好 +1，且只出现在一侧。
    let mut seen = BTreeSet::new();
    for taken in left.iter().chain(right.iter()) {
        assert_eq!(
            taken.fencing_token,
            FencingToken::new((ORIGINAL_TOKEN + 1) as u64),
            "each takeover moves the token by exactly one"
        );
        assert!(
            seen.insert(taken.job_id.0),
            "a job may appear in only one batch"
        );
    }

    // 库里的落定事实：owner 是领走它的一方，token 恰好 +1。
    let rows = sqlx::query(
        "SELECT id, execution_owner, fencing_token
         FROM generation.jobs",
    )
    .fetch_all(&pool)
    .await
    .expect("the jobs after the concurrent takeovers");
    assert_eq!(rows.len(), JOBS);
    for row in &rows {
        let id: Uuid = row.try_get("id").expect("id");
        let owner: Option<String> = row.try_get("execution_owner").expect("owner");
        let token: i64 = row.try_get("fencing_token").expect("token");
        let expected_owner = if left_ids.contains(&id) {
            "worker-a"
        } else {
            "worker-b"
        };
        assert_eq!(
            owner.as_deref(),
            Some(expected_owner),
            "the owner is written only by the side that took the job"
        );
        assert_eq!(
            token,
            ORIGINAL_TOKEN + 1,
            "each row moved exactly one fencing step"
        );
    }

    drop(pool);
    drop(repository_a);
    drop(repository_b);
    drop_isolated_database(&database_name).await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn reaping_an_unsubmitted_admission_releases_hold_and_channel_slot() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "orphan").await;

    // 还没超龄：不回收。
    assert_eq!(
        repository
            .reap_unsubmitted_admissions(ChronoDuration::days(1), 10)
            .await
            .expect("reap"),
        0
    );

    sqlx::query("UPDATE generation.jobs SET created_at = now() - interval '1 hour' WHERE id = $1")
        .bind(job_id.0)
        .execute(&pool)
        .await
        .expect("age the admission");
    assert_eq!(
        repository
            .reap_unsubmitted_admissions(ChronoDuration::minutes(30), 10)
            .await
            .expect("reap"),
        1
    );

    let job = sqlx::query("SELECT state, terminal_at FROM generation.jobs WHERE id = $1")
        .bind(job_id.0)
        .fetch_one(&pool)
        .await
        .expect("the reaped job");
    assert_eq!(job.try_get::<String, _>("state").expect("state"), "failed");
    assert!(
        job.try_get::<Option<chrono::DateTime<Utc>>, _>("terminal_at")
            .expect("terminal_at")
            .is_some(),
        "reaping stamps the terminal moment"
    );
    let hold: String = sqlx::query_scalar("SELECT status FROM ledger.holds WHERE job_id = $1")
        .bind(job_id.0)
        .fetch_one(&pool)
        .await
        .expect("hold");
    assert_eq!(hold, "released");
    let capacity: String =
        sqlx::query_scalar("SELECT state FROM generation.execution_capacity WHERE job_id = $1")
            .bind(job_id.0)
            .fetch_one(&pool)
            .await
            .expect("capacity");
    assert_eq!(capacity, "released");
    let held: i64 = sqlx::query_scalar("SELECT held_microusd FROM ledger.accounts WHERE id = $1")
        .bind(fixture.account_id.0)
        .fetch_one(&pool)
        .await
        .expect("held");
    assert_eq!(held, 0, "the reservation is given back");

    // 已经有 Attempt 的执行不是孤儿：即使超龄也不回收。
    let submitted = admit_one(&repository, &fixture, "submitted-orphan").await;
    repository
        .begin_submission(begin(
            submitted,
            "supervisor-a",
            0,
            Utc::now() + ChronoDuration::minutes(5),
        ))
        .await
        .expect("begin_submission");
    sqlx::query("UPDATE generation.jobs SET created_at = now() - interval '1 hour' WHERE id = $1")
        .bind(submitted.0)
        .execute(&pool)
        .await
        .expect("age the submitted job");
    assert_eq!(
        repository
            .reap_unsubmitted_admissions(ChronoDuration::minutes(30), 10)
            .await
            .expect("reap"),
        0,
        "a job with a submission declaration is not an orphan"
    );
    let state: String = sqlx::query_scalar("SELECT state FROM generation.jobs WHERE id = $1")
        .bind(submitted.0)
        .fetch_one(&pool)
        .await
        .expect("state");
    assert_eq!(state, "executing");

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

/// 带 fencing 的"确定未提交"取消：原子释放 Hold、账户占用与渠道容量，且**不建 Attempt**。
///
/// 硬边界是**已经交给渠道**的 Attempt（`accepted`/`unknown`，提交可能已经在飞），不是还没发出的
/// 提交声明：声明落下但生成发送未开始时按确定未提交释放，并把它收成 `terminal`。这些判据都在
/// 行锁内，只有真库验得到（RFC 0018 §4.1）。
#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn cancelling_an_unsubmitted_execution_releases_without_creating_an_attempt() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "cancel").await;

    // 错误 token 一律冲突：释放的资格来自库里的所有权与 fencing token，不来自调用方自称。
    let wrong_token = repository
        .cancel_unsubmitted(cancel(job_id, "supervisor-a", 1))
        .await
        .expect_err("a stale fencing token must not release");
    assert!(matches!(wrong_token, ApplicationError::Conflict(_)));

    let finalization = repository
        .cancel_unsubmitted(cancel(job_id, "supervisor-a", 0))
        .await
        .expect("the unsubmitted cancellation");
    assert_eq!(finalization.stage, ExecutionStage::Failed);
    assert_eq!(finalization.charge_microusd, 0);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM generation.attempts WHERE job_id = $1",
            job_id.0
        )
        .await,
        0,
        "releasing as unsubmitted must not fabricate a submitting Attempt"
    );
    let job = sqlx::query("SELECT state, terminal_at FROM generation.jobs WHERE id = $1")
        .bind(job_id.0)
        .fetch_one(&pool)
        .await
        .expect("the cancelled job");
    assert_eq!(job.try_get::<String, _>("state").expect("state"), "failed");
    assert!(
        job.try_get::<Option<chrono::DateTime<Utc>>, _>("terminal_at")
            .expect("terminal_at")
            .is_some(),
        "the cancellation stamps the terminal moment"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM ledger.holds WHERE job_id = $1")
            .bind(job_id.0)
            .fetch_one(&pool)
            .await
            .expect("the hold"),
        "released"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT state FROM generation.execution_capacity WHERE job_id = $1"
        )
        .bind(job_id.0)
        .fetch_one(&pool)
        .await
        .expect("the channel slot"),
        "released"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT held_microusd FROM ledger.accounts WHERE id = $1")
            .bind(fixture.account_id.0)
            .fetch_one(&pool)
            .await
            .expect("the reservation"),
        0,
        "the reservation is given back exactly once"
    );
    // 重复调用幂等：已经 failed 的执行回已提交结论，不重复释放。
    let again = repository
        .cancel_unsubmitted(cancel(job_id, "supervisor-a", 0))
        .await
        .expect("a repeated cancellation is idempotent");
    assert_eq!(again.stage, ExecutionStage::Failed);

    // 提交声明落下但生成尚未开始：调用方凭带 fencing 的取消把这次声明收成 terminal 并释放，
    // 不把它伪装成可能已提交（RFC 0018 §4.1）。
    let declared = admit_one(&repository, &fixture, "cancel-with-declaration").await;
    let started = repository
        .begin_submission(begin(
            declared,
            "supervisor-a",
            0,
            Utc::now() + ChronoDuration::minutes(5),
        ))
        .await
        .expect("begin_submission");
    let released = repository
        .cancel_unsubmitted(cancel(declared, "supervisor-a", 0))
        .await
        .expect("a declaration that never reached the provider is releasable");
    assert_eq!(released.stage, ExecutionStage::Failed);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM generation.attempts WHERE id = $1")
            .bind(started.attempt_id.0)
            .fetch_one(&pool)
            .await
            .expect("the collected declaration"),
        "terminal",
        "the release closes the declaration instead of leaving it open"
    );

    // 已经交给渠道的 Attempt 是硬边界：提交可能已经在飞，不许释放。
    let accepted_job = admit_one(&repository, &fixture, "cancel-with-acceptance").await;
    let accepted = repository
        .begin_submission(begin(
            accepted_job,
            "supervisor-a",
            0,
            Utc::now() + ChronoDuration::minutes(5),
        ))
        .await
        .expect("begin_submission");
    repository
        .record_acceptance(RecordAcceptance {
            job_id: accepted_job,
            attempt_id: accepted.attempt_id,
            execution_owner: "supervisor-a".to_owned(),
            fencing_token: FencingToken::new(0),
            provider_task_handle: None,
            provider_trace_id: None,
        })
        .await
        .expect("record_acceptance");
    let refused = repository
        .cancel_unsubmitted(cancel(accepted_job, "supervisor-a", 0))
        .await
        .expect_err("an attempt that reached the provider must not be released as unsubmitted");
    assert!(matches!(refused, ApplicationError::Conflict(_)));
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM generation.jobs WHERE id = $1")
            .bind(accepted_job.0)
            .fetch_one(&pool)
            .await
            .expect("the still executing job"),
        "executing",
        "a refused cancellation changes nothing"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM generation.attempts WHERE id = $1")
            .bind(accepted.attempt_id.0)
            .fetch_one(&pool)
            .await
            .expect("the still executing attempt"),
        "accepted",
        "a refused cancellation leaves the accepted attempt untouched"
    );

    // `unknown`（结果不明）与 accepted 是同一条硬边界：同样不许按确定未提交释放。
    sqlx::query("UPDATE generation.attempts SET state = 'unknown' WHERE id = $1")
        .bind(accepted.attempt_id.0)
        .execute(&pool)
        .await
        .expect("mark the attempt unknown");
    let still_refused = repository
        .cancel_unsubmitted(cancel(accepted_job, "supervisor-a", 0))
        .await
        .expect_err("an attempt with an unknown outcome must not be released");
    assert!(matches!(still_refused, ApplicationError::Conflict(_)));

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

/// 不是标识的句柄/ trace 在类型构造处就被拒绝，端口拿不到这样的值，原文因此永不落库；
/// 合法值照常受理——拒绝只发生在值进入端口之前。
#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_handle_that_is_not_an_identifier_cannot_reach_acceptance() {
    let (repository, database_name) = connect().await;
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;
    let job_id = admit_one(&repository, &fixture, "bad-handle").await;
    let started = repository
        .begin_submission(begin(
            job_id,
            "supervisor-a",
            0,
            Utc::now() + ChronoDuration::minutes(5),
        ))
        .await
        .expect("begin_submission");

    // 非标识的"句柄"与 trace 都构造不出来：URL、data URL、控制字符与超长正文到这里为止。
    assert!(
        ProviderTaskHandle::parse("data:image/png;base64,AAAA".to_owned()).is_err(),
        "a payload must not construct a task handle"
    );
    assert!(
        ProviderTraceId::parse("https://example.invalid/trace/1").is_none(),
        "a url must not construct a trace id"
    );

    let acceptance = |handle: &str, trace: &str| RecordAcceptance {
        job_id,
        attempt_id: started.attempt_id,
        execution_owner: "supervisor-a".to_owned(),
        fencing_token: FencingToken::new(0),
        provider_task_handle: ProviderTaskHandle::parse(handle.to_owned()).ok(),
        provider_trace_id: ProviderTraceId::parse(trace),
    };

    // 非法 trace 在这里被丢弃，句柄合法时其余事实照常受理。
    repository
        .record_acceptance(acceptance("task-good", "https://example.invalid/trace/1"))
        .await
        .expect("a valid handle is accepted even when the trace is dropped");
    let stored: Option<String> =
        sqlx::query_scalar("SELECT provider_task_handle FROM generation.jobs WHERE id = $1")
            .bind(job_id.0)
            .fetch_one(&pool)
            .await
            .expect("the stored handle");
    assert_eq!(
        stored.as_deref(),
        Some("task-good"),
        "the bounded handle is stored verbatim"
    );
    let stored_trace: Option<String> =
        sqlx::query_scalar("SELECT provider_trace_id FROM generation.attempts WHERE id = $1")
            .bind(started.attempt_id.0)
            .fetch_one(&pool)
            .await
            .expect("the stored trace");
    assert!(
        stored_trace.is_none(),
        "an invalid trace is dropped instead of stored"
    );
    let state: String = sqlx::query_scalar("SELECT state FROM generation.attempts WHERE id = $1")
        .bind(started.attempt_id.0)
        .fetch_one(&pool)
        .await
        .expect("the attempt state");
    assert_eq!(state, "accepted");

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}
