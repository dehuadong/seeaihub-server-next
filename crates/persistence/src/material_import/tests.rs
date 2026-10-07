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
        "consumer_reference_rates": material.consumer_reference_rates.as_ref().map(|rates| json!({
            "currency": rates.currency,
            "text_input_microusd_per_million": rates.text_input_microusd_per_million,
            "image_input_microusd_per_million": rates.image_input_microusd_per_million,
            "text_output_microusd_per_million": rates.text_output_microusd_per_million,
            "image_output_microusd_per_million": rates.image_output_microusd_per_million,
        })),
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
        // 成本币种是渠道事实：没有 Price Plan 的供给也要说得清上游给的钱是哪个币种，
        // 否则引用式发布填不出成本币种（工作项 #81）。
        assert_eq!(offering.cost_currency.as_deref(), Some("USD"));
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
    // 对客参考价目是**模型级**的一份（顶层）：与成本形态无关，一个模型一份，同模型的每条候选共用
    // 它当初始价来源（设计 0007 §2）。
    let reference = flare
        .consumer_reference_rates
        .as_ref()
        .expect("the model declares the reference price list");
    assert_eq!(reference.currency, "USD");
    assert_eq!(reference.text_input_microusd_per_million, 5_000_000);
    assert_eq!(reference.image_output_microusd_per_million, 30_000_000);
    assert!(
        parse("gpt-image-2.5-sunburst.json")
            .consumer_reference_rates
            .is_some(),
        "the second model declares its own list"
    );

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
fn material_offering(formula: PricingFormula, unit: Option<u64>, plan: bool) -> MaterialOffering {
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
        cost_currency: Some("USD".to_owned()),
    }
}

/// 计价形态与它的参数必须配套：库层只钉得住"单价只属于按张 / 按次"，价目表那条是发布期的判据，
/// 导入期先拦一道，为的是错误里点得出是哪条候选。
#[test]
fn an_offering_whose_formula_contradicts_its_parameters_is_rejected() {
    let offering = |formula, unit, plan| material_offering(formula, unit, plan);
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

/// 对客参考价目是**模型级**的一份：任何成本形态的模型都可以声明它（它不挂在供给上），缺了也不影响；
/// 但币种必须说得清——运营界面按它展示那份初始价（设计 0007 §2）。
#[test]
fn the_consumer_reference_rates_are_checked_once_per_model() {
    let rates = |currency: &str| ConsumerReferenceRates {
        currency: currency.to_owned(),
        text_input_microusd_per_million: 5_000_000,
        image_input_microusd_per_million: 8_000_000,
        text_output_microusd_per_million: 10_000_000,
        image_output_microusd_per_million: 30_000_000,
    };
    let mut material = parse("gpt-image-2.5-flare.json");
    let path = Path::new("material.json");

    material.consumer_reference_rates = Some(rates("USD"));
    check_consumer_reference_rates(path, &material).expect("a currency is enough");

    material.consumer_reference_rates = None;
    check_consumer_reference_rates(path, &material).expect("the list is optional");

    material.consumer_reference_rates = Some(rates("  "));
    let error = check_consumer_reference_rates(path, &material)
        .expect_err("a price list without a currency is rejected");
    let message = error.to_string();
    assert!(message.contains("material.json"), "{message}");
    assert!(message.contains("currency"), "{message}");
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

/// 组合约束点到的字段名必须是**同一层**自己声明的：点到别的名字时那条约束落不到任何值上（合同是
/// 封闭对象，承载面的线上字段也只由声明面产生），模型使用文档还会教调用方填一个会被丢弃的字段
/// （AIHubMix 改名那次在顶层合同上留下过 `images`，见工作项 #79）。判据只有一份
/// （`undeclared_clause_name`），这里按真素材跑一遍，承载面那一条同时走导入期的 `check_offering_shape`。
#[test]
fn the_materials_only_constrain_fields_they_declare() {
    for name in ["gpt-image-2.5-flare.json", "gpt-image-2.5-sunburst.json"] {
        let material = parse(name);
        let mut schemas = vec![("合同", &material.capability_schema)];
        for offering in &material.offerings {
            schemas.push((offering.provider_kind.as_str(), &offering.carrier_schema));
        }
        for (label, schema) in schemas {
            assert_eq!(
                seeai_application::model_document::undeclared_clause_name(schema),
                None,
                "{name}: {label} 的组合约束点到没声明的字段"
            );
        }
        for (index, offering) in material.offerings.iter().enumerate() {
            check_offering_shape(&format!("{name}: offerings[{index}]"), offering)
                .expect("the two materials' carriers are well-formed");
        }
        // 遮罩那条约束点的必须是合同声明的 `image`（对客字段名），不是承载面的线上名 `images`。
        assert_eq!(
            material.capability_schema["allOf"][0]["then"]["required"],
            json!(["image"]),
            "{name}"
        );
        // 对客文档里那句结构描述由合同渲染：说的是 `image`，不再教调用方填 `images`。
        let documentation =
            resolve_documentation(&public_docs(), &material_dir().join(name), &material)
                .expect("the documentation resolves");
        let body = seeai_application::model_document::render_model_document(
            &material.capability_schema,
            &material.native_model_id,
            &material.vendor_id,
            "image",
            &material.native_revision,
            "http://api.test",
            &documentation,
        )
        .expect("the contract renders");
        assert!(body.contains("提供 `image`"), "{name}: {body}");
        assert!(!body.contains("提供 `images`"), "{name}: {body}");
    }
}

/// 承载面的组合约束点到没声明的线上名时，导入期就拒并点名文件／候选与那处指针。
#[test]
fn a_carrier_clause_naming_an_undeclared_field_is_rejected_at_import() {
    let mut offering = material_offering(PricingFormula::UpstreamDeclared, None, false);
    offering.carrier_schema = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": {
            "model": {"const": "gpt-image-2.5-flare"},
            "prompt": {"type": "string"},
            "images": {"type": "array", "items": {"type": "string"}},
            "mask": {"type": "string"}
        },
        "allOf": [{"if": {"required": ["mask"]}, "then": {"required": ["image"]}}]
    });
    let error = check_offering_shape("material.json: offerings[0]", &offering)
        .expect_err("a carrier clause naming an undeclared field is rejected");
    let message = error.to_string();
    assert!(message.contains("material.json: offerings[0]"), "{message}");
    assert!(message.contains("image"), "{message}");
    assert!(message.contains("/allOf/0/then/required/0"), "{message}");

    // 换成承载面自己声明的线上名 `images` 就通过。
    offering.carrier_schema["allOf"][0]["then"]["required"] = json!(["images"]);
    check_offering_shape("material.json: offerings[0]", &offering)
        .expect("the wire name the carrier declares is accepted");
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
