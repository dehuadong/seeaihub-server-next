use super::*;

fn usage() -> TokenUsage {
    TokenUsage {
        input_tokens: 1051,
        input_text_tokens: 27,
        input_image_tokens: 1024,
        output_tokens: 196,
        output_text_tokens: 0,
        output_image_tokens: 196,
        total_tokens: 1247,
    }
}

/// 一次成功执行的**执行事实**：按 token 计量量的候选读用量，按张的读产出张数，上游给金额的读金额。
///
/// 三样一起给，是因为"这条供给按什么计价"只有快照自己知道——调用方不该先替它判一次形态。
fn facts<'a>(usage: &'a TokenUsage) -> ChargeFacts<'a> {
    ChargeFacts {
        usage: Some(usage),
        images: 1,
        declared_cost_microusd: None,
    }
}

/// 对客金额落账前向上取整到整积分：1积分 = 1000 微单位，不足 1 积分的部分向上进一。
#[test]
fn consumer_amounts_round_up_to_whole_points() {
    assert_eq!(whole_points_microusd(0), 0);
    assert_eq!(whole_points_microusd(1), 1_000);
    assert_eq!(whole_points_microusd(999), 1_000);
    assert_eq!(whole_points_microusd(1_000), 1_000);
    assert_eq!(whole_points_microusd(1_001), 2_000);
    assert_eq!(whole_points_microusd(14_207), 15_000);
    assert_eq!(whole_points_microusd(96_737), 97_000);
}

#[test]
fn calculates_edit_charge_from_verified_usage() {
    let snapshot = snapshot_with_rates("USD", 5_000_000, 8_000_000, 10_000_000, 30_000_000);
    assert_eq!(snapshot.charge_microusd(facts(&usage())), Ok(15_000));
}

/// 成本自算与对客扣费是**两个口径**：算式同一个，读的费率各自一份。
///
/// 今天 Price Plan 暂时兼作渠道成本费率，两份费率恰好是同一张表，所以两条路算出来的数
/// 相同；用例把两份费率**人为设成不同的值**，钉住"成本读成本费率、扣费读对客费率"——
/// 对客价是运营定的那份，成本不能跟着售价漂移。
#[test]
fn computed_cost_reads_the_channel_cost_rates_not_the_consumer_charge() {
    let cost_snapshot = snapshot_with_rates("USD", 5_000_000, 8_000_000, 10_000_000, 30_000_000);
    let consumer_snapshot =
        snapshot_with_rates("CNY", 7_000_000, 9_000_000, 11_000_000, 40_000_000);

    let cost = cost_snapshot
        .cost_rates()
        .expect("这条快照带 Price Plan")
        .amount_microusd(&usage());
    let charge = consumer_snapshot.charge_microusd(facts(&usage()));
    // 成本：27 文本输入 × 5 + 1024 图像输入 × 8 + 196 图像输出 × 30（每 1M）。
    assert_eq!(cost, Ok(14_207));
    // 对客：同一份用量，换成对客那份费率，金额就不一样了。
    assert_eq!(charge, Ok(18_000));
    assert_ne!(
        cost, charge,
        "两份费率不同时，成本与对客扣费必须各自算各自的，不能互相顶替"
    );
    assert_eq!(
        cost_snapshot.cost_currency(),
        Some("USD"),
        "成本币种取渠道声明的那一份，不假定 USD、也不跟着对客 CNY 走"
    );
}

/// 造一份价格快照：四档费率按参数给，其余字段取与用例无关的定值。
///
/// `pricing` 留空 = 旧口径（已发布费率兼作对客费率、预授权回落平台兜底数）。
fn snapshot_with_rates(
    currency: &str,
    text_input: u64,
    image_input: u64,
    text_output: u64,
    image_output: u64,
) -> PriceSnapshot {
    PriceSnapshot {
        price_plan_id: Some(PricePlanId::new()),
        rates: Some(PriceRates {
            currency: currency.to_owned(),
            text_input_microusd_per_million: text_input,
            image_input_microusd_per_million: image_input,
            text_output_microusd_per_million: text_output,
            image_output_microusd_per_million: image_output,
        }),
        formula: PricingFormula::TokenRates,
        cost_unit_price_microusd: None,
        consumer_formula: None,
        captured_at: Utc::now(),
        hit_candidate: None,
        consumer_rates_cny: None,
        tier_prices: None,
        floor_amounts: None,
        hold_microusd: None,
        hold_source: None,
        cost_basis: None,
        reference_cost_microusd: None,
        cost_currency: None,
        markup_bps: None,
        fx_rate: None,
    }
}

