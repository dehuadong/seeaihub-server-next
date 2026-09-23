use super::*;

fn schema(properties: Value) -> Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": properties
    })
}

#[test]
fn places_reference_images_on_the_candidates_own_parameter() {
    let vendor = schema(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "image_urls": {"type": "array", "items": {"type": "string"}, "maxItems": 4},
        "mask_url": {"type": "string"}
    }));
    let mut parameters = Map::new();
    parameters.insert("prompt".to_owned(), Value::String("x".to_owned()));
    place_image_inputs(
        &vendor,
        &mut parameters,
        &[
            "https://example.invalid/a.png".to_owned(),
            "data:image/png;base64,AAAA".to_owned(),
        ],
        Some("data:image/png;base64,BBBB"),
    )
    .expect("the candidate can express both inputs");
    assert_eq!(
        parameters.get("image_urls"),
        Some(&serde_json::json!([
            "https://example.invalid/a.png",
            "data:image/png;base64,AAAA"
        ]))
    );
    assert_eq!(
        parameters.get("mask_url"),
        Some(&Value::String("data:image/png;base64,BBBB".to_owned()))
    );
}

#[test]
fn a_candidate_that_cannot_express_the_input_is_rejected() {
    let text_only = schema(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"}
    }));
    let mut parameters = Map::new();
    let error = place_image_inputs(
        &text_only,
        &mut parameters,
        &["https://example.invalid/a.png".to_owned()],
        None,
    )
    .expect_err("a text-only offering cannot take a reference image");
    assert!(error.contains("reference image"), "{error}");
    // 只给遮罩、没有参考图字段时也一样拒绝。
    let error = place_image_inputs(&text_only, &mut parameters, &[], Some("data:,"))
        .expect_err("a text-only offering cannot take a mask");
    assert!(error.contains("mask"), "{error}");
    // 单值字段装不下两张。
    let single = schema(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "image": {"type": "string"}
    }));
    assert!(
        place_image_inputs(
            &single,
            &mut Map::new(),
            &["a".to_owned(), "b".to_owned()],
            None
        )
        .is_err()
    );
}

#[test]
fn declared_reference_image_limit_needs_a_promise_from_the_profile() {
    let bounded = schema(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "image_urls": {"type": "array", "items": {"type": "string"}, "maxItems": 3}
    }));
    assert_eq!(declared_reference_image_limit(&bounded), Some(3));
    // 数组没写 maxItems ＝ 没有承诺上限，不能据它接受任何收图限制。
    let unbounded = schema(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "image_urls": {"type": "array", "items": {"type": "string"}}
    }));
    assert_eq!(declared_reference_image_limit(&unbounded), None);
    // 单值参数按一张算。
    let scalar = schema(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "image": {"type": "string"}
    }));
    assert_eq!(declared_reference_image_limit(&scalar), Some(1));
    assert!(!declares_mask_parameter(&scalar));
    assert!(declares_reference_image_parameter(&scalar));
}

#[test]
fn reads_back_what_placement_wrote() {
    let vendor = schema(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "image_urls": {"type": "array", "items": {"type": "string"}},
        "mask_url": {"type": "string"}
    }));
    let names = platform_image_parameters(&vendor, crate::ImageBranch::Masked);
    assert_eq!(
        names,
        vec!["image_urls".to_owned(), "mask_url".to_owned()],
        "名单就是候选声明面里那几个名字"
    );
    let parameters = serde_json::json!({
        "model": "m",
        "prompt": "x",
        "image_urls": ["https://example.invalid/a.png", "data:image/png;base64,AAAA"],
        "mask_url": "data:image/png;base64,BBBB"
    });
    let inputs = image_inputs(&parameters, &names).expect("inputs");
    assert_eq!(
        inputs.reference_images,
        vec![
            "https://example.invalid/a.png".to_owned(),
            "data:image/png;base64,AAAA".to_owned()
        ]
    );
    assert_eq!(inputs.mask.as_deref(), Some("data:image/png;base64,BBBB"));
    // 名单里的名字同时也是"平台的图该写到哪儿"的答案：按角色取名字，同样只有名单说了算。
    assert_eq!(
        platform_image_parameter(&names, ImageParameterKind::Reference),
        Some("image_urls")
    );
    assert_eq!(
        platform_image_parameter(&names, ImageParameterKind::Mask),
        Some("mask_url")
    );
    // 名单里没有这个角色：没有名字可用，调用方据此明确失败（不退回写死的名字）。
    assert_eq!(
        platform_image_parameter(&[], ImageParameterKind::Reference),
        None
    );
    // 单值形态同样读得出来。
    let scalar = serde_json::json!({"image": "https://example.invalid/a.png"});
    assert_eq!(
        image_inputs(&scalar, &["image".to_owned()])
            .expect("scalar")
            .reference_images,
        vec!["https://example.invalid/a.png".to_owned()]
    );
    // 名单里没有的名字：不是平台装载的东西，一个都不读。
    let not_ours = serde_json::json!({
        "image_with_roles": [{"role": "reference", "url": "https://example.invalid/a.png"}],
        "images": ["https://example.invalid/b.png"]
    });
    assert_eq!(
        image_inputs(&not_ours, &names).expect("no image is not an error"),
        ImageInputs::default()
    );
}

