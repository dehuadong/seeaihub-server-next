use super::*;

pub(super) fn offering() -> PublishedOffering {
    let carrier = carrier_schema();
    PublishedOffering {
        runtime_revision_id: RuntimeRevisionId::new(),
        vendor_model_id: VendorModelId::new(),
        offering_id: OfferingId::new(),
        channel_id: ChannelId::new(),
        gateway_model: "gpt-image-2".to_owned(),
        native_revision: "2026-04-21".to_owned(),
        // 这份夹具里合同与承载面同值：它要覆盖的是受理侧"按承载面过滤与装载"的行为。
        capability_schema: carrier.clone(),
        carrier_schema: carrier,
        parameter_mapping: serde_json::json!({}),
        restrictions: serde_json::json!({
            "allowed_branches": ["prompt_only", "image_conditioned", "masked"],
            "max_reference_images": 1
        }),
        adapter_key: "aihubmix-image-v1".to_owned(),
        provider_model_id: "gpt-image-2".to_owned(),
        provider_kind: "AIHubMix".to_owned(),
        base_url: "https://api.inferera.com".to_owned(),
        credential_env: "AIHUBMIX_API_KEY".to_owned(),
        price_snapshot: PriceSnapshot {
            price_plan_id: Some(PricePlanId::new()),
            rates: Some(PriceRates {
                currency: "USD".to_owned(),
                text_input_microusd_per_million: 5_000_000,
                image_input_microusd_per_million: 8_000_000,
                text_output_microusd_per_million: 10_000_000,
                image_output_microusd_per_million: 30_000_000,
            }),
            formula: PricingFormula::TokenRates,
            cost_unit_price_microusd: None,
            consumer_formula: None,
            captured_at: Utc::now(),
            // 夹具走**旧口径**（没有定价）：受理与结算的行为与今天逐位相同。
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
        },
    }
}

/// 夹具里那条供给**能承载**的字段面。
pub(super) fn carrier_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": {
            "model": {"const": "gpt-image-2"},
            "prompt": {"type": "string", "minLength": 1},
            "image": {"type": "string"},
            "mask": {"type": "string"}
        }
    })
}

/// 把一个已发布供给变成"发布物里的候选"：字段完全一致，只多 `routing_priority` 与 `weight`。
pub(super) fn candidate_of(
    offering: &PublishedOffering,
    routing_priority: i32,
) -> OfferingCandidate {
    candidate_with_weight(offering, routing_priority, 1)
}

/// 同 `candidate_of`，但能指定档内分流比。
pub(super) fn candidate_with_weight(
    offering: &PublishedOffering,
    routing_priority: i32,
    weight: u32,
) -> OfferingCandidate {
    OfferingCandidate {
        runtime_revision_id: offering.runtime_revision_id,
        vendor_model_id: offering.vendor_model_id,
        offering_id: offering.offering_id,
        channel_id: offering.channel_id,
        gateway_model: offering.gateway_model.clone(),
        native_revision: offering.native_revision.clone(),
        capability_schema: offering.capability_schema.clone(),
        carrier_schema: offering.carrier_schema.clone(),
        parameter_mapping: offering.parameter_mapping.clone(),
        restrictions: offering.restrictions.clone(),
        adapter_key: offering.adapter_key.clone(),
        provider_model_id: offering.provider_model_id.clone(),
        provider_kind: offering.provider_kind.clone(),
        base_url: offering.base_url.clone(),
        credential_env: offering.credential_env.clone(),
        price_snapshot: offering.price_snapshot.clone(),
        routing_priority,
        weight,
    }
}

pub(super) fn schema(model: &str) -> Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": {
            "model": {"const": model},
            "prompt": {"type": "string", "minLength": 1}
        }
    })
}

pub(super) fn base_command() -> PublishRuntimeCommand {
    PublishRuntimeCommand {
        vendor_id: Some("OpenAI".to_owned()),
        native_model_id: Some("gpt-image-2.5-flare".to_owned()),
        gateway_model: None,
        native_revision: Some("test-1".to_owned()),
        capability_schema: None,
        offerings: None,
        references: None,
        markup_bps: None,
        actor: "tester".to_owned(),
    }
}

pub(super) fn price_plan() -> PricePlanDraft {
    PricePlanDraft {
        currency: "USD".to_owned(),
        text_input_microusd_per_million: 5_000_000,
        image_input_microusd_per_million: 8_000_000,
        text_output_microusd_per_million: 10_000_000,
        image_output_microusd_per_million: 30_000_000,
        source_url: "https://example.invalid/price".to_owned(),
    }
}