#[test]
fn rejects_inconsistent_usage() {
    let mut invalid = usage();
    invalid.total_tokens = 1;
    assert_eq!(invalid.validate(), Err(DomainError::InconsistentUsage));
}

/// 成本来源的落库字符串是**库层 CHECK 的取值集合**，读写必须自洽：
/// 落下去的值读不回来，等于把事实写成了一次性写入。
#[test]
fn provider_cost_sources_round_trip_through_their_stored_form() {
    for source in [
        ProviderCostSource::Computed,
        ProviderCostSource::Declared,
        ProviderCostSource::Unavailable,
    ] {
        assert_eq!(ProviderCostSource::parse(source.as_str()), Some(source));
    }
    assert_eq!(ProviderCostSource::parse("guessed"), None);
}

#[test]
fn rejects_unsafe_state_jump() {
    assert!(matches!(
        JobState::Accepted.transition(JobState::Succeeded),
        Err(DomainError::InvalidStateTransition { .. })
    ));
}

#[test]
fn reconciliation_cannot_be_promoted_to_success_without_evidence() {
    assert!(matches!(
        JobState::ReconciliationRequired.transition(JobState::Succeeded),
        Err(DomainError::InvalidStateTransition { .. })
    ));
    assert_eq!(
        JobState::ReconciliationRequired.transition(JobState::Failed),
        Ok(JobState::Failed)
    );
}

#[test]
fn allows_lease_failure_before_provider_submission() {
    assert_eq!(
        JobState::Leased.transition(JobState::Failed),
        Ok(JobState::Failed)
    );
}

/// 一份发布在**某条供给**下的保底表：只按 `size` 填（OpenAI 系当前形态）。
fn openai_floor_table() -> FloorTable {
    FloorTable::from_json(&serde_json::json!({
        "amounts": {"1K": 160_000, "2K": 250_000, "4K": 300_000},
        "cap_microusd": 300_000,
    }))
    .expect("the published floor table parses")
}

/// 归位一次并查表：把"请求的 `size` → 档位 → 保底额"这条链走完。
fn hold_for(
    table: &FloorTable,
    size: Option<&str>,
    quality: Option<&str>,
) -> Option<(u64, HoldSource)> {
    let tier = resolve_size_tier(size, &SizeProfile::default());
    table.lookup(tier.as_ref(), quality)
}

#[test]
fn floor_lookup_takes_the_tier_the_request_asked_for() {
    let table = openai_floor_table();
    assert_eq!(
        hold_for(&table, Some("2K"), None),
        Some((250_000, HoldSource::Tier))
    );
    // `quality` 维留空即按 `size` 档：带任意质量都查到同一个 `size` 档保底额。
    for quality in ["low", "medium", "high", "xhigh", "max", "auto"] {
        assert_eq!(
            hold_for(&table, Some("2K"), Some(quality)),
            Some((250_000, HoldSource::Tier)),
            "只填了 size 维时，quality 不该改变查到的档位"
        );
    }
    // 档位写法的大小写不影响查表：调用方给 `2k` 与管理员的 `2K` 是同一个档。
    assert_eq!(
        hold_for(&table, Some("2k"), None),
        Some((250_000, HoldSource::Tier))
    );
}

/// **像素型 `size` 先归到档位再查表**：用户口径是"按分辨率保底、通过 `size` 判断 1K/2K/4K"。
///
/// 这条供给没发布尺寸档案（OpenAI 系当前的形态），所以走**最长边**阈值兜底。
#[test]
fn pixel_sizes_resolve_to_a_tier_before_the_lookup() {
    let table = openai_floor_table();
    assert_eq!(
        hold_for(&table, Some("1024x1024"), None),
        Some((160_000, HoldSource::Tier)),
        "最长边 1024 ⇒ 1K 档"
    );
    assert_eq!(
        hold_for(&table, Some("2048x2048"), None),
        Some((250_000, HoldSource::Tier)),
        "最长边 2048 ⇒ 2K 档"
    );
    assert_eq!(
        hold_for(&table, Some("3840x2160"), None),
        Some((300_000, HoldSource::Tier)),
        "最长边 3840 ⇒ 4K 档"
    );
    // 阈值看的是**最长边**：短边小不改变档位。
    assert_eq!(
        hold_for(&table, Some("1024x2048"), None),
        Some((250_000, HoldSource::Tier))
    );
    // 质量维照旧只在同一档位内取格。
    assert_eq!(
        hold_for(&table, Some("1024x1024"), Some("high")),
        Some((160_000, HoldSource::Tier))
    );
}

