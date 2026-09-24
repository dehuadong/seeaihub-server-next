use super::*;

#[test]
fn restriction_declaring_an_undeclared_branch_is_rejected() {
    // 反例一：Profile 只声明 prompt，却把 image_conditioned 放进允许分支。
    // 这是"限制放宽"——供货方声明了 Profile 自己都没声明的东西。
    let offering = offering_with(
        serde_json::json!({"model": {"const": "m"}, "prompt": {"type": "string"}}),
        serde_json::json!({"allowed_branches": ["prompt_only", "image_conditioned"]}),
    );
    let error = validate_restrictions_within_profile(&offering)
        .expect_err("an undeclared branch must be rejected");
    assert!(error.to_string().contains("does not declare"), "{error}");
}

#[test]
fn restriction_allowing_more_images_than_declared_is_rejected() {
    // 反例二：Profile 只声明一张参考图，限制却允许 4 张。
    let offering = offering_with(
        serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "images": {"type": "array", "maxItems": 1}
        }),
        serde_json::json!({"allowed_branches": ["image_conditioned"], "max_reference_images": 4}),
    );
    let error = validate_restrictions_within_profile(&offering)
        .expect_err("more images than declared must be rejected");
    assert!(error.to_string().contains("declares at most"), "{error}");
}

#[test]
fn restriction_staying_within_the_profile_is_accepted() {
    // 正例：收窄（声明 image_conditioned/masked 且真的有对应字段；收图数不超过声明）。
    let offering = offering_with(
        serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "images": {"type": "array", "maxItems": 4},
            "mask": {"type": "string"}
        }),
        serde_json::json!({
            "allowed_branches": ["image_conditioned", "masked"],
            "max_reference_images": 2
        }),
    );
    assert!(validate_restrictions_within_profile(&offering).is_ok());
}

#[test]
fn restriction_cannot_allow_edits_when_only_one_image_is_supported() {
    // 收窄的另一面：Profile 只有单图字段时，`max_reference_images: 1` 合法、`2` 不合法。
    let single = offering_with(
        serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "image": {"type": "string"}
        }),
        serde_json::json!({"allowed_branches": ["image_conditioned"], "max_reference_images": 1}),
    );
    assert!(validate_restrictions_within_profile(&single).is_ok());
    let widened = offering_with(
        serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "image": {"type": "string"}
        }),
        serde_json::json!({"allowed_branches": ["image_conditioned"], "max_reference_images": 2}),
    );
    assert!(validate_restrictions_within_profile(&widened).is_err());
}

#[test]
fn restriction_recognises_image_and_mask_parameters_by_name() {
    // 参考图/遮罩参数按渠道各自的原生名给出：APIMart 的参考图字段叫 `image_urls`、
    // 遮罩叫 `mask_url`。判定办法是名字约定——参考图以 `image` 开头，遮罩含 `mask`。
    let vendor_names = offering_with(
        serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "image_urls": {"type": "array", "items": {"type": "string"}, "maxItems": 16},
            "mask_url": {"type": "string"}
        }),
        serde_json::json!({
            "allowed_branches": ["prompt_only", "image_conditioned", "masked"],
            "max_reference_images": 16
        }),
    );
    assert!(validate_restrictions_within_profile(&vendor_names).is_ok());
    // 声明了遮罩却没有任何参考图参数：`masked` 不成立（遮罩不能脱离参考图）。
    let mask_without_image = offering_with(
        serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "mask_url": {"type": "string"}
        }),
        serde_json::json!({"allowed_branches": ["masked"]}),
    );
    assert!(validate_restrictions_within_profile(&mask_without_image).is_err());
    // 数组形式没写 `maxItems` ＝ Profile 没有承诺上限，不能据它接受 `max_reference_images`。
    let unbounded = offering_with(
        serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "image_urls": {"type": "array", "items": {"type": "string"}}
        }),
        serde_json::json!({"allowed_branches": ["image_conditioned"], "max_reference_images": 16}),
    );
    assert!(validate_restrictions_within_profile(&unbounded).is_err());
}

