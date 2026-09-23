use super::*;

#[test]
fn validates_prompt_only_native_request() {
    let request = image_request(serde_json::json!({"prompt": "hello"}));
    let vendor = offering();
    let face = contract_face(&request, &vendor);
    assert_eq!(
        face.keys().collect::<Vec<_>>(),
        vec!["model", "prompt"],
        "`model` 由平台自己落，其余按合同留下"
    );
    assert!(prepare_carrier_parameters(&face, &request, &vendor).is_ok());
}

/// 合同里没有的字段在受理期丢掉——不报错，也不会跟着 Job 走去上游。
///
/// 判据是**合同**：调用方多发一个平台不认的字段（渠道一手参数、`seed`、纯属多余的 `foo`）
/// 不该让整次请求失败；而"这个字段在命中的候选上存不存在"本身随选路变化，逐次报错会把
/// 选路结果变成调用方的负担。
#[test]
fn parameters_the_contract_never_declared_are_dropped_without_an_error() {
    let mut vendor = offering();
    // 合同与承载面这次同值：这条用例验的是"合同外的字段"，与承载面无关。
    let declared = surface(serde_json::json!({
        "model": {"const": "gpt-image-2"},
        "prompt": {"type": "string", "minLength": 1},
        "n": {"type": "integer"},
        "quality": {"enum": ["low", "high"]}
    }));
    vendor.capability_schema = declared.clone();
    vendor.carrier_schema = declared;
    let request = image_request(serde_json::json!({
        "prompt": "hello",
        "n": "not-a-number",
        "quality": "high",
        "channel_specific_knob": {"a": 1},
        "seed": 7,
        "foo": "bar",
        "image_with_roles": [{"role": "reference", "url": "https://example.invalid/a.png"}]
    }));
    let face = contract_face(&request, &vendor);
    assert_eq!(
        face.keys().collect::<Vec<_>>(),
        vec!["model", "n", "prompt", "quality"],
        "只有合同声明过的名字留到 Job 里：{face:?}"
    );
    // 合同声明过的参数取值不校验：类型不对也照原样留下。
    assert_eq!(face.get("n"), Some(&serde_json::json!("not-a-number")));
    assert_eq!(face.get("quality"), Some(&serde_json::json!("high")));
    let prepared = prepare_carrier_parameters(&face, &request, &vendor)
        .expect("the carrier declares every field the request uses");
    assert_eq!(prepared, Value::Object(face));
}

/// 请求**用到的**字段必须在这条候选的承载面里；缺了就是这条候选不合格。
///
/// 注意它与"请求违反合同"是两件事：请求本身没问题（`quality` 在合同里），只是这条供给
/// 承载不了它——所以这条候选落选、换下一条，而不是把整次请求判成参数错。
#[test]
fn a_used_field_the_carrier_cannot_carry_makes_the_candidate_ineligible() {
    let mut vendor = offering();
    vendor.capability_schema = surface(serde_json::json!({
        "model": {"const": "gpt-image-2"},
        "prompt": {"type": "string"},
        "quality": {"enum": ["low", "high"]}
    }));
    // 承载面收窄：这条供给承载不了 `quality`。
    vendor.carrier_schema = surface(serde_json::json!({
        "model": {"const": "gpt-image-2"},
        "prompt": {"type": "string"}
    }));
    let request = image_request(serde_json::json!({"prompt": "hello", "quality": "high"}));
    let face = contract_face(&request, &vendor);
    let reason = prepare_carrier_parameters(&face, &request, &vendor)
        .expect_err("the carrier cannot carry quality");
    assert!(reason.contains("quality"), "{reason}");

    // 调用方没用这个字段（空值＝没给）：这条候选照样合格，空位也不会被发上去。
    for empty in [
        serde_json::json!(null),
        serde_json::json!(""),
        serde_json::json!([]),
    ] {
        let request =
            image_request(serde_json::json!({"prompt": "hello", "quality": empty.clone()}));
        let face = contract_face(&request, &vendor);
        let prepared = prepare_carrier_parameters(&face, &request, &vendor)
            .unwrap_or_else(|error| panic!("`{empty}` 是没给，候选该合格：{error}"));
        assert!(
            prepared.get("quality").is_none(),
            "承载面没声明的空位不该发上去：{prepared}"
        );
    }
}

