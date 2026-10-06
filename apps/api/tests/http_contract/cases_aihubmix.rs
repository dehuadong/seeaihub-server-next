use super::*;

/// AIHubMix：两条同步路径都能跑通，且**渠道给什么就返回什么**。
///
/// 覆盖公网 URL 输入、`url` / `b64_json` 两种上游形态、data URL 在受理前被拒（Spec 0005 A12），
/// 以及 multipart 正文不再被接受（Spec 0005 §1）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn aihubmix_sync_entries_accept_images_and_return_the_provider_envelope() {
    // 上游给 base64：平台的响应里就必须是 b64_json。
    let harness = Harness::start(UpstreamBehaviour::aihubmix(SyncImageShape::Base64)).await;
    let client = Client::new();

    // 1) 文生图：JSON 入口，没有图片。
    let key = format!("sync-gen-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "sync prompt"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("文生图", &body);
    assert_eq!(
        body["data"][0]["b64_json"].as_str(),
        Some(STANDARD.encode(PNG_FIXTURE).as_str()),
        "上游给 base64，平台必须原样交回"
    );
    // 结果载荷**不落库**：它只在这条对客响应里，所以上面那条断言就是"原样交回"的全部判据。
    assert_job_succeeded(&harness, &key).await;

    // 2) 参考图走**公网 URL**：这个渠道要字节，所以由 Adapter 自己去取。
    let reference_url = format!("{}/inputs/ref.png", harness.upstream_base_url);
    let key = format!("sync-url-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "edit with a public url");
    request["image_urls"] = json!([reference_url]);
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("公网 URL 参考图", &body);
    assert_eq!(
        harness.count("GET", "/inputs/ref.png"),
        1,
        "公网 URL 由 Adapter 自己取一次"
    );
    let edits = harness.submit_bytes("/v1/images/edits");
    let rendered = String::from_utf8_lossy(&edits);
    assert!(
        rendered.contains("name=\"image\""),
        "参考图必须走 image 文件部件"
    );
    assert!(
        body_contains_bytes(&edits, PNG_FIXTURE),
        "公网 URL 取回的字节必须原样进文件部件"
    );
    // 平台不落盘：这次执行没有调用渠道的上传端点，参考图字节由 Adapter 自己从 URL 取。
    assert_eq!(harness.count("POST", "/v1/uploads/images"), 0);

    // 3) data URL 参考图：生成入口只收公网 URL，受理前拒绝。
    let creates_before = harness.create_calls();
    let key = format!("sync-inline-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "edit with an inline image");
    request["image"] = json!(inline_png_data_url());
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert_eq!(
        body["error"]["code"].as_str(),
        Some("public_image_url_required"),
        "{body}"
    );
    assert_public_only("data URL 参考图", &body);
    // 拒绝发生在受理前：不调上游、不建执行记录。
    assert_eq!(harness.create_calls(), creates_before, "被拒请求不调上游");
    let jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM generation.jobs WHERE idempotency_key_digest = $1",
    )
    .bind(idempotency_key_digest(&key))
    .fetch_one(&harness.pool)
    .await
    .expect("job count");
    assert_eq!(jobs, 0, "被拒的 data URL 不建执行记录");

    // 4) 两个图片入口共用一套 JSON 解码：multipart 正文不再被接受（Spec 0005 §1）。
    let key = format!("sync-edit-{}", Uuid::new_v4());
    let form = reqwest::multipart::Form::new()
        .part(
            "image",
            reqwest::multipart::Part::bytes(PNG_FIXTURE.to_vec())
                .file_name("input.png")
                .mime_str("image/png")
                .expect("mime"),
        )
        .text("model", harness.model.to_owned())
        .text("prompt", "edit through the multipart entry");
    let (status, body) = harness.sync_multipart(&key, form).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "got {body}");
    assert_eq!(harness.create_calls(), creates_before, "被拒请求不调上游");

    // 5) 同义字段只能给一个；只给遮罩是结构性错误。
    let key = format!("sync-conflict-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "both synonyms");
    request["image"] = json!(harness.png_url());
    request["image_urls"] = json!(["https://example.invalid/a.png"]);
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert_eq!(body["error"]["code"].as_str(), Some("invalid_parameter"));
    assert_public_only("同义字段冲突", &body);

    let key = format!("sync-mask-only-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "mask without an image");
    request["mask"] = json!(harness.png_url());
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert_public_only("只有遮罩", &body);

    let _ = client;
    harness.cleanup().await;
}

/// AIHubMix 的参考图按**张数**换线上部件名：多张是重复的 `image[]`，单张才是单值 `image`。
///
/// 为什么要有这条端到端：这条规则在 Driver 单测里已经钉住，但真正要保证的是**发布出去的那份
/// 声明面**——参考图按厂商契约声明成字符串数组（≤16）——能一路走到线上：受理时两张都留得下、
/// 选路时这条候选表达得了、装图时两张都落到它声明的名字上、最后按张数编码。上面任何一处只留下
/// 第一张，对客响应照样是 200，只有看发给上游的报文才暴露出来。
///
/// 两张都指向假上游的公网 URL：字节由 Adapter 自己取，线上按张数编码。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn aihubmix_encodes_several_reference_images_as_repeated_list_parts() {
    let harness =
        Harness::start_with_bootstrap(UpstreamBehaviour::aihubmix(SyncImageShape::Url), 64).await;

    // 1) 两张参考图：线上是两个 `image[]` 部件，不出现单值 `image`，也没有遮罩部件。
    let key = format!("sync-two-refs-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "edit with two reference images");
    request["image"] = json!([harness.png_url(), harness.input_url("ref-2.png")]);
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request)
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("两张参考图", &body);
    // 带图的请求走的是编辑端点：这条渠道的参考图只在 /v1/images/edits 上收。
    assert_eq!(harness.count("POST", "/v1/images/edits"), 1);
    assert_eq!(harness.count("POST", "/v1/images/generations"), 0);

    let edits = harness.submit_bytes("/v1/images/edits");
    let rendered = String::from_utf8_lossy(&edits);
    assert_eq!(
        part_name_count(&rendered, "image[]"),
        2,
        "两张参考图就是两个 `image[]` 部件：{rendered}"
    );
    assert_eq!(
        part_name_count(&rendered, "image"),
        0,
        "多张时不许退回单值 `image`（渠道会 400）：{rendered}"
    );
    assert_eq!(
        part_name_count(&rendered, "mask"),
        0,
        "这次请求没有遮罩，线上就不该有 `mask` 部件：{rendered}"
    );
    assert!(
        body_contains_bytes(&edits, PNG_FIXTURE),
        "参考图的字节必须原样进文件部件"
    );

    // 2) 一张参考图：同一份声明面下仍是单值 `image`——列表形态只属于多张。
    let key = format!("sync-one-ref-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "edit with one reference image");
    request["image"] = json!([harness.png_url()]);
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request)
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("一张参考图", &body);
    let edits = harness.submit_bytes("/v1/images/edits");
    let rendered = String::from_utf8_lossy(&edits);
    assert_eq!(
        part_name_count(&rendered, "image"),
        1,
        "一张参考图就是单值 `image`：{rendered}"
    );
    assert_eq!(
        part_name_count(&rendered, "image[]"),
        0,
        "单张不许用列表形态：{rendered}"
    );

    assert_job_succeeded(&harness, &key).await;
    harness.cleanup().await;
}

/// 上游给 `url` 时，平台把那个地址**原样**交回，绝不下载、不转存。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn aihubmix_returns_the_url_shape_verbatim() {
    let harness = Harness::start(UpstreamBehaviour::aihubmix(SyncImageShape::Url)).await;
    let key = format!("sync-url-shape-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "url shape"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("url 形态", &body);
    let expected = format!("{}/result.png", harness.upstream_base_url);
    assert_eq!(body["data"][0]["url"].as_str(), Some(expected.as_str()));
    assert!(
        body["data"][0].get("b64_json").is_none(),
        "上游只给了 url，平台不许自己补一个 base64"
    );
    // 结果地址是**给调用方**的：平台自己不去取它。
    assert_eq!(harness.count("GET", "/result.png"), 0);
    assert_job_succeeded(&harness, &key).await;
    harness.cleanup().await;
}
