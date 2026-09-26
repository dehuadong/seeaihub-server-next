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

/// 读取按**调用方给的名字**走：承载面把输出张数声明成别的线上名时，用那个名字读到的是同一条规则
/// 的同一个数（受理时按候选承载面收敛就靠它）。
#[test]
fn the_maximum_is_read_by_the_name_the_caller_asks_for() {
    let carrier_schema = serde_json::json!({
        "properties": {"num_images": {"type": "integer", "minimum": 1, "maximum": 4}}
    });
    assert_eq!(
        declared_output_image_maximum(&carrier_schema, "num_images"),
        Some(4)
    );
    assert_eq!(
        declared_output_image_maximum(&carrier_schema, "n"),
        None,
        "承载面没声明 `n` 这个名字：不能凭空读出一个上限"
    );
    // 这个读法不带取值语义：`maximum: 0` 原样读出来。按 0 当"没声明"是**超时链**的口径，
    // 由 `declared_output_images` 那一侧过滤（那条路按 0 比会让上限恒真）。
    assert_eq!(
        declared_output_image_maximum(
            &serde_json::json!({"properties": {"n": {"type": "integer", "maximum": 0}}}),
            "n"
        ),
        Some(0)
    );
}
