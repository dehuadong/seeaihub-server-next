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
    // 对客形态是"上游声明金额 × 倍率"，四档向量不会被读、带着它发布期就拒：精简形态里没有它。
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

    // 这次增量发布省略渠道三要素，沿用是否真的发生由上面两条查询钉住（生效候选只有一条、    // 渠道地址与凭证都在）。
    // 定价侧的沿用由「新受理的 Job 用新倍率、已受理的 Job 快照不动」那条用例覆盖（cases_pricing）。

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

/// 引用式发布的条目快照取**被引用的那条 Offering 行**：八列技术定义与渠道三要素都不来自请求
/// （引用形态里根本没有它们），而是发布那一刻活行上的值。
///
/// 判据分两半，缺一不可：
/// - 引用形态发出来的条目八列**逐位等于**那条 Offering 行与它所属渠道行，且条目指向的就是它；
/// - 直接改行上的 `carrier_schema` 之后再引用一次，新条目取到的是**新值**——否则把值写死在发布
///   路径里也能让前一半通过（引用一条供给就是引用它的当前值，不是引用发布那一刻的拷贝）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_referenced_publication_freezes_the_offering_row_it_points_at() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    // 工程师那一侧：先按内联形态发布一次，库里因此有一条**可被引用**的 Offering。
    let model = "referenced-freeze-model";
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1}
    }));
    let mut full = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    full["base_url"] = json!("https://referenced.example.com");
    assert_eq!(
        publish_candidates(
            &client,
            &base_url,
            &admin_token,
            model,
            Some(contract),
            vec![full]
        )
        .await,
        StatusCode::OK,
        "第一次发布应当成功"
    );

    let offering_id: Uuid = sqlx::query_scalar(
        "SELECT o.id FROM supply.offerings o
         JOIN catalog.vendor_models vm ON vm.id = o.vendor_model_id
         WHERE vm.native_model_id = $1 AND vm.native_revision = 'route-test-1'",
    )
    .bind(model)
    .fetch_one(&pool)
    .await
    .expect("the offering the operators will reference");

    // 引用形态：只给"选了哪条 Offering"与这条候选的价，一个技术字段都不给（渠道币种也由服务端
    // 从该供给当前那行费率取，所以这里连 `cost_currency` 都不提）。
    let gateway = "referenced-freeze-gateway";
    let reference_body = json!({
        "vendor_id": "OpenAI",
        "native_model_id": model,
        "native_revision": "route-test-1",
        "gateway_model": gateway,
        "actor": "contract-test",
        "markup_bps": 2_400,
        "references": [{
            "offering_id": offering_id
        }]
    });

    // 条目八列 + 它们指向的那条活行，用同一个形状读回来：判据是"逐位相等"，不是"某个字段像"。
    type Snapshot = (String, String, Value, Value, Value, String, String, String);
    let referenced_row = |pool: &PgPool, offering_id: Uuid| {
        let pool = pool.clone();
        async move {
            sqlx::query_as::<_, Snapshot>(
                "SELECT o.adapter_key, o.provider_model_id, o.carrier_schema, o.parameter_mapping,
                        o.restrictions, c.provider_kind, c.base_url, c.credential_env
                 FROM supply.offerings o
                 JOIN supply.channels c ON c.id = o.channel_id
                 WHERE o.id = $1",
            )
            .bind(offering_id)
            .fetch_one(&pool)
            .await
            .expect("the referenced offering row")
        }
    };
    let entry_of = |pool: &PgPool, gateway: &str| {
        let pool = pool.clone();
        let gateway = gateway.to_owned();
        async move {
            sqlx::query_as::<_, Snapshot>(
                "SELECT re.adapter_key, re.provider_model_id, re.carrier_schema,
                        re.parameter_mapping, re.restrictions, re.provider_kind, re.base_url,
                        re.credential_env
                 FROM publication.runtime_entries re
                 WHERE re.active AND re.gateway_model = $1",
            )
            .bind(gateway)
            .fetch_one(&pool)
            .await
            .expect("the frozen entry")
        }
    };

    let response = client
        .post(format!("{base_url}/api/v1/runtime-revisions"))
        .bearer_auth(&admin_token)
        .json(&reference_body)
        .send()
        .await
        .expect("referenced publication");
    let status = response.status();
    let text = response.text().await.expect("referenced body");
    assert_eq!(status, StatusCode::OK, "引用式发布应当成功：{text}");

    let entry = entry_of(&pool, gateway).await;
    assert_eq!(
        entry,
        referenced_row(&pool, offering_id).await,
        "引用式发布的条目八列必须逐位等于被引用的那条供给行"
    );
    // 引用的是**既有那条**供给：引用式发布不新建供给行（技术定义是工程师的资产）。
    let (entry_offering, supply_rows): (Uuid, i64) = (
        sqlx::query_scalar(
            "SELECT offering_id FROM publication.runtime_entries
             WHERE active AND gateway_model = $1",
        )
        .bind(gateway)
        .fetch_one(&pool)
        .await
        .expect("the entry's offering"),
        sqlx::query_scalar(
            "SELECT count(*) FROM supply.offerings o
             JOIN catalog.vendor_models vm ON vm.id = o.vendor_model_id
             WHERE vm.native_model_id = $1",
        )
        .bind(model)
        .fetch_one(&pool)
        .await
        .expect("supply rows of the vendor model"),
    );
    assert_eq!(
        entry_offering, offering_id,
        "条目必须指向运营选中的那条供给"
    );
    assert_eq!(supply_rows, 1, "引用式发布不该为这个厂商模型新建供给行");

    // 直接改活表：模拟"工程师事后改了这条供给的承载面"。改的是一个不改变受理口径的注记
    // （`description` 是 JSON Schema 的注解关键字），所以这次发布该走通、且必须取到改后的那一份。
    let rewritten = "rewritten by the engineer";
    let updated = sqlx::query(
        "UPDATE supply.offerings
         SET carrier_schema = carrier_schema || jsonb_build_object('description', $2::text)
         WHERE id = $1",
    )
    .bind(offering_id)
    .bind(rewritten)
    .execute(&pool)
    .await
    .expect("rewrite the carrier")
    .rows_affected();
    assert_eq!(updated, 1, "夹具必须改到那条供给");

    let republished = client
        .post(format!("{base_url}/api/v1/runtime-revisions"))
        .bearer_auth(&admin_token)
        .json(&reference_body)
        .send()
        .await
        .expect("second referenced publication");
    let status = republished.status();
    let text = republished.text().await.expect("second referenced body");
    assert_eq!(status, StatusCode::OK, "重新用引用形态发布应当成功：{text}");

    let fresh = entry_of(&pool, gateway).await;
    assert_eq!(
        fresh,
        referenced_row(&pool, offering_id).await,
        "重新发布必须取到改后的那行"
    );
    assert_eq!(
        fresh.2["description"],
        json!(rewritten),
        "新条目的承载面必须取到行上改后的值，而不是上一次发布的那一份"
    );

    drop_isolated_database(&database_name).await;
}

