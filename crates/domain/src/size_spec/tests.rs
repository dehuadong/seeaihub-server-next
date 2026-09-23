use super::*;

/// Seedream 5.0 lite 的 2K 档映射表（一手来源是厂商文档里的"分辨率 × 宽高比 → 宽高像素值"）。
/// 它出现在这里只是**测试夹具**：领域代码里没有任何厂商的档位表。
fn lite_2k() -> Value {
    serde_json::json!({
        "2K": {
            "1:1": "2048x2048",
            "4:3": "2304x1728",
            "3:4": "1728x2304",
            "16:9": "2848x1600",
            "9:16": "1600x2848",
            "3:2": "2496x1664",
            "2:3": "1664x2496",
            "21:9": "3136x1344"
        }
    })
}

/// Seedream 5.0 pro 的表：与 lite 在同一个档位、同一个比例上给出**不同的像素**。
/// 两份档案因此能证明"同一格查出来的值来自档案，而不是来自代码"。
fn pro_1k_and_2k() -> Value {
    serde_json::json!({
        "1K": {
            "1:1": "1024x1024",
            "4:3": "1152x864",
            "16:9": "1424x800",
            "2:3": "832x1248",
            "21:9": "1568x672"
        },
        "2K": {
            "1:1": "2048x2048",
            "4:3": "2368x1776",
            "16:9": "2816x1584",
            "2:3": "1664x2496",
            "21:9": "3136x1344"
        }
    })
}

fn profile(value: Value) -> SizeProfile {
    SizeProfile::from_json(&value).expect("the fixture profile parses")
}

#[test]
fn parses_the_four_forms_and_normalises_them() {
    for (value, expected) in [
        (
            "1024x1024",
            SizeSpec::Pixels {
                width: 1024,
                height: 1024,
            },
        ),
        (
            "2048X2048",
            SizeSpec::Pixels {
                width: 2048,
                height: 2048,
            },
        ),
        (
            "1664x2496",
            SizeSpec::Pixels {
                width: 1664,
                height: 2496,
            },
        ),
        (
            "16:9",
            SizeSpec::Ratio {
                ratio: "16:9".to_owned(),
            },
        ),
        // 比例约分：同一个宽高比的不同写法在解析后是同一个值。
        (
            "32:18",
            SizeSpec::Ratio {
                ratio: "16:9".to_owned(),
            },
        ),
        (
            "4:6",
            SizeSpec::Ratio {
                ratio: "2:3".to_owned(),
            },
        ),
        (
            "2K",
            SizeSpec::Tier {
                tier: "2K".to_owned(),
            },
        ),
        (
            "2k",
            SizeSpec::Tier {
                tier: "2K".to_owned(),
            },
        ),
        (
            "04K",
            SizeSpec::Tier {
                tier: "4K".to_owned(),
            },
        ),
        // 第四型：不是一个具体尺寸，线上原样就是 `auto`。
        ("auto", SizeSpec::Auto),
    ] {
        let parsed = SizeSpec::parse(value).unwrap_or_else(|error| panic!("`{value}`: {error}"));
        assert_eq!(parsed, expected, "`{value}`");
        // 序列化回字符串：线上就是这一份形态。
        assert_eq!(parsed.to_string(), expected.to_string(), "`{value}`");
    }
    assert_eq!(
        SizeSpec::parse("1024x1024").expect("pixels").form(),
        SizeForm::Pixels
    );
    assert_eq!(
        SizeSpec::parse("16:9").expect("ratio").form(),
        SizeForm::Ratio
    );
    assert_eq!(SizeSpec::parse("2K").expect("tier").form(), SizeForm::Tier);
    assert_eq!(
        SizeSpec::parse("auto").expect("auto").form(),
        SizeForm::Auto
    );
    // 型别的名字就是这四个。
    assert_eq!(SizeForm::parse("pixels"), Some(SizeForm::Pixels));
    assert_eq!(SizeForm::parse("ratio"), Some(SizeForm::Ratio));
    assert_eq!(SizeForm::parse("tier"), Some(SizeForm::Tier));
    assert_eq!(SizeForm::parse("auto"), Some(SizeForm::Auto));
    assert_eq!(SizeForm::parse("pixel"), None);
    assert_eq!(SizeForm::Pixels.as_str(), "pixels");
    assert_eq!(SizeForm::Auto.as_str(), "auto");
}

