use super::*;

#[test]
fn the_declared_maximum_comes_from_the_contracts_n_property() {
    let declared = declared_output_images(
        "gpt-image-2.5-flare",
        &serde_json::json!({
            "type": "object",
            "properties": {"n": {"type": "integer", "minimum": 1, "maximum": 10, "default": 1}}
        }),
    )
    .expect("the contract declares n");
    assert_eq!(declared.maximum, 10);
    assert_eq!(declared.gateway_model, "gpt-image-2.5-flare");
}

/// 合同没声明 `n` 的几种写法都算"收不到这个参数"：缺字段、`n` 不是对象、没写 `maximum`。
#[test]
fn a_contract_without_an_n_maximum_declares_nothing() {
    for capability_schema in [
        serde_json::json!({"properties": {"prompt": {"type": "string"}}}),
        serde_json::json!({"properties": {"n": {"type": "integer"}}}),
        serde_json::json!({"properties": {"n": "integer"}}),
        serde_json::json!({"type": "object"}),
        serde_json::json!(null),
    ] {
        assert!(
            declared_output_images("model", &capability_schema).is_none(),
            "{capability_schema} must not declare an output image count"
        );
    }
}

/// `maximum: 0` 不是"要 0 张"这种请求形状，按没声明算：链上因此按 1 张比，而不是按 0（按 0 会让
/// "上限 ≥ 算出来的值"恒真，等于把这条链关掉）。
#[test]
fn a_zero_maximum_is_not_a_declaration() {
    assert!(
        declared_output_images(
            "model",
            &serde_json::json!({"properties": {"n": {"type": "integer", "maximum": 0}}})
        )
        .is_none()
    );
}

/// 与**输入参考图**上限划清：承载面声明能收 16 张参考图，与这一次能要几张图出来无关，不能被当作
/// 输出张数读走。
#[test]
fn an_input_reference_image_limit_is_not_an_output_count() {
    let carrier_schema = serde_json::json!({
        "properties": {
            "prompt": {"type": "string"},
            "image": {"type": "array", "maxItems": 16}
        }
    });
    assert!(
        declared_output_images("model", &carrier_schema).is_none(),
        "a carrier that accepts 16 reference images declares nothing about n"
    );
}
