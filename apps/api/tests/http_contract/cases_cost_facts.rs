use super::*;

/// 渠道声明了会给金额，这次却**拿不到**（终态里没有这个字段）⇒ 不猜：
/// 金额与币种留空、来源记 `unavailable`，缺口查得出来；对客结算照常完成。
///
/// 这是"成本缺口"与"执行失败"的分界：缺口是平台侧的账务问题，不该把消费者的钱扣在对账里。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_declared_cost_that_never_arrives_is_recorded_as_a_gap_not_guessed() {
    let mut behaviour = UpstreamBehaviour::apimart();
    behaviour.declared_cost = None;
    let harness = Harness::start(behaviour).await;
    let key = format!("cost-gap-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "cost never arrives"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("成本缺口", &body);

    let (job_id, state, _) = harness.job(&key).await;
    assert_eq!(state, "succeeded", "成本缺口不是执行失败：对客结算照常完成");
    let (amount, currency, source, cny) = harness.attempt_cost(job_id).await;
    assert_eq!(source.as_deref(), Some("unavailable"));
    assert_eq!(amount, None, "拿不到金额就留空：不写 0、也不用费率顶替");
    assert_eq!(currency, None);
    assert_eq!(cny, None);
    // 缺口可发现：按来源筛得出来，不用去翻上游账单才知道有这么一笔。
    let gaps: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM generation.attempts \
         WHERE job_id = $1 AND provider_cost_source = 'unavailable'",
    )
    .bind(job_id)
    .fetch_one(&harness.pool)
    .await
    .expect("gap query");
    assert_eq!(gaps, 1);
    // 对客实收不受成本缺口影响。
    assert_eq!(harness.captured_microusd(job_id).await, -5_950);
    harness.cleanup().await;
}

/// 上游终态**给了金额、却没有任何结果图** ⇒ 失败件照样按 `declared` 落四列：
/// 金额取上游声明值、币种取渠道声明。
///
/// 没有结果图是"结果没交付"，不是"钱没花"：金额在终态里就已经解析到手，而终态之后的失败是它
/// 唯一的落点——丢掉它，这笔真实成本既不在账上也不在缺口里。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_terminal_without_images_still_records_the_cost_it_already_declared() {
    let mut behaviour = UpstreamBehaviour::apimart();
    behaviour.terminal_without_images = true;
    let harness = Harness::start(behaviour).await;
    let key = format!("cost-empty-result-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "terminal without images"),
        )
        .await;
    assert_ne!(
        status,
        StatusCode::OK,
        "没有结果图的那次执行是失败的：{body}"
    );

    let (job_id, state, _) = harness.job(&key).await;
    // 失败件的处置不变：受理状态不明仍然进对账。成本事实与这条处置无关，只是不再跟着结果丢。
    assert_eq!(state, "reconciliation_required", "没有结果图不该算成功");
    let (amount, currency, source, cny) = harness.attempt_cost(job_id).await;
    assert_eq!(
        source.as_deref(),
        Some("declared"),
        "上游声明了金额就直接取它：没有结果图不代表这笔钱没花"
    );
    assert_eq!(amount, Some(11_354), "金额是上游声明的 0.011354");
    assert_eq!(currency.as_deref(), Some("USD"), "币种按渠道声明");
    assert_eq!(
        cny,
        Some(80_614),
        "终态给了金额就已经能折算了：受理时冻结的汇率把 11354 微美元折成人民币"
    );
    harness.cleanup().await;
}

