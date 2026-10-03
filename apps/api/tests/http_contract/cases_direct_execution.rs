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