/// 声明面过滤：没声明的参数名一律留下不来，声明的参数（含取值）一字不改。
#[test]
fn only_declared_parameter_names_survive_the_filter() {
    let vendor = schema(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "quality": {"enum": ["low", "high"]},
        "n": {"type": "integer"},
        "images": {"type": "array", "items": {"type": "string"}}
    }));
    let supplied = Map::from_iter([
        ("model".to_owned(), serde_json::json!("m")),
        ("prompt".to_owned(), serde_json::json!("x")),
        // 声明过的：原样留下，取值不做任何校验或改写。
        ("quality".to_owned(), serde_json::json!("high")),
        ("n".to_owned(), serde_json::json!("not-a-number")),
        // 没声明的：直接丢掉，既不报错也不出现在结果里。
        ("seed".to_owned(), serde_json::json!(7)),
        ("foo".to_owned(), serde_json::json!({"a": 1})),
        (
            "image_with_roles".to_owned(),
            serde_json::json!([{"role": "reference", "url": "https://example.invalid/a.png"}]),
        ),
    ]);
    let kept = declared_parameter_names(&vendor, &supplied);
    assert_eq!(
        kept.keys().collect::<Vec<_>>(),
        vec!["model", "n", "prompt", "quality"],
        "只留声明过的名字"
    );
    assert_eq!(kept.get("quality"), supplied.get("quality"));
    assert_eq!(kept.get("n"), supplied.get("n"));
    assert!(
        supplied.contains_key("seed") && supplied.contains_key("image_with_roles"),
        "过滤不改动入参：平台只是不把它们交给上游"
    );
    // 一份 schema 一个属性都没声明（连 properties 都没有）：什么都不留。
    assert_eq!(
        declared_parameter_names(&serde_json::json!({"type": "object"}), &supplied),
        Map::new()
    );
    // 只写进 `required`、`properties` 里没有的名字同样是"声明过的"：过滤留得下它。
    // 漏掉它就会把调用方给的一个声明字段静默丢掉，而"没声明"与"声明了"的处置完全相反。
    let required_only = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt", "seed"],
        "properties": {
            "model": {"const": "m"},
            "prompt": {"type": "string"}
        }
    });
    let kept = declared_parameter_names(&required_only, &supplied);
    assert_eq!(
        kept.keys().collect::<Vec<_>>(),
        vec!["model", "prompt", "seed"]
    );
}