/// **该供给发布的档位像素表优先**：同一个像素在别的供给上属于别的档位，只有它自己的表算数。
#[test]
fn the_supply_size_profile_wins_over_the_longest_edge_fallback() {
    let table = openai_floor_table();
    // 这条供给把 1024x1024 归在 2K 档（各供给的档位像素不同）。
    let profile = SizeProfile::from_json(&serde_json::json!({
        "2K": {"1:1": "1024x1024", "16:9": "2048x1152"},
    }))
    .expect("the published size profile parses");
    let tier = resolve_size_tier(Some("1024x1024"), &profile).expect("the profile knows this cell");
    assert_eq!(tier.tier, "2K");
    assert_eq!(
        table.lookup(Some(&tier), None),
        Some((250_000, HoldSource::Tier)),
        "按该供给自己的档位像素表归位，最长边兜底不参与"
    );
    // 表里没有这一格 ⇒ 回到最长边兜底（最长边 1024 ⇒ 1K）。
    let tier = resolve_size_tier(Some("1024x768"), &profile).expect("pixels always resolve");
    assert_eq!(tier.tier, "1K");
    assert_eq!(
        table.lookup(Some(&tier), None),
        Some((160_000, HoldSource::Tier))
    );
}

#[test]
fn auto_and_a_missing_size_take_the_default_tier() {
    let table = openai_floor_table();
    // `size = auto` ⇒ 默认档 2K（中间档：既不是最小、也不是最大）。
    assert_eq!(
        hold_for(&table, Some("auto"), None),
        Some((250_000, HoldSource::AutoTier))
    );
    // 没给 size 与 `auto` 同义：都是"调用方没钉尺寸、由模型自选"。
    assert_eq!(
        hold_for(&table, None, None),
        Some((250_000, HoldSource::AutoTier))
    );
    // `auto` 也照旧受质量维影响。
    assert_eq!(
        hold_for(&table, Some("auto"), Some("high")),
        Some((250_000, HoldSource::AutoTier))
    );
}

/// **空串不是"没给"**：`size` 的字面量就是调用方说的那个尺寸，平台不替它 trim、也不把
/// "给了个空"当成"没说话"再猜一个默认档——归不出就回落该供给的封顶保底值。
#[test]
fn an_empty_or_padded_size_is_given_and_is_not_the_missing_size() {
    let table = openai_floor_table();
    assert_eq!(
        resolve_size_tier(Some(""), &SizeProfile::default()),
        None,
        "空串归不出档位，不落到 `auto` 的默认档上"
    );
    assert_eq!(
        hold_for(&table, Some(""), None),
        Some((300_000, HoldSource::SupplyCap)),
        "空串与'没给这个字段'必须落到不同的保底额上"
    );
    assert_eq!(
        hold_for(&table, None, None),
        Some((250_000, HoldSource::AutoTier)),
        "只有字段缺失才走 `auto` 的默认档 2K"
    );
    // 纯空白同理：它也是一个字面量，不是"没给"。
    assert_eq!(
        hold_for(&table, Some("  "), None),
        Some((300_000, HoldSource::SupplyCap))
    );
    // `auto` 只有这一个写法：别名与带空格的写法都照字面读，认不出即回落。
    for value in ["AUTO", "Auto", " auto", "auto "] {
        assert_eq!(
            hold_for(&table, Some(value), None),
            Some((300_000, HoldSource::SupplyCap)),
            "`{value}` 不是那个字面量 `auto`"
        );
    }
}

#[test]
fn floor_lookup_falls_back_to_the_supply_cap_when_the_tier_cannot_be_resolved() {
    let table = openai_floor_table();
    // 比例型只说了形状、没说分辨率：归不出档位 ⇒ 回落该供给的封顶保底值。
    assert_eq!(
        hold_for(&table, Some("16:9"), None),
        Some((300_000, HoldSource::SupplyCap))
    );
    // 认不出的取值同理。
    assert_eq!(
        hold_for(&table, Some("huge"), None),
        Some((300_000, HoldSource::SupplyCap))
    );
    // 归得出档位、但表里没有这一档 ⇒ 也是封顶值。
    let partial = FloorTable::from_json(&serde_json::json!({
        "amounts": {"1K": 160_000},
        "cap_microusd": 300_000,
    }))
    .expect("the published floor table parses");
    assert_eq!(
        hold_for(&partial, Some("4K"), None),
        Some((300_000, HoldSource::SupplyCap))
    );
}

