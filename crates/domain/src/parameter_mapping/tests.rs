use super::*;

fn schema(properties: Value) -> Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": properties
    })
}

#[test]
fn a_declared_field_counts_from_properties_or_required() {
    let with_required_only = serde_json::json!({
        "type": "object",
        "required": ["model"],
        "properties": {"prompt": {"type": "string"}}
    });
    assert!(declares_parameter(&with_required_only, "model"));
    assert!(declares_parameter(&with_required_only, "prompt"));
    assert!(!declares_parameter(&with_required_only, "quality"));
    // 清单与判据是同一份：`properties` 的键在前，只写进 `required` 的名字在后。
    assert_eq!(
        declared_field_names(&with_required_only),
        vec!["prompt", "model"]
    );
    // 连 properties 都没有：一个名字都不算声明。
    assert!(!declares_parameter(
        &serde_json::json!({"type": "object"}),
        "model"
    ));
    assert!(declared_field_names(&serde_json::json!({"type": "object"})).is_empty());
}

#[test]
fn only_values_that_carry_something_count_as_used() {
    for empty in [
        serde_json::json!(null),
        serde_json::json!(""),
        serde_json::json!([]),
    ] {
        assert!(!is_used_parameter_value(&empty), "`{empty}` 是没给");
    }
    for used in [
        serde_json::json!("low"),
        serde_json::json!(0),
        serde_json::json!(false),
        serde_json::json!([1]),
        // 空对象算给了：平台不猜它的内部结构。
        serde_json::json!({}),
    ] {
        assert!(is_used_parameter_value(&used), "`{used}` 是给了");
    }
}

#[test]
fn defaults_fill_only_the_fields_the_caller_left_empty() {
    let contract = schema(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "watermark": {"type": "boolean"}
    }));
    let carrier = contract.clone();
    let mapping = serde_json::json!({"defaults": {"watermark": false}});
    let renames = declared_renames(&mapping).expect("no rename declared");
    let defaults = declared_defaults(&mapping);

    // 调用方没给：注入默认值。
    let mut parameters = Map::from_iter([("prompt".to_owned(), serde_json::json!("x"))]);
    apply_parameter_defaults(
        &contract,
        &carrier,
        renames.as_ref(),
        defaults,
        &mut parameters,
    );
    assert_eq!(parameters.get("watermark"), Some(&serde_json::json!(false)));

    // 调用方给了：用调用方的值，一个字都不改。
    let mut parameters = Map::from_iter([
        ("prompt".to_owned(), serde_json::json!("x")),
        ("watermark".to_owned(), serde_json::json!(true)),
    ]);
    apply_parameter_defaults(
        &contract,
        &carrier,
        renames.as_ref(),
        defaults,
        &mut parameters,
    );
    assert_eq!(parameters.get("watermark"), Some(&serde_json::json!(true)));

    // 调用方写了个空位：那也是"没给"，默认值照旧生效。
    for empty in [serde_json::json!(null), serde_json::json!("")] {
        let mut parameters = Map::from_iter([
            ("prompt".to_owned(), serde_json::json!("x")),
            ("watermark".to_owned(), empty.clone()),
        ]);
        apply_parameter_defaults(
            &contract,
            &carrier,
            renames.as_ref(),
            defaults,
            &mut parameters,
        );
        assert_eq!(
            parameters.get("watermark"),
            Some(&serde_json::json!(false)),
            "`{empty}` 是空位，默认值该生效"
        );
    }
}

