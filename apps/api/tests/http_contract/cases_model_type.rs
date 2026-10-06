use super::*;

/// 发布一份内联命令并带回状态与正文：断言要点名失败原因，所以正文要拿得到。
async fn post_publication(
    client: &Client,
    harness: &Harness,
    body: &Value,
) -> (StatusCode, String) {
    let response = client
        .post(format!("{}/api/v1/runtime-revisions", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(body)
        .send()
        .await
        .expect("publication request");
    let status = response.status();
    (status, response.text().await.expect("publication body"))
}

/// 客户登录拿会话（用例要读自己的用量与账单）。
async fn customer_session(client: &Client, base_url: &str, email: &str, password: &str) -> String {
    client
        .post(format!("{base_url}/v1/customer/sessions"))
        .json(&json!({"email": email, "password": password}))
        .send()
        .await
        .expect("customer login request")
        .json::<Value>()
        .await
        .expect("login body")["token"]
        .as_str()
        .expect("session token")
        .to_owned()
}

/// 把一份已有账户配上客户登录身份（对客读要认客户会话）。
async fn open_customer(
    client: &Client,
    harness: &Harness,
    account_id: &str,
    email: &str,
    password: &str,
) {
    let opened = client
        .post(format!("{}/api/v1/customers", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"email": email, "password": password, "account_id": account_id}))
        .send()
        .await
        .expect("open customer request");
    assert_eq!(opened.status(), StatusCode::CREATED);
}

/// 对客目录只增 `type`，既有字段与取值不变（Spec A1）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_catalog_exposes_the_model_type() {
    let harness = Harness::start(UpstreamBehaviour::aihubmix(SyncImageShape::Url)).await;
    let client = Client::new();
    let catalog: Value = client
        .get(format!("{}/v1/models", harness.base_url))
        .send()
        .await
        .expect("catalog request")
        .json()
        .await
        .expect("catalog body");
    let entry = catalog["data"]
        .as_array()
        .expect("catalog data")
        .iter()
        .find(|entry| entry["name"] == json!(harness.model))
        .expect("the published model is listed");
    assert_eq!(entry["type"], json!("image"), "{catalog}");
    for field in ["name", "vendor_id", "revision", "contract"] {
        assert!(
            entry.get(field).is_some(),
            "catalog must keep {field}: {catalog}"
        );
    }
    harness.cleanup().await;
}

/// 内联发布必须给一个已知类型：缺了或写成三种之外都拒，并点名型号（Spec A3）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_inline_publication_requires_a_known_model_type() {
    let harness = Harness::start(UpstreamBehaviour::aihubmix(SyncImageShape::Url)).await;
    let client = Client::new();
    let mut draft = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    draft["base_url"] = json!(harness.upstream_base_url);

    let mut missing = publication_body(
        "no-type-model",
        "route-test-1",
        None,
        vec![draft.clone()],
        // 这条构造体按上游声明的金额计价，倍率是对客价来源。
        Some(2_000),
    );
    missing.as_object_mut().expect("object").remove("type");
    let (status, body) = post_publication(&client, &harness, &missing).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("type is required"), "{body}");
    assert!(body.contains("no-type-model"), "{body}");

    let mut unknown = publication_body(
        "bad-type-model",
        "route-test-1",
        None,
        vec![draft],
        Some(2_000),
    );
    unknown["type"] = json!("audio");
    let (status, body) = post_publication(&client, &harness, &unknown).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("audio"), "{body}");

    harness.cleanup().await;
}

/// 同一 Vendor Model 修订改类型被拒：类型与合同一样不可变（Spec A4）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_vendor_model_type_is_immutable_within_a_revision() {
    let harness = Harness::start(UpstreamBehaviour::aihubmix(SyncImageShape::Url)).await;
    let client = Client::new();
    let mut draft = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    draft["base_url"] = json!(harness.upstream_base_url);

    let first = publication_body(
        "typed-model",
        "route-test-1",
        None,
        vec![draft.clone()],
        Some(2_000),
    );
    let (status, body) = post_publication(&client, &harness, &first).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let mut second = publication_body(
        "typed-model",
        "route-test-1",
        None,
        vec![draft],
        Some(2_000),
    );
    second["type"] = json!("video");
    let (status, body) = post_publication(&client, &harness, &second).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "改类型必须被拒：{body}");
    assert!(body.contains("type is immutable"), "{body}");
    assert!(body.contains("image"), "{body}");

    harness.cleanup().await;
}

