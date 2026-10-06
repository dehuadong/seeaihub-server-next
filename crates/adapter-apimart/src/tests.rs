use super::*;
use async_trait::async_trait;
use seeai_adapter_sdk::{ExternalActionRefused, ImageSites};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

/// 同一传输策略（REQUEST_TIMEOUT）下，每次创建都复用同一个 Client，连接池跨请求、跨 Channel 复用。
#[test]
fn the_same_transport_policy_reuses_one_client() {
    let first = ApimartImageAdapter::new("https://api.apimart.ai", Duration::from_secs(30))
        .expect("config");
    let second = ApimartImageAdapter::new("https://api.apimart.ai", Duration::from_secs(30))
        .expect("config");
    assert!(Arc::ptr_eq(&first.client, &second.client));
}

/// 策略进入缓存 key：不同超时拿到不同 Client，不会共享一份默认超时。
#[test]
fn a_different_transport_policy_gets_its_own_client() {
    let first = shared_client(REQUEST_TIMEOUT).expect("client");
    let other = shared_client(REQUEST_TIMEOUT + Duration::from_secs(1)).expect("client");
    assert!(!Arc::ptr_eq(&first, &other));
}

fn envelope(code: i64, message: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "error": { "code": code, "message": message, "type": "invalid_request_error" }
    }))
    .expect("error envelope serializes")
}

fn task(status: &str, usage: Value, urls: Vec<&str>) -> TaskData {
    let body = serde_json::json!({
        "data": {
            "id": "task-x",
            "status": status,
            "usage": usage,
            "result": { "images": urls.into_iter().map(|url| serde_json::json!({"url": [url]})).collect::<Vec<_>>() }
        }
    });
    let parsed: TaskEnvelope = serde_json::from_value(body).expect("task envelope parses");
    parsed.data
}

fn full_usage() -> Value {
    serde_json::json!({
        "input_tokens": 14,
        "input_tokens_details": { "cached_tokens": 0, "image_tokens": 0, "text_tokens": 14 },
        "output_tokens": 196,
        "output_tokens_details": { "image_tokens": 196, "text_tokens": 0 },
        "total_tokens": 210
    })
}

#[test]
fn usage_maps_four_part_buckets_into_domain_shape() {
    let data = task(
        "completed",
        full_usage(),
        vec!["https://example.invalid/a.png"],
    );
    let usage = data.usage().expect("usage is present and consistent");
    assert_eq!(usage.input_tokens, 14);
    assert_eq!(usage.input_text_tokens, 14);
    assert_eq!(usage.input_image_tokens, 0);
    assert_eq!(usage.output_tokens, 196);
    assert_eq!(usage.output_image_tokens, 196);
    assert_eq!(usage.total_tokens, 210);
}

#[test]
fn usage_without_details_is_rejected_rather_than_guessed() {
    // 缺字段时不得猜测费用。
    let data = task(
        "completed",
        serde_json::json!({
            "input_tokens": 14,
            "output_tokens": 196,
            "total_tokens": 210
        }),
        vec!["https://example.invalid/a.png"],
    );
    let error = data.usage().expect_err("missing details must fail");
    assert!(
        error.to_string().contains("input_tokens_details"),
        "{error}"
    );
}

#[test]
fn usage_buckets_that_do_not_sum_are_rejected() {
    let mut usage = full_usage();
    usage["input_tokens"] = serde_json::json!(99);
    let data = task("completed", usage, vec!["https://example.invalid/a.png"]);
    let error = data.usage().expect_err("inconsistent buckets must fail");
    assert!(error.to_string().contains("do not sum"), "{error}");
}

#[test]
fn missing_usage_is_rejected() {
    let body = serde_json::json!({
        "data": { "id": "task-x", "status": "completed", "result": { "images": [] } }
    });
    let parsed: TaskEnvelope = serde_json::from_value(body).expect("parses");
    let error = parsed.data.usage().expect_err("absent usage must fail");
    assert!(error.to_string().contains("no token usage"), "{error}");
}

/// 造一条带（或不带）`cost` 的终态任务：`None` 表示响应里**根本没有这个字段**。
fn task_with_cost(cost: Option<Value>) -> TaskData {
    task_with_cost_and_images(cost, true)
}

/// 同上，`images` 决定终态里有没有结果图。
fn task_with_cost_and_images(cost: Option<Value>, images: bool) -> TaskData {
    let mut body = serde_json::json!({
        "data": {
            "id": "task-x",
            "status": "completed",
            "usage": full_usage(),
            "result": {"images": if images {
                vec![serde_json::json!({"url": ["https://example.invalid/a.png"]})]
            } else {
                Vec::<Value>::new()
            }}
        }
    });
    if let Some(cost) = cost {
        body["data"]["cost"] = cost;
    }
    let parsed: TaskEnvelope = serde_json::from_value(body).expect("task envelope parses");
    parsed.data
}

/// 终态声明的 `cost` **直接取用**：精确换成微单位，不按费率自算，也不经过浮点。
/// 币种跟着**渠道声明**走——金额本身不带币种，所以这里不假定任何币种。
#[test]
fn a_declared_cost_is_taken_verbatim_in_micro_units() {
    let declared = |currency: &str| {
        ProviderCost::Declared(DeclaredCost {
            amount_microusd: 11_354,
            currency: currency.to_owned(),
        })
    };
    // 实测样例：cost = 0.011354（含账号折扣，比自算权威）。
    let data = task_with_cost(Some(serde_json::json!(0.011354)));
    assert_eq!(data.provider_cost("USD"), declared("USD"));
    assert_eq!(data.provider_cost("CNY"), declared("CNY"));
    // 字符串形态的金额同样读得出来（有些上游把金额写成字符串）。
    let data = task_with_cost(Some(serde_json::json!("0.011354")));
    assert_eq!(data.provider_cost("USD"), declared("USD"));
}

/// 拿不到金额就**不猜**：缺字段、显式 null、负数、非数字一律记 `unavailable`——
/// 不写 0、不用"token × 费率"顶替、也不用上一次的值。
#[test]
fn a_cost_it_cannot_read_is_never_guessed() {
    for unreadable in [
        None,
        Some(Value::Null),
        Some(serde_json::json!(-0.01)),
        Some(serde_json::json!("n/a")),
        Some(serde_json::json!([0.01])),
    ] {
        let data = task_with_cost(unreadable.clone());
        assert_eq!(
            data.provider_cost("USD"),
            ProviderCost::Unavailable,
            "{unreadable:?} 不是可读的金额，不许被猜成一个数"
        );
    }
}

