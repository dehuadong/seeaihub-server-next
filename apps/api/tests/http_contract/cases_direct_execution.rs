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

/// 用同一个网关模型发一份**新修订**：现行合同多要一个必填参数 `style`。
///
/// 合同行按 (厂商, 型号, 修订) 不可变，换合同必须换修订号；发布即原子替换这个型号的 active
/// 候选，之后的受理按新合同走，而已经受理的 Job 仍指向它自己那一行旧合同。
async fn republish_requiring_style(harness: &Harness) {
    let mut draft = candidate(
        "AIHubMix",
        "aihubmix-image-v1",
        &["prompt_only", "image_conditioned", "masked"],
    );
    draft["base_url"] = Value::String(harness.upstream_base_url.clone());
    // 只动**合同**：`style` 是调用方那一侧的面，承载面不必声明它（这条用例也不会真的提交它）。
    let mut contract = draft["capability_schema"].clone();
    contract["properties"]["style"] = json!({"type": "string"});
    contract["required"] = json!(["model", "prompt", "style"]);
    assert_eq!(
        publish_on_revision(
            harness,
            harness.model,
            "route-test-2",
            contract,
            vec![draft],
            None,
        )
        .await,
        StatusCode::OK,
        "第二次发布必须成功，否则这条用例证明不了现行合同确实换了"
    );
}

/// 该账户落下的执行记录数：被拒的请求不该留下记录，判据取账户维度。
async fn account_job_count(harness: &Harness) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM generation.jobs WHERE account_id = $1")
        .bind(Uuid::parse_str(&harness.account_id).expect("the fixture account id"))
        .fetch_one(&harness.pool)
        .await
        .expect("the direct job count")
}

/// RFC 0018 §9.1 与 Spec 0005 §4：合同换了修订之后，同键重发仍按**记录冻结的合同**比对并投影
/// 原事实；同一个正文换一把新键才是"未命中"，按**当前**合同解释。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn direct_replay_uses_the_recorded_contract_after_a_republish() {
    let harness =
        Harness::start_direct_aihubmix(UpstreamBehaviour::aihubmix(SyncImageShape::Url), 4, 30)
            .await;
    let key = format!("direct-recorded-contract-{}", Uuid::new_v4());
    let request = route_request(harness.model, "the same body across two revisions");
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(harness.create_calls(), 1, "第一次请求调一次上游");

    republish_requiring_style(&harness).await;

    // 同键同正文：命中记录，用记录冻结的合同（没有 `style` 这一条）比对，投影"已完成、结果不保留"。
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
        "同键重发必须按记录规则投影，不拿新修订重新解释原请求：{body}"
    );
    assert_eq!(harness.create_calls(), 1, "重放不再调上游");

    // 同一正文换一把新键：未命中记录，按当前合同解释 → 缺必填 `style`。
    let fresh_key = format!("direct-current-contract-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &fresh_key,
        &request,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "未命中的请求要按当前合同校验：{body}"
    );
    assert_eq!(
        body["error"]["code"].as_str(),
        Some("validation_error"),
        "{body}"
    );
    assert_eq!(harness.create_calls(), 1, "被当前合同拒绝的请求不调上游");
    assert_eq!(
        account_job_count(&harness).await,
        1,
        "被当前合同拒绝的请求不建执行记录"
    );
    harness.cleanup().await;
}