/// 合同与承载面**都**声明了才注入：只有一边声明时，注入出去的东西平台自己都说不清。
#[test]
fn defaults_are_not_injected_outside_the_contract_and_the_carrier() {
    let contract = schema(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "watermark": {"type": "boolean"}
    }));
    // 承载面收窄：它承载不了 `watermark`。
    let carrier = schema(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"}
    }));
    let mapping = serde_json::json!({"defaults": {"watermark": false, "seed": 7}});
    let renames = declared_renames(&mapping).expect("no rename declared");
    let defaults = declared_defaults(&mapping);
    let mut parameters = Map::from_iter([("prompt".to_owned(), serde_json::json!("x"))]);
    apply_parameter_defaults(
        &contract,
        &carrier,
        renames.as_ref(),
        defaults,
        &mut parameters,
    );
    assert!(
        parameters.get("watermark").is_none(),
        "承载面没声明就不注入：{parameters:?}"
    );
    assert!(
        parameters.get("seed").is_none(),
        "合同与承载面都没声明就更不该注入：{parameters:?}"
    );

    // 合同没声明（承载面也不可能声明，承载面是合同的子集）：同样不注入。
    let mut parameters = Map::from_iter([("prompt".to_owned(), serde_json::json!("x"))]);
    apply_parameter_defaults(
        &carrier,
        &carrier,
        renames.as_ref(),
        defaults,
        &mut parameters,
    );
    assert!(parameters.get("watermark").is_none());
}

/// 默认值的键经**改名**落到承载面声明的名字上时照旧注入：那条供给承载得了它，只是线上叫
/// 另一个名字。注入仍然发生在合同名字上，改名在注入之后才做。
#[test]
fn a_default_for_a_field_the_offering_carries_under_another_name_is_injected() {
    let contract = schema(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "quality": {"type": "string"}
    }));
    // 承载面写的是**线上字段名**：它线上叫 `xquality`。
    let carrier = schema(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "xquality": {"type": "string"}
    }));
    let mapping =
        serde_json::json!({"defaults": {"quality": "low"}, "rename": {"quality": "xquality"}});
    let renames = declared_renames(&mapping).expect("a rename is declared");
    let defaults = declared_defaults(&mapping);
    let mut parameters = Map::from_iter([("prompt".to_owned(), serde_json::json!("x"))]);
    apply_parameter_defaults(
        &contract,
        &carrier,
        renames.as_ref(),
        defaults,
        &mut parameters,
    );
    assert_eq!(
        parameters.get("quality"),
        Some(&serde_json::json!("low")),
        "注入按合同名字落：{parameters:?}"
    );
    apply_parameter_renames(&carrier, renames.as_ref(), &mut parameters).expect("renamed");
    assert_eq!(parameters.get("xquality"), Some(&serde_json::json!("low")));
    assert!(parameters.get("quality").is_none());
}

#[test]
fn a_mapping_without_defaults_changes_nothing() {
    let contract = schema(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"}
    }));
    let mut parameters = Map::from_iter([("prompt".to_owned(), serde_json::json!("x"))]);
    let before = parameters.clone();
    for mapping in [
        serde_json::json!({}),
        // 声明得不成形状（不是对象）：按"没有声明"处理，不报错。
        serde_json::json!({"defaults": ["watermark"]}),
        serde_json::json!({"renames": {"watermark": "wm"}}),
    ] {
        let renames = declared_renames(&mapping).expect("no rename declared");
        let defaults = declared_defaults(&mapping);
        apply_parameter_defaults(
            &contract,
            &contract,
            renames.as_ref(),
            defaults,
            &mut parameters,
        );
        assert_eq!(parameters, before, "映射 {mapping} 不该改动参数面");
    }
    assert!(declared_defaults(&serde_json::json!({})).is_none());
}

// ── 改名与取值映射 ──────────────────────────────────────────────────────