#[test]
fn filtering_does_not_drop_the_images_the_platform_places() {
    // 装载用的名字取自同一份声明面，所以"先过滤、再装载"不会把图丢掉。
    let mut vendor = offering();
    vendor.carrier_schema = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": {
            "model": {"const": "gpt-image-2"},
            "prompt": {"type": "string", "minLength": 1},
            "image_urls": {"type": "array", "items": {"type": "string"}, "minItems": 1},
            "mask_url": {"type": "string"}
        }
    });
    let mut request = image_request(serde_json::json!({
        "prompt": "hello",
        "image_with_roles": [],
        "seed": 1
    }));
    request.reference_images = vec!["data:image/png;base64,AAAA".to_owned()];
    request.mask = Some("data:image/png;base64,BBBB".to_owned());
    let face = contract_face(&request, &vendor);
    let prepared = prepare_carrier_parameters(&face, &request, &vendor).expect("images are placed");
    assert_eq!(
        prepared.get("image_urls"),
        Some(&serde_json::json!(["data:image/png;base64,AAAA"]))
    );
    assert_eq!(
        prepared.get("mask_url"),
        Some(&Value::String("data:image/png;base64,BBBB".to_owned()))
    );
    assert_eq!(
        prepared
            .as_object()
            .expect("an object")
            .keys()
            .collect::<Vec<_>>(),
        vec!["image_urls", "mask_url", "model", "prompt"],
        "未声明的名字一个都不留：{prepared}"
    );
}

/// 合同说必填的字段必须给出；缺了就是调用方的参数错（400），与选路无关。
#[test]
fn missing_required_parameters_are_rejected() {
    let request = image_request(serde_json::json!({}));
    let error = contract_parameter_face(&request, &offering().capability_schema)
        .expect_err("a missing required parameter must fail");
    assert!(error.to_string().contains("prompt"), "{error}");

    // 参考图与遮罩已被受理侧按契约字段名取出，但合同把它们声明成必填时不能算缺：
    // 调用方**确实给了**这张图。
    let with_image_required = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt", "image"],
        "properties": {
            "model": {"const": "gpt-image-2"},
            "prompt": {"type": "string"},
            "image": {"type": "string"}
        }
    });
    let mut request = image_request(serde_json::json!({"prompt": "hello"}));
    assert!(
        contract_parameter_face(&request, &with_image_required)
            .expect_err("no image was given")
            .to_string()
            .contains("image")
    );
    request.reference_images = vec!["https://example.invalid/a.png".to_owned()];
    assert!(contract_parameter_face(&request, &with_image_required).is_ok());
}

/// 合同没为图片留位置时，带图请求是**参数错**——不是"合同外字段丢弃"（丢图等于悄悄生成一张
/// 没有参考图的图），也不是平台侧故障（供给面没问题，是这个模型不接图）。
#[test]
fn an_image_the_contract_never_declared_is_an_invalid_parameter() {
    let text_only = surface(serde_json::json!({
        "model": {"const": "gpt-image-2"},
        "prompt": {"type": "string"}
    }));
    let mut request = image_request(serde_json::json!({"prompt": "hello"}));
    request.reference_images = vec!["data:image/png;base64,AAAA".to_owned()];
    let error = contract_parameter_face(&request, &text_only)
        .expect_err("a contract without an image field cannot take a reference image");
    assert!(
        matches!(error, ApplicationError::InvalidParameter(_)),
        "带图请求必须说成参数错：{error}"
    );
    assert!(error.to_string().contains("reference image"), "{error}");

    // 遮罩同理：合同留了参考图的位置、没留遮罩的位置，带遮罩的请求还是参数错。
    let no_mask = surface(serde_json::json!({
        "model": {"const": "gpt-image-2"},
        "prompt": {"type": "string"},
        "image": {"type": "string"}
    }));
    let mut request = image_request(serde_json::json!({"prompt": "hello"}));
    request.reference_images = vec!["data:image/png;base64,AAAA".to_owned()];
    request.mask = Some("data:image/png;base64,BBBB".to_owned());
    let error = contract_parameter_face(&request, &no_mask)
        .expect_err("a contract without a mask field cannot take a mask");
    assert!(
        matches!(error, ApplicationError::InvalidParameter(_)),
        "{error}"
    );
    assert!(error.to_string().contains("mask"), "{error}");

    // 合同声明了其中**任意一个**同义字段就算留了位置：`image` 与 `image_urls` 是一回事。
    for name in ["image", "image_urls"] {
        let contract = surface(serde_json::json!({
            "model": {"const": "gpt-image-2"},
            "prompt": {"type": "string"},
            name: {"type": "string"}
        }));
        let mut request = image_request(serde_json::json!({"prompt": "hello"}));
        request.reference_images = vec!["https://example.invalid/a.png".to_owned()];
        assert!(
            contract_parameter_face(&request, &contract).is_ok(),
            "合同声明了 {name} 就该收下这张图"
        );
    }

    // 请求没带图：合同里有没有图片字段都不影响。
    let request = image_request(serde_json::json!({"prompt": "hello"}));
    assert!(contract_parameter_face(&request, &text_only).is_ok());
}