/// 十进制换算：小数位、指数形式与四舍五入都要准——钱差 1 微单位就是对不上账。
#[test]
fn decimal_amounts_convert_to_micro_units_exactly() {
    assert_eq!(parse_decimal_microusd("0.011354"), Some(11_354));
    assert_eq!(parse_decimal_microusd("0.00476"), Some(4_760));
    assert_eq!(parse_decimal_microusd("1"), Some(1_000_000));
    assert_eq!(parse_decimal_microusd("12.5"), Some(12_500_000));
    assert_eq!(parse_decimal_microusd("1e-6"), Some(1));
    assert_eq!(parse_decimal_microusd("2.5e-7"), Some(0), "不足半微单位");
    assert_eq!(parse_decimal_microusd("0.0000005"), Some(1), "半微单位进位");
    assert_eq!(parse_decimal_microusd("0"), Some(0), "上游明说这笔是 0");
    assert_eq!(parse_decimal_microusd("0.000"), Some(0));
    for unreadable in ["", "-1", "+", "abc", "1.2.3", "1e", "0.1e-1000", "1e99999"] {
        assert_eq!(
            parse_decimal_microusd(unreadable),
            None,
            "`{unreadable}` 读不出确切金额"
        );
    }
}

#[test]
fn completed_task_without_urls_is_rejected() {
    let data = task("completed", full_usage(), Vec::new());
    let error = data.image_urls().expect_err("no urls must fail");
    assert!(error.to_string().contains("no image url"), "{error}");
}

/// 终态读到了金额、却**没有结果图**：失败照样把已经读到的成本带走。
///
/// 这笔钱在金额读出来的那一刻就已经花了；附在错误上之后还要过补对账标识那一步，
/// 那一步只加不改——成本事实被冲掉，失败件在账上与缺口清单两头就都看不见。
#[test]
fn a_terminal_without_images_still_carries_the_cost_it_declared() {
    let data = task_with_cost_and_images(Some(serde_json::json!(0.011354)), false);
    let provider_cost = data.provider_cost("USD");
    assert_eq!(
        provider_cost,
        ProviderCost::Declared(DeclaredCost {
            amount_microusd: 11_354,
            currency: "USD".to_owned(),
        })
    );

    let error = data.image_urls().expect_err("no urls must fail");
    let error = with_task_id(with_provider_cost(error, provider_cost.clone()), "task-x");
    match error {
        AdapterError::Provider(provider) => {
            assert_eq!(provider.code, "provider_result_missing");
            assert_eq!(
                provider.trace_id.as_ref().map(ProviderTraceId::as_str),
                Some("task-x")
            );
            assert_eq!(
                provider.provider_cost,
                Some(provider_cost),
                "补对账标识只加不改：金额与结果无关，不许跟着结果一起丢"
            );
        }
        other => panic!("expected a provider error, got {other:?}"),
    }
}

#[test]
fn error_classification_prefers_code_over_status() {
    // 400 参数错误：未受理；类别上是渠道拒绝了平台的请求。
    let error = parse_provider_error(StatusCode::BAD_REQUEST, &envelope(400, "bad"));
    assert_eq!(error.retry_safety, RetrySafety::NotRetryable);
    assert_eq!(error.kind, ProviderFailureKind::UpstreamRejected);
    // 401/403：平台与渠道之间的凭据问题；402：平台在渠道侧欠费。
    let error = parse_provider_error(StatusCode::UNAUTHORIZED, &envelope(401, "no"));
    assert_eq!(error.retry_safety, RetrySafety::NotRetryable);
    assert_eq!(error.kind, ProviderFailureKind::PlatformCredential);
    let error = parse_provider_error(StatusCode::PAYMENT_REQUIRED, &envelope(402, "pay up"));
    assert_eq!(error.retry_safety, RetrySafety::NotRetryable);
    assert_eq!(error.kind, ProviderFailureKind::PlatformFunding);
    let error = parse_provider_error(StatusCode::FORBIDDEN, &envelope(403, "no"));
    assert_eq!(error.retry_safety, RetrySafety::NotRetryable);
    assert_eq!(error.kind, ProviderFailureKind::PlatformCredential);
    // 429 限流：不能证明未生成；类别上是渠道对平台限流。
    let error = parse_provider_error(StatusCode::TOO_MANY_REQUESTS, &envelope(429, "slow down"));
    assert_eq!(error.retry_safety, RetrySafety::AcceptanceUnknown);
    assert_eq!(error.kind, ProviderFailureKind::UpstreamRateLimited);
    // 其它 5xx：渠道不可用。
    let error = parse_provider_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        &envelope(500, "internal error"),
    );
    assert_eq!(error.retry_safety, RetrySafety::AcceptanceUnknown);
    assert_eq!(error.kind, ProviderFailureKind::UpstreamUnavailable);
}

#[test]
fn parameter_error_carried_as_500_is_not_retryable() {
    // 这是"分类以 error.code 与消息前缀为主、状态码只兜底"的关键理由。
    let error = parse_provider_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        &envelope(500, "build_request_failed: unsupported size"),
    );
    assert_eq!(error.retry_safety, RetrySafety::NotRetryable);
    // 用 5xx 承载的参数错误，类别上仍然是渠道拒绝了平台的请求。
    assert_eq!(error.kind, ProviderFailureKind::UpstreamRejected);
}

#[test]
fn other_server_errors_are_acceptance_unknown() {
    let error = parse_provider_error(StatusCode::BAD_GATEWAY, &envelope(502, "gateway down"));
    assert_eq!(error.retry_safety, RetrySafety::AcceptanceUnknown);
    let error = parse_provider_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        &envelope(500, "internal error"),
    );
    assert_eq!(error.retry_safety, RetrySafety::AcceptanceUnknown);
}

#[test]
fn unparseable_error_body_is_acceptance_unknown() {
    let error = parse_provider_error(StatusCode::INTERNAL_SERVER_ERROR, b"not json");
    assert_eq!(error.retry_safety, RetrySafety::AcceptanceUnknown);
    assert_eq!(error.code, "http_500");
}

