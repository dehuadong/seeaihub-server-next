//! 安全重投：一个 Job 可以有多次上游调用，但**只有在可证明上游没有受理时**才会多出来。
//!
//! 这条纪律的账上含义就是"不会为同一个请求付两次上游成本"：
//!
//! - 可证明未受理（参考图取不到、上游明确拒绝受理）⇒ 上游没开始计费 ⇒ 按指数退避
//!   重投，直到成功或用完额度；
//! - 状态不确定（超时、5xx、响应读不出）⇒ 上游**可能已经受理并计费** ⇒ 一律不重投，按既有
//!   口径进对账。宁可进对账，也不重投。
//!
//! 预授权跨 Attempt 保留、只结算一次：重投的不是一笔新业务，重新预授权等于把同一笔钱扣两遍。

use super::*;

/// 可证明未受理的失败重投一次之后成功：Job 终态 `succeeded`，两行 Attempt 各有自己的号。
///
/// 这条用例同时钉住"预授权只扣过一次"：受理时扣的那一笔预授权在重投期间原样保留，最后只在
/// 成功那一次结算（`ledger.holds` 从 `active` 直接变 `captured`，中间没有 `released` 再扣一遍）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_provably_unaccepted_failure_is_retried_and_the_job_settles_once() {
    // 参考图取用先失败一次再恢复：AIHubMix 的取图发生在生成请求提交**之前**，所以这一次失败是
    // **可证明未受理**（上游没开始计费），重投不会付两次。用"恢复"而不是"一直失败"，是为了看到
    // 第二次执行真的成功——那正是重投要换来的结果。
    let behaviour = UpstreamBehaviour {
        reference_get_failures: 1,
        ..UpstreamBehaviour::aihubmix(SyncImageShape::Url)
    };
    let harness = Harness::start_with_retry(
        candidate(
            "AIHubMix",
            "aihubmix-image-v1",
            &["prompt_only", "image_conditioned"],
        ),
        behaviour,
        64,
        RetrySettings {
            max_attempts: 3,
            backoff_base_ms: 20,
        },
    )
    .await;
    let key = format!("retry-then-succeed-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "retry me");
    request["image"] = json!(harness.png_url());
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request)
        .await;
    assert_eq!(status, StatusCode::OK, "第二次成功之后对客是成功：{body}");
    assert_sync_success("重投之后成功", &body);

    let (job_id, state) = harness.job(&key).await;
    assert_eq!(state, "succeeded", "重投成功之后 Job 落成功终态");

    // 参考图被取了两次：第一次失败（生成请求没发出去）、第二次成功。
    assert_eq!(
        harness.count("GET", "/inputs/ref.png"),
        2,
        "两次执行各取了一次参考图"
    );
    assert_eq!(
        harness.create_calls(),
        1,
        "第一次执行连生成请求都没发出去（参考图就取失败了），重投那次才发"
    );

    // 两次执行各占一行，号按执行顺序从 1 起。
    let attempts: Vec<(i32, String)> = sqlx::query_as(
        "SELECT attempt_no, state FROM generation.attempts WHERE job_id = $1 ORDER BY attempt_no",
    )
    .bind(job_id)
    .fetch_all(&harness.pool)
    .await
    .expect("attempt rows");
    assert_eq!(
        attempts,
        vec![(1, "terminal".to_owned()), (2, "terminal".to_owned())],
        "一次重投留下两行，两行各自收尾（v1 的 Attempt 终态名是 terminal）"
    );

    // 结算只发生一次：capture 分录只有一条。
    let captures: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ledger.entries WHERE job_id = $1 AND kind = 'capture'",
    )
    .bind(job_id)
    .fetch_one(&harness.pool)
    .await
    .expect("capture entries");
    assert_eq!(captures, 1, "一次成功只结算一次");

    // 预授权跨 Attempt 保留：两次执行期间它一直是同一个 hold，没有释放再扣一遍。
    let holds: Vec<(String, i64)> = sqlx::query_as(
        "SELECT status, amount_microusd FROM ledger.holds WHERE job_id = $1 ORDER BY created_at",
    )
    .bind(job_id)
    .fetch_all(&harness.pool)
    .await
    .expect("holds");
    assert_eq!(
        holds.len(),
        1,
        "重投不重新预授权：这台 Job 自始至终只有一个 hold"
    );
    assert_eq!(holds[0].0, "captured", "结算之后这个 hold 是 captured");

    // 资金流水里**没有**释放分录：预授权只留在 `ledger.holds`（上面已断言它是 `captured`），
    // 结算只留一条实收（`0002` §3）。重投不重复扣、也不重复结清。
    let releases: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ledger.entries WHERE job_id = $1 AND kind = 'release'",
    )
    .bind(job_id)
    .fetch_one(&harness.pool)
    .await
    .expect("release entries");
    assert_eq!(releases, 0, "预授权不进资金流水，只留在 holds");

    harness.cleanup().await;
}

