use super::*;
use std::path::PathBuf;

/// 公开文档目录：单元测试直接指到仓库里那份，不依赖进程工作目录。
fn public_docs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../public-docs")
}

#[test]
fn the_bootstrap_documentation_resolves_into_a_validated_material() {
    let flare = parse("gpt-image-2.5-flare.json");
    let material = resolve_documentation(
        &public_docs(),
        &material_dir().join("gpt-image-2.5-flare.json"),
        &flare,
    )
    .expect("the bootstrap documentation resolves");
    let narrative = material["narrative"]
        .as_str()
        .expect("the narrative content");
    assert!(narrative.contains("{{parameter_table}}"), "{narrative}");
    assert!(material["fields"]["/properties/prompt"].is_string());
    assert!(material["fields"]["/allOf/0"].is_string());
}

#[test]
fn a_material_without_documentation_is_rejected() {
    let mut flare = parse("gpt-image-2.5-flare.json");
    flare.documentation = None;
    let error = resolve_documentation(
        &public_docs(),
        &material_dir().join("gpt-image-2.5-flare.json"),
        &flare,
    )
    .expect_err("missing documentation is rejected");
    assert!(error.to_string().contains("documentation"), "{error}");
}

#[test]
fn a_documentation_path_outside_public_docs_is_rejected() {
    let mut flare = parse("gpt-image-2.5-flare.json");
    flare.documentation = Some(json!({"narrative_path": "../../etc/passwd", "fields": {}}));
    let error = resolve_documentation(
        &public_docs(),
        &material_dir().join("gpt-image-2.5-flare.json"),
        &flare,
    )
    .expect_err("an escaping path is rejected");
    assert!(error.to_string().contains("public-docs"), "{error}");
}

/// 素材目录就是仓库里那份真实素材：解析这一层要对着**真输入**验，不能自己编一份形状好看的。
fn material_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/bootstrap")
}

fn parse(name: &str) -> Material {
    let path = material_dir().join(name);
    let text = std::fs::read_to_string(&path).expect("a material file");
    serde_json::from_str(&text).expect("the material parses")
}

/// 导入会写进四张表的那些字段；运营的定价不在这里，比较它就是比较"导入看见的东西"。
fn imported_projection(material: &Material) -> Value {
    json!({
        "vendor_id": material.vendor_id,
        "native_model_id": material.native_model_id,
        "native_revision": material.native_revision,
        "capability_schema": material.capability_schema,
        "offerings": material
            .offerings
            .iter()
            .map(|offering| json!({
                "provider_kind": offering.provider_kind,
                "adapter_key": offering.adapter_key,
                "provider_model_id": offering.provider_model_id,
                "base_url": offering.base_url,
                "credential_env": offering.credential_env,
                "restrictions": offering.restrictions,
                "carrier_schema": offering.carrier_schema,
                "parameter_mapping": offering.parameter_mapping,
                "formula": offering.formula.as_str(),
                "cost_unit_price_microusd": offering.cost_unit_price_microusd,
                "price_plan": offering.price_plan.as_ref().map(|plan| json!({
                    "currency": plan.currency,
                    "text_input_microusd_per_million": plan.text_input_microusd_per_million,
                    "image_input_microusd_per_million": plan.image_input_microusd_per_million,
                    "text_output_microusd_per_million": plan.text_output_microusd_per_million,
                    "image_output_microusd_per_million": plan.image_output_microusd_per_million,
                    "source_url": plan.source_url,
                })),
                "consumer_reference_rates": offering.consumer_reference_rates.as_ref().map(|rates| json!({
                    "currency": rates.currency,
                    "text_input_microusd_per_million": rates.text_input_microusd_per_million,
                    "image_input_microusd_per_million": rates.image_input_microusd_per_million,
                    "text_output_microusd_per_million": rates.text_output_microusd_per_million,
                    "image_output_microusd_per_million": rates.image_output_microusd_per_million,
                    "source_url": rates.source_url,
                })),
            }))
            .collect::<Vec<_>>(),
    })
}