#[test]
fn rejects_anything_that_is_not_one_of_the_four_forms() {
    for value in [
        "",
        "1024",
        "1024x",
        "x1024",
        "1024x1024x1024",
        "0x1024",
        "1024x0",
        "0x0",
        "-1024x1024",
        "1e3x1e3",
        "1024.5x1024",
        " 1024x1024",
        "1024 x 1024",
        "16:",
        ":9",
        "16:9:1",
        "0:9",
        "16:0",
        "1.5:1",
        "K",
        "k",
        "0K",
        "2M",
        "2 K",
        "K2",
        "2KB",
        "99999999999999K",
        "1024x99999999999999",
        // `auto` 只有这一个写法：它不是"随便什么写法都能认"的别名。
        "AUTO",
        "Auto",
        " auto",
        "auto ",
    ] {
        let error = SizeSpec::parse(value)
            .err()
            .unwrap_or_else(|| panic!("`{value}` 不是一个尺寸"));
        assert!(
            error.contains("size"),
            "报错要说清这不是一个尺寸：`{value}` → {error}"
        );
    }
}

/// `auto` 的语义是"由模型按提示词自己决定最佳比例"：它只原样透传，永不换算。
#[test]
fn auto_is_only_passed_through_and_never_converted() {
    let lite = profile(lite_2k());
    let auto = [SizeSpec::parse("auto").expect("auto")];
    // 目标形态本身就是 `auto`：原样通过，档案用不上。
    assert_eq!(
        convert_size(SizeForm::Auto, &SizeProfile::default(), &auto).expect("auto passes through"),
        SizeSpec::Auto
    );
    // 目标形态是别的形态：明确失败，错误信息说清"只能原样透传、不能换算"。
    for form in [SizeForm::Pixels, SizeForm::Ratio, SizeForm::Tier] {
        let error = convert_size(form, &lite, &auto).expect_err("auto is not convertible at all");
        assert!(
            error.contains("auto")
                && error.contains("passed through")
                && error.contains("cannot be converted"),
            "报错要说清 `auto` 只能原样透传、不能换算：{error}"
        );
    }
    // 目标形态就是 `auto`，但请求还给了别的尺寸取值：那些取值会被无声丢掉，因此失败。
    let error = convert_size(
        SizeForm::Auto,
        &lite,
        &[SizeSpec::Auto, SizeSpec::parse("2K").expect("tier")],
    )
    .expect_err("the tier would be silently dropped");
    assert!(
        error.contains("a tier") && error.contains("cannot convert"),
        "{error}"
    );
    // 目标形态是 `auto`、请求给的是像素：不能把调用方说的尺寸换成"让模型自己挑"。
    let error = convert_size(
        SizeForm::Auto,
        &lite,
        &[SizeSpec::parse("1024x1024").expect("pixels")],
    )
    .expect_err("pixels cannot become auto");
    assert!(
        error.contains("`auto`") && error.contains("cannot convert"),
        "{error}"
    );
    // 同一个 `auto` 给两次不是含糊：它就是同一个值。
    assert_eq!(
        convert_size(
            SizeForm::Auto,
            &lite,
            &[SizeSpec::Auto, SizeSpec::parse("auto").expect("auto")]
        )
        .expect("auto twice is the same value"),
        SizeSpec::Auto
    );
}

