use super::*;

/// 省略渠道的候选（渠道三要素与驱动器留空），用来验增量发布的沿用规则。
/// 计价形态**显式给出**，这样这条只验渠道沿用，不牵扯计价沿用。
fn draft_without_channel(provider_model_id: &str) -> OfferingDraft {
    OfferingDraft {
        base_url: None,
        credential_env: None,
        adapter_key: None,
        ..draft(provider_model_id)
    }
}

/// 省略**计价依据**的候选（形态留空），其余照给。
fn draft_without_pricing(provider_model_id: &str) -> OfferingDraft {
    OfferingDraft {
        formula: None,
        price_plan: None,
        cost_unit_price_microusd: None,
        cost_currency: None,
        reference_cost_microusd: None,
        cost_basis: None,
        tier_prices: None,
        floor_amounts: None,
        ..draft(provider_model_id)
    }
}

/// 上一版里可被沿用的一条候选：渠道三要素 + 一份四档渠道费率。
fn previous_offering(
    provider_kind: &str,
    provider_model_id: &str,
    base_url: &str,
) -> ActiveOfferingChannel {
    ActiveOfferingChannel {
        provider_kind: provider_kind.to_owned(),
        provider_model_id: provider_model_id.to_owned(),
        adapter_key: "aihubmix-image-v1".to_owned(),
        base_url: base_url.to_owned(),
        credential_env: "AIHUBMIX_API_KEY".to_owned(),
        formula: "token_rates".to_owned(),
        plan: Some(PricePlanRates {
            currency: "USD".to_owned(),
            text_input_microusd_per_million: 5_000_000,
            image_input_microusd_per_million: 8_000_000,
            text_output_microusd_per_million: 10_000_000,
            image_output_microusd_per_million: 30_000_000,
            source_url: "https://vendor.example.com/pricing".to_owned(),
        }),
        cost_unit_price_microusd: None,
        cost_currency: Some("USD".to_owned()),
        reference_cost_microusd: Some(5_950),
        cost_basis: Some("computed".to_owned()),
        tier_prices: None,
        floor_amounts: None,
    }
}

/// 省略渠道的候选，渠道三要素与驱动器从上一版同 `provider_kind` + `provider_model_id` 的那条取。
#[test]
fn an_omitted_channel_is_inherited_from_the_current_revision() {
    let previous = vec![previous_offering(
        "AIHubMix",
        "gpt-image-2.5-flare",
        "https://api.example.com",
    )];
    let inherited = inherit_offering(0, &draft_without_channel("gpt-image-2.5-flare"), &previous)
        .expect("the channel is inheritable");
    assert_eq!(
        inherited.base_url.as_deref(),
        Some("https://api.example.com")
    );
    assert_eq!(
        inherited.credential_env.as_deref(),
        Some("AIHUBMIX_API_KEY")
    );
    assert_eq!(inherited.adapter_key.as_deref(), Some("aihubmix-image-v1"));
    assert_eq!(inherited.provider_kind.as_deref(), Some("AIHubMix"));
}

/// 整组给了渠道就不沿用：**这条**发布声明的渠道优先于上一版。
#[test]
fn a_declared_channel_is_not_overwritten_by_the_previous_revision() {
    let previous = vec![previous_offering(
        "AIHubMix",
        "gpt-image-2.5-flare",
        "https://old.example.com",
    )];
    let declared = OfferingDraft {
        base_url: Some("https://new.example.com".to_owned()),
        ..draft("gpt-image-2.5-flare")
    };
    let kept =
        inherit_offering(0, &declared, &previous).expect("a declared channel passes through");
    assert_eq!(kept.base_url.as_deref(), Some("https://new.example.com"));
}

/// 上一版里没有同身份的候选（例如新加的候选）：拒绝并点名，不用"最近的那条"顶替。
#[test]
fn an_omitted_channel_without_a_matching_previous_offering_is_rejected() {
    let previous = vec![previous_offering(
        "AIHubMix",
        "another-model",
        "https://api.example.com",
    )];
    let error = inherit_offering(0, &draft_without_channel("gpt-image-2.5-flare"), &previous)
        .expect_err("no match must be rejected");
    let text = error.to_string();
    assert!(
        text.contains("the model's current revision has no offering with that identity"),
        "{text}"
    );
    assert!(
        text.contains("give base_url and credential_env explicitly"),
        "{text}"
    );
}