/// RFC 0018 §9.1 与 Spec 0005 §4：记录缺比较材料（记录还在、请求指纹不在）时按
/// 409 `idempotency_conflict` 拒绝，**不当作未命中**去执行新请求。
///
/// `request_digest` 在库层可空（迁移 0038 只把幂等摘要收成 NOT NULL），所以"记录在、材料不在"
/// 这种形态用 SQL 就造得出来；修好之前这里会一路走到 500，而不是 409。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn direct_replay_without_comparison_material_is_a_conflict() {
    let harness =
        Harness::start_direct_aihubmix(UpstreamBehaviour::aihubmix(SyncImageShape::Url), 4, 30)
            .await;
    let key = format!("direct-no-material-{}", Uuid::new_v4());
    let request = route_request(harness.model, "drop the recorded fingerprint");
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");

    let cleared = sqlx::query(
        "UPDATE generation.jobs SET request_digest = NULL WHERE idempotency_key_digest = $1",
    )
    .bind(idempotency_key_digest(&key))
    .execute(&harness.pool)
    .await
    .expect("clear the recorded request fingerprint");
    assert_eq!(
        cleared.rows_affected(),
        1,
        "必须真的抹掉了一行，否则下面的断言没有前提"
    );

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
        Some("idempotency_conflict"),
        "缺比较材料必须按冲突拒绝，不能当作未命中：{body}"
    );
    assert_eq!(harness.create_calls(), 1, "不执行第二次生成");
    assert_eq!(account_job_count(&harness).await, 1, "不新建执行记录");
    harness.cleanup().await;
}

/// RFC 0018 §9.1：按幂等键查记录发生在按当前合同的图片字段抽取与型号判定**之前**。
///
/// 同键正文里放一个既不是公网 URL 也不是 data URL 的图片值：入口若先按当前规则解释，这里是
/// 400 `invalid_parameter`；先查记录则命中，用记录规则比对不上，按 409 `idempotency_conflict` 拒绝。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn direct_replay_is_checked_before_the_current_contract_interprets_the_body() {
    let harness =
        Harness::start_direct_aihubmix(UpstreamBehaviour::aihubmix(SyncImageShape::Url), 4, 30)
            .await;
    let key = format!("direct-lookup-first-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &route_request(harness.model, "a recorded request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");

    let mut replay = route_request(harness.model, "a recorded request");
    replay["image"] = json!("neither a public url nor a data url");
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &replay,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "先查记录就不该落到当前合同的图片校验：{body}"
    );
    assert_eq!(
        body["error"]["code"].as_str(),
        Some("idempotency_conflict"),
        "{body}"
    );
    assert_eq!(harness.create_calls(), 1, "不执行第二次生成");
    assert_eq!(account_job_count(&harness).await, 1, "不新建执行记录");
    harness.cleanup().await;
}

/// RFC 0018 §9.1：带参考图的同键重发也按记录规则比对——记录比对从**原始参数面**里摘图片字段，
/// 指纹里的图片取值与受理时逐字相同。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn direct_replay_with_a_reference_image_uses_the_recorded_fingerprint() {
    let harness =
        Harness::start_direct_aihubmix(UpstreamBehaviour::aihubmix(SyncImageShape::Url), 4, 30)
            .await;
    let key = format!("direct-image-replay-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "edit with a public url twice");
    request["image"] = json!(harness.png_url());
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
        "带图的同键重发必须命中同一条记录：{body}"
    );
    assert_eq!(harness.create_calls(), 1, "重放不再调上游");
    assert_eq!(account_job_count(&harness).await, 1, "不新建执行记录");
    harness.cleanup().await;
}