/// 承载判据：承载面声明了这个名字就用它；没声明时看改名表能不能把它落到一个声明的名字上。
#[test]
fn a_rename_bridges_the_contract_name_to_the_name_written_on_the_wire() {
    let carrier = schema(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "resolution": {"type": "string"}
    }));
    let renames = declared_renames(&serde_json::json!({
        "rename": {"size": "resolution", "prompt": "prompt"}
    }))
    .expect("the declaration parses")
    .expect("a rename is declared");

    // 承载面直接声明的名字：原样用。
    assert_eq!(
        wire_parameter_name(&carrier, Some(&renames), "resolution"),
        Some("resolution".to_owned())
    );
    // 承载面没声明、但改名落到了一个声明的名字上：承载得了，线上用那个名字。
    assert_eq!(
        wire_parameter_name(&carrier, Some(&renames), "size"),
        Some("resolution".to_owned())
    );
    assert!(carries_parameter(&carrier, Some(&renames), "size"));
    // 两边都没有：承载不了。
    assert_eq!(
        wire_parameter_name(&carrier, Some(&renames), "quality"),
        None
    );
    assert!(!carries_parameter(&carrier, Some(&renames), "quality"));
    // 没有改名表时只有承载面声明过的名字算数。
    assert_eq!(wire_parameter_name(&carrier, None, "size"), None);
    // 改名落到一个承载面都没声明的名字上：等于发不出去，不算承载。
    let dangling = declared_renames(&serde_json::json!({"rename": {"size": "aspect_ratio"}}))
        .expect("the declaration parses")
        .expect("a rename is declared");
    assert_eq!(wire_parameter_name(&carrier, Some(&dangling), "size"), None);
}

/// 改名只改**承载面没声明**的名字，取值一字不动；两个合同字段落到同一个线上名字时明确失败。
#[test]
fn renaming_moves_only_the_names_the_carrier_does_not_declare() {
    let carrier = schema(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "resolution": {"type": "string"}
    }));
    let renames = declared_renames(&serde_json::json!({
        "rename": {"size": "resolution", "quality": "resolution"}
    }))
    .expect("the declaration parses")
    .expect("a rename is declared");

    let mut parameters = Map::from_iter([
        ("model".to_owned(), serde_json::json!("m")),
        ("prompt".to_owned(), serde_json::json!("x")),
        ("size".to_owned(), serde_json::json!("2:3")),
    ]);
    apply_parameter_renames(&carrier, Some(&renames), &mut parameters).expect("renamed");
    assert_eq!(
        parameters.get("resolution"),
        Some(&serde_json::json!("2:3"))
    );
    assert!(!parameters.contains_key("size"));
    assert_eq!(parameters.get("prompt"), Some(&serde_json::json!("x")));

    // 承载面自己声明了 `resolution`：改名表对那个名字不生效，它原样留着。
    let mut parameters = Map::from_iter([
        ("resolution".to_owned(), serde_json::json!("1k")),
        ("prompt".to_owned(), serde_json::json!("x")),
    ]);
    apply_parameter_renames(&carrier, Some(&renames), &mut parameters).expect("renamed");
    assert_eq!(
        parameters.get("resolution"),
        Some(&serde_json::json!("1k")),
        "承载面声明的名字优先：{parameters:?}"
    );

    // 两个合同字段落到同一个线上名字：平台不猜留哪一个，明确失败。
    let mut parameters = Map::from_iter([
        ("size".to_owned(), serde_json::json!("2:3")),
        ("quality".to_owned(), serde_json::json!("high")),
    ]);
    let error = apply_parameter_renames(&carrier, Some(&renames), &mut parameters)
        .expect_err("two contract fields cannot share one wire name");
    assert!(error.contains("resolution"), "{error}");

    // 没有改名表：一个名字都不动。
    let mut untouched = Map::from_iter([("size".to_owned(), serde_json::json!("2:3"))]);
    let before = untouched.clone();
    apply_parameter_renames(&carrier, None, &mut untouched).expect("nothing to rename");
    assert_eq!(untouched, before);
}