#[test]
fn idempotency_subtypes_are_classified_by_the_first_party_criteria() {
    // 这两个子类能证明请求未被受理：类别上是"渠道拒了平台的请求"。
    let body =
        serde_json::json!({"error": {"code": "idempotency_in_progress", "message": "in flight"}});
    let error = parse_provider_error(
        StatusCode::CONFLICT,
        &serde_json::to_vec(&body).expect("body"),
    );
    assert_eq!(error.kind, ProviderFailureKind::UpstreamRejected);
    // 受理判定本项不动：第一方允许"未受理"，但收窄 `retry_safety` 是另一件事。
    assert_eq!(error.retry_safety, RetrySafety::AcceptanceUnknown);

    let body = serde_json::json!({"error": {"code": 409, "message": "idempotency_key_reused"}});
    let error = parse_provider_error(
        StatusCode::CONFLICT,
        &serde_json::to_vec(&body).expect("body"),
    );
    assert_eq!(error.kind, ProviderFailureKind::UpstreamRejected);

    // 这个子类**不能**证明未受理：拿不准按平台侧处理，不许当成"渠道拒绝了我们的请求"。
    let body = serde_json::json!({
        "error": {"code": 409, "message": "idempotency_result_indeterminate"}
    });
    let error = parse_provider_error(
        StatusCode::CONFLICT,
        &serde_json::to_vec(&body).expect("body"),
    );
    assert_eq!(error.kind, ProviderFailureKind::Unknown);
    // 状态码与文本不配对时同样不算依据：第一方只承诺了 `409`/`503` 这两个组合。
    let body = serde_json::json!({
        "error": {"code": 500, "message": "idempotency_in_progress"}
    });
    let error = parse_provider_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        &serde_json::to_vec(&body).expect("body"),
    );
    assert_eq!(error.kind, ProviderFailureKind::UpstreamUnavailable);
}

#[test]
fn unknown_status_is_not_treated_as_failure() {
    // 文档两份取值集合不一致；未列出的取值必须继续轮询。
    let body = serde_json::json!({
        "data": { "id": "task-x", "status": "queued_somewhere_new", "result": { "images": [] } }
    });
    let parsed: TaskEnvelope = serde_json::from_value(body).expect("parses");
    assert!(!matches!(
        parsed.data.status.as_str(),
        "completed" | "failed" | "cancelled"
    ));
}

#[test]
fn descriptor_declares_apimart_parameters_without_extra_wrapper() {
    let factory = ApimartAdapterFactory;
    let descriptor = factory
        .descriptor(ADAPTER_KEY)
        .expect("descriptor is declared");
    assert!(descriptor.supported_extra_parameters.is_empty());
    for name in ["model", "prompt", "quality", "resolution", "image_urls"] {
        assert!(
            descriptor.supported_top_level_parameters.contains(&name),
            "{name} must be a top-level parameter"
        );
    }
    assert_eq!(descriptor.max_reference_images, 16);
    assert!(factory.descriptor("some-other-key").is_none());
}

#[test]
fn query_phase_errors_are_routed_to_reconciliation_not_to_failure() {
    // 跨阶段例外：创建已成功后，查询阶段返回「无效的任务ID」（HTTP 400）
    // 必须处置为**对账**，而不是"未受理"式的失败并释放预授权。
    let raw = parse_provider_error(StatusCode::BAD_REQUEST, &envelope(400, "无效的任务ID"));
    // 就错误分类本身而言 400 仍是 NotRetryable（它确实是参数/资源问题）……
    assert_eq!(raw.retry_safety, RetrySafety::NotRetryable);
    // ……但 `poll` 对查询阶段的错误统一加一层 after_acceptance，改为不确定。
    let adjusted = after_acceptance(AdapterError::Provider(raw));
    match adjusted {
        AdapterError::Provider(provider) => {
            assert_eq!(provider.retry_safety, RetrySafety::AcceptanceUnknown);
        }
        other => panic!("expected a provider error, got {other:?}"),
    }
}

#[test]
fn build_request_failed_is_recognised_by_message_prefix_not_by_code() {
    // 文档只承诺这个前缀；即使它出现在别的 code 下，同样说明请求未被接受。
    let error = parse_provider_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        &envelope(503, "build_request_failed: unsupported size"),
    );
    assert_eq!(error.retry_safety, RetrySafety::NotRetryable);
    assert_eq!(error.kind, ProviderFailureKind::UpstreamRejected);
    // 无 code 但带前缀：同样判为未受理，也同样是渠道拒绝了平台的请求。
    let body = br#"{"error":{"message":"build_request_failed: bad field"}}"#;
    let error = parse_provider_error(StatusCode::INTERNAL_SERVER_ERROR, body);
    assert_eq!(error.retry_safety, RetrySafety::NotRetryable);
    assert_eq!(error.kind, ProviderFailureKind::UpstreamRejected);
}

#[test]
fn descriptor_declares_the_edit_branches() {
    // 参考图与遮罩只收公网 URL，APIMart 逐字透传，所以三条分支都可以声明。
    let descriptor = ApimartAdapterFactory
        .descriptor(ADAPTER_KEY)
        .expect("descriptor is declared");
    assert_eq!(
        descriptor.supported_branches,
        &[
            ImageBranch::PromptOnly,
            ImageBranch::ImageConditioned,
            ImageBranch::Masked
        ]
    );
}

#[test]
fn submit_rejections_that_prove_no_execution_are_narrowed() {
    // 第一方明文写明"请求未执行"的三类：提交阶段收窄为可证明未受理。
    let narrowed = |status: StatusCode, body: &[u8]| {
        let raw = parse_provider_error(status, body);
        // 收窄前一律是"受理状态不确定"——否则这条测试没有意义。
        assert_eq!(raw.retry_safety, RetrySafety::AcceptanceUnknown);
        match narrow_submit_rejection(status, AdapterError::Provider(raw)) {
            AdapterError::Provider(provider) => provider,
            other => panic!("expected a provider error, got {other:?}"),
        }
    };

    // 429 限流：第一方口径"能证明未受理"。
    let error = narrowed(
        StatusCode::TOO_MANY_REQUESTS,
        &envelope(429, "rate_limit_error"),
    );
    assert_eq!(error.retry_safety, RetrySafety::SafeBeforeAcceptance);
    // code 与 message 原样保留：平台仍看得出这是哪一类失败。
    assert_eq!(error.code, "429");
    // `429` 的依据就是状态码本身：错误体给不出可用标识符时仍然收窄——
    // 第一方承诺的是"429 这个响应"能证明未受理。
    let error = narrowed(
        StatusCode::TOO_MANY_REQUESTS,
        br#"{"error":{"message":"too many requests"}}"#,
    );
    assert_eq!(error.retry_safety, RetrySafety::SafeBeforeAcceptance);

    // 409 的两个幂等子类：第一方口径"能"。标识符可能落在 `error.code`（字符串）或消息里。
    let error = narrowed(
        StatusCode::CONFLICT,
        br#"{"error":{"code":"idempotency_in_progress","message":"in flight"}}"#,
    );
    assert_eq!(error.retry_safety, RetrySafety::SafeBeforeAcceptance);
    // 上游自己给的字符串标识符要留住，而不是退化成 `type` 或 `http_409`。
    assert_eq!(error.code, "idempotency_in_progress");
    let error = narrowed(
        StatusCode::CONFLICT,
        br#"{"error":{"code":409,"message":"idempotency_key_reused"}}"#,
    );
    assert_eq!(error.retry_safety, RetrySafety::SafeBeforeAcceptance);

    // 503 idempotency_unavailable：第一方原文"当前请求未执行"。
    let error = narrowed(
        StatusCode::SERVICE_UNAVAILABLE,
        &envelope(503, "idempotency_unavailable"),
    );
    assert_eq!(error.retry_safety, RetrySafety::SafeBeforeAcceptance);
}