/// 尺寸换算声明也是**发布数据**：源字段在合同里、目标字段在承载面里、档案成形状，才准发布。
///
/// 写歪的声明不按"没有声明"处理——那会让换算静默不发生，调用方与运营都以为它发生了。
#[test]
fn the_size_mapping_must_be_something_this_offering_can_execute() {
    let contract = surface(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "size": {"type": "string"},
        "resolution": {"type": "string"}
    }));
    let carrier = serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "size": {"type": "string"}
    });
    let size_mapping = serde_json::json!({
        "size": {
            "source": ["size", "resolution"],
            "target": "size",
            "form": "pixels",
            "profile": {"2K": {"2:3": "1664x2496"}}
        }
    });
    let mut offering = offering_with(carrier.clone(), serde_json::json!({}));
    offering.parameter_mapping = size_mapping.clone();
    assert!(
        validate_size_mapping(&contract, &offering).is_ok(),
        "源字段在合同里、目标字段在承载面里、档案成形状：这条声明可以发布"
    );

    // 源字段不在合同里：客户端提交不了它，换算没有输入。
    let mut bad = offering.clone();
    bad.parameter_mapping = serde_json::json!({
        "size": {"source": ["aspect_ratio"], "target": "size", "form": "pixels"}
    });
    let error = validate_size_mapping(&contract, &bad).expect_err("source outside the contract");
    assert!(error.to_string().contains("aspect_ratio"), "{error}");

    // 目标字段不在承载面里：换算出来的值发不出去。
    let mut bad = offering.clone();
    bad.carrier_schema = surface(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"}
    }));
    let error = validate_size_mapping(&contract, &bad).expect_err("target outside the carrier");
    assert!(error.to_string().contains("writes size"), "{error}");

    // 档案不成形状：档位/比例/像素三样没各就各位。
    let mut bad = offering.clone();
    bad.parameter_mapping = serde_json::json!({
        "size": {"source": ["size"], "target": "size", "form": "pixels", "profile": []}
    });
    let error = validate_size_mapping(&contract, &bad).expect_err("a profile must be a table");
    assert!(
        error.to_string().contains("parameter_mapping.size"),
        "{error}"
    );

    // 没声明尺寸换算：这条供给不做换算，照常发布。
    let mut plain = offering.clone();
    plain.parameter_mapping = serde_json::json!({});
    assert!(validate_size_mapping(&contract, &plain).is_ok());
}

/// R1：承载面声明了合同里没有的字段 ⇒ 供给凭空多出调用方可提交的参数，拒绝。
#[test]
fn carrier_field_the_contract_does_not_declare_is_rejected() {
    let contract = surface(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"}
    }));
    let carrier = surface(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "quality": {"type": "string"}
    }));
    let error = validate_carrier_within_contract(&contract, &carrier, &serde_json::json!({}))
        .expect_err("a field outside the contract must be rejected");
    assert!(
        error
            .to_string()
            .contains("which the vendor model contract does not"),
        "{error}"
    );
}

/// R2：承载面声明了这个 Driver 写不出去的字段名 ⇒ 声明了发不出去，拒绝。
#[test]
fn carrier_field_the_driver_cannot_write_is_rejected() {
    let mut offering = offering_with(
        serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "resolution": {"type": "string"}
        }),
        serde_json::json!({}),
    );
    offering.carrier_schema = surface(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "resolution": {"type": "string"}
    }));
    let error = validate_adapter_compatibility(&offering, &descriptor())
        .expect_err("a field the driver cannot write must be rejected");
    assert!(
        error
            .to_string()
            .contains("cannot write parameter resolution"),
        "{error}"
    );
}