/// 过滤不会把平台装载的图丢掉：装载用的名字就是从同一份声明面选出来的。
///
/// 顺序也在这里钉住：先按声明面过滤、再装载图片（受理侧就是这么用的），
/// 图片照样落在候选声明的名字上；反过来先装再滤也是一样结果。
#[test]
fn filtering_keeps_the_images_the_platform_placed() {
    let vendor = schema(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "image_urls": {"type": "array", "items": {"type": "string"}},
        "mask_url": {"type": "string"}
    }));
    let supplied = Map::from_iter([
        ("prompt".to_owned(), serde_json::json!("x")),
        ("image_with_roles".to_owned(), serde_json::json!([])),
    ]);
    let mut filtered = declared_parameter_names(&vendor, &supplied);
    place_image_inputs(
        &vendor,
        &mut filtered,
        &["data:image/png;base64,AAAA".to_owned()],
        Some("data:image/png;base64,BBBB"),
    )
    .expect("the candidate declares both image parameters");
    assert_eq!(
        filtered.get("image_urls"),
        Some(&serde_json::json!(["data:image/png;base64,AAAA"]))
    );
    assert_eq!(
        filtered.get("mask_url"),
        Some(&serde_json::json!("data:image/png;base64,BBBB"))
    );
    assert!(!filtered.contains_key("image_with_roles"));
    // 名单里的名字必然都在声明面内，过滤前后都留得下。
    for name in platform_image_parameters(&vendor, crate::ImageBranch::Masked) {
        assert!(
            declared_parameter_names(&vendor, &filtered).contains_key(&name),
            "装载的 `{name}` 不会被过滤掉"
        );
    }
    // 先装载再过滤：同一份声明面下结果一致。
    let mut placed = supplied.clone();
    place_image_inputs(
        &vendor,
        &mut placed,
        &["data:image/png;base64,AAAA".to_owned()],
        Some("data:image/png;base64,BBBB"),
    )
    .expect("the candidate declares both image parameters");
    assert_eq!(declared_parameter_names(&vendor, &placed), filtered);
}

/// 空值与形状约定：三处读写图片的地方共用这一份，行为必须一模一样。
#[test]
fn null_and_empty_values_mean_there_is_no_image() {
    let reference = ["image".to_owned()];
    let with_mask = ["image".to_owned(), "mask".to_owned()];
    for value in [
        serde_json::json!(null),
        serde_json::json!(""),
        serde_json::json!([]),
        serde_json::json!([null]),
        serde_json::json!([""]),
        serde_json::json!(["", null]),
    ] {
        assert_eq!(
            image_inputs(&serde_json::json!({"image": value}), &reference)
                .expect("no image is not an error"),
            ImageInputs::default(),
            "`{value}` 就是「没有图」"
        );
    }
    // 标量与数组两种形态都读得出来，数组按调用方给的顺序。
    assert_eq!(
        image_inputs(&serde_json::json!({"image": "a"}), &reference).expect("scalar"),
        ImageInputs {
            reference_images: vec!["a".to_owned()],
            mask: None,
        }
    );
    assert_eq!(
        image_inputs(&serde_json::json!({"image": ["a", "b"]}), &reference).expect("array"),
        ImageInputs {
            reference_images: vec!["a".to_owned(), "b".to_owned()],
            mask: None,
        }
    );
    // 数组里的空值只是"这一格没给"，不占位。
    assert_eq!(
        image_inputs(
            &serde_json::json!({"image": ["a", null, "", "b"]}),
            &reference
        )
        .expect("holes"),
        ImageInputs {
            reference_images: vec!["a".to_owned(), "b".to_owned()],
            mask: None,
        }
    );
    // 遮罩同一套约定：空值＝没给。
    assert_eq!(
        image_inputs(&serde_json::json!({"image": "a", "mask": ""}), &with_mask)
            .expect("empty mask"),
        ImageInputs {
            reference_images: vec!["a".to_owned()],
            mask: None,
        }
    );
}