#[test]
fn the_bootstrap_materials_parse_into_their_offerings() {
    let flare = parse("gpt-image-2.5-flare.json");
    assert_eq!(flare.vendor_id, "OpenAI");
    assert_eq!(flare.native_model_id, "gpt-image-2.5-flare");
    assert_eq!(flare.model_type.as_deref(), Some("image"));
    assert_eq!(flare.offerings.len(), 2);
    // 两家渠道都按上游声明的金额计价：AIHubMix 只回金额、不给 token 分项；APIMart 同样由上游给金额。
    for offering in &flare.offerings {
        assert_eq!(offering.formula, PricingFormula::UpstreamDeclared);
        assert!(
            offering.price_plan.is_none(),
            "upstream_declared has no price plan"
        );
    }
    // 参考图的限制必须与承载面的形态一致，这条是发布期的判据；导入照抄，不做二次推导。
    assert_eq!(flare.offerings[0].restrictions["max_reference_images"], 16);
    // AIHubMix 走 `/ai/v1`：参考图是字符串数组（`images`）、模型专属字段落 `extra` 容器；
    // 对客的 `image` 由映射改名成线上名 `images`。
    assert_eq!(
        flare.offerings[0].carrier_schema["properties"]["images"]["maxItems"],
        16
    );
    assert!(
        flare.offerings[0].carrier_schema["properties"]["extra"].is_object(),
        "the model-specific fields live in the extra container"
    );
    assert_eq!(
        flare.offerings[0].parameter_mapping["rename"]["image"],
        "images"
    );
    // APIMart 的承载面用它自己的原生名，同样靠映射接过去。
    assert_eq!(
        flare.offerings[1].parameter_mapping["rename"]["image"],
        "image_urls"
    );
    // 对客参考价目与成本形态无关：AIHubMix 那条（`upstream_declared`）声明了它，APIMart 那条没声明
    // ——同模型下按 token 四档卖的候选，初始价取前者（设计 0007 §2）。
    let reference = flare.offerings[0]
        .consumer_reference_rates
        .as_ref()
        .expect("AIHubMix declares the reference price list");
    assert_eq!(reference.currency, "USD");
    assert_eq!(reference.text_input_microusd_per_million, 5_000_000);
    assert_eq!(reference.image_output_microusd_per_million, 30_000_000);
    assert!(flare.offerings[1].consumer_reference_rates.is_none());

    let sunburst = parse("gpt-image-2.5-sunburst.json");
    assert_eq!(sunburst.native_model_id, "gpt-image-2.5-sunburst");
    assert_eq!(sunburst.offerings.len(), 2);
}

/// 运营的定价不进这四张表：改那些字段不该改变导入看见的任何东西。
///
/// 它们是随发布物按候选给的价（`publication.runtime_revisions` 的定价列），素材里那份只是写素材时
/// 顺手留下的测试值。拿它当导入输入，等于让一份工程素材顶掉运营的价。
#[test]
fn the_operators_pricing_is_invisible_to_the_import() {
    let path = material_dir().join("gpt-image-2.5-flare.json");
    let text = std::fs::read_to_string(&path).expect("a material file");
    let original: Material = serde_json::from_str(&text).expect("the material parses");

    let mut changed: Value = serde_json::from_str(&text).expect("the material is json");
    changed["markup_bps"] = json!(9999);
    changed["offerings"][0]["consumer_rates_cny"] = json!({ "text_input_micros_per_million": 1 });
    changed["offerings"][0]["reference_cost_microusd"] = json!(1);
    changed["offerings"][0]["cost_basis"] = json!("declared");
    changed["offerings"][0]["floor_amounts"] = json!({ "cap_microusd": 1 });
    let edited: Material =
        serde_json::from_value(changed).expect("the edited material still parses");

    assert_eq!(imported_projection(&original), imported_projection(&edited));
}