#[test]
fn a_rename_declaration_that_is_not_shaped_like_one_is_rejected() {
    for (mapping, needle) in [
        (serde_json::json!({"rename": "size"}), "must be an object"),
        (
            serde_json::json!({"rename": {"size": 7}}),
            "must be a non-empty field name",
        ),
        (
            serde_json::json!({"rename": {"size": ""}}),
            "must be a non-empty field name",
        ),
    ] {
        let error = declared_renames(&mapping)
            .err()
            .unwrap_or_else(|| panic!("{mapping} 不是一份改名声明"));
        assert!(error.contains(needle), "{mapping} → {error}");
    }
    // 没有声明（键缺失或显式写 null）＝ 这条供给不改名。
    assert_eq!(
        declared_renames(&serde_json::json!({"defaults": {"watermark": false}})),
        Ok(None)
    );
    assert_eq!(
        declared_renames(&serde_json::json!({"rename": null})),
        Ok(None)
    );
    // 改名成同一个名字是合法的空操作（它不改变任何东西）。
    assert_eq!(
        declared_renames(&serde_json::json!({"rename": {"size": "size"}})),
        Ok(Some(BTreeMap::from_iter([(
            "size".to_owned(),
            "size".to_owned()
        )])))
    );
}

/// 取值映射：合同取值换成线上取值；表里没有的取值**明确失败**，不猜也不透传原值。
#[test]
fn an_enum_map_replaces_the_value_and_a_missing_one_is_an_error() {
    let carrier = schema(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "xquality": {"type": "string"}
    }));
    let mapping = serde_json::json!({
        "rename": {"quality": "xquality"},
        "enum_map": {"quality": {"high": "xhigh", "low": "low"}}
    });
    let renames = declared_renames(&mapping).expect("a rename is declared");
    let enum_maps = declared_enum_maps(&mapping)
        .expect("the declaration parses")
        .expect("an enum map is declared");

    // 合同名字上是 `high`，线上那个名字上是映射后的 `xhigh`。
    let mut parameters = Map::from_iter([("xquality".to_owned(), serde_json::json!("high"))]);
    apply_enum_maps(&carrier, renames.as_ref(), &enum_maps, &mut parameters)
        .expect("high is in the table");
    assert_eq!(
        parameters.get("xquality"),
        Some(&serde_json::json!("xhigh")),
        "{parameters:?}"
    );

    // 表里没有这个取值：明确失败，参数面不许留下一个半映射的值。
    for value in [serde_json::json!("medium"), serde_json::json!(7)] {
        let mut parameters = Map::from_iter([("xquality".to_owned(), value.clone())]);
        let error = apply_enum_maps(&carrier, renames.as_ref(), &enum_maps, &mut parameters)
            .expect_err("a value outside the table must fail");
        assert!(error.contains("quality"), "{value} → {error}");
        assert_eq!(parameters.get("xquality"), Some(&value));
    }

    // 字段没给（不在参数面上）或给了个空位：无事可做。
    let mut parameters = Map::from_iter([("prompt".to_owned(), serde_json::json!("x"))]);
    let before = parameters.clone();
    apply_enum_maps(&carrier, renames.as_ref(), &enum_maps, &mut parameters)
        .expect("nothing to map");
    assert_eq!(parameters, before);
    let mut parameters = Map::from_iter([("xquality".to_owned(), serde_json::json!(""))]);
    let before = parameters.clone();
    apply_enum_maps(&carrier, renames.as_ref(), &enum_maps, &mut parameters)
        .expect("an empty value is not a value");
    assert_eq!(parameters, before);

    // 承载面承载不了的字段：这张表对它无事可做（请求用到它时早就判过不合格了）。
    let narrow = schema(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"}
    }));
    let mut parameters = Map::from_iter([("quality".to_owned(), serde_json::json!("high"))]);
    let before = parameters.clone();
    apply_enum_maps(&narrow, renames.as_ref(), &enum_maps, &mut parameters)
        .expect("the offering cannot carry quality at all");
    assert_eq!(parameters, before);
}