/// 归属只看名单：**取值长得再像图**，名字不在名单里就不是平台的图片参数。
///
/// 这正是按取值形状判归属会出错的地方——调用方给渠道的原生名 `images`（字符串数组、
/// 值就是公网 URL），看起来与平台装载的参考图一模一样。
#[test]
fn ownership_comes_from_the_name_list_not_from_the_shape_of_the_value() {
    let vendor = schema(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "image_urls": {"type": "array", "items": {"type": "string"}},
        "mask_url": {"type": "string"}
    }));
    // 文生图分支：平台一张图都没装载，名单为空——哪怕是候选声明过的名字也不认领。
    assert!(
        platform_image_parameters(&vendor, crate::ImageBranch::PromptOnly).is_empty(),
        "prompt_only 分支平台不装载任何图"
    );
    // 图生图分支：只有参考图那个名字，遮罩还没进名单。
    assert_eq!(
        platform_image_parameters(&vendor, crate::ImageBranch::ImageConditioned),
        vec!["image_urls".to_owned()]
    );
    // 名单为空时，调用方给的图名参数（`images`）同样不许被当成图片。
    let caller_supplied = serde_json::json!({
        "prompt": "x",
        "images": ["https://example.invalid/b.png"],
        "image_with_roles": [{"role": "reference", "url": "https://example.invalid/a.png"}]
    });
    assert_eq!(
        image_inputs(
            &caller_supplied,
            &platform_image_parameters(&vendor, crate::ImageBranch::PromptOnly)
        )
        .expect("caller supplied parameters are not images"),
        ImageInputs::default()
    );
    // 同一个名字在名单里才被读走，取值形状（标量/数组）不影响归属。
    let placed = serde_json::json!({
        "image_urls": ["https://example.invalid/a.png"],
        "images": ["https://example.invalid/b.png"]
    });
    let inputs = image_inputs(
        &placed,
        &platform_image_parameters(&vendor, crate::ImageBranch::ImageConditioned),
    )
    .expect("inputs");
    assert_eq!(
        inputs.reference_images,
        vec!["https://example.invalid/a.png".to_owned()],
        "名单外的同名参数不参与读图"
    );
}

/// 受理侧只认契约字段名：取值解释不成一张图的直接报错，其余名字一个字都不碰。
#[test]
fn the_contract_side_reads_exactly_three_names() {
    let mut parameters = serde_json::Map::from_iter([
        ("model".to_owned(), serde_json::json!("m")),
        ("prompt".to_owned(), serde_json::json!("x")),
        ("image_urls".to_owned(), serde_json::json!(["a", "", "b"])),
        (
            "mask".to_owned(),
            serde_json::json!("data:image/png;base64,BBBB"),
        ),
        // 渠道文档里的一手参数：名字以 image 开头、含 mask，但不是平台契约字段。
        (
            "image_with_roles".to_owned(),
            serde_json::json!([{"role": "reference", "url": "https://example.invalid/a.png"}]),
        ),
        (
            "mask_url".to_owned(),
            serde_json::json!("https://example.invalid/m.png"),
        ),
        (
            "images".to_owned(),
            serde_json::json!(["https://example.invalid/b.png"]),
        ),
    ]);
    let inputs = take_contract_image_inputs(&mut parameters).expect("inputs");
    assert_eq!(
        inputs.reference_images,
        vec!["a".to_owned(), "b".to_owned()]
    );
    assert_eq!(inputs.mask.as_deref(), Some("data:image/png;base64,BBBB"));
    // 只摘契约里的三个名字：其余字段留在参数面里，等选路后按候选声明面处置。
    assert_eq!(
        parameters.keys().collect::<Vec<_>>(),
        vec!["image_with_roles", "images", "mask_url", "model", "prompt"]
    );
    // 契约字段的取值解释不成一张图：拒绝，不猜。
    for value in [
        serde_json::json!(7),
        serde_json::json!(true),
        serde_json::json!({"url": "a"}),
        serde_json::json!([7]),
        serde_json::json!(["a", false]),
    ] {
        let mut wrong = serde_json::Map::from_iter([
            ("model".to_owned(), serde_json::json!("m")),
            ("image".to_owned(), value.clone()),
        ]);
        assert!(
            take_contract_image_inputs(&mut wrong).is_err(),
            "`{value}` 不是一张图"
        );
    }
    // 遮罩没有数组形态：给一个数组（哪怕只有一项）也是形状不对。
    for value in [serde_json::json!(["m1"]), serde_json::json!(["m1", "m2"])] {
        let mut wrong = serde_json::Map::from_iter([
            ("image".to_owned(), serde_json::json!("a")),
            ("mask".to_owned(), value),
        ]);
        assert!(take_contract_image_inputs(&mut wrong).is_err());
    }
    // 空值是"没给"，不是错误；空值也不参与同义判定。
    let mut blank = serde_json::Map::from_iter([
        ("image".to_owned(), serde_json::json!("")),
        ("mask".to_owned(), serde_json::json!(null)),
    ]);
    assert_eq!(
        take_contract_image_inputs(&mut blank).expect("blank is not an error"),
        ImageInputs::default()
    );
    assert!(blank.is_empty(), "摘干净的契约字段不再留在参数面里");
}