#[test]
fn floor_lookup_falls_back_to_the_platform_default_when_the_supply_declares_nothing() {
    let empty = FloorTable::from_json(&serde_json::json!({})).expect("an empty table is legal");
    assert!(empty.is_empty());
    // 连封顶保底值都没有 ⇒ 查不到，由调用方回落到平台兜底数。
    assert_eq!(hold_for(&empty, Some("2K"), None), None);
    assert_eq!(hold_for(&empty, Some("auto"), None), None);
    assert_eq!(hold_for(&empty, Some("1024x1024"), None), None);
}

#[test]
fn floor_lookup_prefers_the_quality_specific_entry_then_the_size_entry() {
    let table = FloorTable::from_json(&serde_json::json!({
        "amounts": {"2K": 250_000, "2K/high": 900_000, "4K": 300_000},
    }))
    .expect("the published floor table parses");
    assert_eq!(
        hold_for(&table, Some("2K"), Some("high")),
        Some((900_000, HoldSource::Tier))
    );
    // 没为这个质量单列 ⇒ 取该档位"任意质量"的那一格，不拿别的档位顶上。
    assert_eq!(
        hold_for(&table, Some("2K"), Some("low")),
        Some((250_000, HoldSource::Tier))
    );
    // 该档位连"任意质量"都没有（只有 `2K/high`）⇒ 算没查到这一档。
    let quality_only = FloorTable::from_json(&serde_json::json!({
        "amounts": {"2K/high": 900_000},
        "cap_microusd": 300_000,
    }))
    .expect("the published floor table parses");
    assert_eq!(
        hold_for(&quality_only, Some("2K"), Some("low")),
        Some((300_000, HoldSource::SupplyCap))
    );
    // `auto` 取默认档 `2K`：它只有质量单列 ⇒ 质量对得上就用它。
    assert_eq!(
        hold_for(&quality_only, Some("auto"), Some("high")),
        Some((900_000, HoldSource::AutoTier))
    );
    // 默认档没有这个质量的那一格 ⇒ 回落封顶值，不拿别的档位顶上。
    assert_eq!(
        hold_for(&quality_only, Some("auto"), Some("low")),
        Some((300_000, HoldSource::SupplyCap))
    );
}

#[test]
fn a_floor_table_that_declares_one_tier_twice_is_rejected() {
    // 规范化之后撞到同一个档位：同一档两个保底额，查出来的数就不确定了。
    let error = FloorTable::from_json(&serde_json::json!({
        "amounts": {"2K": 250_000, "2k": 260_000},
    }))
    .expect_err("the same tier twice is ambiguous");
    assert!(error.contains("2K"), "{error}");
    assert!(
        FloorTable::from_json(&serde_json::json!({"amounts": {"2K/": 1}})).is_err(),
        "半截的 size/quality 键不是档位"
    );
}

#[test]
fn fx_conversion_is_fixed_point_and_rounds_up() {
    let rate = FxRate {
        currency: "USD".to_owned(),
        // 1 美元 = 7.1 元人民币。
        rate_micros: 7_100_000,
        effective_at: Utc::now(),
    };
    // 11354 微美元 × 7.1 = 80613.4 微元 ⇒ 向上取整 80614。
    assert_eq!(rate.to_cny_microusd(11_354), Ok(80_614));
    assert_eq!(rate.to_cny_microusd(0), Ok(0));
    // 折算率是定点整数：1:1 的币种（例如人民币自己）折出来逐位不变。
    let identity = FxRate {
        currency: "CNY".to_owned(),
        rate_micros: FX_RATE_DENOMINATOR,
        effective_at: Utc::now(),
    };
    assert_eq!(identity.to_cny_microusd(5950), Ok(5950));
}