/// Spec 0005 A12：以 `data:` URL 受理的历史记录在改版后仍按原记录回应。
///
/// 生成入口现在只收公网 URL，这样的历史记录已经造不出来：先用公网 URL 走一遍，留下记录与冻结的
/// 合同；再把记录里的请求摘要换成"同一份请求、图片取值是 data URL"的摘要——那正是历史记录当时写下
/// 的那份材料。重发同一幂等键的 data URL 请求必须走重放（`409 result_not_retained`），而不是按
/// 新入口规则回 `400 public_image_url_required`。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_historical_data_url_record_replays_by_the_recorded_rules() {
    let harness =
        Harness::start_direct_aihubmix(UpstreamBehaviour::aihubmix(SyncImageShape::Url), 4, 30)
            .await;
    let key = format!("direct-data-url-replay-{}", Uuid::new_v4());
    let prompt = "edit with a historically recorded inline image";
    let data_url = inline_png_data_url();
    let mut request = route_request(harness.model, prompt);
    request["image"] = json!(harness.png_url());
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");

    // 历史记录写下的摘要：同一份请求面（模型与普通参数）配 data URL 的图片取值。
    let digest = contract_fingerprint_keys()
        .request_fingerprint(
            1,
            &RequestFingerprintInput {
                endpoint: "/v1/images/generations",
                gateway_model: harness.model,
                parameters: &json!({"model": harness.model, "prompt": prompt}),
                reference_images: std::slice::from_ref(&data_url),
                mask: None,
                n: 1,
            },
        )
        .expect("the fixture fingerprint computes")
        .expect("key version 1 is configured");
    let updated = sqlx::query(
        "UPDATE generation.jobs SET request_digest = $1 WHERE idempotency_key_digest = $2",
    )
    .bind(&digest)
    .bind(idempotency_key_digest(&key))
    .execute(&harness.pool)
    .await
    .expect("rewrite the recorded request digest");
    assert_eq!(updated.rows_affected(), 1, "必须真的改到了一行");

    // 用 data URL 重发同一幂等键：命中记录并按原记录回应，不被新入口规则拦下。
    let mut request = route_request(harness.model, prompt);
    request["image"] = json!(data_url);
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
        "历史 data URL 记录必须按原记录回应：{body}"
    );
    assert_eq!(harness.create_calls(), 1, "重放不再调上游");
    assert_eq!(account_job_count(&harness).await, 1, "不新建执行记录");
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

/// 等计数器稳定的上限：到点用最后一次读数，不让等待无限拉长。
const SETTLE_DEADLINE: Duration = Duration::from_secs(10);

/// **目标库**的已提交事务计数：直接执行期间的 SQL 增量按它观测。
///
/// 从基库的池上读、按库名过滤：读计数本身也是一次提交。若从被观测的那个库上读，观测者自己
/// 的读会被算进去，负载高、统计刷新勤时前后两次读数永远差 1，等待稳定就成了死循环。
async fn committed_transactions(admin: &sqlx::PgPool, database: &str) -> i64 {
    let count: i64 =
        sqlx::query_scalar("SELECT xact_commit FROM pg_stat_database WHERE datname = $1")
            .bind(database)
            .fetch_one(admin)
            .await
            .expect("read the database transaction counter");
    count
}

