use super::*;

/// 发布命令支持**增量**：候选省略渠道三要素与驱动器时，从该型号当前生效的修订按
/// `provider_kind` + `provider_model_id` 复用（`docs/design/0010` §4.1）。
///
/// 这条要证明的是"改价不用重填技术字段"能成立：先整份发布一次，再只带着新的加价系数与对客费率发一次，
/// **候选里不出现 `base_url` / `credential_env` / `adapter_key`**，而落库的候选仍然指向同一个渠道、
/// 同一把凭证身份——沿用真的发生了，不是被当成了空值。
///
/// 反例一起验：同一 `provider_kind` 与渠道模型名在上一版有多条候选时按歧义拒绝，不猜。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_publication_may_omit_the_channel_and_inherit_it_from_the_current_revision() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    let model = "inherited-channel-model";
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1}
    }));

    // 第一次：整份发布，候选自带渠道三要素（`candidate()` 已经带了一个地址占位）。
    let full = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    assert_eq!(
        publish_candidates(
            &client,
            &base_url,
            &admin_token,
            model,
            Some(contract.clone()),
            vec![full]
        )
        .await,
        StatusCode::OK,
        "整份发布应当成功"
    );

    // 第二次：**省略**渠道三要素与驱动器，只带新的加价系数与对客费率。
    let mut lean = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    {
        let object = lean.as_object_mut().expect("candidate is an object");
        object.remove("base_url");
        object.remove("credential_env");
        object.remove("adapter_key");
    }
    lean["consumer_rates_cny"] = json!({
        "text_input_micros_per_million": 42_600_000u64,
        "image_input_micros_per_million": 68_160_000u64,
        "text_output_micros_per_million": 85_200_000u64,
        "image_output_micros_per_million": 255_600_000u64
    });
    let lean_body = publication_body(
        model,
        "route-test-1",
        Some(contract),
        vec![lean],
        Some(2_400),
    );
    let lean_response = client
        .post(format!("{base_url}/api/v1/runtime-revisions"))
        .bearer_auth(&admin_token)
        .json(&lean_body)
        .send()
        .await
        .expect("incremental publication");
    let lean_status = lean_response.status();
    let lean_text = lean_response.text().await.expect("incremental body");
    assert_eq!(
        lean_status,
        StatusCode::OK,
        "省略渠道的增量发布应当成功：{lean_text}"
    );

    // 沿用真的发生了：当前生效的候选只有一条，且它的渠道三要素与驱动器都还在。
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT c.provider_kind, c.base_url, c.credential_env, o.adapter_key
         FROM publication.runtime_entries re
         JOIN supply.offerings o ON o.id = re.offering_id
         JOIN supply.channels c ON c.id = o.channel_id
         WHERE re.active AND re.gateway_model = $1",
    )
    .bind(model)
    .fetch_all(&pool)
    .await
    .expect("inherited candidate");
    assert_eq!(rows.len(), 1, "只该有一条生效候选：{rows:?}");
    let (provider_kind, upstream, credential_env, adapter_key) = &rows[0];
    assert_eq!(provider_kind, "AIHubMix");
    assert!(!upstream.is_empty(), "渠道地址必须被沿用了回来");
    assert_eq!(credential_env, "AIHUBMIX_API_KEY");
    assert_eq!(adapter_key, "aihubmix-image-v1");

    // 新的加价系数真的落库（这次发布改变的东西必须改变）。
    let markup: Option<i32> = sqlx::query_scalar(
        "SELECT markup_bps FROM publication.runtime_revisions
         WHERE id = (SELECT runtime_revision_id FROM publication.runtime_entries
                     WHERE active AND gateway_model = $1)",
    )
    .bind(model)
    .fetch_one(&pool)
    .await
    .expect("markup of the active revision");
    assert_eq!(markup, Some(2_400), "加价系数应当是这次发布给的值");

    // 渠道价目**原样沿用**：这次发布一个字都没提它，所以四档费率与价目出处都得是上一版那一份——
    // 落成空值的效果是"改价顺手改坏了渠道结算依据"，而它在结算之前不会有人发现。
    let (text_input, source_url): (i64, String) = sqlx::query_as(
        "SELECT p.text_input_microusd_per_million, p.source_url
         FROM publication.runtime_entries re
         JOIN pricing.price_plans p ON p.id = re.price_plan_id
         WHERE re.active AND re.gateway_model = $1",
    )
    .bind(model)
    .fetch_one(&pool)
    .await
    .expect("inherited price plan");
    assert_eq!(text_input, 5_000_000, "四档渠道费率必须原样沿用");
    assert_eq!(
        source_url, "https://example.invalid/price",
        "价目出处必须原样沿用"
    );

    drop_isolated_database(&database_name).await;
}

/// 已发布修订的技术定义**冻结在条目上**：改活表（渠道地址）不改已发修订的受理口径（`#33` 的 P6）。
///
/// 这条要证明的是"发布即冻结"这句话对**候选**也成立。`0016` 已经把执行入口冻到 Job 上，但在它之前
/// 那一段——装配候选读的驱动器、承载面与渠道三要素——一直是现场 JOIN 活表读的。于是工程师事后改一条
/// 渠道的地址，**已发布修订**的候选跟着变；两条指向同一个厂商模型的网关模型还会互相改活对方的候选。
///
/// 判据分两半，缺一不可：
/// - 改活表之后，**同一份已发修订**的受理口径逐位不变（旧值）；
/// - 而**重新发布**出来的新修订取到的是**新值**——否则把值写死在代码里也能让前一半通过。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_published_revision_keeps_its_offering_definition_when_the_channel_changes() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    let model = "frozen-supply-definition-model";
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1}
    }));
    let mut full = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    full["base_url"] = json!("https://frozen.example.com");
    assert_eq!(
        publish_candidates(
            &client,
            &base_url,
            &admin_token,
            model,
            Some(contract.clone()),
            vec![full]
        )
        .await,
        StatusCode::OK,
        "第一次发布应当成功"
    );

    // 直接改活表：模拟"工程师事后换了这个渠道的入口"。
    let updated = sqlx::query(
        "UPDATE supply.channels SET base_url = 'https://moved.example.com'
         WHERE provider_kind = 'AIHubMix' AND base_url = 'https://frozen.example.com'",
    )
    .execute(&pool)
    .await
    .expect("move the channel")
    .rows_affected();
    assert_eq!(updated, 1, "夹具必须改到那条渠道");

    // 已发修订那次发布的条目**仍是旧值**：受理读的是它，不是活表。
    let frozen: String = sqlx::query_scalar(
        "SELECT base_url FROM publication.runtime_entries
         WHERE active AND gateway_model = $1",
    )
    .bind(model)
    .fetch_one(&pool)
    .await
    .expect("frozen entry");
    assert_eq!(
        frozen, "https://frozen.example.com",
        "已发布修订的条目必须留着发布那一刻的渠道地址"
    );

    // 反向确认：重新发布出来的新修订取到的是**新值**——否则上面那条断言靠写死也能过。
    let mut republished = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    republished["base_url"] = json!("https://moved.example.com");
    assert_eq!(
        publish_candidates(
            &client,
            &base_url,
            &admin_token,
            model,
            Some(contract),
            vec![republished]
        )
        .await,
        StatusCode::OK,
        "重新发布应当成功"
    );
    let fresh: String = sqlx::query_scalar(
        "SELECT base_url FROM publication.runtime_entries
         WHERE active AND gateway_model = $1",
    )
    .bind(model)
    .fetch_one(&pool)
    .await
    .expect("fresh entry");
    assert_eq!(
        fresh, "https://moved.example.com",
        "新发布的修订要取到新的渠道地址"
    );

    drop_isolated_database(&database_name).await;
}

/// 省略渠道但上一版里没有同身份的候选：拒绝并点名，不用"最近的那条"顶替。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_incremental_publication_without_a_matching_previous_offering_is_rejected() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let model = "no-previous-offering-model";
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1}
    }));

    // 一次都没发布过：型号还没有生效修订可沿用。
    // 用真的删掉字段来表达"省略"：给 `null` 是另一回事（当前形状下会先被反序列化拒掉）。
    let mut lean = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    lean["provider_model_id"] = json!("a-model-this-revision-never-had");
    {
        let object = lean.as_object_mut().expect("candidate is an object");
        object.remove("base_url");
        object.remove("credential_env");
        object.remove("adapter_key");
    }
    let response = client
        .post(format!("{base_url}/api/v1/runtime-revisions"))
        .bearer_auth(&admin_token)
        .json(&publication_body(
            model,
            "route-test-1",
            Some(contract),
            vec![lean],
            None,
        ))
        .send()
        .await
        .expect("incremental publication");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response.text().await.expect("body");
    assert!(
        body.contains("no offering with that identity"),
        "拒绝的理由要点名沿用不到：{body}"
    );

    drop_isolated_database(&database_name).await;
}

