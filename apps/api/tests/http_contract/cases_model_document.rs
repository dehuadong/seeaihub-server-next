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
    document_url(client, base_url, model).await
}

/// 目录里某个模型当前的文档地址。
async fn document_url(client: &Client, base_url: &str, model: &str) -> String {
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

/// A7：四份公共文档（含对客入口）按固定名称提供；未列出的名称与路径穿越都回 404，不提供任意文件读取。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_public_usage_documents_are_served_by_their_fixed_names() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    for name in [
        "README.md",
        "authentication.md",
        "uploads/images.md",
        "http-errors.md",
    ] {
        let response = client
            .get(format!("{base_url}/v1/docs/{name}"))
            .send()
            .await
            .expect("public document");
        assert_eq!(response.status(), StatusCode::OK, "{name}");
        let body = response.text().await.expect("public document body");
        assert!(!body.trim().is_empty(), "{name} 的正文不该为空");
    }
    // 入口讲清目录字段的含义，并把三份说明写成绝对地址。
    let entry = client
        .get(format!("{base_url}/v1/docs/README.md"))
        .send()
        .await
        .expect("entry document")
        .text()
        .await
        .expect("entry body");
    assert!(entry.contains("documentation_url"), "{entry}");
    assert!(
        entry.contains("http://api.contract.test/v1/docs/authentication.md"),
        "{entry}"
    );
    assert!(
        !entry.contains(seeai_application::model_document::BASE_URL_PLACEHOLDER),
        "{entry}"
    );
    for name in ["models/openai/gpt-image-2.5.md", "%2e%2e%2fCargo.toml"] {
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
/// A6 的文案条款：合同修订不变、只改说明文案，也要产生新的文档版本；旧地址读到旧正文。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_copy_only_republish_creates_a_new_document_version() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let contract = surface_schema(json!({
        "model": {"const": "copy-fix-model"},
        "prompt": {"type": "string", "minLength": 1}
    }));
    let status = publish_with_surfaces(
        &client,
        &base_url,
        &admin_token,
        "copy-fix-model",
        "doc-1",
        contract.clone(),
        vec![("aihubmix-image-v1", contract.clone())],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let first_url = document_url(&client, &base_url, "copy-fix-model").await;
    let first_body = read_document(&client, &base_url, &first_url).await;

    // 同一合同修订、同一合同内容：只把内联文档素材的正文换掉再发布一次。
    let mut documentation = documentation_for(&contract);
    documentation["narrative"] =
        json!("# {{platform_name}}\n\n（修正过的说明）\n\n{{parameter_table}}\n");
    let body = json!({
        "vendor_id": "OpenAI",
        "native_model_id": "copy-fix-model",
        "native_revision": "doc-1",
        "type": "image",
        "actor": "contract-test",
        "capability_schema": contract.clone(),
        "documentation": documentation,
        "offerings": [{
            "provider_kind": "AIHubMix",
            "adapter_key": "aihubmix-image-v1",
            "provider_model_id": "copy-fix-model",
            "base_url": "http://127.0.0.1:1",
            "credential_env": "AIHUBMIX_API_KEY",
            "restrictions": {"allowed_branches": ["prompt_only"], "max_reference_images": 0},
            "carrier_schema": contract,
            "parameter_mapping": {},
            "formula": "token_rates",
            "price_plan": {
                "currency": "USD",
                "text_input_microusd_per_million": 5_000_000,
                "image_input_microusd_per_million": 8_000_000,
                "text_output_microusd_per_million": 10_000_000,
                "image_output_microusd_per_million": 30_000_000,
                "source_url": "https://example.invalid/price"
            }
        }]
    });
    let status = client
        .post(format!("{base_url}/api/v1/runtime-revisions"))
        .bearer_auth(&admin_token)
        .json(&body)
        .send()
        .await
        .expect("copy-only publication")
        .status();
    assert_eq!(status, StatusCode::OK);

    let second_url = document_url(&client, &base_url, "copy-fix-model").await;
    assert_ne!(first_url, second_url, "只改文案也要换文档版本");
    let second_body = read_document(&client, &base_url, &second_url).await;
    assert!(second_body.contains("修正过的说明"), "{second_body}");
    assert_eq!(
        read_document(&client, &base_url, &first_url).await,
        first_body,
        "旧版本读到的是旧正文"
    );

    drop_isolated_database(&database_name).await;
}

