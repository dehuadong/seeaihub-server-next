//! 直接同步执行（RFC 0017 §1、§6）的端到端契约。
//!
//! 开关开着时，图片入口在 API 进程内直连假上游，把结果从内存直接回给调用方；这批用例**不启
//! Worker**，所以 200 本身就证明这条路不依赖 Worker 生成队列或结果轮询。
//!
//! 需要真实 PostgreSQL（夹具派生一次性库并跑迁移），因此都是 ignored。

use super::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

/// A3/A7 的受理前慢读：只发一半正文并停住，正文期限到点必须回 408 `request_timeout`，
/// 而且**不建任何执行记录**、不调上游。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn direct_slow_read_is_408_and_creates_no_record() {
    let harness = Harness::start_direct_with(
        candidate(
            "AIHubMix",
            "aihubmix-image-v1",
            &["prompt_only", "image_conditioned", "masked"],
        ),
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        4,
        30,
        ApiProcessSettings {
            direct_execution: true,
            slow_read_timeout_seconds: Some(1),
            ..ApiProcessSettings::default()
        },
    )
    .await;
    let address = harness
        .base_url
        .strip_prefix("http://")
        .expect("the fixture base url")
        .to_owned();
    let key = format!("direct-slow-read-{}", Uuid::new_v4());
    let headers = format!(
        "POST /v1/images/generations HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: 100000\r\nIdempotency-Key: {key}\r\n\r\n",
        harness.api_key
    );
    let mut socket = tokio::net::TcpStream::connect(&address)
        .await
        .expect("connect to the fixture API");
    socket
        .write_all(headers.as_bytes())
        .await
        .expect("write the request headers");
    // 只写一部分正文就停住：剩下的永远不来，读时限必须收口。
    socket
        .write_all(format!("{{\"model\":\"{}\",\"prompt\":\"slow", harness.model).as_bytes())
        .await
        .expect("write a partial body");

    let mut reader = tokio::io::BufReader::new(socket);
    let mut status_line = String::new();
    tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut status_line))
        .await
        .expect("the server answers within the read limit")
        .expect("a status line");
    assert!(
        status_line.starts_with("HTTP/1.1 408"),
        "a pre-acceptance slow read must be 408, got {status_line}"
    );

    let jobs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.jobs WHERE account_id = $1")
            .bind(Uuid::parse_str(&harness.account_id).expect("the fixture account id"))
            .fetch_one(&harness.pool)
            .await
            .expect("the direct job count");
    assert_eq!(jobs, 0, "a slow read before acceptance creates no record");
    assert_eq!(harness.create_calls(), 0, "the provider is never called");
    harness.cleanup().await;
}

/// 直接执行留下的内部记录状态：按夹具账户查这次执行。
///
/// 不走 Harness::job——那是按明文幂等键查旧协议的；直接执行只落幂等摘要（Spec 0005 §2）。
async fn direct_job_state(harness: &Harness) -> String {
    let account_id = Uuid::parse_str(&harness.account_id).expect("the fixture account id");
    sqlx::query_scalar("SELECT state FROM generation.jobs WHERE account_id = $1")
        .bind(account_id)
        .fetch_one(&harness.pool)
        .await
        .expect("the direct execution must leave a job record")
}

/// A1 的最小闭环：开关开着、不启 Worker，JSON 入口经假上游同步返回上游给的 url。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn direct_json_generation_returns_url_without_a_worker() {
    let harness =
        Harness::start_direct_aihubmix(UpstreamBehaviour::aihubmix(SyncImageShape::Url), 4, 30)
            .await;
    let key = format!("direct-url-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &route_request(harness.model, "a synchronously generated fox"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("direct url", &body);
    assert!(
        body["data"][0]["url"].as_str().is_some(),
        "the upstream url must be returned as is, got {body}"
    );
    assert_eq!(harness.create_calls(), 1, "the provider is called once");
    assert_eq!(
        direct_job_state(&harness).await,
        "succeeded",
        "the direct execution settles in the same process"
    );
    harness.cleanup().await;
}

/// 同一条闭环的 b64_json 形态：上游给 base64，平台原样交回。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn direct_json_generation_returns_base64_without_a_worker() {
    let harness =
        Harness::start_direct_aihubmix(UpstreamBehaviour::aihubmix(SyncImageShape::Base64), 4, 30)
            .await;
    let key = format!("direct-b64-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &route_request(harness.model, "a synchronously generated cat"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("direct base64", &body);
    assert_eq!(
        body["data"][0]["b64_json"].as_str(),
        Some(STANDARD.encode(PNG_FIXTURE).as_str()),
        "the upstream base64 must be returned as is"
    );
    assert_eq!(harness.create_calls(), 1, "the provider is called once");
    harness.cleanup().await;
}