#[test]
fn places_reference_images_on_the_candidates_own_parameter() {
    let mut request = image_request(serde_json::json!({"prompt": "hello"}));
    request.reference_images = vec!["https://example.invalid/a.png".to_owned()];
    let vendor = offering();
    let face = contract_face(&request, &vendor);
    let prepared = prepare_carrier_parameters(&face, &request, &vendor).expect("images are placed");
    assert_eq!(
        prepared.get("image"),
        Some(&Value::String("https://example.invalid/a.png".to_owned()))
    );
}

#[test]
fn places_images_into_the_vendors_own_array_parameter() {
    // 调用方只给参考图/遮罩；装到 `image_urls` / `mask_url` 是平台按候选声明做的映射。
    let mut vendor = offering();
    vendor.carrier_schema = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": {
            "model": {"const": "gpt-image-2"},
            "prompt": {"type": "string", "minLength": 1},
            "image_urls": {"type": "array", "items": {"type": "string"}, "minItems": 1},
            "mask_url": {"type": "string"}
        }
    });
    let mut request = image_request(serde_json::json!({"prompt": "hello"}));
    request.reference_images = vec!["data:image/png;base64,AAAA".to_owned()];
    request.mask = Some("data:image/png;base64,BBBB".to_owned());
    // 合同（`offering()` 的那一份）把参考图与遮罩声明成 `image` / `mask`，
    // 而这条承载面把同一件事声明成 `image_urls` / `mask_url`：图片按**承载面**的名字落。
    let face = contract_face(&request, &vendor);
    let prepared = prepare_carrier_parameters(&face, &request, &vendor).expect("images are placed");
    assert_eq!(
        prepared.get("image_urls"),
        Some(&serde_json::json!(["data:image/png;base64,AAAA"]))
    );
    assert_eq!(
        prepared.get("mask_url"),
        Some(&Value::String("data:image/png;base64,BBBB".to_owned()))
    );

    // 承载面里没有装参考图的参数：这条供给表达不了，直接不合格（不静默丢图）。
    let mut text_only = offering();
    text_only.capability_schema = vendor.capability_schema.clone();
    text_only.carrier_schema = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": {
            "model": {"const": "gpt-image-2"},
            "prompt": {"type": "string", "minLength": 1}
        }
    });
    let face = contract_face(&request, &text_only);
    assert!(prepare_carrier_parameters(&face, &request, &text_only).is_err());
}