/// 网关模型改绑到别的 Vendor Model 后，已受理记录仍回受理时的类型（Spec A5）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_admitted_usage_row_keeps_the_type_it_was_admitted_with() {
    let harness = Harness::start(UpstreamBehaviour::aihubmix(SyncImageShape::Url)).await;
    let client = Client::new();
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let email = "rebind@example.com";
    let password = "a-long-enough-password";
    open_customer(&client, &harness, &account_id, email, password).await;

    let key = format!("rebind-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request(harness.model, "rebind contract"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");

    // 同一个网关模型名改绑到一个 video 类型的 Vendor Model。
    let mut draft = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    draft["base_url"] = json!(harness.upstream_base_url);
    let mut rebind = publication_body(
        "rebind-vendor-model",
        "route-test-1",
        None,
        vec![draft],
        Some(2_000),
    );
    rebind["gateway_model"] = json!(harness.model);
    rebind["type"] = json!("video");
    let (status, body) = post_publication(&client, &harness, &rebind).await;
    assert_eq!(status, StatusCode::OK, "改绑必须成功：{body}");

    let session = customer_session(&client, &harness.base_url, email, password).await;
    let usage: Value = client
        .get(format!("{}/v1/customer/usage", harness.base_url))
        .bearer_auth(&session)
        .send()
        .await
        .expect("usage request")
        .json()
        .await
        .expect("usage body");
    let row = &usage["usage"][0];
    assert_eq!(row["type"], json!("image"), "{usage}");
    assert_eq!(row["usage"]["images"], json!(1), "{usage}");

    harness.cleanup().await;
}

/// video 记录的量落点缺失时 `usage` 为空；账单只按类型给量，不把 video 记录经图片路径产出的图
/// 算进 `images`（Spec A6、A8）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_video_record_reports_an_empty_usage_and_the_summary_counts_only_image_records() {
    let harness = Harness::start(UpstreamBehaviour::aihubmix(SyncImageShape::Url)).await;
    let client = Client::new();

    let mut draft = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    draft["base_url"] = json!(harness.upstream_base_url);
    let mut video = publication_body(
        "video-model",
        "route-test-1",
        None,
        vec![draft],
        Some(2_000),
    );
    video["type"] = json!("video");
    let (status, body) = post_publication(&client, &harness, &video).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let email = "model-type@example.com";
    let password = "a-long-enough-password";
    open_customer(&client, &harness, &account_id, email, password).await;

    for (model, label) in [(harness.model, "image"), ("video-model", "video")] {
        let key = format!("{label}-{}", Uuid::new_v4());
        let (status, body) = post_json(
            &harness.base_url,
            &api_key,
            "/v1/images/generations",
            &key,
            &route_request(model, label),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "got {body}");
    }

    let session = customer_session(&client, &harness.base_url, email, password).await;
    let usage: Value = client
        .get(format!("{}/v1/customer/usage", harness.base_url))
        .bearer_auth(&session)
        .send()
        .await
        .expect("usage request")
        .json()
        .await
        .expect("usage body");
    let rows = usage["usage"].as_array().expect("usage array");
    let video_row = rows
        .iter()
        .find(|row| row["type"] == json!("video"))
        .expect("the video record is listed");
    assert_eq!(video_row["usage"], json!({}), "{usage}");
    let image_row = rows
        .iter()
        .find(|row| row["type"] == json!("image"))
        .expect("the image record is listed");
    assert_eq!(image_row["usage"]["images"], json!(1), "{usage}");

    let billing: Value = client
        .get(format!("{}/v1/customer/billing", harness.base_url))
        .bearer_auth(&session)
        .send()
        .await
        .expect("billing request")
        .json()
        .await
        .expect("billing body");
    assert_eq!(billing["requests"], json!(2), "{billing}");
    assert_eq!(billing["usage"]["images"], json!(1), "{billing}");
    assert!(
        billing["usage"].get("seconds").is_none(),
        "视频秒数落点未落地，汇总里不该出现 seconds：{billing}"
    );
    // 扣费净额仍与账本一致（Spec A8）。
    let captured: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount_microusd), 0)::bigint FROM ledger.entries
         WHERE account_id = $1 AND kind IN ('capture', 'adjustment')",
    )
    .bind(Uuid::parse_str(&account_id).expect("account id"))
    .fetch_one(&harness.pool)
    .await
    .expect("ledger capture sum");
    assert_eq!(billing["charged_microusd"], json!(captured), "{billing}");

    harness.cleanup().await;
}