/// 同键同指纹重放：已结算成功但结果不保留，409 result_not_retained，不再调上游、不再收费。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn direct_same_key_replay_is_result_not_retained() {
    let harness =
        Harness::start_direct_aihubmix(UpstreamBehaviour::aihubmix(SyncImageShape::Url), 4, 30)
            .await;
    let key = format!("direct-replay-{}", Uuid::new_v4());
    let request = route_request(harness.model, "the same request twice");
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");

    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "got {body}");
    assert_eq!(
        body["error"]["code"].as_str(),
        Some("result_not_retained"),
        "the replay must project the completed, unretained result"
    );
    assert_eq!(
        harness.create_calls(),
        1,
        "the provider is not called again"
    );
    harness.cleanup().await;
}

/// 同键不同指纹：409 idempotency_conflict，不改原请求、不调上游。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn direct_same_key_different_prompt_is_an_idempotency_conflict() {
    let harness =
        Harness::start_direct_aihubmix(UpstreamBehaviour::aihubmix(SyncImageShape::Url), 4, 30)
            .await;
    let key = format!("direct-conflict-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &route_request(harness.model, "the first prompt"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");

    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &route_request(harness.model, "a different prompt"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "got {body}");
    assert_eq!(
        body["error"]["code"].as_str(),
        Some("idempotency_conflict"),
        "a different fingerprint on the same key must conflict"
    );
    assert_eq!(
        harness.create_calls(),
        1,
        "the provider is not called again"
    );
    harness.cleanup().await;
}

/// 认证发生在消费正文之前：无效密钥配一个坏 JSON 体，必须回 401，而不是 JSON 解析的 4xx。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn direct_invalid_key_is_rejected_before_the_body_is_parsed() {
    let harness =
        Harness::start_direct_aihubmix(UpstreamBehaviour::aihubmix(SyncImageShape::Url), 4, 30)
            .await;
    let response = Client::new()
        .post(format!("{}/v1/images/generations", harness.base_url))
        .bearer_auth("sk_seeai_not_a_real_key")
        .header("content-type", "application/json")
        .body("{ this is not json")
        .send()
        .await
        .expect("the request is sent");
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "authentication must run before the body is parsed"
    );
    assert_eq!(
        harness.create_calls(),
        0,
        "an invalid key must not reach the provider"
    );
    harness.cleanup().await;
}