/// `image` 与 `image_urls` 同义：只有两边都给出非空值才算含糊。
#[test]
fn synonymous_fields_conflict_only_when_both_carry_a_value() {
    let conflict = |image: Value, image_urls: Value| {
        let mut parameters = serde_json::Map::from_iter([
            ("model".to_owned(), serde_json::json!("m")),
            ("image".to_owned(), image),
            ("image_urls".to_owned(), image_urls),
        ]);
        take_contract_image_inputs(&mut parameters)
    };
    // 两边都有图：含糊，拒绝。
    let error = conflict(
        serde_json::json!("https://example.invalid/a.png"),
        serde_json::json!(["https://example.invalid/b.png"]),
    )
    .expect_err("two synonyms with values");
    assert!(error.contains("synonyms"), "{error}");
    // 一边是空位：另一边说了算，不是冲突。
    for empty in [
        serde_json::json!(null),
        serde_json::json!(""),
        serde_json::json!([]),
        serde_json::json!([null]),
        serde_json::json!([""]),
    ] {
        let inputs = conflict(
            empty.clone(),
            serde_json::json!(["https://example.invalid/b.png"]),
        )
        .unwrap_or_else(|error| panic!("`{empty}` 是空位，不该冲突：{error}"));
        assert_eq!(
            inputs.reference_images,
            vec!["https://example.invalid/b.png".to_owned()]
        );
        let inputs = conflict(
            serde_json::json!("https://example.invalid/a.png"),
            empty.clone(),
        )
        .unwrap_or_else(|error| panic!("`{empty}` 是空位，不该冲突：{error}"));
        assert_eq!(
            inputs.reference_images,
            vec!["https://example.invalid/a.png".to_owned()]
        );
    }
    // 两边都是空位：就是"没有图"。
    assert_eq!(
        conflict(serde_json::json!(null), serde_json::json!([])).expect("no image"),
        ImageInputs::default()
    );
}

#[test]
fn a_name_has_exactly_one_meaning() {
    assert_eq!(
        image_parameter_kind("image_urls"),
        Some(ImageParameterKind::Reference)
    );
    assert_eq!(
        image_parameter_kind("mask_url"),
        Some(ImageParameterKind::Mask)
    );
    assert_eq!(image_parameter_kind("prompt"), None);
    // 两类都像的名字按遮罩归类：遮罩的含义更窄，优先。
    assert_eq!(
        image_parameter_kind("image_mask"),
        Some(ImageParameterKind::Mask)
    );
    assert!(is_reference_image_parameter("image_mask"));
    assert!(is_mask_parameter("image_mask"));
    // 归类变了，候选面据此选到的字段也跟着变。
    let both_names = schema(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "image_mask": {"type": "string"}
    }));
    assert!(declares_mask_parameter(&both_names));
    assert!(!declares_reference_image_parameter(&both_names));
    let mut parameters = Map::new();
    place_image_inputs(&both_names, &mut parameters, &[], Some("data:,"))
        .expect("image_mask 是遮罩字段，装得下这块编辑范围");
    assert_eq!(
        parameters.get("image_mask"),
        Some(&serde_json::json!("data:,"))
    );
}

/// 契约面与候选面是两件事：同一个名字在两面里的归属可以不同。
#[test]
fn the_contract_side_does_not_use_the_shape_of_the_name() {
    assert_eq!(
        contract_image_parameter_kind("image"),
        Some(ImageParameterKind::Reference)
    );
    assert_eq!(
        contract_image_parameter_kind("image_urls"),
        Some(ImageParameterKind::Reference)
    );
    assert_eq!(
        contract_image_parameter_kind("mask"),
        Some(ImageParameterKind::Mask)
    );
    for name in ["image_with_roles", "mask_url", "images", "image_mask"] {
        assert_eq!(contract_image_parameter_kind(name), None, "{name}");
        // 但它们在候选面里仍然按名字形状认。
        assert!(image_parameter_kind(name).is_some(), "{name}");
    }
}