/// 同一个网关模型的**并发发布**：替换必须真的是一次替换——两份修订的 active 条目不得并存。
///
/// 为什么单独验这一条：唯一索引从"每型号每档一条"换成"每型号每条供给一行"之后，它不再能拦住
/// "两份修订同时生效"。而"active 候选跨修订并存"是**读时**才会暴露的问题：表现是这个型号的
/// 所有请求一起失败（平台侧故障），直到有人重新发布一次。发布事务按名字取事务级咨询锁就是为了
/// 让这件事不可能发生——没有那把锁时，两条并发发布的"先失效、后插入"会交错。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn concurrent_publications_of_one_gateway_model_leave_a_single_active_revision() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");
    let account = create_account(&client, &base_url, &admin_token).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;

    let model = "concurrent-publish-model";
    let mut tasks = Vec::new();
    for index in 0..6_u64 {
        let mut offering = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
        offering["routing_priority"] = json!(index % 2);
        offering["weight"] = json!(index + 1);
        let body = publication_body(model, "route-test-1", None, vec![offering], None);
        let client = client.clone();
        let url = format!("{base_url}/api/v1/runtime-revisions");
        let token = admin_token.clone();
        tasks.push(tokio::spawn(async move {
            client
                .post(url)
                .bearer_auth(token)
                .json(&body)
                .send()
                .await
                .expect("concurrent publication")
                .status()
        }));
    }
    for task in tasks {
        let status = task.await.expect("publication task must not panic");
        assert_eq!(status, StatusCode::OK, "并发发布各自都该成功");
    }

    let revisions: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT runtime_revision_id) FROM publication.runtime_entries
         WHERE active AND gateway_model = $1",
    )
    .bind(model)
    .fetch_one(&pool)
    .await
    .expect("active revisions");
    assert_eq!(
        revisions, 1,
        "同一名字的 active 条目必须来自同一次发布（并发发布不得交错）"
    );

    // 受理照常：跨修订并存会让这个型号的所有请求一起失败。
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        "concurrent-publish-0001",
        &route_request(model, "concurrent publish"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "并发发布之后这个型号仍要能受理（没有 Worker，只会等到超时）：{body}"
    );

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 第二阶段的**发布素材**要真的能用：一个 Vendor Model 一份文件、只落**一份合同**，
/// 而每个候选各带**自己的承载面**——缺一不可：素材发不出去、或候选没带上自己的承载面，
/// 都算没覆盖。
///
/// 素材本身就是完整的发布命令（顶层一份合同 + 两条供给），所以直接按它发布：
/// AIHubMix 下标 0（收 `image` / `mask`），APIMart 下标 1（收 `image_urls` / `mask_url`），
/// 两家能承载的字段面不同，正是"合同一份、承载面各一份"要覆盖的情形。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn stage_two_bootstrap_material_publishes_one_contract_with_per_candidate_carriers() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    let material: Value = serde_json::from_str(include_str!(
        "../../../../config/bootstrap/gpt-image-2.5-flare.json"
    ))
    .expect("2.5 material parses");
    let model = material["native_model_id"]
        .as_str()
        .expect("native model id")
        .to_owned();
    // 路由条目挂的是**平台对客名**：合同按厂商原生名落行，候选集按对客名生效。种子素材不写
    // 这个字段，按发布期的回退规则取厂商原生名；平台自命名的例子见命名层那条用例。
    let gateway = material["gateway_model"]
        .as_str()
        .unwrap_or(&model)
        .to_owned();
    let revision = material["native_revision"]
        .as_str()
        .expect("native revision")
        .to_owned();
    let offerings = material["offerings"]
        .as_array()
        .expect("offerings must be an array")
        .clone();
    assert_eq!(offerings.len(), 2, "一份素材两条供给");
    assert_eq!(
        offerings[0]["provider_kind"], "AIHubMix",
        "下标 0 是首选：AIHubMix"
    );
    assert_eq!(
        offerings[1]["provider_kind"], "APIMart",
        "下标 1 是次选：APIMart"
    );
    // 两家能承载的面必须真的不同——否则这个用例覆盖不到"承载面各自一份"。
    let carriers = [
        offerings[0]["carrier_schema"].clone(),
        offerings[1]["carrier_schema"].clone(),
    ];
    assert_ne!(
        carriers[0], carriers[1],
        "this test only covers the split surface if the two carriers actually differ"
    );

    let published = client
        .post(format!("{base_url}/api/v1/runtime-revisions"))
        .bearer_auth(&admin_token)
        .json(&material)
        .send()
        .await
        .expect("publication request");
    assert_eq!(
        published.status(),
        StatusCode::OK,
        "the prepared 2.5 material must be publishable: {:?}",
        published.text().await
    );

    // 合同是**模型级唯一一份**：同一个型号的这一版只落一行，内容就是顶层那一份。
    let contracts = sqlx::query(
        "SELECT id, capability_schema FROM catalog.vendor_models
         WHERE vendor_id = $1 AND native_model_id = $2 AND native_revision = $3",
    )
    .bind(material["vendor_id"].as_str().expect("vendor id"))
    .bind(&model)
    .bind(&revision)
    .fetch_all(&pool)
    .await
    .expect("contract rows");
    assert_eq!(contracts.len(), 1, "one contract per vendor model revision");
    let stored_contract: Value = contracts[0].try_get("capability_schema").expect("contract");
    assert_eq!(
        stored_contract, material["capability_schema"],
        "the stored contract must be the one given at the top level"
    );

    // 每个候选携带**它自己**的承载面，而合同只读那一份。
    let rows = sqlx::query(
        "SELECT o.provider_model_id, o.adapter_key, o.carrier_schema, o.parameter_mapping,
                c.provider_kind, vm.capability_schema
         FROM publication.runtime_entries re
         JOIN supply.offerings o ON o.id = re.offering_id
         JOIN supply.channels c ON c.id = o.channel_id
         JOIN catalog.vendor_models vm ON vm.id = re.vendor_model_id
         WHERE re.active AND re.gateway_model = $1
         ORDER BY re.routing_priority",
    )
    .bind(&gateway)
    .fetch_all(&pool)
    .await
    .expect("candidate rows");
    assert_eq!(rows.len(), 2, "both providers must be active candidates");

    let stored_carriers: Vec<Value> = rows
        .iter()
        .map(|row| row.try_get("carrier_schema").expect("carrier"))
        .collect();
    assert_ne!(
        stored_carriers[0], stored_carriers[1],
        "each candidate must carry its own surface, not a shared one"
    );
    for (index, offering) in offerings.iter().enumerate() {
        let row = &rows[index];
        let provider_model_id: String = row.try_get("provider_model_id").expect("provider model");
        let adapter_key: String = row.try_get("adapter_key").expect("adapter key");
        assert_eq!(provider_model_id, offering["provider_model_id"]);
        assert_eq!(adapter_key, offering["adapter_key"]);
        assert_eq!(
            stored_carriers[index], offering["carrier_schema"],
            "candidate {index} must carry its own surface"
        );
        assert_eq!(
            stored_carriers[index], carriers[index],
            "候选带上线的承载面必须逐字就是素材里那一份"
        );
        // 每个候选读到的合同都是同一份，且它的 `model.const` 就是该型号。
        let contract: Value = row.try_get("capability_schema").expect("contract");
        assert_eq!(contract, stored_contract);
        assert_eq!(contract["properties"]["model"]["const"], model);
        // 承载面的每个字段名都要**从合同可达**（R1 的判据，这里独立复核一遍）：要么合同直接声明，
        // 要么被 `rename` 接过去——线上名不必等于合同名（APIMart 的 `image_urls` 就是这么来的）。
        let mapping: Value = row.try_get("parameter_mapping").expect("mapping");
        let wires: Vec<Value> = mapping["rename"]
            .as_object()
            .map(|renames| renames.values().cloned().collect())
            .unwrap_or_default();
        for name in stored_carriers[index]["properties"]
            .as_object()
            .expect("carrier properties")
            .keys()
        {
            let declared = contract["properties"]
                .as_object()
                .expect("contract properties")
                .contains_key(name);
            let renamed = wires
                .iter()
                .any(|wire| wire.as_str() == Some(name.as_str()));
            assert!(
                declared || renamed,
                "carrier field {name} must be reachable from the contract"
            );
        }
    }

    // 快照指纹跟着"合同 + 承载面"走：两个候选承载面不同，指纹就该不同。
    let snapshot: Value = sqlx::query_scalar(
        "SELECT snapshot FROM publication.runtime_revisions rr
         JOIN publication.runtime_entries re ON re.runtime_revision_id = rr.id
         WHERE re.active AND re.gateway_model = $1 LIMIT 1",
    )
    .bind(&gateway)
    .fetch_one(&pool)
    .await
    .expect("runtime revision snapshot");
    let candidates = snapshot["candidates"]
        .as_array()
        .expect("snapshot candidates");
    assert_eq!(candidates.len(), 2);
    for candidate in candidates {
        assert!(
            candidate.get("schema_hash").is_none(),
            "快照指纹必须覆盖合同与承载面，不再是只有一份 schema 的哈希"
        );
        assert!(candidate["contract_carrier_hash"].is_string());
    }
    assert_ne!(
        candidates[0]["contract_carrier_hash"], candidates[1]["contract_carrier_hash"],
        "different carriers must produce different snapshot fingerprints"
    );

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 承载面 ⊆ 合同（R1）：供给不能凭空多出调用方可提交的字段。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn carrier_field_outside_the_contract_is_rejected() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let model = "contract-boundary-model";
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1}
    }));
    // 承载面多声明了 `quality`：合同里没有它，客户端按合同提交永远不会发这个名字。
    let carrier = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string", "enum": ["low", "high"]}
    }));
    let status = publish_with_surfaces(
        &client,
        &base_url,
        &admin_token,
        model,
        "contract-boundary-1",
        contract,
        vec![("aihubmix-image-v1", carrier)],
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a carrier field the contract does not declare must be rejected"
    );

    // 把 `quality` 补进合同后同一份承载面就能发布：拒绝的是那条边界，不是 `quality` 本身。
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string", "enum": ["low", "high"]}
    }));
    let carrier = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string", "enum": ["low", "high"]}
    }));
    let status = publish_with_surfaces(
        &client,
        &base_url,
        &admin_token,
        model,
        "contract-boundary-2",
        contract,
        vec![("aihubmix-image-v1", carrier)],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    drop_isolated_database(&database_name).await;
}