#[test]
fn submit_rejections_without_a_first_party_basis_stay_unknown() {
    let unchanged = |status: StatusCode, body: &[u8]| {
        let raw = parse_provider_error(status, body);
        match narrow_submit_rejection(status, AdapterError::Provider(raw)) {
            AdapterError::Provider(provider) => provider.retry_safety,
            other => panic!("expected a provider error, got {other:?}"),
        }
    };

    // 结果不明的幂等子类：第一方要求停止自动重试、不要换 Key——不许收窄。
    assert_eq!(
        unchanged(
            StatusCode::CONFLICT,
            &envelope(409, "idempotency_result_indeterminate")
        ),
        RetrySafety::AcceptanceUnknown
    );
    // 普通 503 与 500：第一方没有"未受理"承诺。
    assert_eq!(
        unchanged(
            StatusCode::SERVICE_UNAVAILABLE,
            &envelope(503, "service_unavailable")
        ),
        RetrySafety::AcceptanceUnknown
    );
    assert_eq!(
        unchanged(
            StatusCode::INTERNAL_SERVER_ERROR,
            &envelope(500, "server_error")
        ),
        RetrySafety::AcceptanceUnknown
    );
    // 凭据/余额/参数类本来就是确定性拒绝，收窄不改变它们。
    assert_eq!(
        unchanged(StatusCode::PAYMENT_REQUIRED, &envelope(402, "pay up")),
        RetrySafety::NotRetryable
    );
    // 状态码与文本不配对时不算依据：第一方只承诺了 `409`+子类、`503`+unavailable。
    assert_eq!(
        unchanged(
            StatusCode::INTERNAL_SERVER_ERROR,
            &envelope(500, "idempotency_unavailable")
        ),
        RetrySafety::AcceptanceUnknown
    );
    assert_eq!(
        unchanged(
            StatusCode::SERVICE_UNAVAILABLE,
            &envelope(503, "idempotency_key_reused")
        ),
        RetrySafety::AcceptanceUnknown
    );
}

#[test]
fn post_acceptance_failures_are_never_narrowed() {
    // 同一批状态码出现在**已受理之后**（轮询阶段）时，一律仍按受理状态不确定处理：
    // 任务已经在跑，把它当失败会让平台白付一次生成。
    for (status, body) in [
        (
            StatusCode::TOO_MANY_REQUESTS,
            envelope(429, "rate_limit_error"),
        ),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            envelope(503, "idempotency_unavailable"),
        ),
    ] {
        let raw = parse_provider_error(status, &body);
        match after_acceptance(AdapterError::Provider(raw)) {
            AdapterError::Provider(provider) => {
                assert_eq!(provider.retry_safety, RetrySafety::AcceptanceUnknown);
            }
            other => panic!("expected a provider error, got {other:?}"),
        }
    }
}

#[test]
fn credential_failures_are_not_retryable_even_without_an_error_code() {
    // 实测（零费用、无凭证）：APIMart 的 401 信封是
    // `{"error":{"code":"","message":"invalid API key (request id: …)","param":"","type":"apimart_error"}}`
    // ——没有可用的 `error.code`。凭据问题不该被当成"受理状态不确定"而送进对账。
    let body = br#"{"error":{"code":"","message":"invalid API key (request id: 20260919182056471923385yBRUUrTx)","param":"","type":"apimart_error"}}"#;
    let error = parse_provider_error(StatusCode::UNAUTHORIZED, body);
    assert_eq!(error.retry_safety, RetrySafety::NotRetryable);
    // code 退化成 `type`，message 原样保留（请求 id 就在里面，落库后可用于排查）。
    assert_eq!(error.code, "apimart_error");
    assert!(error.message.contains("request id"));
    // 同为"无 code"的 5xx 仍然是不确定：状态码只在凭据类上兜底。
    let error = parse_provider_error(StatusCode::INTERNAL_SERVER_ERROR, br#"{"error":{}}"#);
    assert_eq!(error.retry_safety, RetrySafety::AcceptanceUnknown);
}

#[test]
fn post_acceptance_failures_carry_the_task_id_for_reconciliation() {
    // 提交成功之后失败：task id 是人工对账的唯一线索，必须附上。
    let raw = parse_provider_error(StatusCode::INTERNAL_SERVER_ERROR, &envelope(500, "boom"));
    let adjusted = after_acceptance(AdapterError::Provider(raw));
    match with_task_id(adjusted, "task_abc") {
        AdapterError::Provider(provider) => {
            assert_eq!(
                provider.trace_id.as_ref().map(ProviderTraceId::as_str),
                Some("task_abc")
            );
            // 分类与 code 不被这条补丁改变。
            assert_eq!(provider.retry_safety, RetrySafety::AcceptanceUnknown);
            assert_eq!(provider.code, "500");
        }
        other => panic!("expected a provider error, got {other:?}"),
    }
    // 已经有 trace id 的不覆盖；非 Provider 错误原样返回。
    let mut existing = provider_error(
        "x",
        "y".to_owned(),
        RetrySafety::AcceptanceUnknown,
        ProviderFailureKind::Unknown,
    );
    if let AdapterError::Provider(provider) = &mut existing {
        provider.trace_id = ProviderTraceId::parse("from-upstream");
    }
    match with_task_id(existing, "task_abc") {
        AdapterError::Provider(provider) => {
            assert_eq!(
                provider.trace_id.as_ref().map(ProviderTraceId::as_str),
                Some("from-upstream")
            );
        }
        other => panic!("expected a provider error, got {other:?}"),
    }
    assert!(matches!(
        with_task_id(
            AdapterError::UnsupportedInput("nope".to_owned()),
            "task_abc"
        ),
        AdapterError::UnsupportedInput(_)
    ));
}

// ── 同步网关协议：句柄 barrier、期限/取消与只读对账查询（RFC 0017 §4）──────────────

type Route = (&'static str, &'static str, u16, String);

fn submit_body() -> String {
    serde_json::json!({
        "code": 200,
        "data": [{"status": "submitted", "task_id": "task_abc"}]
    })
    .to_string()
}

fn completed_body() -> String {
    serde_json::json!({
        "code": 200,
        "data": {
            "id": "task_abc",
            "status": "completed",
            "usage": full_usage(),
            "result": {"images": [{"url": ["https://example.invalid/a.png"]}]},
            "cost": 0.011354
        }
    })
    .to_string()
}

fn running_body() -> String {
    serde_json::json!({
        "code": 200,
        "data": {"id": "task_abc", "status": "processing", "result": {"images": []}}
    })
    .to_string()
}

/// 一个本地假上游：按方法+路径应答，并逐条记下收到的请求。
struct FakeProvider {
    base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
    _task: tokio::task::JoinHandle<()>,
}

impl FakeProvider {
    async fn start(routes: Vec<Route>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the fake provider binds a local port");
        let port = listener.local_addr().expect("the fake address").port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let routes = Arc::new(routes);
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let recorded = recorded.clone();
                let routes = routes.clone();
                tokio::spawn(async move {
                    let _ = serve(&mut socket, recorded, routes).await;
                });
            }
        });
        Self {
            base_url: format!("http://127.0.0.1:{port}"),
            requests,
            _task: task,
        }
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().expect("requests lock").clone()
    }
}

