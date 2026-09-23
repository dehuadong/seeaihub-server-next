use super::*;
use seeai_domain::platform_image_parameters;

fn request(branch: ImageBranch) -> PreparedImageRequest {
    request_for(&published_schema(), branch)
}

/// 按某个候选声明面造一份受理产物：名单与线上**同一处推导**（候选声明的参数名 + 分支），
/// 测试里不另抄一份名字。
fn request_for(schema: &Value, branch: ImageBranch) -> PreparedImageRequest {
    PreparedImageRequest {
        provider_model_id: "gpt-image-2.5-flare".to_owned(),
        branch,
        native_parameters: serde_json::json!({
            "prompt": "test",
            "n": 1,
            "size": "1024x1024",
            "output_format": "png",
            "quality": "low"
        }),
        platform_parameters: platform_image_parameters(schema, branch),
        cost_currency: "USD".to_owned(),
    }
}

/// 发布素材里那条 AIHubMix 供给的**承载面**（声明的是 `image` / `mask`）。
fn published_schema() -> Value {
    published_offering(&published_config())["carrier_schema"].clone()
}

/// 把 Driver 组好的编辑表单摊成**将要发出去的字节**：`name="…"` 就是上游收到的东西。
///
/// 不起上游也不走网络：测试里的图都是内联 data URL，取字节这一步没有任何 IO。
/// （表单的字节流由 reqwest 自己拼装，这里只是把它读出来。）
async fn multipart_body(request: &PreparedImageRequest) -> Result<String, AdapterError> {
    let adapter = AihubmixImageAdapter::new("https://api.inferera.com/", Duration::from_secs(10))
        .expect("adapter config should be valid");
    let form = adapter.edit_form(request).await?;
    let chunks = form.into_stream().collect::<Vec<_>>().await;
    let mut body = Vec::new();
    for chunk in chunks {
        body.extend_from_slice(&chunk.expect("the form should stream"));
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// 在售素材是"一份合同 + 两条供给"：AIHubMix 这条在下标 0，声明面在 `carrier_schema` 里。
fn published_config() -> Value {
    serde_json::from_str(include_str!(
        "../../../config/bootstrap/gpt-image-2.5-flare.json"
    ))
    .expect("bootstrap config should parse")
}

fn published_offering(config: &Value) -> &Value {
    &config["offerings"][0]
}

#[test]
fn accepts_bootstrap_capability_contract() {
    let config = published_config();
    let offering = published_offering(&config);
    AihubmixAdapterFactory
        .validate_publication(
            offering["adapter_key"].as_str().expect("adapter key"),
            &offering["carrier_schema"],
            &offering["restrictions"],
        )
        .expect("bootstrap contract should be executable");
}

/// Driver 的收图上限与素材声明同源：16 张参考图（发布期用它卡素材，两边对不上就发不出去）。
#[test]
fn the_descriptor_allows_the_sixteen_reference_images_the_materials_declare() {
    let descriptor = AihubmixAdapterFactory
        .descriptor(ADAPTER_KEY)
        .expect("the adapter describes itself");
    assert_eq!(descriptor.max_images, 16);
}

#[test]
fn rejects_schema_with_wrong_prompt_type() {
    let mut config = published_config();
    config["offerings"][0]["carrier_schema"]["properties"]["prompt"]["type"] =
        Value::String("integer".to_owned());
    let offering = published_offering(&config).clone();
    let error = AihubmixAdapterFactory
        .validate_publication(
            ADAPTER_KEY,
            &offering["carrier_schema"],
            &offering["restrictions"],
        )
        .expect_err("wrong prompt type must be rejected");
    assert!(error.contains("prompt"));
}

/// 参考图声明成**字符串数组**也算声明得成形状（同名部件重复出现就是 multipart 里的列表形态）；
/// 数组里不是字符串则拒绝——那种形态本 Driver 发不出去。
#[test]
fn a_reference_image_declared_as_a_string_array_is_executable() {
    let mut config = published_config();
    config["offerings"][0]["carrier_schema"]["properties"]["image"] = serde_json::json!({
        "type": "array",
        "items": {"type": "string"},
        "minItems": 1,
        "maxItems": 16
    });
    let offering = published_offering(&config).clone();
    AihubmixAdapterFactory
        .validate_publication(
            ADAPTER_KEY,
            &offering["carrier_schema"],
            &offering["restrictions"],
        )
        .expect("a string array reference image is executable");

    config["offerings"][0]["carrier_schema"]["properties"]["image"]["items"]["type"] =
        Value::String("integer".to_owned());
    let offering = published_offering(&config).clone();
    let error = AihubmixAdapterFactory
        .validate_publication(
            ADAPTER_KEY,
            &offering["carrier_schema"],
            &offering["restrictions"],
        )
        .expect_err("array items that are not strings must be rejected");
    assert!(error.contains("image"), "{error}");
}

#[test]
fn maps_branch_to_provider_endpoint() {
    let adapter = AihubmixImageAdapter::new("https://api.inferera.com/", Duration::from_secs(10))
        .expect("adapter config should be valid");
    assert_eq!(
        adapter
            .endpoint(ImageBranch::PromptOnly)
            .expect("endpoint should resolve")
            .as_str(),
        "https://api.inferera.com/v1/images/generations"
    );
    assert_eq!(
        adapter
            .endpoint(ImageBranch::Masked)
            .expect("endpoint should resolve")
            .as_str(),
        "https://api.inferera.com/v1/images/edits"
    );
}

#[test]
fn sends_quality_on_the_wire_field() {
    let body =
        generation_body(&request(ImageBranch::PromptOnly)).expect("request should be supported");
    assert_eq!(
        body.pointer("/quality"),
        Some(&Value::String("low".to_owned()))
    );
    // 调用方与线上都是顶层：不再有 `extra` 这一层包装。
    assert!(body.get("extra").is_none());
}

#[test]
fn passes_the_documented_optional_parameters_through() {
    let mut prepared = request(ImageBranch::PromptOnly);
    let Value::Object(parameters) = &mut prepared.native_parameters else {
        panic!("fixture parameters must be an object");
    };
    parameters.insert("background".to_owned(), Value::String("opaque".to_owned()));
    parameters.insert("output_compression".to_owned(), Value::from(80));
    parameters.insert("moderation".to_owned(), Value::String("low".to_owned()));
    parameters.insert("user".to_owned(), Value::String("end-user-1".to_owned()));
    let body = generation_body(&prepared).expect("request should be supported");
    assert_eq!(
        body.pointer("/background"),
        Some(&Value::String("opaque".to_owned()))
    );
    assert_eq!(body.pointer("/output_compression"), Some(&Value::from(80)));
    assert_eq!(
        body.pointer("/moderation"),
        Some(&Value::String("low".to_owned()))
    );
    assert_eq!(
        body.pointer("/user"),
        Some(&Value::String("end-user-1".to_owned()))
    );
}

/// 到手的每一个参数都原样进请求体：名字不改，取值也不改。
///
/// 这是个**防御性**用例：按现行规则，本用例塞进来的名字在受理期就按候选声明面丢掉了，
/// 到不了 Driver（到手的参数面本来就是声明面里的子集）。之所以还要这么写，是为了钉住
/// Driver **自己**的判断依据：它不按名字、也不按取值的形状重新解释任何参数——名字以 `image`
/// 开头、取值是对象数组的一手参数（渠道文档里写明的形状）在它眼里同样只是一个普通参数。
/// 将来上游过滤一旦放松，这里会立刻看得见。
#[test]
fn every_parameter_it_receives_goes_upstream_verbatim() {
    let mut prepared = request(ImageBranch::PromptOnly);
    let Value::Object(parameters) = &mut prepared.native_parameters else {
        panic!("fixture parameters must be an object");
    };
    parameters.insert("channel_specific_knob".to_owned(), Value::from(7));
    parameters.insert(
        "image_with_roles".to_owned(),
        serde_json::json!([{"role": "reference", "url": "https://example.invalid/a.png"}]),
    );
    let body = generation_body(&prepared).expect("request should be supported");
    assert_eq!(
        body.pointer("/channel_specific_knob"),
        Some(&Value::from(7))
    );
    assert_eq!(
        body.pointer("/image_with_roles/0/role"),
        Some(&Value::String("reference".to_owned()))
    );
    // 声明过的可选参数一并原样过去（`request()` 的声明面里有 `quality`）。
    assert_eq!(
        body.pointer("/quality"),
        Some(&Value::String("low".to_owned()))
    );
}

/// Driver 不按名字或取值的形状重新解释参数：`images` 留在原地，不会被当成参考图。
///
/// 归属只看平台名单（名单里是候选声明、平台装载过的那些名字）：名字不在名单里的参数，
/// Driver 既不拿它当图、也不把它的值改写成上游 URL。这也是个**防御性**用例：`images` 没被
/// 这份候选声明，正常链路上在受理期就丢了，到不了这里；把它直接塞进请求，是为了钉住
/// Driver 的判断依据始终是名单，而不是"这个名字看起来像图"。
#[test]
fn a_received_parameter_is_never_reinterpreted_by_its_shape() {
    let mut prepared = request(ImageBranch::PromptOnly);
    let Value::Object(parameters) = &mut prepared.native_parameters else {
        panic!("fixture parameters must be an object");
    };
    parameters.insert(
        "images".to_owned(),
        serde_json::json!(["https://example.invalid/u.png"]),
    );
    let body = generation_body(&prepared).expect("request should be supported");
    assert_eq!(
        body.pointer("/images"),
        Some(&serde_json::json!(["https://example.invalid/u.png"])),
        "到手的参数逐字上行：{body}"
    );
    assert!(
        body.pointer("/image").is_none(),
        "名单里没有 `images`：Driver 不按形状把它认领成参考图，也不改写名字：{body}"
    );
}

/// multipart 的编辑路径：到手的一手参数**要么进文本部件、要么明确失败**，不许被默默跳过。
///
/// 标量照旧进文本部件；数组在 multipart 里没有平台承认的表示法，于是报错并点名是哪个参数
/// （`multipart_text` 里写了为什么不做"序列化成 JSON 文本"这种替代形态）。用例里塞进来的
/// 两个名字同样越过了受理期的声明面过滤（正常链路上到不了这里），钉的是这条路径的处置
/// 不依赖"这个名字认不认识"：标量有文本部件形态、数组明确失败，两条都在。
#[test]
fn received_parameters_on_the_multipart_path_are_never_dropped_silently() {
    let mut prepared = request(ImageBranch::ImageConditioned);
    prepared.native_parameters = serde_json::json!({
        "prompt": "test",
        "image": "data:image/png;base64,AAAA",
        "channel_specific_knob": "scalar",
        // 名字像图、取值是字符串数组，但不在名单里：Driver 只当它是普通参数，
        // 而 multipart 没有能承载数组的文本部件形态。
        "images": ["https://example.invalid/u.png"]
    });
    let scalar = multipart_text("channel_specific_knob", &Value::from("scalar"))
        .expect("标量有文本部件形态");
    assert_eq!(scalar, "scalar");
    let error = multipart_text(
        "images",
        &serde_json::json!(["https://example.invalid/u.png"]),
    )
    .expect_err("数组在这条路径上没有表示法");
    let message = error.to_string();
    assert!(message.contains("images"), "报错必须点名参数：{message}");
    // 名单里的图片参数不在这条透传链路上：它们走文件部件。
    assert!(
        !passthrough_parameters(&prepared)
            .iter()
            .any(|(name, _)| name.as_str() == "image")
    );
}

/// 编辑端点收几张参考图由发布物与选路定，Driver 只保证"至少一张"：一张都不给就直接拒绝，
/// 多张照收——声明的能力与实现必须是同一件事。
#[test]
fn the_edit_endpoint_takes_every_reference_image_it_is_given() {
    let mut value = request(ImageBranch::ImageConditioned);
    value.native_parameters = serde_json::json!({
        "prompt": "test",
        "image": ["data:image/png;base64,AAAA", "data:image/png;base64,BBBB"]
    });
    let inputs = reference_inputs(&value).expect("this surface is published for several images");
    assert_eq!(inputs.reference_images.len(), 2);
    // 一张参考图可以被接受；遮罩一并带出来。
    let mut masked = request(ImageBranch::Masked);
    masked.native_parameters = serde_json::json!({
        "prompt": "test",
        "image": "data:image/png;base64,AAAA",
        "mask": "data:image/png;base64,BBBB"
    });
    let inputs = reference_inputs(&masked).expect("one reference image plus a mask");
    assert_eq!(inputs.reference_images.len(), 1);
    assert!(inputs.mask.is_some());
    // 没有参考图：编辑端点没有可编辑的图，直接拒绝。
    assert!(reference_inputs(&request(ImageBranch::ImageConditioned)).is_err());
}

/// 多张参考图在线上是**重复的 `image[]` 部件**：不是只发第一张，也不是重复单值 `image`
/// （实测后者会 400）。一张时仍是单值 `image`。
#[tokio::test]
async fn several_reference_images_become_repeated_list_parts() {
    let mut two = request(ImageBranch::ImageConditioned);
    two.native_parameters = serde_json::json!({
        "prompt": "test",
        "image": ["data:image/png;base64,AAAA", "data:image/png;base64,BBBB"]
    });
    let rendered = multipart_body(&two)
        .await
        .expect("two reference images are expressible");
    assert_eq!(
        rendered.matches("name=\"image[]\"").count(),
        2,
        "两张参考图就是两个 `image[]` 部件：{rendered}"
    );
    assert!(
        !rendered.contains("name=\"image\""),
        "多张时不许退回单值 `image`（渠道会 400）：{rendered}"
    );

    // 一张时仍是单值 `image`：列表形态只属于多张。
    let mut one = request(ImageBranch::ImageConditioned);
    one.native_parameters = serde_json::json!({
        "prompt": "test",
        "image": ["data:image/png;base64,AAAA"]
    });
    let rendered = multipart_body(&one)
        .await
        .expect("one reference image is expressible");
    assert!(rendered.contains("name=\"image\""), "{rendered}");
    assert!(!rendered.contains("image[]"), "{rendered}");
}

#[test]
fn images_are_not_sent_as_text_parameters() {
    // 图片走文件部件：JSON 体里不许再出现 `image` / `mask` 的字符串值。
    let mut prepared = request(ImageBranch::Masked);
    prepared.native_parameters = serde_json::json!({
        "prompt": "test",
        "image": "data:image/png;base64,AAAA",
        "mask": "data:image/png;base64,BBBB"
    });
    let names = passthrough_parameters(&prepared)
        .into_iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    assert!(!names.iter().any(|name| name == "image" || name == "mask"));
}

/// 部件名与候选声明面同源：Profile 把参考图参数声明成 `image_urls`（**不是** `image`）时，
/// 线上 multipart 的部件名就是 `image_urls`。
///
/// 断言看的是表单摊成的字节里那些 `name="…"`——写死名字的实现会在这里发出 `name="image"`，
/// 于是把图塞进上游根本没声明过的字段：静默改名。名字由名单给出，所以这里换个声明面就换名字。
#[tokio::test]
async fn edit_part_names_are_the_names_the_profile_declared() {
    let schema = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": {
            "model": {"const": "gpt-image-2"},
            "prompt": {"type": "string"},
            "image_urls": {"type": "array", "items": {"type": "string"}, "maxItems": 1},
            "mask_url": {"type": "string"}
        }
    });
    let mut prepared = request_for(&schema, ImageBranch::Masked);
    prepared.native_parameters = serde_json::json!({
        "prompt": "test",
        "image_urls": ["data:image/png;base64,AAAA"],
        "mask_url": "data:image/png;base64,BBBB"
    });
    let rendered = multipart_body(&prepared)
        .await
        .expect("the candidate can express both inputs");
    assert!(rendered.contains("name=\"image_urls\""), "{rendered}");
    assert!(rendered.contains("name=\"mask_url\""), "{rendered}");
    assert!(
        !rendered.contains("name=\"image\"") && !rendered.contains("name=\"mask\""),
        "部件名不许退回写死的 image / mask：{rendered}"
    );
    // 名单为空（候选一张图都没声明）：这次请求表达不了——明确失败，同样不退回写死的名字。
    let mut unclaimed = request(ImageBranch::ImageConditioned);
    unclaimed.native_parameters = serde_json::json!({
        "prompt": "test",
        "image": "data:image/png;base64,AAAA"
    });
    unclaimed.platform_parameters = Vec::new();
    assert!(matches!(
        multipart_body(&unclaimed).await,
        Err(AdapterError::UnsupportedInput(_))
    ));
}