/// 同一 `provider_kind` 与渠道模型名在上一版有多条候选：**歧义，拒绝**。
///
/// 两条不同渠道（地址或凭证身份不同）可以提供同一个渠道模型名，那时"沿用哪一条"没有唯一答案。
#[test]
fn an_ambiguous_omitted_channel_is_rejected() {
    let previous = vec![
        previous_offering("AIHubMix", "gpt-image-2.5-flare", "https://a.example.com"),
        previous_offering("AIHubMix", "gpt-image-2.5-flare", "https://b.example.com"),
    ];
    let error = inherit_offering(0, &draft_without_channel("gpt-image-2.5-flare"), &previous)
        .expect_err("ambiguity must be rejected");
    assert!(
        error
            .to_string()
            .contains("more than one offering with that identity"),
        "{error}"
    );
}

/// 省略了渠道，却连"这条候选是谁"都没说（缺 `provider_kind` 或渠道模型名）：拒绝。
#[test]
fn an_omitted_channel_without_an_identity_is_rejected() {
    let previous = vec![previous_offering(
        "AIHubMix",
        "gpt-image-2.5-flare",
        "https://a.example.com",
    )];
    let anonymous = OfferingDraft {
        provider_kind: None,
        ..draft_without_channel("gpt-image-2.5-flare")
    };
    let error =
        inherit_offering(0, &anonymous, &previous).expect_err("an anonymous candidate is rejected");
    assert!(
        error
            .to_string()
            .contains("does not say which offering it continues"),
        "{error}"
    );
}

/// 型号还没有任何生效修订（沿用来源为空）：拒绝。
#[test]
fn an_omitted_channel_is_rejected_when_the_model_has_no_revision() {
    let error = inherit_offering(0, &draft_without_channel("gpt-image-2.5-flare"), &[])
        .expect_err("an empty previous revision cannot be inherited from");
    assert!(
        error.to_string().contains("no offering with that identity"),
        "{error}"
    );
}

/// 省略计价依据的候选：形态、四档渠道费率与价目出处都从上一版取。
///
/// 这条守的是"改价不会顺手把渠道价目改坏"：管理端视图不回显渠道费率，运营改一个倍率时那些字段只能是
/// 沿用来的；沿用不到就该拒，而不是落成空值。
#[test]
fn omitted_pricing_is_inherited_from_the_current_revision() {
    let previous = vec![previous_offering(
        "AIHubMix",
        "gpt-image-2.5-flare",
        "https://api.example.com",
    )];
    let inherited = inherit_offering(0, &draft_without_pricing("gpt-image-2.5-flare"), &previous)
        .expect("the pricing is inheritable");
    assert_eq!(inherited.formula.as_deref(), Some("token_rates"));
    let plan = inherited.price_plan.expect("the four rates come back");
    assert_eq!(plan.text_input_microusd_per_million, 5_000_000);
    assert_eq!(plan.source_url, "https://vendor.example.com/pricing");
    assert_eq!(inherited.cost_currency.as_deref(), Some("USD"));
    assert_eq!(inherited.reference_cost_microusd, Some(5_950));
    assert_eq!(inherited.cost_basis.as_deref(), Some("computed"));
}

/// **只省渠道、却显式给了新计价**时，计价用新的——两组字段各自独立判断。
///
/// 少了这条独立判断，"重签渠道价目但不动渠道地址"会被上一版的旧价目覆盖掉。
#[test]
fn a_declared_pricing_is_not_overwritten_when_only_the_channel_is_omitted() {
    let previous = vec![previous_offering(
        "AIHubMix",
        "gpt-image-2.5-flare",
        "https://api.example.com",
    )];
    let mut declared = draft_without_channel("gpt-image-2.5-flare");
    declared.formula = Some("per_call".to_owned());
    declared.price_plan = None;
    declared.cost_unit_price_microusd = Some(7_000);
    let kept = inherit_offering(0, &declared, &previous).expect("the declared pricing wins");
    assert_eq!(kept.formula.as_deref(), Some("per_call"));
    assert_eq!(kept.cost_unit_price_microusd, Some(7_000));
    assert!(kept.price_plan.is_none(), "旧的四档费率不该被带过来");
    // 渠道三要素仍然沿用。
    assert_eq!(kept.base_url.as_deref(), Some("https://api.example.com"));
}