async fn serve(
    socket: &mut tokio::net::TcpStream,
    requests: Arc<Mutex<Vec<String>>>,
    routes: Arc<Vec<Route>>,
) -> std::io::Result<()> {
    let mut reader = tokio::io::BufReader::new(&mut *socket);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;
    let mut content_length = 0_usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).await? == 0 {
            break;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }
    if content_length > 0 {
        let mut body = vec![0_u8; content_length];
        reader.read_exact(&mut body).await?;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    let path = path.split('?').next().unwrap_or(path);
    requests
        .lock()
        .expect("requests lock")
        .push(format!("{method} {path}"));
    let (status, body) = routes
        .iter()
        .find(|(route_method, route_path, _, _)| *route_method == method && *route_path == path)
        .map(|(_, _, status, body)| (*status, body.as_str()))
        .unwrap_or((404, "{\"error\":{\"code\":404,\"message\":\"not found\"}}"));
    let reason = if status < 400 { "OK" } else { "Error" };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    socket.write_all(head.as_bytes()).await?;
    socket.write_all(body.as_bytes()).await?;
    socket.flush().await
}

/// 假执行上下文：记下 accepted 被调用的时刻，以及那一刻假上游已经收到的请求。
struct FakeContext {
    deadline: Deadline,
    cancelled: bool,
    /// 取消恰好落在生成发送闸口：前面的取消检查都通过，但最后资格检查必须被拦下。
    gate_closed: bool,
    request_log: Arc<Mutex<Vec<String>>>,
    accepted: Mutex<Vec<(AcceptedHandle, Vec<String>)>>,
    accept_result: Result<(), AcceptanceError>,
}

impl FakeContext {
    fn new(
        request_log: Arc<Mutex<Vec<String>>>,
        accept_result: Result<(), AcceptanceError>,
    ) -> Self {
        Self {
            deadline: Deadline::after(Duration::from_secs(30)),
            cancelled: false,
            gate_closed: false,
            request_log,
            accepted: Mutex::new(Vec::new()),
            accept_result,
        }
    }

    /// 一开始就取消：证明新路径在第一次外部副作用之前停下。
    fn cancelled(request_log: Arc<Mutex<Vec<String>>>) -> Self {
        let mut context = Self::new(request_log, Ok(()));
        context.cancelled = true;
        context
    }

    /// 取消落在最后一道闸：前面的取消检查都通过，但生成发送必须被拦下。
    fn gate_closed(request_log: Arc<Mutex<Vec<String>>>) -> Self {
        let mut context = Self::new(request_log, Ok(()));
        context.gate_closed = true;
        context
    }

    /// 总期限已过：与取消区分开，报的是明确的期限错误。
    fn expired(request_log: Arc<Mutex<Vec<String>>>) -> Self {
        let mut context = Self::new(request_log, Ok(()));
        context.deadline = Deadline::after(Duration::ZERO);
        context
    }

    fn accepted_calls(&self) -> Vec<(AcceptedHandle, Vec<String>)> {
        self.accepted.lock().expect("accepted lock").clone()
    }
}

#[async_trait]
impl ExecutionContext for FakeContext {
    fn deadline(&self) -> Deadline {
        self.deadline
    }

    fn client_gone(&self) -> bool {
        self.cancelled
    }

    fn ownership_lost(&self) -> bool {
        false
    }

    fn try_begin_external_action(&self) -> Result<(), ExternalActionRefused> {
        if self.cancelled || self.gate_closed {
            return Err(ExternalActionRefused::ClientGone);
        }
        Ok(())
    }

    async fn accepted(&self, handle: AcceptedHandle) -> Result<(), AcceptanceError> {
        let snapshot = self.request_log.lock().expect("request log lock").clone();
        self.accepted
            .lock()
            .expect("accepted lock")
            .push((handle, snapshot));
        self.accept_result.clone()
    }
}

fn prompt_only_input() -> GatewayInput {
    GatewayInput {
        provider_model_id: "gpt-image-2.5-flare".to_owned(),
        branch: ImageBranch::PromptOnly,
        native_parameters: serde_json::json!({"prompt": "test", "n": 1}),
        reference_images: Vec::new(),
        mask: None,
        image_sites: ImageSites::default(),
        cost_currency: "USD".to_owned(),
    }
}

fn adapter(provider: &FakeProvider) -> ApimartImageAdapter {
    ApimartImageAdapter::new(&provider.base_url, Duration::from_secs(30)).expect("adapter config")
}