#[test]
fn keeps_the_shape_the_provider_gave() {
    let url = images_from_response(vec![ImageData {
        url: Some("https://example.invalid/a.png".to_owned()),
        b64_json: None,
    }])
    .expect("a url is a usable result");
    assert_eq!(
        url,
        vec![GeneratedImage::from_url(
            "https://example.invalid/a.png".to_owned()
        )]
    );
    let base64 = images_from_response(vec![ImageData {
        url: None,
        b64_json: Some("AAAA".to_owned()),
    }])
    .expect("base64 is a usable result");
    assert_eq!(base64, vec![GeneratedImage::from_base64("AAAA".to_owned())]);
    // 两个都给时保留 url；两个都没有就是不可用的结果。
    let both = images_from_response(vec![ImageData {
        url: Some("https://example.invalid/a.png".to_owned()),
        b64_json: Some("AAAA".to_owned()),
    }])
    .expect("both shapes are usable");
    assert_eq!(
        both,
        vec![GeneratedImage::from_url(
            "https://example.invalid/a.png".to_owned()
        )],
        "两个都给时只留 url：结果信封里永远只有一种取图方式"
    );
    assert!(
        images_from_response(vec![ImageData {
            url: None,
            b64_json: None
        }])
        .is_err()
    );
}

/// 响应读成了、却没有可用结果：**失败也要报告这次执行的成本事实**。
///
/// 这条渠道不给金额字段，所以成本事实与成功分支同口径报 `computed`（平台按实际用量自算），
/// 而不是报"没采到"——后者会被读成"这次执行没有成本"，与"渠道不报金额"混成一件事。
#[test]
fn a_response_without_images_still_reports_where_the_cost_comes_from() {
    let body = serde_json::to_vec(&serde_json::json!({
        "data": [],
        "usage": {
            "input_tokens": 9,
            "input_tokens_details": {"text_tokens": 9, "image_tokens": 0},
            "output_tokens": 196,
            "output_tokens_details": {"text_tokens": 0, "image_tokens": 196},
            "total_tokens": 205
        }
    }))
    .expect("a response body serializes");
    match success_from_body(&body, None).expect_err("no images must fail") {
        AdapterError::Provider(provider) => {
            assert_eq!(provider.code, "provider_result_empty");
            assert_eq!(provider.provider_cost, Some(ProviderCost::Computed));
            assert_eq!(provider.retry_safety, RetrySafety::AcceptanceUnknown);
        }
        other => panic!("expected a provider error, got {other:?}"),
    }
}