/// 等计数器**稳定**后再读它，最多等到 `SETTLE_DEADLINE`。
///
/// `pg_stat_database` 的计数按约 1s 的粒度成批可见：只前后各读一次，批次边界上与本请求无关的
/// 整批提交（夹具建库、发布与账户夹具）会被算进请求增量。这里读到连续两次读数相同为止。
/// 上限只作兜底：正常情况下读数在基库上不再自增，很快稳定。
async fn settled_committed_transactions(admin: &sqlx::PgPool, database: &str) -> i64 {
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    let mut last = committed_transactions(admin, database).await;
    loop {
        tokio::time::sleep(Duration::from_millis(1_200)).await;
        let after = committed_transactions(admin, database).await;
        if last == after || tokio::time::Instant::now() >= deadline {
            return after;
        }
        last = after;
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
    let before = settled_committed_transactions(&harness.admin_pool, &harness.database_name).await;
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
    let after = settled_committed_transactions(&harness.admin_pool, &harness.database_name).await;
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
            max_memory_bytes: Some(one_execution_reservation_bytes() * CONCURRENT),
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

/// A8 与 Spec 0005 §3：本机字节预算被在飞执行占满时，新请求按容量不足拒（503
/// platform_unavailable），不建执行记录、不调上游；预算释放后同样的请求正常工作。
///
/// 为什么用并发构造"超过预算"：单次执行的预留是按 Driver 字节上限实测钉出来的（见
/// `GatewayByteLimits::max_bytes_per_execution`），预算配到比它小进程直接拒绝启动（启动组合
/// 校验），所以单个请求永远在预算内；能压出来的边界只有"又一个执行要预留，而预算已被在飞执行
/// 占满"。字节预算被压到刚好一次执行时，执行名额按预算推导出来也是 1（见 `apps/api/src/main.rs`），
/// 因此这次 503 是"本机执行容量不足"，不会与读/发送名额超限混淆。
/// 单次执行的内存预留：与网关按 Driver 字节上限算出的口径一致（见 `apps/api/src/main.rs`）。
fn one_execution_reservation_bytes() -> usize {
    let response = seeai_adapter_aihubmix::MAX_PROVIDER_RESPONSE_BYTES
        .max(seeai_adapter_apimart::MAX_PROVIDER_RESPONSE_BYTES);
    seeai_adapter_sdk::GatewayByteLimits {
        request_wire_bytes: seeai_adapter_sdk::GATEWAY_REQUEST_WIRE_BYTES,
        provider_response_bytes: response,
    }
    // 夹具不配 transport 的两个缓冲上限，进程用 API 的缺省值（HTTP/1 解析缓冲 64 KiB +
    // HTTP/2 发送缓冲 1 MiB）。这两个缺省写在 `apps/api/src/main.rs`，测试二进制读不到，只能同值。
    .max_bytes_per_execution(1024 * 1024 + 64 * 1024)
}

#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn direct_memory_budget_rejects_a_second_concurrent_execution() {
    // 预算刚好够一次执行：网关按 Driver 字节上限算出的那份预留。
    let harness = Harness::start_direct_with(
        candidate(
            "AIHubMix",
            "aihubmix-image-v1",
            &["prompt_only", "image_conditioned", "masked"],
        ),
        UpstreamBehaviour {
            delay_ms: 2_500,
            ..UpstreamBehaviour::aihubmix(SyncImageShape::Url)
        },
        64,
        30,
        ApiProcessSettings {
            max_memory_bytes: Some(one_execution_reservation_bytes()),
            ..ApiProcessSettings::default()
        },
    )
    .await;
    let base = harness.base_url.clone();
    let api_key = harness.api_key.clone();
    let first_key = format!("direct-budget-first-{}", Uuid::new_v4());
    let first_request = route_request(harness.model, "hold the whole memory budget open");
    let first = tokio::spawn(async move {
        post_json(
            &base,
            &api_key,
            "/v1/images/generations",
            &first_key,
            &first_request,
        )
        .await
    });
    // 假上游已经收到第一个请求：它此刻持有那次执行的字节预留。
    wait_for_create_calls(&harness, 1).await;

    let second_key = format!("direct-budget-second-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &second_key,
        &route_request(harness.model, "must be rejected by the byte budget"),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "got {body}");
    assert_eq!(
        body["error"]["code"].as_str(),
        Some("platform_unavailable"),
        "预算占满时必须按本机容量不足投影，got {body}"
    );
    assert_eq!(harness.create_calls(), 1, "被拒请求不能调上游");

    let (status, body) = first.await.expect("the first request joins");
    assert_eq!(status, StatusCode::OK, "got {body}");
    // 客户端已经读完整段响应，服务端的响应 body 随之销毁、字节预留回到预算池；这个短等的只有
    // "body 被丢掉"这一跳，不用它掩盖任何判定。
    tokio::time::sleep(Duration::from_millis(200)).await;

    let third_key = format!("direct-budget-third-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &third_key,
        &route_request(harness.model, "the budget is free again"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(harness.create_calls(), 2, "预算内的请求各调一次上游");

    // 被拒那次没有留下任何执行记录：本账户只有受理成功的那两条。
    let jobs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.jobs WHERE account_id = $1")
            .bind(Uuid::parse_str(&harness.account_id).expect("the fixture account id"))
            .fetch_one(&harness.pool)
            .await
            .expect("the direct job count");
    assert_eq!(jobs, 2, "被预算拒绝的请求不建执行记录");
    harness.cleanup().await;
}

/// 读 /proc/<pid>/status 里的峰值常驻内存 VmHWM（KiB）。
///
/// 取整段生命周期的最高水位而不是当前 VmRSS：测量发生在请求完成之后，当前值已经回落。
#[cfg(target_os = "linux")]
fn peak_rss_kib(pid: u32) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status"))
        .expect("the API process must expose /proc/<pid>/status");
    let line = status
        .lines()
        .find(|line| line.starts_with("VmHWM:"))
        .expect("VmHWM must be reported for a live process");
    line.split_whitespace()
        .nth(1)
        .expect("VmHWM has a numeric value")
        .parse()
        .expect("VmHWM is in KiB")
}

/// A8 与 RFC 0017 §8：一次大图请求后，API 进程的峰值 RSS 落在预先配置的字节预算内。
///
/// 预算在这里是进程级上界而不是"每执行预留"：RFC §8 要求最大合法输入与慢发送下 RSS 落在配置的
/// 预算内。夹具把预算压到**刚好一次执行的预留**（按 Driver 字节上限实测钉出来的那个数），断言
/// 才有判别力。生成入口收敛为只收公网 URL 后，参考图字节不再随请求体进来，而是由 Adapter 从公网
/// URL 取回后再编码进 multipart：假上游按 `reference_image_bytes` 交出一张大图，压的仍是
/// "取图 + 编码"那一段。抖动来源：进程基线与 tokio/reqwest/sqlx 的运行时缓冲、分配器把释放后的
/// 内存留在 arena 里不还给内核、以及 multipart 编码的同尺寸副本。VmHWM 是内核对整段生命周期的
/// 最高水位，只单调上升，不会漏记峰值。
#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn direct_peak_rss_stays_within_the_memory_budget() {
    let memory_budget_bytes = one_execution_reservation_bytes();
    const REFERENCE_IMAGE_BYTES: usize = 6 * 1024 * 1024;
    let harness = Harness::start_direct_with(
        candidate(
            "AIHubMix",
            "aihubmix-image-v1",
            &["prompt_only", "image_conditioned", "masked"],
        ),
        UpstreamBehaviour {
            reference_image_bytes: REFERENCE_IMAGE_BYTES,
            ..UpstreamBehaviour::aihubmix(SyncImageShape::Url)
        },
        4,
        30,
        ApiProcessSettings {
            max_memory_bytes: Some(memory_budget_bytes),
            ..ApiProcessSettings::default()
        },
    )
    .await;
    // 调用方只给公网 URL；参考图的 6 MiB 字节由 Adapter 自己从假上游取回。
    let mut request = route_request(harness.model, "a large reference image");
    request["image"] = json!(harness.png_url());
    let key = format!("direct-peak-rss-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let peak_kib = peak_rss_kib(harness.api_pid());
    let budget_kib = (memory_budget_bytes / 1024) as u64;
    println!("API 子进程峰值 RSS: {peak_kib} KiB（配置预算 {budget_kib} KiB）");
    assert!(
        peak_kib < budget_kib,
        "大图请求后 API 峰值 RSS {peak_kib} KiB 超出配置预算 {budget_kib} KiB"
    );
    harness.cleanup().await;
}

/// R1 与 RFC 0018 §2.1：上游响应顶到 Driver 声明的上限（AIHubMix 128 MiB）时，API 进程的峰值
/// RSS 仍落在"一次执行预留"以内。
///
/// 与 A8 那条大图请求互补：那条压请求侧，这条压响应侧——原始响应缓冲、解析出的图片字符串与
/// 编码后的对客正文在峰值时确实同时存在，正是单次预留要罩住的部分。预算压到刚好一次预留，
/// 因此这条用例的判别力就是"实测预留常数罩不罩得住最大响应"。
#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn direct_max_provider_response_peak_rss_stays_within_the_memory_budget() {
    let memory_budget_bytes = one_execution_reservation_bytes();
    let harness = Harness::start_direct_with(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        UpstreamBehaviour {
            sync_image_b64_bytes: seeai_adapter_aihubmix::MAX_PROVIDER_RESPONSE_BYTES - 8192,
            ..UpstreamBehaviour::aihubmix(SyncImageShape::Base64)
        },
        1,
        30,
        ApiProcessSettings {
            max_memory_bytes: Some(memory_budget_bytes),
            ..ApiProcessSettings::default()
        },
    )
    .await;
    let key = format!("direct-max-response-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &route_request(harness.model, "a max provider response"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert!(
        body["data"][0]["b64_json"].is_string(),
        "渠道给什么就原样回什么，got {body}"
    );
    let peak_kib = peak_rss_kib(harness.api_pid());
    let budget_kib = (memory_budget_bytes / 1024) as u64;
    println!("最大上游响应后 API 子进程峰值 RSS: {peak_kib} KiB（配置预算 {budget_kib} KiB）");
    assert!(
        peak_kib < budget_kib,
        "128 MiB 上游响应后 API 峰值 RSS {peak_kib} KiB 超出配置预算 {budget_kib} KiB"
    );
    harness.cleanup().await;
}

/// 数据库里当前 `held` 的渠道名额行数：跨副本共同遵守的唯一事实就是它。
async fn held_channel_slots(harness: &Harness) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM generation.execution_capacity WHERE state = 'held'")
        .fetch_one(&harness.pool)
        .await
        .expect("channel slot count")
}

