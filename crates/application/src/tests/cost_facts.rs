//! 成本事实的三态判定与折算：判据是"成本从哪来"，不是"金额对不对"。
//!
//! 这些用例只碰 [`provider_cost_fact`] 与它读的那份快照——成功件与失败件共用同一处映射，
//! 直接执行与对账都经它落成本列。

use super::*;
use seeai_domain::ChargeFacts;

/// 成本来源的判定：上游给金额就**直接取**，渠道不报就按实际用量自算，拿不到就留空（不猜）。
///
/// 同一个用量与同一份快照下走三态，钉住"判据是成本从哪来，不是金额对不对"。
#[test]
fn provider_cost_source_follows_where_the_cost_came_from() {
    let snapshot = offering().price_snapshot;
    let usage = TokenUsage {
        input_tokens: 9,
        input_text_tokens: 9,
        input_image_tokens: 0,
        output_tokens: 196,
        output_text_tokens: 0,
        output_image_tokens: 196,
        total_tokens: 205,
    };
    // 声明分支：金额与币种**都取上游随金额报回的那一份**，自算那份费率不参与这一态
    // ——不是"传了却忘了用"，是这条来源本就不认它。
    let declared = provider_cost_fact(
        &snapshot,
        &ProviderCost::Declared(seeai_adapter_sdk::DeclaredCost {
            amount_microusd: 11_354,
            currency: "CNY".to_owned(),
        }),
        CostInputs::Succeeded {
            usage: &usage,
            images: 1,
        },
    );
    assert_eq!(declared.source, ProviderCostSource::Declared);
    assert_eq!(
        declared.amount_microusd,
        Some(11_354),
        "上游给了金额就直接取它，不许用自算值顶替"
    );
    assert_eq!(
        declared.currency.as_deref(),
        Some("CNY"),
        "币种按上游报的那一份，不取渠道声明的成本币种、也不假定 USD"
    );

    let computed = provider_cost_fact(
        &snapshot,
        &ProviderCost::Computed,
        CostInputs::Succeeded {
            usage: &usage,
            images: 1,
        },
    );
    assert_eq!(computed.source, ProviderCostSource::Computed);
    // 9 文本输入 × 5 + 196 图像输出 × 30（每 1M）。
    assert_eq!(computed.amount_microusd, Some(5_925));
    assert_eq!(computed.currency.as_deref(), Some("USD"));

    let unavailable = provider_cost_fact(
        &snapshot,
        &ProviderCost::Unavailable,
        CostInputs::Succeeded {
            usage: &usage,
            images: 1,
        },
    );
    assert_eq!(unavailable.source, ProviderCostSource::Unavailable);
    assert_eq!(
        unavailable.amount_microusd, None,
        "拿不到金额就留空，不许写 0"
    );
    assert_eq!(unavailable.currency, None);
    // 这份快照没有定价（没有冻结的汇率）：折算值留空，是"没有折算值"，不是"折算成了 0"。
    for fact in [&declared, &computed, &unavailable] {
        assert_eq!(fact.cny_microusd, None);
    }

    // 用量不在手里（失败件没有本次用量）：自算那一态算不出金额，按缺口落，不猜。
    let without_usage = provider_cost_fact(&snapshot, &ProviderCost::Computed, CostInputs::Failed);
    assert_eq!(without_usage.source, ProviderCostSource::Unavailable);
    assert_eq!(without_usage.amount_microusd, None);
    assert_eq!(without_usage.currency, None);
}

/// 按张 / 按次计费的渠道：成本 = **数量 × 单价**，形态与它的参数随快照冻结。
///
/// 逐位断言三种情形：按张看**产出的张数**、按次永远是**一次的钱**（不随张数变）、
/// 上游直接给金额的形态平台没有可算的东西（上游没给就是缺口）。这两种形态的快照里
/// **没有四档费率**——Price Plan 是 token 计量量那一种形态的参数，不是成本自算的前提。
#[test]
fn a_supply_priced_per_image_or_per_call_computes_from_its_unit_price() {
    let usage = TokenUsage {
        input_tokens: 9,
        input_text_tokens: 9,
        input_image_tokens: 0,
        output_tokens: 196,
        output_text_tokens: 0,
        output_image_tokens: 196,
        total_tokens: 205,
    };
    let mut snapshot = offering().price_snapshot;
    snapshot.price_plan_id = None;
    snapshot.rates = None;
    snapshot.cost_currency = Some("USD".to_owned());
    snapshot.fx_rate = Some(FxRate {
        currency: "USD".to_owned(),
        rate_micros: 7_100_000,
        effective_at: Utc::now(),
    });

    // 按张：3 张 × 11_354 = 34_062（微单位），折算 34_062 × 7.1 = 241840.2 ⇒ 241841。
    let mut per_image = snapshot.clone();
    per_image.formula = PricingFormula::PerImage;
    per_image.cost_unit_price_microusd = Some(11_354);
    let fact = provider_cost_fact(
        &per_image,
        &ProviderCost::Computed,
        CostInputs::Succeeded {
            usage: &usage,
            images: 3,
        },
    );
    assert_eq!(fact.source, ProviderCostSource::Computed);
    assert_eq!(fact.amount_microusd, Some(34_062), "按张 = 张数 × 单价");
    assert_eq!(fact.currency.as_deref(), Some("USD"));
    assert_eq!(
        fact.cny_microusd,
        Some(241_841),
        "按冻结的汇率折成人民币算毛利"
    );

    // 按次：一次的钱，产出 7 张也一样。
    let mut per_call = snapshot.clone();
    per_call.formula = PricingFormula::PerCall;
    per_call.cost_unit_price_microusd = Some(20_000);
    let fact = provider_cost_fact(
        &per_call,
        &ProviderCost::Computed,
        CostInputs::Succeeded {
            usage: &usage,
            images: 7,
        },
    );
    assert_eq!(fact.amount_microusd, Some(20_000), "按次 = 1 × 单价");

    // 上游直接给金额的形态：平台没有可算的东西，上游没给就是缺口（不编一个数）。
    let mut declared_by_upstream = snapshot.clone();
    declared_by_upstream.formula = PricingFormula::UpstreamDeclared;
    let fact = provider_cost_fact(
        &declared_by_upstream,
        &ProviderCost::Computed,
        CostInputs::Succeeded {
            usage: &usage,
            images: 3,
        },
    );
    assert_eq!(fact.source, ProviderCostSource::Unavailable);
    assert_eq!(fact.amount_microusd, None);
    assert_eq!(fact.currency, None);

    // 失败件手里没有产出张数：按张算不出来 ⇒ 缺口，不用别的数顶替。
    let failed = provider_cost_fact(&per_image, &ProviderCost::Computed, CostInputs::Failed);
    assert_eq!(failed.source, ProviderCostSource::Unavailable);
    assert_eq!(failed.amount_microusd, None);

    // 上游给了金额时形态不参与：直接取它，失败件也一样（那是执行事实，不是自算）。
    let declared = provider_cost_fact(
        &per_image,
        &ProviderCost::Declared(seeai_adapter_sdk::DeclaredCost {
            amount_microusd: 11_354,
            currency: "USD".to_owned(),
        }),
        CostInputs::Failed,
    );
    assert_eq!(declared.source, ProviderCostSource::Declared);
    assert_eq!(declared.amount_microusd, Some(11_354));
    assert_eq!(declared.cny_microusd, Some(80_614));
}

