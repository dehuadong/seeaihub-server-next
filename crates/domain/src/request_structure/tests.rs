use super::*;

/// 贴住 wire 上限的 data URL 请求：16 MiB 里绝大部分是图片字符串那一份。
///
/// 这就是"已支持的输入形态"里的合法大请求；结构上限不得把它拒掉。
fn max_wire_image_body() -> Vec<u8> {
    let head =
        b"{\"model\":\"bounded-model\",\"prompt\":\"draw\",\"image\":\"data:image/png;base64,";
    let tail = b"\",\"n\":1}";
    let payload = SUPPORTED_REQUEST_WIRE_BYTES - head.len() - tail.len();
    let mut body = Vec::with_capacity(SUPPORTED_REQUEST_WIRE_BYTES);
    body.extend_from_slice(head);
    body.resize(body.len() + payload, b'A');
    body.extend_from_slice(tail);
    assert_eq!(body.len(), SUPPORTED_REQUEST_WIRE_BYTES);
    body
}

fn array_body(element: &[u8], wire: usize) -> Vec<u8> {
    let head = b"{\"model\":\"m\",\"x\":[";
    let tail = b"]}";
    let elements = (wire - head.len() - tail.len() + 1) / (element.len() + 1);
    let mut body = Vec::with_capacity(wire);
    body.extend_from_slice(head);
    for _ in 0..elements {
        if body.last() != Some(&b'[') {
            body.push(b',');
        }
        body.extend_from_slice(element);
    }
    body.extend_from_slice(tail);
    body
}

/// 上限之内的合法大请求照旧通过：一份贴住 16 MiB 的 data URL 图片请求解析成功，图片值取出来
/// 仍是原来那些字节。
#[test]
fn a_full_size_image_request_still_parses() {
    let body = max_wire_image_body();
    let parameters =
        RequestParameters::parse(&body).expect("the supported max-wire request parses");
    let image = parameters
        .get("image")
        .and_then(Value::as_str)
        .expect("the image field is a string");
    assert!(
        image.len() > SUPPORTED_REQUEST_WIRE_BYTES - 1024,
        "图片那一份必须是请求的绝大部分：{}",
        image.len()
    );
    assert_eq!(
        parameters.as_object().len(),
        4,
        "model/prompt/image/n 四个字段"
    );
}

/// 节点密集的两种形态在**节点数**上限处被拒：不是先把 2 GiB 的树建好再统计。
#[test]
fn node_dense_bodies_are_rejected_at_the_node_limit() {
    // 标量节点：2 字节一个节点，16 MiB 能放八百多万个。
    let scalars = array_body(b"0", SUPPORTED_REQUEST_WIRE_BYTES);
    let error = RequestParameters::parse(&scalars).expect_err("scalar node density is rejected");
    assert!(
        matches!(error, RequestJsonError::Limit(RequestStructureViolation::Nodes { limit }) if limit == REQUEST_JSON_MAX_NODES),
        "got {error:?}"
    );

    // 单字段对象节点：8 字节一个节点，每个节点一次 `Map` 分配，是最贵的一档。
    let objects = array_body(b"{\"a\":0}", SUPPORTED_REQUEST_WIRE_BYTES);
    let error = RequestParameters::parse(&objects).expect_err("object node density is rejected");
    assert!(
        matches!(error, RequestJsonError::Limit(RequestStructureViolation::Nodes { limit }) if limit == REQUEST_JSON_MAX_NODES),
        "got {error:?}"
    );
}

/// 超限是**边解析边判**的：上限之后即使还有语法错误的字节，先报的也是结构超限。
///
/// 这条是"不能先把整份 `Value` 建好再数节点"的直接证据——真要是先建完整棵树，解析会在越界那一段
/// 先撞上语法错误，回来的就是 `Syntax`。
#[test]
fn the_limit_is_checked_while_parsing_not_after() {
    let body = format!(
        "{{\"x\":[{}, !!not-json]}}",
        vec!["0"; REQUEST_JSON_MAX_NODES - 1].join(",")
    );
    let error = RequestParameters::parse(body.as_bytes())
        .expect_err("the node flood is reported before the trailing syntax error");
    assert!(
        matches!(
            error,
            RequestJsonError::Limit(RequestStructureViolation::Nodes { .. })
        ),
        "got {error:?}"
    );
}

