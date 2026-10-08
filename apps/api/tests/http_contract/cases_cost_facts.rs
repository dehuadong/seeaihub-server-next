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

    let (job_id, state) = harness.job(&key).await;
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
    assert_eq!(harness.captured_microusd(job_id).await, -6_000);
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

    let (job_id, state) = harness.job(&key).await;
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

    let (job_id, state) = harness.job(&key).await;
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
    let (job_id, state) = harness.job(&key).await;
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
    // 对客实收 = **上游声明的金额 × 倍率 × 折算率**（11354 × 1.2 × 7.1 = 96736.08 ⇒ 96737，
    // 落账再取整到整积分 97000），与成本折算（11354 × 7.1 = 80613.4 ⇒ 80614）是两个量：后者只进毛利口径。
    assert_eq!(harness.captured_microusd(job_id).await, -97_000);
    assert_eq!(97_000 - 80_614, 16_386, "毛利 = 售价 − 成本折算后 CNY");
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

    let (job_id, _) = harness.job(&key).await;
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
    let (job_id, state) = harness.job(&key).await;
    assert_eq!(state, "succeeded");
    let (amount, currency, source, cny) = harness.attempt_cost(job_id).await;
    assert_eq!(amount, Some(11_354), "上游给了金额就直接取它，不自己算");
    assert_eq!(currency.as_deref(), Some("USD"));
    assert_eq!(source.as_deref(), Some("declared"));
    assert_eq!(cny, Some(80_614), "11354 微美元 × 7.1 = 80613.4 ⇒ 向上取整");
    assert_eq!(
        harness.captured_microusd(job_id).await,
        -44_000,
        "实际成本不改对客金额：对客金额只由冻结的对客费率向量决定，落账取整到整积分"
    );
    // 毛利 = 售价（CNY）− 成本折算后 CNY：这一笔是负的（参考成本只是发布时的定价参考，
    // 上游实际声明的金额比它高），照样能逐笔算出——不猜、不掩盖。
    assert_eq!(44_000 - 80_614, -36_614);

    harness.cleanup().await;
}

/// **同币种也要能录折算率**：`CNY → CNY = 1` 录得进去，声明 `cost_currency: "CNY"` 的供给因此
/// 能发布、受理、结算；率 1 那一行把**成本**折成 CNY 时逐位不变——代码里没有"这个币种不用
/// 折算"的分支。
///
/// 渠道币种不是"全平台统一美元"：以人民币声明成本的渠道（例如方舟）就是这一条。成本记上游声明
/// 的原值，同币种折出来逐位不变；对客收多少不影响这条事实。
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
    let (job_id, state) = harness.job(&key).await;
    assert_eq!(state, "succeeded");

    let snapshot = frozen_snapshot(&harness.pool, &key).await;
    assert_eq!(snapshot["cost_currency"], json!("CNY"));
    assert_eq!(snapshot["fx_rate"]["currency"], json!("CNY"));
    assert_eq!(
        snapshot["fx_rate"]["rate_micros"],
        json!(1_000_000),
        "同币种的折算率就是 1：{snapshot}"
    );
    // 同币种不产生折算：证明在**成本**侧——记的是上游声明的原值，率 1 折出来逐位不变。
    let (amount, currency, source, cny) = harness.attempt_cost(job_id).await;
    assert_eq!(amount, Some(11_354), "成本记上游声明的金额，币种取供给声明");
    assert_eq!(currency.as_deref(), Some("CNY"));
    // 这条渠道的执行面总会声明金额，所以成本来源是 `declared`（成本形态只作定价参考）。
    assert_eq!(source.as_deref(), Some("declared"));
    assert_eq!(cny, Some(11_354), "率 1 折出来逐位不变");

    // 产出张数落在 `image_count`，用量读的就是它：按张计价的这次执行记下的是**实际**张数
    // （假上游一张），不是请求的 `n`，也不是 0。
    let usage = client
        .get(format!(
            "{}/api/v1/accounts/{}/usage",
            harness.base_url, harness.account_id
        ))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("usage read");
    assert_eq!(usage.status(), StatusCode::OK);
    let usage: Value = usage.json().await.expect("usage body");
    let row = &usage["usage"][0];
    assert_eq!(row["job_id"], json!(job_id.to_string()), "{usage}");
    assert_eq!(row["type"], json!("image"), "{usage}");
    assert_eq!(
        row["usage"]["images"],
        json!(1),
        "per_image 请求的用量张数必须等于实际张数：{usage}"
    );

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
    let (job_id, state) = harness.job(&key).await;
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
    assert_eq!(harness.captured_microusd(job_id).await, -6_000);

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
    assert_eq!(unauthorized.status(), StatusCode::FORBIDDEN);

    harness.cleanup().await;
}