#[test]
fn normalize_rejects_an_empty_offering_array() {
    let command = PublishRuntimeCommand {
        offerings: Some(Vec::new()),
        ..base_command()
    };
    let error = command
        .normalize()
        .expect_err("empty array must be rejected");
    assert!(error.to_string().contains("must not be empty"), "{error}");
}

/// 没有候选集合就没有可发布的东西：缺省与 `null` 都落到同一条校验错误上（发布期 400）。
#[test]
fn normalize_rejects_a_command_without_the_offering_array() {
    let command = base_command();
    let error = command
        .normalize()
        .expect_err("a command without offerings must be rejected");
    assert!(
        error.to_string().contains("offerings is required"),
        "{error}"
    );
}

#[test]
fn normalize_assigns_priority_from_array_index() {
    let command = PublishRuntimeCommand {
        offerings: Some(vec![draft("pm-a"), draft("pm-b"), draft("pm-c")]),
        ..base_command()
    };
    let normalized = command.normalize().expect("array form is valid");
    assert_eq!(normalized.offerings.len(), 3);
    // 缺省档位就是数组下标（没给 `routing_priority` 时唯一的来源）。
    assert_eq!(
        normalized
            .offerings
            .iter()
            .map(|offering| offering.routing_priority)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(normalized.offerings[1].provider_model_id, "pm-b");
}

#[test]
fn normalize_requires_a_carrier_schema_and_price_plan_per_offering() {
    let mut without_carrier = draft("pm-a");
    without_carrier.carrier_schema = None;
    without_carrier.capability_schema = None;
    let command = PublishRuntimeCommand {
        offerings: Some(vec![without_carrier]),
        ..base_command()
    };
    let error = command.normalize().expect_err("carrier is required");
    assert!(error.to_string().contains("carrier_schema"), "{error}");

    let mut without_price = draft("pm-a");
    without_price.price_plan = None;
    let command = PublishRuntimeCommand {
        offerings: Some(vec![without_price]),
        ..base_command()
    };
    let error = command.normalize().expect_err("price plan is required");
    assert!(error.to_string().contains("price_plan"), "{error}");
}

/// 数组形式下顶层给合同、候选给承载面：新形状的正常用法。
#[test]
fn array_form_takes_the_contract_from_the_top_level_and_the_carrier_from_each_offering() {
    let contract = schema("gpt-image-2.5-flare");
    let carrier = surface(serde_json::json!({
        "model": {"const": "gpt-image-2.5-flare"},
        "prompt": {"type": "string", "minLength": 1}
    }));
    let command = PublishRuntimeCommand {
        capability_schema: Some(contract.clone()),
        offerings: Some(vec![
            carrier_draft("pm-a", carrier.clone()),
            carrier_draft("pm-b", carrier.clone()),
        ]),
        ..base_command()
    };
    let normalized = command.normalize().expect("array form is valid");
    assert_eq!(normalized.contract, contract);
    assert_eq!(normalized.offerings[0].carrier_schema, carrier);
    assert_eq!(normalized.offerings[1].carrier_schema, carrier);
}

/// 顶层合同优先：候选自带的旧字段这时是**承载面**，不再是合同。
#[test]
fn top_level_contract_wins_over_the_legacy_offering_field() {
    let contract = schema("gpt-image-2.5-flare");
    let legacy = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": {
            "model": {"const": "gpt-image-2.5-flare"},
            "prompt": {"type": "string"}
        }
    });
    let command = PublishRuntimeCommand {
        capability_schema: Some(contract.clone()),
        offerings: Some(vec![OfferingDraft {
            capability_schema: Some(legacy.clone()),
            ..draft("pm-a")
        }]),
        ..base_command()
    };
    let normalized = command.normalize().expect("top-level contract wins");
    assert_eq!(normalized.contract, contract);
    assert_eq!(normalized.offerings[0].carrier_schema, legacy);
}