/// 按 token 计量量的候选，实收读**对客费率向量**，不是已发布费率；没有向量时才走旧口径。
///
/// 两份费率**人为设成不同的值**：拿错一份就会算出另一个数，用例因此钉得住"实收按哪份费率"。
#[test]
fn the_charge_reads_the_consumer_vector_when_the_snapshot_carries_pricing() {
    let mut snapshot = snapshot_with_rates("USD", 5_000_000, 8_000_000, 10_000_000, 30_000_000);
    assert_eq!(
        snapshot.charge_microusd(facts(&usage())),
        Ok(15_000),
        "旧口径"
    );
    assert_eq!(snapshot.hold_microusd, None);
    snapshot.consumer_rates_cny = Some(ConsumerRatesCny {
        text_input_micros_per_million: 7_000_000,
        image_input_micros_per_million: 9_000_000,
        text_output_micros_per_million: 11_000_000,
        image_output_micros_per_million: 40_000_000,
    });
    snapshot.cost_basis = Some(CostBasis::Declared);
    snapshot.reference_cost_microusd = Some(11_354);
    snapshot.cost_currency = Some("USD".to_owned());
    snapshot.markup_bps = Some(2_000);
    snapshot.hold_microusd = Some(250_000);
    snapshot.hold_source = Some(HoldSource::Tier);
    snapshot.fx_rate = Some(FxRate {
        currency: "USD".to_owned(),
        rate_micros: 7_100_000,
        effective_at: Utc::now(),
    });
    assert_eq!(
        snapshot.charge_microusd(facts(&usage())),
        Ok(18_000),
        "对客费率向量"
    );
    // 成本侧不受影响：它仍读该渠道的成本费率。
    assert_eq!(
        snapshot
            .cost_rates()
            .expect("这条快照带 Price Plan")
            .amount_microusd(&usage()),
        Ok(14_207)
    );
    assert_eq!(snapshot.hold_microusd, Some(250_000));
    assert_eq!(
        snapshot.fx_rate.as_ref().map(|rate| rate.rate_micros),
        Some(7_100_000)
    );
}

/// 历史快照（没有 `consumer_rates_cny` / `hold_microusd` 这些键）必须照样读得回来。
///
/// 库里的 `price_snapshot` 是 jsonb：加字段这件事只有在**旧 JSON 仍能解析**时才不破坏
/// 已受理 Job 的结算——解析失败会让历史 Job 直接读不出来。
#[test]
fn a_snapshot_without_the_pricing_keys_still_parses() {
    let legacy = serde_json::json!({
        "price_plan_id": PricePlanId::new(),
        "rates": {
            "currency": "USD",
            "text_input_microusd_per_million": 5_000_000,
            "image_input_microusd_per_million": 8_000_000,
            "text_output_microusd_per_million": 10_000_000,
            "image_output_microusd_per_million": 30_000_000,
        },
        "captured_at": Utc::now(),
    });
    let snapshot: PriceSnapshot =
        serde_json::from_value(legacy).expect("a legacy snapshot must still parse");
    assert_eq!(snapshot.consumer_rates_cny, None);
    assert_eq!(snapshot.hold_microusd, None);
    assert_eq!(snapshot.fx_rate, None);
    assert_eq!(snapshot.hit_candidate, None);
    assert_eq!(
        snapshot.formula,
        PricingFormula::TokenRates,
        "历史快照缺这个键时按当时唯一存在的计价形态读"
    );
    assert_eq!(snapshot.charge_microusd(facts(&usage())), Ok(15_000));
}

/// 对客实收**算不出来时不按 0 收**：返回错误，由调用方按平台侧故障处置。
///
/// 按 token 计量量的候选没有对客费率向量、也没有 Price Plan 费率时是这样；上游直接给金额的候选
/// 这次没拿到金额时也是这样——那都是"没有对客计费基准"，不是"这笔钱是 0"。
#[test]
fn a_supply_without_a_consumer_basis_cannot_be_charged() {
    let mut snapshot = snapshot_with_rates("USD", 5_000_000, 8_000_000, 10_000_000, 30_000_000);
    snapshot.price_plan_id = None;
    snapshot.rates = None;
    snapshot.cost_currency = Some("USD".to_owned());
    snapshot.markup_bps = Some(2_000);
    snapshot.fx_rate = Some(FxRate {
        currency: "USD".to_owned(),
        rate_micros: 7_100_000,
        effective_at: Utc::now(),
    });
    assert_eq!(
        snapshot.charge_microusd(facts(&usage())),
        Err(DomainError::MissingConsumerRate)
    );
    assert_eq!(snapshot.cost_rates(), None);
    assert_eq!(
        snapshot.cost_currency(),
        Some("USD"),
        "声明还在：成本记账要用它，但对客金额与它无关"
    );

    // 上游直接给金额的候选：声明到了就按它算，没声明就是算不出来。
    snapshot.formula = PricingFormula::UpstreamDeclared;
    assert_eq!(
        snapshot.charge_microusd(facts(&usage())),
        Err(DomainError::MissingConsumerRate),
        "上游没声明金额 ⇒ 没有对客计费基准"
    );
    let executed = usage();
    let declared = ChargeFacts {
        declared_cost_microusd: Some(11_354),
        ..facts(&executed)
    };
    assert_eq!(snapshot.charge_microusd(declared), Ok(97_000));
}