#[test]
fn inline_data_urls_are_decoded_in_memory() {
    let decoded =
        decode_inline_image("data:image/png;base64,iVBORw0KGgo=").expect("decodes in memory");
    assert_eq!(decoded.media_type, "image/png");
    assert_eq!(
        &decoded.bytes[..],
        &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]
    );
    // 不是 http(s) 也不是 data URL 的值一律拒绝，不猜。
    let error =
        classify_image_value("asset://not-a-thing").expect_err("an unknown shape must be rejected");
    assert!(error.to_string().contains("http(s) url"));
    // 公网地址走下载那一支，data URL 走解码那一支——两条路只有一处判定。
    assert!(matches!(
        classify_image_value("https://example.invalid/a.png"),
        Ok(ImageValue::Remote(_))
    ));
    assert!(matches!(
        classify_image_value("data:image/png;base64,AAAA"),
        Ok(ImageValue::Inline(_))
    ));
}

#[test]
fn provider_error_marks_upstream_unreachable_as_unknown() {
    let body =
        br#"{"error":{"message":"maybe accepted","code":"upstream_unreachable","tid":"req_1"}}"#;
    let error = parse_provider_error(StatusCode::BAD_GATEWAY, body);
    assert_eq!(error.retry_safety, RetrySafety::AcceptanceUnknown);
    assert_eq!(error.trace_id.as_deref(), Some("req_1"));
}