/// 承载面落在合同与 Driver 之内时通过：两个边界各判一次，缺一不可。
#[test]
fn carrier_within_the_contract_and_the_driver_is_accepted() {
    let contract = surface(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "image": {"type": "string"},
        "quality": {"type": "string"}
    }));
    let mut offering = offering_with(
        serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "image": {"type": "string"},
            "quality": {"type": "string"}
        }),
        serde_json::json!({"allowed_branches": ["prompt_only"], "max_reference_images": 0}),
    );
    offering.carrier_schema = contract.clone();
    assert!(
        validate_carrier_within_contract(
            &contract,
            &offering.carrier_schema,
            &serde_json::json!({})
        )
        .is_ok()
    );
    assert!(validate_adapter_compatibility(&offering, &descriptor()).is_ok());
}

/// R1 的另一面：承载面声明的是**线上字段名**，合同字段经改名落到它身上时，它同样是"从合同来的"，
/// 不是供给凭空多出来的参数；尺寸换算的目标字段同理。
#[test]
fn a_carrier_field_reachable_from_the_contract_is_accepted() {
    let contract = surface(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "size": {"type": "string"}
    }));
    // 线上叫 `resolution`：合同里没有这个名字，但改名把它从合同的 `size` 接了过来。
    let carrier = surface(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "resolution": {"type": "string"}
    }));
    let mapping = serde_json::json!({"rename": {"size": "resolution"}});
    assert!(
        validate_carrier_within_contract(&contract, &carrier, &mapping).is_ok(),
        "改名接过来的线上名字不算凭空多出"
    );
    // 没有改名表时同一个承载面就是凭空多出：`resolution` 在合同里没有、也没人接它。
    let error = validate_carrier_within_contract(&contract, &carrier, &serde_json::json!({}))
        .expect_err("without the bridge it is a field the contract does not declare");
    assert!(error.to_string().contains("resolution"), "{error}");
    // 改名接的是**合同里没有的**字段：那条线上名字仍然没有来源，照样拒绝。
    let dangling = serde_json::json!({"rename": {"aspect_ratio": "resolution"}});
    assert!(validate_carrier_within_contract(&contract, &carrier, &dangling).is_err());

    // 尺寸换算的目标字段同理：源字段在合同里，算出来的值写在这个线上名字上。
    let size_contract = surface(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "size": {"type": "string"},
        "resolution": {"type": "string"}
    }));
    let pixel_carrier = surface(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "pixels": {"type": "string"}
    }));
    let size_mapping = serde_json::json!({
        "size": {
            "source": ["size", "resolution"],
            "target": "pixels",
            "form": "pixels",
            "profile": {"2K": {"1:1": "2048x2048"}}
        }
    });
    assert!(
        validate_carrier_within_contract(&size_contract, &pixel_carrier, &size_mapping).is_ok(),
        "换算的目标字段是从合同的源字段来的"
    );
}

/// 改名表本身必须是这条供给真能做到的事：源在合同里、目标在承载面里，缺一就拒绝。
#[test]
fn a_rename_must_bridge_the_contract_and_the_carrier() {
    let contract = surface(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "size": {"type": "string"}
    }));
    let carrier = surface(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "resolution": {"type": "string"}
    }));
    let mut offering = offering_with(carrier.clone(), serde_json::json!({}));
    offering.carrier_schema = carrier.clone();
    offering.parameter_mapping = serde_json::json!({"rename": {"size": "resolution"}});
    assert!(
        validate_parameter_mapping(&contract, &offering).is_ok(),
        "源在合同里、目标在承载面里：这份改名声明可以发布"
    );

    // 源不在合同里：调用方提交不了这个名字，改名没有输入。
    offering.parameter_mapping = serde_json::json!({"rename": {"aspect_ratio": "resolution"}});
    let error = validate_parameter_mapping(&contract, &offering)
        .expect_err("a rename source outside the contract");
    assert!(error.to_string().contains("aspect_ratio"), "{error}");

    // 目标不在承载面里：改出来的名字发不出去，等于没改。
    offering.parameter_mapping = serde_json::json!({"rename": {"size": "pixels"}});
    let error = validate_parameter_mapping(&contract, &offering)
        .expect_err("a rename target outside the carrier");
    assert!(error.to_string().contains("pixels"), "{error}");

    // 声明得不成形状：不按"没有声明"处理，发布期就拒绝。
    offering.parameter_mapping = serde_json::json!({"rename": "size"});
    let error =
        validate_parameter_mapping(&contract, &offering).expect_err("a malformed rename table");
    assert!(
        error.to_string().contains("parameter_mapping.rename"),
        "{error}"
    );
}

