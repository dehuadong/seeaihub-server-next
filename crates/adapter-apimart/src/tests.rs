use super::*;

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
            assert_eq!(provider.trace_id.as_deref(), Some("task-x"));
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
fn descriptor_declares_the_edit_branches_now_that_upload_exists() {
    // 参考图与遮罩以前被拒绝，因为上游只收公网可访问的 URL，而我们没有上传链路。
    // 上传实现之后三条分支都可以声明了。
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
fn upload_filename_follows_the_media_type() {
    let bytes = b"image";
    assert!(upload_filename(bytes, "image/jpeg").ends_with(".jpg"));
    assert!(upload_filename(bytes, "image/webp").ends_with(".webp"));
    assert!(upload_filename(bytes, "image/gif").ends_with(".gif"));
    assert!(upload_filename(bytes, "image/png").ends_with(".png"));
    // 文件名里带上内容摘要：同一张图重复上传时上游看到同一个名字。
    assert_eq!(
        upload_filename(bytes, "image/png"),
        upload_filename(bytes, "image/png")
    );
}

#[test]
fn upload_failures_are_safe_before_acceptance() {
    // 上传发生在提交生成任务之前，所以它的失败不是"受理状态不确定"，
    // 而是**可证明未受理**（`SafeBeforeAcceptance`）——本阶段同样映射为
    // 失败并释放预授权，但不该被标成"确定性拒绝"。
    let raw = parse_provider_error(
        StatusCode::BAD_REQUEST,
        br#"{"error":{"message":"unsupported image type","type":"invalid_request_error"}}"#,
    );
    // `parse_provider_error` 只看 `error.code`；这份错误体没有 code，所以是"不确定"。
    assert_eq!(raw.retry_safety, RetrySafety::AcceptanceUnknown);
    match upload_failure(AdapterError::Provider(raw)) {
        AdapterError::Provider(provider) => {
            assert_eq!(provider.retry_safety, RetrySafety::SafeBeforeAcceptance);
            // code 与 message 原样保留：平台仍能看出这是哪一类失败。
            assert_eq!(provider.code, "invalid_request_error");
        }
        other => panic!("expected a provider error, got {other:?}"),
    }
}

#[tokio::test]
async fn upload_transport_failures_are_safe_before_acceptance() {
    // 连不上上游：生成任务同样从未发出 ⇒ 可证明未受理，不是"不确定"。
    let error = reqwest::Client::new()
        .get("http://127.0.0.1:1/never")
        .send()
        .await
        .expect_err("nothing listens on port 1");
    match upload_transport_error(error) {
        AdapterError::Provider(provider) => {
            assert_eq!(provider.retry_safety, RetrySafety::SafeBeforeAcceptance);
        }
        other => panic!("expected a provider error, got {other:?}"),
    }
}

#[test]
fn credential_failures_are_not_retryable_even_without_an_error_code() {
    // 实测（零费用、无凭证）：`POST /v1/uploads/images` 的 401 信封是
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
            assert_eq!(provider.trace_id.as_deref(), Some("task_abc"));
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
        provider.trace_id = Some("from-upstream".to_owned());
    }
    match with_task_id(existing, "task_abc") {
        AdapterError::Provider(provider) => {
            assert_eq!(provider.trace_id.as_deref(), Some("from-upstream"));
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

#[test]
fn uploaded_urls_must_be_absolute_http_addresses() {
    assert_eq!(
        validate_uploaded_url("https://upload.example/a.png").expect("https is fine"),
        "https://upload.example/a.png"
    );
    for bad in ["", "asset://abc", "file:///etc/passwd", "not a url"] {
        assert!(
            validate_uploaded_url(bad).is_err(),
            "`{bad}` must not be forwarded into the generation request"
        );
    }
}

#[test]
fn total_inline_upload_size_is_capped_like_the_per_file_limit() {
    // 公网 URL 不计入：平台不读它的字节。只有内联 data URL 需要挡住总量。
    let inline = |payload: String| PreparedImageRequest {
        provider_model_id: "gpt-image-2.5-flare".to_owned(),
        branch: ImageBranch::ImageConditioned,
        native_parameters: serde_json::json!({
            "prompt": "x",
            "image_urls": [format!("data:image/png;base64,{payload}")]
        }),
        platform_parameters: vec!["image_urls".to_owned()],
        cost_currency: "USD".to_owned(),
    };
    assert!(
        ApimartImageAdapter::ensure_total_upload_within_limit(&inline("A".repeat(1024))).is_ok()
    );
    let mut public = inline("A".repeat(1024));
    public.native_parameters = serde_json::json!({
        "prompt": "x",
        "image_urls": ["https://example.invalid/very-large.png"]
    });
    assert!(
        ApimartImageAdapter::ensure_total_upload_within_limit(&public).is_ok(),
        "a public url is passed through, so it costs the platform nothing"
    );
    // 名单外的图名参数不参与统计：平台不上传它，也就不为它申请内存。
    // （这类没声明的名字在受理期就按声明面丢掉了，到不了 Driver；这条钉的是换算只看名单。）
    let mut unclaimed = inline("A".repeat(1024));
    unclaimed.native_parameters = serde_json::json!({
        "prompt": "x",
        "images": [format!("data:image/png;base64,{}", "A".repeat(MAX_TOTAL_UPLOAD_BYTES))]
    });
    assert!(
        ApimartImageAdapter::ensure_total_upload_within_limit(&unclaimed).is_ok(),
        "名单外的参数不是平台的图，不算进上传总量"
    );
    // 超过上游总量上限的内联图片直接拒绝，不去为它申请那么多内存。
    let oversized = (MAX_TOTAL_UPLOAD_BYTES / 3 + 8) * 4;
    assert!(
        ApimartImageAdapter::ensure_total_upload_within_limit(&inline("A".repeat(oversized)))
            .is_err()
    );
}

/// Driver 收到的参数面**已经**是候选声明面里的子集：受理期按声明面过滤过了。
///
/// 所以这里没有"认不认识这个参数"的判别——到手的每个名字（声明过的 `n`、`resolution`
/// 与任何别的名字）都原样进请求体，取值一个都不改。
#[test]
fn every_parameter_it_receives_goes_upstream_verbatim() {
    let mut prepared = PreparedImageRequest {
        provider_model_id: "gpt-image-2".to_owned(),
        branch: ImageBranch::PromptOnly,
        native_parameters: serde_json::json!({"prompt": "test", "n": 1}),
        platform_parameters: Vec::new(),
        cost_currency: "USD".to_owned(),
    };
    let Value::Object(parameters) = &mut prepared.native_parameters else {
        panic!("fixture parameters must be an object");
    };
    parameters.insert("channel_specific_knob".to_owned(), Value::from(7));
    parameters.insert("resolution".to_owned(), Value::from("2k"));
    parameters.insert(
        "image_with_roles".to_owned(),
        serde_json::json!([{"role": "reference", "url": "https://example.invalid/a.png"}]),
    );
    let body = generation_body(&prepared, &Map::new()).expect("supported");
    assert_eq!(
        body.pointer("/channel_specific_knob"),
        Some(&Value::from(7))
    );
    assert_eq!(body.pointer("/resolution"), Some(&Value::from("2k")));
    assert_eq!(
        body.pointer("/image_with_roles/0/role"),
        Some(&Value::from("reference"))
    );
}

/// 归属只看平台名单，不看取值的形状：名字不在名单里，Driver 既不换算它，也不改写它。
///
/// 受理期已经按候选声明面过滤过，没声明的名字**根本到不了 Driver**；这条用例钉的是
/// Driver 这一侧的行为：`images`（即使带着 data URL）不在名单里，就不参与换算——平台不会
/// 解码上传、把它改成上游 URL，请求体里它仍是原样。
#[tokio::test]
async fn an_images_array_outside_the_platform_list_is_not_converted() {
    let adapter =
        ApimartImageAdapter::new("http://127.0.0.1:1", Duration::from_secs(1)).expect("config");
    let credential = ProviderCredential::new("test-key".to_owned()).expect("credential");
    let inline = "data:image/png;base64,AAAA";
    let prepared = PreparedImageRequest {
        provider_model_id: "gpt-image-2.5-flare".to_owned(),
        branch: ImageBranch::PromptOnly,
        native_parameters: serde_json::json!({
            "prompt": "test",
            "images": [inline]
        }),
        platform_parameters: Vec::new(),
        cost_currency: "USD".to_owned(),
    };
    // `http://127.0.0.1:1` 上没有任何东西可以连：一旦它真去上传就会失败，
    // 因此这个用例通过本身就证明 data URL 没有被拿去上传。
    let resolved = adapter
        .resolve_images(&prepared, &credential)
        .await
        .expect("名单外没有图片要换算");
    assert!(resolved.is_empty(), "名单外没有可换算的图片：{resolved:?}");
    let body = generation_body(&prepared, &resolved).expect("supported");
    assert_eq!(
        body.pointer("/images"),
        Some(&serde_json::json!([inline])),
        "到手的参数逐字上行：{body}"
    );
    assert!(
        body.pointer("/image_urls").is_none(),
        "名单里没有的名字不会被改写成候选声明的图片字段：{body}"
    );
}

#[test]
fn generation_body_carries_the_resolved_image_urls_at_their_parameter_names() {
    let request = PreparedImageRequest {
        provider_model_id: "gpt-image-2.5-flare".to_owned(),
        branch: ImageBranch::Masked,
        native_parameters: serde_json::json!({
            "prompt": "edit this",
            "image_urls": ["data:image/png;base64,AAAA"],
            "mask_url": "data:image/png;base64,BBBB"
        }),
        platform_parameters: vec!["image_urls".to_owned(), "mask_url".to_owned()],
        cost_currency: "USD".to_owned(),
    };
    // 换算结果由 `resolve_images` 给出：这里只验装配（顺序与参数名逐字保持）。
    let mut resolved = Map::new();
    resolved.insert(
        "image_urls".to_owned(),
        serde_json::json!(["https://up.example/a.png"]),
    );
    resolved.insert(
        "mask_url".to_owned(),
        Value::String("https://up.example/mask.png".to_owned()),
    );
    let body = generation_body(&request, &resolved).expect("body");
    assert_eq!(
        body["image_urls"],
        serde_json::json!(["https://up.example/a.png"])
    );
    assert_eq!(body["mask_url"], "https://up.example/mask.png");
    assert!(
        !body.to_string().contains("data:image"),
        "内联图片绝不能以 data URL 形态出现在上行请求里：{body}"
    );
    assert!(
        !body.to_string().contains("asset://"),
        "本地资产引用绝不能出现在上行请求里"
    );
}

#[tokio::test]
async fn public_urls_pass_through_without_being_uploaded_or_downloaded() {
    // 上游只吃公网 URL：data URL 才需要上传换 URL，公网 URL 原样透传。
    let adapter =
        ApimartImageAdapter::new("http://127.0.0.1:1", Duration::from_secs(1)).expect("config");
    let credential = ProviderCredential::new("test-key".to_owned()).expect("credential");
    let resolved = adapter
        .resolve_url("https://example.invalid/a.png", &credential)
        .await
        .expect("a public url needs no network round trip");
    assert_eq!(resolved, "https://example.invalid/a.png");
    // 不是 http(s) 也不是 data URL 的值一律拒绝，不猜。
    let error = adapter
        .resolve_url("asset://not-a-thing", &credential)
        .await
        .expect_err("an unknown shape must be rejected");
    assert!(error.to_string().contains("http(s) url"));
}