/// 过渡期回退：顶层没给合同，老素材那份 offering 级声明面同时当合同与承载面。
#[test]
fn legacy_offering_schema_falls_back_to_the_contract() {
    let command = PublishRuntimeCommand {
        offerings: Some(vec![draft("pm-a")]),
        ..base_command()
    };
    let normalized = command
        .normalize()
        .expect("legacy material still publishes");
    assert_eq!(normalized.contract, schema("gpt-image-2.5-flare"));
    assert_eq!(normalized.offerings[0].carrier_schema, normalized.contract);
}

/// 回退时各候选的旧字段必须一致：合同只有一份，两份内容不能同时当合同。
#[test]
fn legacy_offering_schemas_that_disagree_are_rejected() {
    let other = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": {
            "model": {"const": "gpt-image-2.5-flare"},
            "prompt": {"type": "string"},
            "image_urls": {"type": "array", "items": {"type": "string"}}
        }
    });
    let command = PublishRuntimeCommand {
        offerings: Some(vec![
            draft("pm-a"),
            OfferingDraft {
                capability_schema: Some(other),
                ..draft("pm-b")
            },
        ]),
        ..base_command()
    };
    let error = command
        .normalize()
        .expect_err("two different contracts for one model must be rejected");
    assert!(
        error.to_string().contains("different capability schemas"),
        "{error}"
    );
}

/// 对客名缺省回退取厂商原生名：老素材、老已发布数据与老测试的形状因此逐位不变。
#[test]
fn the_gateway_name_falls_back_to_the_vendor_name_when_absent() {
    let request = base_command().into_request(schema("gpt-image-2.5-flare"), Vec::new(), false);
    assert_eq!(request.gateway_model, "gpt-image-2.5-flare");
    assert_eq!(request.native_model_id, "gpt-image-2.5-flare");

    // 给了就用给的：同一份供给可以包成另一个对客名，而合同挂的还是那个厂商模型。
    let command = PublishRuntimeCommand {
        gateway_model: Some("gpt-image-2.5-plus".to_owned()),
        ..base_command()
    };
    let request = command.into_request(schema("gpt-image-2.5-flare"), Vec::new(), false);
    assert_eq!(request.gateway_model, "gpt-image-2.5-plus");
    assert_eq!(request.native_model_id, "gpt-image-2.5-flare");

    // 只写空白等于没写：对客目录不该列出一个空名字。
    let command = PublishRuntimeCommand {
        gateway_model: Some("   ".to_owned()),
        ..base_command()
    };
    let request = command.into_request(schema("gpt-image-2.5-flare"), Vec::new(), false);
    assert_eq!(request.gateway_model, "gpt-image-2.5-flare");
}

/// 一次发布定义的就是一个网关模型：名字不能是空白。
#[test]
fn a_publish_must_declare_a_gateway_name() {
    let mut request = base_command().into_request(schema("gpt-image-2.5-flare"), Vec::new(), false);
    assert!(validate_gateway_model_identity(&request).is_ok());
    request.gateway_model = "  ".to_owned();
    let error = validate_gateway_model_identity(&request)
        .expect_err("a blank gateway name must be rejected");
    assert!(error.to_string().contains("gateway_model"), "{error}");
}

/// 合同的身份就是发布的型号：`model.const` 不符即拒绝（换型号要发新的合同）。
#[test]
fn contract_identity_must_match_the_published_model() {
    let contract = schema("gpt-image-2.5-flare");
    assert!(validate_contract("gpt-image-2.5-flare", &contract).is_ok());
    let error = validate_contract("another-model", &contract)
        .expect_err("a contract for another model must be rejected");
    assert!(error.to_string().contains("model.const"), "{error}");
    // 不封闭的 schema 不是合同：调用方写错字段名会被静默收下。
    let open = serde_json::json!({
        "type": "object",
        "properties": {"model": {"const": "m"}, "prompt": {"type": "string"}}
    });
    assert!(validate_contract("m", &open).is_err());
}

#[test]
fn normalize_rejects_a_missing_or_unknown_pricing_formula() {
    // 一条供给说不清它按什么计价，它的成本就没有算法：缺形态与写错取值都要显式拒绝，
    // 而不是静默落库成某个默认形态。
    let mut without_formula = draft("pm-a");
    without_formula.formula = None;
    let command = PublishRuntimeCommand {
        offerings: Some(vec![without_formula]),
        ..base_command()
    };
    let error = command
        .normalize()
        .expect_err("a supply without a pricing formula must fail");
    assert!(error.to_string().contains("formula is required"), "{error}");

    let mut unknown = draft("pm-a");
    unknown.formula = Some("by_the_hour".to_owned());
    let command = PublishRuntimeCommand {
        offerings: Some(vec![unknown]),
        ..base_command()
    };
    let error = command.normalize().expect_err("unknown formula must fail");
    assert!(error.to_string().contains("token_rates"), "{error}");
}