#[test]
fn provider_error_marks_service_unavailable_as_unknown() {
    let body = br#"{"error":{"message":"not accepted","code":"service_unavailable"}}"#;
    let error = parse_provider_error(StatusCode::SERVICE_UNAVAILABLE, body);
    assert_eq!(error.retry_safety, RetrySafety::AcceptanceUnknown);
    assert_eq!(error.kind, ProviderFailureKind::UpstreamUnavailable);
}

#[test]
fn provider_error_marks_rate_limit_as_unknown_without_acceptance_proof() {
    let body = br#"{"error":{"message":"rate limited","code":"upstream_rate_limited"}}"#;
    let error = parse_provider_error(StatusCode::TOO_MANY_REQUESTS, body);
    assert_eq!(error.retry_safety, RetrySafety::AcceptanceUnknown);
}

#[test]
fn provider_error_marks_generic_server_error_as_unknown() {
    let body = br#"{"error":{"message":"unknown","code":"internal_error"}}"#;
    let error = parse_provider_error(StatusCode::INTERNAL_SERVER_ERROR, body);
    assert_eq!(error.retry_safety, RetrySafety::AcceptanceUnknown);
}

#[test]
fn provider_error_separates_platform_funding_from_platform_credentials() {
    // 渠道明确说余额不足：属于平台自己在渠道侧的账户问题。
    let body = br#"{"error":{"message":"quota exhausted","code":"insufficient_user_quota"}}"#;
    let error = parse_provider_error(StatusCode::FORBIDDEN, body);
    assert_eq!(error.retry_safety, RetrySafety::NotRetryable);
    assert_eq!(error.kind, ProviderFailureKind::PlatformFunding);
    // 403 但不是余额：凭证、权限或白名单问题。
    let body = br#"{"error":{"message":"forbidden","code":"permission_denied"}}"#;
    assert_eq!(
        parse_provider_error(StatusCode::FORBIDDEN, body).kind,
        ProviderFailureKind::PlatformCredential
    );
    // 没有可用 code 的 403 同样按状态码落到凭据类。
    let body = br#"{"error":{"message":"forbidden"}}"#;
    assert_eq!(
        parse_provider_error(StatusCode::FORBIDDEN, body).kind,
        ProviderFailureKind::PlatformCredential
    );
    // 401：凭据问题。
    let body = br#"{"error":{"message":"invalid key"}}"#;
    assert_eq!(
        parse_provider_error(StatusCode::UNAUTHORIZED, body).kind,
        ProviderFailureKind::PlatformCredential
    );
}

#[test]
fn usage_rejects_missing_token_detail_fields() {
    let response = br#"{
            "data": [{"b64_json": "unused"}],
            "usage": {
                "input_tokens": 1,
                "input_tokens_details": {"text_tokens": 1},
                "output_tokens": 1,
                "output_tokens_details": {"text_tokens": 0, "image_tokens": 1},
                "total_tokens": 2
            }
        }"#;
    let error = serde_json::from_slice::<ImageResponse>(response)
        .expect_err("missing image_tokens must reject metering evidence");
    assert!(error.to_string().contains("image_tokens"));
}
