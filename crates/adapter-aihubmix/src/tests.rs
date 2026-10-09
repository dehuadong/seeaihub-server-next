use super::*;
use async_trait::async_trait;
use seeai_adapter_sdk::{
    AcceptanceError, AcceptedHandle, Deadline, ExternalActionRefused, ImageSite, ImageSites,
    ImageValueShape, InputImage,
};
use seeai_domain::{
    ImageParameterKind, MAX_PROVIDER_IDENTIFIER_BYTES, platform_image_parameter,
    platform_image_parameters,
};
use std::sync::Arc;

/// 同一传输策略（这里就是单次超时）经创建路径拿到同一个 Client，连接池因此跨请求、跨 Channel 复用。
#[test]
fn the_same_transport_policy_reuses_one_client() {
    let first = AihubmixImageAdapter::new("https://api.inferera.com/", Duration::from_secs(17))
        .expect("config");
    let second = AihubmixImageAdapter::new("https://api.inferera.com/", Duration::from_secs(17))
        .expect("config");
    assert!(Arc::ptr_eq(&first.client, &second.client));
}

/// 超时不同的策略不共用 Client：不同默认超时的请求不能落进同一个 Client。
#[test]
fn a_different_transport_policy_gets_its_own_client() {
    let first = AihubmixImageAdapter::new("https://api.inferera.com/", Duration::from_secs(17))
        .expect("config");
    let other = AihubmixImageAdapter::new("https://api.inferera.com/", Duration::from_secs(18))
        .expect("config");
    assert!(!Arc::ptr_eq(&first.client, &other.client));
}

/// 假执行上下文：期限与两种取消事实固定；同步渠道绝不该调用 accepted。
struct FakeContext {
    deadline: Deadline,
    client_gone: bool,
    /// 取消恰好落在生成发送闸口：前面的取消检查都通过，但最后资格检查必须被拦下。
    gate_closed: bool,
}

impl FakeContext {
    fn cancelled() -> Self {
        Self {
            deadline: Deadline::after(Duration::from_secs(30)),
            client_gone: true,
            gate_closed: false,
        }
    }

    /// 取消落在最后一道闸：前面的取消检查都通过，但生成发送必须被拦下。
    fn gate_closed() -> Self {
        Self {
            deadline: Deadline::after(Duration::from_secs(30)),
            client_gone: false,
            gate_closed: true,
        }
    }

    fn expired() -> Self {
        Self {
            deadline: Deadline::after(Duration::ZERO),
            client_gone: false,
            gate_closed: false,
        }
    }
}

#[async_trait]
impl ExecutionContext for FakeContext {
    fn deadline(&self) -> Deadline {
        self.deadline
    }

    fn client_gone(&self) -> bool {
        self.client_gone
    }

    fn ownership_lost(&self) -> bool {
        false
    }

    fn try_begin_external_action(&self) -> Result<(), ExternalActionRefused> {
        if self.gate_closed || self.client_gone {
            return Err(ExternalActionRefused::ClientGone);
        }
        Ok(())
    }

    async fn accepted(&self, _handle: AcceptedHandle) -> Result<(), AcceptanceError> {
        panic!("a synchronous channel must never confirm an accepted handle")
    }
}