#[test]
fn converts_a_ratio_and_a_tier_into_pixels_with_the_published_profile() {
    let lite = profile(lite_2k());
    for (ratio, expected) in [
        ("1:1", (2048, 2048)),
        ("4:3", (2304, 1728)),
        ("16:9", (2848, 1600)),
        ("3:2", (2496, 1664)),
        // 设计里点名的那一格：2:3 + 2K。
        ("2:3", (1664, 2496)),
        ("21:9", (3136, 1344)),
    ] {
        let values = [
            SizeSpec::parse(ratio).expect("ratio"),
            SizeSpec::parse("2K").expect("tier"),
        ];
        let converted = convert_size(SizeForm::Pixels, &lite, &values)
            .unwrap_or_else(|error| panic!("{ratio} + 2K: {error}"));
        assert_eq!(
            converted,
            SizeSpec::Pixels {
                width: expected.0,
                height: expected.1
            },
            "{ratio} + 2K"
        );
        assert_eq!(
            converted.to_string(),
            format!("{}x{}", expected.0, expected.1)
        );
    }
    // 同一个档位、同一个比例，在两份档案上换算出的像素不同：值来自档案。
    let pro = profile(pro_1k_and_2k());
    let values = [
        SizeSpec::parse("16:9").expect("ratio"),
        SizeSpec::parse("2K").expect("tier"),
    ];
    assert_eq!(
        convert_size(SizeForm::Pixels, &pro, &values).expect("pro 2K 16:9"),
        SizeSpec::Pixels {
            width: 2816,
            height: 1584
        }
    );
    assert_eq!(
        convert_size(SizeForm::Pixels, &lite, &values).expect("lite 2K 16:9"),
        SizeSpec::Pixels {
            width: 2848,
            height: 1600
        }
    );
    // 调用方直接给像素、供给也要像素：不需要档案，空档案也照样通过。
    let direct = [SizeSpec::parse("3750x1250").expect("pixels")];
    assert_eq!(
        convert_size(SizeForm::Pixels, &SizeProfile::default(), &direct)
            .expect("pixels pass through"),
        SizeSpec::Pixels {
            width: 3750,
            height: 1250
        }
    );
}

#[test]
fn converts_pixels_back_into_a_ratio_and_a_tier() {
    let pro = profile(pro_1k_and_2k());
    let pixels = [SizeSpec::parse("1024x1024").expect("pixels")];
    assert_eq!(
        convert_size(SizeForm::Ratio, &pro, &pixels).expect("ratio"),
        SizeSpec::Ratio {
            ratio: "1:1".to_owned()
        }
    );
    assert_eq!(
        convert_size(SizeForm::Tier, &pro, &pixels).expect("tier"),
        SizeSpec::Tier {
            tier: "1K".to_owned()
        }
    );
    // 同一份档案里另一档的像素值反查回另一档。
    let pixels = [SizeSpec::parse("2048x2048").expect("pixels")];
    assert_eq!(
        convert_size(SizeForm::Tier, &pro, &pixels).expect("tier"),
        SizeSpec::Tier {
            tier: "2K".to_owned()
        }
    );
    // 档案里没有的像素值：反查不出，明确失败。
    let pixels = [SizeSpec::parse("1500x1500").expect("pixels")];
    let error = convert_size(SizeForm::Tier, &pro, &pixels).expect_err("no such entry");
    assert!(error.contains("1500x1500"), "{error}");
    // 比例/档位本来就是要的那一型：原样用，不查档案。
    assert_eq!(
        convert_size(
            SizeForm::Ratio,
            &SizeProfile::default(),
            &[SizeSpec::parse("32:18").expect("ratio")]
        )
        .expect("ratio pass through"),
        SizeSpec::Ratio {
            ratio: "16:9".to_owned()
        }
    );
}

#[test]
fn refuses_to_guess_when_the_profile_has_no_such_combination() {
    let lite = profile(lite_2k());
    // lite 的档案里只有 2K：3K 那一格缺了。
    let values = [
        SizeSpec::parse("2:3").expect("ratio"),
        SizeSpec::parse("3K").expect("tier"),
    ];
    let error = convert_size(SizeForm::Pixels, &lite, &values).expect_err("3K is not in lite");
    assert!(error.contains("2:3") && error.contains("3K"), "{error}");
    // 比例缺了同样失败：档案里没有 5:4。
    let values = [
        SizeSpec::parse("5:4").expect("ratio"),
        SizeSpec::parse("2K").expect("tier"),
    ];
    let error = convert_size(SizeForm::Pixels, &lite, &values).expect_err("5:4 is not in lite");
    assert!(error.contains("5:4") && error.contains("2K"), "{error}");
}