/// 引用式发布**给参考成本**时，成本口径与保底表由服务端定，不要运营给。
///
/// 这条守的是一个只有真给参考成本才会走到的分支：`carries_pricing` 在"给了 `reference_cost_microusd`"
/// 时为真，而那之后旧代码会要求调用方**同时**给出 `cost_basis` 与 `floor_amounts`。这两样都不是运营的
/// 选择——`cost_basis` 两态由计价形态唯一决定（渠道终态给金额就是 `declared`，否则平台按用量自算就是
/// `computed`），保底表缺省是空表。要运营填它们，等于让他去猜一个他无从知道的枚举值。
///
/// 既有用例从没给过 `reference_cost_microusd`，所以这个分支一直没被走到——是真机上走一遍运营路径才
/// 撞出来的（400 `cost_basis is required when the candidate carries pricing`）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_referenced_publication_derives_the_cost_basis_from_the_channel_formula() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    let model = "referenced-cost-basis-model";
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1}
    }));
    let mut full = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    full["base_url"] = json!("https://cost-basis.example.com");
    assert_eq!(
        publish_candidates(
            &client,
            &base_url,
            &admin_token,
            model,
            Some(contract),
            vec![full]
        )
        .await,
        StatusCode::OK,
        "先把那条可被引用的 Offering 造出来"
    );
    let offering_id: Uuid = sqlx::query_scalar(
        "SELECT o.id FROM supply.offerings o
         JOIN catalog.vendor_models vm ON vm.id = o.vendor_model_id
         WHERE vm.native_model_id = $1",
    )
    .bind(model)
    .fetch_one(&pool)
    .await
    .expect("the offering the operators will reference");

    // 只给"选了哪条"与这条候选的价——**包括参考成本**，但一个技术字段都不给。
    let gateway = "referenced-cost-basis-gateway";
    let body = json!({
        "gateway_model": gateway,
        "actor": "contract-test",
        "markup_bps": 2_400,
        "references": [{
            "offering_id": offering_id,
            // 给一个小值：这条要验的是"成本口径由服务端推出来"，不是单请求成本上限
            // （那条上限由 `GENERATION_MAX_REQUEST_COST_MICROUSD` 兜着，另有用例管它）。
            "reference_cost_microusd": 120_000_u64
        }]
    });
    let response = client
        .post(format!("{base_url}/api/v1/runtime-revisions"))
        .bearer_auth(&admin_token)
        .json(&body)
        .send()
        .await
        .expect("referenced publication");
    let status = response.status();
    let text = response.text().await.expect("referenced body");
    assert_eq!(status, StatusCode::OK, "给了参考成本也要能发出去：{text}");

    // 成本口径由该渠道的计价形态决定：`token_rates` 是"平台按用量自算"，所以是 `computed`。
    let basis: String = sqlx::query_scalar(
        "SELECT rr.cost_basis ->> re.offering_id::text
         FROM publication.runtime_entries re
         JOIN publication.runtime_revisions rr ON rr.id = re.runtime_revision_id
         WHERE re.active AND re.gateway_model = $1",
    )
    .bind(gateway)
    .fetch_one(&pool)
    .await
    .expect("the cost basis of the published candidate");
    assert_eq!(
        basis, "declared",
        "渠道不给金额字段时成本由平台自算，运营不必声明"
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
        // 有候选按上游声明的金额计价，倍率是对客价来源，必须一起发。
        let body = publication_body(model, "route-test-1", None, vec![offering], Some(2_000));
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
            // 容器字段按**成员**判可达：每个成员都要由映射从已声明的合同字段指过来，与平台侧
            // 那条发布校验同一套判据。
            let container = stored_carriers[index]["properties"][name]
                .get("properties")
                .and_then(Value::as_object)
                .is_some_and(|members| {
                    !members.is_empty()
                        && members.keys().all(|member| {
                            let wire = format!("{name}.{member}");
                            wires.iter().any(|w| w.as_str() == Some(wire.as_str()))
                        })
                });
            assert!(
                declared || renamed || container,
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

    // 把 `quality` 补进合同、承载面按线上形态收进 `extra` 之后就能发布：拒绝的是那条边界，不是
    // `quality` 本身。
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string", "enum": ["low", "high"]}
    }));
    let carrier = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "extra": {
            "type": "object",
            "properties": {"quality": {"type": "string", "enum": ["low", "high"]}}
        }
    }));
    let status = publish_with_mappings(
        &client,
        &base_url,
        &admin_token,
        model,
        "contract-boundary-2",
        contract,
        vec![(
            "aihubmix-image-v1",
            carrier,
            json!({"rename": {"quality": "extra.quality"}}),
        )],
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

    // 承载面按线上形态声明了它（`extra` 容器 + 改名落位）：同一份默认值照常发布。
    let wide = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "extra": {
            "type": "object",
            "properties": {"quality": {"type": "string", "enum": ["low", "high"]}}
        }
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
            json!({
                "rename": {"quality": "extra.quality"},
                "defaults": {"quality": "low"}
            }),
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
        json!({"object": "list", "data": []}),
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
    // 这条型号的合同不含模型专属字段：本用例只看目录列出的那一份合同，与字段面无关。
    let other_contract = surface_schema(json!({
        "model": {"const": other},
        "prompt": {"type": "string", "minLength": 1}
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

    // ── 两个型号都在，形状是平台字段 + OpenAI 列表标准字段（Spec 0009 §2）；照旧不带鉴权头 ──
    let (status, catalog) = get_catalog(&client, &base_url, None).await;
    assert_eq!(status, StatusCode::OK, "got {catalog}");
    assert_public_only("目录", &catalog);
    assert_eq!(
        catalog["object"].as_str(),
        Some("list"),
        "顶层带标准列表信封：{catalog}"
    );
    assert_eq!(
        catalog.as_object().map(serde_json::Map::len),
        Some(2),
        "目录顶层只有 object 与 data：{catalog}"
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
            vec![
                "contract",
                "created",
                "documentation_url",
                "id",
                "name",
                "object",
                "owned_by",
                "revision",
                "type",
                "vendor_id"
            ],
            "目录条目是平台六个字段加标准的 id / object / created / owned_by：{entry}"
        );
        // 标准字段是平台字段的投射，不是另一份事实（Spec 0009 §2 A2）。
        assert_eq!(entry["object"].as_str(), Some("model"), "{entry}");
        assert_eq!(entry["id"].as_str(), Some(name), "{entry}");
        assert_eq!(entry["owned_by"].as_str(), Some("OpenAI"), "{entry}");
        // A2：`created` 等于该条当前 Runtime Revision 的发布时间（两边都取整到秒）。
        let published_at: i64 = sqlx::query_scalar(
            "SELECT EXTRACT(EPOCH FROM date_trunc('second', rr.created_at))::bigint
             FROM publication.runtime_entries re
             JOIN publication.runtime_revisions rr ON rr.id = re.runtime_revision_id
             WHERE re.gateway_model = $1 AND re.active",
        )
        .bind(name)
        .fetch_one(&pool)
        .await
        .expect("该型号当前生效的 Runtime Revision");
        assert_eq!(
            entry["created"].as_i64(),
            Some(published_at),
            "created 必须是当前发布的生效时间：{entry}"
        );
        assert_eq!(entry["type"].as_str(), Some("image"), "{entry}");
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
    // A4：标准客户端只读 `id` 也能取到全部可见模型。
    let ids: Vec<&str> = entries
        .iter()
        .filter_map(|entry| entry["id"].as_str())
        .collect();
    assert_eq!(ids.len(), published.len(), "id 覆盖全部可见模型：{catalog}");
    for (name, ..) in published {
        assert!(ids.contains(&name), "id 必须覆盖 {name}：{catalog}");
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
        json!({"object": "list", "data": []}),
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
    // 模型级的组合约束随合同一并交给客户端：取值由上游判，客户端要知道这两个参数怎么一起用。
    let declares_joint = entry["contract"]["allOf"]
        .as_array()
        .is_some_and(|clauses| {
            clauses.iter().any(|clause| {
                clause["if"]["properties"]["background"]["const"] == json!("transparent")
                    && clause["then"]["properties"]["output_format"]["enum"]
                        == json!(["png", "webp"])
            })
        });
    assert!(
        declares_joint,
        "对客合同必须声明 transparent => png/webp：{entry}"
    );

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
        count_calls(&calls, "POST", "/ai/v1/images/generations"),
        1,
        "请求要真的发到假上游"
    );
    let submit = last_submit_body(&calls, "/ai/v1/images/generations");
    assert_eq!(
        submit["model"], NATIVE,
        "上行给渠道的是厂商原生名，不是对客名：{submit}"
    );
    let stored_model: String = sqlx::query_scalar(
        "SELECT gateway_model FROM generation.jobs WHERE idempotency_key_digest = $1",
    )
    .bind(idempotency_key_digest(&key))
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
    // 上一条按对客名受理的请求已经发到过上游，这条按厂商原生名的请求不该再发一次。
    assert_eq!(
        count_calls(&calls, "POST", "/ai/v1/images/generations"),
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
        json!({"object": "list", "data": []}),
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
    // 承载面按线上形态：模型专属字段收进 `extra`，参考图字段叫 `images`。
    let legacy_carrier = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": {
            "model": { "const": "gpt-image-2" },
            "prompt": { "type": "string", "minLength": 1 },
            "images": { "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": 16 },
            "mask": { "type": "string" },
            "n": { "type": "integer", "minimum": 1, "maximum": 10, "default": 1 },
            "size": { "type": "string", "anyOf": [{ "const": "auto" }, { "pattern": "^[0-9]+x[0-9]+$" }] },
            "output_format": { "type": "string", "enum": ["png", "jpeg"], "default": "png" },
            "extra": {
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "quality": { "type": "string", "enum": ["low", "medium", "high"] },
            "output_compression": { "type": "integer", "minimum": 0, "maximum": 100, "default": 100 },
            "background": { "type": "string", "enum": ["auto", "opaque", "transparent"], "default": "auto" },
            "moderation": { "type": "string", "enum": ["auto", "low"], "default": "auto" }
        }
            }
        },
        "allOf": [
            { "if": { "required": ["mask"] }, "then": { "required": ["images"] } }
        ]
    });
    let command = json!({
        "vendor_id": "OpenAI",
        "native_model_id": "gpt-image-2",
        "native_revision": "2026-09-18-validated-1.3",
        "type": "image",
        "actor": "bootstrap",
        // 有候选按上游声明的金额计价，修订级倍率是它的对客价来源。
        "markup_bps": 2_000,
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
            "carrier_schema": legacy_carrier,
            "parameter_mapping": {
                "rename": {
                    "image": "images",
                    "quality": "extra.quality",
                    "background": "extra.background",
                    "moderation": "extra.moderation",
                    "output_compression": "extra.output_compression"
                }
            },
            "formula": "token_rates",
            // 对客形态显式给出：这条通路给不出 token 分项，对客按上游声明的金额加价。
            "consumer_formula": "upstream_declared",
            "cost_currency": "USD",
            "price_plan": {
                "currency": "USD",
                "text_input_microusd_per_million": 5000000,
                "image_input_microusd_per_million": 8000000,
                "text_output_microusd_per_million": 10000000,
                "image_output_microusd_per_million": 30000000,
                "source_url": "https://aihubmix.com/model/gpt-image-2"
            }
        }],
        "documentation": documentation_for(&contract)
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

    // ── 目录逐位一致：name 是厂商原生名，平台字段照旧 ──
    let (status, catalog) = get_catalog(&client, &base_url, None).await;
    assert_eq!(status, StatusCode::OK, "{catalog}");
    // 文档地址随发布生成（版本标识是随机 UUID），先校验它指对模型，再剥掉它逐位比其余字段。
    let mut entry = catalog["data"][0].clone();
    let documentation_url = entry["documentation_url"]
        .as_str()
        .expect("catalog entry carries documentation_url")
        .to_owned();
    assert!(
        documentation_url.starts_with("/v1/models/gpt-image-2/llms.txt?version="),
        "文档地址指向该模型的公开说明：{documentation_url}"
    );
    // 标准字段由平台字段派生，逐位比较前与文档地址一起剥掉。
    let fields = entry.as_object_mut().expect("entry is an object");
    for key in ["documentation_url", "id", "object", "created", "owned_by"] {
        fields.remove(key);
    }
    assert_eq!(
        entry,
        json!({
            "name": "gpt-image-2",
            "vendor_id": "OpenAI",
            "revision": "2026-09-18-validated-1.3",
            "type": "image",
            "contract": contract,
        }),
        "缺省回退之后目录与新增 type 逐位一致：{catalog}"
    );

    // ── 受理行为同样照旧：按回退出来的名字跑通 ──
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
    // 现在决定折算的是**成本币种**（这条供给按上游声明的金额计价，没有价目表）。
    draft["cost_currency"] = json!("EUR");
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
        republish_priced(&harness, &client, openai_floor_amounts(), 2_000).await,
        StatusCode::OK
    );
    let (_, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
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
    let (job_id, _) = harness.job(&key).await;
    // 成本是上游声明的 11 354 微美元（这条供给按声明金额计价），冻结的折算率把它折成人民币。
    assert_eq!(harness.attempt_cost(job_id).await.3, Some(80_614));

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
        Some(80_614),
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

    // 3) 按 token 计量量计价却没有那份四档费率（这条构造体按 token 计价的是 APIMart）。
    let mut token_without_plan = candidate("APIMart", "apimart-image-v1", &["prompt_only"]);
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
    // 声明金额形态用不到那份四档费率：带着它发布期就拒，免得让人以为它在生效。
    declared_with_plan["price_plan"] = json!({
        "currency": "USD",
        "text_input_microusd_per_million": 5000000,
        "image_input_microusd_per_million": 8000000,
        "text_output_microusd_per_million": 10000000,
        "image_output_microusd_per_million": 30000000,
        "source_url": "https://example.invalid/price"
    });
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

    // 5) 按张 / 按次是**成本**形态：它的参数是单价，不是那份四档费率（Price Plan 可以不发）。
    let mut per_image = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    per_image["formula"] = json!("per_image");
    per_image["price_plan"] = Value::Null;
    per_image["cost_unit_price_microusd"] = json!(11_354);
    per_image["cost_currency"] = json!("USD");
    // 这条构造体是按渠道给对客形态的（AIHubMix 默认 `upstream_declared`）；下面要验的是"缺对客
    // 形态"，所以显式清掉它。
    per_image
        .as_object_mut()
        .expect("candidate")
        .remove("consumer_formula");
    // 6) 对客形态是另一件事，只有按 token 四档 / 上游声明金额 × 倍率两种，必须显式给出——
    //    成本按张的候选没有可沿用的对客形态。
    let (status, body) = publish(
        &client,
        &base_url,
        &admin_token,
        model,
        vec![per_image.clone()],
        Some(2_000),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert!(message(&body).contains("consumer_formula"), "{body}");

    // 7) 对客选"上游声明金额 × 倍率"：缺倍率算不出该收多少钱，发布期就拒。
    let mut declared = per_image.clone();
    declared["consumer_formula"] = json!("upstream_declared");
    let (status, body) = publish(
        &client,
        &base_url,
        &admin_token,
        model,
        vec![declared.clone()],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert!(message(&body).contains("markup_bps is required"), "{body}");

    // 8) 给倍率也不行：AIHubMix 只回四分项 `usage`、**声明不了金额**，对客按上游金额这条形态在它
    //    身上发布期就拒（渠道能力，`0012` §4）。
    // 8) 对客选"按 token 四档"也不行：AIHubMix 的成功件没有 token 分项，拿不到用量就算不出对客价，
    //    发布期就拒（渠道能力）。
    let mut token_priced = per_image.clone();
    token_priced["consumer_formula"] = json!("token_rates");
    token_priced["consumer_rates_cny"] = json!({
        "text_input_micros_per_million": 42_600_000u64,
        "image_input_micros_per_million": 68_160_000u64,
        "text_output_micros_per_million": 85_200_000u64,
        "image_output_micros_per_million": 255_600_000u64
    });
    let (status, body) = publish(
        &client,
        &base_url,
        &admin_token,
        model,
        vec![token_priced],
        Some(2_000),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert!(message(&body).contains("provides no token usage"), "{body}");

    // 9) 对客选上游金额却带一份对客四档向量：那份向量只属对客按 token 四档，永远不会被读。
    let mut declared_with_rates = per_image.clone();
    declared_with_rates["consumer_formula"] = json!("upstream_declared");
    declared_with_rates["consumer_rates_cny"] = json!({
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
        vec![declared_with_rates],
        Some(2_000),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert!(
        message(&body).contains("consumer_rates_cny does not apply to consumer_formula"),
        "{body}"
    );

    // 10) 对客按 token 四档：同样不要价目表，发布成功；受理照常、快照冻结成本形态与单价。
    //     按 token 四档卖要求这条通路给得出四分项用量，所以这里用 APIMart 的构造体。
    let mut token_priced = candidate("APIMart", "apimart-image-v1", &["prompt_only"]);
    token_priced["formula"] = json!("per_image");
    token_priced["price_plan"] = Value::Null;
    token_priced["cost_unit_price_microusd"] = json!(11_354);
    token_priced["cost_currency"] = json!("USD");
    token_priced["consumer_formula"] = json!("token_rates");
    token_priced["consumer_rates_cny"] = json!({
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
        vec![token_priced],
        Some(2_000),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "成本按张、对客按 token 四档，没有价目表照样发布：{body}"
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
    assert_eq!(snapshot["consumer_formula"], json!("token_rates"));
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
                // 有候选按上游声明的金额计价，倍率是对客价来源，必须一起发。
                Some(2_000),
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

/// 发布命令引用一条**库里没有**的 Offering：400，且答复里必须出现那个标识本身（P3）。
///
/// 判据只有这一条：运营照着清单勾了一条、而清单与库不同步时，他要能立刻知道是**哪一条**不见了。
/// 少一条候选而照样发布成功，等于这次发布的模型少一条路，而运营以为自己选上了。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_reference_to_an_offering_outside_the_supply_table_is_rejected_by_name() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let missing = Uuid::new_v4();
    let response = client
        .post(format!("{base_url}/api/v1/runtime-revisions"))
        .bearer_auth(&admin_token)
        .json(&json!({
            "vendor_id": "OpenAI",
            "native_model_id": "missing-reference-model",
            "native_revision": "route-test-1",
            "gateway_model": "missing-reference-gateway",
            "actor": "contract-test",
            "references": [{
                "offering_id": missing,
            }]
        }))
        .send()
        .await
        .expect("referenced publication");
    let status = response.status();
    let text = response.text().await.expect("rejection body");
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "引用一条不存在的供给必须被拒：{text}"
    );
    assert!(
        text.contains(&missing.to_string()),
        "答复必须点名那条取不到的供给：{text}"
    );

    drop_isolated_database(&database_name).await;
}

/// 引用一条**已停用**的 Offering：被拒并点名（P3）。停用是"这条现在选不了"，不是"它不存在"。
///
/// 三件事一起验：那条供给在清单里照样列着（停用不是删除，所以"不存在"那种答复在这里是错的）、
/// 拿它去发布被拒、答复能读出是停用/不可用。少一条不拒就是真缺陷：运营能在界面上勾一条停用的
/// 供给、发布成功，而那个模型从发布那一刻起就有一条路是死的。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_reference_to_a_disabled_offering_is_rejected_by_name() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    let model = "disabled-reference-model";
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1}
    }));
    let mut full = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    full["base_url"] = json!("https://disabled.example.com");
    assert_eq!(
        publish_candidates(
            &client,
            &base_url,
            &admin_token,
            model,
            Some(contract),
            vec![full]
        )
        .await,
        StatusCode::OK,
        "夹具那条供给要先发出来，才有东西可停用"
    );

    let offering_id: Uuid = sqlx::query_scalar(
        "SELECT o.id FROM supply.offerings o
         JOIN catalog.vendor_models vm ON vm.id = o.vendor_model_id
         WHERE vm.native_model_id = $1",
    )
    .bind(model)
    .fetch_one(&pool)
    .await
    .expect("the offering to disable");

    assert_eq!(
        patch_offering(&client, &base_url, &admin_token, offering_id, false).await,
        StatusCode::NO_CONTENT,
        "停用那条供给"
    );
    let still_listed = client
        .get(format!("{base_url}/api/v1/offerings"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("selectable offerings");
    let listed: Value = serde_json::from_str(
        &still_listed
            .text()
            .await
            .expect("selectable offerings body"),
    )
    .expect("selectable offerings json");
    assert!(
        listed["offerings"]
            .as_array()
            .expect("an offerings array")
            .iter()
            .any(|offering| offering["offering_id"] == json!(offering_id)
                && offering["enabled"] == json!(false)),
        "停用的供给仍然在清单里（停用不是删除）：{listed}"
    );

    let response = client
        .post(format!("{base_url}/api/v1/runtime-revisions"))
        .bearer_auth(&admin_token)
        .json(&json!({
            "vendor_id": "OpenAI",
            "native_model_id": model,
            "native_revision": "route-test-1",
            "gateway_model": "disabled-reference-gateway",
            "actor": "contract-test",
            "references": [{
                "offering_id": offering_id,
            }]
        }))
        .send()
        .await
        .expect("referenced publication");
    let status = response.status();
    let text = response.text().await.expect("rejection body");
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "引用一条已停用的供给必须被拒：{text}"
    );
    assert!(
        text.contains(&offering_id.to_string()),
        "答复必须点名那条选不了的供给：{text}"
    );
    // 措辞不指定，但这几个词是"停用/不可用"绕不开的说法：答复必须让人看出为什么选不了，
    // 而不是笼统的"发布不成立"。
    let lowered = text.to_lowercase();
    assert!(
        ["disabled", "not enabled", "unavailable"]
            .iter()
            .any(|hint| lowered.contains(hint)),
        "答复要能看出是停用/不可用：{text}"
    );

    drop_isolated_database(&database_name).await;
}

/// 管理员读可选 Offering 清单（`0012` §2.1、P1）：字段齐全、带 `offering_id`、按厂商与厂商模型名
/// 稳定排序，且**不含渠道地址与凭证变量名**。
///
/// 三个判据不是显然的，各验一条：
/// - 顺序按 `vendor_id` → `native_model_id` → `provider_kind`，而且**不是插入顺序**（后发布的那台
///   厂商排在前面）——发布页据此分组，顺序不稳清单就会跳；
/// - 停用的供给照样列出来并带 `enabled: false`——藏起来等于"关掉之后再也找不到怎么打开"；
/// - 无管理员凭证是 403，它与网关模型清单在同一层鉴权。
///
/// 另外挂一条**没有 Price Plan、也还没被发布过**的按张计价供给（R2 的素材导入就产出这种行）：
/// 它的成本币种无从谈起，清单读侧该给 `null`，不是报错（`0012` §3 末段：`supply.offerings` 里没有
/// 成本币种这一列，币种只在 Price Plan 或发布物里）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_selectable_offering_list_carries_the_selection_key_without_deployment_facts() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    // 两台厂商模型各一条供给：厂商标识与模型名故意让"排序结果"与"插入顺序"相反。
    for (vendor_id, model, channel_url) in [
        ("OpenAI", "zebra-model", "https://zebra.example.com"),
        ("AIHubMix", "alpha-model", "https://alpha.example.com"),
    ] {
        let contract = surface_schema(json!({
            "model": {"const": model},
            "prompt": {"type": "string", "minLength": 1}
        }));
        let mut full = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
        full["base_url"] = json!(channel_url);
        full["provider_model_id"] = json!(model);
        full["capability_schema"]["properties"]["model"]["const"] = json!(model);
        let response = client
            .post(format!("{base_url}/api/v1/runtime-revisions"))
            .bearer_auth(&admin_token)
            .json(&json!({
                "vendor_id": vendor_id,
                "native_model_id": model,
                "native_revision": "route-test-1",
                "type": "image",
                "actor": "contract-test",
                "documentation": documentation_for(&contract),
                "capability_schema": contract,
                // AIHubMix 的构造体按上游声明的金额计价，倍率是对客价来源，必须一起发。
                "markup_bps": 2_000,
                "offerings": [full]
            }))
            .send()
            .await
            .expect("inline publication");
        let status = response.status();
        let text = response.text().await.expect("inline publication body");
        assert_eq!(status, StatusCode::OK, "{model} 应当发布成功：{text}");
    }

    let anonymous = client
        .get(format!("{base_url}/api/v1/offerings"))
        .send()
        .await
        .expect("anonymous list");
    assert_eq!(
        anonymous.status(),
        StatusCode::FORBIDDEN,
        "清单是运营视图，必须管理员凭证"
    );

    // 一条**按张计价、没有 Price Plan、也还没被发布过**的供给：素材导入产出的就是这种行（R2）。
    // 直接写在库里，是因为它要验的正是"没有那两处币种来源时读侧怎么办"——走发布路径反而会补上
    // 发布物里那份成本币种，把这个用例要问的东西遮掉。
    let per_image_model = "per-image-model";
    let per_image_offering = Uuid::new_v4();
    let vendor_model = Uuid::new_v4();
    let channel = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO catalog.vendor_models
             (id, vendor_id, native_model_id, native_revision, model_type, capability_schema)
         VALUES ($1, 'AIHubMix', $2, 'route-test-1', 'image', '{}'::jsonb)",
    )
    .bind(vendor_model)
    .bind(per_image_model)
    .execute(&pool)
    .await
    .expect("the per-image vendor model");
    sqlx::query(
        "INSERT INTO supply.channels (id, provider_kind, base_url, credential_env)
         VALUES ($1, 'AIHubMix', 'https://per-image.example.com', 'AIHUBMIX_API_KEY')",
    )
    .bind(channel)
    .execute(&pool)
    .await
    .expect("the per-image channel");
    sqlx::query(
        "INSERT INTO supply.offerings
             (id, vendor_model_id, channel_id, adapter_key, provider_model_id, restrictions,
              carrier_schema, parameter_mapping, formula, cost_unit_price_microusd)
         VALUES ($1, $2, $3, 'aihubmix-image-v1', $4,
                 '{\"allowed_branches\": [\"prompt_only\"], \"max_reference_images\": 0}'::jsonb,
                 '{}'::jsonb, '{}'::jsonb, 'per_image', 12345)",
    )
    .bind(per_image_offering)
    .bind(vendor_model)
    .bind(channel)
    .bind(per_image_model)
    .execute(&pool)
    .await
    .expect("the per-image offering");

    // 停用其中一条：它仍要在清单里，并把状态带出来。
    let disabled_offering: Uuid = sqlx::query_scalar(
        "SELECT o.id FROM supply.offerings o
         JOIN catalog.vendor_models vm ON vm.id = o.vendor_model_id
         WHERE vm.vendor_id = 'OpenAI'",
    )
    .fetch_one(&pool)
    .await
    .expect("the offering to disable");
    assert_eq!(
        patch_offering(&client, &base_url, &admin_token, disabled_offering, false).await,
        StatusCode::NO_CONTENT
    );

    let response = client
        .get(format!("{base_url}/api/v1/offerings"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("selectable offerings");
    let status = response.status();
    let text = response.text().await.expect("selectable offerings body");
    assert_eq!(status, StatusCode::OK, "{text}");
    let body: Value = serde_json::from_str(&text).expect("selectable offerings json");
    let offerings = body["offerings"]
        .as_array()
        .expect("an offerings array")
        .clone();
    assert_eq!(
        offerings.len(),
        3,
        "三条供给都要在清单里，停用的与没被发布过的也不例外：{text}"
    );
    // 整份答复里不该出现任何渠道部署事实：键不出现，值也不出现。
    for leaked in [
        "base_url",
        "credential_env",
        "AIHUBMIX_API_KEY",
        "per-image.example.com",
        "zebra.example.com",
    ] {
        assert!(
            !text.contains(leaked),
            "清单泄出了渠道部署事实 `{leaked}`：{text}"
        );
    }

    for offering in &offerings {
        let object = offering.as_object().expect("an offering object");
        for key in [
            "offering_id",
            "vendor_id",
            "native_model_id",
            "native_revision",
            "provider_kind",
            "provider_model_id",
            "adapter_key",
            "formula",
            "cost_currency",
            "cost_rates",
            "enabled",
        ] {
            assert!(object.contains_key(key), "清单缺 `{key}`：{offering}");
        }
        Uuid::parse_str(offering["offering_id"].as_str().expect("offering_id"))
            .expect("offering_id 是一个 UUID");
        // 选择的键之外，清单不回显任何渠道部署事实。
        for forbidden in ["base_url", "credential_env"] {
            assert!(
                !object.contains_key(forbidden),
                "清单不回显渠道部署事实 `{forbidden}`：{offering}"
            );
        }
        match offering["formula"].as_str().expect("formula") {
            // 按 token 量计价：币种与四档费率都来自供给当前那行 Price Plan。
            "token_rates" => {
                assert_eq!(offering["cost_currency"], json!("USD"));
                assert_eq!(offering["cost_rates"]["currency"], json!("USD"));
                assert_eq!(
                    offering["cost_rates"]["text_input_microusd_per_million"],
                    json!(5_000_000),
                    "渠道费率取该供给当前那行 Price Plan：{offering}"
                );
            }
            // 没有 Price Plan、也没有发布物声明过币种：读侧给 `null`，不是报错。
            "per_image" => {
                assert!(
                    offering["cost_currency"].is_null(),
                    "没有币种来源时成本币种是 null：{offering}"
                );
                assert!(
                    offering["cost_rates"].is_null(),
                    "按张计价的供给没有四档费率：{offering}"
                );
            }
            // 按上游声明的金额计价：没有 Price Plan，成本币种来自供给自己的声明。
            "upstream_declared" => {
                assert_eq!(offering["cost_currency"], json!("USD"));
                assert!(
                    offering["cost_rates"].is_null(),
                    "声明金额形态没有四档费率：{offering}"
                );
            }
            other => panic!("清单里出现了没见过的计价形态 `{other}`：{offering}"),
        }
    }

    let order = offerings
        .iter()
        .map(|offering| {
            (
                offering["vendor_id"]
                    .as_str()
                    .expect("vendor_id")
                    .to_owned(),
                offering["native_model_id"]
                    .as_str()
                    .expect("native_model_id")
                    .to_owned(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec![
            ("AIHubMix".to_owned(), "alpha-model".to_owned()),
            ("AIHubMix".to_owned(), per_image_model.to_owned()),
            ("OpenAI".to_owned(), "zebra-model".to_owned()),
        ],
        "清单按 vendor_id → native_model_id 排序，发布页据此分组：{text}"
    );
    assert_eq!(
        offerings
            .iter()
            .find(|offering| offering["vendor_id"] == json!("OpenAI"))
            .expect("停用的那条仍在清单里")["enabled"],
        json!(false),
        "停用的供给要标明它选不了：{text}"
    );
    assert_eq!(
        offerings
            .iter()
            .find(|offering| offering["vendor_id"] == json!("AIHubMix"))
            .expect("启用那条")["enabled"],
        json!(true)
    );

    drop_isolated_database(&database_name).await;
}

/// **渠道能力**：对客选"按 token 四档"要求这条通路真的给得出四分项用量，拿不到就算不出对客价。
///
/// 能力是驱动器的事实（见 [`AdapterDescriptor::provides_token_usage`]）：AIHubMix 的成功件只有上游
/// 声明的金额、没有 token 分项，所以对客按 token 四档卖的候选在它身上发布期就拒并点名驱动器；
/// 对客选"上游声明金额 × 倍率"在它身上成立，因为它的终态给得出金额。APIMart 反过来：给得出
/// 四分项用量，按 token 四档卖成立（见 `cases_cost_facts`）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_token_priced_form_requires_a_channel_that_provides_token_usage() {
    let harness = Harness::start_with_draft(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let client = Client::new();
    let publish = async |offering: Value| {
        let body = publication_body(
            Harness::MODEL,
            "route-test-1",
            None,
            vec![offering],
            Some(2_000),
        );
        let response = client
            .post(format!("{}/api/v1/runtime-revisions", harness.base_url))
            .bearer_auth(&harness.admin_token)
            .json(&body)
            .send()
            .await
            .expect("runtime publication");
        let status = response.status();
        let text = response.text().await.expect("publication body");
        (status, text)
    };

    // 对客按 token 四档：这条通路给不出四分项用量。
    let mut token_priced = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    token_priced["base_url"] = Value::String(harness.upstream_base_url.clone());
    token_priced["consumer_formula"] = json!("token_rates");
    token_priced["consumer_rates_cny"] = json!({
        "text_input_micros_per_million": 42_600_000u64,
        "image_input_micros_per_million": 68_160_000u64,
        "text_output_micros_per_million": 85_200_000u64,
        "image_output_micros_per_million": 255_600_000u64
    });
    token_priced["reference_cost_microusd"] = json!(11_354);
    token_priced["cost_basis"] = json!("computed");
    token_priced["tier_prices"] = json!({});
    token_priced["floor_amounts"] = openai_floor_amounts();
    let (status, text) = publish(token_priced).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {text}");
    assert!(text.contains("provides no token usage"), "{text}");

    // 对客按上游声明金额：同一条通路成立——它的终态给得出金额。
    let mut declared = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    declared["base_url"] = Value::String(harness.upstream_base_url.clone());
    declared["reference_cost_microusd"] = json!(11_354);
    declared["cost_basis"] = json!("declared");
    declared["tier_prices"] = json!({});
    declared["floor_amounts"] = openai_floor_amounts();
    let (status, text) = publish(declared).await;
    assert_eq!(status, StatusCode::OK, "got {text}");

    harness.cleanup().await;
}

/// **清单里带两条渠道能力**（`declares_cost` 与 `provides_token_usage`）：它们分别决定"上游声明金额
/// × 倍率"与"按 token 四档"这两条对客形态成不成立——界面据此过滤下拉、发布期据此拒绝。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_offering_list_reports_the_channel_capabilities() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    // 素材构造体按渠道给对客形态：AIHubMix 只给上游声明的金额（要倍率），APIMart 给得出四分项
    // 用量（按 token 四档卖，倍率不参与计算）。
    let mut declared = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    declared["base_url"] = json!("https://declared.example.com");
    // 对客参考价目与成本形态无关：这条只回金额的供给照样声明它，清单原样带出来（设计 0007 §2）。
    declared["consumer_reference_rates"] = json!({
        "currency": "USD",
        "text_input_microusd_per_million": 5_000_000,
        "image_input_microusd_per_million": 8_000_000,
        "text_output_microusd_per_million": 10_000_000,
        "image_output_microusd_per_million": 30_000_000,
        "source_url": "https://example.invalid/list"
    });
    let mut token = candidate("APIMart", "apimart-image-v1", &["prompt_only"]);
    token["base_url"] = json!("https://token.example.com");
    for (model, offering, markup_bps) in [
        ("token-model", token, None),
        ("declared-model", declared, Some(2_000)),
    ] {
        let contract = surface_schema(json!({
            "model": {"const": model},
            "prompt": {"type": "string", "minLength": 1}
        }));
        let mut full = offering;
        full["provider_model_id"] = json!(model);
        full["capability_schema"]["properties"]["model"]["const"] = json!(model);
        let mut body = json!({
            "vendor_id": "OpenAI",
            "native_model_id": model,
            "native_revision": "route-test-1",
            "type": "image",
            "actor": "contract-test",
            "documentation": documentation_for(&contract),
            "capability_schema": contract,
            "offerings": [full]
        });
        if let Some(markup_bps) = markup_bps {
            body["markup_bps"] = json!(markup_bps);
        }
        let response = client
            .post(format!("{base_url}/api/v1/runtime-revisions"))
            .bearer_auth(&admin_token)
            .json(&body)
            .send()
            .await
            .expect("inline publication");
        let status = response.status();
        let text = response.text().await.expect("inline publication body");
        assert_eq!(status, StatusCode::OK, "{model} 应当发布成功：{text}");
    }

    let listing = client
        .get(format!("{base_url}/api/v1/offerings"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("offering list");
    let body: Value = listing.json().await.expect("offering list body");
    let offerings = body["offerings"].as_array().expect("offerings").clone();
    let find = |model: &str| {
        offerings
            .iter()
            .find(|item| item["native_model_id"] == json!(model))
            .unwrap_or_else(|| panic!("{model} 应当在清单里：{body}"))
            .clone()
    };
    // 两家都声明金额（AIHubMix 的任务终态给 `usage.cost`，APIMart 的终态给 `cost`），
    // 但只有 APIMart 给得出四分项用量：AIHubMix 的 `/ai/v1` 成功件没有 token 分项。
    assert_eq!(
        find("token-model")["declares_cost"],
        json!(true),
        "APIMart 的终态给实扣金额"
    );
    assert_eq!(
        find("token-model")["provides_token_usage"],
        json!(true),
        "APIMart 回四分项用量"
    );
    assert_eq!(
        find("declared-model")["declares_cost"],
        json!(true),
        "AIHubMix 的终态带 usage.cost"
    );
    assert_eq!(
        find("declared-model")["provides_token_usage"],
        json!(false),
        "AIHubMix 的成功件没有 token 分项"
    );
    // 参考价目随清单带出来：对客 token 四档的初始价取它；没声明的供给是 null，不造一份空价目。
    assert_eq!(
        find("declared-model")["consumer_reference_rates"]["currency"],
        json!("USD"),
        "声明了参考价目的供给原样带出来"
    );
    assert_eq!(
        find("declared-model")["consumer_reference_rates"]["text_input_microusd_per_million"],
        json!(5_000_000)
    );
    assert!(
        find("token-model")["consumer_reference_rates"].is_null(),
        "没声明参考价目的供给是 null"
    );

    drop_isolated_database(&database_name).await;
}