#[test]
fn normalize_rejects_parameters_that_do_not_match_the_formula() {
    // 形态与参数配套：按 token 计量量要有那份四档费率；按张 / 按次要有一个单价；上游直接
    // 给金额两种参数都不要。缺了要说清缺哪一个，给了用不到的也要拒。
    let mut token_without_plan = draft("pm-a");
    token_without_plan.price_plan = None;
    let error = PublishRuntimeCommand {
        offerings: Some(vec![token_without_plan]),
        ..base_command()
    }
    .normalize()
    .expect_err("token_rates without a price plan must fail");
    assert!(
        error.to_string().contains("price_plan is required"),
        "{error}"
    );

    let mut per_image_without_unit_price = draft("pm-a");
    per_image_without_unit_price.formula = Some("per_image".to_owned());
    per_image_without_unit_price.price_plan = None;
    per_image_without_unit_price.cost_currency = Some("USD".to_owned());
    let error = PublishRuntimeCommand {
        offerings: Some(vec![per_image_without_unit_price.clone()]),
        ..base_command()
    }
    .normalize()
    .expect_err("per_image without a unit price must fail");
    assert!(
        error.to_string().contains("cost_unit_price_microusd"),
        "{error}"
    );

    // 按张 / 按次**不必发**那份四档费率：这就是"Price Plan 不是每条供给必填"。但成本形态是按张 /
    // 按次时，对客形态没有可沿用的取值（对客只有按 token 四档 / 上游声明金额两种），必须显式给出。
    per_image_without_unit_price.cost_unit_price_microusd = Some(11_354);
    let error = PublishRuntimeCommand {
        offerings: Some(vec![per_image_without_unit_price.clone()]),
        ..base_command()
    }
    .normalize()
    .expect_err("a per_image cost form must name its consumer form explicitly");
    assert!(error.to_string().contains("consumer_formula"), "{error}");

    // 显式选一种对客形态（这里选上游声明金额 × 倍率）就能发布；成本形态仍按张。
    let normalized = PublishRuntimeCommand {
        markup_bps: Some(2_000),
        offerings: Some(vec![OfferingDraft {
            consumer_formula: Some("upstream_declared".to_owned()),
            ..per_image_without_unit_price.clone()
        }]),
        ..base_command()
    }
    .normalize()
    .expect("a per_image cost form can be sold with an explicit consumer form");
    assert_eq!(normalized.offerings[0].formula, PricingFormula::PerImage);
    assert!(normalized.offerings[0].rates.is_none());
    assert_eq!(normalized.offerings[0].cost_currency(), Some("USD"));
    assert_eq!(
        normalized.offerings[0].consumer_formula,
        PricingFormula::UpstreamDeclared
    );
    assert!(normalized.offerings[0].consumer_rates_cny.is_none());

    // 反向：对客选上游声明金额时给一份对客四档向量也要拒——那个数永远不会被读。
    let mut per_image_with_rates = per_image_without_unit_price;
    per_image_with_rates.consumer_formula = Some("upstream_declared".to_owned());
    per_image_with_rates.consumer_rates_cny = Some(ConsumerRatesCny {
        text_input_micros_per_million: 35_500_000,
        image_input_micros_per_million: 56_800_000,
        text_output_micros_per_million: 71_000_000,
        image_output_micros_per_million: 213_000_000,
    });
    let error = PublishRuntimeCommand {
        markup_bps: Some(2_000),
        offerings: Some(vec![per_image_with_rates]),
        ..base_command()
    }
    .normalize()
    .expect_err("a consumer vector under upstream_declared must fail");
    assert!(
        error
            .to_string()
            .contains("consumer_rates_cny does not apply"),
        "{error}"
    );

    // 反向：形态用不到的参数也要拒（它永远不会被读，留着只会让人以为它在生效）。
    let mut token_rates_with_unit_price = draft("pm-a");
    token_rates_with_unit_price.cost_unit_price_microusd = Some(11_354);
    let error = PublishRuntimeCommand {
        offerings: Some(vec![token_rates_with_unit_price]),
        ..base_command()
    }
    .normalize()
    .expect_err("a unit price under token_rates must fail");
    assert!(
        error.to_string().contains("does not apply to formula"),
        "{error}"
    );

    let mut declared_with_plan = draft("pm-a");
    declared_with_plan.formula = Some("upstream_declared".to_owned());
    let error = PublishRuntimeCommand {
        offerings: Some(vec![declared_with_plan]),
        ..base_command()
    }
    .normalize()
    .expect_err("a price plan under upstream_declared must fail");
    assert!(
        error.to_string().contains("does not apply to formula"),
        "{error}"
    );

    // 没有 Price Plan 时成本币种必须显式声明：上游声明的金额与单价都要说清是哪个币种的钱。
    let mut declared_without_currency = draft("pm-a");
    declared_without_currency.formula = Some("upstream_declared".to_owned());
    declared_without_currency.price_plan = None;
    let error = PublishRuntimeCommand {
        offerings: Some(vec![declared_without_currency]),
        ..base_command()
    }
    .normalize()
    .expect_err("a supply without a price plan must declare its cost currency");
    assert!(error.to_string().contains("cost_currency"), "{error}");
}

