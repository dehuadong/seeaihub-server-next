//! 安全重投：一个 Job 可以有多次上游调用，但**只有在可证明上游没有受理时**才会多出来。
//!
//! 这条纪律的账上含义就是"不会为同一个请求付两次上游成本"：
//!
//! - 状态不确定（超时、5xx、响应读不出）⇒ 上游**可能已经受理并计费** ⇒ 一律不重投，按既有
//!   口径进对账。宁可进对账，也不重投。
//! - 上游明确拒绝受理（参数/凭证类）⇒ 拒绝来自请求本身，重发一次只会得到同一个答复 ⇒ 不重投。
//!
//! 参考图只收公网 URL、由平台原样透传给渠道之后，受理前不再有平台侧的取图动作，
//! "可证明未受理"这一档在当前两条渠道上没有触发点；重投机制保留给任务查询一类可重试的读。
//!
//! 预授权跨 Attempt 保留、只结算一次：重投的不是一笔新业务，重新预授权等于把同一笔钱扣两遍。

use super::*;

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