/// 上游终态**没有金额字段**、也没有结果图 ⇒ 来源落 `unavailable`：金额与折算值留空，
/// 而且这一笔**出现在成本缺口清单里**（清单的判据就是来源是 `unavailable`）。
///
/// `unavailable` 是"本该有金额却拿不到"的显式事实，NULL 是"根本没采"；留 NULL 的话，这笔
/// 成本在账上与缺口两头都看不见，运营也就无从核账单。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_terminal_without_images_or_amount_lands_in_the_cost_gap_list() {
    let mut behaviour = UpstreamBehaviour::apimart();
    behaviour.declared_cost = None;
    behaviour.terminal_without_images = true;
    let harness = Harness::start(behaviour).await;
    let key = format!("cost-empty-no-amount-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "terminal without images or amount"),
        )
        .await;
    assert_ne!(
        status,
        StatusCode::OK,
        "没有结果图的那次执行是失败的：{body}"
    );

    let (job_id, state, _) = harness.job(&key).await;
    assert_ne!(state, "succeeded", "没有结果图不该算成功");
    let (amount, currency, source, cny) = harness.attempt_cost(job_id).await;
    assert_eq!(source.as_deref(), Some("unavailable"));
    assert_eq!(amount, None, "拿不到金额就留空：不写 0、也不用费率顶替");
    assert_eq!(currency, None);
    assert_eq!(cny, None);
    let gaps: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM generation.attempts \
         WHERE job_id = $1 AND provider_cost_source = 'unavailable'",
    )
    .bind(job_id)
    .fetch_one(&harness.pool)
    .await
    .expect("gap query");
    assert_eq!(
        gaps, 1,
        "来源是 `unavailable` ⇒ 这笔进成本缺口清单，运营核账单时看得到它"
    );
    harness.cleanup().await;
}

/// 渠道**不给任何金额字段**（AIHubMix）⇒ 成本按本次实际用量与该渠道四档费率自算，
/// 币种按该渠道声明。它只进成本口径：对客实收另有出处（账本），两者不是同一个量。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_cost_the_channel_never_reports_is_computed_from_the_actual_usage() {
    let harness = Harness::start(UpstreamBehaviour::aihubmix(SyncImageShape::Url)).await;
    let key = format!("cost-computed-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "computed cost"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("自算成本", &body);

    let (job_id, state, _) = harness.job(&key).await;
    assert_eq!(state, "succeeded");
    let (amount, currency, source, cny) = harness.attempt_cost(job_id).await;
    assert_eq!(source.as_deref(), Some("computed"));
    assert_eq!(currency.as_deref(), Some("USD"));
    // 实际用量：14 文本输入 × 5 + 196 图像输出 × 30（每 1M） = 5950 微单位。
    assert_eq!(amount, Some(5_950));
    assert_eq!(
        cny,
        Some(42_245),
        "受理时冻结的汇率把自算出来的成本折成人民币（5950 × 7.1 向上取整），毛利要用它"
    );
    assert_eq!(harness.captured_microusd(job_id).await, -5_950);
    harness.cleanup().await;
}