/// 同键执行还在进行：409 request_in_progress，不重复占用、不执行第二次。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn direct_same_key_while_running_is_request_in_progress() {
    let behaviour = UpstreamBehaviour {
        delay_ms: 2_500,
        ..UpstreamBehaviour::aihubmix(SyncImageShape::Url)
    };
    let harness = Harness::start_direct_aihubmix(behaviour, 4, 30).await;
    let key = format!("direct-inflight-{}", Uuid::new_v4());
    let request = route_request(harness.model, "hold this execution open");
    // 第一个请求在后台跑：上游被压住 2.5 秒，这次执行一直停在在飞状态。
    let base = harness.base_url.clone();
    let api_key = harness.api_key.clone();
    let key_for_first = key.clone();
    let request_for_first = request.clone();
    let first = tokio::spawn(async move {
        post_json(
            &base,
            &api_key,
            "/v1/images/generations",
            &key_for_first,
            &request_for_first,
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "got {body}");
    assert_eq!(
        body["error"]["code"].as_str(),
        Some("request_in_progress"),
        "the same key must not start a second execution"
    );
    let (status, body) = first.await.expect("the first request joins");
    assert_eq!(
        status,
        StatusCode::OK,
        "the first request still succeeds: {body}"
    );
    assert_eq!(harness.create_calls(), 1, "the provider is called once");
    harness.cleanup().await;
}

/// 本库**已提交事务**计数：直接执行期间的 SQL 增量按它观测。
async fn committed_transactions(pool: &sqlx::PgPool) -> i64 {
    let count: i64 = sqlx::query_scalar(
        "SELECT xact_commit FROM pg_stat_database WHERE datname = current_database()",
    )
    .fetch_one(pool)
    .await
    .expect("read the database transaction counter");
    count
}

/// 等计数器**稳定**后再读它。
///
/// `pg_stat_database` 的计数按约 1s 的粒度成批可见：只前后各读一次，批次边界上与本请求无关的
/// 整批提交（夹具建库、发布与账户夹具）会被算进请求增量。这里读到连续两次读数相同为止
/// （间隔大于一档统计粒度），读到的是真正落定的提交数。
async fn settled_committed_transactions(pool: &sqlx::PgPool) -> i64 {
    loop {
        let before = committed_transactions(pool).await;
        tokio::time::sleep(Duration::from_millis(1_200)).await;
        let after = committed_transactions(pool).await;
        if before == after {
            return after;
        }
    }
}

/// 先走通一次直接执行：选路与余额缓存是**首请求**冷、后续热，热了的请求才和长延迟那次可比。
async fn warm_up_direct(harness: &Harness) {
    let key = format!("direct-warmup-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &route_request(harness.model, "warm the route and balance caches"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the warm-up got {body}");
}

/// 发一次成功的直接执行，返回这次请求**落定**的本库已提交事务数之差。
///
/// 调用前必须先 [`warm_up_direct`]：`create_calls == 2` 就是预热那一次加上测量这一次。
async fn one_direct_request_commits(harness: &Harness) -> i64 {
    let key = format!("direct-sql-{}", Uuid::new_v4());
    let before = settled_committed_transactions(&harness.pool).await;
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &route_request(harness.model, "count the committed transactions"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("direct sql counting", &body);
    assert_eq!(harness.create_calls(), 2, "预热与测量各调一次上游");
    let after = settled_committed_transactions(&harness.pool).await;
    after - before
}

/// 轮询到假上游已经收到至少 `expected` 个生成请求。
async fn wait_for_create_calls(harness: &Harness, expected: usize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while harness.create_calls() < expected {
        assert!(
            tokio::time::Instant::now() < deadline,
            "只有 {} 个生成请求到达假上游，期望至少 {expected} 个：请求没有进入 Provider 等待",
            harness.create_calls()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A3/A8 与 RFC 0017 §8 的结构性结论：SQL 次数不随 Provider 等待时长增长。
///
/// 旧路径按 250ms 一次查库等结果，等 6s 会比等 0.1s 多出约 24 次已提交事务；直接执行停在
/// Provider 上时不查库。两次请求都先预热缓存、并在计数落定后各读一次，事务增量应当相同。
/// 上界 10 留给实测抖动：本机开着 Redis 时，两个夹具进程的缓存写入与统计粒度最多让增量差出
/// 约 8 次；要挡的每 250ms 一次查询是 24 次，不落在余量里。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn direct_sql_count_does_not_grow_with_provider_wait() {
    let short = Harness::start_direct_with(
        candidate(
            "AIHubMix",
            "aihubmix-image-v1",
            &["prompt_only", "image_conditioned", "masked"],
        ),
        UpstreamBehaviour {
            delay_ms: 100,
            ..UpstreamBehaviour::aihubmix(SyncImageShape::Url)
        },
        4,
        30,
        ApiProcessSettings {
            direct_execution: true,
            ..ApiProcessSettings::default()
        },
    )
    .await;
    warm_up_direct(&short).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let short_delta = one_direct_request_commits(&short).await;
    short.cleanup().await;

    let long = Harness::start_direct_with(
        candidate(
            "AIHubMix",
            "aihubmix-image-v1",
            &["prompt_only", "image_conditioned", "masked"],
        ),
        UpstreamBehaviour {
            delay_ms: 6_000,
            ..UpstreamBehaviour::aihubmix(SyncImageShape::Url)
        },
        4,
        30,
        ApiProcessSettings {
            direct_execution: true,
            ..ApiProcessSettings::default()
        },
    )
    .await;
    warm_up_direct(&long).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let long_delta = one_direct_request_commits(&long).await;
    long.cleanup().await;

    const JITTER_ALLOWANCE: i64 = 10;
    assert!(
        long_delta <= short_delta + JITTER_ALLOWANCE,
        "等待 6s 的请求比等待 0.1s 的请求多提交了 {} 次事务（短 {short_delta}、长 {long_delta}，允许的多余量 {JITTER_ALLOWANCE}）：SQL 次数在随 Provider 等待时长增长",
        long_delta - short_delta
    );
}

/// A3 与 RFC 0017 §6/§8：慢 Provider 期间不持有数据库连接，池之外的连接仍能及时完成 `SELECT 1`。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn direct_slow_provider_does_not_occupy_a_database_connection() {
    let harness = Harness::start_direct_with(
        candidate(
            "AIHubMix",
            "aihubmix-image-v1",
            &["prompt_only", "image_conditioned", "masked"],
        ),
        UpstreamBehaviour {
            delay_ms: 1_500,
            ..UpstreamBehaviour::aihubmix(SyncImageShape::Url)
        },
        4,
        30,
        ApiProcessSettings {
            direct_execution: true,
            ..ApiProcessSettings::default()
        },
    )
    .await;
    let base = harness.base_url.clone();
    let api_key = harness.api_key.clone();
    let key = format!("direct-idle-{}", Uuid::new_v4());
    let request = route_request(harness.model, "a slow provider call");
    let inflight = tokio::spawn(async move {
        post_json(&base, &api_key, "/v1/images/generations", &key, &request).await
    });
    // 假上游已经收到生成请求：这次请求正停在 Provider 等待里。
    wait_for_create_calls(&harness, 1).await;

    // 池之外的**新**连接：等待期间数据库仍能服务新会话。
    let observer = sqlx::PgPool::connect(&harness.database_url)
        .await
        .expect("a fresh pool connects while the provider is slow");
    let started = tokio::time::Instant::now();
    let probe = tokio::time::timeout(
        Duration::from_secs(2),
        sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&observer),
    )
    .await;
    let elapsed = started.elapsed();
    assert!(
        probe.is_ok(),
        "Provider 等待期间，池外的新连接没能在 2s 内完成 SELECT 1（实际等待 {elapsed:?}）"
    );
    assert_eq!(
        probe
            .expect("the probe answered in time")
            .expect("SELECT 1"),
        1
    );
    observer.close().await;

    let (status, body) = inflight.await.expect("the request joins");
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(harness.create_calls(), 1, "the provider is called once");
    harness.cleanup().await;
}

/// A8：并发数可以超过 API 自身的连接池上限，说明等待 Provider 时不占连接。
///
/// API 进程的连接池上限固定是 10（见 `PgHubRepository::connect(&database_url, 10)`）。这里同时发
/// 16 个请求并等它们**都**到达假上游；如果有连接跨 Provider 等待被独占，最多只有 10 个能同时等，
/// 第 11 个起会卡在池上。再用 `/health` 的 `SELECT 1` 确认等待期间池仍可服务新请求。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn direct_slow_provider_keeps_more_requests_than_pool_connections_in_flight() {
    const CONCURRENT: usize = 16; // 大于 API 连接池上限 10
    let harness = Harness::start_direct_with(
        candidate(
            "AIHubMix",
            "aihubmix-image-v1",
            &["prompt_only", "image_conditioned", "masked"],
        ),
        UpstreamBehaviour {
            delay_ms: 6_000,
            ..UpstreamBehaviour::aihubmix(SyncImageShape::Url)
        },
        32,
        30,
        ApiProcessSettings {
            direct_execution: true,
            ..ApiProcessSettings::default()
        },
    )
    .await;
    let mut inflight = Vec::with_capacity(CONCURRENT);
    for index in 0..CONCURRENT {
        let base = harness.base_url.clone();
        let api_key = harness.api_key.clone();
        let key = format!("direct-pool-{index}-{}", Uuid::new_v4());
        let request = route_request(harness.model, "hold the provider wait open");
        inflight.push(tokio::spawn(async move {
            post_json(&base, &api_key, "/v1/images/generations", &key, &request).await
        }));
    }
    wait_for_create_calls(&harness, CONCURRENT).await;
    let finished = inflight
        .iter()
        .filter(|handle| handle.is_finished())
        .count();
    assert_eq!(
        finished, 0,
        "观察时不应有请求已经结束，否则「并发数超过池上限」不成立"
    );

    let health = Client::new()
        .get(format!("{}/health", harness.base_url))
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .expect("the pool serves the health probe while providers wait");
    assert_eq!(
        health.status(),
        StatusCode::OK,
        "慢 Provider 等待期间 /health 必须能从空闲池拿到连接"
    );

    for handle in inflight {
        let (status, body) = handle.await.expect("a concurrent request joins");
        assert_eq!(status, StatusCode::OK, "got {body}");
    }
    assert_eq!(harness.create_calls(), CONCURRENT, "每个请求只调一次上游");
    harness.cleanup().await;
}