#[test]
fn an_enum_map_declaration_that_is_not_shaped_like_one_is_rejected() {
    for (mapping, needle) in [
        (
            serde_json::json!({"enum_map": "quality"}),
            "must be an object",
        ),
        (
            serde_json::json!({"enum_map": {"quality": ["high"]}}),
            "must be an object of value mappings",
        ),
    ] {
        let error = declared_enum_maps(&mapping)
            .err()
            .unwrap_or_else(|| panic!("{mapping} 不是一份取值映射声明"));
        assert!(error.contains(needle), "{mapping} → {error}");
    }
    assert_eq!(declared_enum_maps(&serde_json::json!({})), Ok(None));
    assert_eq!(
        declared_enum_maps(&serde_json::json!({"enum_map": null})),
        Ok(None)
    );
}

/// 尺寸换算：调用方给"比例 + 档位"，这条供给要像素——组装出来的参数面里是查表得到的像素，
/// 而那两个源字段不再原样上行。
#[test]
fn the_size_conversion_writes_the_target_field_and_consumes_its_sources() {
    let mapping = declared_size_mapping(&serde_json::json!({
        "size": {
            "source": ["size", "resolution"],
            "target": "size",
            "form": "pixels",
            "profile": {"2K": {"1:1": "2048x2048", "2:3": "1664x2496"}}
        }
    }))
    .expect("the declaration parses")
    .expect("a size declaration is present");
    assert_eq!(mapping.form, SizeForm::Pixels);
    assert!(mapping.consumes("resolution"));
    assert!(mapping.consumes("size"));
    assert!(!mapping.consumes("prompt"));

    // 合同面（调用方提交的字段）：比例 + 档位。
    let contract_face = Map::from_iter([
        ("model".to_owned(), serde_json::json!("m")),
        ("prompt".to_owned(), serde_json::json!("x")),
        ("size".to_owned(), serde_json::json!("2:3")),
        ("resolution".to_owned(), serde_json::json!("2K")),
    ]);
    // 承载面过滤后的参数面：`resolution` 没被承载面声明，所以本来就不在里面。
    let mut parameters = Map::from_iter([
        ("model".to_owned(), serde_json::json!("m")),
        ("prompt".to_owned(), serde_json::json!("x")),
        ("size".to_owned(), serde_json::json!("2:3")),
    ]);
    apply_size_mapping(&mapping, &contract_face, &mut parameters)
        .expect("2:3 at 2K is in the profile");
    assert_eq!(
        parameters.get("size"),
        Some(&serde_json::json!("1664x2496")),
        "线上那个字段是换算后的像素：{parameters:?}"
    );
    assert!(
        !parameters.contains_key("resolution"),
        "源字段被换算消耗，不再原样上行：{parameters:?}"
    );
    assert_eq!(parameters.get("prompt"), Some(&serde_json::json!("x")));

    // 调用方这次没用到尺寸：一个字段都不动。
    let untouched = Map::from_iter([("prompt".to_owned(), serde_json::json!("x"))]);
    let mut parameters = untouched.clone();
    apply_size_mapping(&mapping, &untouched, &mut parameters).expect("nothing to convert");
    assert_eq!(parameters, untouched);
    // 源字段在场但是空值：同样按"没给"处理。
    let blank = Map::from_iter([
        ("prompt".to_owned(), serde_json::json!("x")),
        ("size".to_owned(), serde_json::json!("")),
        ("resolution".to_owned(), serde_json::json!(null)),
    ]);
    let mut parameters = blank.clone();
    apply_size_mapping(&mapping, &blank, &mut parameters).expect("blank is not a size");
    assert_eq!(parameters, blank);
}