/// A6 barrier：accepted 返回 Ok 之前不许有任何 GET task；Ok 之后才轮询，且提交只发一次。
#[tokio::test]
async fn the_handle_is_persisted_before_any_task_poll() {
    let provider = FakeProvider::start(vec![
        ("POST", "/v1/images/generations", 200, submit_body()),
        ("GET", "/v1/tasks/task_abc", 200, completed_body()),
    ])
    .await;
    let context = FakeContext::new(provider.requests.clone(), Ok(()));
    let credential = ProviderCredential::new("test-key".to_owned()).expect("credential");
    let output = GatewayAdapter::execute(
        &adapter(&provider),
        Arc::new(prompt_only_input()),
        &context,
        &credential,
    )
    .await
    .expect("accepted then completed");
    assert_eq!(
        output.response_payload.images,
        vec![GeneratedImage::from_url(
            "https://example.invalid/a.png".to_owned()
        )]
    );
    assert_eq!(output.accounting_facts.image_count, 1);
    assert_eq!(
        output
            .accounting_facts
            .provider_trace_id
            .as_ref()
            .map(ProviderTraceId::as_str),
        Some("task_abc")
    );
    let calls = context.accepted_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0.task_id.as_str(), "task_abc");
    assert!(
        calls[0]
            .1
            .iter()
            .all(|request| !request.starts_with("GET ")),
        "accepted 返回 Ok 之前不许有 GET task：{:?}",
        calls[0].1
    );
    assert_eq!(calls[0].1, vec!["POST /v1/images/generations".to_owned()]);
    let requests = provider.requests();
    assert_eq!(
        requests.first().map(String::as_str),
        Some("POST /v1/images/generations")
    );
    assert!(
        requests.iter().any(|request| request.starts_with("GET ")),
        "句柄确认之后才轮询：{requests:?}"
    );
}

/// accepted 失败：上报 AcceptedUnpersisted 并带 task id，绝不进 poll、绝不发第二次 POST。
#[tokio::test]
async fn a_failed_handle_persist_never_polls_or_resubmits() {
    let provider = FakeProvider::start(vec![
        ("POST", "/v1/images/generations", 200, submit_body()),
        ("GET", "/v1/tasks/task_abc", 200, completed_body()),
    ])
    .await;
    let context = FakeContext::new(
        provider.requests.clone(),
        Err(AcceptanceError::Persist("db down".to_owned())),
    );
    let credential = ProviderCredential::new("test-key".to_owned()).expect("credential");
    let error = GatewayAdapter::execute(
        &adapter(&provider),
        Arc::new(prompt_only_input()),
        &context,
        &credential,
    )
    .await
    .expect_err("the barrier must stop the execution");
    match error {
        AdapterError::AcceptedUnpersisted { handle, reason } => {
            assert_eq!(handle.task_id.as_str(), "task_abc");
            assert_eq!(reason, "db down");
        }
        other => panic!("expected AcceptedUnpersisted, got {other:?}"),
    }
    assert_eq!(
        provider.requests(),
        vec!["POST /v1/images/generations".to_owned()],
        "句柄没入库：不许有第二次 POST，也不许进入 poll"
    );
}

/// 只读查询：同一任务端点取终态与账务事实，绝不 submit 或 upload。
#[tokio::test]
async fn query_accounting_reads_the_known_task_without_submitting() {
    let provider = FakeProvider::start(vec![
        ("POST", "/v1/images/generations", 200, submit_body()),
        ("GET", "/v1/tasks/task_abc", 200, completed_body()),
    ])
    .await;
    let credential = ProviderCredential::new("test-key".to_owned()).expect("credential");
    let handle = AcceptedHandle {
        task_id: ProviderTaskHandle::parse("task_abc".to_owned()).expect("test handle"),
        trace_id: ProviderTraceId::parse("task_abc"),
    };
    let query = adapter(&provider)
        .query_accounting(
            &handle,
            "CNY",
            Deadline::after(Duration::from_secs(30)),
            &credential,
        )
        .await
        .expect("the terminal task is queryable");
    assert_eq!(query.state, ProviderTaskState::Succeeded);
    let facts = query
        .accounting_facts
        .expect("a terminal task carries facts");
    assert_eq!(facts.image_count, 1);
    assert_eq!(facts.usage.expect("usage").total_tokens, 210);
    assert_eq!(
        facts.provider_cost,
        ProviderCost::Declared(DeclaredCost {
            amount_microusd: 11_354,
            currency: "CNY".to_owned(),
        }),
        "币种取受理时冻结的成本币种，不硬编码"
    );
    assert_eq!(
        facts
            .provider_trace_id
            .as_ref()
            .map(ProviderTraceId::as_str),
        Some("task_abc")
    );
    let requests = provider.requests();
    assert!(
        requests.iter().all(|request| request.starts_with("GET ")),
        "只读查询绝不 submit 或 upload：{requests:?}"
    );
}

/// 非终态：状态是 Pending，不带账务事实，也不产生任何生成副作用。
#[tokio::test]
async fn a_running_task_query_is_not_terminal() {
    let provider =
        FakeProvider::start(vec![("GET", "/v1/tasks/task_abc", 200, running_body())]).await;
    let credential = ProviderCredential::new("test-key".to_owned()).expect("credential");
    let handle = AcceptedHandle {
        task_id: ProviderTaskHandle::parse("task_abc".to_owned()).expect("test handle"),
        trace_id: None,
    };
    let query = adapter(&provider)
        .query_accounting(
            &handle,
            "CNY",
            Deadline::after(Duration::from_secs(30)),
            &credential,
        )
        .await
        .expect("a running task is a valid query result");
    assert_eq!(query.state, ProviderTaskState::Pending);
    assert!(query.accounting_facts.is_none());
    assert_eq!(
        provider.requests(),
        vec!["GET /v1/tasks/task_abc".to_owned()]
    );
}

fn failed_body() -> String {
    serde_json::json!({
        "code": 200,
        "data": {
            "id": "task_abc",
            "status": "failed",
            "usage": full_usage(),
            "error": {"code": 400, "message": "the content was rejected"},
            "cost": 0.011354
        }
    })
    .to_string()
}