/// 折算只在**成本币种与冻结的汇率对得上**时才做，且按定点整数算。
#[test]
fn the_cost_is_converted_with_the_frozen_rate_of_its_own_currency() {
    let mut snapshot = offering().price_snapshot;
    snapshot.fx_rate = Some(FxRate {
        currency: "USD".to_owned(),
        rate_micros: 7_100_000,
        effective_at: Utc::now(),
    });
    let usage = TokenUsage {
        input_tokens: 0,
        input_text_tokens: 0,
        input_image_tokens: 0,
        output_tokens: 0,
        output_text_tokens: 0,
        output_image_tokens: 0,
        total_tokens: 0,
    };
    let declared = provider_cost_fact(
        &snapshot,
        &ProviderCost::Declared(seeai_adapter_sdk::DeclaredCost {
            amount_microusd: 11_354,
            currency: "USD".to_owned(),
        }),
        CostInputs::Succeeded {
            usage: &usage,
            images: 1,
        },
    );
    // 11354 微美元 × 7.1 = 80613.4 微元 ⇒ 向上取整。
    assert_eq!(declared.cny_microusd, Some(80_614));

    // 上游报的币种与冻结的汇率不是一回事：不折（留空），不拿另一个币种的汇率去乘。
    let foreign = provider_cost_fact(
        &snapshot,
        &ProviderCost::Declared(seeai_adapter_sdk::DeclaredCost {
            amount_microusd: 11_354,
            currency: "CNY".to_owned(),
        }),
        CostInputs::Succeeded {
            usage: &usage,
            images: 1,
        },
    );
    assert_eq!(foreign.amount_microusd, Some(11_354));
    assert_eq!(foreign.cny_microusd, None);
}

/// 记进成本列的是**按渠道成本费率自算的成本**，不是对客扣费。
///
/// 把两个口径**人为设成不同的值**（成本读渠道成本费率、对客读自己的 CNY 向量）：这条断言
/// 防的是"又把对客金额接回来当成本"——那会让成本跟着售价漂移，而两者本来是两个量。
#[test]
fn the_recorded_computed_cost_is_not_the_consumer_charge() {
    let mut snapshot = offering().price_snapshot;
    snapshot.consumer_rates_cny = Some(ConsumerRatesCny {
        text_input_micros_per_million: 7_000_000,
        image_input_micros_per_million: 9_000_000,
        text_output_micros_per_million: 11_000_000,
        image_output_micros_per_million: 40_000_000,
    });
    let usage = TokenUsage {
        input_tokens: 9,
        input_text_tokens: 9,
        input_image_tokens: 0,
        output_tokens: 196,
        output_text_tokens: 0,
        output_image_tokens: 196,
        total_tokens: 205,
    };
    let cost = snapshot
        .cost_rates()
        .expect("这条快照带 Price Plan")
        .amount_microusd(&usage)
        .expect("cost rates price the usage");
    let charge = snapshot
        .charge_microusd(ChargeFacts {
            usage: &usage,
            images: 1,
            declared_cost_microusd: None,
        })
        .expect("consumer rates price the usage");
    assert_ne!(cost, charge, "用例得先让两个口径真的不同");

    let fact = provider_cost_fact(
        &snapshot,
        &ProviderCost::Computed,
        CostInputs::Succeeded {
            usage: &usage,
            images: 1,
        },
    );
    assert_eq!(
        fact.amount_microusd,
        Some(cost),
        "成本列记的是渠道成本费率算出来的钱"
    );
    assert_ne!(fact.amount_microusd, Some(charge), "成本列不许记对客扣费");
}
