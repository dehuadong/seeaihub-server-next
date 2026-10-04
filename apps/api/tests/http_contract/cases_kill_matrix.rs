//! A5 的**进程级强杀**矩阵（Spec 0005 §5、§8）。
//!
//! 与 `crates/application/tests/execution_reconciliation.rs`（直接构造数据库状态再让 Worker 接管）
//! 互补：这里真的把一次执行跑到某一格，用**屏障**确认 API 确实到了那一格，再给 API 子进程发
//! SIGKILL，最后用**同一份生产对账代码**（`ExecutionReconciliationService` + 真渠道 Driver）对着
//! 同一个库恢复，核对假上游的生成调用计数与 Job / Attempt / Hold / 渠道槽位。
//!
//! 屏障不是 sleep 猜时间：
//!
//! - 假上游的闸门（[`UpstreamGate`]）：命中目标请求时置到达信号并停住，等用例放行。它证明的是
//!   "请求真的发出去了、响应还没回"；
//! - 数据库的行/表锁：用例先持锁，再放行上游，API 的下一句写库必然停在锁等待上；用例等的信号是
//!   PostgreSQL 自己的 `pg_stat_activity.wait_event_type = 'Lock'`。
//!
//! SIGKILL 之后还有一步夹具动作：把**仍挂在锁等待上**的那条后端用 `pg_terminate_backend` 终止。
//! 进程死了以后 PostgreSQL 不会立刻发现连接已断——一个正等锁的后端要等到锁被放行、下一次碰
//! socket 才知道对端没了，而那一刻它可能已经把事务提交掉。用例要的是"崩溃即事务未提交"，所以
//! 在放掉自己的锁之前先掐掉它；这是连接断开被检测到的同一个结果。
//!
//! 每格都断言：假上游**生成请求计数**在恢复后不增加（或本就没有）；Job / Attempt / Hold /
//! 渠道槽位符合 §5；需要时由 Worker 对账接管。
//!
//! 前置：真实 PostgreSQL（`HTTP_CONTRACT_DATABASE_URL`，且角色能建库）、Linux（SIGKILL 与
//! `std::os::unix`）。用例派生一次性库、起真实 API 子进程、只连进程内假上游，**不产生任何外部
//! 调用**。全部 `#[ignore]`。

use super::*;
use chrono::Duration as ChronoDuration;
use seeai_adapter_aihubmix::AihubmixAdapterFactory;
use seeai_adapter_apimart::ApimartAdapterFactory;
use seeai_adapter_sdk::ProviderCredential;
use seeai_application::{
    AdapterRegistry, ApplicationError, CredentialProvider, ExecutionReconciliationService,
    ReconciliationPolicy, RetryPolicy,
};
use seeai_persistence::PgHubRepository;
use sqlx::PgPool;

/// 这些用例的同步等待窗口：要留得下"卡住某一格 + 杀进程 + 恢复"，所以比别的用例宽。
const KILL_SYNC_WAIT_SECONDS: u64 = 30;

/// 强杀矩阵用的候选：任务式（APIMart）渠道覆盖句柄入库与轮询两格；分支声明齐全，
/// 参考图走 data URL 时先上传换 URL（"提交前"那一格用它把 API 停在生成请求之前）。
fn apimart_draft() -> Value {
    candidate(
        "APIMart",
        "apimart-image-v1",
        &["prompt_only", "image_conditioned", "masked"],
    )
}

/// 起一次直接执行（不启 Worker），并给假上游装上闸门。
async fn direct_with_hold(behaviour: UpstreamBehaviour) -> Harness {
    Harness::start_direct(apimart_draft(), behaviour, 4, KILL_SYNC_WAIT_SECONDS).await
}

/// 后台发一次生成请求。
///
/// 进程会被 SIGKILL，所以这里不解析响应：连接被对端复位是预期结果，用例要的是"请求真的发出去了"
/// 以及"API 到了哪一格"。
fn spawn_generation(harness: &Harness, key: String, body: Value) -> tokio::task::JoinHandle<()> {
    let base_url = harness.base_url.clone();
    let api_key = harness.api_key.clone();
    tokio::spawn(async move {
        let _ = Client::new()
            .post(format!("{base_url}/v1/images/generations"))
            .bearer_auth(api_key)
            .header("idempotency-key", key)
            .json(&body)
            .send()
            .await;
    })
}

/// 夹具账户的标识。
fn fixture_account(harness: &Harness) -> Uuid {
    Uuid::parse_str(&harness.account_id).expect("the fixture account id")
}