/// 失败任务也带得回用量与成本：状态必须如实报成 Failed，不能被"有事实"翻成成功。
#[tokio::test]
async fn a_failed_task_query_reports_failure_with_its_cost() {
    let provider =
        FakeProvider::start(vec![("GET", "/v1/tasks/task_abc", 200, failed_body())]).await;
    let credential = ProviderCredential::new("test-key".to_owned()).expect("credential");
    let handle = AcceptedHandle {
        task_id: ProviderTaskHandle::parse("task_abc".to_owned()).expect("test handle"),
        trace_id: None,
    };
    let query = adapter(&provider)
        .query_accounting(
            &handle,
            "CNY",
            Deadline::after(Duration::from_secs(30)),
            &credential,
        )
        .await
        .expect("a failed task is a valid query result");
    assert_eq!(query.state, ProviderTaskState::Failed);
    let facts = query.accounting_facts.expect("the failure carried facts");
    assert_eq!(facts.image_count, 0);
    assert_eq!(
        facts.provider_cost,
        ProviderCost::Declared(DeclaredCost {
            amount_microusd: 11_354,
            currency: "CNY".to_owned(),
        })
    );
    assert_eq!(
        provider.requests(),
        vec!["GET /v1/tasks/task_abc".to_owned()],
        "查询失败任务也绝不 submit"
    );
}

fn cancelled_body() -> String {
    serde_json::json!({
        "code": 200,
        "data": {"id": "task_abc", "status": "cancelled", "usage": full_usage()}
    })
    .to_string()
}

#[tokio::test]
async fn a_cancelled_task_query_reports_cancellation() {
    let provider =
        FakeProvider::start(vec![("GET", "/v1/tasks/task_abc", 200, cancelled_body())]).await;
    let credential = ProviderCredential::new("test-key".to_owned()).expect("credential");
    let handle = AcceptedHandle {
        task_id: ProviderTaskHandle::parse("task_abc".to_owned()).expect("test handle"),
        trace_id: None,
    };
    let query = adapter(&provider)
        .query_accounting(
            &handle,
            "CNY",
            Deadline::after(Duration::from_secs(30)),
            &credential,
        )
        .await
        .expect("a cancelled task is a valid query result");
    assert_eq!(query.state, ProviderTaskState::Cancelled);
}

/// 未列出的状态取值不能当终态、更不能当成功：标成不可信，由收尾方保留占用。
#[tokio::test]
async fn an_unlisted_task_status_is_not_treated_as_success() {
    let body = serde_json::json!({
        "code": 200,
        "data": {"id": "task_abc", "status": "some_new_state", "usage": full_usage()}
    })
    .to_string();
    let provider = FakeProvider::start(vec![("GET", "/v1/tasks/task_abc", 200, body)]).await;
    let credential = ProviderCredential::new("test-key".to_owned()).expect("credential");
    let handle = AcceptedHandle {
        task_id: ProviderTaskHandle::parse("task_abc".to_owned()).expect("test handle"),
        trace_id: None,
    };
    let query = adapter(&provider)
        .query_accounting(
            &handle,
            "CNY",
            Deadline::after(Duration::from_secs(30)),
            &credential,
        )
        .await
        .expect("an unlisted status is a valid query result");
    assert_eq!(query.state, ProviderTaskState::Unknown);
    assert!(query.accounting_facts.is_none());
}

/// 响应里的任务标识与句柄不一致：整份响应都不可信，不能把它的状态或计量算到已知任务头上。
#[tokio::test]
async fn a_task_query_whose_identifier_differs_is_untrusted() {
    let body = serde_json::json!({
        "code": 200,
        "data": {
            "id": "task_other",
            "status": "completed",
            "usage": full_usage(),
            "cost": 0.011354
        }
    })
    .to_string();
    let provider = FakeProvider::start(vec![("GET", "/v1/tasks/task_abc", 200, body)]).await;
    let credential = ProviderCredential::new("test-key".to_owned()).expect("credential");
    let handle = AcceptedHandle {
        task_id: ProviderTaskHandle::parse("task_abc".to_owned()).expect("test handle"),
        trace_id: None,
    };
    let query = adapter(&provider)
        .query_accounting(
            &handle,
            "CNY",
            Deadline::after(Duration::from_secs(30)),
            &credential,
        )
        .await
        .expect("a mismatched response is still a valid transport result");
    assert_eq!(
        query.state,
        ProviderTaskState::Unknown,
        "a completed status for another task must never be read as this task's success"
    );
    assert!(query.accounting_facts.is_none());
}

/// 响应没有任务标识：同样无法证明状态属于已知任务，按不可信处理。
#[tokio::test]
async fn a_task_query_without_an_identifier_is_untrusted() {
    let body = serde_json::json!({
        "code": 200,
        "data": {"status": "completed", "usage": full_usage()}
    })
    .to_string();
    let provider = FakeProvider::start(vec![("GET", "/v1/tasks/task_abc", 200, body)]).await;
    let credential = ProviderCredential::new("test-key".to_owned()).expect("credential");
    let handle = AcceptedHandle {
        task_id: ProviderTaskHandle::parse("task_abc".to_owned()).expect("test handle"),
        trace_id: None,
    };
    let query = adapter(&provider)
        .query_accounting(
            &handle,
            "CNY",
            Deadline::after(Duration::from_secs(30)),
            &credential,
        )
        .await
        .expect("a response without an identifier is a valid transport result");
    assert_eq!(query.state, ProviderTaskState::Unknown);
    assert!(query.accounting_facts.is_none());
}
/// 取消在 submit 之前生效：没有 POST、没有 accepted，也不会有轮询。
#[tokio::test]
async fn a_cancelled_execution_never_submits() {
    let provider = FakeProvider::start(vec![
        ("POST", "/v1/images/generations", 200, submit_body()),
        ("GET", "/v1/tasks/task_abc", 200, completed_body()),
    ])
    .await;
    let context = FakeContext::cancelled(provider.requests.clone());
    let credential = ProviderCredential::new("test-key".to_owned()).expect("credential");
    let error = GatewayAdapter::execute(
        &adapter(&provider),
        Arc::new(prompt_only_input()),
        &context,
        &credential,
    )
    .await
    .expect_err("取消必须停下");
    assert!(
        matches!(error, AdapterError::CancelledBeforeSend),
        "发送前的取消必须与接受后的取消区分：{error:?}"
    );
    assert!(
        provider.requests().is_empty(),
        "取消后不许有上传或 submit：{:?}",
        provider.requests()
    );
    assert!(context.accepted_calls().is_empty());
}

