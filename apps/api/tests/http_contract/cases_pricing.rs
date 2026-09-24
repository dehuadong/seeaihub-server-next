use super::*;

/// **定价随 Job 冻结，结算只读那份快照**。
///
/// 一次请求同时钉住四件事：售价按**命中候选**发布的那份对客 CNY 费率向量算（不是渠道成本
/// 费率）、保底额按请求的 `(size, quality)` 查该供给的保底表（**不由售价派生**）、汇率按该候选
/// 的成本币种取受理时刻生效的那一行并随快照冻结、成本记原币种原值并用冻结的汇率折出人民币。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn pricing_is_frozen_into_the_job_and_settlement_only_reads_that_snapshot() {
    let harness = Harness::start_with_draft(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let client = Client::new();
    assert_eq!(
        republish_priced(
            &harness,
            &client,
            openai_floor_amounts(),
            priced_consumer_rates(),
            2_000
        )
        .await,
        StatusCode::OK,
        "带定价的发布必须成功"
    );

    let (_, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let _worker = harness.spawn_worker();
    let key = format!("pricing-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "pricing contract");
    request["size"] = json!("2K");
    request["quality"] = json!("low");
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("定价与结算", &body);
    // 对客面**只有人民币**：响应里不出现成本侧那个币种。
    assert!(
        !body.to_string().contains("USD"),
        "对客响应不该出现外币：{body}"
    );

    let snapshot = frozen_snapshot(&harness.pool, &key).await;
    assert_eq!(
        snapshot["consumer_rates_cny"],
        priced_consumer_rates(),
        "对客费率向量必须与后台设定逐位一致"
    );
    assert_eq!(snapshot["hold_microusd"], json!(250_000), "2K 档的保底额");
    assert_eq!(snapshot["hold_source"], json!("tier"));
    assert_eq!(snapshot["cost_currency"], json!("USD"));
    assert_eq!(snapshot["reference_cost_microusd"], json!(11_354));
    assert_eq!(snapshot["cost_basis"], json!("computed"));
    assert_eq!(snapshot["markup_bps"], json!(2_000));
    assert_eq!(snapshot["fx_rate"]["currency"], json!("USD"));
    assert_eq!(snapshot["fx_rate"]["rate_micros"], json!(7_100_000));

    let (job_id, state, _) = harness.job(&key).await;
    assert_eq!(state, "succeeded");
    let offering_id: Uuid =
        sqlx::query_scalar("SELECT offering_id FROM generation.jobs WHERE id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("offering");
    assert_eq!(
        snapshot["hit_candidate"]["offering_id"],
        json!(offering_id.to_string()),
        "快照记的命中候选就是这次真正选中的那一条"
    );
    // 预授权额 = 保底额（不由售价派生）：Job 上的数与账本里的 hold 都是它。
    let authorized: i64 =
        sqlx::query_scalar("SELECT max_cost_microusd FROM generation.jobs WHERE id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("authorization");
    assert_eq!(authorized, 250_000);
    let held: i64 =
        sqlx::query_scalar("SELECT amount_microusd FROM ledger.holds WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("hold");
    assert_eq!(held, 250_000);
    // 实收 = 对客费率向量 × 实际用量：14 文本输入 × 40 + 196 图像输出 × 220（每 1M）。
    assert_eq!(harness.captured_microusd(job_id).await, -43_680);
    // 成本 = 原币种原值 + 折算后 CNY：5950 微美元 × 7.1 = 42245 微元。
    let (amount, currency, source, cny) = harness.attempt_cost(job_id).await;
    assert_eq!(amount, Some(5_950));
    assert_eq!(currency.as_deref(), Some("USD"));
    assert_eq!(source.as_deref(), Some("computed"));
    assert_eq!(cny, Some(42_245));
    // 毛利 = 售价（CNY）− 成本折算后 CNY，两条线分开留痕、可逐笔算出。
    assert_eq!(43_680 - 42_245, 1_435);
    // 余额 = 初始 − 实收（受理时先按保底额冻，结算按实际结清）。
    let balance_after_first = account_balance(&harness, job_id).await;
    assert_eq!(balance_after_first, 1_000_000 - 43_680);

    // ── 重发修订（换对客费率向量与加价系数）**不影响已受理的 Job** ──
    let mut higher = priced_consumer_rates();
    higher["image_output_micros_per_million"] = json!(440_000_000);
    assert_eq!(
        republish_priced(
            &harness,
            &client,
            openai_floor_amounts(),
            higher.clone(),
            3_000
        )
        .await,
        StatusCode::OK
    );
    let next_key = format!("pricing-next-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &next_key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let next_snapshot = frozen_snapshot(&harness.pool, &next_key).await;
    assert_eq!(
        next_snapshot["consumer_rates_cny"], higher,
        "新受理的 Job 用新价"
    );
    assert_eq!(next_snapshot["markup_bps"], json!(3_000));
    assert_eq!(
        frozen_snapshot(&harness.pool, &key).await,
        snapshot,
        "已受理 Job 的快照逐位不动"
    );
    assert_eq!(harness.captured_microusd(job_id).await, -43_680);
    // 第二笔按新价结算：14 文本输入 × 40 + 196 图像输出 × 440（每 1M） = 86800 微元。
    assert_eq!(
        account_balance(&harness, job_id).await,
        balance_after_first - 86_800,
        "旧 Job 的金额不动，新 Job 按新价扣"
    );

    harness.cleanup().await;
}

/// **售价按命中的那条候选算**：同一个网关模型的两个候选各带一份对客费率向量，实收按**命中**的
/// 那一份算，而且只随它变。
///
/// 两份向量故意差得很远（便宜那份算出来是 210 微元、正常那份是 43680 微元），拿错一份立刻露出来；
/// 用**承载面差异**把请求逼到优先级 1 的那条（优先级 0 的候选承载不了 `quality`）。随后只改
/// `reference_cost_microusd` 重发：参考成本只是定价参考，对客实收逐位不变。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_charge_follows_the_hit_candidate_and_ignores_the_reference_cost() {
    let harness = Harness::start_with_draft(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let client = Client::new();
    let model = Harness::MODEL;
    // 合同声明 `quality`（调用方能提交它），而**优先级 0** 的候选承载面里没有它——请求带上
    // `quality` 就一定落到优先级 1 的那条（选路规则：按优先级取第一个合格者）。
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string", "enum": ["low", "high"]}
    }));
    let narrow = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1}
    }));
    let wide = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string", "enum": ["low", "high"]}
    }));
    // 便宜得离谱的那份：这次的用量按它算只有 14 × 1 + 196 × 1 = 210 微元。
    let cheap = json!({
        "text_input_micros_per_million": 1_000_000,
        "image_input_micros_per_million": 1_000_000,
        "text_output_micros_per_million": 1_000_000,
        "image_output_micros_per_million": 1_000_000
    });

    let mut first = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    // 承载面走新名字：这一份发布带**模型级合同**，候选自带的旧 `capability_schema` 不该再充当
    // 合同（两份不同的旧字段会让归一期拒掉整份发布）。
    first["carrier_schema"] = narrow;
    first
        .as_object_mut()
        .expect("a draft object")
        .remove("capability_schema");
    first["base_url"] = Value::String(harness.upstream_base_url.clone());
    first["reference_cost_microusd"] = json!(11_354);
    first["cost_basis"] = json!("computed");
    first["consumer_rates_cny"] = cheap;
    first["tier_prices"] = json!({});
    first["floor_amounts"] = openai_floor_amounts();

    let mut second = first.clone();
    second["carrier_schema"] = wide;
    second["reference_cost_microusd"] = json!(999_999);
    second["consumer_rates_cny"] = priced_consumer_rates();

    // 在克隆**之后**把**不被命中**的那条换到另一个渠道：命中那条保持 AIHubMix，
    // 请求才会打到本用例已经起好的假上游。
    first["provider_kind"] = json!("APIMart");
    first["adapter_key"] = json!("apimart-image-v1");

    // 加价系数**不给**：对客费率向量是直接录入的，那一步用不上它（发布期不再强制）。
    assert_eq!(
        publish_on_revision(
            &harness,
            model,
            "two-candidates-1",
            contract.clone(),
            vec![first.clone(), second.clone()],
            None,
        )
        .await,
        StatusCode::OK,
        "直接录入对客费率向量、不填加价系数也必须发得出去"
    );

    let (_, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let _worker = harness.spawn_worker();
    let mut request = route_request(harness.model, "hit candidate");
    request["quality"] = json!("low");
    let key = format!("hit-candidate-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let (job_id, state, _) = harness.job(&key).await;
    assert_eq!(state, "succeeded");

    let snapshot = frozen_snapshot(&harness.pool, &key).await;
    assert_eq!(
        snapshot["consumer_rates_cny"],
        priced_consumer_rates(),
        "快照冻的是**命中候选**那一份向量"
    );
    let hit: Uuid = snapshot["hit_candidate"]["offering_id"]
        .as_str()
        .expect("the frozen snapshot names the hit candidate")
        .parse()
        .expect("an offering id");
    let chosen: Uuid = sqlx::query_scalar(
        "SELECT re.offering_id FROM publication.runtime_entries re
         WHERE re.active AND re.gateway_model = $1 AND re.routing_priority = 1",
    )
    .bind(model)
    .fetch_one(&harness.pool)
    .await
    .expect("the priority 1 offering");
    assert_eq!(
        hit, chosen,
        "承载不了 `quality` 的那条落选，这次请求落到下一条"
    );
    // 实收按**命中候选**的向量算：14 文本输入 × 40 + 196 图像输出 × 220（每 1M）。
    assert_eq!(
        harness.captured_microusd(job_id).await,
        -43_680,
        "拿优先级 0 那份便宜向量算就是 -210，两者差得很远"
    );

    // ── 只改参考成本重发：对客实收只随对客费率向量变 ──
    // 取值要留在**成本护栏**之下（默认 10 元；按 7.1 的折算率约合 1_408_450 微美元）：它一旦被那道
    // 护栏判成"配得离谱"，发布会被整份拒——那时拒绝来自护栏，与这里要验的"参考成本不参与对客实收"
    // 无关，用例会变成一个在验另一件事的用例。1_200_000 微美元约合 8.52 元，既明显不同于原值，
    // 又留有边界余量。
    let mut repriced = second.clone();
    repriced["reference_cost_microusd"] = json!(1_200_000);
    assert_eq!(
        publish_on_revision(
            &harness,
            model,
            "two-candidates-2",
            contract,
            vec![first, repriced],
            None,
        )
        .await,
        StatusCode::OK
    );
    let next_key = format!("hit-candidate-next-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &next_key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let (next_job, state, _) = harness.job(&next_key).await;
    assert_eq!(state, "succeeded");
    let next_snapshot = frozen_snapshot(&harness.pool, &next_key).await;
    assert_eq!(
        next_snapshot["reference_cost_microusd"],
        json!(1_200_000),
        "重发确实换掉了参考成本（否则下面那条断言就是空的）"
    );
    assert_eq!(
        harness.captured_microusd(next_job).await,
        -43_680,
        "参考成本只是定价参考：对客实收只随 `consumer_rates_cny` 变"
    );
    assert_eq!(
        harness.captured_microusd(job_id).await,
        -43_680,
        "已受理 Job 的金额不动"
    );

    harness.cleanup().await;
}

/// **保底按供给维度查表**：先把这次请求的 `size` 归到档位，再查表；查不到走该供给封顶保底值。
///
/// 归位规则（设计 §6）：像素型 `size` 先按该供给发布的档位像素表反向查、缺失时按**最长边**
/// 阈值兜底；`auto`（与没给 `size` 同义）取**默认档 2K**；比例型归不出档位。
/// `quality` 维留空即按 `size` 档：表里没为某个质量单列时，带任意质量都查到同一个档位。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_hold_resolves_the_tier_then_walks_the_supply_floor_chain() {
    let harness = Harness::start_with_draft(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let client = Client::new();
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

    // 档位查表：2K = ¥0.25。
    assert_eq!(
        hold_for(&harness, &api_key, json!({"size": "2K"})).await,
        (250_000, "tier".to_owned())
    );
    // 档位写法的大小写不影响查表：调用方的 `2k` 与管理员的 `2K` 是同一个档。
    assert_eq!(
        hold_for(&harness, &api_key, json!({"size": "2k"})).await,
        (250_000, "tier".to_owned())
    );
    // `quality` 维留空即按 `size` 档：带任意质量都查到同一个档位。
    for quality in ["low", "high", "xhigh", "auto"] {
        assert_eq!(
            hold_for(
                &harness,
                &api_key,
                json!({"size": "2K", "quality": quality})
            )
            .await,
            (250_000, "tier".to_owned()),
            "quality={quality}"
        );
    }
    // **像素型 `size` 先归到档位**（用户口径：按分辨率保底、通过 `size` 判断 1K/2K/4K）：
    // 这条供给没发布尺寸档案，所以按**最长边**阈值兜底。
    for (size, amount) in [
        ("1024x1024", 160_000), // 最长边 1024 ⇒ 1K = ¥0.16
        ("2048x2048", 250_000), // 最长边 2048 ⇒ 2K = ¥0.25
        ("3840x2160", 300_000), // 最长边 3840 ⇒ 4K = ¥0.3
    ] {
        assert_eq!(
            hold_for(&harness, &api_key, json!({"size": size})).await,
            (amount, "tier".to_owned()),
            "size={size} 必须按它归出来的档位查保底"
        );
    }
    // `size = auto`（与没给 `size` 同义）⇒ 默认档 2K（中间档）。
    assert_eq!(
        hold_for(&harness, &api_key, json!({"size": "auto"})).await,
        (250_000, "auto_tier".to_owned())
    );
    assert_eq!(
        hold_for(&harness, &api_key, json!({})).await,
        (250_000, "auto_tier".to_owned())
    );
    // **空串不是"没给"**：`size` 的字面量就是调用方说的那个尺寸，归不出档位就回落封顶保底值
    // ——只有字段缺失或字面 `auto` 才走默认档 2K。
    assert_eq!(
        hold_for(&harness, &api_key, json!({"size": ""})).await,
        (300_000, "supply_cap".to_owned()),
        "空串与'没给这个字段'必须落到不同的保底额上"
    );
    // 比例型只说了形状、没说分辨率 ⇒ 归不出档位 ⇒ 回落该供给的封顶保底值。
    assert_eq!(
        hold_for(&harness, &api_key, json!({"size": "16:9"})).await,
        (300_000, "supply_cap".to_owned())
    );

    harness.cleanup().await;
}

/// **没有定价的旧修订与空保底表都回落到平台兜底数**（`GENERATION_MAX_COST_MICROUSD`）。
///
/// 前者的预授权与结算都走旧口径、与今天逐位相同；后者有定价（售价按对客费率向量），只是连该
/// 供给的封顶保底值都没有——来源记的是平台兜底，事后分得清。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_unpriced_revision_and_an_empty_floor_table_fall_back_to_the_platform_default() {
    let harness = Harness::start_with_draft(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let client = Client::new();
    let (_, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let _worker = harness.spawn_worker();

    // 1) 没有定价的修订：快照里没有对客费率向量，预授权回落平台兜底数，结算按已发布费率。
    let key = format!("unpriced-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request(harness.model, "unpriced revision"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let (job_id, state, _) = harness.job(&key).await;
    assert_eq!(state, "succeeded");
    let snapshot = frozen_snapshot(&harness.pool, &key).await;
    assert!(
        snapshot["consumer_rates_cny"].is_null(),
        "旧口径的快照不带对客费率向量：{snapshot}"
    );
    assert!(snapshot["hold_microusd"].is_null());
    let authorized: i64 =
        sqlx::query_scalar("SELECT max_cost_microusd FROM generation.jobs WHERE id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("authorization");
    assert_eq!(
        authorized, 20_000,
        "没有定价时预授权回落平台兜底数（今天的行为）"
    );
    assert_eq!(
        harness.captured_microusd(job_id).await,
        -5_950,
        "结算也走旧口径：已发布费率 × 实际用量"
    );

    // 2) 有定价、但保底表里什么都没有：连封顶保底值也没有 ⇒ 平台兜底。
    assert_eq!(
        republish_priced(&harness, &client, json!({}), priced_consumer_rates(), 2_000).await,
        StatusCode::OK
    );
    assert_eq!(
        hold_for(&harness, &api_key, json!({"size": "2K"})).await,
        (20_000, "platform_default".to_owned())
    );

    harness.cleanup().await;
}

/// **透支**：实收超过保底额时余额被扣成负数，随后同一账户再发请求按当时余额判 402。
///
/// 受理闸门是"余额 ≥ 保底额"——保底额估小了由**结算**吸收，估大了结算释放差额；透支不是错误，
/// 也不进对账（对账态是"受理/执行状态不明"，会把消费者的钱扣在对账里）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_overdraft_settles_into_a_negative_balance_and_the_next_request_is_refused() {
    let harness = Harness::start_with_draft(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let client = Client::new();
    // 保底额 ¥0.001（1000 微元），而这次生成实际要 ¥0.043680：估小了。
    assert_eq!(
        republish_priced(
            &harness,
            &client,
            json!({"amounts": {"1K": 1_000}, "cap_microusd": 1_000}),
            priced_consumer_rates(),
            2_000
        )
        .await,
        StatusCode::OK
    );
    let (_, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000).await;
    let _worker = harness.spawn_worker();

    let key = format!("overdraft-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "overdraft");
    request["size"] = json!("1K");
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "受理闸门是'余额 ≥ 保底额'：1000 ≥ 1000，照常受理。got {body}"
    );
    let (job_id, state, _) = harness.job(&key).await;
    assert_eq!(state, "succeeded");
    assert_eq!(harness.captured_microusd(job_id).await, -43_680);
    let balance = account_balance(&harness, job_id).await;
    assert_eq!(balance, 1_000 - 43_680, "结算按实际扣：差额把余额扣成负数");
    assert!(balance < 0);
    let cases: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operations.reconciliation_cases WHERE job_id = $1",
    )
    .bind(job_id)
    .fetch_one(&harness.pool)
    .await
    .expect("cases");
    assert_eq!(cases, 0, "透支不是'状态不明'，不该把消费者的钱扣在对账里");

    // 随后同一个账户再发一次：按当时（负）余额判 ⇒ 402，不产生 Job、不扣款。
    let refused_key = format!("overdraft-refused-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &refused_key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED, "got {body}");
    assert_eq!(body["error"]["code"], json!("insufficient_balance"));
    let created: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.jobs WHERE idempotency_key = $1")
            .bind(&refused_key)
            .fetch_one(&harness.pool)
            .await
            .expect("refused jobs");
    assert_eq!(created, 0, "被拒的受理不产生 Job");
    assert_eq!(
        account_balance(&harness, job_id).await,
        balance,
        "被拒的受理不扣款"
    );

    harness.cleanup().await;
}

/// **未指定 `effective_at` 时，落库的生效时刻由数据库决定**，不由 API 进程的时钟盖章。
///
/// 判据是"同一事务里两个库侧时刻必须逐位相同"：折算率那一行的 `effective_at` 由库的 `now()`
/// 盖章，同一事务里那条审计事件的 `created_at` 也是库的 `now()`。若改回由进程时钟盖章，两者
/// 会差出宿主与容器的时钟漂移——那正是"录完折算率立刻发布"被判成"该币种还没有生效的折算率"
/// 的成因（发布期校验与受理取值比的都是库的 `now()`）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_fx_rate_without_an_effective_time_is_stamped_by_the_database_clock() {
    let harness = Harness::start(UpstreamBehaviour::aihubmix(SyncImageShape::Url)).await;
    let client = Client::new();

    let response = client
        .put(format!("{}/api/v1/fx-rates", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"currency": "GBP", "rate_micros": 8_800_000u64}))
        .send()
        .await
        .expect("fx rate without effective_at");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let same_clock: bool = sqlx::query_scalar(
        r#"
        SELECT f.effective_at = a.created_at
        FROM pricing.fx_rates f
        JOIN operations.audit_events a
          ON a.action = 'fx_rate.upsert' AND a.subject_id = f.currency
        WHERE f.currency = 'GBP'
        "#,
    )
    .fetch_one(&harness.pool)
    .await
    .expect("stamped fx rate row and its audit event");
    assert!(
        same_clock,
        "未指定生效时刻的折算率必须由库盖章：它的生效时刻要与同一事务里那条审计事件的库侧时间戳相同"
    );

    // 同一事实的另一面：库里不该出现一行"还没生效"的折算率——发布期校验看到的就是这些行。
    let pending: i64 =
        sqlx::query_scalar("SELECT count(*) FROM pricing.fx_rates WHERE effective_at > now()")
            .fetch_one(&harness.pool)
            .await
            .expect("pending fx rates");
    assert_eq!(pending, 0, "库盖章的行落库即生效，不会落在库的 now() 之后");

    harness.cleanup().await;
}

/// 管理员读：`GET /api/v1/gateway-models` 列出每个候选的定价与修订级加价系数，不用直查库。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_admin_view_lists_the_published_pricing() {
    let harness = Harness::start_with_draft(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let client = Client::new();
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

    let (status, admin) =
        get_gateway_models(&client, &harness.base_url, Some(&harness.admin_token)).await;
    assert_eq!(status, StatusCode::OK, "{admin}");
    let view = &admin["gateway_models"][0];
    assert_eq!(view["markup_bps"], json!(2_000), "加价系数是修订级的");
    let candidate = &view["candidates"][0];
    assert_eq!(candidate["consumer_rates_cny"], priced_consumer_rates());
    assert_eq!(candidate["reference_cost_microusd"], json!(11_354));
    assert_eq!(candidate["cost_currency"], json!("USD"));
    assert_eq!(candidate["cost_basis"], json!("computed"));
    assert_eq!(candidate["tier_prices"]["2K"], json!(250_000));
    assert_eq!(candidate["floor_amounts"]["amounts"]["2K"], json!(250_000));
    assert_eq!(candidate["floor_amounts"]["cap_microusd"], json!(300_000));

    harness.cleanup().await;
}

/// **按张计价的实收 = 成本单价 × 倍率 × 折算率 × 本次产出的张数**。
///
/// 这条用例同时钉住两件事：
/// - **倍率是数据，不是常量**：同一个成本单价分别用 2000 与 5000 基点各发一次，实收按倍率之比
///   变化（170_400 与 213_000，恰好成比例）；代码里没有倍率的默认值，也没有"按 20% 算"这种写法。
/// - **改价＝发布新修订**：第一次受理的 Job 在第二次发布之后金额逐位不动。
///
/// 单价取 20_000 微美元是为了两个倍率都整除，于是"成比例"可以逐位断言而不是近似断言。
/// 假上游固定回一张图，"张数"那一维由领域用例逐位钉住（两张就是每张价的两倍）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_per_image_charge_follows_the_markup_coefficient() {
    let harness = Harness::start_with_draft(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let client = Client::new();
    let _worker = harness.spawn_worker();
    let request = route_request(harness.model, "priced per image");

    assert_eq!(
        republish_candidate(
            &harness,
            &client,
            unit_candidate("per_image", 20_000, "USD"),
            2_000,
        )
        .await,
        StatusCode::OK
    );
    let key = format!("per-image-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let (cheap_job, state, _) = harness.job(&key).await;
    assert_eq!(state, "succeeded");
    let snapshot = frozen_snapshot(&harness.pool, &key).await;
    assert_eq!(snapshot["formula"], json!("per_image"));
    assert_eq!(snapshot["cost_unit_price_microusd"], json!(20_000));
    assert_eq!(snapshot["markup_bps"], json!(2_000));
    assert!(
        snapshot["consumer_rates_cny"].is_null(),
        "按张的候选没有对客四档向量：对客价由成本单价乘倍率算出来：{snapshot}"
    );
    // 20000 微美元 × 1.2 × 7.1 = 170400 微元每张。
    assert_eq!(harness.captured_microusd(cheap_job).await, -170_400);

    // 同一个单价、同一份执行事实，只把倍率换成 5000 基点。
    assert_eq!(
        republish_candidate(
            &harness,
            &client,
            unit_candidate("per_image", 20_000, "USD"),
            5_000,
        )
        .await,
        StatusCode::OK
    );
    let next_key = format!("per-image-next-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &next_key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let (dear_job, state, _) = harness.job(&next_key).await;
    assert_eq!(state, "succeeded");
    // 20000 微美元 × 1.5 × 7.1 = 213000 微元每张。
    assert_eq!(harness.captured_microusd(dear_job).await, -213_000);
    assert_eq!(
        170_400 * 15,
        213_000 * 12,
        "实收之比必须等于倍率之比（1.2 : 1.5）"
    );
    assert_eq!(
        harness.captured_microusd(cheap_job).await,
        -170_400,
        "改价是发布新修订：已受理的 Job 不受影响"
    );

    harness.cleanup().await;
}

/// **按次计价只收一次的钱**：与用量、产出张数都无关，一次就是"成本单价 × 倍率 × 折算率"。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_per_call_charge_is_one_unit_regardless_of_the_usage() {
    let harness = Harness::start_with_draft(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let client = Client::new();
    assert_eq!(
        republish_candidate(
            &harness,
            &client,
            unit_candidate("per_call", 20_000, "USD"),
            2_000,
        )
        .await,
        StatusCode::OK
    );
    let key = format!("per-call-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "priced per call"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let (job_id, state, _) = harness.job(&key).await;
    assert_eq!(state, "succeeded");
    let snapshot = frozen_snapshot(&harness.pool, &key).await;
    assert_eq!(snapshot["formula"], json!("per_call"));
    assert_eq!(snapshot["cost_unit_price_microusd"], json!(20_000));
    // 20 微美元一次 × 1.2 × 7.1 = 170400 微元；这次用量与产出一张都不参与。
    assert_eq!(harness.captured_microusd(job_id).await, -170_400);

    harness.cleanup().await;
}
