use super::*;
use serde_json::json;

fn contract() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": {
            "model": {"const": "vendor-name"},
            "prompt": {"type": "string", "minLength": 1, "maxLength": 32000},
            "n": {"type": "integer", "minimum": 1, "maximum": 10, "default": 1}
        },
        "allOf": [
            {"if": {"required": ["mask"]}, "then": {"required": ["image"]}}
        ]
    })
}

fn material() -> Value {
    json!({
        "narrative": "# {{platform_name}}\n\n厂商 {{vendor_id}}，类型 {{model_type}}，修订 {{contract_revision}}。\n\n## 参数\n\n{{parameter_table}}\n\n[鉴权](../../authentication.md)\n",
        "fields": {
            "/properties/model": "平台目录返回的对客名。",
            "/properties/prompt": "提示词。",
            "/properties/n": "张数。",
            "/allOf/0": "遮罩必须同时给参考图。"
        }
    })
}

fn render(material: &Value) -> Result<String, ApplicationError> {
    let contract = contract();
    render_model_document(
        &contract,
        "platform-name",
        "OpenAI",
        "image",
        "r1",
        material,
    )
}

#[test]
fn renders_the_contract_and_the_public_links() {
    let body = render(&material()).expect("the material renders");
    assert!(body.contains("# platform-name"), "{body}");
    // 参数表里的型号身份是平台对客名：合同里冻结的厂商原生名不得出现在对客正文里。
    assert!(body.contains("固定 `platform-name`"), "{body}");
    assert!(!body.contains("vendor-name"), "{body}");
    assert!(body.contains("厂商 OpenAI，类型 image，修订 r1"), "{body}");
    // 参数表来自同版合同：字段、必填、限制与释义都在。
    assert!(body.contains("| `model` | 是 |"), "{body}");
    assert!(body.contains("| `prompt` | 是 |"), "{body}");
    assert!(body.contains("| `n` | 否 |"), "{body}");
    assert!(body.contains("最长 32000"), "{body}");
    assert!(body.contains("默认声明 1"), "{body}");
    assert!(body.contains("提示词。"), "{body}");
    // 组合约束以结构描述 + 素材原义出现。
    assert!(body.contains("组合约束"), "{body}");
    assert!(body.contains("遮罩必须同时给参考图。"), "{body}");
    // 公共文档链接转成同源地址。
    assert!(body.contains("/v1/docs/authentication.md"), "{body}");
    assert!(!body.contains("../../"), "{body}");
}

#[test]
fn rejects_materials_that_do_not_match_the_contract() {
    let mut missing = material();
    missing["fields"]
        .as_object_mut()
        .expect("fields")
        .remove("/properties/n");
    let error = render(&missing).expect_err("a missing definition is rejected");
    assert!(error.to_string().contains("/properties/n"), "{error}");

    let mut extra = material();
    extra["fields"]["/properties/seed"] = json!("合同没有的字段。");
    let error = render(&extra).expect_err("an extra definition is rejected");
    assert!(error.to_string().contains("/properties/seed"), "{error}");
}

#[test]
fn rejects_unresolved_placeholders_and_bad_links() {
    let mut unresolved = material();
    unresolved["narrative"] = json!("# {{platform_name}}\n\n{{parameter_table}}\n\n{{unknown}}\n");
    let error = render(&unresolved).expect_err("an unknown placeholder is rejected");
    assert!(error.to_string().contains("{{unknown}}"), "{error}");

    let mut no_table = material();
    no_table["narrative"] = json!("# {{platform_name}}\n");
    let error = render(&no_table).expect_err("a narrative without a table is rejected");
    assert!(error.to_string().contains("{{parameter_table}}"), "{error}");

    let mut internal = material();
    internal["narrative"] = json!(
        "# {{platform_name}}\n\n{{parameter_table}}\n\n[Spec](../../../docs/specs/0008-model-usage-documentation.md)\n"
    );
    let error = render(&internal).expect_err("an internal link is rejected");
    assert!(
        error
            .to_string()
            .contains("only the public usage documents"),
        "{error}"
    );
}

#[test]
fn rejects_a_body_over_the_limit() {
    let mut huge = material();
    huge["narrative"] = json!(format!(
        "# {{{{platform_name}}}}\n\n{{{{parameter_table}}}}\n\n{}\n",
        "x".repeat(MAX_MODEL_DOCUMENT_BYTES)
    ));
    let error = render(&huge).expect_err("an oversized document is rejected");
    assert!(error.to_string().contains("byte limit"), "{error}");
}
#[test]
fn renders_nested_limits_and_combination_values() {
    let contract = json!({
        "type": "object",
        "required": ["model", "prompt"],
        "properties": {
            "model": {"const": "vendor-name"},
            "prompt": {"type": "string", "description": "提示词正文。"},
            "image": {"type": "array", "items": {"type": "string"}, "minItems": 1},
            "size": {
                "type": "string",
                "anyOf": [{"const": "auto"}, {"pattern": "^[0-9]+x[0-9]+$"}],
                "description": "宽 x 高，或 auto。"
            }
        }
    });
    let material = json!({
        "narrative": "# {{platform_name}}\n\n{{parameter_table}}\n",
        "fields": {
            "/properties/model": "模型名。",
            "/properties/prompt": "提示词。",
            "/properties/image": "参考图。",
            "/properties/size": "尺寸。"
        }
    });
    let body = render_model_document(
        &contract,
        "platform-name",
        "OpenAI",
        "image",
        "r1",
        &material,
    )
    .expect("the material renders");
    // 数组元素类型、组合取值与合同里的自然语言描述都要读到。
    assert!(body.contains("元素 string"), "{body}");
    assert!(body.contains("至少满足一条"), "{body}");
    assert!(body.contains("固定 `auto`"), "{body}");
    assert!(body.contains("格式 ^[0-9]+x[0-9]+$"), "{body}");
    assert!(body.contains("提示词正文。"), "{body}");
    assert!(body.contains("宽 x 高，或 auto。"), "{body}");
}

#[test]
fn image_video_and_chat_documents_keep_their_identity() {
    let contract = contract();
    let render = |model_type: &str, vendor_id: &str| {
        render_model_document(
            &contract,
            "platform-name",
            vendor_id,
            model_type,
            "r1",
            &material(),
        )
        .expect("the material renders")
    };
    let image = render("image", "OpenAI");
    let video = render("video", "OtherVendor");
    let chat = render("chat", "OpenAI");
    assert!(
        image.contains("类型 image") && image.contains("厂商 OpenAI"),
        "{image}"
    );
    assert!(
        video.contains("类型 video") && video.contains("厂商 OtherVendor"),
        "{video}"
    );
    assert!(chat.contains("类型 chat"), "{chat}");
    assert_ne!(image, video, "不同类型/厂商的正文不得互相顶替");
}