#[test]
fn refuses_size_values_it_cannot_turn_into_the_form_the_offering_wants() {
    let lite = profile(lite_2k());
    // 只有一个比例、没有档位：查不到像素，也不许猜一个档位。
    let error = convert_size(
        SizeForm::Pixels,
        &lite,
        &[SizeSpec::parse("16:9").expect("ratio")],
    )
    .expect_err("a ratio alone cannot become pixels");
    assert!(
        error.contains("pixels") && error.contains("a ratio"),
        "{error}"
    );
    // 只有一个档位：同理。
    let error = convert_size(
        SizeForm::Pixels,
        &lite,
        &[SizeSpec::parse("2K").expect("tier")],
    )
    .expect_err("a tier alone cannot become pixels");
    assert!(error.contains("a tier"), "{error}");
    // 供给只要比例，调用方却同时给了比例与档位：档位会被无声丢掉，因此失败。
    let error = convert_size(
        SizeForm::Ratio,
        &lite,
        &[
            SizeSpec::parse("16:9").expect("ratio"),
            SizeSpec::parse("2K").expect("tier"),
        ],
    )
    .expect_err("the tier would be silently dropped");
    assert!(error.contains("ratio") && error.contains("tier"), "{error}");
    // 一次请求给出两个档位：含糊，失败。
    let error = convert_size(
        SizeForm::Pixels,
        &lite,
        &[
            SizeSpec::parse("2:3").expect("ratio"),
            SizeSpec::parse("2K").expect("tier"),
            SizeSpec::parse("3K").expect("tier"),
        ],
    )
    .expect_err("two tiers are ambiguous");
    assert!(error.contains("two different tier"), "{error}");
    // 一个尺寸都没给：也说清楚。
    let error = convert_size(SizeForm::Pixels, &lite, &[]).expect_err("no size at all");
    assert!(error.contains("no size value"), "{error}");
}

/// 反向查必须有确定答案：同一组像素在档案里出现两次时按键序取第一组。
#[test]
fn the_reverse_lookup_is_deterministic() {
    let profile = profile(serde_json::json!({
        "2K": {"1:1": "2048x2048", "4:3": "2048x2048"},
        "1K": {"1:1": "2048x2048"}
    }));
    assert_eq!(
        profile.lookup_reverse(2048, 2048),
        Some(("1:1".to_owned(), "1K".to_owned())),
        "键序最靠前的那一组"
    );
    assert_eq!(profile.lookup_reverse(1024, 1024), None);
    assert_eq!(profile.lookup("1:1", "2K"), Some((2048, 2048)));
    assert_eq!(profile.lookup("1:1", "4K"), None);
}

#[test]
fn the_profile_keys_are_normalised_when_parsed() {
    // 档案里写小写档位、没约分的比例，查的时候用规范化后的形态照样命中。
    let profile = profile(serde_json::json!({
        "2k": {"32:18": "2848x1600"}
    }));
    assert_eq!(profile.lookup("16:9", "2K"), Some((2848, 1600)));
    assert!(!profile.is_empty());
    assert!(SizeProfile::default().is_empty());
}

#[test]
fn a_profile_that_is_not_a_table_is_rejected() {
    for (value, needle) in [
        (serde_json::json!([]), "must be an object"),
        (serde_json::json!("2K"), "must be an object"),
        (serde_json::json!({"2K": "2048x2048"}), "must be an object"),
        (
            serde_json::json!({"2K": {"1:1": 2048}}),
            "must be a pixel string",
        ),
        (serde_json::json!({"2K": {"1:1": "16:9"}}), "must be pixels"),
        (
            serde_json::json!({"1:1": {"2K": "2048x2048"}}),
            "must be a tier",
        ),
        (
            serde_json::json!({"2K": {"2K": "2048x2048"}}),
            "must be a ratio",
        ),
        (serde_json::json!({"2K": {"1:1": "0x0"}}), "is not a size"),
        // 规范化之后撞在同一格上：档案自己含糊，拒绝。
        (
            serde_json::json!({"2K": {"1:1": "2048x2048", "2:2": "2048x2048"}}),
            "twice",
        ),
    ] {
        let error = SizeProfile::from_json(&value)
            .err()
            .unwrap_or_else(|| panic!("{value} 不是一张表"));
        assert!(error.contains(needle), "{value} → {error}");
    }
}