/// A8：两个 API 副本连同一数据库时，账户与渠道上限由数据库事实共同遵守。
///
/// 主进程与副本各配 1 个账户名额、1 个渠道名额。第一个请求在主进程上停在上游等待里，此时：
/// - 同一账户在副本上再发一次 ⇒ 429 `too_many_in_flight`（账户名额已被占）；
/// - 另一个账户在副本上发一次 ⇒ 503 `platform_unavailable`（它自己的账户名额是空的，但唯一的
///   渠道名额被占）。
///
/// 两次拒绝都不建执行记录，`execution_capacity` 始终只有第一个请求那一行 held；第一个请求结束
/// 后，另一个账户在副本上的请求照常成功，证明释放也被对方看见。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn two_api_replicas_share_the_account_and_channel_capacity() {
    let harness = Harness::start_direct_with(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        UpstreamBehaviour {
            delay_ms: 3_000,
            ..UpstreamBehaviour::aihubmix(SyncImageShape::Url)
        },
        1,  // 账户在飞上限
        30, // 总期限：要盖过 3s 的上游等待
        ApiProcessSettings {
            channel_max_in_flight: Some(1),
            ..ApiProcessSettings::default()
        },
    )
    .await;
    // 第二个副本与本装置同一个库、同一组上限，但有自己的进程与连接池。
    let (peer_base, peer) = harness
        .start_replica(
            1,
            30,
            &ApiProcessSettings {
                channel_max_in_flight: Some(1),
                ..ApiProcessSettings::default()
            },
        )
        .await;

    // 另一个账户：它自己的账户名额是空的，所以它只能被渠道名额挡住。
    let client = Client::new();
    let (_, other_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;

    // 第一个请求在主进程上停在上游等待里：此时账户与渠道名额都已占住。
    let first_key = format!("replica-cap-first-{}", Uuid::new_v4());
    let first = tokio::spawn({
        let base_url = harness.base_url.clone();
        let api_key = harness.api_key.clone();
        let key = first_key.clone();
        let body = route_request(harness.model, "hold both capacities open");
        async move { post_json(&base_url, &api_key, "/v1/images/generations", &key, &body).await }
    });
    wait_for_create_calls(&harness, 1).await;
    assert_eq!(
        held_channel_slots(&harness).await,
        1,
        "第一个请求必须已经占住唯一的渠道名额"
    );

    // ① 同一账户在**副本**上再发：账户名额已满。
    let same_account_key = format!("replica-cap-same-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &peer_base,
        &harness.api_key,
        "/v1/images/generations",
        &same_account_key,
        &route_request(harness.model, "same account on the peer"),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "got {body}");
    assert_eq!(body["error"]["code"], json!("too_many_in_flight"));
    assert_public_only("跨副本账户名额", &body);

    // ② 另一个账户在副本上发：账户名额是空的，但唯一的渠道名额被占。
    let other_account_key = format!("replica-cap-other-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &peer_base,
        &other_key,
        "/v1/images/generations",
        &other_account_key,
        &route_request(harness.model, "another account on the peer"),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "got {body}");
    assert_eq!(body["error"]["code"], json!("platform_unavailable"));
    assert_public_only("跨副本渠道名额", &body);

    // 两次拒绝都不留下执行记录，held 槽位仍然只有第一个请求那一行。
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM generation.jobs")
            .fetch_one(&harness.pool)
            .await
            .expect("job count"),
        1,
        "被容量拒绝的受理不该建 Job"
    );
    assert_eq!(held_channel_slots(&harness).await, 1);

    // 第一个请求跑完：名额释放，副本立刻看得见。
    let (status, body) = first.await.expect("the first request joins");
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        held_channel_slots(&harness).await,
        0,
        "终态必须释放渠道名额"
    );

    let after_key = format!("replica-cap-after-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &peer_base,
        &other_key,
        "/v1/images/generations",
        &after_key,
        &route_request(harness.model, "the capacity is free again"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("释放后另一账户在副本上的请求", &body);

    drop(peer);
    harness.cleanup().await;
}

/// RFC 0018 §2.3：容量组合不自洽时进程**拒绝启动并点名那两个配置**。
///
/// 8 个执行名额 × 单次预留装在 2 份预算里：多出来的名额永远取不到字节，是配置错误，不是运行时
/// 的"内存拒绝"——所以判据是进程根本没起来，而且 stderr 里能读到该改哪两个变量。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_inconsistent_execution_capacity_combination_refuses_to_start_by_name() {
    let database_url = std::env::var("HTTP_CONTRACT_DATABASE_URL")
        .expect("HTTP_CONTRACT_DATABASE_URL is required for the ignored contract test");
    let reservation = one_execution_reservation_bytes();
    let (running, stderr) =
        probe_api_startup_with_execution_capacity(&database_url, Some(8), Some(reservation * 2))
            .await;
    assert!(
        !running,
        "8 个执行名额装不进 2 份预算，进程必须拒绝启动；stderr: {stderr}"
    );
    assert!(
        stderr.contains("GENERATION_EXECUTION_SLOTS"),
        "报错要点名名额配置；stderr: {stderr}"
    );
    assert!(
        stderr.contains("GENERATION_MAX_MEMORY_BYTES"),
        "报错要点名内存预算；stderr: {stderr}"
    );
}

/// 同一个探针，把组合换成刚好自洽的那一档：进程必须能起来。
///
/// 两条用例共用一份探针，才排除得了"是不是别的必填项没配"这种混淆。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_smallest_consistent_execution_capacity_combination_starts() {
    let database_url = std::env::var("HTTP_CONTRACT_DATABASE_URL")
        .expect("HTTP_CONTRACT_DATABASE_URL is required for the ignored contract test");
    let reservation = one_execution_reservation_bytes();
    let (running, stderr) =
        probe_api_startup_with_execution_capacity(&database_url, Some(2), Some(reservation * 2))
            .await;
    assert!(running, "刚好自洽的组合必须起得来；stderr: {stderr}");
}