/// **平台自担的成本在管理员面的流水里查得到**：科目是它自己的 `cost`、金额为负，挂在平台账户上；
/// 消费者的流水里没有它。
///
/// 上游终态给了金额却没有结果图就是这条路径的典型形态：钱已经花了，消费者那一侧的预授权还留着
/// （等对账处置），平台先把这笔钱认在自己账上。这条链路同时钉住"科目取值能被读回来"——漏认一个
/// 科目，管理员查流水看到的是 500（见 `LedgerEntryKind::parse`）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_platform_cost_is_queryable_under_its_own_kind_on_the_platform_account() {
    let mut behaviour = UpstreamBehaviour::apimart();
    behaviour.terminal_without_images = true;
    let harness = Harness::start(behaviour).await;
    let key = format!("platform-cost-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "who pays for this"),
        )
        .await;
    assert_ne!(
        status,
        StatusCode::OK,
        "没有结果图的那次执行是失败的：{body}"
    );
    let (job_id, state) = harness.job(&key).await;
    assert_eq!(state, "reconciliation_required", "金额到手、结果没交付");

    let client = Client::new();
    let platform = platform_account(&harness.pool).await;
    let listed: Value = client
        .get(format!(
            "{}/api/v1/accounts/{platform}/entries",
            harness.base_url
        ))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("platform entries")
        .json()
        .await
        .expect("platform entries JSON");
    let entries = listed["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 1, "平台账户上只有这一笔自担成本：{listed}");
    assert_eq!(
        entries[0]["kind"].as_str(),
        Some("cost"),
        "成本是它自己的科目，不混进 `adjustment`：{listed}"
    );
    assert_eq!(
        entries[0]["amount_microusd"].as_i64(),
        Some(-80_614),
        "金额为负，就是上游实扣的那笔折算额：{listed}"
    );
    assert_eq!(
        entries[0]["job_id"].as_str(),
        Some(job_id.to_string().as_str()),
        "要指得出是哪次执行花的钱：{listed}"
    );

    // 消费者那一侧只认自己的预授权：成本不进它的流水，也就不进它的余额。
    let consumer: Value = client
        .get(format!(
            "{}/api/v1/accounts/{}/entries",
            harness.base_url, harness.account_id
        ))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("consumer entries")
        .json()
        .await
        .expect("consumer entries JSON");
    let consumer_entries = consumer["entries"].as_array().expect("entries array");
    assert!(
        consumer_entries
            .iter()
            .all(|entry| entry["kind"].as_str() != Some("cost")),
        "平台自担的成本不许写进消费者的流水：{consumer}"
    );

    harness.cleanup().await;
}

/// 平台账户那一行的 id：按 `kind` 找，不写死任何常量——账户是数据，迁移种下它。
async fn platform_account(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM ledger.accounts WHERE kind = 'platform'")
        .fetch_one(pool)
        .await
        .expect("the migrations seed exactly one platform account")
}

/// 单次请求的上游成本远超旧的 10 元上限也不拒：受理的金额判定只有余额那一道。
///
/// 按张计价的候选单价 1 美元/张（按 7.1 折算约 7.1 元）：发布期按合同允许的最大张数算约 71 元、
/// 这一次的 2 张约 14.2 元——两处都在旧上限（10 元）之上。账户余额够就必须受理：发布与受理都不该
/// 有以成本为理由的判定。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_request_whose_upstream_cost_is_far_above_the_old_ceiling_is_accepted() {
    // 用 `start_with_draft`：它那条发布占的不是 `route-test-1`，下面这次重新发布才落得成新修订。
    let harness = Harness::start_with_draft(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    // 合同里声明 `n`（上限 10 张）：发布期按 1 美元 × 10 张 ≈ 71 元判，这一次要 2 张 ≈ 14.2 元。
    // 不声明它，`n` 会在受理前被按合同过滤掉，这条用例就只是在测一张图。
    let contract = surface_schema(json!({
        "model": {"const": Harness::MODEL},
        "prompt": {"type": "string", "minLength": 1},
        "n": {"type": "integer", "minimum": 1, "maximum": 10, "default": 1}
    }));
    let mut draft = unit_candidate("per_image", 1_000_000, "USD");
    draft["carrier_schema"] = contract.clone();
    // 假上游的地址：不写回夹具里那个占位地址，请求会去连一个不存在的端口。
    draft["base_url"] = Value::String(harness.upstream_base_url.clone());
    assert_eq!(
        publish_on_revision(
            &harness,
            Harness::MODEL,
            "route-test-2",
            contract,
            vec![draft],
            Some(2_000),
        )
        .await,
        StatusCode::OK,
        "最坏单次成本远高于 10 元的候选必须发得出去"
    );

    let key = format!("no-cost-ceiling-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "cost far above the old ceiling");
    request["n"] = json!(2);
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request)
        .await;
    assert_eq!(status, StatusCode::OK, "余额够就必须受理：{body}");
    assert_sync_success("成本远超旧上限的一次请求", &body);
    // 这一次必须真的带 2 张，否则"成本判据"这件事根本没被走到：保底额按 2K 档 0.25 元 × 2 张。
    let (job_id, state) = harness.job(&key).await;
    assert_eq!(state, "succeeded");
    let held: i64 =
        sqlx::query_scalar("SELECT max_cost_microusd FROM generation.jobs WHERE id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("the frozen hold");
    assert_eq!(held, 500_000, "这一次要 2 张：2K 档 0.25 元 × 2");

    // 同一天的第二笔：当日已完成实收已经不是任何闸门的判据，它照样受理。
    let second = format!("no-cost-ceiling-second-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &second,
            route_request(harness.model, "second request on the same day"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "当天已经花掉多少不拦下一笔：{body}");
    assert_sync_success("同一天的第二笔", &body);

    harness.cleanup().await;
}