/// 取值映射表的字段必须"合同里有、这条供给承载得了"，否则这张表永远不会被用到。
#[test]
fn an_enum_map_must_name_a_field_the_contract_and_the_carrier_share() {
    let contract = surface(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "quality": {"type": "string"}
    }));
    let mut offering = offering_with(contract.clone(), serde_json::json!({}));
    offering.carrier_schema = contract.clone();
    offering.parameter_mapping = serde_json::json!({"enum_map": {"quality": {"high": "xhigh"}}});
    assert!(validate_parameter_mapping(&contract, &offering).is_ok());

    // 承载面承载不了它：这张表在这条供给上永远不会生效。
    offering.carrier_schema = surface(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"}
    }));
    let error = validate_parameter_mapping(&contract, &offering)
        .expect_err("the carrier cannot carry quality");
    assert!(error.to_string().contains("enum_map"), "{error}");

    // 合同里没有它：调用方提交不了这个字段，表没有输入。
    offering.carrier_schema = contract.clone();
    offering.parameter_mapping = serde_json::json!({"enum_map": {"seed": {"1": "2"}}});
    let error = validate_parameter_mapping(&contract, &offering)
        .expect_err("the contract does not declare seed");
    assert!(error.to_string().contains("seed"), "{error}");
}

/// 显式默认值的每个键必须被这条供给承载：声明了一个发不出去的默认值，就是"声明了却发不出去"。
#[test]
fn a_default_the_offering_cannot_carry_is_rejected_at_publication() {
    let contract = surface(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "watermark": {"type": "boolean"}
    }));
    let mut offering = offering_with(contract.clone(), serde_json::json!({}));
    offering.carrier_schema = contract.clone();
    offering.parameter_mapping = serde_json::json!({"defaults": {"watermark": false}});
    assert!(validate_parameter_mapping(&contract, &offering).is_ok());

    // 承载面承载不了 `watermark`：这条默认值注入不进去，声明与行为分了家。
    offering.carrier_schema = surface(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"}
    }));
    let error = validate_parameter_mapping(&contract, &offering)
        .expect_err("a default outside what the carrier can carry");
    assert!(error.to_string().contains("defaults"), "{error}");

    // 合同里没有这个字段：调用方提交不了它，这条默认值同样永远不会生效。
    offering.carrier_schema = contract.clone();
    offering.parameter_mapping = serde_json::json!({"defaults": {"seed": 7}});
    let error = validate_parameter_mapping(&contract, &offering)
        .expect_err("a default outside the contract");
    assert!(error.to_string().contains("seed"), "{error}");

    // 经改名落到承载面声明的名字上：这条供给承载得了它，声明有效。
    offering.carrier_schema = surface(serde_json::json!({
        "model": {"const": "m"},
        "prompt": {"type": "string"},
        "xwatermark": {"type": "boolean"}
    }));
    offering.parameter_mapping = serde_json::json!({
        "rename": {"watermark": "xwatermark"},
        "defaults": {"watermark": false}
    });
    assert!(
        validate_parameter_mapping(&contract, &offering).is_ok(),
        "改名接过来的字段照样承载得了这条默认值"
    );
}