/// 造一份**上游声明金额**计价的快照（成本与对客都是 `upstream_declared`）。
///
/// `cost_unit_price_microusd` 是成本侧的按张 / 按次单价占位（上游声明金额形态没有可算的单价，
/// 传 0）；倍率与折算率用于把上游声明的金额折成对客价。
fn unit_snapshot(
    formula: PricingFormula,
    cost_unit_price_microusd: u64,
    currency: &str,
    markup_bps: i32,
    rate_micros: u64,
) -> PriceSnapshot {
    let mut snapshot = snapshot_with_rates(currency, 0, 0, 0, 0);
    snapshot.price_plan_id = None;
    snapshot.rates = None;
    snapshot.formula = formula;
    snapshot.cost_unit_price_microusd = Some(cost_unit_price_microusd);
    snapshot.consumer_formula = Some(formula);
    snapshot.cost_currency = Some(currency.to_owned());
    snapshot.markup_bps = Some(markup_bps);
    snapshot.fx_rate = Some(FxRate {
        currency: currency.to_owned(),
        rate_micros,
        effective_at: Utc::now(),
    });
    snapshot
}

/// 上游直接给金额：对客价 = **这次声明的金额 × 倍率 × 折算率**，再向上取整到整积分。
///
/// 11354 微美元 × 1.2 × 7.1 = 96736.08 ⇒ 向上取整 96737，再取整到整积分 97000：
/// 一次除、一次取整，不是"先折人民币再乘倍率"那样取整两遍。
#[test]
fn an_upstream_declared_supply_sells_at_the_declared_amount_times_the_markup() {
    let snapshot = unit_snapshot(PricingFormula::UpstreamDeclared, 0, "USD", 2_000, 7_100_000);
    assert_eq!(
        snapshot.charge_microusd(ChargeFacts {
            declared_cost_microusd: Some(11_354),
            ..facts(&usage())
        }),
        Ok(97_000)
    );
}

/// **同币种不产生折算**：折算率表里同币种那一行率恒为 1（CNY → CNY = 1），所以上游金额形态的
/// 对客价就是声明金额 × 倍率，乘 1 不改数。代码里没有"这个币种不用折算"的分支。
#[test]
fn a_same_currency_supply_is_not_converted() {
    let snapshot = unit_snapshot(
        PricingFormula::UpstreamDeclared,
        0,
        "CNY",
        2_000,
        FX_RATE_DENOMINATOR,
    );
    assert_eq!(
        snapshot.charge_microusd(ChargeFacts {
            declared_cost_microusd: Some(300_000),
            ..facts(&usage())
        }),
        Ok(360_000),
        "300_000 微元 × 1.2 = 360_000，折算率 1 不改数"
    );
}

/// **倍率不是常量**：同一个声明金额、同一份执行事实，换个倍率对客实收就成比例地变。
///
/// 用 20_000 微单位这个金额是为了两个倍率都整除（24_000 与 30_000），于是"成比例"可以逐位
/// 断言：24_000 × 15 = 30_000 × 12 = 360_000。倍率是发布数据（每个网关模型一个），代码里没有
/// 它的默认值——这里换的是数据，不是常量。
#[test]
fn the_markup_coefficient_scales_the_charge_proportionally() {
    let twenty_percent = unit_snapshot(
        PricingFormula::UpstreamDeclared,
        0,
        "CNY",
        2_000,
        FX_RATE_DENOMINATOR,
    );
    let fifty_percent = unit_snapshot(
        PricingFormula::UpstreamDeclared,
        0,
        "CNY",
        5_000,
        FX_RATE_DENOMINATOR,
    );
    let cheap = twenty_percent
        .charge_microusd(ChargeFacts {
            declared_cost_microusd: Some(20_000),
            ..facts(&usage())
        })
        .expect("1.2 倍率算得出对客价");
    let dear = fifty_percent
        .charge_microusd(ChargeFacts {
            declared_cost_microusd: Some(20_000),
            ..facts(&usage())
        })
        .expect("1.5 倍率算得出对客价");
    assert_eq!(cheap, 24_000);
    assert_eq!(dear, 30_000);
    assert_eq!(
        cheap * 15,
        dear * 12,
        "实收之比必须等于倍率之比（1.2 : 1.5）"
    );
}