/// 刚好在上限内的节点数通过；多一个就拒。
#[test]
fn the_node_limit_is_exact() {
    // 根对象 1 + 数组 1 + n 个标量 = n + 2 个节点。
    let inside = format!(
        "{{\"x\":[{}]}}",
        vec!["0"; REQUEST_JSON_MAX_NODES - 2].join(",")
    );
    RequestParameters::parse(inside.as_bytes()).expect("exactly the node limit still parses");
    let outside = format!(
        "{{\"x\":[{}]}}",
        vec!["0"; REQUEST_JSON_MAX_NODES - 1].join(",")
    );
    assert!(RequestParameters::parse(outside.as_bytes()).is_err());
}

/// 深嵌套在**深度**上限被拒；上限之内的嵌套放行。
#[test]
fn deeply_nested_bodies_are_rejected_at_the_depth_limit() {
    let inside = format!(
        "{{\"x\":{}{}}}",
        "[".repeat(REQUEST_JSON_MAX_DEPTH - 1),
        "]".repeat(REQUEST_JSON_MAX_DEPTH - 1)
    );
    RequestParameters::parse(inside.as_bytes()).expect("nesting within the limit parses");
    let outside = format!(
        "{{\"x\":{}{}}}",
        "[".repeat(REQUEST_JSON_MAX_DEPTH),
        "]".repeat(REQUEST_JSON_MAX_DEPTH)
    );
    let error = RequestParameters::parse(outside.as_bytes()).expect_err("too deep is rejected");
    assert!(
        matches!(error, RequestJsonError::Limit(RequestStructureViolation::Depth { limit }) if limit == REQUEST_JSON_MAX_DEPTH),
        "got {error:?}"
    );
}

/// 单个对象的字段数超限即拒；上限之内通过。
#[test]
fn wide_objects_are_rejected_at_the_field_limit() {
    let fields = |count: usize| {
        let names = (0..count)
            .map(|index| format!("\"f{index}\":0"))
            .collect::<Vec<_>>()
            .join(",");
        format!("{{{names}}}")
    };
    RequestParameters::parse(fields(REQUEST_JSON_MAX_OBJECT_FIELDS).as_bytes())
        .expect("exactly the field limit still parses");
    let error = RequestParameters::parse(fields(REQUEST_JSON_MAX_OBJECT_FIELDS + 1).as_bytes())
        .expect_err("one more field is rejected");
    assert!(
        matches!(error, RequestJsonError::Limit(RequestStructureViolation::ObjectFields { limit }) if limit == REQUEST_JSON_MAX_OBJECT_FIELDS),
        "got {error:?}"
    );
}