/// 渠道**直接在上游终态给实扣金额**的供给不需要那份四档费率表：它能发布、能受理，成本取上游
/// 声明的金额（平台没有可算的参数，也不自己编一个）。
///
/// 素材里那条 APIMart 供给就是这种事实（终态给 `cost`）：它**没有 Price Plan**，所以
/// `runtime_entries.price_plan_id` 落 NULL——放开的正是这一条，而不是"`token_rates` 也可以
/// 不发费率表"。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_upstream_declared_supply_needs_no_rate_card_and_takes_the_upstream_amount() {
    let harness = Harness::start_with_bootstrap(UpstreamBehaviour::apimart(), 64).await;
    // 发布成功的判据在夹具里（`publication must succeed`）：这条供给没有价目表也发得出去。
    let row = sqlx::query(
        "SELECT o.formula, re.price_plan_id,
                (SELECT count(*) FROM pricing.price_plans p WHERE p.offering_id = o.id) AS plans
         FROM publication.runtime_entries re
         JOIN supply.offerings o ON o.id = re.offering_id
         WHERE re.active AND re.gateway_model = $1",
    )
    .bind(Harness::MODEL)
    .fetch_one(&harness.pool)
    .await
    .expect("the published candidate");
    let formula: String = row.try_get("formula").expect("formula");
    assert_eq!(
        formula, "upstream_declared",
        "素材里这条供给登记的计价形态就是它"
    );
    assert!(
        row.try_get::<Option<Uuid>, _>("price_plan_id")
            .expect("price plan id")
            .is_none(),
        "没有 Price Plan：价目表的非空约束已放开"
    );
    let plans: i64 = row.try_get("plans").expect("price plan rows");
    assert_eq!(plans, 0);

    let key = format!("upstream-declared-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "declared amount"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("上游直接给金额", &body);
    let (job_id, state, _) = harness.job(&key).await;
    assert_eq!(state, "succeeded", "受理与结算照常");

    let (amount, currency, source, cny) = harness.attempt_cost(job_id).await;
    assert_eq!(source.as_deref(), Some("declared"));
    assert_eq!(amount, Some(11_354), "上游声明的 0.011354 直接取");
    assert_eq!(currency.as_deref(), Some("USD"));
    assert_eq!(
        cny,
        Some(80_614),
        "受理时冻结的折算率把上游声明的金额折成人民币算毛利"
    );

    let snapshot = frozen_snapshot(&harness.pool, &key).await;
    assert_eq!(snapshot["formula"], json!("upstream_declared"));
    assert!(snapshot["price_plan_id"].is_null());
    assert!(
        snapshot["rates"].is_null(),
        "没有 Price Plan 就没有那份四档费率：{snapshot}"
    );
    assert!(
        snapshot["consumer_rates_cny"].is_null(),
        "对客四档向量是 token 计量量那一种形态的价格，这条供给没有它：{snapshot}"
    );
    assert_eq!(
        snapshot["hold_source"],
        json!("auto_tier"),
        "请求没给 `size` ⇒ 默认档 2K，按该供给的保底表冻：{snapshot}"
    );
    let authorized: i64 =
        sqlx::query_scalar("SELECT max_cost_microusd FROM generation.jobs WHERE id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("authorization");
    assert_eq!(authorized, 250_000, "保底额是 2K 档的 ¥0.25");
    // 对客实收 = **上游声明的金额 × 倍率 × 折算率**（11354 × 1.2 × 7.1 = 96736.08 ⇒ 96737），
    // 与成本折算（11354 × 7.1 = 80613.4 ⇒ 80614）是两个量：后者只进毛利口径。
    assert_eq!(harness.captured_microusd(job_id).await, -96_737);
    assert_eq!(96_737 - 80_614, 16_123, "毛利 = 售价 − 成本折算后 CNY");
    harness.cleanup().await;
}

/// 币种**按渠道声明接受**：声明 `CNY` 的供给不再被发布期硬拒，落库的币种就是声明值。
///
/// 能发布出来本身就证明那条"必须是 USD"的硬校验已经不在了；而成本列里的币种证明它不是
/// 被平台替换成某个默认币种，而是**照声明的原值**记下来的。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_channel_declared_currency_other_than_usd_is_accepted_and_recorded() {
    let harness = Harness::start_with(
        "APIMart",
        "apimart-image-v1",
        &["prompt_only"],
        Some("CNY"),
        UpstreamBehaviour::apimart(),
        64,
    )
    .await;
    let key = format!("cost-currency-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "declared currency"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("按声明币种", &body);

    let (job_id, _, _) = harness.job(&key).await;
    let (amount, currency, source, _) = harness.attempt_cost(job_id).await;
    assert_eq!(source.as_deref(), Some("declared"));
    assert_eq!(amount, Some(11_354));
    assert_eq!(
        currency.as_deref(),
        Some("CNY"),
        "币种权威是该供给声明的那个值，平台不替换成 USD"
    );
    harness.cleanup().await;
}

/// **上游声明的金额直接取，并用冻结的汇率折出人民币**（`declared` 那一态）。
///
/// 上游声明的是 11354 微美元，而按该渠道成本费率自算是 5950——两个数不同，正好钉住"声明就
/// 直接取、不自己算"。对客金额只由受理时冻结的对客费率向量决定，实际成本只进毛利口径。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_declared_cost_is_taken_as_is_and_converted_with_the_frozen_rate() {
    let harness = Harness::start(UpstreamBehaviour::apimart()).await;
    let client = Client::new();
    // 承载面与首次发布那一份**逐字一致**：合同是模型级唯一一份且不可变，换了面会被发布期拒。
    let mut draft = candidate(
        "APIMart",
        "apimart-image-v1",
        &["prompt_only", "image_conditioned", "masked"],
    );
    draft["base_url"] = Value::String(harness.upstream_base_url.clone());
    draft["reference_cost_microusd"] = json!(11_354);
    draft["cost_basis"] = json!("declared");
    draft["consumer_rates_cny"] = priced_consumer_rates();
    draft["tier_prices"] = json!({});
    draft["floor_amounts"] = openai_floor_amounts();
    assert_eq!(
        publish_candidates_with_markup(
            &client,
            &harness.base_url,
            &harness.admin_token,
            Harness::MODEL,
            None,
            vec![draft],
            Some(2_000),
        )
        .await,
        StatusCode::OK
    );

    let (_, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let _worker = harness.spawn_worker();
    let key = format!("declared-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request(harness.model, "declared cost"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let (job_id, state, _) = harness.job(&key).await;
    assert_eq!(state, "succeeded");
    let (amount, currency, source, cny) = harness.attempt_cost(job_id).await;
    assert_eq!(amount, Some(11_354), "上游给了金额就直接取它，不自己算");
    assert_eq!(currency.as_deref(), Some("USD"));
    assert_eq!(source.as_deref(), Some("declared"));
    assert_eq!(cny, Some(80_614), "11354 微美元 × 7.1 = 80613.4 ⇒ 向上取整");
    assert_eq!(
        harness.captured_microusd(job_id).await,
        -43_680,
        "实际成本不改对客金额：对客金额只由冻结的对客费率向量决定"
    );
    // 毛利 = 售价（CNY）− 成本折算后 CNY：这一笔是负的（参考成本只是发布时的定价参考，
    // 上游实际声明的金额比它高），照样能逐笔算出——不猜、不掩盖。
    assert_eq!(43_680 - 80_614, -36_934);

    harness.cleanup().await;
}

/// **同币种也要能录折算率**：`CNY → CNY = 1` 录得进去，声明 `cost_currency: "CNY"` 的供给因此
/// 能发布、受理、结算，而且**对客实收不因折算而变化**——乘的是率恒为 1 的那一行。
///
/// 渠道币种不是"全平台统一美元"：按张计价的人民币渠道（例如方舟）就是这一条。代码里没有
/// "这个币种不用折算"的分支——那条路走的就是折算率表里率 1 的那一行。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_cny_supply_publishes_and_is_charged_without_conversion() {
    let harness = Harness::start_with_draft(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let client = Client::new();

    // 1) 同币种那一行录得进去（率 1）。
    let response = client
        .put(format!("{}/api/v1/fx-rates", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"currency": "CNY", "rate_micros": 1_000_000u64}))
        .send()
        .await
        .expect("same-currency fx rate");
    assert_eq!(
        response.status(),
        StatusCode::NO_CONTENT,
        "CNY → CNY = 1 必须能录进折算率表"
    );

    // 2) 声明 CNY、按张计价的供给：发布、受理、结算都跑得通。单价取方舟 pro 低档的 ¥0.30/张。
    assert_eq!(
        republish_candidate(
            &harness,
            &client,
            unit_candidate("per_image", 300_000, "CNY"),
            2_000,
        )
        .await,
        StatusCode::OK,
        "同币种的供给必须发得出去"
    );
    let key = format!("cny-per-image-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "cny per image"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("同币种按张", &body);
    let (job_id, state, _) = harness.job(&key).await;
    assert_eq!(state, "succeeded");

    let snapshot = frozen_snapshot(&harness.pool, &key).await;
    assert_eq!(snapshot["cost_currency"], json!("CNY"));
    assert_eq!(snapshot["fx_rate"]["currency"], json!("CNY"));
    assert_eq!(
        snapshot["fx_rate"]["rate_micros"],
        json!(1_000_000),
        "同币种的折算率就是 1：{snapshot}"
    );
    // 对客实收 = 成本单价 300_000 微元 × 倍率 1.2 = 360_000（一张）；折算率 1 不改数。
    assert_eq!(
        harness.captured_microusd(job_id).await,
        -360_000,
        "同币种不产生折算：对客价就是成本单价乘倍率"
    );
    let (amount, currency, source, cny) = harness.attempt_cost(job_id).await;
    assert_eq!(amount, Some(300_000), "按张的成本 = 一张 × 单价");
    assert_eq!(currency.as_deref(), Some("CNY"));
    assert_eq!(source.as_deref(), Some("computed"));
    assert_eq!(cny, Some(300_000), "率 1 折出来逐位不变");

    harness.cleanup().await;
}

/// **成本缺口**的处置：不进对账态、对客结算照常完成，运营从缺口清单里看得到它。
///
/// 补录金额归账实核对那条线（另一张工单）；补录完成后这一笔不再出现在清单里，所以清单就是
/// "当前还有哪些缺口"的答案。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_cost_gap_is_listed_for_operations_without_pushing_the_job_into_reconciliation() {
    let mut behaviour = UpstreamBehaviour::apimart();
    // 渠道声明了金额却拿不到（终态没有 `cost` 字段）⇒ 成本缺口。
    behaviour.declared_cost = None;
    let harness = Harness::start(behaviour).await;
    let key = format!("gap-list-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "cost gap for operations"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let (job_id, state, _) = harness.job(&key).await;
    assert_eq!(state, "succeeded", "成本缺口不是执行失败");
    let (amount, currency, source, cny) = harness.attempt_cost(job_id).await;
    assert_eq!(source.as_deref(), Some("unavailable"));
    assert_eq!(
        (amount, currency, cny),
        (None, None, None),
        "缺口不猜：三样都留空"
    );

    // 不进对账态、也不开对账案例：消费者的钱该扣的照扣。
    let cases: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operations.reconciliation_cases WHERE job_id = $1",
    )
    .bind(job_id)
    .fetch_one(&harness.pool)
    .await
    .expect("cases");
    assert_eq!(cases, 0);
    assert_eq!(harness.captured_microusd(job_id).await, -5_950);

    // 运营从缺口清单里看到它，带着去上游核账单要用的对账标识。
    let client = Client::new();
    let gaps: Value = client
        .get(format!("{}/api/v1/provider-cost-gaps", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("cost gap list")
        .json()
        .await
        .expect("cost gap list JSON");
    assert_eq!(gaps["count"], json!(1), "{gaps}");
    assert_eq!(gaps["truncated"], json!(false));
    assert_eq!(gaps["gaps"][0]["job_id"], json!(job_id.to_string()));
    assert_eq!(gaps["gaps"][0]["gateway_model"], json!(harness.model));
    assert!(
        gaps["gaps"][0]["provider_trace_id"].as_str().is_some(),
        "缺口清单必须带上游对账标识，否则核账单的人不知道该查哪个任务：{gaps}"
    );
    // 该接口只对管理员开放。
    let unauthorized = client
        .get(format!("{}/api/v1/provider-cost-gaps", harness.base_url))
        .send()
        .await
        .expect("unauthorized cost gap list");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    harness.cleanup().await;
}

/// **进对账那条路径也落成本事实**：执行已经发生、上游成本也拿得到，成本必须有去处。
///
/// 直接调仓库端口的 `fail_job`：结果交付失败在端到端里很难构造（假上游总会给图），而这条路径
/// 的写入本来就是库层的事。同时验"没有成本事实时四列留空"——那是"这次没有成本事实可落"，
/// 与"成本是 0"不是一回事。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_reconciliation_path_records_the_cost_fact_it_already_has() {
    let (database_url, database_name) = isolated_database_url().await;
    let repository = PgHubRepository::connect(&database_url, 2)
        .await
        .expect("repository");
    repository.migrate().await.expect("migrations");
    let pool = repository.pool().clone();

    let account = Uuid::new_v4();
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
            "model": {"const": "cost-path"},
            "prompt": {"type": "string"}
        }
    });
    sqlx::query("INSERT INTO ledger.accounts (id, balance_microusd) VALUES ($1, 100000)")
        .bind(account)
        .execute(&pool)
        .await
        .expect("account fixture");
    sqlx::query(
        "INSERT INTO catalog.vendor_models
             (id, vendor_id, native_model_id, native_revision, capability_schema)
         VALUES ($1,'OpenAI','cost-path','rev-1',$2)",
    )
    .bind(vendor_model)
    .bind(&contract)
    .execute(&pool)
    .await
    .expect("contract fixture");
    sqlx::query(
        "INSERT INTO supply.channels (id, provider_kind, base_url, credential_env)
         VALUES ($1,'AIHubMix','https://api.inferera.com','AIHUBMIX_API_KEY')",
    )
    .bind(channel)
    .execute(&pool)
    .await
    .expect("channel fixture");
    sqlx::query(
        "INSERT INTO supply.offerings
             (id, vendor_model_id, channel_id, adapter_key, provider_model_id, restrictions,
              carrier_schema, parameter_mapping)
         VALUES ($1,$2,$3,'aihubmix-image-v1','cost-path','{}'::jsonb,$4,'{}'::jsonb)",
    )
    .bind(offering)
    .bind(vendor_model)
    .bind(channel)
    .bind(&contract)
    .execute(&pool)
    .await
    .expect("offering fixture");
    sqlx::query(
        "INSERT INTO pricing.price_plans
             (id, offering_id, currency, text_input_microusd_per_million, image_input_microusd_per_million,
              text_output_microusd_per_million, image_output_microusd_per_million, source_url, approved_by)
         VALUES ($1,$2,'USD',5,8,10,30,'https://example.invalid/price','cost-path-test')",
    )
    .bind(price_plan)
    .bind(offering)
    .execute(&pool)
    .await
    .expect("price plan fixture");
    sqlx::query(
        "INSERT INTO publication.runtime_revisions
             (id, snapshot, published_by, gateway_model, vendor_model_id)
         VALUES ($1,'{}'::jsonb,'cost-path-test','cost-path',$2)",
    )
    .bind(revision)
    .bind(vendor_model)
    .execute(&pool)
    .await
    .expect("runtime revision fixture");
    sqlx::query(
        "INSERT INTO publication.runtime_entries
             (runtime_revision_id, vendor_model_id, offering_id, price_plan_id, gateway_model, active)
         VALUES ($1,$2,$3,$4,'cost-path',true)",
    )
    .bind(revision)
    .bind(vendor_model)
    .bind(offering)
    .bind(price_plan)
    .execute(&pool)
    .await
    .expect("runtime entry fixture");

    // 两条停在"正在调上游"、持有租约的 Job：一条带成本事实进对账，一条不带。
    let mut jobs = Vec::new();
    for key in ["with-cost", "without-cost"] {
        let job_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO generation.jobs
                 (id, account_id, idempotency_key, request_hash, state, branch, gateway_model,
                  native_parameters, carrier_schema, parameter_mapping, adapter_key, provider_model_id,
                  base_url, credential_env,
                  runtime_revision_id, vendor_model_id, offering_id, channel_id, price_snapshot,
                  max_cost_microusd, lease_owner, lease_expires_at)
             VALUES ($1,$2,$3,'hash','submitting','prompt_only','cost-path',
                     '{}'::jsonb,'{}'::jsonb,'{}'::jsonb,'aihubmix-image-v1','cost-path',
                     'https://api.inferera.com','AIHUBMIX_API_KEY',
                     $4,$5,$6,$7,'{}'::jsonb,20000,'worker-x', now() + interval '1 hour')",
        )
        .bind(job_id)
        .bind(account)
        .bind(key)
        .bind(revision)
        .bind(vendor_model)
        .bind(offering)
        .bind(channel)
        .execute(&pool)
        .await
        .expect("job fixture");
        let attempt_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO generation.attempts (id, job_id, state, request_digest)
             VALUES ($1,$2,'submitting','digest')",
        )
        .bind(attempt_id)
        .bind(job_id)
        .execute(&pool)
        .await
        .expect("attempt fixture");
        jobs.push((JobId(job_id), AttemptId(attempt_id)));
    }

    let failure = |provider_cost| AttemptFailure {
        provider_code: "result_delivery_failed".to_owned(),
        public_code: PublicErrorCode::OutcomeUnknown,
        message: "provider returned no image".to_owned(),
        trace_id: Some("task-1".to_owned()),
        kind: ProviderFailureKind::PlatformInternal,
        target_state: seeai_domain::JobState::ReconciliationRequired,
        hold_disposition: HoldDisposition::RetainForReconciliation,
        provider_cost,
    };
    let (with_cost, with_cost_attempt) = jobs[0];
    repository
        .fail_job(
            with_cost,
            "worker-x",
            Some(with_cost_attempt),
            failure(Some(ProviderCostFact {
                source: ProviderCostSource::Computed,
                amount_microusd: Some(5_950),
                currency: Some("USD".to_owned()),
                cny_microusd: Some(42_245),
            })),
        )
        .await
        .expect("the reconciliation path must record the cost it already has");

    let row = sqlx::query(
        "SELECT provider_cost_microusd, provider_cost_currency, provider_cost_source,
                provider_cost_cny_microusd, provider_trace_id
         FROM generation.attempts WHERE id = $1",
    )
    .bind(with_cost_attempt.0)
    .fetch_one(&pool)
    .await
    .expect("attempt after failure");
    assert_eq!(
        row.get::<Option<i64>, _>("provider_cost_microusd"),
        Some(5_950)
    );
    assert_eq!(
        row.get::<Option<String>, _>("provider_cost_currency")
            .as_deref(),
        Some("USD")
    );
    assert_eq!(
        row.get::<Option<String>, _>("provider_cost_source")
            .as_deref(),
        Some("computed")
    );
    assert_eq!(
        row.get::<Option<i64>, _>("provider_cost_cny_microusd"),
        Some(42_245)
    );
    assert_eq!(
        row.get::<Option<String>, _>("provider_trace_id").as_deref(),
        Some("task-1")
    );
    let cases: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operations.reconciliation_cases WHERE job_id = $1",
    )
    .bind(with_cost.0)
    .fetch_one(&pool)
    .await
    .expect("cases");
    assert_eq!(cases, 1, "结果交付失败仍然进对账（与成本缺口不同）");

    // 没有成本事实（连用量都算不出）：四列留空，不写成 0。
    let (without_cost, without_cost_attempt) = jobs[1];
    repository
        .fail_job(
            without_cost,
            "worker-x",
            Some(without_cost_attempt),
            failure(None),
        )
        .await
        .expect("a failure without a cost fact is still recorded");
    let row = sqlx::query(
        "SELECT provider_cost_microusd, provider_cost_currency, provider_cost_source,
                provider_cost_cny_microusd
         FROM generation.attempts WHERE id = $1",
    )
    .bind(without_cost_attempt.0)
    .fetch_one(&pool)
    .await
    .expect("attempt after failure");
    assert!(
        row.get::<Option<i64>, _>("provider_cost_microusd")
            .is_none()
    );
    assert!(
        row.get::<Option<String>, _>("provider_cost_currency")
            .is_none()
    );
    assert!(
        row.get::<Option<String>, _>("provider_cost_source")
            .is_none()
    );
    assert!(
        row.get::<Option<i64>, _>("provider_cost_cny_microusd")
            .is_none()
    );

    pool.close().await;
    drop_isolated_database(&database_name).await;
}