#[test]
fn a_priced_candidate_normalizes_with_its_declared_cost_currency() {
    let command = PublishRuntimeCommand {
        markup_bps: Some(2_000),
        offerings: Some(vec![priced_draft("pm-a")]),
        ..base_command()
    };
    let normalized = command.normalize().expect("a priced candidate is valid");
    let pricing = normalized.offerings[0]
        .pricing
        .as_ref()
        .expect("the candidate must carry its pricing");
    assert_eq!(
        normalized.offerings[0].cost_currency(),
        Some("USD"),
        "没显式声明成本币种时取该供给 Price Plan 的币种"
    );
    assert_eq!(pricing.reference_cost_microusd, 11_354);
    assert_eq!(pricing.cost_basis, CostBasis::Declared);
    assert_eq!(normalized.offerings[0].pricing.as_ref(), Some(pricing));
}

#[test]
fn a_candidate_cost_currency_must_match_its_price_plan_currency() {
    let mut mismatched = draft("pm-a");
    mismatched.cost_currency = Some("CNY".to_owned());
    let command = PublishRuntimeCommand {
        offerings: Some(vec![mismatched]),
        ..base_command()
    };
    let error = command
        .normalize()
        .expect_err("two currencies for one supply must fail");
    assert!(error.to_string().contains("cost_currency"), "{error}");
}

#[test]
fn a_candidate_that_carries_half_a_pricing_is_rejected() {
    // 只给对客费率、不给参考成本与保底表：发布出来就是"有售价、说不清成本、也算不出预授权"
    // 的半成品，问题要等到结算才暴露。
    let command = PublishRuntimeCommand {
        markup_bps: Some(2_000),
        offerings: Some(vec![OfferingDraft {
            floor_amounts: None,
            ..priced_draft("pm-a")
        }]),
        ..base_command()
    };
    let error = command.normalize().expect_err("half a pricing must fail");
    assert!(error.to_string().contains("floor_amounts"), "{error}");

    let command = PublishRuntimeCommand {
        markup_bps: Some(2_000),
        offerings: Some(vec![OfferingDraft {
            reference_cost_microusd: None,
            ..priced_draft("pm-a")
        }]),
        ..base_command()
    };
    let error = command.normalize().expect_err("half a pricing must fail");
    assert!(
        error.to_string().contains("reference_cost_microusd"),
        "{error}"
    );
}