async fn read_document(client: &Client, base_url: &str, url: &str) -> String {
    let response = client
        .get(format!("{base_url}{url}"))
        .send()
        .await
        .expect("document request");
    assert_eq!(response.status(), StatusCode::OK, "{url}");
    response.text().await.expect("document body")
}

/// A10：当前模型没有文档素材时**阻止切换**——进程起不来并点名模型，不靠隐藏模型通过 A1。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_current_model_without_documentation_material_blocks_startup() {
    let (database_url, database_name) = isolated_database_url().await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");
    let migrations = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    sqlx::migrate::Migrator::new(migrations)
        .await
        .expect("migrator")
        .run(&pool)
        .await
        .expect("migrations");

    // 一个「已发布、可调用」却没有文档素材的模型：不隐藏它才是这条用例要验的处置。
    let vendor_model = Uuid::new_v4();
    let channel = Uuid::new_v4();
    let offering = Uuid::new_v4();
    let price_plan = Uuid::new_v4();
    let revision = Uuid::new_v4();
    let contract = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": {
            "model": {"const": "undocumented-model"},
            "prompt": {"type": "string"}
        }
    });
    sqlx::query("INSERT INTO catalog.vendor_models (id, vendor_id, native_model_id, native_revision, model_type, capability_schema) VALUES ($1,'OpenAI','undocumented-model','r1','image',$2)")
        .bind(vendor_model).bind(&contract).execute(&pool).await.expect("vendor model");
    sqlx::query("INSERT INTO supply.channels (id, provider_kind, base_url, credential_env) VALUES ($1,'AIHubMix','https://api.inferera.com','AIHUBMIX_API_KEY')")
        .bind(channel).execute(&pool).await.expect("channel");
    sqlx::query("INSERT INTO supply.offerings (id, vendor_model_id, channel_id, adapter_key, provider_model_id, restrictions, carrier_schema, parameter_mapping) VALUES ($1,$2,$3,'aihubmix-image-v1','undocumented-model','{}'::jsonb,$4,'{}'::jsonb)")
        .bind(offering).bind(vendor_model).bind(channel).bind(&contract).execute(&pool).await.expect("offering");
    sqlx::query("INSERT INTO pricing.price_plans (id, offering_id, currency, text_input_microusd_per_million, image_input_microusd_per_million, text_output_microusd_per_million, image_output_microusd_per_million, source_url, approved_by) VALUES ($1,$2,'USD',0,0,0,0,'https://example.invalid/price','doc-test')")
        .bind(price_plan).bind(offering).execute(&pool).await.expect("price plan");
    sqlx::query("INSERT INTO publication.runtime_revisions (id, snapshot, published_by, gateway_model, vendor_model_id) VALUES ($1,'{}'::jsonb,'doc-test','undocumented-model',$2)")
        .bind(revision).bind(vendor_model).execute(&pool).await.expect("revision");
    sqlx::query("INSERT INTO publication.runtime_entries (runtime_revision_id, vendor_model_id, offering_id, price_plan_id, gateway_model, active, routing_priority, weight, adapter_key, provider_model_id, carrier_schema, parameter_mapping, restrictions, provider_kind, base_url, credential_env) VALUES ($1,$2,$3,$4,'undocumented-model',true,0,1,'aihubmix-image-v1','undocumented-model',$5,'{}'::jsonb,'{}'::jsonb,'AIHubMix','https://api.inferera.com','AIHUBMIX_API_KEY')")
        .bind(revision).bind(vendor_model).bind(offering).bind(price_plan).bind(&contract).execute(&pool).await.expect("entry");

    let (running, stderr) = probe_api_startup_with_seed_stderr(
        &database_url,
        "probe@example.com",
        "a-long-enough-password",
    )
    .await;
    assert!(!running, "缺素材时进程不该起来：{stderr}");
    assert!(
        stderr.contains("undocumented-model") && stderr.contains("document material"),
        "失败要点名模型与缺的东西：{stderr}"
    );

    drop(pool);
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
