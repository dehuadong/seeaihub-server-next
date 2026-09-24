use super::*;

/// 合同为 `n` 声明了上下界 ⇒ 越界的请求在受理前被拒，而且是**调用方的参数问题**（400），不是
/// 平台侧故障：值是他给的，改法也在他那一侧。恰好等于两端允许。
#[test]
fn a_requested_image_count_outside_the_contracts_bounds_is_rejected_as_a_parameter_problem() {
    let mut vendor = offering();
    vendor.capability_schema = surface(serde_json::json!({
        "model": {"const": "gpt-image-2"},
        "prompt": {"type": "string"},
        "n": {"type": "integer", "minimum": 1, "maximum": 4}
    }));

    let request = image_request(serde_json::json!({"prompt": "hello", "n": 5}));
    let error = contract_parameter_face(&request, &vendor.capability_schema)
        .expect_err("5 is beyond the declared maximum of 4");
    assert!(
        matches!(error, ApplicationError::InvalidParameter(_)),
        "越界是参数问题，不是平台侧故障：{error:?}"
    );
    assert!(error.to_string().contains("at most 4"), "{error}");

    let request = image_request(serde_json::json!({"prompt": "hello", "n": 0}));
    let error = contract_parameter_face(&request, &vendor.capability_schema)
        .expect_err("0 is below the declared minimum of 1");
    assert!(error.to_string().contains("at least 1"), "{error}");

    for boundary in [1, 4] {
        let request = image_request(serde_json::json!({"prompt": "hello", "n": boundary}));
        assert!(
            contract_parameter_face(&request, &vendor.capability_schema).is_ok(),
            "恰好落在界上的 {boundary} 张必须放行"
        );
    }
}

/// 合同声明 `n` 是整数 ⇒ 非整数取值被拒。`3.0` 算整数：JSON Schema 的 `integer` 就是"没有小数
/// 部分"，按类型严格拒绝会把一个合法取值判成错。
#[test]
fn a_requested_image_count_that_is_not_an_integer_is_rejected() {
    let mut vendor = offering();
    vendor.capability_schema = surface(serde_json::json!({
        "model": {"const": "gpt-image-2"},
        "prompt": {"type": "string"},
        "n": {"type": "integer", "minimum": 1, "maximum": 10}
    }));

    for value in [serde_json::json!("3"), serde_json::json!(2.5)] {
        let request = image_request(serde_json::json!({"prompt": "hello", "n": value}));
        let error = contract_parameter_face(&request, &vendor.capability_schema)
            .expect_err("a non-integer n must be rejected");
        assert!(error.to_string().contains("must be an integer"), "{error}");
    }

    let request = image_request(serde_json::json!({"prompt": "hello", "n": 3.0}));
    assert!(
        contract_parameter_face(&request, &vendor.capability_schema).is_ok(),
        "3.0 是整数：按 JSON Schema 的口径它没有小数部分"
    );
}

/// 合同**没为 `n` 声明界**时按今天的口径走：声明了名字但没声明上下界 ⇒ 取值照原样留下；根本没声明
/// 这个名字 ⇒ 它按"未声明字段"被丢掉（既有的名字过滤）。别的参数的取值一律不判。
#[test]
fn a_contract_without_declared_bounds_leaves_the_value_alone() {
    let mut vendor = offering();
    vendor.capability_schema = surface(serde_json::json!({
        "model": {"const": "gpt-image-2"},
        "prompt": {"type": "string"},
        "n": {"type": "integer"},
        "quality": {"enum": ["low", "high"]}
    }));
    let request = image_request(serde_json::json!({"prompt": "hello", "n": 100, "quality": 42}));
    let face = contract_parameter_face(&request, &vendor.capability_schema)
        .expect("a contract that declares no bounds does not constrain the value");
    assert_eq!(face.get("n"), Some(&serde_json::json!(100)));
    assert_eq!(
        face.get("quality"),
        Some(&serde_json::json!(42)),
        "别的参数的取值不判：`42` 不在它声明的 enum 里，也照原样留下"
    );

    let mut without_n = offering();
    without_n.capability_schema = surface(serde_json::json!({
        "model": {"const": "gpt-image-2"},
        "prompt": {"type": "string"}
    }));
    let request = image_request(serde_json::json!({"prompt": "hello", "n": 100}));
    let face = contract_parameter_face(&request, &without_n.capability_schema)
        .expect("a contract that never declares n has nothing to constrain");
    assert!(
        !face.contains_key("n"),
        "合同没声明 `n`：它按未声明字段被丢掉（既有口径）：{face:?}"
    );
}