#[test]
fn markup_may_be_omitted_but_never_negative() {
    // 带定价却**没给**加价系数是合法的：管理员可以直接录入对客费率向量，那一步用不上它。
    let command = PublishRuntimeCommand {
        offerings: Some(vec![priced_draft("pm-a")]),
        ..base_command()
    };
    let normalized = command
        .normalize()
        .expect("a directly entered consumer rate vector needs no markup");
    assert!(
        normalized.offerings[0].pricing.is_some(),
        "定价照旧完整地归一出来"
    );

    // 给了加价系数但这条修订没有对客选"上游金额 × 倍率"的候选：合法——它仍用于推导对客 token
    // 价目的初始值，不再当作"没人读的记录"拒掉。
    PublishRuntimeCommand {
        markup_bps: Some(2_000),
        offerings: Some(vec![draft("pm-a")]),
        ..base_command()
    }
    .normalize()
    .expect("a markup that only seeds the token price list is allowed");

    let command = PublishRuntimeCommand {
        markup_bps: Some(-1),
        offerings: Some(vec![priced_draft("pm-a")]),
        ..base_command()
    };
    let error = command.normalize().expect_err("negative markup");
    assert!(error.to_string().contains("negative"), "{error}");
}

#[test]
fn a_malformed_floor_table_is_rejected_at_publication() {
    let command = PublishRuntimeCommand {
        markup_bps: Some(2_000),
        offerings: Some(vec![OfferingDraft {
            floor_amounts: Some(serde_json::json!({"amounts": {"2K": 250_000, "2k": 260_000}})),
            ..priced_draft("pm-a")
        }]),
        ..base_command()
    };
    let error = command
        .normalize()
        .expect_err("the same tier twice must be rejected before it can be looked up");
    assert!(error.to_string().contains("2K"), "{error}");
}

/// **对客计价形态与成本形态解耦**：成本是 `upstream_declared` 的候选也能对客按 token 四档卖——
/// 只要显式给出对客四档向量（那条路径没有成本费率可沿用）。
#[test]
fn a_consumer_token_form_is_allowed_when_the_cost_is_upstream_declared() {
    let command = PublishRuntimeCommand {
        offerings: Some(vec![OfferingDraft {
            provider_kind: Some("APIMart".to_owned()),
            adapter_key: Some("apimart-image-v1".to_owned()),
            formula: Some("upstream_declared".to_owned()),
            price_plan: None,
            cost_currency: Some("USD".to_owned()),
            consumer_formula: Some("token_rates".to_owned()),
            consumer_rates_cny: Some(ConsumerRatesCny {
                text_input_micros_per_million: 7_000_000,
                image_input_micros_per_million: 9_000_000,
                text_output_micros_per_million: 11_000_000,
                image_output_micros_per_million: 40_000_000,
            }),
            ..draft("gpt-image-2.5-flare")
        }]),
        ..base_command()
    };
    let normalized = command
        .normalize()
        .expect("the token consumer form is allowed");
    assert_eq!(
        normalized.offerings[0].consumer_formula,
        PricingFormula::TokenRates
    );
    assert_eq!(
        normalized.offerings[0].formula,
        PricingFormula::UpstreamDeclared,
        "成本形态不受对客形态影响"
    );
}

/// 对客选 token 四档、成本不是 token 计量量时，必须显式给对客向量：没有成本费率可沿用。
#[test]
fn a_consumer_token_form_without_rates_is_rejected_when_the_cost_is_not_token_rates() {
    let command = PublishRuntimeCommand {
        offerings: Some(vec![OfferingDraft {
            provider_kind: Some("APIMart".to_owned()),
            adapter_key: Some("apimart-image-v1".to_owned()),
            formula: Some("upstream_declared".to_owned()),
            price_plan: None,
            cost_currency: Some("USD".to_owned()),
            consumer_formula: Some("token_rates".to_owned()),
            consumer_rates_cny: None,
            ..draft("gpt-image-2.5-flare")
        }]),
        ..base_command()
    };
    let error = command.normalize().expect_err("no rates to fall back on");
    assert!(error.to_string().contains("consumer_rates_cny"), "{error}");
}

/// 对客形态只有按 token 四档 / 上游声明金额两种：给出按张 / 按次被拒并点名。
#[test]
fn a_consumer_per_image_form_is_rejected() {
    let command = PublishRuntimeCommand {
        offerings: Some(vec![OfferingDraft {
            consumer_formula: Some("per_image".to_owned()),
            ..draft("pm-a")
        }]),
        ..base_command()
    };
    let error = command
        .normalize()
        .expect_err("the per_image consumer form is not offered");
    assert!(
        error
            .to_string()
            .contains("consumer_formula must be token_rates or upstream_declared"),
        "{error}"
    );
}