/// 档案里缺那一格：明确失败，绝不退回调用方给的原值、也不猜一个近似值。
#[test]
fn a_combination_the_profile_does_not_have_is_an_error() {
    let mapping = declared_size_mapping(&serde_json::json!({
        "size": {
            "source": ["size", "resolution"],
            "target": "size",
            "form": "pixels",
            "profile": {"2K": {"2:3": "1664x2496"}}
        }
    }))
    .expect("the declaration parses")
    .expect("a size declaration is present");
    let contract_face = Map::from_iter([
        ("size".to_owned(), serde_json::json!("2:3")),
        ("resolution".to_owned(), serde_json::json!("3K")),
    ]);
    let mut parameters = Map::from_iter([("size".to_owned(), serde_json::json!("2:3"))]);
    let error = apply_size_mapping(&mapping, &contract_face, &mut parameters)
        .expect_err("3K is not in the profile");
    assert!(error.contains("2:3") && error.contains("3K"), "{error}");
    assert_eq!(
        parameters.get("size"),
        Some(&serde_json::json!("2:3")),
        "失败时不许留下一个半换算的参数面：{parameters:?}"
    );
    // 取值不是字符串（合同只声明类型，取值本身从不校验）：换算不了，明确失败。
    let contract_face = Map::from_iter([("size".to_owned(), serde_json::json!(1024))]);
    let mut parameters = Map::new();
    let error = apply_size_mapping(&mapping, &contract_face, &mut parameters)
        .expect_err("a number is not a size");
    assert!(error.contains("must be a string"), "{error}");
}

#[test]
fn a_size_declaration_that_is_not_shaped_like_one_is_rejected() {
    for (mapping, needle) in [
        (serde_json::json!({"size": "pixels"}), "must be an object"),
        (
            serde_json::json!({"size": {"target": "size", "form": "pixels"}}),
            "size.source is required",
        ),
        (
            serde_json::json!({"size": {"source": [], "target": "size", "form": "pixels"}}),
            "size.source is required",
        ),
        (
            serde_json::json!({"size": {"source": ["size"], "form": "pixels"}}),
            "size.target is required",
        ),
        (
            serde_json::json!({"size": {"source": ["size"], "target": "size"}}),
            "size.form is required",
        ),
        (
            serde_json::json!({"size": {"source": ["size"], "target": "size", "form": "pixel"}}),
            "size.form is required",
        ),
        (
            serde_json::json!({
                "size": {"source": ["size"], "target": "size", "form": "pixels", "profile": []}
            }),
            "must be an object",
        ),
    ] {
        let error = declared_size_mapping(&mapping)
            .err()
            .unwrap_or_else(|| panic!("{mapping} 不是一份尺寸声明"));
        assert!(error.contains(needle), "{mapping} → {error}");
    }
    // 没有声明（键缺失或显式写 null）＝ 这条供给不做尺寸换算。
    assert_eq!(
        declared_size_mapping(&serde_json::json!({"defaults": {"watermark": false}})),
        Ok(None)
    );
    assert_eq!(
        declared_size_mapping(&serde_json::json!({"size": null})),
        Ok(None)
    );
}

/// 没声明尺寸换算的供给（纯透传）根本不经过换算函数：`auto` 照常原样上行。
///
/// 换算只在映射声明了 `size` 时才发生（[`declared_size_mapping`] 为 `None` 就没有这一步），
/// 因此"承载面收得了 `auto`"的渠道不会被平台拿去算一个比例。
#[test]
fn a_supply_without_a_size_declaration_passes_auto_through_untouched() {
    let mapping = serde_json::json!({"rename": {"size": "resolution"}});
    assert_eq!(
        declared_size_mapping(&mapping).expect("no size declaration is not an error"),
        None,
        "没有尺寸声明就没有换算这一步，`auto` 不会经过 convert_size"
    );
    let renames = declared_renames(&mapping).expect("a rename is declared");
    let mut parameters = Map::from_iter([
        ("prompt".to_owned(), serde_json::json!("x")),
        ("size".to_owned(), serde_json::json!("auto")),
    ]);
    apply_parameter_renames(
        &schema(serde_json::json!({
            "prompt": {"type": "string"},
            "resolution": {"type": "string"}
        })),
        renames.as_ref(),
        &mut parameters,
    )
    .expect("the wire name is declared");
    assert_eq!(
        parameters.get("resolution"),
        Some(&serde_json::json!("auto")),
        "纯透传供给把 `auto` 原样带到线上：{parameters:?}"
    );
    assert!(!parameters.contains_key("size"), "{parameters:?}");
}