/// 一条**专用**连接池：屏障事务占住其中一条连接，其余的用来读 `pg_stat_activity` 与执行事实。
///
/// 不借 `harness.pool` 是因为并发上限不归这些用例管：专用池的连接数由这里显式定死。
async fn observation_pool(harness: &Harness) -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&harness.database_url)
        .await
        .expect("a second pool on the fixture database")
}

/// 这次执行在本夹具账户下留下的唯一 Job。
async fn the_job_id(pool: &PgPool, account_id: Uuid) -> Uuid {
    sqlx::query_scalar("SELECT id FROM generation.jobs WHERE account_id = $1")
        .bind(account_id)
        .fetch_one(pool)
        .await
        .expect("the killed execution must have left a job record")
}

async fn job_state(pool: &PgPool, job_id: Uuid) -> String {
    sqlx::query_scalar("SELECT state FROM generation.jobs WHERE id = $1")
        .bind(job_id)
        .fetch_one(pool)
        .await
        .expect("the job state")
}

/// 最新一条 Attempt 的状态（没有 Attempt 就是 `None`）。
async fn latest_attempt(pool: &PgPool, job_id: Uuid) -> Option<String> {
    sqlx::query_scalar(
        "SELECT state FROM generation.attempts WHERE job_id = $1 \
         ORDER BY attempt_no DESC LIMIT 1",
    )
    .bind(job_id)
    .fetch_optional(pool)
    .await
    .expect("the latest attempt state")
}

async fn attempt_count(pool: &PgPool, job_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM generation.attempts WHERE job_id = $1")
        .bind(job_id)
        .fetch_one(pool)
        .await
        .expect("the attempt count")
}

async fn handle_of(pool: &PgPool, job_id: Uuid) -> Option<String> {
    sqlx::query_scalar("SELECT provider_task_handle FROM generation.jobs WHERE id = $1")
        .bind(job_id)
        .fetch_one(pool)
        .await
        .expect("the provider task handle")
}

async fn held_microusd(pool: &PgPool, account_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT held_microusd FROM ledger.accounts WHERE id = $1")
        .bind(account_id)
        .fetch_one(pool)
        .await
        .expect("the account hold total")
}

/// 这台 Job 当前生效的预授权金额：强杀恢复后 Hold 必须原样留着。
async fn active_hold(pool: &PgPool, job_id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT amount_microusd FROM ledger.holds WHERE job_id = $1 AND status = 'active'",
    )
    .bind(job_id)
    .fetch_one(pool)
    .await
    .expect("the active hold")
}

async fn captures(pool: &PgPool, job_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM ledger.entries WHERE job_id = $1 AND kind = 'capture'")
        .bind(job_id)
        .fetch_one(pool)
        .await
        .expect("the capture count")
}

async fn open_cases(pool: &PgPool, job_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM operations.reconciliation_cases WHERE job_id = $1")
        .bind(job_id)
        .fetch_one(pool)
        .await
        .expect("the reconciliation case count")
}

async fn capacity_state(pool: &PgPool, job_id: Uuid) -> String {
    sqlx::query_scalar("SELECT state FROM generation.execution_capacity WHERE job_id = $1")
        .bind(job_id)
        .fetch_one(pool)
        .await
        .expect("the channel slot state")
}

/// 把被杀进程的租约推到过期：进程死后再没有续约，对账只接管过期所有权。
///
/// 这是夹具对时间的前推，不是对状态的伪造——`takeover_expired_executions` 的判据就是
/// `lease_expires_at <= now()`（Spec 0005 §6 的"过期所有权清理"）。
async fn expire_lease(pool: &PgPool, job_id: Uuid) {
    sqlx::query(
        "UPDATE generation.jobs SET lease_expires_at = now() - interval '1 second' WHERE id = $1",
    )
    .bind(job_id)
    .execute(pool)
    .await
    .expect("expire the killed execution's lease");
}

/// 等这个库上真的出现一个锁等待的后端（PostgreSQL 自己的事实，不是 sleep 猜时间）。
async fn await_lock_waiter(pool: &PgPool, database_name: &str, what: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity \
             WHERE datname = $1 AND wait_event_type = 'Lock'",
        )
        .bind(database_name)
        .fetch_one(pool)
        .await
        .expect("read pg_stat_activity");
        if waiting > 0 {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what}: no backend reached the lock wait"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// SIGKILL 之后，把仍挂在锁等待上的后端终止掉（理由见模块文档）。