/// 一条素材供给：只给参与形状校验的那几个字段，其余取最小可用值。
fn material_offering(
    formula: PricingFormula,
    unit: Option<u64>,
    plan: bool,
    reference: Option<ConsumerReferenceRates>,
) -> MaterialOffering {
    MaterialOffering {
        provider_kind: "AIHubMix".to_owned(),
        adapter_key: "aihubmix-image-v1".to_owned(),
        provider_model_id: "gpt-image-2.5-flare".to_owned(),
        base_url: "https://api.inferera.com".to_owned(),
        credential_env: "AIHUBMIX_API_KEY".to_owned(),
        restrictions: json!({}),
        carrier_schema: json!({}),
        parameter_mapping: json!({}),
        formula,
        cost_unit_price_microusd: unit,
        price_plan: plan.then(|| MaterialPricePlan {
            currency: "USD".to_owned(),
            text_input_microusd_per_million: 1,
            image_input_microusd_per_million: 1,
            text_output_microusd_per_million: 1,
            image_output_microusd_per_million: 1,
            source_url: "https://example.invalid/rates".to_owned(),
        }),
        consumer_reference_rates: reference,
    }
}

/// 计价形态与它的参数必须配套：库层只钉得住"单价只属于按张 / 按次"，价目表那条是发布期的判据，
/// 导入期先拦一道，为的是错误里点得出是哪条候选。
#[test]
fn an_offering_whose_formula_contradicts_its_parameters_is_rejected() {
    let offering = |formula, unit, plan| material_offering(formula, unit, plan, None);
    let token_rates = || offering(PricingFormula::TokenRates, None, true);
    let per_image = || offering(PricingFormula::PerImage, Some(7), false);

    check_offering_shape("material.json: offerings[0]", &token_rates())
        .expect("token_rates with its four-tier rates is the well-formed shape");
    check_offering_shape("material.json: offerings[0]", &per_image())
        .expect("per_image with its unit price is the well-formed shape");

    for wrong in [
        offering(PricingFormula::TokenRates, None, false),
        offering(PricingFormula::TokenRates, Some(7), true),
        offering(PricingFormula::PerImage, None, false),
        offering(PricingFormula::PerImage, Some(7), true),
        offering(PricingFormula::UpstreamDeclared, None, true),
        offering(PricingFormula::UpstreamDeclared, Some(7), false),
    ] {
        let error = check_offering_shape("material.json: offerings[0]", &wrong)
            .expect_err("a formula that contradicts its parameters is rejected");
        assert!(
            error.to_string().contains("offerings[0]"),
            "the error names the candidate: {error}"
        );
    }
}

/// 对客参考价目**不是成本参数**：任何成本形态的供给都可以声明它，缺了也不影响；但币种必须说得清
/// ——运营界面按它展示那份初始价（设计 0007 §2）。
#[test]
fn the_consumer_reference_rates_are_independent_of_the_cost_form() {
    let rates = |currency: &str| ConsumerReferenceRates {
        currency: currency.to_owned(),
        text_input_microusd_per_million: 5_000_000,
        image_input_microusd_per_million: 8_000_000,
        text_output_microusd_per_million: 10_000_000,
        image_output_microusd_per_million: 30_000_000,
        source_url: "https://example.invalid/list".to_owned(),
    };
    let offering = |formula, reference| material_offering(formula, None, false, reference);

    check_offering_shape(
        "material.json: offerings[0]",
        &offering(PricingFormula::UpstreamDeclared, Some(rates("USD"))),
    )
    .expect("a declared-cost supply may still declare the reference price list");
    check_offering_shape(
        "material.json: offerings[0]",
        &offering(PricingFormula::UpstreamDeclared, None),
    )
    .expect("the reference price list is optional");

    let error = check_offering_shape(
        "material.json: offerings[0]",
        &offering(PricingFormula::UpstreamDeclared, Some(rates("  "))),
    )
    .expect_err("a reference price list without a currency is rejected");
    assert!(error.to_string().contains("offerings[0]"), "{error}");
    assert!(error.to_string().contains("currency"), "{error}");
}