/// 承载面 ⊆ Driver 能写上线文的字段名（R2）：声明了发不出去的字段就拒绝。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn carrier_field_the_driver_cannot_write_is_rejected() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let model = "driver-boundary-model";
    // `resolution` 是 APIMart 那一侧的渠道字段名，AIHubMix 的 Driver 写不出去。
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "resolution": {"type": "string", "enum": ["1k", "2k"]}
    }));
    let status = publish_with_surfaces(
        &client,
        &base_url,
        &admin_token,
        model,
        "driver-boundary-1",
        contract.clone(),
        vec![("aihubmix-image-v1", contract.clone())],
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a carrier field the driver cannot write must be rejected"
    );

    // 同一份声明面挂到能写 `resolution` 的 Driver 上就能发布：判的是"发得出去吗"。
    let status = publish_with_surfaces(
        &client,
        &base_url,
        &admin_token,
        model,
        "driver-boundary-2",
        contract.clone(),
        vec![("apimart-image-v1", contract)],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    drop_isolated_database(&database_name).await;
}

/// 合同行不可变：同一 (vendor, model, revision) 重发幂等、不就地改写；
/// 内容不同的重发要拒绝；新修订才落新行。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn contract_rows_are_immutable_and_republishing_the_same_revision_is_idempotent() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    let model = "immutable-contract-model";
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1}
    }));
    let publish = |revision: &'static str, contract: Value| {
        let client = client.clone();
        let base_url = base_url.clone();
        let admin_token = admin_token.clone();
        async move {
            publish_with_surfaces(
                &client,
                &base_url,
                &admin_token,
                model,
                revision,
                contract.clone(),
                vec![("aihubmix-image-v1", contract)],
            )
            .await
        }
    };

    assert_eq!(
        publish("immutable-1", contract.clone()).await,
        StatusCode::OK
    );
    let first = contract_row(&pool, model, "immutable-1").await;
    // 同一修订重发（内容相同）：幂等——还是那一行，且**没有**被改写（时间戳与内容都不变）。
    assert_eq!(
        publish("immutable-1", contract.clone()).await,
        StatusCode::OK
    );
    let again = contract_row(&pool, model, "immutable-1").await;
    assert_eq!(again.0, first.0, "a republish must not create a second row");
    assert_eq!(
        again.1, first.1,
        "a republish must not rewrite the contract"
    );
    assert_eq!(
        again.2, first.2,
        "a republish must not touch the row at all"
    );

    // 同一修订换个合同：拒绝。合同落库后不可改，改合同要发新修订。
    let changed = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string"}
    }));
    assert_eq!(
        publish("immutable-1", changed).await,
        StatusCode::BAD_REQUEST,
        "the same revision must not accept a different contract"
    );
    let after = contract_row(&pool, model, "immutable-1").await;
    assert_eq!(
        after, first,
        "a rejected republish must leave the row untouched"
    );

    // 新修订落新行：同一个模型可以有多版合同，但每一版只有一份。
    assert_eq!(
        publish("immutable-2", contract.clone()).await,
        StatusCode::OK
    );
    let revisions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM catalog.vendor_models WHERE native_model_id = $1")
            .bind(model)
            .fetch_one(&pool)
            .await
            .expect("contract revision count");
    assert_eq!(revisions, 2, "a new revision is a new contract row");

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 显式默认值的每个键必须被这条供给承载：声明了一个发不出去的默认值，发布被拒。
///
/// 与"承载面 ⊆ 合同 ⊆ Driver 能写上线文的名字"同一条道理——声明了却做不到，就是声明与行为分了家。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn defaults_the_carrier_cannot_carry_are_rejected_at_publication() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let model = "defaults-boundary-model";
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string", "enum": ["low", "high"]}
    }));
    // 承载面承载不了 `quality`：这条默认值永远不会生效。
    let narrow = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1}
    }));
    // 同一份承载面、不带默认值时照常发布：拒绝的是那条默认值，不是承载面本身。
    let status = publish_with_mappings(
        &client,
        &base_url,
        &admin_token,
        model,
        "defaults-0",
        contract.clone(),
        vec![("aihubmix-image-v1", narrow.clone(), json!({}))],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let status = publish_with_mappings(
        &client,
        &base_url,
        &admin_token,
        model,
        "defaults-1",
        contract.clone(),
        vec![(
            "aihubmix-image-v1",
            narrow.clone(),
            json!({"defaults": {"quality": "low"}}),
        )],
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a default the carrier cannot carry must be rejected"
    );

    // 承载面声明了它：同一份默认值照常发布。
    let wide = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string", "enum": ["low", "high"]}
    }));
    let status = publish_with_mappings(
        &client,
        &base_url,
        &admin_token,
        model,
        "defaults-2",
        contract.clone(),
        vec![(
            "aihubmix-image-v1",
            wide,
            json!({"defaults": {"quality": "low"}}),
        )],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    drop_isolated_database(&database_name).await;
}