/// 显式默认值：调用方没给、映射声明了、且合同与承载面都声明了这个字段 → 注入。
///
/// 注入发生在组装参数面的最后一步，所以它会跟着 Job 落库、并出现在发给上游的报文里——
/// 渠道自己那套默认值（例如上游把水印默认打开）因此再也用不上。
#[test]
fn explicit_defaults_fill_the_fields_the_caller_left_out() {
    let mut vendor = offering();
    vendor.capability_schema = surface(serde_json::json!({
        "model": {"const": "gpt-image-2"},
        "prompt": {"type": "string"},
        "watermark": {"type": "boolean"}
    }));
    vendor.carrier_schema = vendor.capability_schema.clone();
    vendor.parameter_mapping = serde_json::json!({"defaults": {"watermark": false}});

    // 调用方没给：注入默认值。
    let request = image_request(serde_json::json!({"prompt": "hello"}));
    let face = contract_face(&request, &vendor);
    let prepared = prepare_carrier_parameters(&face, &request, &vendor).expect("carried");
    assert_eq!(
        prepared.get("watermark"),
        Some(&serde_json::json!(false)),
        "调用方没给的字段该由平台定，而不是由渠道的默认值定：{prepared}"
    );

    // 调用方给了：用调用方的值，一个字都不改。
    let request = image_request(serde_json::json!({"prompt": "hello", "watermark": true}));
    let face = contract_face(&request, &vendor);
    let prepared = prepare_carrier_parameters(&face, &request, &vendor).expect("carried");
    assert_eq!(prepared.get("watermark"), Some(&serde_json::json!(true)));

    // 承载面承载不了这个字段：不注入（发出去只会得到上游自己的一套解释）。
    vendor.carrier_schema = surface(serde_json::json!({
        "model": {"const": "gpt-image-2"},
        "prompt": {"type": "string"}
    }));
    let request = image_request(serde_json::json!({"prompt": "hello"}));
    let face = contract_face(&request, &vendor);
    let prepared = prepare_carrier_parameters(&face, &request, &vendor).expect("carried");
    assert!(prepared.get("watermark").is_none(), "{prepared}");
}

/// 改名：合同字段承载面承载不了、但改名把它落到一个承载面声明的名字上时，这条供给照样承载得了
/// 这次请求；组装出来的参数面里是**线上那个名字**，Job 里存的也是线上形态。
#[test]
fn a_renamed_parameter_is_written_under_the_wire_name() {
    let mut vendor = offering();
    vendor.capability_schema = surface(serde_json::json!({
        "model": {"const": "gpt-image-2"},
        "prompt": {"type": "string"},
        "size": {"type": "string"}
    }));
    // 承载面声明的是线上字段名：这条供给线上叫 `resolution`。
    vendor.carrier_schema = surface(serde_json::json!({
        "model": {"const": "gpt-image-2"},
        "prompt": {"type": "string"},
        "resolution": {"type": "string"}
    }));
    vendor.parameter_mapping = serde_json::json!({"rename": {"size": "resolution"}});

    let request = image_request(serde_json::json!({"prompt": "hello", "size": "1:1"}));
    let face = contract_face(&request, &vendor);
    let prepared = prepare_carrier_parameters(&face, &request, &vendor)
        .expect("the rename bridges size to resolution");
    assert_eq!(
        prepared.get("resolution"),
        Some(&serde_json::json!("1:1")),
        "线上那个名字才是要发的东西：{prepared}"
    );
    assert!(prepared.get("size").is_none(), "{prepared}");

    // 请求**用到**的字段承载不了（承载面没有它、改名也没接它）：这条候选不合格。
    vendor.parameter_mapping = serde_json::json!({});
    let request = image_request(serde_json::json!({"prompt": "hello", "size": "1:1"}));
    let face = contract_face(&request, &vendor);
    let reason = prepare_carrier_parameters(&face, &request, &vendor)
        .expect_err("without the rename the carrier cannot carry size");
    assert!(reason.contains("size"), "{reason}");

    // 承载面自己声明了这个名字：改名表对它不生效，名字原样上行。
    vendor.parameter_mapping = serde_json::json!({"rename": {"size": "resolution"}});
    vendor.carrier_schema = surface(serde_json::json!({
        "model": {"const": "gpt-image-2"},
        "prompt": {"type": "string"},
        "size": {"type": "string"},
        "resolution": {"type": "string"}
    }));
    let request = image_request(serde_json::json!({"prompt": "hello", "size": "1:1"}));
    let face = contract_face(&request, &vendor);
    let prepared = prepare_carrier_parameters(&face, &request, &vendor).expect("carried");
    assert_eq!(prepared.get("size"), Some(&serde_json::json!("1:1")));
    assert!(prepared.get("resolution").is_none(), "{prepared}");
}