///
/// 返回终止的条数：调用方据此断言"确实有一条被杀的进程卡在那一格"。
async fn terminate_lock_waiters(pool: &PgPool, database_name: &str) -> u32 {
    let pids: Vec<i32> = sqlx::query_scalar(
        "SELECT pid FROM pg_stat_activity WHERE datname = $1 AND wait_event_type = 'Lock'",
    )
    .bind(database_name)
    .fetch_all(pool)
    .await
    .expect("list the lock waiters");
    for pid in &pids {
        sqlx::query("SELECT pg_terminate_backend($1)")
            .bind(pid)
            .execute(pool)
            .await
            .expect("terminate the killed execution's backend");
    }
    u32::try_from(pids.len()).unwrap_or(u32::MAX)
}

/// 对账用的渠道凭证替身：强杀用例只连进程内假上游，值只在这里用过一次。
struct FixtureCredentials;

impl CredentialProvider for FixtureCredentials {
    fn resolve(&self, _reference: &str) -> Result<ProviderCredential, ApplicationError> {
        ProviderCredential::new(CONTRACT_PROVIDER_KEY.to_owned())
            .map_err(|error| ApplicationError::Configuration(error.to_string()))
    }
}

/// 强杀矩阵的对账策略：孤儿回收年龄归零（用例要立刻看到回收），其余取等得起的量级。
fn kill_matrix_policy() -> ReconciliationPolicy {
    ReconciliationPolicy {
        batch_limit: 8,
        orphan_max_age: ChronoDuration::seconds(0),
        late_fact_claim_ttl: ChronoDuration::minutes(5),
        query_timeout: Duration::from_secs(10),
        query_max_attempts: 5,
        query_backoff_base: Duration::from_secs(1),
        query_backoff_max: Duration::from_secs(60),
        // 慢周期账务核对这一轮不跑：用例只关心被强杀的那条执行。
        ledger_audit_every_rounds: 100,
        ledger_audit_limit: 100,
        ledger_audit_window: Duration::from_secs(3600),
    }
}

/// 生产同一份 Worker 对账代码，跑在测试进程里对着同一个一次性库。
fn reconciler(repository: Arc<PgHubRepository>) -> ExecutionReconciliationService {
    let adapters: Arc<dyn seeai_application::AdapterFactory> =
        Arc::new(AdapterRegistry::new(vec![
            Arc::new(AihubmixAdapterFactory),
            Arc::new(ApimartAdapterFactory),
        ]));
    ExecutionReconciliationService::new(
        repository.clone(),
        repository,
        adapters,
        Arc::new(FixtureCredentials),
        "kill-matrix-worker".to_owned(),
        ChronoDuration::seconds(
            i64::try_from(KILL_SYNC_WAIT_SECONDS).expect("the takeover lease fits in i64"),
        ),
    )
    .with_policy(kill_matrix_policy())
    .with_retry_policy(RetryPolicy {
        max_attempts: 2,
        backoff_base: Duration::from_millis(1),
    })
}

/// 连上夹具的一次性库，供恢复用。
async fn recovery_repository(harness: &Harness) -> Arc<PgHubRepository> {
    Arc::new(
        PgHubRepository::connect(&harness.database_url, 4)
            .await
            .expect("the fixture database for recovery"),
    )
}

