//! admit 的原子性、幂等与容量占用要对着**真库**验：账户与键唯一性、可用额条件更新、预授权行
//! 与渠道容量事实都是库层的判据，不是内存里能模拟的东西（Spec 0005 §3–§4，RFC 0017 §3）。
//!
//! 用例从 HTTP_CONTRACT_DATABASE_URL 派生一次性库，跑完整迁移后对同一个键受理两次、换请求
//! 指纹一次、再换一个键耗尽渠道容量一次；另一条用四个并发请求验账户名额只放一个。
//! 跑完删掉这个库，不动基库。

use seeai_application::{
    AdmitExecution, AdmitOffering, AdmitOutcome, ApplicationError, ExecutionRepository,
    RoutingDecision,
};
use seeai_domain::{
    AccountId, ChannelId, ExecutionStage, ImageBranch, OfferingId, PriceSnapshot,
    RuntimeRevisionId, VendorModelId,
};
use seeai_persistence::PgHubRepository;
use serde_json::{Value, json};
use sqlx::{AssertSqlSafe, PgPool, Row};
use std::sync::Arc;
use uuid::Uuid;

async fn isolated_database_url() -> (String, String) {
    let base = std::env::var("HTTP_CONTRACT_DATABASE_URL")
        .expect("HTTP_CONTRACT_DATABASE_URL is required for the ignored admit test");
    let admin = PgPool::connect(&base)
        .await
        .expect("connect to the provided contract database");
    let name = format!("seeai_admit_{}", Uuid::new_v4().simple());
    sqlx::query(AssertSqlSafe(format!("CREATE DATABASE \"{name}\"")))
        .execute(&admin)
        .await
        .expect("create an isolated admit database");
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
    sqlx::query(
        "INSERT INTO ledger.accounts (id, balance_microusd, held_microusd, version, kind, name)
         VALUES ($1, 5000, 0, 0, 'consumer', 'admit test account')",
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
         VALUES ($1, '{}'::jsonb, 'admit-test', 'fake-gateway', $2)",
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

fn command(
    fixture: &Fixture,
    idempotency_key_digest: &str,
    request_digest: &str,
) -> AdmitExecution {
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
        idempotency_key_digest: idempotency_key_digest.to_owned(),
        request_digest: request_digest.to_owned(),
        request_digest_key_version: 1,
        max_cost_microusd: 1000,
        max_account_in_flight: 8,
        max_channel_in_flight: 4,
    }
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
async fn admit_is_atomic_idempotent_and_owns_a_channel_slot() {
    let (database_url, database_name) = isolated_database_url().await;
    let repository = PgHubRepository::connect(&database_url, 4)
        .await
        .expect("the isolated database");
    repository.migrate().await.expect("the migrations apply");
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;

    // 首次受理：Job、Hold 与容量事实同事务落库。
    let first = repository
        .admit(command(&fixture, "key-a", "request-digest-1"))
        .await
        .expect("the first admit");
    let AdmitOutcome::Admitted { job, balance } = &first else {
        panic!("the first admit must create a job, got {first:?}");
    };
    assert_eq!(
        job.stage,
        ExecutionStage::Admitted,
        "a fresh job is admitted"
    );
    assert_eq!(
        balance.held_microusd, 1000,
        "the hold is now on the account"
    );
    assert_eq!(
        balance.available_microusd, 4000,
        "available = balance - held"
    );

    let job_row = sqlx::query(
        "SELECT execution_protocol, state, idempotency_key, request_hash,
                native_parameters, carrier_schema, result_images
         FROM generation.jobs WHERE id = $1",
    )
    .bind(job.job_id.0)
    .fetch_one(&pool)
    .await
    .expect("the admitted job row");
    assert_eq!(
        job_row
            .try_get::<String, _>("execution_protocol")
            .expect("protocol"),
        "v1"
    );
    assert_eq!(
        job_row.try_get::<String, _>("state").expect("state"),
        "admitted"
    );
    // 业务载荷列必须为空：明文键、请求哈希、原生参数、承载面与结果都不进新协议记录。
    assert_eq!(
        job_row
            .try_get::<Option<String>, _>("idempotency_key")
            .expect("key"),
        None
    );
    assert_eq!(
        job_row
            .try_get::<Option<String>, _>("request_hash")
            .expect("hash"),
        None
    );
    assert_eq!(
        job_row
            .try_get::<Option<Value>, _>("native_parameters")
            .expect("params"),
        None
    );
    assert_eq!(
        job_row
            .try_get::<Option<Value>, _>("carrier_schema")
            .expect("carrier"),
        None
    );
    assert_eq!(
        job_row
            .try_get::<Option<Value>, _>("result_images")
            .expect("result"),
        None
    );
    let holds = scalar(
        &pool,
        "SELECT count(*) FROM ledger.holds WHERE job_id = $1 AND status = 'active'",
        job.job_id.0,
    )
    .await;
    assert_eq!(holds, 1, "one active hold");
    let slots = scalar(
        &pool,
        "SELECT count(*) FROM generation.execution_capacity WHERE job_id = $1 AND state = 'held'",
        job.job_id.0,
    )
    .await;
    assert_eq!(slots, 1, "one held channel slot");

    // 同键同指纹：只读投影，不新建、不占用。
    let second = repository
        .admit(command(&fixture, "key-a", "request-digest-1"))
        .await
        .expect("the replay");
    let AdmitOutcome::Replayed(replay) = second else {
        panic!("the same key and digest must replay");
    };
    assert_eq!(
        replay.job_id, job.job_id,
        "the replay points at the original"
    );
    assert_eq!(replay.stage, ExecutionStage::Admitted);
    let jobs_after_replay = scalar(
        &pool,
        "SELECT count(*) FROM generation.jobs WHERE account_id = $1",
        fixture.account_id.0,
    )
    .await;
    assert_eq!(
        jobs_after_replay, 1,
        "a replay must not create a second job"
    );
    let held_after_replay: i64 =
        sqlx::query_scalar("SELECT held_microusd FROM ledger.accounts WHERE id = $1")
            .bind(fixture.account_id.0)
            .fetch_one(&pool)
            .await
            .expect("account after replay");
    assert_eq!(held_after_replay, 1000, "a replay must not reserve again");
    let slots_after_replay = scalar(
        &pool,
        "SELECT count(*) FROM generation.execution_capacity WHERE job_id = $1 AND state = 'held'",
        job.job_id.0,
    )
    .await;
    assert_eq!(
        slots_after_replay, 1,
        "a replay must not take a second slot"
    );

    // 同键不同指纹：冲突，不覆盖、不新建。
    let conflict = repository
        .admit(command(&fixture, "key-a", "request-digest-2"))
        .await;
    assert!(
        matches!(conflict, Err(ApplicationError::Conflict(_))),
        "a different request digest under the same key must conflict, got {conflict:?}"
    );
    let jobs_after_conflict = scalar(
        &pool,
        "SELECT count(*) FROM generation.jobs WHERE account_id = $1",
        fixture.account_id.0,
    )
    .await;
    assert_eq!(jobs_after_conflict, 1, "a conflict must not create a job");

    // 渠道全局容量已满：换一个键也进不来，且不留任何新行。
    let mut blocked = command(&fixture, "key-b", "request-digest-3");
    blocked.max_channel_in_flight = 1;
    let rejected = repository.admit(blocked).await;
    assert!(
        matches!(rejected, Err(ApplicationError::PlatformCapacityExhausted)),
        "a full channel must reject a new acceptance, got {rejected:?}"
    );
    let jobs_after_rejection = scalar(
        &pool,
        "SELECT count(*) FROM generation.jobs WHERE account_id = $1",
        fixture.account_id.0,
    )
    .await;
    assert_eq!(
        jobs_after_rejection, 1,
        "a capacity rejection leaves no job"
    );

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

/// 切换前已在飞的旧协议 Job 同样占用渠道上游并发：即使没有任何 v1 槽位也要计入渠道上限；
/// 旧 Job 进终态后不再计入（RFC 0017 §7、Spec 0005 A9）。
#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn legacy_in_flight_jobs_count_toward_the_channel_capacity() {
    let (database_url, database_name) = isolated_database_url().await;
    let repository = PgHubRepository::connect(&database_url, 4)
        .await
        .expect("the isolated database");
    repository.migrate().await.expect("the migrations apply");
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;

    sqlx::query(
        "INSERT INTO generation.jobs
             (id, account_id, state, branch, gateway_model, runtime_revision_id, vendor_model_id,
              offering_id, channel_id, adapter_key, provider_model_id, base_url, credential_env,
              price_snapshot, max_cost_microusd)
         VALUES ($1,$2,'submitting','prompt_only','fake-gateway',$3,$4,$5,$6,'fake','fake-model',
                 'http://127.0.0.1:9','FAKE_PROVIDER_KEY','{}'::jsonb,1000)",
    )
    .bind(Uuid::new_v4())
    .bind(fixture.account_id.0)
    .bind(fixture.runtime_revision_id.0)
    .bind(fixture.vendor_model_id.0)
    .bind(fixture.offering_id.0)
    .bind(fixture.channel_id.0)
    .execute(&pool)
    .await
    .expect("seed a legacy in-flight job");

    let mut blocked = command(&fixture, "legacy-slot", "legacy-slot-digest");
    blocked.max_channel_in_flight = 1;
    let rejected = repository.admit(blocked).await;
    assert!(
        matches!(rejected, Err(ApplicationError::PlatformCapacityExhausted)),
        "a legacy in-flight job must consume the channel capacity, got {rejected:?}"
    );

    // 旧 Job 进终态后不再计入，同一渠道可以再受理。
    sqlx::query(
        "UPDATE generation.jobs SET state = 'succeeded' WHERE account_id = $1 AND execution_protocol = 'legacy'",
    )
    .bind(fixture.account_id.0)
    .execute(&pool)
    .await
    .expect("finish the legacy job");
    let mut allowed = command(&fixture, "legacy-slot-2", "legacy-slot-digest-2");
    allowed.max_channel_in_flight = 1;
    let admitted = repository
        .admit(allowed)
        .await
        .expect("after the legacy job finished");
    assert!(matches!(admitted, AdmitOutcome::Admitted { .. }));

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}
/// 并发受理共同遵守同一份账户名额：四个不同键同时进来，名额只允许一个通过。
#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn concurrent_admits_respect_the_account_slot() {
    let (database_url, database_name) = isolated_database_url().await;
    let repository = Arc::new(
        PgHubRepository::connect(&database_url, 4)
            .await
            .expect("the isolated database"),
    );
    repository.migrate().await.expect("the migrations apply");
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;

    let mut handles = Vec::new();
    for index in 0..4 {
        let repository = Arc::clone(&repository);
        let mut command = command(
            &fixture,
            &format!("concurrent-key-{index}"),
            &format!("concurrent-digest-{index}"),
        );
        // 名额只放一个；渠道与资金都够，唯一能挡住并发的必须是这份账户名额。
        command.max_account_in_flight = 1;
        handles.push(tokio::spawn(async move { repository.admit(command).await }));
    }
    let mut admitted = 0;
    let mut rejected = 0;
    for handle in handles {
        match handle.await.expect("the admit task") {
            Ok(AdmitOutcome::Admitted { .. }) => admitted += 1,
            Err(ApplicationError::TooManyInFlight) => rejected += 1,
            other => panic!("unexpected concurrent admit outcome: {other:?}"),
        }
    }
    assert_eq!(
        admitted, 1,
        "exactly one concurrent admit may take the slot"
    );
    assert_eq!(
        rejected, 3,
        "the other three must be rejected by the account slot"
    );
    let jobs = scalar(
        &pool,
        "SELECT count(*) FROM generation.jobs WHERE account_id = $1",
        fixture.account_id.0,
    )
    .await;
    assert_eq!(jobs, 1, "only the admitted request leaves a job");

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}
/// 保底额可以为零（账户资金 Spec v4 §2.1、迁移 0009 起 holds.amount_microusd 允许 0）：
/// 零元预授权在可用额非负时放行并留下占用行，可用额为负时仍按资金闸门拒绝——占用为零
/// 不豁免资金判定。
#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_zero_hold_is_admitted_but_a_negative_available_still_rejects() {
    let (database_url, database_name) = isolated_database_url().await;
    let repository = PgHubRepository::connect(&database_url, 4)
        .await
        .expect("the isolated database");
    repository.migrate().await.expect("the migrations apply");
    let pool = repository.pool().clone();
    let fixture = seed_fixture(&pool).await;

    // 可用额正好为零：零元预授权放行，占用不变。
    sqlx::query("UPDATE ledger.accounts SET balance_microusd = 0 WHERE id = $1")
        .bind(fixture.account_id.0)
        .execute(&pool)
        .await
        .expect("set a zero balance");
    let mut zero = command(&fixture, "zero-hold", "zero-digest");
    zero.max_cost_microusd = 0;
    let admitted = repository
        .admit(zero)
        .await
        .expect("a zero hold is allowed when the available amount is not negative");
    let AdmitOutcome::Admitted { job, balance } = admitted else {
        panic!("a fresh zero-hold request must be admitted, got {admitted:?}");
    };
    assert_eq!(balance.held_microusd, 0, "a zero hold reserves nothing");
    assert_eq!(balance.available_microusd, 0);
    let holds = scalar(
        &pool,
        "SELECT count(*) FROM ledger.holds WHERE job_id = $1",
        job.job_id.0,
    )
    .await;
    assert_eq!(holds, 1, "the zero hold still leaves a hold row");

    // 负可用额加零元预授权：仍然拒绝，且不新建 Job。
    sqlx::query("UPDATE ledger.accounts SET balance_microusd = -5 WHERE id = $1")
        .bind(fixture.account_id.0)
        .execute(&pool)
        .await
        .expect("push the available amount negative");
    let mut negative = command(&fixture, "negative-available", "negative-digest");
    negative.max_cost_microusd = 0;
    let rejected = repository.admit(negative).await;
    assert!(
        matches!(rejected, Err(ApplicationError::InsufficientBalance)),
        "a negative available amount rejects even a zero hold, got {rejected:?}"
    );
    let jobs = scalar(
        &pool,
        "SELECT count(*) FROM generation.jobs WHERE account_id = $1",
        fixture.account_id.0,
    )
    .await;
    assert_eq!(jobs, 1, "the rejected zero-hold request leaves no job");

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}