/// 发布素材里那条 AIHubMix 供给的**承载面**（声明的是 `image` / `mask`）。
fn published_schema() -> Value {
    published_offering(&published_config())["carrier_schema"].clone()
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
    assert_eq!(descriptor.max_reference_images, 16);
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
    config["offerings"][0]["carrier_schema"]["properties"]["images"] = serde_json::json!({
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

    config["offerings"][0]["carrier_schema"]["properties"]["images"]["items"]["type"] =
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
fn provider_error_marks_upstream_unreachable_as_unknown() {
    let body =
        br#"{"error":{"message":"maybe accepted","code":"upstream_unreachable","tid":"req_1"}}"#;
    let error = parse_provider_error(StatusCode::BAD_GATEWAY, body);
    assert_eq!(error.retry_safety, RetrySafety::AcceptanceUnknown);
    assert_eq!(
        error.trace_id.as_ref().map(ProviderTraceId::as_str),
        Some("req_1")
    );
}

#[test]
fn provider_error_marks_service_unavailable_as_unknown() {
    let body = br#"{"error":{"message":"not accepted","code":"service_unavailable"}}"#;
    let error = parse_provider_error(StatusCode::SERVICE_UNAVAILABLE, body);
    assert_eq!(error.retry_safety, RetrySafety::AcceptanceUnknown);
    assert_eq!(error.kind, ProviderFailureKind::UpstreamUnavailable);
}

/// 错误体里的 tid 与成功响应头同源：URL、data URL、控制字符与超长值都不是标识，
/// 入口就丢弃，只留平台自己的分类（Spec 0005 §2、RFC 0018 §6）。
#[test]
fn provider_error_drops_a_tid_that_is_not_a_bounded_identifier() {
    for tid in [
        "https://example.invalid/trace/1",
        "data:image/png;base64,AAAA",
        "trace\u{7f}",
        "line\\nbreak",
    ] {
        let body = format!(
            r#"{{"error":{{"message":"boom","code":"upstream_unreachable","tid":"{tid}"}}}}"#
        );
        let error = parse_provider_error(StatusCode::BAD_GATEWAY, body.as_bytes());
        assert!(
            error.trace_id.is_none(),
            "{tid:?} must not become a trace id"
        );
    }

    let too_long = "a".repeat(MAX_PROVIDER_IDENTIFIER_BYTES + 1);
    let body = format!(
        r#"{{"error":{{"message":"boom","code":"upstream_unreachable","tid":"{too_long}"}}}}"#
    );
    assert!(
        parse_provider_error(StatusCode::BAD_GATEWAY, body.as_bytes())
            .trace_id
            .is_none(),
        "an over-long tid must not become a trace id"
    );
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

// ── 同步网关协议（RFC 0017 §2/§4）──────────────────────────────────────────────

/// 从候选声明面推出新协议的图片参数位；角色判定与线上同一处推导。
fn gateway_sites(schema: &Value, branch: ImageBranch) -> ImageSites {
    let platform = platform_image_parameters(schema, branch);
    let reference =
        platform_image_parameter(&platform, ImageParameterKind::Reference).map(|name| ImageSite {
            parameter: name.to_owned(),
            shape: gateway_shape(schema, name),
        });
    let mask =
        platform_image_parameter(&platform, ImageParameterKind::Mask).map(|name| ImageSite {
            parameter: name.to_owned(),
            shape: ImageValueShape::Scalar,
        });
    ImageSites { reference, mask }
}

fn gateway_shape(schema: &Value, name: &str) -> ImageValueShape {
    match schema
        .pointer(&format!("/properties/{name}/type"))
        .and_then(Value::as_str)
    {
        Some("array") => ImageValueShape::Array,
        _ => ImageValueShape::Scalar,
    }
}

/// 编辑入口把公网 URL 逐字写成**媒体引用**：不下载、不上传，也不去取结果。
#[test]
fn the_edit_body_carries_public_urls_as_media_references() {
    let schema = published_schema();
    let input = GatewayInput {
        provider_model_id: "gpt-image-2.5-sunburst".to_owned(),
        branch: ImageBranch::ImageConditioned,
        native_parameters: serde_json::json!({"prompt": "test"}),
        reference_images: vec![
            InputImage::url("https://example.invalid/one.png"),
            InputImage::url("https://example.invalid/two.png"),
        ],
        mask: None,
        image_sites: gateway_sites(&schema, ImageBranch::ImageConditioned),
        cost_currency: "USD".to_owned(),
    };
    let body = gateway_body(&input).expect("the body builds");
    let reference = input
        .image_sites
        .reference
        .as_ref()
        .expect("the offering declares a reference parameter");
    assert_eq!(
        body[&reference.parameter],
        serde_json::json!([
            "https://example.invalid/one.png",
            "https://example.invalid/two.png"
        ]),
        "参考图按承载面声明的名字写成公网 URL：{body}"
    );
    assert!(body.get("model").is_some() && body.get("prompt").is_some());
}

/// 取消在生成请求之前生效：闸门拦下，不会真的发出去（基址上没有服务在听）。
#[tokio::test]
async fn a_cancelled_gateway_execution_never_sends() {
    let adapter =
        AihubmixImageAdapter::new("http://127.0.0.1:1/", Duration::from_secs(10)).expect("config");
    let input = GatewayInput {
        provider_model_id: "gpt-image-2.5-flare".to_owned(),
        branch: ImageBranch::PromptOnly,
        native_parameters: serde_json::json!({"prompt": "test"}),
        reference_images: Vec::new(),
        mask: None,
        image_sites: ImageSites::default(),
        cost_currency: "USD".to_owned(),
    };
    let credential = ProviderCredential::new("test-key".to_owned()).expect("credential");
    let error = GatewayAdapter::execute(
        &adapter,
        Arc::new(input),
        &FakeContext::cancelled(),
        &credential,
    )
    .await
    .expect_err("取消必须停下");
    assert!(
        matches!(error, AdapterError::CancelledBeforeSend),
        "发送前的取消必须与接受后的取消区分：{error:?}"
    );
}

/// 取消恰好落在最后一道闸：前面的取消检查都通过，发送仍必须被拦下。
///
/// 基址上没有服务在听：如果闸口没拦住，这里会先报连接失败而不是 `CancelledBeforeSend`。
#[tokio::test]
async fn a_cancellation_at_the_generation_gate_blocks_the_send() {
    let adapter =
        AihubmixImageAdapter::new("http://127.0.0.1:1/", Duration::from_secs(10)).expect("config");
    let input = GatewayInput {
        provider_model_id: "gpt-image-2.5-flare".to_owned(),
        branch: ImageBranch::PromptOnly,
        native_parameters: serde_json::json!({"prompt": "test"}),
        reference_images: Vec::new(),
        mask: None,
        image_sites: ImageSites::default(),
        cost_currency: "USD".to_owned(),
    };
    let credential = ProviderCredential::new("test-key".to_owned()).expect("credential");
    let error = GatewayAdapter::execute(
        &adapter,
        Arc::new(input),
        &FakeContext::gate_closed(),
        &credential,
    )
    .await
    .expect_err("闸口关闭必须停下");
    assert!(
        matches!(error, AdapterError::CancelledBeforeSend),
        "闸口拒绝必须与传输失败区分：{error:?}"
    );
}

/// 总期限已到：生成请求之前停下，报明确的期限错误而不是传输失败。
#[tokio::test]
async fn an_expired_gateway_deadline_stops_before_sending() {
    let adapter =
        AihubmixImageAdapter::new("http://127.0.0.1:1/", Duration::from_secs(10)).expect("config");
    let input = GatewayInput {
        provider_model_id: "gpt-image-2.5-flare".to_owned(),
        branch: ImageBranch::PromptOnly,
        native_parameters: serde_json::json!({"prompt": "test"}),
        reference_images: Vec::new(),
        mask: None,
        image_sites: ImageSites::default(),
        cost_currency: "USD".to_owned(),
    };
    let credential = ProviderCredential::new("test-key".to_owned()).expect("credential");
    tokio::time::sleep(Duration::from_millis(1)).await;
    let error = GatewayAdapter::execute(
        &adapter,
        Arc::new(input),
        &FakeContext::expired(),
        &credential,
    )
    .await
    .expect_err("期限已到必须停下");
    match error {
        AdapterError::Provider(call) => {
            assert_eq!(call.code, "execution_deadline_exceeded");
            assert_eq!(call.retry_safety, RetrySafety::NotRetryable);
        }
        other => panic!("expected the deadline error, got {other:?}"),
    }
}

/// 新协议的错误出口不把 Provider 自由文本或 reqwest 原始串带出去。
#[test]
fn gateway_errors_drop_provider_text_and_transport_strings() {
    let error = AdapterError::Provider(ProviderCallError {
        code: "provider_transport_unknown".to_owned(),
        message: "error sending request for url (https://up.example/x?token=SECRET)".to_owned(),
        trace_id: None,
        retry_safety: RetrySafety::AcceptanceUnknown,
        kind: ProviderFailureKind::UpstreamUnavailable,
        provider_cost: None,
    });
    let AdapterError::Provider(call) = gateway_error(error) else {
        panic!("a provider error stays a provider error");
    };
    assert_eq!(call.message, "the provider call failed");
    assert!(!call.message.contains("SECRET"));
}

#[test]
fn every_branch_uses_the_single_image_endpoint() {
    let adapter = AihubmixImageAdapter::new("https://api.inferera.com/", Duration::from_secs(10))
        .expect("adapter config should be valid");
    // 上游只有一个图片端点：分支由请求里有没有图片字段决定，不由端点分流。
    assert_eq!(
        adapter
            .endpoint()
            .expect("endpoint should resolve")
            .as_str(),
        "https://api.inferera.com/ai/v1/images/generations"
    );
}

#[test]
fn the_result_envelope_takes_the_inline_base64_only() {
    let inline = images_from_task(
        vec![TaskOutput {
            b64_json: Some("AAAA".to_owned()),
            content_url: Some("https://api.inferera.com/ai/v1/images/t/content/r".to_owned()),
        }],
        &ProviderCost::Unavailable,
    )
    .expect("an inline image is a usable result");
    assert_eq!(inline, vec![GeneratedImage::from_base64("AAAA".to_owned())]);
    // content_url 要平台凭据，平台既不下载也不交给调用方：只有它就没有可用结果。而且失败件要带着
    // **已经读到的金额**回去，不能在判定失败时把成本事实丢掉。
    let declared = ProviderCost::Declared(DeclaredCost {
        amount_microusd: 11_354,
        currency: "USD".to_owned(),
    });
    let error = images_from_task(
        vec![TaskOutput {
            b64_json: None,
            content_url: Some("https://api.inferera.com/ai/v1/images/t/content/r".to_owned()),
        }],
        &declared,
    )
    .expect_err("only content_url is not a usable result");
    match error {
        AdapterError::Provider(failure) => assert_eq!(
            failure.provider_cost,
            Some(declared),
            "结果缺失也要把读到的金额交回平台"
        ),
        other => panic!("expected a provider failure, got {other:?}"),
    }
    assert!(
        images_from_task(
            vec![TaskOutput {
                b64_json: None,
                content_url: None
            }],
            &ProviderCost::Unavailable,
        )
        .is_err()
    );
}

#[test]
fn a_declared_amount_is_taken_verbatim_and_a_missing_one_is_a_cost_gap() {
    let declared = declared_cost(
        Some(&TaskUsage {
            cost: Some(serde_json::json!(0.026436)),
        }),
        "USD",
    );
    assert_eq!(
        declared,
        ProviderCost::Declared(DeclaredCost {
            amount_microusd: 26_436,
            currency: "USD".to_owned()
        })
    );
    // 字段在但为空、或读不出来：按成本缺口记，不猜金额。
    for usage in [
        TaskUsage {
            cost: Some(Value::Null),
        },
        TaskUsage { cost: None },
        TaskUsage {
            cost: Some(serde_json::json!("nonsense")),
        },
    ] {
        assert_eq!(
            declared_cost(Some(&usage), "USD"),
            ProviderCost::Unavailable
        );
    }
    assert_eq!(declared_cost(None, "USD"), ProviderCost::Unavailable);
}

#[test]
fn a_completed_task_without_a_result_carries_the_declared_cost() {
    let body = serde_json::json!({
        "id": "t_1",
        "status": "completed",
        "output": [],
        "usage": {"cost": 0.011354}
    })
    .to_string();
    let error =
        success_from_body(body.as_bytes(), None, "USD").expect_err("no images is not a result");
    match error {
        AdapterError::Provider(call) => {
            assert_eq!(call.code, "provider_result_empty");
            assert_eq!(
                call.provider_cost,
                Some(ProviderCost::Declared(DeclaredCost {
                    amount_microusd: 11_354,
                    currency: "USD".to_owned()
                }))
            );
        }
        other => panic!("expected a provider error, got {other:?}"),
    }
}

#[test]
fn a_failed_task_is_a_provider_error_with_its_amount() {
    let body = serde_json::json!({
        "id": "t_2",
        "status": "failed",
        "output": [],
        "error": {"code": "content_policy", "message": "rejected"},
        "usage": {"cost": 0.001}
    })
    .to_string();
    let error =
        success_from_body(body.as_bytes(), None, "USD").expect_err("failed tasks are failures");
    match error {
        AdapterError::Provider(call) => {
            assert_eq!(call.code, "content_policy");
            assert_eq!(
                call.provider_cost,
                Some(ProviderCost::Declared(DeclaredCost {
                    amount_microusd: 1_000,
                    currency: "USD".to_owned()
                }))
            );
        }
        other => panic!("expected a provider error, got {other:?}"),
    }
}

/// 完成态任务对象里的 `id` 落到 `provider_trace_id`：它就是这次执行的对账标识，
/// 平台据此在审计与对账里认这一笔（[设计 0022](../../../.agents/notes/implemented/platform/2026-10-06-aihubmix-ai-v1-execution-path.md)）。
#[test]
fn a_completed_task_keeps_its_id_as_the_provider_trace() {
    let body = serde_json::json!({
        "id": "t_2f9c1b7a",
        "status": "completed",
        "output": [{"b64_json": "AAAA"}],
        "usage": {"cost": 0.026436}
    })
    .to_string();
    let success = success_from_body(body.as_bytes(), None, "USD")
        .expect("a completed task with an inline image is a success");
    assert_eq!(
        success
            .provider_trace_id
            .as_ref()
            .map(ProviderTraceId::as_str),
        Some("t_2f9c1b7a")
    );
}