/// `auto`（由模型自己决定）只原样透传：声明了尺寸换算的供给收不了它，纯透传的供给照常带上它。
///
/// 换算声明说的是"把合同的尺寸变成这条供给要的那一型"，而 `auto` 不是一个可换算的尺寸——替它
/// 算一个比例就是把模型的决定权拿走了。这条候选因此落选、换下一条；一条都收不了时是平台侧
/// 供给问题，不是调用方的参数错。
#[test]
fn auto_is_passed_through_and_a_size_converting_offering_cannot_take_it() {
    let contract = surface(serde_json::json!({
        "model": {"const": "gpt-image-2"},
        "prompt": {"type": "string"},
        "size": {"type": "string"},
        "resolution": {"type": "string"}
    }));
    // 优先级 0 的候选声明了尺寸换算（它要像素）；优先级 1 的候选把尺寸原样承载。
    let mut converting = offering();
    converting.capability_schema = contract.clone();
    converting.carrier_schema = surface(serde_json::json!({
        "model": {"const": "gpt-image-2"},
        "prompt": {"type": "string"},
        "size": {"type": "string"}
    }));
    converting.parameter_mapping = serde_json::json!({
        "size": {
            "source": ["size", "resolution"],
            "target": "size",
            "form": "pixels",
            "profile": {"2K": {"2:3": "1664x2496"}}
        }
    });
    let mut pass_through = offering();
    pass_through.capability_schema = contract.clone();
    pass_through.carrier_schema = contract.clone();
    pass_through.offering_id = OfferingId::new();

    let request = image_request(serde_json::json!({"prompt": "hello", "size": "auto"}));
    let face = contract_face(&request, &converting);
    // 声明了换算的供给：明确落选，原因写清 `auto` 只能原样透传、不能换算。
    let reason = prepare_carrier_parameters(&face, &request, &converting)
        .expect_err("a size-converting offering cannot take auto");
    assert!(
        reason.contains("auto") && reason.contains("cannot be converted"),
        "{reason}"
    );
    // 纯透传的供给：`auto` 原样留在参数面上。
    let prepared = prepare_carrier_parameters(&face, &request, &pass_through)
        .expect("the pass-through offering carries auto as it is");
    assert_eq!(
        prepared.get("size"),
        Some(&serde_json::json!("auto")),
        "{prepared}"
    );

    // 有别的候选就落过去：判定记录写明优先级 0 为什么落选。
    let branch = request.branch().expect("prompt only");
    let (chosen, parameters, decision) = select_candidate(
        &request,
        branch,
        &[candidate_of(&converting, 0), candidate_of(&pass_through, 1)],
        None,
    )
    .expect("the pass-through candidate carries auto");
    assert_eq!(chosen.offering_id, pass_through.offering_id);
    assert_eq!(parameters.get("size"), Some(&serde_json::json!("auto")));
    assert!(!decision.considered[0].eligible);
    assert!(
            decision.considered[0]
                .skip_reason
                .as_deref()
                .is_some_and(
                    |reason| reason.contains("auto") && reason.contains("cannot be converted")
                ),
            "落选原因必须写明 `auto` 不能换算：{:?}",
            decision.considered[0].skip_reason
        );
    assert!(decision.considered[1].eligible);

    // 一条都收不了 `auto`：不是参数错，是平台侧供给问题。
    let error = select_candidate(&request, branch, &[candidate_of(&converting, 0)], None)
        .expect_err("no candidate can take auto");
    assert!(
        matches!(error, ApplicationError::NoEligibleOffering(_)),
        "{error}"
    );
}

