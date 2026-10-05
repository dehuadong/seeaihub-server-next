use super::*;

/// 发布一个模型并取它在目录里的文档地址。
async fn publish_and_read_url(
    client: &Client,
    base_url: &str,
    admin_token: &str,
    model: &str,
    revision: &str,
) -> String {
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "n": {"type": "integer", "minimum": 1, "maximum": 4, "default": 1}
    }));
    let status = publish_with_surfaces(
        client,
        base_url,
        admin_token,
        model,
        revision,
        contract.clone(),
        vec![("aihubmix-image-v1", contract)],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{model} {revision} 必须发布成功");
    let (status, catalog) = get_catalog(client, base_url, None).await;
    assert_eq!(status, StatusCode::OK, "{catalog}");
    let entry = catalog["data"]
        .as_array()
        .expect("catalog data")
        .iter()
        .find(|entry| entry["name"] == json!(model))
        .unwrap_or_else(|| panic!("目录里找不到 {model}：{catalog}"));
    entry["documentation_url"]
        .as_str()
        .expect("documentation_url")
        .to_owned()
}

/// A1：目录里的每个模型都给出可读的 `documentation_url`；公开读取不需要凭据，正文是同版合同渲染的
/// Markdown（平台名、参数表、转好的公共文档链接都在）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_catalog_advertises_a_readable_document_for_every_model() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let url = publish_and_read_url(
        &client,
        &base_url,
        &admin_token,
        "documented-model",
        "doc-1",
    )
    .await;
    assert!(
        url.starts_with("/v1/models/documented-model/llms.txt?version="),
        "目录给的是同源根相对地址：{url}"
    );

    let response = client
        .get(format!("{base_url}{url}"))
        .send()
        .await
        .expect("document request");
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(content_type.starts_with("text/plain"), "{content_type}");
    let body = response.text().await.expect("document body");
    assert!(body.contains("# documented-model"), "{body}");
    assert!(body.contains("| `prompt` | 是 |"), "{body}");
    assert!(body.contains("| `n` | 否 |"), "{body}");
    // 素材里的相对链接在发布时转成同源公共地址。
    assert!(body.contains("/v1/docs/authentication.md"), "{body}");
    assert!(!body.contains("../../"), "{body}");

    drop_isolated_database(&database_name).await;
}

/// A2：重新发布只换当前版本；旧版本地址仍读到**当日那份正文**，未知版本回 404。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_document_version_keeps_its_body_after_the_model_is_republished() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let first_url =
        publish_and_read_url(&client, &base_url, &admin_token, "history-model", "doc-1").await;
    let first_body = client
        .get(format!("{base_url}{first_url}"))
        .send()
        .await
        .expect("first document")
        .text()
        .await
        .expect("first body");
    assert!(first_body.contains("修订 doc-1"), "{first_body}");

    let second_url =
        publish_and_read_url(&client, &base_url, &admin_token, "history-model", "doc-2").await;
    assert_ne!(first_url, second_url, "重发换的是当前版本标识");
    let second_body = client
        .get(format!("{base_url}{second_url}"))
        .send()
        .await
        .expect("second document")
        .text()
        .await
        .expect("second body");
    assert!(second_body.contains("修订 doc-2"), "{second_body}");

    // 旧版本地址仍然读到旧正文——发布快照不随之后的发布变化。
    let archived = client
        .get(format!("{base_url}{first_url}"))
        .send()
        .await
        .expect("archived document");
    assert_eq!(archived.status(), StatusCode::OK);
    assert_eq!(archived.text().await.expect("archived body"), first_body);

    // 未知版本、以及「属于别的模型的版本」都回 404，不回退到当前版本。
    let unknown = client
        .get(format!("{base_url}/v1/models/history-model/llms.txt?version=00000000-0000-4000-8000-000000000000"))
        .send()
        .await
        .expect("unknown version");
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    let mismatched = client
        .get(format!(
            "{base_url}/v1/models/other-model/llms.txt?version={}",
            version_of(&second_url)
        ))
        .send()
        .await
        .expect("mismatched name");
    assert_eq!(mismatched.status(), StatusCode::NOT_FOUND);

    drop_isolated_database(&database_name).await;
}

fn version_of(url: &str) -> &str {
    url.split("version=").nth(1).expect("version parameter")
}