/// 对客目录：`GET /v1/models` 只列**当前真的能调**的型号，合同就是发布的那一份。
///
/// 目录**公开**：不带任何鉴权头就能取，乱给的 Key 也不会把它变成 401——调用方要先知道有哪些
/// 型号、各自的参数面，才建得出表单。
/// 判据与受理期选路**同一条**（生效的发布条目 + 启用的供给 + 启用的渠道）：目录里列出的型号
/// 必须真的提交得起来。列着却提交不了比不列更糟——调用方会照它建表单，然后在提交时落空。
/// 本用例只读发布物与目录，不起 Worker、不连上游：零外部调用。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_model_catalog_lists_only_callable_models_with_their_published_contract() {
    let (database_url, database_name) = isolated_database_url().await;
    // 同步入口在这个用例里只用来验"停用之后真的调不了"；那一步在受理前就失败，不会等超时。
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;
    let account = create_account(&client, &base_url, &admin_token).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    // ── 目录公开：不带任何鉴权头也 200；乱给的 Key 同样不影响它 ──
    let (status, catalog) = get_catalog(&client, &base_url, None).await;
    assert_eq!(status, StatusCode::OK, "公开目录不该要 Key：{catalog}");
    assert_public_only("无鉴权头的目录请求", &catalog);
    assert_eq!(
        catalog,
        json!({"data": []}),
        "还没发布任何型号时，目录是空列表而不是错误：{catalog}"
    );
    let (status, catalog) = get_catalog(&client, &base_url, Some("sk_seeai_not_a_real_key")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "目录根本不读 Authorization，乱给的 Key 也不该被拒：{catalog}"
    );

    // 两个型号、两份不同的合同：目录里每一条都必须带**它自己**那份，且逐字一致。
    let model = "catalog-model-a";
    let other = "catalog-model-b";
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1}
    }));
    let other_contract = surface_schema(json!({
        "model": {"const": other},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string", "enum": ["low", "high"]}
    }));
    let published = [
        (model, "catalog-a-1", &contract),
        (other, "catalog-b-1", &other_contract),
    ];
    for (name, revision, schema) in published {
        let status = publish_with_surfaces(
            &client,
            &base_url,
            &admin_token,
            name,
            revision,
            schema.clone(),
            vec![("aihubmix-image-v1", schema.clone())],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{name} 必须发布成功");
    }

    // ── 两个型号都在，形状是 `{name, vendor_id, revision, contract}`；照旧不带鉴权头 ──
    let (status, catalog) = get_catalog(&client, &base_url, None).await;
    assert_eq!(status, StatusCode::OK, "got {catalog}");
    assert_public_only("目录", &catalog);
    assert_eq!(
        catalog.as_object().map(serde_json::Map::len),
        Some(1),
        "目录顶层只有 data：{catalog}"
    );
    let entries = catalog["data"].as_array().expect("data is an array");
    assert_eq!(entries.len(), 2, "两个在售型号都要在目录里：{catalog}");
    for (name, revision, schema) in published {
        let entry = entries
            .iter()
            .find(|entry| entry["name"].as_str() == Some(name))
            .unwrap_or_else(|| panic!("`{name}` 必须在目录里：{catalog}"));
        let mut keys: Vec<&str> = entry
            .as_object()
            .expect("entry is an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["contract", "name", "revision", "vendor_id"],
            "目录条目只有 name / vendor_id / revision / contract 四个字段：{entry}"
        );
        assert_eq!(entry["vendor_id"].as_str(), Some("OpenAI"));
        assert_eq!(entry["revision"].as_str(), Some(revision));
        // 厂商原生名不进对客面：这两个型号的对客名恰好等于原生名，因此这里只钉住"响应里
        // 没有 native_model_id 这个**字段**"；名字不同时"正文不含原生名"由专门的用例钉。
        assert!(
            entry.get("native_model_id").is_none(),
            "对客目录不许出现 native_model_id：{entry}"
        );
        assert_eq!(
            &entry["contract"], schema,
            "目录里的合同必须与发布的那一份逐字一致（对客名与原生名同值时逐字相同）"
        );
    }

    // ── 停用供给：该型号从目录里消失，提交也确实取不到候选 ──
    sqlx::query(
        "UPDATE supply.offerings SET enabled = false
         WHERE vendor_model_id = (SELECT id FROM catalog.vendor_models WHERE native_model_id = $1)",
    )
    .bind(model)
    .execute(&pool)
    .await
    .expect("disable the offering");
    let (_, catalog) = get_catalog(&client, &base_url, None).await;
    let names: Vec<&str> = catalog["data"]
        .as_array()
        .expect("data is an array")
        .iter()
        .filter_map(|entry| entry["name"].as_str())
        .collect();
    assert_eq!(names, vec![other], "停用的型号必须从目录里消失：{catalog}");
    let key = format!("catalog-disabled-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request(model, "disabled model"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "目录不列的型号，受理期同样取不到候选：{body}"
    );

    // ── 渠道停用与供给停用是**同一条**判据：它同样让型号从目录里消失 ──
    sqlx::query(
        "UPDATE supply.channels SET enabled = false
         WHERE id = (SELECT o.channel_id FROM supply.offerings o
                     JOIN catalog.vendor_models vm ON vm.id = o.vendor_model_id
                     WHERE vm.native_model_id = $1)",
    )
    .bind(other)
    .execute(&pool)
    .await
    .expect("disable the channel");
    let (status, catalog) = get_catalog(&client, &base_url, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        catalog,
        json!({"data": []}),
        "供给与渠道全停用后，目录是空列表而不是错误：{catalog}"
    );

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 命名层：对客面只出现**平台网关模型名**，厂商原生名留在管理端。
///
/// 素材刻意让两个名字不同（厂商 `gpt-image-2.5-sunburst`、对客 `gpt-image-2.5-plus`），
/// 一次把命名层的几条硬约束都钉住：
/// - 素材把**对客名**写进合同正文会被发布期拒掉（合同正文那个常量是厂商模型的身份）；
/// - 目录的 `name` 是对客名、带 `vendor_id`、**正文全文不含**厂商原生名（含合同正文）；
/// - 用对客名能真的受理（假上游跑通），用厂商原生名是"模型不存在"；
/// - 存的那份合同不动：库里 `model.const` 仍是厂商原生名，只有投射给调用方时替换；
/// - 运维开关一关，目录与受理**同时**消失，管理端照样列得出来（否则关了就没法打开）。
///
/// 零外部调用：假上游在进程内，凭证只从环境变量读。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn gateway_model_naming_keeps_the_vendor_name_off_the_consumer_surface() {
    const NATIVE: &str = "gpt-image-2.5-sunburst";
    const GATEWAY: &str = "gpt-image-2.5-plus";

    let (database_url, database_name) = isolated_database_url().await;
    let client = Client::new();
    let calls: UpstreamCalls = Arc::new(Mutex::new(Vec::new()));
    let upstream = start_fake_upstream_with(
        calls.clone(),
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
    )
    .await;
    let (base_url, admin_token, _process) = start_api(&database_url, 60, 64).await;
    wait_until_ready(&client, &base_url, &admin_token).await;
    // 素材带保底表，受理闸门是"余额 ≥ 保底额"：账户要付得起它，才看得到发布与受理本身的行为。
    let account = create_account_with_credit(&client, &base_url, &admin_token, 1_000_000).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");
    let material = renamed_material(&upstream.base_url);

    // ── 素材把**对客名**写进合同正文 → 发布期拒掉 ──
    // 这条同时是"响应全文不含原生名"可判定的前提：合同正文里没有第二个模型名来源。
    let mut misnamed = material.clone();
    misnamed["capability_schema"]["properties"]["model"]["const"] =
        Value::String(GATEWAY.to_owned());
    let rejected = client
        .post(format!("{base_url}/api/v1/runtime-revisions"))
        .bearer_auth(&admin_token)
        .json(&misnamed)
        .send()
        .await
        .expect("misnamed publication");
    assert_eq!(
        rejected.status(),
        StatusCode::BAD_REQUEST,
        "合同正文写对客名必须被拒：{:?}",
        rejected.text().await
    );

    // ── 正常发布：厂商原生名写进合同正文，对客名写在顶层 ──
    let published = client
        .post(format!("{base_url}/api/v1/runtime-revisions"))
        .bearer_auth(&admin_token)
        .json(&material)
        .send()
        .await
        .expect("publication request");
    let status = published.status();
    let body = published.text().await.expect("publication body");
    assert_eq!(status, StatusCode::OK, "素材必须能发布：{body}");

    // ── 对客目录：name 是对客名，带 vendor_id，正文全文不含厂商原生名 ──
    let raw = client
        .get(format!("{base_url}/v1/models"))
        .send()
        .await
        .expect("catalog request")
        .text()
        .await
        .expect("catalog text");
    assert!(
        !raw.contains(NATIVE),
        "对客目录正文不许出现厂商原生名（含合同正文）：{raw}"
    );
    let catalog: Value = serde_json::from_str(&raw).expect("catalog JSON");
    assert_eq!(
        catalog_names(&catalog),
        vec![GATEWAY.to_owned()],
        "{catalog}"
    );
    let entry = &catalog["data"][0];
    assert_eq!(entry["vendor_id"].as_str(), Some("OpenAI"));
    assert_eq!(entry["revision"], material["native_revision"]);
    assert!(
        entry.get("native_model_id").is_none(),
        "对客目录不许出现 native_model_id：{entry}"
    );
    assert_eq!(
        entry["contract"],
        consumer_contract(material["capability_schema"].clone(), GATEWAY),
        "目录里的合同是发布的那一份，只有 model.const 换成对客名"
    );
    assert_eq!(entry["contract"]["properties"]["model"]["const"], GATEWAY);

    // ── 存的那份合同**不动**：库里那个常量仍是厂商原生名 ──
    let stored_contract: Value = sqlx::query_scalar(
        "SELECT capability_schema FROM catalog.vendor_models WHERE native_model_id = $1",
    )
    .bind(NATIVE)
    .fetch_one(&pool)
    .await
    .expect("stored contract");
    assert_eq!(
        stored_contract["properties"]["model"]["const"], NATIVE,
        "合同行不可变：替换只发生在投射那一步"
    );
    let vendor_model_id: Uuid =
        sqlx::query_scalar("SELECT id FROM catalog.vendor_models WHERE native_model_id = $1")
            .bind(NATIVE)
            .fetch_one(&pool)
            .await
            .expect("vendor model id");

    // ── 管理员读：一条网关模型一项，带候选清单与运维开关 ──
    let (unauthorized, _) = get_gateway_models(&client, &base_url, None).await;
    assert_eq!(
        unauthorized,
        StatusCode::FORBIDDEN,
        "运营视图要管理员凭证，与公开的对客目录不是一回事"
    );
    let (status, admin) = get_gateway_models(&client, &base_url, Some(&admin_token)).await;
    assert_eq!(status, StatusCode::OK, "{admin}");
    assert_eq!(
        admin.as_object().map(serde_json::Map::len),
        Some(1),
        "管理端清单只有 gateway_models 一个顶层字段：{admin}"
    );
    let listed = admin["gateway_models"]
        .as_array()
        .expect("gateway_models is an array");
    assert_eq!(listed.len(), 1, "{admin}");
    let view = &listed[0];
    assert_eq!(view["gateway_model"], GATEWAY);
    assert_eq!(view["enabled"], true);
    assert_eq!(view["vendor_id"], "OpenAI");
    assert_eq!(view["native_model_id"], NATIVE, "厂商原生名只在管理端出现");
    assert_eq!(view["native_revision"], material["native_revision"]);
    assert!(
        view["runtime_revision_id"].as_str().is_some(),
        "要能看出这是哪一次发布：{view}"
    );
    assert!(
        view["published_at"].as_str().is_some(),
        "要能看出这次发布是什么时候发的：{view}"
    );
    let candidates = view["candidates"].as_array().expect("candidates");
    assert_eq!(candidates.len(), 1, "一条候选：{view}");
    assert_eq!(candidates[0]["provider_kind"], "AIHubMix");
    assert_eq!(candidates[0]["provider_model_id"], NATIVE);
    assert_eq!(candidates[0]["adapter_key"], "aihubmix-image-v1");
    assert_eq!(candidates[0]["routing_priority"], 0);
    assert_eq!(candidates[0]["enabled"], true);
    assert!(
        candidates[0]["offering_id"].as_str().is_some(),
        "候选要能被指认：{view}"
    );
    assert!(
        candidates[0]["carrier_schema"].is_object()
            && candidates[0]["parameter_mapping"].is_object(),
        "候选自带承载面与映射，不必直查库：{view}"
    );
    assert!(
        candidates[0].get("credential_env").is_none(),
        "不回显渠道凭证：{view}"
    );

    // ── 用**对客名**受理：假上游真跑通；上行给渠道的仍是厂商原生名 ──
    let _worker = spawn_worker_process(&database_url);
    let key = format!("naming-gateway-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request(GATEWAY, "命名层：按对客名受理"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "对客名必须真的能受理：{body}");
    assert_sync_success("按对客名受理", &body);
    assert_eq!(
        count_calls(&calls, "POST", "/v1/images/generations"),
        1,
        "请求要真的发到假上游"
    );
    let submit = last_submit_body(&calls, "/v1/images/generations");
    assert_eq!(
        submit["model"], NATIVE,
        "上行给渠道的是厂商原生名，不是对客名：{submit}"
    );
    let stored_model: String =
        sqlx::query_scalar("SELECT gateway_model FROM generation.jobs WHERE idempotency_key = $1")
            .bind(&key)
            .fetch_one(&pool)
            .await
            .expect("job gateway model");
    assert_eq!(stored_model, GATEWAY, "Job 固化的是对客名");
    let (revision_gateway, revision_vendor_model): (String, Uuid) = {
        let row = sqlx::query(
            "SELECT gateway_model, vendor_model_id FROM publication.runtime_revisions
             WHERE id = (SELECT runtime_revision_id FROM publication.runtime_entries
                         WHERE active AND gateway_model = $1 LIMIT 1)",
        )
        .bind(GATEWAY)
        .fetch_one(&pool)
        .await
        .expect("runtime revision naming columns");
        (
            row.try_get("gateway_model").expect("gateway model"),
            row.try_get("vendor_model_id").expect("vendor model id"),
        )
    };
    assert_eq!(revision_gateway, GATEWAY, "修订上记的是对客名");
    assert_eq!(
        revision_vendor_model, vendor_model_id,
        "修订指向它挂的那行合同"
    );

    // ── 用**厂商原生名**受理：模型不存在 ──
    let native_key = format!("naming-native-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &native_key,
        &route_request(NATIVE, "命名层：按厂商原生名受理"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "厂商原生名不是对客身份，受理期取不到候选：{body}"
    );
    assert_eq!(
        count_calls(&calls, "POST", "/v1/images/generations"),
        1,
        "被拒的请求不该发到上游"
    );

    // ── 运维开关：关掉之后目录与受理同时消失，管理端照样列得出来 ──
    assert_eq!(
        patch_gateway_model(&client, &base_url, &admin_token, GATEWAY, false).await,
        StatusCode::NO_CONTENT
    );
    let (_, catalog) = get_catalog(&client, &base_url, None).await;
    assert_eq!(
        catalog,
        json!({"data": []}),
        "关掉的模型从目录里消失：{catalog}"
    );
    let off_key = format!("naming-off-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &off_key,
        &route_request(GATEWAY, "命名层：关掉之后受理"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "关掉的模型受理得到'模型不存在'：{body}"
    );
    let (_, admin) = get_gateway_models(&client, &base_url, Some(&admin_token)).await;
    assert_eq!(
        admin["gateway_models"][0]["gateway_model"], GATEWAY,
        "关掉的模型照样列得出来，否则关了就没法打开：{admin}"
    );
    assert_eq!(admin["gateway_models"][0]["enabled"], false);
    let audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operations.audit_events
         WHERE action = 'gateway_model.set_enabled' AND subject_id = $1",
    )
    .bind(GATEWAY)
    .fetch_one(&pool)
    .await
    .expect("audit events");
    assert_eq!(audits, 1, "PATCH 要写出一条审计事件");

    // ── 重新启用：目录与受理都恢复 ──
    assert_eq!(
        patch_gateway_model(&client, &base_url, &admin_token, GATEWAY, true).await,
        StatusCode::NO_CONTENT
    );
    let (_, catalog) = get_catalog(&client, &base_url, None).await;
    assert_eq!(
        catalog_names(&catalog),
        vec![GATEWAY.to_owned()],
        "重新启用后回到目录：{catalog}"
    );
    let on_key = format!("naming-on-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &on_key,
        &route_request(GATEWAY, "命名层：重新启用之后受理"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "重新启用后必须能受理：{body}");
    assert_sync_success("重新启用后受理", &body);

    // ── 没发布过的名字：404，而且不留下任何审计事件 ──
    let unknown = format!("never-published-{}", Uuid::new_v4());
    assert_eq!(
        patch_gateway_model(&client, &base_url, &admin_token, &unknown, false).await,
        StatusCode::NOT_FOUND,
        "没发布过的名字是'不存在'，不是'待创建'"
    );
    let unknown_audits: i64 =
        sqlx::query_scalar("SELECT count(*) FROM operations.audit_events WHERE subject_id = $1")
            .bind(&unknown)
            .fetch_one(&pool)
            .await
            .expect("audit events");
    assert_eq!(unknown_audits, 0, "被拒的 PATCH 不该留下审计事件");

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// **不带对客名**的发布命令照常可发布：对客名回退取厂商原生名，行为与今天逐位一致。
///
/// 这是命名层"缺省回退"那条兼容承诺的证据：命令不带 `gateway_model` 时，目录里的 `name`
/// 就是厂商原生名，受理也照旧跑得通。素材在测试内构造（顶层一份合同 + 候选自带承载面），
/// 刻意不带对客名。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn legacy_material_without_a_gateway_name_falls_back_to_the_vendor_name() {
    let (database_url, database_name) = isolated_database_url().await;
    let client = Client::new();
    let calls: UpstreamCalls = Arc::new(Mutex::new(Vec::new()));
    let upstream = start_fake_upstream_with(
        calls.clone(),
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
    )
    .await;
    let (base_url, admin_token, _process) = start_api(&database_url, 60, 64).await;
    wait_until_ready(&client, &base_url, &admin_token).await;
    let account = create_account(&client, &base_url, &admin_token).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    let contract = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": {
            "model": { "const": "gpt-image-2" },
            "prompt": { "type": "string", "minLength": 1 },
            "image": { "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": 16 },
            "mask": { "type": "string" },
            "n": { "type": "integer", "minimum": 1, "maximum": 10, "default": 1 },
            "size": { "type": "string", "anyOf": [{ "const": "auto" }, { "pattern": "^[0-9]+x[0-9]+$" }] },
            "output_format": { "type": "string", "enum": ["png", "jpeg"], "default": "png" },
            "quality": { "type": "string", "enum": ["low", "medium", "high"] },
            "output_compression": { "type": "integer", "minimum": 0, "maximum": 100, "default": 100 },
            "background": { "type": "string", "enum": ["auto", "opaque", "transparent"], "default": "auto" },
            "moderation": { "type": "string", "enum": ["auto", "low"], "default": "auto" }
        },
        "allOf": [
            { "if": { "required": ["mask"] }, "then": { "required": ["image"] } }
        ]
    });
    let command = json!({
        "vendor_id": "OpenAI",
        "native_model_id": "gpt-image-2",
        "native_revision": "2026-09-18-validated-1.3",
        "actor": "bootstrap",
        "capability_schema": contract.clone(),
        "offerings": [{
            "provider_kind": "AIHubMix",
            "adapter_key": "aihubmix-image-v1",
            "provider_model_id": "gpt-image-2",
            "base_url": upstream.base_url.clone(),
            "credential_env": "AIHUBMIX_API_KEY",
            "restrictions": {
                "allowed_branches": ["prompt_only", "image_conditioned", "masked"],
                "max_reference_images": 16
            },
            "carrier_schema": contract.clone(),
            "formula": "token_rates",
            "price_plan": {
                "currency": "USD",
                "text_input_microusd_per_million": 5000000,
                "image_input_microusd_per_million": 8000000,
                "text_output_microusd_per_million": 10000000,
                "image_output_microusd_per_million": 30000000,
                "source_url": "https://aihubmix.com/model/gpt-image-2"
            }
        }]
    });
    assert!(
        command.get("gateway_model").is_none(),
        "这份构造刻意不带对客名，缺省回退才有的可验"
    );
    let response = client
        .post(format!("{base_url}/api/v1/runtime-revisions"))
        .bearer_auth(&admin_token)
        .json(&command)
        .send()
        .await
        .expect("publication request");
    let status = response.status();
    let body = response.text().await.expect("publication body");
    assert_eq!(
        status,
        StatusCode::OK,
        "不带对客名的命令必须照常可发布：{body}"
    );

    // ── 目录逐位一致：name 是厂商原生名，形状就是新的四字段形状 ──
    let (status, catalog) = get_catalog(&client, &base_url, None).await;
    assert_eq!(status, StatusCode::OK, "{catalog}");
    assert_eq!(
        catalog,
        json!({"data": [{
            "name": "gpt-image-2",
            "vendor_id": "OpenAI",
            "revision": "2026-09-18-validated-1.3",
            "contract": contract,
        }]}),
        "缺省回退之后目录与今天逐位一致：{catalog}"
    );

    // ── 受理行为同样照旧：按回退出来的名字跑通 ──
    let _worker = spawn_worker_process(&database_url);
    let key = format!("naming-fallback-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request("gpt-image-2", "不带对客名：缺省回退"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "不带对客名的命令必须照常受理：{body}"
    );
    assert_sync_success("不带对客名的命令受理", &body);

    // ── 落库事实：修订上的对客名就是回退出来的厂商原生名，开关行也在（默认启用）──
    let revision_gateway: String = sqlx::query_scalar(
        "SELECT rr.gateway_model FROM publication.runtime_revisions rr
         JOIN publication.runtime_entries re ON re.runtime_revision_id = rr.id
         WHERE re.active AND re.gateway_model = 'gpt-image-2' LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .expect("runtime revision naming column");
    assert_eq!(revision_gateway, "gpt-image-2");
    let enabled: bool = sqlx::query_scalar(
        "SELECT enabled FROM publication.gateway_models WHERE gateway_model = $1",
    )
    .bind("gpt-image-2")
    .fetch_one(&pool)
    .await
    .expect("gateway model switch");
    assert!(enabled, "首次发布成功时落一行开关，默认开着");

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 发布命令只有"候选数组"这一种形状：不带 `offerings` 的请求在发布期被拒（400），
/// 不是"发布成功但没有候选"，也不落任何行。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn publication_without_the_offering_array_is_rejected() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    let response = client
        .post(format!("{base_url}/api/v1/runtime-revisions"))
        .bearer_auth(&admin_token)
        .json(&json!({
            "vendor_id": "OpenAI",
            "native_model_id": "no-offerings-model",
            "native_revision": "no-offerings-1",
            "actor": "contract-test",
            "capability_schema": {"type": "object"}
        }))
        .send()
        .await
        .expect("publication request");
    let status = response.status();
    let body = response.text().await.expect("publication body");
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "没有候选集合必须在发布期被拒：{body}"
    );
    assert!(
        body.contains("offerings is required"),
        "错误要说清缺什么：{body}"
    );
    let revisions: i64 = sqlx::query_scalar("SELECT count(*) FROM publication.runtime_revisions")
        .fetch_one(&pool)
        .await
        .expect("runtime revisions");
    assert_eq!(revisions, 0, "被拒的发布不落任何行");

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// **汇率按受理时刻生效的那一行取值**，且**没有折算率的币种在发布期被拒**。
///
/// 未来生效的一行是调价预告：受理时该用的仍是受理时刻之前已生效的那一行。受理之后再录一行
/// 也不动已受理 Job 的折算——快照已经把它冻住了。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_rate_effective_at_acceptance_is_frozen_and_a_missing_rate_blocks_publication() {
    let harness = Harness::start_with_draft(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let client = Client::new();

    // 调价预告：未来生效的一行不参与受理时的取值。
    let future = chrono::Utc::now() + chrono::Duration::days(1);
    let response = client
        .put(format!("{}/api/v1/fx-rates", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&json!({
            "currency": "USD",
            "rate_micros": 9_000_000u64,
            "effective_at": future.to_rfc3339(),
        }))
        .send()
        .await
        .expect("future fx rate");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // 没有折算率的币种：发布期拒绝，整份发布不落任何行。
    let mut draft = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    draft["base_url"] = Value::String(harness.upstream_base_url.clone());
    draft["price_plan"]["currency"] = json!("EUR");
    assert_eq!(
        publish_candidates(
            &client,
            &harness.base_url,
            &harness.admin_token,
            "eur-model",
            None,
            vec![draft]
        )
        .await,
        StatusCode::BAD_REQUEST,
        "该币种没有折算率就必须在发布期被拒"
    );
    let revisions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM publication.runtime_revisions WHERE gateway_model = 'eur-model'",
    )
    .fetch_one(&harness.pool)
    .await
    .expect("revisions");
    assert_eq!(revisions, 0, "被拒的发布不落任何行");

    assert_eq!(
        republish_priced(
            &harness,
            &client,
            openai_floor_amounts(),
            priced_consumer_rates(),
            2_000
        )
        .await,
        StatusCode::OK
    );
    let (_, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let _worker = harness.spawn_worker();
    let key = format!("fx-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request(harness.model, "fx rate"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let snapshot = frozen_snapshot(&harness.pool, &key).await;
    assert_eq!(
        snapshot["fx_rate"]["rate_micros"],
        json!(7_100_000),
        "取的是受理时刻生效的那一行，不是未来那一行"
    );
    let (job_id, _, _) = harness.job(&key).await;
    assert_eq!(harness.attempt_cost(job_id).await.3, Some(42_245));

    // 受理之后再录一行（立即生效）：已受理 Job 的折算用的是冻结的那个数。
    let response = client
        .put(format!("{}/api/v1/fx-rates", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"currency": "USD", "rate_micros": 5_000_000u64}))
        .send()
        .await
        .expect("later fx rate");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        frozen_snapshot(&harness.pool, &key).await["fx_rate"]["rate_micros"],
        json!(7_100_000),
        "快照已经冻住了受理当时那一行"
    );
    assert_eq!(
        harness.attempt_cost(job_id).await.3,
        Some(42_245),
        "已受理 Job 的成本折算不变"
    );

    harness.cleanup().await;
}

/// 发布期校验**计价形态**：形态必填、取值受控，而且**形态与参数配套**。
///
/// 不配套的三条都要拒并说清缺什么；反过来，"按张计价**不带**那份四档费率"是合法的——Price Plan
/// 只是"按 token 计量量计价"这一种形态的参数，不是每条供给都有的东西。但按张 / 按次 / 上游给金额
/// 的候选的对客价是"成本单价 × 倍率 × 折算率"，所以**倍率必须给**，而那份对客四档向量在它们身上
/// 永远不会被读、给了就拒。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn publication_requires_a_pricing_formula_that_matches_its_parameters() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");
    let model = "formula-model";

    async fn publish(
        client: &Client,
        base_url: &str,
        admin_token: &str,
        model: &str,
        offerings: Vec<Value>,
        markup_bps: Option<i32>,
    ) -> (StatusCode, Value) {
        let body = publication_body(model, "route-test-1", None, offerings, markup_bps);
        let response = client
            .post(format!("{base_url}/api/v1/runtime-revisions"))
            .bearer_auth(admin_token)
            .json(&body)
            .send()
            .await
            .expect("runtime publication");
        let status = response.status();
        let body = response.json().await.unwrap_or(Value::Null);
        (status, body)
    }
    let message = |body: &Value| {
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    };

    // 1) 缺形态：说不清一条供给按什么计价，它的成本就没有算法。
    let mut without_formula = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    without_formula["formula"] = Value::Null;
    let (status, body) = publish(
        &client,
        &base_url,
        &admin_token,
        model,
        vec![without_formula],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert!(message(&body).contains("formula is required"), "{body}");

    // 2) 取值受控：认不出的形态一样拒。
    let mut unknown_formula = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    unknown_formula["formula"] = json!("by_the_hour");
    let (status, body) = publish(
        &client,
        &base_url,
        &admin_token,
        model,
        vec![unknown_formula],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert!(message(&body).contains("must be token_rates"), "{body}");

    // 3) 按 token 计量量计价却没有那份四档费率。
    let mut token_without_plan = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    token_without_plan["price_plan"] = Value::Null;
    let (status, body) = publish(
        &client,
        &base_url,
        &admin_token,
        model,
        vec![token_without_plan],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert!(message(&body).contains("price_plan is required"), "{body}");

    // 4) 反向也要拒：形态用不到的参数永远不会被读，留着只会让人以为它在生效。
    let mut declared_with_plan = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    declared_with_plan["formula"] = json!("upstream_declared");
    let (status, body) = publish(
        &client,
        &base_url,
        &admin_token,
        model,
        vec![declared_with_plan],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert!(
        message(&body).contains("does not apply to formula"),
        "{body}"
    );

    // 5) 按张计价**不带**费率表也能发布：参数是单价，不是那份四档费率。
    let mut per_image = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    per_image["formula"] = json!("per_image");
    per_image["price_plan"] = Value::Null;
    per_image["cost_unit_price_microusd"] = json!(11_354);
    per_image["cost_currency"] = json!("USD");
    // 6) 对客价是"成本单价 × 倍率 × 折算率"：缺了倍率就算不出该收多少钱，发布期就拒——
    //    "不带价目表也能发布"不等于"连对客价一起没有"，那样结算只能按 0 收（等于白送）。
    let (status, body) = publish(
        &client,
        &base_url,
        &admin_token,
        model,
        vec![per_image.clone()],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert!(message(&body).contains("markup_bps is required"), "{body}");

    // 7) 给它倍率（没有价目表也可以）：发布成功，受理照常、快照冻结形态与单价。
    let (status, body) = publish(
        &client,
        &base_url,
        &admin_token,
        model,
        vec![per_image.clone()],
        Some(2_000),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "有对客计费基准、没有价目表照样发布：{body}"
    );

    // 8) 按张计价再带一份对客四档向量也要拒：那份向量是 token 计量量的价格，在别的形态下
    //    永远不会被读——留着它只会让人以为它在生效。
    let mut per_image_with_rates = per_image.clone();
    per_image_with_rates["consumer_rates_cny"] = json!({
        "text_input_micros_per_million": 35_500_000u64,
        "image_input_micros_per_million": 56_800_000u64,
        "text_output_micros_per_million": 71_000_000u64,
        "image_output_micros_per_million": 213_000_000u64
    });
    let (status, body) = publish(
        &client,
        &base_url,
        &admin_token,
        model,
        vec![per_image_with_rates],
        Some(2_000),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert!(
        message(&body).contains("consumer_rates_cny does not apply to formula"),
        "{body}"
    );

    // 落库：形态与单价在供给行上，价目行为 0、条目上的 Price Plan 为空。
    let row = sqlx::query(
        "SELECT o.formula, o.cost_unit_price_microusd, re.price_plan_id,
                (SELECT count(*) FROM pricing.price_plans p WHERE p.offering_id = o.id) AS plans
         FROM publication.runtime_entries re
         JOIN supply.offerings o ON o.id = re.offering_id
         WHERE re.active AND re.gateway_model = $1",
    )
    .bind(model)
    .fetch_one(&pool)
    .await
    .expect("the published candidate");
    assert_eq!(
        row.try_get::<String, _>("formula").expect("formula"),
        "per_image"
    );
    assert_eq!(
        row.try_get::<Option<i64>, _>("cost_unit_price_microusd")
            .expect("unit price"),
        Some(11_354)
    );
    assert!(
        row.try_get::<Option<Uuid>, _>("price_plan_id")
            .expect("price plan id")
            .is_none()
    );
    assert_eq!(row.try_get::<i64, _>("plans").expect("plans"), 0);

    // 受理照常，且形态与单价随 Job 快照冻结。这里**不起 Worker**：受理本身就把快照冻好了，
    // 等不到终态只是这次请求超时回错——要看的是快照。
    let account_id = create_account(&client, &base_url, &admin_token).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account_id).await;
    let key = format!("per-image-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request(model, "priced per image"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "没有 Worker 时这次请求超时（受理已经发生）：{body}"
    );
    let snapshot = frozen_snapshot(&pool, &key).await;
    assert_eq!(snapshot["formula"], json!("per_image"));
    assert_eq!(snapshot["cost_unit_price_microusd"], json!(11_354));
    assert_eq!(snapshot["cost_currency"], json!("USD"));
    assert!(
        snapshot["fx_rate"].is_object(),
        "声明了成本币种就把折算率冻结下来（毛利要用它）：{snapshot}"
    );

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 发布按**身份**复用渠道与供给行：手工停用的状态不会被下一次发布顶掉。
///
/// 老形状的发布每次都给候选新插一行渠道与一行供给、并写死 `enabled = true`——手工停用因此在
/// 下一次发布时无声消失。这里按"停用 → 重发该模型的其它变动 → 仍然停用"两个方向各验一次
/// （渠道停用、供给停用），顺带钉住"重发没有多插一行"。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn republishing_a_model_keeps_the_disabled_supply_disabled() {
    let harness = Harness::start(UpstreamBehaviour::aihubmix(SyncImageShape::Url)).await;
    let client = Client::new();
    let (offering, channel) = active_supply(&harness).await;
    let channels_before: i64 = sqlx::query_scalar("SELECT count(*) FROM supply.channels")
        .fetch_one(&harness.pool)
        .await
        .expect("channel count");

    // 重发用的草案与夹具发布的那条逐字同身份，只改一处**可变量**（档位）——这正是
    // "同一模型的其它变动"：渠道身份没变，因此必须复用同一行。
    let republish = |priority: i32| {
        let mut draft = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
        draft["base_url"] = json!(harness.upstream_base_url);
        draft["routing_priority"] = json!(priority);
        draft
    };
    // 重发必须落在**同一个 vendor model** 上，所以合同要与入库那份**逐字相同**（合同不可变）。
    // 这里直接**把库里存的那份读回来**当入参：如果这样仍被判"合同不同"，那就是入库时的归一化
    // 与比较用的原始入参不对称——问题在代码的校验，而不是用例的构造。
    let bootstrap_contract: Value = sqlx::query_scalar(
        "SELECT capability_schema FROM catalog.vendor_models \
         WHERE native_model_id = $1 AND native_revision = $2",
    )
    .bind("driver-model")
    .bind("route-test-1")
    .fetch_one(&harness.pool)
    .await
    .expect("the bootstrapped vendor model contract");
    let publish = async |draft: Value| {
        let response = client
            .post(format!("{}/api/v1/runtime-revisions", harness.base_url))
            .bearer_auth(&harness.admin_token)
            .json(&publication_body(
                harness.model,
                "route-test-1",
                Some(bootstrap_contract.clone()),
                vec![draft],
                None,
            ))
            .send()
            .await
            .expect("runtime publication");
        let status = response.status();
        let raw = response.text().await.expect("publication body");
        (status, raw)
    };

    // ── 停用渠道 → 重发 ──
    assert_eq!(
        patch_channel(
            &client,
            &harness.base_url,
            &harness.admin_token,
            channel,
            false
        )
        .await,
        StatusCode::NO_CONTENT
    );
    let (status, body) = publish(republish(2)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "重发同一模型的其它变动必须成功：{body}"
    );

    let channels_after: i64 = sqlx::query_scalar("SELECT count(*) FROM supply.channels")
        .fetch_one(&harness.pool)
        .await
        .expect("channel count");
    assert_eq!(
        channels_after, channels_before,
        "同一个调用入口与凭证身份只该有一行：重发必须复用它，不是再插一行"
    );
    let channel_enabled: bool =
        sqlx::query_scalar("SELECT enabled FROM supply.channels WHERE id = $1")
            .bind(channel)
            .fetch_one(&harness.pool)
            .await
            .expect("the reused channel row");
    assert!(
        !channel_enabled,
        "重发不得把手工停用的渠道顶回启用——那正是这条切片要修掉的缺陷"
    );
    assert_eq!(
        active_supply(&harness).await,
        (offering, channel),
        "生效条目仍指向同一行供给与渠道"
    );
    let off_key = format!("supply-channel-off-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &off_key,
        &route_request(harness.model, "channel stays off"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "渠道停着，重发之后这个型号仍然取不到候选：{body}"
    );

    // ── 反过来：渠道重新启用、改停**供给** → 再重发 ──
    assert_eq!(
        patch_channel(
            &client,
            &harness.base_url,
            &harness.admin_token,
            channel,
            true
        )
        .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        patch_offering(
            &client,
            &harness.base_url,
            &harness.admin_token,
            offering,
            false
        )
        .await,
        StatusCode::NO_CONTENT
    );
    let (status, body) = publish(republish(3)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let offering_enabled: bool =
        sqlx::query_scalar("SELECT enabled FROM supply.offerings WHERE id = $1")
            .bind(offering)
            .fetch_one(&harness.pool)
            .await
            .expect("the reused offering row");
    assert!(!offering_enabled, "供给的停用状态同样不得被重发顶回启用");
    assert_eq!(
        active_supply(&harness).await,
        (offering, channel),
        "供给行也按身份复用：重发之后生效条目仍指向原来那一行"
    );

    harness.cleanup().await;
}

/// **已受理但还没执行**的 Job 仍按受理时那一套**执行事实**跑：适配器、渠道模型、入口地址与凭证名。
///
/// 为什么单独验这一条：受理与执行之间有两处会动到这些值——发布按身份复用供给行、重发**就地改写**
/// 那一行；渠道行的入口与凭证名虽没有应用内的写入方，却也能被一次直接改库（迁移或运维脚本）换掉。
/// Worker 执行时若现场读这两张表，一台已受理、还没被领走的 Job 就会用上后来改的适配器与渠道模型、
/// 或者带着受理时的渠道模型打到另一个入口、按新变量名去取凭证。它要交给哪个驱动、往线文里写哪个
/// 渠道模型名、打在哪个入口、用哪个变量名取凭证，在受理那一刻就随 Job 定下了。所以这里刻意让 Job
/// 停在"已受理"：先重发供给改掉适配器与渠道模型，再直接改库换掉渠道行的入口与凭证名，然后才放
/// Worker 出去，并钉住这次执行**仍落在受理时那个入口上**。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_accepted_job_keeps_the_execution_facts_of_its_acceptance() {
    // Worker 是独立包，`cargo test -p seeai-api` 不会顺带建它，第一次起 Worker 会现场编译。
    // 这次执行要在受理之后才起 Worker，编译时间会吃掉同步入口的等待窗口，所以先把二进制建好。
    let _ = worker_binary();

    // 候选只声明文生图：承载面在这些名字上对两家适配器都成立，于是"换适配器"不被承载面差异
    // 挡住，换的确实是适配器本身。
    let harness = Harness::start_with_draft(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let client = Client::new();
    let (offering, channel) = active_supply(&harness).await;

    // 同步入口受理之后会一直等到终态。这里先不放 Worker，让它停在"已受理"。
    let key = format!("frozen-supply-{}", Uuid::new_v4());
    let in_flight = tokio::spawn({
        let base_url = harness.base_url.clone();
        let api_key = harness.api_key.clone();
        let key = key.clone();
        let body = route_request(harness.model, "frozen supply identity");
        async move { post_json(&base_url, &api_key, "/v1/images/generations", &key, &body).await }
    });
    // 等它真的受理落库：这一刻 Job 上的适配器与渠道模型就该定下来。这里轮询库而不是睡一会儿，
    // 因为要等的事实是"Job 已经在库里"，不是一个时长。
    let mut accepted = false;
    for _ in 0..200 {
        let state: Option<String> =
            sqlx::query_scalar("SELECT state FROM generation.jobs WHERE idempotency_key = $1")
                .bind(&key)
                .fetch_optional(&harness.pool)
                .await
                .expect("job lookup");
        if state.as_deref() == Some("accepted") {
            accepted = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(accepted, "受理必须落库，且此时还没有 Worker 领它");

    let row = sqlx::query(
        "SELECT adapter_key, provider_model_id, base_url, credential_env
         FROM generation.jobs WHERE idempotency_key = $1",
    )
    .bind(&key)
    .fetch_one(&harness.pool)
    .await
    .expect("the accepted job");
    let accepted_adapter: String = row.try_get("adapter_key").expect("frozen adapter");
    let accepted_model: String = row
        .try_get("provider_model_id")
        .expect("frozen channel model");
    let accepted_entry: String = row.try_get("base_url").expect("frozen entry");
    let accepted_credential: String = row
        .try_get("credential_env")
        .expect("frozen credential name");
    // 受理写入的这两个值就是这条渠道行当时的取值（下面会把它们就地改掉）。
    assert_eq!(
        accepted_entry, harness.upstream_base_url,
        "受理时冻结的入口地址"
    );
    assert_eq!(
        accepted_credential, "AIHUBMIX_API_KEY",
        "受理时冻结的凭证变量名"
    );

    // 重发：同一个渠道身份、同一份合同（合同不可变），只改这一行上的**可变量**——另一套适配器与
    // 渠道模型。`publication_body` 会把 `provider_model_id` 写成型号名，所以这里在它之后覆盖它，
    // 那正是本次要观察的值；上游地址必须与夹具那条逐字相同，否则渠道身份变了，落下来的会是另
    // 一条供给行，就验不到"就地改写复用行"这件事了。
    let mut republished = publication_body(
        harness.model,
        "route-test-1",
        None,
        vec![candidate("AIHubMix", "apimart-image-v1", &["prompt_only"])],
        None,
    );
    republished["offerings"][0]["base_url"] = json!(harness.upstream_base_url);
    republished["offerings"][0]["provider_model_id"] = json!("republished-channel-model");
    let response = client
        .post(format!("{}/api/v1/runtime-revisions", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&republished)
        .send()
        .await
        .expect("runtime publication");
    let status = response.status();
    let raw = response.text().await.expect("publication body");
    assert_eq!(status, StatusCode::OK, "重发必须成功：{raw}");

    // 前提：重发确实改掉了这一行上的那两个值（否则下面验的就不是"不受后续发布影响"）。
    let row =
        sqlx::query("SELECT adapter_key, provider_model_id FROM supply.offerings WHERE id = $1")
            .bind(offering)
            .fetch_one(&harness.pool)
            .await
            .expect("the reused supply row");
    let rewritten_adapter: String = row.try_get("adapter_key").expect("adapter");
    let rewritten_model: String = row.try_get("provider_model_id").expect("channel model");
    assert_eq!(
        rewritten_adapter, "apimart-image-v1",
        "重发就地改写这条供给的适配器"
    );
    assert_eq!(
        rewritten_model, "republished-channel-model",
        "重发就地改写这条供给的渠道模型"
    );
    assert_ne!(accepted_adapter, rewritten_adapter);
    assert_ne!(accepted_model, rewritten_model);
    assert_eq!(
        active_supply(&harness).await,
        (offering, channel),
        "重发复用同一行供给与渠道"
    );

    // 再**直接改库**换掉渠道行的入口与凭证名：这正是"没有应用内写入方、也没有约束挡着就地改"
    // 的那条路径。入口指到同一条假上游下的另一个前缀（若执行时读现场，请求就会落到那个前缀上，
    // 下面的计数与请求体都取不到），凭证名换成一个进程环境里根本没有的变量名（读现场的话这次
    // 执行连凭证都取不到，直接失败）。
    let rewritten_entry = format!("{}/republished-entry", harness.upstream_base_url);
    let rewritten_credential = "CONTRACT_TEST_UNSET_KEY";
    sqlx::query("UPDATE supply.channels SET base_url = $2, credential_env = $3 WHERE id = $1")
        .bind(channel)
        .bind(&rewritten_entry)
        .bind(rewritten_credential)
        .execute(&harness.pool)
        .await
        .expect("改库换入口");

    // 前提：改库确实换掉了这一行上的入口与凭证名（否则下面验的就不是"不被改库带走"）。
    let row = sqlx::query("SELECT base_url, credential_env FROM supply.channels WHERE id = $1")
        .bind(channel)
        .fetch_one(&harness.pool)
        .await
        .expect("the rewritten channel row");
    assert_eq!(
        row.try_get::<String, _>("base_url").expect("entry"),
        rewritten_entry
    );
    assert_eq!(
        row.try_get::<String, _>("credential_env")
            .expect("credential name"),
        rewritten_credential
    );

    // 放 Worker 出去执行这台早已受理的 Job。
    let worker = harness.spawn_worker();
    let (status, response_body) = in_flight.await.expect("the in-flight request");
    assert_eq!(
        status,
        StatusCode::OK,
        "受理时那一套仍然可用，这次执行必须成功：{response_body}"
    );
    assert_sync_success("受理时冻结的供给身份", &response_body);

    let submit = harness.submit_body("/v1/images/generations");
    assert_eq!(
        submit["model"].as_str(),
        Some(accepted_model.as_str()),
        "线上请求体里的 `model` 必须是受理时冻结的渠道模型，不是重发写进去的那个"
    );
    // 这次执行仍打在**受理时那个入口**上：假上游按原路径收到了它。若入口是执行时从渠道行现场读的，
    // 请求会落到改库换上去的 `/republished-entry/v1/images/generations` 上——上面的 `submit_body`
    // 取不到，这里的计数也是 0。
    assert_eq!(
        harness.count("POST", "/v1/images/generations"),
        1,
        "一次执行只交一次，而且必须交在受理时那个入口上"
    );
    assert_eq!(
        harness.count("POST", "/republished-entry"),
        0,
        "改库换上去的入口不许被这次执行用到"
    );
    // Job 自己那两列仍是受理时的值：冻结的是执行事实，改库换不掉它。
    let row = sqlx::query(
        "SELECT base_url, credential_env FROM generation.jobs WHERE idempotency_key = $1",
    )
    .bind(&key)
    .fetch_one(&harness.pool)
    .await
    .expect("the settled job");
    assert_eq!(
        row.try_get::<String, _>("base_url").expect("frozen entry"),
        accepted_entry,
        "Job 上的入口地址不因改库而变"
    );
    assert_eq!(
        row.try_get::<String, _>("credential_env")
            .expect("frozen credential name"),
        accepted_credential,
        "Job 上的凭证变量名不因改库而变"
    );
    assert_eq!(
        harness.count("GET", "/v1/tasks/"),
        0,
        "走的是受理时那个同步适配器：任务式适配器交完之后会去轮询任务"
    );
    let (_, state, _) = harness.job(&key).await;
    assert_eq!(state, "succeeded", "内部执行记录必须跑到终态");

    drop(worker);
    harness.cleanup().await;
}

/// 两条供给级启停接口的对外形状：管理员凭证、目标不存在、只允许改 `enabled`。
///
/// 形状本身就是合同的一部分：多给一个字段必须**被拒**而不是被静默忽略——忽略会让调用方以为
/// 承载面 / 计价改成功了，而定义只能由发布产生。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_supply_switches_take_only_enabled_and_need_admin_credentials() {
    let harness = Harness::start(UpstreamBehaviour::aihubmix(SyncImageShape::Url)).await;
    let client = Client::new();
    let (offering, channel) = active_supply(&harness).await;
    let offering_path = format!("/api/v1/offerings/{offering}");
    let channel_path = format!("/api/v1/channels/{channel}");

    // 无凭证：403。鉴权先于取数，所以不存在的 id 也一样。
    for path in [&offering_path, &channel_path] {
        let (status, body) = patch_supply(
            &client,
            &harness.base_url,
            path,
            None,
            &json!({"enabled": false}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {body}");
    }

    // 目标不存在：404，而且不留下审计——被拒的写入什么都没发生。
    let unknown = Uuid::new_v4();
    assert_eq!(
        patch_offering(
            &client,
            &harness.base_url,
            &harness.admin_token,
            unknown,
            false
        )
        .await,
        StatusCode::NOT_FOUND,
        "没发布过的供给 id 是'不存在'，不是'待创建'"
    );
    assert_eq!(
        patch_channel(
            &client,
            &harness.base_url,
            &harness.admin_token,
            unknown,
            false
        )
        .await,
        StatusCode::NOT_FOUND
    );

    // 只有 `enabled` 是可变位：多给字段 / 少给 / 类型不对都是 400（不是 422）。
    for body in [
        json!({"enabled": false, "carrier_schema": {"type": "object"}}),
        json!({"enabled": false, "formula": "per_image"}),
        json!({}),
        json!({"enabled": "no"}),
        json!([true]),
    ] {
        let (status, response) = patch_supply(
            &client,
            &harness.base_url,
            &offering_path,
            Some(&harness.admin_token),
            &body,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {response}");
        let (status, response) = patch_supply(
            &client,
            &harness.base_url,
            &channel_path,
            Some(&harness.admin_token),
            &body,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {response}");
    }

    // 到这一步一次合法写入都还没有发生过，两类审计都该是空的。
    assert!(
        audit_events(&harness, "offering.set_enabled")
            .await
            .is_empty()
    );
    assert!(
        audit_events(&harness, "channel.set_enabled")
            .await
            .is_empty()
    );

    // 合法写入：204，并各留一条审计，载荷记下改成了什么。
    assert_eq!(
        patch_offering(
            &client,
            &harness.base_url,
            &harness.admin_token,
            offering,
            false
        )
        .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        patch_channel(
            &client,
            &harness.base_url,
            &harness.admin_token,
            channel,
            false
        )
        .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        audit_events(&harness, "offering.set_enabled").await,
        vec![json!({"enabled": false})]
    );
    assert_eq!(
        audit_events(&harness, "channel.set_enabled").await,
        vec![json!({"enabled": false})]
    );

    harness.cleanup().await;
}
