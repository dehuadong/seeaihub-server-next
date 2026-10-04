use super::*;

/// AIHubMix：两条同步路径都能跑通，且**渠道给什么就返回什么**。
///
/// 覆盖 data URL 输入、公网 URL 输入与 `url` / `b64_json` 两种上游形态。
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
    // 平台不落盘：没有上传接口调用，也没有资产接口可用。
    assert_eq!(harness.count("POST", "/v1/uploads/images"), 0);

    // 3) 参考图走**data URL**：就地解码成字节，仍然不落盘。
    let key = format!("sync-inline-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "edit with an inline image");
    request["image"] = json!(png_data_url());
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("data URL 参考图", &body);
    let edits = harness.submit_bytes("/v1/images/edits");
    assert!(
        body_contains_bytes(&edits, PNG_FIXTURE),
        "data URL 必须就地解码进文件部件"
    );
    let (_, state) = harness.job(&key).await;
    assert_eq!(state, "succeeded");

    // 4) edits 入口（multipart 文件部件）：与 generations 是**同一个能力**。
    let key = format!("sync-edit-{}", Uuid::new_v4());
    let form = reqwest::multipart::Form::new()
        .part(
            "image",
            reqwest::multipart::Part::bytes(PNG_FIXTURE.to_vec())
                .file_name("input.png")
                .mime_str("image/png")
                .expect("mime"),
        )
        .part(
            "mask",
            reqwest::multipart::Part::bytes(PNG_FIXTURE.to_vec())
                .file_name("mask.png")
                .mime_str("image/png")
                .expect("mime"),
        )
        .text("model", harness.model.to_owned())
        .text("prompt", "edit through the multipart entry");
    let (status, body) = harness.sync_multipart(&key, form).await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("edits 入口", &body);
    let edits = harness.submit_bytes("/v1/images/edits");
    let rendered = String::from_utf8_lossy(&edits);
    assert!(rendered.contains("name=\"image\"") && rendered.contains("name=\"mask\""));
    assert!(
        body_contains_bytes(&edits, PNG_FIXTURE),
        "两个文件部件的字节都必须到上游"
    );
    // 文件部件在受理期被转成 data URL 语义、落在候选声明的参数名上；这一步的可见判据就是
    // 上面那条"字节原样进文件部件"——载荷本身不落库。

    // 5) 同义字段只能给一个；只给遮罩是结构性错误。
    let key = format!("sync-conflict-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "both synonyms");
    request["image"] = json!(png_data_url());
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
    request["mask"] = json!(png_data_url());
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
/// 两张都用内联 data URL：图片字节就地解码，这条链路不必让假上游提供图片文件。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn aihubmix_encodes_several_reference_images_as_repeated_list_parts() {
    let harness =
        Harness::start_with_bootstrap(UpstreamBehaviour::aihubmix(SyncImageShape::Url), 64).await;

    // 1) 两张参考图：线上是两个 `image[]` 部件，不出现单值 `image`，也没有遮罩部件。
    let key = format!("sync-two-refs-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "edit with two reference images");
    request["image"] = json!([png_data_url(), png_data_url()]);
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
    request["image"] = json!([png_data_url()]);
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