/// 一个候选：**过渡期的老素材形状**——只给 offering 级旧字段，合同由它回退得来。
pub(super) fn draft(provider_model_id: &str) -> OfferingDraft {
    OfferingDraft {
        offering_id: None,
        provider_kind: Some("AIHubMix".to_owned()),
        adapter_key: Some("aihubmix-image-v1".to_owned()),
        provider_model_id: provider_model_id.to_owned(),
        base_url: Some("https://api.inferera.com".to_owned()),
        credential_env: Some("AIHUBMIX_API_KEY".to_owned()),
        routing_priority: None,
        weight: None,
        restrictions: serde_json::json!({}),
        carrier_schema: None,
        parameter_mapping: serde_json::json!({}),
        capability_schema: Some(schema("gpt-image-2.5-flare")),
        formula: Some("token_rates".to_owned()),
        price_plan: Some(price_plan()),
        cost_unit_price_microusd: None,
        cost_currency: None,
        reference_cost_microusd: None,
        consumer_rates_cny: None,
        consumer_formula: None,
        cost_basis: None,
        tier_prices: None,
        floor_amounts: None,
    }
}

/// 一个候选：**新形状**——承载面用新名字声明，合同留给顶层。
pub(super) fn carrier_draft(provider_model_id: &str, carrier: Value) -> OfferingDraft {
    OfferingDraft {
        offering_id: None,
        carrier_schema: Some(carrier),
        capability_schema: None,
        ..draft(provider_model_id)
    }
}

/// 造一个用于兼容性校验的候选：承载面只声明给定的字段。
pub(super) fn offering_with(schema_properties: Value, restrictions: Value) -> NormalizedOffering {
    NormalizedOffering {
        offering_id: None,
        carrier_schema: serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["model", "prompt"],
            "properties": schema_properties
        }),
        parameter_mapping: serde_json::json!({}),
        restrictions,
        provider_kind: "AIHubMix".to_owned(),
        adapter_key: "aihubmix-image-v1".to_owned(),
        provider_model_id: "m".to_owned(),
        base_url: "https://api.inferera.com".to_owned(),
        credential_env: "AIHUBMIX_API_KEY".to_owned(),
        rates: Some(PriceRates {
            currency: "USD".to_owned(),
            text_input_microusd_per_million: 5_000_000,
            image_input_microusd_per_million: 8_000_000,
            text_output_microusd_per_million: 10_000_000,
            image_output_microusd_per_million: 30_000_000,
        }),
        formula: PricingFormula::TokenRates,
        price_source_url: Some("https://example.invalid/price".to_owned()),
        cost_unit_price_microusd: None,
        cost_currency: Some("USD".to_owned()),
        consumer_rates_cny: None,
        consumer_formula: PricingFormula::TokenRates,
        routing_priority: 0,
        weight: 1,
        pricing: None,
    }
}

/// 一份只声明给定顶层字段的合同/承载面。
pub(super) fn surface(properties: Value) -> Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": properties
    })
}

/// 一个"能写这几个字段名上线文"的 Driver。
pub(super) fn descriptor() -> AdapterDescriptor {
    AdapterDescriptor {
        key: "aihubmix-image-v1",
        supported_top_level_parameters: &["model", "prompt", "image", "mask", "quality"],
        supported_extra_parameters: &[],
        supported_branches: &[
            ImageBranch::PromptOnly,
            ImageBranch::ImageConditioned,
            ImageBranch::Masked,
        ],
        max_reference_images: 1,
    }
}

/// 一条**带定价**的候选：参考成本、对客费率向量、成本来源与保底表都给齐。
///
/// 成本币种故意不给：它缺省取该供给 Price Plan 的币种（旧形状仍然这样），这里顺带钉住
/// "两条路取到的是同一个答案"。
pub(super) fn priced_draft(provider_model_id: &str) -> OfferingDraft {
    OfferingDraft {
        offering_id: None,
        reference_cost_microusd: Some(11_354),
        cost_currency: None,
        consumer_rates_cny: Some(ConsumerRatesCny {
            text_input_micros_per_million: 7_000_000,
            image_input_micros_per_million: 9_000_000,
            text_output_micros_per_million: 11_000_000,
            image_output_micros_per_million: 40_000_000,
        }),
        cost_basis: Some("declared".to_owned()),
        tier_prices: Some(serde_json::json!({"2K": 250_000})),
        floor_amounts: Some(serde_json::json!({
            "amounts": {"1K": 160_000, "2K": 250_000, "4K": 300_000},
            "cap_microusd": 300_000,
        })),
        ..draft(provider_model_id)
    }
}

/// 一条最小的对客请求（文生图）；参考图与遮罩由各用例自己加。
pub(super) fn image_request(parameters: Value) -> CreateImageGenerationRequest {
    CreateImageGenerationRequest {
        account_id: AccountId::new(),
        model: "gpt-image-2".to_owned(),
        native_parameters: parameters,
        reference_images: Vec::new(),
        mask: None,
        idempotency_key: "request-0001".to_owned(),
    }
}

/// 请求按合同校验之后留下的参数面（合同外的字段已丢弃）。测试里用它把"按合同校验"与
/// "逐候选承载校验"分开看。
pub(super) fn contract_face(
    request: &CreateImageGenerationRequest,
    offering: &PublishedOffering,
) -> Map<String, Value> {
    contract_parameter_face(request, &offering.capability_schema)
        .expect("the fixture request satisfies the contract")
}