/// **状态不确定绝不重投**：上游回 5xx 时只有一行 Attempt、没有第二次上游调用。
///
/// 这是"不付两次"的直接证据：5xx 无法证明上游没有受理，重投就可能为同一个请求付两次上游成本，
/// 所以按既有口径进对账，把这件事交给人。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_uncertain_failure_is_never_retried() {
    // 生成请求与任务查询一直 500：上游可能已经受理并计费，状态不确定。
    let behaviour = UpstreamBehaviour {
        query_failures: 99,
        ..UpstreamBehaviour::apimart()
    };
    let harness = Harness::start_with_retry(
        candidate("APIMart", "apimart-image-v1", &["prompt_only"]),
        behaviour,
        64,
        // 上限给到 5：就算额度很宽，不确定的失败也必须一次都不重投。
        RetrySettings {
            max_attempts: 5,
            backoff_base_ms: 20,
        },
    )
    .await;
    let key = format!("no-retry-uncertain-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "uncertain"),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "got {body}");
    assert_eq!(body["error"]["code"].as_str(), Some("outcome_unknown"));
    assert_public_only("受理状态不明", &body);

    let (job_id, state) = harness.job(&key).await;
    assert_eq!(
        state, "reconciliation_required",
        "状态不确定按既有口径进对账，不重投"
    );

    // **不付两次的直接证据**：假上游只收到过一次生成请求。
    assert_eq!(
        harness.create_calls(),
        1,
        "状态不确定时上游只能被调一次：多一次就可能多付一笔上游成本"
    );
    let attempts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.attempts WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("attempt rows");
    assert_eq!(attempts, 1, "不确定的失败只有一行执行记录");
    let attempt_no: i32 =
        sqlx::query_scalar("SELECT attempt_no FROM generation.attempts WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("attempt number");
    assert_eq!(attempt_no, 1, "第一次执行就是唯一一次");

    // 进对账意味着预授权被**保留**（等人工处置），不是释放。
    let hold_status: String =
        sqlx::query_scalar("SELECT status FROM ledger.holds WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("the job must have a hold");
    assert_eq!(hold_status, "active", "进对账的 Job 保留预授权，等人工处置");

    harness.cleanup().await;
}

/// 上游**确定性地拒绝受理**（参数/凭证类）时也不重投：同一份请求再发一次只会得到同一个答复。
///
/// 这一档与"可证明未受理"的区别在**重投有没有意义**：拒绝来自请求本身，不来自上游一时的状态。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_deterministic_rejection_is_not_retried() {
    // APIMart 的 `error.code = 400` 是参数类确定性拒绝（`RetrySafety::NotRetryable`）。
    let behaviour = UpstreamBehaviour {
        create_rejection_status: 400,
        create_rejection_times: 0,
        ..UpstreamBehaviour::apimart()
    };
    let harness = Harness::start_with_retry(
        candidate("APIMart", "apimart-image-v1", &["prompt_only"]),
        behaviour,
        64,
        RetrySettings {
            max_attempts: 3,
            backoff_base_ms: 20,
        },
    )
    .await;
    let key = format!("no-retry-rejected-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "rejected for good"),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "got {body}");
    assert_eq!(body["error"]["code"].as_str(), Some("platform_unavailable"));

    let (job_id, state) = harness.job(&key).await;
    assert_eq!(state, "failed", "确定性拒绝按既有失败处置落失败终态");
    assert_eq!(
        harness.create_calls(),
        1,
        "参数/凭证类拒绝重投同一份请求不会有别的结果，因此一次都不重投"
    );
    let attempts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.attempts WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("attempt rows");
    assert_eq!(attempts, 1);
    let hold_status: String =
        sqlx::query_scalar("SELECT status FROM ledger.holds WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("the job must have a hold");
    assert_eq!(hold_status, "released", "失败终态释放预授权");

    harness.cleanup().await;
}

/// 用尽额度仍失败：上游被调用次数**正好等于上限**，然后按既有失败处置。
///
/// 上限是运维配置项，所以"停在几次"必须由配置决定，不能由别的东西（比如上游恰好恢复）决定。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn retries_stop_at_the_configured_limit() {
    // 参考图一直取不到：这是**可证明未受理**（生成任务此时还没提交），所以每次执行都会走到
    // 重投判据上；额度用完时就停在失败终态。
    let harness = Harness::start_with_retry(
        candidate(
            "AIHubMix",
            "aihubmix-image-v1",
            &["prompt_only", "image_conditioned"],
        ),
        // 假上游对参考图的 GET 一律回 500：每次执行都在取图那一步失败。
        UpstreamBehaviour {
            reference_get_failures: usize::MAX,
            ..UpstreamBehaviour::aihubmix(SyncImageShape::Url)
        },
        64,
        RetrySettings {
            max_attempts: 2,
            backoff_base_ms: 20,
        },
    )
    .await;
    let key = format!("retry-until-limit-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "always fails");
    request["image"] = json!(harness.png_url());
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request)
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "got {body}");

    let (job_id, state) = harness.job(&key).await;
    assert_eq!(
        state, "failed",
        "用尽额度之后按既有失败处置：不新增对账态语义"
    );

    // 上限是 2：执行两次，每次都试过取参考图（取图失败时生成请求根本不会发出去）。
    let attempts: Vec<i32> = sqlx::query_scalar(
        "SELECT attempt_no FROM generation.attempts WHERE job_id = $1 ORDER BY attempt_no",
    )
    .bind(job_id)
    .fetch_all(&harness.pool)
    .await
    .expect("attempt rows");
    assert_eq!(attempts, vec![1, 2], "重投停在上限次数上，不多不少");
    assert_eq!(
        harness.create_calls(),
        0,
        "参考图取不到意味着生成请求根本没发出去"
    );
    let hold_status: String =
        sqlx::query_scalar("SELECT status FROM ledger.holds WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("the job must have a hold");
    assert_eq!(
        hold_status, "released",
        "用尽额度这条路径与今天逐位相同：释放预授权"
    );

    harness.cleanup().await;
}

/// 回归：成功路径只有一行 Attempt，与今天逐位相同。
///
/// 关掉重投（上限 1）之后再跑一次成功请求：内部执行记录必须与今天完全一样——一行 Attempt、
/// 号是 1、持有额从 active 直接到 captured。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_plain_success_still_leaves_exactly_one_attempt() {
    let harness = Harness::start_with_retry(
        candidate("APIMart", "apimart-image-v1", &["prompt_only"]),
        UpstreamBehaviour::apimart(),
        64,
        RetrySettings::disabled(),
    )
    .await;
    let key = format!("single-attempt-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "one shot"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("成功路径", &body);

    let (job_id, state) = harness.job(&key).await;
    assert_eq!(state, "succeeded");
    let attempts: Vec<(i32, String)> =
        sqlx::query_as("SELECT attempt_no, state FROM generation.attempts WHERE job_id = $1")
            .bind(job_id)
            .fetch_all(&harness.pool)
            .await
            .expect("attempt rows");
    assert_eq!(
        attempts,
        vec![(1, "terminal".to_owned())],
        "成功路径只留一行执行记录，号从 1 起"
    );
    assert_eq!(harness.create_calls(), 1, "成功路径只调一次上游");
    let hold_status: String =
        sqlx::query_scalar("SELECT status FROM ledger.holds WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("the job must have a hold");
    assert_eq!(hold_status, "captured");

    harness.cleanup().await;
}