/// 取值映射：组装期把合同取值换成线上取值；表里没有这个取值 → 这条候选**不合格**（不猜、
/// 不透传原值），原因写进路由判定记录，有别的候选就落过去。
#[test]
fn an_enum_map_replaces_the_value_and_an_unmapped_one_skips_the_candidate() {
    let contract = surface(serde_json::json!({
        "model": {"const": "gpt-image-2"},
        "prompt": {"type": "string"},
        "quality": {"type": "string"}
    }));
    // 优先级 0 的候选只会把 `high` 映射出去；优先级 1 的候选原样承载取值。
    let mut mapped = offering();
    mapped.capability_schema = contract.clone();
    mapped.carrier_schema = contract.clone();
    mapped.parameter_mapping = serde_json::json!({"enum_map": {"quality": {"high": "xhigh"}}});
    let mut wide = offering();
    wide.capability_schema = contract.clone();
    wide.carrier_schema = contract.clone();
    wide.offering_id = OfferingId::new();

    // 表里有这个取值：换成线上取值，报文里就是映射后的那个。
    let request = image_request(serde_json::json!({"prompt": "hello", "quality": "high"}));
    let face = contract_face(&request, &mapped);
    let prepared = prepare_carrier_parameters(&face, &request, &mapped).expect("mapped");
    assert_eq!(
        prepared.get("quality"),
        Some(&serde_json::json!("xhigh")),
        "线上那个取值才是要发的东西：{prepared}"
    );

    // 表里没有这个取值：这条候选不合格，换下一条；一条都承载不了时是平台侧供给问题。
    let request = image_request(serde_json::json!({"prompt": "hello", "quality": "low"}));
    let branch = request.branch().expect("prompt only");
    let (chosen, parameters, decision) = select_candidate(
        &request,
        branch,
        &[candidate_of(&mapped, 0), candidate_of(&wide, 1)],
        None,
    )
    .expect("the second candidate carries the value as it is");
    assert_eq!(chosen.offering_id, wide.offering_id);
    assert_eq!(parameters.get("quality"), Some(&serde_json::json!("low")));
    assert!(!decision.considered[0].eligible);
    assert!(
        decision.considered[0]
            .skip_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("quality")),
        "落选原因必须写明是哪个字段的取值映射不了：{:?}",
        decision.considered[0].skip_reason
    );
    let error = select_candidate(&request, branch, &[candidate_of(&mapped, 0)], None)
        .expect_err("no candidate can map this value");
    assert!(
        matches!(error, ApplicationError::NoEligibleOffering(_)),
        "{error}"
    );

    // 调用方没用到这个字段：映射表无事可做，照常受理。
    let request = image_request(serde_json::json!({"prompt": "hello"}));
    let face = contract_face(&request, &mapped);
    assert!(prepare_carrier_parameters(&face, &request, &mapped).is_ok());
}

/// 承载面**自己声明的必填字段**也得在场：供给说了"这次请求必须带上它"，平台不替它省。
///
/// 与"请求用到的字段"是两件事：这是承载面**要求**的字段，不是调用方用到的字段。判它的时机
/// 也重要——要等图落到承载面的名字上、默认值注入之后，否则会把跑得通的候选误判成不合格。
#[test]
fn a_carrier_required_field_the_request_never_provides_makes_the_candidate_ineligible() {
    let mut vendor = offering();
    vendor.capability_schema = surface(serde_json::json!({
        "model": {"const": "gpt-image-2"},
        "prompt": {"type": "string"},
        "watermark": {"type": "boolean"}
    }));
    // 承载面把 `watermark` 声明成必填（在合同里它只是可选项）。
    vendor.carrier_schema = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt", "watermark"],
        "properties": {
            "model": {"const": "gpt-image-2"},
            "prompt": {"type": "string"},
            "watermark": {"type": "boolean"}
        }
    });
    let request = image_request(serde_json::json!({"prompt": "hello"}));
    let face = contract_face(&request, &vendor);
    let reason = prepare_carrier_parameters(&face, &request, &vendor)
        .expect_err("the carrier requires watermark");
    assert!(reason.contains("watermark"), "{reason}");

    // 映射给了默认值：承载面要的字段被补上，这条候选就合格了。
    vendor.parameter_mapping = serde_json::json!({"defaults": {"watermark": false}});
    let prepared = prepare_carrier_parameters(&face, &request, &vendor)
        .expect("the default fills the required field");
    assert_eq!(prepared.get("watermark"), Some(&serde_json::json!(false)));

    // 参考图同理：承载面要的参考图字段由平台装载的图补上。
    let mut vendor = offering();
    vendor.capability_schema = surface(serde_json::json!({
        "model": {"const": "gpt-image-2"},
        "prompt": {"type": "string"},
        "image": {"type": "string"}
    }));
    vendor.carrier_schema = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt", "image"],
        "properties": {
            "model": {"const": "gpt-image-2"},
            "prompt": {"type": "string"},
            "image": {"type": "string"}
        }
    });
    let mut request = image_request(serde_json::json!({"prompt": "hello"}));
    let face = contract_face(&request, &vendor);
    assert!(prepare_carrier_parameters(&face, &request, &vendor).is_err());
    request.reference_images = vec!["https://example.invalid/a.png".to_owned()];
    let face = contract_face(&request, &vendor);
    let prepared = prepare_carrier_parameters(&face, &request, &vendor)
        .expect("the placed image fills the required field");
    assert_eq!(
        prepared.get("image"),
        Some(&Value::String("https://example.invalid/a.png".to_owned()))
    );
}
