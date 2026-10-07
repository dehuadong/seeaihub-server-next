use super::*;
use serde_json::json;

/// 平台对客基址：正文里的链接与 `{{SEE_BASEURL}}` 都按它写成绝对地址。
const BASE_URL: &str = "http://api.test";

fn contract() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": {
            "model": {"const": "vendor-name"},
            "prompt": {"type": "string", "minLength": 1, "maxLength": 32000},
            "n": {"type": "integer", "minimum": 1, "maximum": 10, "default": 1},
            "image": {"type": "array", "items": {"type": "string"}, "minItems": 1},
            "mask": {"type": "string"}
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
            "/properties/image": "参考图。",
            "/properties/mask": "遮罩。",
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
        BASE_URL,
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
    // 公共文档链接统一成平台对客基址的绝对地址。
    assert!(
        body.contains("http://api.test/v1/docs/authentication.md"),
        "{body}"
    );
    assert!(!body.contains("../../"), "{body}");
}

#[test]
fn substitutes_the_platform_base_url_and_rejects_a_bad_one() {
    let mut with_placeholder = material();
    with_placeholder["narrative"] = json!(
        "# {{platform_name}}\n\n调用 {{SEE_BASEURL}}/v1/images/generations。\n\n{{parameter_table}}\n"
    );
    let body = render(&with_placeholder).expect("the material renders");
    assert!(
        body.contains("调用 http://api.test/v1/images/generations"),
        "{body}"
    );
    assert!(!body.contains(BASE_URL_PLACEHOLDER), "{body}");

    let contract = contract();
    let error = render_model_document(
        &contract,
        "platform-name",
        "OpenAI",
        "image",
        "r1",
        "api.test",
        &material(),
    )
    .expect_err("a base url without a scheme is rejected");
    assert!(error.to_string().contains("SEE_BASEURL"), "{error}");

    let error = render_model_document(
        &contract,
        "platform-name",
        "OpenAI",
        "image",
        "r1",
        "http://api.test/",
        &material(),
    )
    .expect_err("a base url with a trailing slash is rejected");
    assert!(error.to_string().contains("trailing slash"), "{error}");
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

/// 组合约束只能点合同自己声明的字段：点到别的名字时那条分支永远无法满足，渲染出来的结构描述还会
/// 教调用方填一个会被丢弃的字段（AIHubMix 改名那次在顶层合同上留下过 `images`）。
#[test]
fn rejects_combination_constraints_that_name_undeclared_fields() {
    let mut bad_then = contract();
    bad_then["allOf"][0]["then"]["required"] = json!(["images"]);
    let error = render_model_document(
        &bad_then,
        "platform-name",
        "OpenAI",
        "image",
        "r1",
        BASE_URL,
        &material(),
    )
    .expect_err("a clause naming an undeclared field is rejected");
    let message = error.to_string();
    assert!(message.contains("platform-name"), "{message}");
    assert!(message.contains("images"), "{message}");
    assert!(message.contains("/allOf/0/then/required/0"), "{message}");

    // `if` 那一侧同样判：条件点到一个没人能提供的名字时，那条约束永远是死条文。
    let mut bad_if = contract();
    bad_if["allOf"][0]["if"]["required"] = json!(["mask_url"]);
    bad_if["allOf"][0]["then"]["required"] = json!(["image"]);
    let error = render_model_document(
        &bad_if,
        "platform-name",
        "OpenAI",
        "image",
        "r1",
        BASE_URL,
        &material(),
    )
    .expect_err("an if-clause naming an undeclared field is rejected");
    assert!(error.to_string().contains("mask_url"), "{error}");
    assert!(
        error.to_string().contains("/allOf/0/if/required/0"),
        "{error}"
    );

    // 子 schema 各判各层：`size` 自己是个**封闭对象**，它的 `allOf` 点到它自己没声明的 `width`
    // （文档里那一行会把这条约束渲染出来，所以同样要拦）。
    let mut nested = contract();
    nested["properties"]["size"] = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {"auto": {"type": "boolean"}},
        "allOf": [{"if": {"required": ["auto"]}, "then": {"required": ["width"]}}]
    });
    let mut material_with_size = material();
    material_with_size["fields"]["/properties/size"] = json!("尺寸。");
    material_with_size["fields"]["/properties/size/properties/auto"] = json!("自动尺寸。");
    let error = render_model_document(
        &nested,
        "platform-name",
        "OpenAI",
        "image",
        "r1",
        BASE_URL,
        &material_with_size,
    )
    .expect_err("a nested clause naming an undeclared field is rejected");
    assert!(
        error
            .to_string()
            .contains("/properties/size/allOf/0/then/required/0"),
        "{error}"
    );

    // 不封闭的那一层不判：它可以带额外属性，`required` 点一个没声明的名字是合法的（由上游按自己的
    // schema 处置），`0005` §5 的尺寸形态就长这样。
    let mut open_nested = contract();
    open_nested["properties"]["size"] = json!({
        "anyOf": [
            {"const": "auto"},
            {"required": ["width"]}
        ]
    });
    let mut material_with_size = material();
    material_with_size["fields"]["/properties/size"] = json!("尺寸。");
    render_model_document(
        &open_nested,
        "platform-name",
        "OpenAI",
        "image",
        "r1",
        BASE_URL,
        &material_with_size,
    )
    .expect("a clause inside an open object is left to the upstream");
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
        BASE_URL,
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
            BASE_URL,
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