/// A7：三份公共文档按固定名称提供；未列出的名称与路径穿越都回 404，不提供任意文件读取。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_public_usage_documents_are_served_by_their_fixed_names() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    for name in ["authentication.md", "uploads/images.md", "http-errors.md"] {
        let response = client
            .get(format!("{base_url}/v1/docs/{name}"))
            .send()
            .await
            .expect("public document");
        assert_eq!(response.status(), StatusCode::OK, "{name}");
        let body = response.text().await.expect("public document body");
        assert!(!body.trim().is_empty(), "{name} 的正文不该为空");
    }
    for name in [
        "README.md",
        "models/openai/gpt-image-2.5.md",
        "%2e%2e%2fCargo.toml",
    ] {
        let response = client
            .get(format!("{base_url}/v1/docs/{name}"))
            .send()
            .await
            .expect("rejected document");
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{name}");
    }

    drop_isolated_database(&database_name).await;
}

/// A9：模型名里的空格、斜杠与 URI 保留字符按路径段编码，目录地址能原样读回同一份正文。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_model_name_with_reserved_characters_is_addressable() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let model = "模型/带 空格+plus";
    let url = publish_and_read_url(&client, &base_url, &admin_token, model, "doc-1").await;
    assert!(url.contains("%2F"), "斜杠要编码进路径段：{url}");
    assert!(url.contains("%20"), "空格要编码：{url}");
    let response = client
        .get(format!("{base_url}{url}"))
        .send()
        .await
        .expect("encoded document");
    assert_eq!(response.status(), StatusCode::OK, "{url}");
    let body = response.text().await.expect("document body");
    assert!(body.contains(&format!("# {model}")), "{body}");

    drop_isolated_database(&database_name).await;
}
/// A7 的后半：停用模型只影响无版本读取，已知文档版本仍按（平台名, 版本）读得到。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn disabling_a_model_keeps_its_published_document_readable() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let url =
        publish_and_read_url(&client, &base_url, &admin_token, "archived-model", "doc-1").await;
    let body = client
        .get(format!("{base_url}{url}"))
        .send()
        .await
        .expect("document")
        .text()
        .await
        .expect("body");

    assert_eq!(
        patch_gateway_model(&client, &base_url, &admin_token, "archived-model", false).await,
        StatusCode::NO_CONTENT
    );
    // 无版本读取跟着当前可调用判据走：停用后 404。
    let current = client
        .get(format!("{base_url}/v1/models/archived-model/llms.txt"))
        .send()
        .await
        .expect("current document");
    assert_eq!(current.status(), StatusCode::NOT_FOUND);
    // 历史读取只看（平台名, 版本），不经当前启用开关：已知版本仍读到同一份正文。
    let versioned = client
        .get(format!("{base_url}{url}"))
        .send()
        .await
        .expect("versioned document");
    assert_eq!(versioned.status(), StatusCode::OK);
    assert_eq!(versioned.text().await.expect("versioned body"), body);

    drop_isolated_database(&database_name).await;
}

/// A8：发布在生效之前被拒时，目录里的文档地址与已发布正文都不变。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_failed_publication_leaves_the_published_document_unchanged() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let url = publish_and_read_url(&client, &base_url, &admin_token, "stable-model", "doc-1").await;
    let body = client
        .get(format!("{base_url}{url}"))
        .send()
        .await
        .expect("document")
        .text()
        .await
        .expect("body");

    // 同一修订换一份合同：合同不可变，发布必须整次回滚。
    let changed = surface_schema(json!({
        "model": {"const": "stable-model"},
        "prompt": {"type": "string", "minLength": 1},
        "n": {"type": "integer", "minimum": 2, "maximum": 4}
    }));
    let status = publish_with_surfaces(
        &client,
        &base_url,
        &admin_token,
        "stable-model",
        "doc-1",
        changed.clone(),
        vec![("aihubmix-image-v1", changed)],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "改合同必须被拒");

    let (_, catalog) = get_catalog(&client, &base_url, None).await;
    let entry = catalog["data"]
        .as_array()
        .expect("catalog data")
        .iter()
        .find(|entry| entry["name"] == json!("stable-model"))
        .expect("the model stays in the catalog");
    assert_eq!(
        entry["documentation_url"],
        json!(url),
        "失败的发布不改文档地址"
    );
    let after = client
        .get(format!("{base_url}{url}"))
        .send()
        .await
        .expect("document")
        .text()
        .await
        .expect("body");
    assert_eq!(after, body, "失败的发布不改已发布正文");

    drop_isolated_database(&database_name).await;
}