/// 上游金额形态算对客价要的三样（声明金额 / 倍率 / 折算率）缺一样就是算不出来：不拿别的数顶替。
#[test]
fn a_derived_consumer_price_needs_all_of_its_inputs() {
    let complete = unit_snapshot(PricingFormula::UpstreamDeclared, 0, "USD", 2_000, 7_100_000);
    assert_eq!(
        complete.charge_microusd(ChargeFacts {
            declared_cost_microusd: Some(11_354),
            ..facts(&usage())
        }),
        Ok(97_000)
    );

    let mut without_markup = complete.clone();
    without_markup.markup_bps = None;
    assert_eq!(
        without_markup.charge_microusd(ChargeFacts {
            declared_cost_microusd: Some(11_354),
            ..facts(&usage())
        }),
        Err(DomainError::MissingConsumerRate),
        "没有倍率就没有对客价"
    );

    let mut without_fx = complete.clone();
    without_fx.fx_rate = None;
    assert_eq!(
        without_fx.charge_microusd(ChargeFacts {
            declared_cost_microusd: Some(11_354),
            ..facts(&usage())
        }),
        Err(DomainError::MissingConsumerRate),
        "没有折算率就没有对客价"
    );

    assert_eq!(
        complete.charge_microusd(facts(&usage())),
        Err(DomainError::MissingConsumerRate),
        "上游没声明金额就没有对客价"
    );
}

/// 对客形态只有 token 四档 / 上游声明金额两种：快照里出现按张 / 按次（成本侧才有的取值）时，
/// 这条候选没有对客计费基准，按平台侧故障处理、不按 0 结算。
#[test]
fn a_per_image_consumer_form_has_no_charge_basis() {
    let mut snapshot = snapshot_with_rates("USD", 5_000_000, 8_000_000, 10_000_000, 30_000_000);
    snapshot.consumer_formula = Some(PricingFormula::PerImage);
    assert_eq!(
        snapshot.charge_microusd(facts(&usage())),
        Err(DomainError::MissingConsumerRate)
    );
}

/// 按张 / 按次的一笔金额逐位等于"数量 × 单价"，溢出按错误落。
#[test]
fn unit_prices_multiply_bit_exactly() {
    // 3 张 × 11_354 微单位 = 34_062。
    assert_eq!(unit_amount_microusd(3, 11_354), Ok(34_062));
    // 1 次 × 11354 = 11354（按次就是一次的钱）。
    assert_eq!(unit_amount_microusd(1, 11_354), Ok(11_354));
    assert_eq!(unit_amount_microusd(0, 11_354), Ok(0));
    assert_eq!(
        unit_amount_microusd(u64::MAX, 2),
        Err(DomainError::ArithmeticOverflow)
    );
}

#[test]
fn pricing_formulas_round_trip_through_their_stored_form() {
    for formula in [
        PricingFormula::TokenRates,
        PricingFormula::PerImage,
        PricingFormula::PerCall,
        PricingFormula::UpstreamDeclared,
    ] {
        assert_eq!(PricingFormula::parse(formula.as_str()), Some(formula));
    }
    assert_eq!(PricingFormula::parse("by_the_hour"), None);
    assert!(PricingFormula::PerImage.takes_unit_price());
    assert!(PricingFormula::PerCall.takes_unit_price());
    assert!(!PricingFormula::TokenRates.takes_unit_price());
    assert!(!PricingFormula::UpstreamDeclared.takes_unit_price());
}

#[test]
fn hold_sources_and_cost_bases_round_trip_through_their_stored_form() {
    for source in [
        HoldSource::Tier,
        HoldSource::AutoTier,
        HoldSource::SupplyCap,
        HoldSource::PlatformDefault,
    ] {
        assert_eq!(HoldSource::parse(source.as_str()), Some(source));
    }
    assert_eq!(HoldSource::parse("guessed"), None);
    for basis in [CostBasis::Computed, CostBasis::Declared] {
        assert_eq!(CostBasis::parse(basis.as_str()), Some(basis));
    }
    assert_eq!(CostBasis::parse("unavailable"), None);
}

