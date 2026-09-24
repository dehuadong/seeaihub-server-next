use super::*;
use chrono::Utc;

fn rate(currency: &str, rate_micros: u64) -> FxRate {
    FxRate {
        currency: currency.to_owned(),
        rate_micros,
        effective_at: Utc::now(),
    }
}

/// 恰好落在上限上算通过：上限说的是"最多允许多少"，等于它就没有超过它（多一微单位才算超）。
#[test]
fn a_cost_exactly_at_the_ceiling_is_allowed() {
    let ceiling = RequestCostCeiling::new(5_000_000).expect("ceiling");
    assert!(!ceiling.exceeded_by(5_000_000), "等于上限不算超");
    assert!(!ceiling.exceeded_by(4_999_999));
    assert!(ceiling.exceeded_by(5_000_001), "多一微单位就是超");
    assert_eq!(ceiling.max_request_cost_microusd(), 5_000_000);
}

/// 上限为 0 是配置错，不是"关停"：真要关停该走供给与渠道那条路。
#[test]
fn a_zero_ceiling_is_rejected_as_configuration() {
    assert!(matches!(
        RequestCostCeiling::new(0),
        Err(ApplicationError::Configuration(_))
    ));
    assert_eq!(
        RequestCostCeiling::default_ceiling().max_request_cost_microusd(),
        DEFAULT_MAX_REQUEST_COST_MICROUSD
    );
}

/// `per_image`：这一次要几张就乘几张。
#[test]
fn a_per_image_candidate_costs_its_unit_price_times_the_images() {
    assert_eq!(
        single_request_cost_native(PricingFormula::PerImage, Some(1_200_000), None, 10),
        Some(12_000_000)
    );
    assert_eq!(
        single_request_cost_native(PricingFormula::PerImage, Some(1_200_000), None, 1),
        Some(1_200_000)
    );
}

/// `per_call` 与张数无关：一次调用就是一个单价。
#[test]
fn a_per_call_candidate_costs_one_unit_price_whatever_the_images() {
    assert_eq!(
        single_request_cost_native(PricingFormula::PerCall, Some(300_000), None, 1),
        Some(300_000)
    );
    assert_eq!(
        single_request_cost_native(PricingFormula::PerCall, Some(300_000), None, 10),
        Some(300_000)
    );
    // 与张数无关的形态在边界上同样按 `>` 判：恰好等于上限算通过。
    let ceiling = RequestCostCeiling::new(300_000).expect("ceiling");
    let cost = single_request_cost_native(PricingFormula::PerCall, Some(300_000), None, 10)
        .expect("a per-call candidate always has a cost");
    assert!(!ceiling.exceeded_by(cost), "恰好等于上限算通过");
}

/// 按 token 计量量与上游给金额这两种形态没有"每张单价"：能用的声明数就是发布者给的参考成本。
#[test]
fn the_two_forms_without_a_unit_price_use_the_published_reference_cost() {
    for formula in [PricingFormula::TokenRates, PricingFormula::UpstreamDeclared] {
        assert_eq!(
            single_request_cost_native(formula, None, Some(11_354), 10),
            Some(11_354),
            "{formula:?} 取参考成本"
        );
        assert_eq!(
            single_request_cost_native(formula, Some(9_999_999), Some(11_354), 10),
            Some(11_354),
            "{formula:?} 的唯一依据是参考成本，不是单价"
        );
    }
}

/// 参数缺失是"算不出这一次要花多少"，不是"花 0 元"：判不出来就不判，不能当作 0 放行。
#[test]
fn a_missing_parameter_is_not_a_zero_cost() {
    assert_eq!(
        single_request_cost_native(PricingFormula::PerImage, None, None, 4),
        None
    );
    assert_eq!(
        single_request_cost_native(PricingFormula::PerCall, None, None, 1),
        None
    );
    assert_eq!(
        single_request_cost_native(PricingFormula::TokenRates, None, None, 1),
        None
    );
    assert_eq!(
        single_request_cost_native(PricingFormula::UpstreamDeclared, None, None, 1),
        None
    );
}

/// 折算率只有与该候选的成本币种对上才用：拿另一个币种的率去折，折出来的是另一个数。
#[test]
fn conversion_needs_the_rate_of_the_candidates_own_currency() {
    let usd = rate("USD", 7_100_000);
    assert_eq!(
        single_request_cost_cny(
            PricingFormula::UpstreamDeclared,
            None,
            Some(11_354),
            Some("USD"),
            Some(&usd),
            1
        ),
        Some(80_614),
        "11354 微美元按 7.1 折出来是 80614 微人民币（向上取整）"
    );
    assert_eq!(
        single_request_cost_cny(
            PricingFormula::UpstreamDeclared,
            None,
            Some(11_354),
            Some("CNY"),
            Some(&usd),
            1
        ),
        None,
        "币种对不上就不判"
    );
    assert_eq!(
        single_request_cost_cny(
            PricingFormula::UpstreamDeclared,
            None,
            Some(11_354),
            Some("USD"),
            None,
            1
        ),
        None,
        "没有折算率就不判"
    );
    assert_eq!(
        single_request_cost_cny(
            PricingFormula::PerCall,
            Some(5_000_000),
            None,
            Some("CNY"),
            Some(&rate("CNY", 1_000_000)),
            1
        ),
        Some(5_000_000),
        "同币种的率恒为 1，折出来逐位不变"
    );
}