/// 接受之后的只读轮询遇到取消：必须报成"不能证明未受理"的取消。
///
/// 这条用例挡住把读路径误用发送前闸的回归——那会让已提交的执行被当成"可证明未发送"而释放占用。
#[tokio::test]
async fn a_cancellation_while_reading_a_task_is_not_a_before_send_cancellation() {
    let provider =
        FakeProvider::start(vec![("GET", "/v1/tasks/task_abc", 200, running_body())]).await;
    let credential = ProviderCredential::new("test-key".to_owned()).expect("credential");
    let error = adapter(&provider)
        .query_task_with(
            "task_abc",
            &credential,
            Some(&FakeContext::cancelled(provider.requests.clone())),
            ReadBudget::GENERATION,
        )
        .await
        .expect_err("取消必须停下");
    assert!(
        matches!(error, AdapterError::Cancelled),
        "读路径的取消不能报成发送前取消：{error:?}"
    );
    assert!(
        provider.requests().is_empty(),
        "取消后不许发查询：{:?}",
        provider.requests()
    );
}

/// 取消恰好落在最后一道闸：前面的取消检查都通过，生成请求仍必须被拦下，且不产生任何提交。
#[tokio::test]
async fn a_cancellation_at_the_generation_gate_blocks_the_submit() {
    let provider =
        FakeProvider::start(vec![("POST", "/v1/images/generations", 200, submit_body())]).await;
    let context = FakeContext::gate_closed(provider.requests.clone());
    let credential = ProviderCredential::new("test-key".to_owned()).expect("credential");
    let error = GatewayAdapter::execute(
        &adapter(&provider),
        Arc::new(prompt_only_input()),
        &context,
        &credential,
    )
    .await
    .expect_err("闸口关闭必须停下");
    assert!(
        matches!(error, AdapterError::CancelledBeforeSend),
        "闸口拒绝必须与接受后的取消区分：{error:?}"
    );
    assert!(
        provider.requests().is_empty(),
        "闸口拒绝后不许有 submit：{:?}",
        provider.requests()
    );
    assert!(context.accepted_calls().is_empty());
}

/// 总期限已到：submit 之前停下，报明确的期限错误而不是笼统的传输失败。
#[tokio::test]
async fn an_expired_deadline_stops_before_submit() {
    let provider = FakeProvider::start(vec![
        ("POST", "/v1/images/generations", 200, submit_body()),
        ("GET", "/v1/tasks/task_abc", 200, completed_body()),
    ])
    .await;
    let context = FakeContext::expired(provider.requests.clone());
    // 期限从构造那一刻起算；推进一点点让"已过期"是确定的。
    tokio::time::sleep(Duration::from_millis(1)).await;
    let credential = ProviderCredential::new("test-key".to_owned()).expect("credential");
    let error = GatewayAdapter::execute(
        &adapter(&provider),
        Arc::new(prompt_only_input()),
        &context,
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
    assert!(
        provider.requests().is_empty(),
        "期限已到后不许有上传或 submit：{:?}",
        provider.requests()
    );
}

/// Provider 错误正文与带敏感 URL 的 reqwest 原始串不进新协议的错误。
#[tokio::test]
async fn provider_error_bodies_are_not_echoed() {
    let provider = FakeProvider::start(vec![(
        "POST",
        "/v1/images/generations",
        500,
        r#"{"error":{"code":500,"message":"SECRET_PROVIDER_BODY https://up.example/x?token=SECRET_TOKEN"}}"#
            .to_owned(),
    )])
    .await;
    let context = FakeContext::new(provider.requests.clone(), Ok(()));
    let credential = ProviderCredential::new("test-key".to_owned()).expect("credential");
    let error = GatewayAdapter::execute(
        &adapter(&provider),
        Arc::new(prompt_only_input()),
        &context,
        &credential,
    )
    .await
    .expect_err("上游 500 必须失败");
    let rendered = format!("{error:?} {error}");
    assert!(
        !rendered.contains("SECRET_PROVIDER_BODY") && !rendered.contains("SECRET_TOKEN"),
        "Provider 正文与敏感 URL 不得出现在错误里：{rendered}"
    );
}

/// 对账读取上限来自这份字节声明，并且不超过生成响应上限。
#[test]
fn the_reconciliation_read_limit_comes_from_the_declared_byte_limits() {
    let adapter = ApimartImageAdapter::new("http://127.0.0.1:1", Duration::from_secs(1))
        .expect("adapter config");
    assert_eq!(
        adapter.reconciliation_read_bytes,
        byte_limits().reconciliation_read_bytes()
    );
    assert!(
        adapter.reconciliation_read_bytes <= MAX_PROVIDER_RESPONSE_BYTES,
        "对账读取不会比生成读取更大"
    );
}

/// 只读对账有自己的、更小的响应上限：超过它的响应读不下去，也不从中推断状态，只留证据缺口。
///
/// 用例把上限自己收成 4 KiB，而响应明显大于它、又远小于生成响应上限（8 MiB）：对账若沿用生成
/// 上限，这份响应会被完整读入并解析成 `Succeeded`；返回 `Unknown` 才证明读取以对账上限为界。
#[tokio::test]
async fn a_reconciliation_response_above_its_own_limit_is_an_evidence_gap() {
    let body = serde_json::json!({
        "code": 200,
        "data": {
            "id": "task_abc",
            "status": "completed",
            "usage": full_usage(),
            "result": {"images": [{"url": ["https://example.invalid/a.png"]}]},
            "cost": 0.011354,
            "note": "x".repeat(8 * 1024),
        }
    })
    .to_string();
    assert!(body.len() > 4096 && body.len() < MAX_PROVIDER_RESPONSE_BYTES);
    let provider = FakeProvider::start(vec![("GET", "/v1/tasks/task_abc", 200, body)]).await;
    let credential = ProviderCredential::new("test-key".to_owned()).expect("credential");
    let handle = AcceptedHandle {
        task_id: ProviderTaskHandle::parse("task_abc".to_owned()).expect("test handle"),
        trace_id: None,
    };
    let mut subject = adapter(&provider);
    subject.reconciliation_read_bytes = 4096;
    let query = subject
        .query_accounting(
            &handle,
            "CNY",
            Deadline::after(Duration::from_secs(30)),
            &credential,
        )
        .await
        .expect("超限按证据缺口交回，不是通道错误");
    assert_eq!(query.state, ProviderTaskState::Unknown);
    assert!(
        query.accounting_facts.is_none(),
        "读不下去的响应不能被当成账务事实"
    );
    assert_eq!(
        provider.requests(),
        vec!["GET /v1/tasks/task_abc".to_owned()],
        "超限不重读：只发一次只读查询，绝不 submit"
    );
}