/// 类型必须声明、且落在三种之内；报错要点名素材文件与型号（Spec 0006 §4.4、设计 0020 §2）。
#[test]
fn a_material_must_declare_a_known_type() {
    let mut material = parse("gpt-image-2.5-flare.json");
    material.model_type = None;
    let error = material_model_type(Path::new("material.json"), &material)
        .expect_err("a material without type is rejected");
    assert!(error.to_string().contains("material.json"), "{error}");
    assert!(error.to_string().contains("gpt-image-2.5-flare"), "{error}");

    material.model_type = Some("audio".to_owned());
    let error = material_model_type(Path::new("material.json"), &material)
        .expect_err("a material with an unknown type is rejected");
    assert!(error.to_string().contains("audio"), "{error}");
    assert!(error.to_string().contains("gpt-image-2.5-flare"), "{error}");
}

/// 目录不存在或里面没有素材时，导入什么都没读——调用方据此静默跳过，开发库与测试库因此不会被
/// 一次启动失败挡住。
#[test]
fn a_missing_or_empty_directory_yields_no_materials() {
    let missing = material_dir().join("no-such-directory");
    assert!(
        read_materials(&missing)
            .expect("a missing directory is not an error")
            .is_empty()
    );

    let empty = std::env::temp_dir().join(format!("seeai-material-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&empty).expect("create an empty material directory");
    let materials = read_materials(&empty).expect("an empty directory is not an error");
    assert!(materials.is_empty());
    std::fs::remove_dir_all(&empty).expect("clean the empty material directory");
}

/// **没设就导默认目录**：运营上架模型不该依赖谁记着去开一个环境变量。
#[test]
fn an_unset_material_dir_falls_back_to_the_shipped_materials() {
    assert_eq!(
        material_dir_from(None),
        Some(PathBuf::from(DEFAULT_SUPPLY_MATERIAL_DIR))
    );
    assert_eq!(DEFAULT_SUPPLY_MATERIAL_DIR, "config/bootstrap");
}

/// **设成空白＝显式不导入**：测试库与开发库要一份干净的供给清单时用它。
#[test]
fn a_blank_material_dir_imports_nothing() {
    assert_eq!(material_dir_from(Some("")), None);
    assert_eq!(material_dir_from(Some("   ")), None);
}

/// 给了路径就用它（去掉两端空白）。
#[test]
fn an_explicit_material_dir_is_used_as_given() {
    assert_eq!(
        material_dir_from(Some("  /tmp/materials  ")),
        Some(PathBuf::from("/tmp/materials"))
    );
}

/// 顶层合同必须声明 `background=transparent` 与 `output_format` 的组合约束：取值由上游判，
/// 但客户端要能从合同知道这两个参数怎么一起用（用户 2026-10-05 的决定）。
#[test]
fn the_contract_declares_the_transparent_output_format_constraint() {
    for name in ["gpt-image-2.5-flare.json", "gpt-image-2.5-sunburst.json"] {
        let material = parse(name);
        let all_of = material.capability_schema["allOf"]
            .as_array()
            .unwrap_or_else(|| panic!("{name}: the contract declares allOf"));
        let declares = all_of.iter().any(|clause| {
            clause["if"]["properties"]["background"]["const"] == json!("transparent")
                && clause["if"]["required"] == json!(["background"])
                && clause["then"]["properties"]["output_format"]["enum"] == json!(["png", "webp"])
        });
        assert!(
            declares,
            "{name}: the contract must declare transparent => png/webp"
        );
        // 模型级组合约束归合同，承载面不重复它——按 `allOf` 整段文本判，换一种写法也漏不掉。
        for offering in &material.offerings {
            let carrier_all_of = offering.carrier_schema["allOf"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let rendered = Value::Array(carrier_all_of).to_string();
            assert!(
                !rendered.contains("output_format"),
                "{name}: {} must not repeat the model-level constraint: {rendered}",
                offering.provider_kind
            );
        }
    }
}