/// 分录类别与存储取值一一对应：库里的 `CHECK` 认的那六个（含入账与成本）都必须读得回来。
///
/// 漏掉一个的代价是把一整个账户的流水读成错误——管理员查账时看到的是 500，而不是"少了一条"。
#[test]
fn ledger_entry_kinds_round_trip_through_their_stored_form() {
    for kind in [
        LedgerEntryKind::Credit,
        LedgerEntryKind::Hold,
        LedgerEntryKind::Capture,
        LedgerEntryKind::Release,
        LedgerEntryKind::Adjustment,
        LedgerEntryKind::Cost,
    ] {
        assert_eq!(LedgerEntryKind::parse(kind.as_str()), Some(kind));
    }
    assert_eq!(LedgerEntryKind::parse("refund"), None);
}

/// **对客形态与成本形态解耦**：成本是 `upstream_declared`（APIMart）的候选也能对客按 token
/// 四档收——`charge_microusd` 读对客形态；成本仍按 `formula` / `cost_rates` 另走一路。
#[test]
fn the_charge_follows_the_consumer_form_not_the_cost_form() {
    let mut snapshot = snapshot_with_rates("USD", 5_000_000, 8_000_000, 10_000_000, 30_000_000);
    snapshot.formula = PricingFormula::UpstreamDeclared;
    snapshot.rates = None;
    snapshot.price_plan_id = None;
    snapshot.consumer_formula = Some(PricingFormula::TokenRates);
    snapshot.consumer_rates_cny = Some(ConsumerRatesCny {
        text_input_micros_per_million: 7_000_000,
        image_input_micros_per_million: 9_000_000,
        text_output_micros_per_million: 11_000_000,
        image_output_micros_per_million: 40_000_000,
    });
    snapshot.markup_bps = Some(2_000);
    snapshot.fx_rate = Some(FxRate {
        currency: "USD".to_owned(),
        rate_micros: 7_100_000,
        effective_at: Utc::now(),
    });
    assert_eq!(
        snapshot.charge_microusd(facts(&usage())),
        Ok(18_000),
        "成本是 upstream_declared，对客仍按四档 CNY 向量收"
    );
}

/// 历史快照没有 `consumer_formula` 时，按**等于成本形态**解释（旧口径）。
#[test]
fn a_legacy_snapshot_reads_the_consumer_form_as_the_cost_form() {
    let mut snapshot = snapshot_with_rates("USD", 5_000_000, 8_000_000, 10_000_000, 30_000_000);
    snapshot.consumer_formula = None;
    assert_eq!(snapshot.consumer_formula(), PricingFormula::TokenRates);
}

#[test]
fn provider_identifiers_are_bounded_and_never_urls_or_payloads() {
    assert!(is_bounded_provider_identifier("task_abc-123.4:x"));
    assert!(is_bounded_provider_identifier(
        "9f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f"
    ));

    for bad in [
        "",
        "https://example.invalid/a.png",
        "data:image/png;base64,AAAA",
        "task id with spaces",
        "line\nbreak",
        "任务标识",
    ] {
        assert!(
            !is_bounded_provider_identifier(bad),
            "{bad:?} must not pass as a provider identifier"
        );
    }
    assert!(
        !is_bounded_provider_identifier(&"a".repeat(MAX_PROVIDER_IDENTIFIER_BYTES + 1)),
        "an over-long value is not an identifier"
    );
}

/// 按 token 计价的候选拿到一个**没有 token 分项**的成功件：算不出该收多少钱，明确失败。
///
/// 这条判据是结算闸门放宽的另一面：声明了成本的渠道可以用那句金额当计量依据，但按 token 计价的
/// 候选不能拿"没有 token"当 0 元。
#[test]
fn a_token_priced_supply_needs_token_evidence() {
    let snapshot = snapshot_with_rates("USD", 5_000_000, 8_000_000, 10_000_000, 30_000_000);
    assert_eq!(
        snapshot.charge_microusd(ChargeFacts {
            usage: None,
            images: 1,
            declared_cost_microusd: None,
        }),
        Err(DomainError::MissingMeteringEvidence)
    );
    // 同一份快照，带用量就算得出来：失败的是"没有证据"，不是"没有费率"。
    assert!(snapshot.charge_microusd(facts(&usage())).is_ok());
}