/// 累计字符串字节按**解码后**的字节数记（对象键也算），并可以用更小的上限验出来。
///
/// 缺省上限等于 wire 上限，而解码后的字符串总量恒 ≤ wire 总量，所以它在缺省配置下永远不会是
/// 先被撞到的那一条——这是刻意的：它不能比"已支持输入"更紧。这里用收紧后的上限验计数本身。
#[test]
fn cumulative_string_bytes_are_counted_after_decoding() {
    let limits = RequestJsonLimits {
        max_string_bytes: 16,
        ..REQUEST_JSON_LIMITS
    };
    // `"key"` 3 + `"value"` 5 = 8 字节。
    RequestParameters::parse_with_limits(br#"{"key":"value"}"#, limits)
        .expect("8 bytes of strings are within 16");
    let error = RequestParameters::parse_with_limits(br#"{"key":"value-is-longer"}"#, limits)
        .expect_err("more than 16 bytes of strings is rejected");
    assert!(
        matches!(error, RequestJsonError::Limit(RequestStructureViolation::StringBytes { limit }) if limit == 16),
        "got {error:?}"
    );

    // 转义序列按解码后的字节数算：`\n` 两个 wire 字节、一个字符串字节。键 `a` 一个字节，所以
    // 上限 2 刚好放行——按 wire 计的话这里是 7 个字节，会误拒。
    let escaped = RequestJsonLimits {
        max_string_bytes: 2,
        ..REQUEST_JSON_LIMITS
    };
    RequestParameters::parse_with_limits(br#"{"a":"\n"}"#, escaped)
        .expect("a decoded newline is one byte");
    assert!(RequestParameters::parse_with_limits(br#"{"a":"\n\n"}"#, escaped).is_err());
}

/// 顶层不是对象按参数面不成立拒绝；空对象是合法请求。
#[test]
fn the_top_level_must_be_an_object() {
    for body in [b"[1,2,3]".as_slice(), b"7", b"\"text\"", b"null"] {
        let error = RequestParameters::parse(body).expect_err("a non-object body is rejected");
        assert!(
            matches!(error, RequestJsonError::NotAnObject),
            "got {error:?}"
        );
    }
    let empty = RequestParameters::parse(b"{}").expect("an empty object is a valid parameter face");
    assert!(empty.as_object().is_empty());
}

/// 语法错误与尾随数据仍是语法错误，不会被当成结构超限。
#[test]
fn syntax_errors_stay_syntax_errors() {
    assert!(matches!(
        RequestParameters::parse(b"{ this is not json"),
        Err(RequestJsonError::Syntax(_))
    ));
    assert!(matches!(
        RequestParameters::parse(br#"{"a":1} trailing"#),
        Err(RequestJsonError::Syntax(_))
    ));
}

/// 计数器逐项构造：字段、键字节、值结构都记；超限时先失败、不写进对象。
#[test]
fn the_builder_counts_each_field_as_it_is_inserted() {
    let limits = RequestJsonLimits {
        max_object_fields: 2,
        max_string_bytes: 8,
        ..REQUEST_JSON_LIMITS
    };
    let mut builder = RequestParameters::builder_with_limits(limits);
    builder
        .insert("a".to_owned(), Value::from(1))
        .expect("the first field fits");
    builder
        .insert("b".to_owned(), Value::from(2))
        .expect("the second field fits");
    let error = builder
        .insert("c".to_owned(), Value::from(3))
        .expect_err("a third field is over the object field limit");
    assert_eq!(error, RequestStructureViolation::ObjectFields { limit: 2 });
    let parameters = builder.finish();
    assert_eq!(parameters.as_object().len(), 2);

    // 累计字符串字节同样在插入时核对。
    let mut narrow = RequestParameters::builder_with_limits(RequestJsonLimits {
        max_string_bytes: 4,
        ..REQUEST_JSON_LIMITS
    });
    let error = narrow
        .insert("name".to_owned(), Value::from("x"))
        .expect_err("the key alone is over the string budget");
    assert_eq!(error, RequestStructureViolation::StringBytes { limit: 4 });
    assert!(narrow.finish().as_object().is_empty());
}

/// 进程内值也走同一套计数：无界结构在构造请求参数面时被拒。
#[test]
fn an_in_memory_value_is_counted_before_it_becomes_request_parameters() {
    let dense = Value::Array(
        (0..REQUEST_JSON_MAX_NODES + 1)
            .map(|_| Value::from(0))
            .collect(),
    );
    assert!(matches!(
        RequestParameters::try_from(dense),
        Err(RequestJsonError::Limit(
            RequestStructureViolation::Nodes { .. }
        ))
    ));
    let object = Value::Object(
        [("prompt".to_owned(), Value::from("draw"))]
            .into_iter()
            .collect(),
    );
    let parameters = RequestParameters::try_from(object).expect("a small object is fine");
    assert_eq!(
        parameters.get("prompt").and_then(Value::as_str),
        Some("draw")
    );
}

/// 摘图只认契约字段，且操作的是已计数的参数面。
#[test]
fn contract_image_inputs_come_out_of_counted_parameters() {
    let mut parameters = RequestParameters::parse(
        br#"{"model":"m","prompt":"p","image":"https://example.invalid/a.png","mask":null}"#,
    )
    .expect("a well-formed request");
    let inputs = crate::take_contract_image_inputs(&mut parameters).expect("image fields");
    assert_eq!(
        inputs.reference_images,
        vec!["https://example.invalid/a.png".to_owned()]
    );
    assert_eq!(inputs.mask, None);
    assert_eq!(
        parameters.as_object().keys().collect::<Vec<_>>(),
        vec!["model", "prompt"],
        "契约字段被摘掉，其余参数留下"
    );
}