/// 目标字段与源字段同名时（`size` 换算成 `size`），换算结果就写回同一个名字。
#[test]
fn a_single_source_field_can_be_converted_in_place() {
    let mapping = declared_size_mapping(&serde_json::json!({
        "size": {
            "source": ["size"],
            "target": "size",
            "form": "ratio",
            "profile": {"2K": {"16:9": "2848x1600"}}
        }
    }))
    .expect("the declaration parses")
    .expect("a size declaration is present");
    let contract_face = Map::from_iter([("size".to_owned(), serde_json::json!("2848x1600"))]);
    let mut parameters = contract_face.clone();
    apply_size_mapping(&mapping, &contract_face, &mut parameters).expect("reverse lookup");
    assert_eq!(parameters.get("size"), Some(&serde_json::json!("16:9")));
}

/// 平台补的**显式默认值**也参与换算：调用方没给档位、映射声明了默认档位时，
/// 那条默认值就是换算的输入，而不是被当成一个孤零零的字段发上去。
#[test]
fn an_injected_default_can_feed_the_conversion() {
    let contract = schema(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "size": {"type": "string"},
        "resolution": {"type": "string"}
    }));
    let carrier = contract.clone();
    let mapping = serde_json::json!({
        "defaults": {"resolution": "2K"},
        "size": {
            "source": ["size", "resolution"],
            "target": "size",
            "form": "pixels",
            "profile": {"2K": {"2:3": "1664x2496"}}
        }
    });
    let declared = declared_size_mapping(&mapping)
        .expect("the declaration parses")
        .expect("a size declaration is present");
    // 调用方只给了比例，档位由映射的显式默认值补上。
    let contract_face = Map::from_iter([
        ("model".to_owned(), serde_json::json!("m")),
        ("size".to_owned(), serde_json::json!("2:3")),
    ]);
    let mut parameters = contract_face.clone();
    apply_parameter_defaults(
        &contract,
        &carrier,
        None,
        declared_defaults(&mapping),
        &mut parameters,
    );
    assert_eq!(
        parameters.get("resolution"),
        Some(&serde_json::json!("2K")),
        "默认值先注入：{parameters:?}"
    );
    apply_size_mapping(&declared, &contract_face, &mut parameters)
        .expect("the default tier completes the conversion");
    assert_eq!(
        parameters.get("size"),
        Some(&serde_json::json!("1664x2496"))
    );
    assert!(
        !parameters.contains_key("resolution"),
        "被消耗的源字段（哪怕是默认值补进来的）不许再上行：{parameters:?}"
    );
}

/// 容器落位：`extra.quality` 判的是 `extra` 自己的声明，容器没声明时里面的名字一个都不算。
#[test]
fn a_container_field_counts_from_the_containers_own_declaration() {
    let with_extra = schema(serde_json::json!({
        "extra": {
            "type": "object",
            "additionalProperties": false,
            "properties": {"quality": {"type": "string"}}
        }
    }));
    assert!(declares_parameter(&with_extra, "extra.quality"));
    // 容器里的名字只在容器里算：顶层没有声明它。
    assert!(!declares_parameter(&with_extra, "quality"));
    assert!(carries_parameter(&with_extra, None, "extra.quality"));
    // 容器本身没声明：里面的名字一个都不算。
    let without_container = schema(serde_json::json!({"prompt": {"type": "string"}}));
    assert!(!declares_parameter(&without_container, "extra.quality"));
    // 改名落到容器里也算承载：合同字段 quality → 线上 extra.quality。
    let renames = declared_renames(&serde_json::json!({"rename": {"quality": "extra.quality"}}))
        .expect("the rename table is well formed")
        .expect("a rename table is declared");
    assert_eq!(
        wire_parameter_name(&with_extra, Some(&renames), "quality").as_deref(),
        Some("extra.quality")
    );
}