/// **格 1a：提交前（受理已提交、提交声明还没落库）**。
///
/// 注入点：用例先对 `generation.attempts` 加 `ACCESS EXCLUSIVE` 锁。`admit` 不碰这张表，所以
/// API 能把受理提交掉；到 `begin_submission` 写提交声明时必然停在锁上——那一格是"受理已提交、
/// 生成请求还没发出"。
///
/// 恢复：库里是 `admitted` + 无 Attempt，没有提交声明就确定没有外部副作用，孤儿回收释放 Hold
/// 与渠道槽位；生成请求计数全程为 0。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; SIGKILLs a real API child process (Linux only)"]
async fn sigkill_before_the_submission_declaration_reaps_the_orphan_admission() {
    let mut harness = direct_with_hold(UpstreamBehaviour::apimart()).await;
    let observation = observation_pool(&harness).await;
    let account_id = fixture_account(&harness);

    // 屏障：整表锁住 attempts。admit 不碰它，begin_submission 一插提交声明就停。
    let mut blocker = observation.begin().await.expect("the barrier transaction");
    sqlx::query("LOCK TABLE generation.attempts IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .expect("hold the attempts table");

    let key = format!("kill-before-submit-{}", Uuid::new_v4());
    let body = route_request(harness.model, "kill before the submission declaration");
    let request = spawn_generation(&harness, key.clone(), body.clone());

    // 屏障信号：`pg_stat_activity` 里真的有一条在等锁，且库里已经有一条 admitted 的受理事实。
    //
    // 这一格里**读不了** `generation.attempts`：整表锁连 SELECT 一起挡，从别的连接查它会把用例
    // 自己卡死。所以"提交声明没落库"这条事实放在杀进程、放锁之后再断言（见下）。
    await_lock_waiter(
        &observation,
        &harness.database_name,
        "the API must block on the attempts table",
    )
    .await;
    let job_id = the_job_id(&observation, account_id).await;
    assert_eq!(job_state(&observation, job_id).await, "admitted");
    assert_eq!(
        harness.create_calls(),
        0,
        "no generation request may be sent before the submission declaration is committed"
    );

    harness._api.sigkill();
    request.abort();
    // 先把仍等在锁上的后端掐掉，再放掉自己的锁：否则它会替死掉的进程把提交声明提交掉。
    assert_eq!(
        terminate_lock_waiters(&observation, &harness.database_name).await,
        1,
        "exactly the killed API backend must still be waiting on the lock"
    );
    blocker.rollback().await.expect("release the barrier");
    assert_eq!(
        attempt_count(&observation, job_id).await,
        0,
        "the submission declaration aborted with the killed process"
    );

    let repository = recovery_repository(&harness).await;
    let service = reconciler(repository.clone());
    let report = service.run_once().await.expect("one reconciliation round");
    assert_eq!(report.reaped_orphans, 1, "the orphan admission is reaped");

    assert_eq!(job_state(&observation, job_id).await, "failed");
    assert_eq!(attempt_count(&observation, job_id).await, 0);
    assert_eq!(
        held_microusd(&observation, account_id).await,
        0,
        "a provably unsubmitted admission releases its hold"
    );
    assert_eq!(capacity_state(&observation, job_id).await, "released");
    assert_eq!(
        open_cases(&observation, job_id).await,
        0,
        "nothing is unknown, so no reconciliation case is opened"
    );
    assert_eq!(
        harness.create_calls(),
        0,
        "recovery must not send a generation request"
    );
    assert_eq!(harness.count("POST", "/v1/uploads/images"), 0);
    assert_eq!(captures(&observation, job_id).await, 0);

    // 同键重发只投影那条已经收成失败的记录：不会再发一次生成请求。
    //
    // 这一格回收时记录里没有原平台错误码（进程死在受理与提交之间，没有任何 Provider 事实），
    // 因此按既有回退口径投影 `502 platform_unavailable`。这不是重发未知请求：上游计数仍是 0。
    let (replica_url, replica) = harness
        .start_replica(4, KILL_SYNC_WAIT_SECONDS, &ApiProcessSettings::default())
        .await;
    let (status, replay) = post_json(
        &replica_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &body,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "got {replay}");
    assert_eq!(
        replay["error"]["code"].as_str(),
        Some("platform_unavailable"),
        "a reaped orphan admission projects its recorded determined failure"
    );
    assert_eq!(
        harness.create_calls(),
        0,
        "the same-key replay must not send a generation request"
    );
    drop(replica);

    drop(service);
    drop(repository);
    observation.close().await;
    harness.cleanup().await;
}

/// **格 1b：提交前（生成请求还没发出，但提交声明已经落库）**。
///
/// 注入点：请求带 data URL 参考图，Adapter 必须先用 `POST /v1/uploads/images` 换公网 URL。
/// 假上游把这条上传请求停住——此刻提交声明已入库、生成请求一次都没发。
///
/// 恢复：库里是 `executing` + Attempt `submitting`、没有句柄。崩溃后的 `submitting` 与"正在发"
/// 不可区分，§5 因此保留 Hold 与渠道槽位并建案，绝不重提；即使用例知道生成请求没发出去，
/// 已落库的事实也证明不了这一点，平台按合同不能据此重投。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; SIGKILLs a real API child process (Linux only)"]
async fn sigkill_with_the_submission_declared_but_before_the_create_request_keeps_the_hold() {
    let mut harness =
        direct_with_hold(UpstreamBehaviour::apimart().holding(HeldRequest::Upload)).await;
    let observation = observation_pool(&harness).await;
    let account_id = fixture_account(&harness);

    let key = format!("kill-before-create-{}", Uuid::new_v4());
    let mut body = route_request(harness.model, "kill before the create request");
    body["image_urls"] = json!([png_data_url()]);
    let request = spawn_generation(&harness, key.clone(), body);

    // 屏障信号：假上游真的收到了上传请求（生成请求一定还没发）。
    harness.gate().wait_for_arrival(1).await;
    let job_id = the_job_id(&observation, account_id).await;
    let hold = active_hold(&observation, job_id).await;
    assert_eq!(job_state(&observation, job_id).await, "executing");
    assert_eq!(
        latest_attempt(&observation, job_id).await.as_deref(),
        Some("submitting")
    );
    assert_eq!(handle_of(&observation, job_id).await, None);
    assert_eq!(harness.count("POST", "/v1/uploads/images"), 1);
    assert_eq!(harness.create_calls(), 0);

    harness._api.sigkill();
    request.abort();
    harness.gate().release_all();
    expire_lease(&observation, job_id).await;

    let repository = recovery_repository(&harness).await;
    let service = reconciler(repository.clone());
    let report = service.run_once().await.expect("one reconciliation round");
    assert_eq!(report.reconciled, 1);

    assert_eq!(
        job_state(&observation, job_id).await,
        "reconciliation_required"
    );
    assert_eq!(
        latest_attempt(&observation, job_id).await.as_deref(),
        Some("unknown")
    );
    assert_eq!(open_cases(&observation, job_id).await, 1);
    assert_eq!(
        held_microusd(&observation, account_id).await,
        hold,
        "the hold is retained: a crash cannot prove the provider was not asked"
    );
    assert_eq!(capacity_state(&observation, job_id).await, "held");
    assert_eq!(captures(&observation, job_id).await, 0);
    assert_eq!(
        harness.create_calls(),
        0,
        "recovery must not send a generation request"
    );
    assert_eq!(
        harness.count("GET", "/v1/tasks/"),
        0,
        "no handle means no read-only query"
    );

    drop(service);
    drop(repository);
    observation.close().await;
    harness.cleanup().await;
}

/// **格 2：提交中／未知接受（生成请求已发出，响应没回来）**。
///
/// 注入点：假上游把生成请求的响应停住。库里是 `executing` + Attempt `submitting`、没有句柄；
/// 上游确实收到了 1 条生成请求。
///
/// 恢复：§5 的"已发请求但接受状态未知"——保留占用、禁止自动重提，生成请求计数仍是 1。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; SIGKILLs a real API child process (Linux only)"]
async fn sigkill_while_the_create_response_is_missing_keeps_the_hold_and_never_resends() {
    let mut harness =
        direct_with_hold(UpstreamBehaviour::apimart().holding(HeldRequest::Create)).await;
    let observation = observation_pool(&harness).await;
    let account_id = fixture_account(&harness);

    let key = format!("kill-create-inflight-{}", Uuid::new_v4());
    let request = spawn_generation(
        &harness,
        key.clone(),
        route_request(harness.model, "kill while the create response is missing"),
    );

    harness.gate().wait_for_arrival(1).await;
    let job_id = the_job_id(&observation, account_id).await;
    let hold = active_hold(&observation, job_id).await;
    assert_eq!(job_state(&observation, job_id).await, "executing");
    assert_eq!(
        latest_attempt(&observation, job_id).await.as_deref(),
        Some("submitting")
    );
    assert_eq!(
        handle_of(&observation, job_id).await,
        None,
        "the handle cannot be stored before the response arrives"
    );
    assert_eq!(
        harness.create_calls(),
        1,
        "the create request was sent once"
    );

    harness._api.sigkill();
    request.abort();
    harness.gate().release_all();
    expire_lease(&observation, job_id).await;

    let repository = recovery_repository(&harness).await;
    let service = reconciler(repository.clone());
    let report = service.run_once().await.expect("one reconciliation round");
    assert_eq!(report.reconciled, 1);

    assert_eq!(
        job_state(&observation, job_id).await,
        "reconciliation_required"
    );
    assert_eq!(
        latest_attempt(&observation, job_id).await.as_deref(),
        Some("unknown")
    );
    assert_eq!(open_cases(&observation, job_id).await, 1);
    assert_eq!(held_microusd(&observation, account_id).await, hold);
    assert_eq!(capacity_state(&observation, job_id).await, "held");
    assert_eq!(captures(&observation, job_id).await, 0);
    assert_eq!(
        harness.create_calls(),
        1,
        "a possibly accepted request must never be resubmitted"
    );
    assert_eq!(harness.count("GET", "/v1/tasks/"), 0);

    drop(service);
    drop(repository);
    observation.close().await;
    harness.cleanup().await;
}

/// **格 3：接受后句柄未写入（上游已受理，句柄还没入库）**。
///
/// 注入点：假上游收到生成请求后先停住；用例锁住这台 Job 的行，再放行提交应答。`record_acceptance`
/// 的锁序是先 Job 行再 Attempt 行，所以它必然停在 Job 行锁上——那一格是"上游已受理、句柄还没入库、
/// 一次轮询都还没发"。
///
/// 恢复：库里没有句柄（§3 要求句柄先入库再轮询），只能保留占用并建案；生成请求计数仍是 1。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; SIGKILLs a real API child process (Linux only)"]
async fn sigkill_after_acceptance_before_the_handle_is_stored_keeps_the_hold() {
    let mut harness =
        direct_with_hold(UpstreamBehaviour::apimart().holding(HeldRequest::Create)).await;
    let observation = observation_pool(&harness).await;
    let account_id = fixture_account(&harness);

    let key = format!("kill-handle-unstored-{}", Uuid::new_v4());
    let request = spawn_generation(
        &harness,
        key.clone(),
        route_request(
            harness.model,
            "kill after acceptance, before the handle is stored",
        ),
    );

    harness.gate().wait_for_arrival(1).await;
    let job_id = the_job_id(&observation, account_id).await;
    let hold = active_hold(&observation, job_id).await;
    // 屏障：先锁住 Job 行，再放行上游的提交应答。
    let mut blocker = observation.begin().await.expect("the barrier transaction");
    sqlx::query("SELECT id FROM generation.jobs WHERE id = $1 FOR UPDATE")
        .bind(job_id)
        .fetch_one(&mut *blocker)
        .await
        .expect("hold the job row");
    harness.gate().release_all();
    await_lock_waiter(
        &observation,
        &harness.database_name,
        "record_acceptance must block on the job row",
    )
    .await;

    assert_eq!(
        handle_of(&observation, job_id).await,
        None,
        "the handle must not be stored yet"
    );
    assert_eq!(
        harness.count("GET", "/v1/tasks/"),
        0,
        "no polling before the handle is committed"
    );
    assert_eq!(harness.create_calls(), 1);

    harness._api.sigkill();
    request.abort();
    assert_eq!(
        terminate_lock_waiters(&observation, &harness.database_name).await,
        1,
        "exactly the killed API backend must still be waiting on the job row"
    );
    blocker.rollback().await.expect("release the barrier");
    expire_lease(&observation, job_id).await;

    let repository = recovery_repository(&harness).await;
    let service = reconciler(repository.clone());
    let report = service.run_once().await.expect("one reconciliation round");
    assert_eq!(report.reconciled, 1);

    assert_eq!(
        job_state(&observation, job_id).await,
        "reconciliation_required"
    );
    assert_eq!(
        latest_attempt(&observation, job_id).await.as_deref(),
        Some("unknown")
    );
    assert_eq!(open_cases(&observation, job_id).await, 1);
    assert_eq!(held_microusd(&observation, account_id).await, hold);
    assert_eq!(capacity_state(&observation, job_id).await, "held");
    assert_eq!(captures(&observation, job_id).await, 0);
    assert_eq!(
        harness.create_calls(),
        1,
        "an accepted request without a stored handle must not be resubmitted"
    );
    assert_eq!(
        harness.count("GET", "/v1/tasks/"),
        0,
        "without a stored handle there is nothing to query read-only"
    );

    drop(service);
    drop(repository);
    observation.close().await;
    harness.cleanup().await;
}

/// **格 4：轮询中（句柄已入库，终态还没回）**。
///
/// 注入点：假上游把第一次任务查询的响应停住。库里已经有句柄（A6 要求先入库再轮询），这一次轮询
/// 的终态没有回来。
///
/// 恢复：Worker 只按原句柄只读查询、取得有效证据后**结算一次**，不重发生成请求、不恢复图片。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; SIGKILLs a real API child process (Linux only)"]
async fn sigkill_while_polling_settles_once_on_recovery_without_resubmitting() {
    let mut harness =
        direct_with_hold(UpstreamBehaviour::apimart().holding(HeldRequest::Query)).await;
    let observation = observation_pool(&harness).await;
    let account_id = fixture_account(&harness);

    let key = format!("kill-polling-{}", Uuid::new_v4());
    let request = spawn_generation(
        &harness,
        key.clone(),
        route_request(harness.model, "kill while polling the task"),
    );

    harness.gate().wait_for_arrival(1).await;
    let job_id = the_job_id(&observation, account_id).await;
    assert_eq!(job_state(&observation, job_id).await, "executing");
    assert_eq!(
        latest_attempt(&observation, job_id).await.as_deref(),
        Some("accepted")
    );
    assert!(
        handle_of(&observation, job_id).await.is_some(),
        "the handle must be stored before the first poll (A6)"
    );
    assert_eq!(harness.create_calls(), 1);

    harness._api.sigkill();
    request.abort();
    // 放行被杀进程手里那次挂起的查询（它写回一条已死的连接），再等它真的走出闸门：恢复查询
    // 因此拿到上游的下一个状态。
    harness.gate().release_all();
    harness.gate().wait_for_resume(1).await;
    expire_lease(&observation, job_id).await;

    let repository = recovery_repository(&harness).await;
    let service = reconciler(repository.clone());
    let report = service.run_once().await.expect("one reconciliation round");
    assert_eq!(report.taken_over, 1);
    assert_eq!(
        report.settled, 1,
        "the read-only query settles exactly once"
    );

    assert_eq!(job_state(&observation, job_id).await, "succeeded");
    assert_eq!(
        latest_attempt(&observation, job_id).await.as_deref(),
        Some("terminal")
    );
    assert_eq!(captures(&observation, job_id).await, 1);
    assert_eq!(held_microusd(&observation, account_id).await, 0);
    assert_eq!(capacity_state(&observation, job_id).await, "released");
    assert_eq!(open_cases(&observation, job_id).await, 0);
    assert_eq!(
        harness.create_calls(),
        1,
        "recovery queries the original task only; the create request count never grows"
    );
    assert_eq!(
        harness.count("GET", "/v1/tasks/"),
        2,
        "one poll from the API process and one read-only query from recovery"
    );

    drop(service);
    drop(repository);
    observation.close().await;
    harness.cleanup().await;
}

/// **格 5：取得证据后、结算提交前（结算事务卡在 Job 行锁上）**。
///
/// 注入点：假上游先停住第一次任务查询；用例锁住 Job 行后再放行终态。终态带着有效计量进了内存，
/// 结算事务的第一句就是 Job 行 `FOR UPDATE`，因此停在锁上——证据已取得、结算还没提交。
///
/// 恢复：内存里的证据随进程消失，Worker 按原句柄重新只读查询同一任务、幂等结算一次；实收一条、
/// Hold 释放、生成请求计数仍是 1。这格同时覆盖 A5 的"取得证据后"与"结算提交前"。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; SIGKILLs a real API child process (Linux only)"]
async fn sigkill_after_the_evidence_arrives_before_the_settlement_commit_settles_once() {
    let mut harness =
        direct_with_hold(UpstreamBehaviour::apimart().holding(HeldRequest::Query)).await;
    let observation = observation_pool(&harness).await;
    let account_id = fixture_account(&harness);

    let key = format!("kill-before-settle-{}", Uuid::new_v4());
    let request = spawn_generation(
        &harness,
        key.clone(),
        route_request(
            harness.model,
            "kill after the evidence, before the settlement commit",
        ),
    );

    harness.gate().wait_for_arrival(1).await;
    let job_id = the_job_id(&observation, account_id).await;
    // 屏障：先锁住 Job 行，再放行带证据的终态。结算事务提交不了。
    let mut blocker = observation.begin().await.expect("the barrier transaction");
    sqlx::query("SELECT id FROM generation.jobs WHERE id = $1 FOR UPDATE")
        .bind(job_id)
        .fetch_one(&mut *blocker)
        .await
        .expect("hold the job row");
    harness.gate().release_all();
    await_lock_waiter(
        &observation,
        &harness.database_name,
        "the settlement must block on the job row",
    )
    .await;

    assert!(
        handle_of(&observation, job_id).await.is_some(),
        "the handle was stored before polling"
    );
    assert_eq!(
        harness.count("GET", "/v1/tasks/"),
        1,
        "the evidence in memory came from that first poll"
    );
    assert_eq!(
        captures(&observation, job_id).await,
        0,
        "the settlement is not committed yet"
    );

    harness._api.sigkill();
    request.abort();
    assert_eq!(
        terminate_lock_waiters(&observation, &harness.database_name).await,
        1,
        "exactly the killed API backend must still be waiting on the job row"
    );
    blocker.rollback().await.expect("release the barrier");
    expire_lease(&observation, job_id).await;

    let repository = recovery_repository(&harness).await;
    let service = reconciler(repository.clone());
    let report = service.run_once().await.expect("one reconciliation round");
    assert_eq!(report.taken_over, 1);
    assert_eq!(
        report.settled, 1,
        "the read-only query settles exactly once"
    );

    assert_eq!(job_state(&observation, job_id).await, "succeeded");
    assert_eq!(captures(&observation, job_id).await, 1);
    assert_eq!(held_microusd(&observation, account_id).await, 0);
    assert_eq!(capacity_state(&observation, job_id).await, "released");
    assert_eq!(open_cases(&observation, job_id).await, 0);
    assert_eq!(
        harness.create_calls(),
        1,
        "recovery must not send a generation request"
    );
    assert_eq!(
        harness.count("GET", "/v1/tasks/"),
        2,
        "one poll from the API process and one read-only query from recovery"
    );

    drop(service);
    drop(repository);
    observation.close().await;
    harness.cleanup().await;
}

/// **格 6：结算提交后（COMMIT 已落、写回缓存没回来、响应还没交出去）**。
///
/// 注入点：同步渠道（AIHubMix）一次上游调用就带回图片与计量。用例在假上游收到生成请求之后再武
/// 装余额写回闸门，然后放行：结算提交、`refresh_balance` 写回缓存时被停住——此刻账上已经有实收，
/// 客户端还没拿到响应。
///
/// 恢复：重启一个 API 副本（不重发、不重算），同键重发按 §4 回 `409 result_not_retained`；
/// 生成请求计数与实收分录都还是 1。这格是"结算提交后强杀"的进程级取证。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; SIGKILLs a real API child process (Linux only)"]
async fn sigkill_after_the_settlement_commit_replays_as_result_not_retained() {
    let mut harness = Harness::build(
        candidate(
            "AIHubMix",
            "aihubmix-image-v1",
            &["prompt_only", "image_conditioned", "masked"],
        ),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url).holding(HeldRequest::Create),
        4,
        KILL_SYNC_WAIT_SECONDS,
        CaseSettings {
            api: ApiProcessSettings {
                cache: Some(
                    CacheFixture::start(CacheSettings {
                        operation_timeout_ms: 30_000,
                        ..CacheSettings::default()
                    })
                    .await,
                ),
                ..ApiProcessSettings::default()
            },
            ..CaseSettings::default()
        },
    )
    .await;
    let observation = observation_pool(&harness).await;
    let account_id = fixture_account(&harness);

    let key = format!("kill-after-settle-{}", Uuid::new_v4());
    let body = route_request(harness.model, "kill after the settlement commit");
    let request = spawn_generation(&harness, key.clone(), body.clone());

    // 屏障 1：生成请求已经发出（此刻受理时那次余额写回早过去了）。
    harness.gate().wait_for_arrival(1).await;
    // 屏障 2：武装余额写回闸门，再放行上游响应——下一次余额写回必然是结算之后那一次。
    harness.cache().hold_next_balance_write();
    harness.gate().release_all();
    harness.cache().wait_for_balance_write_hold().await;

    let job_id = the_job_id(&observation, account_id).await;
    assert_eq!(
        captures(&observation, job_id).await,
        1,
        "the settlement committed before the process was killed"
    );
    assert_eq!(job_state(&observation, job_id).await, "succeeded");
    assert_eq!(
        held_microusd(&observation, account_id).await,
        0,
        "the hold was released by the committed settlement"
    );

    harness._api.sigkill();
    request.abort();

    // 恢复：另一个 API 副本对着同一个库跑，同键重发只投影原记录。
    let (replica_url, replica) = harness
        .start_replica(4, KILL_SYNC_WAIT_SECONDS, &ApiProcessSettings::default())
        .await;
    let (status, replay) = post_json(
        &replica_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &body,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "got {replay}");
    assert_eq!(
        replay["error"]["code"].as_str(),
        Some("result_not_retained"),
        "a settled request is never replayed and never re-charged"
    );
    assert_eq!(
        captures(&observation, job_id).await,
        1,
        "the replay must not add a second capture"
    );
    assert_eq!(
        harness.create_calls(),
        1,
        "the replay must not send a generation request"
    );

    drop(replica);
    observation.close().await;
    harness.cleanup().await;
}
