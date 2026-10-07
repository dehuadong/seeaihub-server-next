use async_trait::async_trait;
use bytes::BytesMut;
use futures_util::StreamExt;
use reqwest::{Client, StatusCode};
use seeai_adapter_sdk::{
    AccountingFacts, AdapterDescriptor, AdapterError, DeclaredCost, ExecutionContext,
    GATEWAY_REQUEST_WIRE_BYTES, GatewayAdapter, GatewayByteLimits, GatewayInput, GeneratedImage,
    ImageValueShape, InputImage, ProviderCallError, ProviderCost, ProviderCredential,
    ProviderFailureKind, ProviderOutput, ProviderSuccess, ProviderTraceId,
    QueryAccountingCapability, ResponsePayload, RetrySafety, begin_generation_send,
    declared_microusd, ensure_external_call_allowed, external_call_timeout,
    gateway_passthrough_parameters, insert_wire_parameter,
};
use seeai_application::{AdapterFactory, ApplicationError};
use seeai_domain::ImageBranch;
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use url::Url;

pub const ADAPTER_KEY: &str = "aihubmix-image-v1";
/// 上游响应正文上限：调用方据此计算一次执行的内存预留。
pub const MAX_PROVIDER_RESPONSE_BYTES: usize = 128 * 1024 * 1024;
/// 本 Driver 声明的字节上限。
///
/// 生成响应上限与只读对账读取上限都从这里派生（RFC 0018 §2.1）。这条通路不声明按句柄查询
/// 计量的能力（[`QueryAccountingCapability::Unsupported`]），对账读取上限仍随这份声明一起
/// 存在：它与生成响应上限的关系不需要第二个地方维护。
#[must_use]
fn byte_limits() -> GatewayByteLimits {
    GatewayByteLimits {
        request_wire_bytes: GATEWAY_REQUEST_WIRE_BYTES,
        provider_response_bytes: MAX_PROVIDER_RESPONSE_BYTES,
    }
}

#[derive(Debug, Default)]
pub struct AihubmixAdapterFactory;

impl AdapterFactory for AihubmixAdapterFactory {
    fn descriptor(&self, adapter_key: &str) -> Option<AdapterDescriptor> {
        (adapter_key == ADAPTER_KEY).then_some(AdapterDescriptor {
            key: ADAPTER_KEY,
            supported_top_level_parameters: &[
                "model",
                "prompt",
                "images",
                "mask",
                "n",
                "size",
                "output_format",
                "extra",
            ],
            // `/ai/v1` 把模型专属字段收在 `extra` 里；顶层放这些名字是硬拒绝。
            supported_extra_parameters: &[
                "quality",
                "background",
                "output_compression",
                "moderation",
                "user",
            ],
            supported_branches: &[
                ImageBranch::PromptOnly,
                ImageBranch::ImageConditioned,
                ImageBranch::Masked,
            ],
            max_reference_images: 16,
            // `/ai/v1` 的任务对象给 `usage.cost`（上游声明的实际扣费，可为 null），没有 token
            // 分项：声明"上游给金额"的候选在这条通路上成立。
            declares_cost: true,
            // `/ai/v1` 的任务终态只给上游声明的金额，没有任何 token 分项。
            provides_token_usage: false,
            byte_limits: byte_limits(),
        })
    }

    fn validate_publication(
        &self,
        adapter_key: &str,
        carrier_schema: &Value,
        restrictions: &Value,
    ) -> Result<(), String> {
        if adapter_key != ADAPTER_KEY {
            return Err(format!("unknown adapter {adapter_key}"));
        }
        validate_aihubmix_publication(carrier_schema, restrictions)
    }

    /// 同步网关协议：同一条供给换成 [`GatewayAdapter`] 交出同一个 Driver。
    fn create_gateway(
        &self,
        adapter_key: &str,
        base_url: &str,
        timeout: Duration,
    ) -> Result<std::sync::Arc<dyn GatewayAdapter>, ApplicationError> {
        if adapter_key != ADAPTER_KEY {
            return Err(ApplicationError::Configuration(format!(
                "unsupported adapter {adapter_key}"
            )));
        }
        AihubmixImageAdapter::new(base_url, timeout)
            .map(|adapter| std::sync::Arc::new(adapter) as std::sync::Arc<dyn GatewayAdapter>)
            .map_err(|error| ApplicationError::Configuration(error.to_string()))
    }
}

/// 校验这条供给的**承载面**（它声明要往线文里写的字段面）本 Driver 能不能执行。
///
/// 看承载面而不是合同：合同是客户端那一侧的面（模型级唯一一份），Driver 只关心
/// "这条供给实际要发的字段与分支，本端点能不能收下并跑通"。
fn validate_aihubmix_publication(
    carrier_schema: &Value,
    restrictions: &Value,
) -> Result<(), String> {
    let properties = carrier_schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| "carrier schema properties are required".to_owned())?;
    let required = carrier_schema
        .get("required")
        .and_then(Value::as_array)
        .ok_or_else(|| "carrier schema required list is missing".to_owned())?;
    for name in ["model", "prompt"] {
        if !required.iter().any(|value| value.as_str() == Some(name)) {
            return Err(format!("AIHubMix adapter requires native parameter {name}"));
        }
    }
    require_const_string(properties, "model")?;
    require_type(properties, "prompt", "string")?;
    // 参考图是媒体引用：字符串（一张）或字符串数组（多张）。名字取承载面声明的那个
    // （平台按名字形状判角色），平台装载的值就写在这个名字上。
    let reference = ["images", "image"]
        .into_iter()
        .find(|name| properties.contains_key(*name));
    if lets_in_reference_image(restrictions) {
        let reference = reference
            .ok_or_else(|| "carrier schema declares no reference image parameter".to_owned())?;
        require_string_or_string_array(properties, reference)?;
    }
    if properties.contains_key("mask") {
        require_type(properties, "mask", "string")?;
    }
    if properties.contains_key("n") {
        require_type(properties, "n", "integer")?;
    }
    for name in ["size", "output_format"] {
        if properties.contains_key(name) {
            // 端点 schema 对这两项只声明类型（连枚举都没有），所以枚举可有可无：声明了就必须是
            // 字符串枚举，没声明就按普通字符串发出去。
            require_string_with_optional_enum(properties, name)?;
        }
    }
    // 模型专属字段只能落在 `extra` 里：顶层放这些名字在 `/ai/v1` 上是硬拒绝。
    if properties.contains_key("extra") {
        require_type(properties, "extra", "object")?;
    }
    let extra = properties
        .get("extra")
        .and_then(|value| value.get("properties"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if extra.contains_key("quality") {
        require_string_with_optional_enum(&extra, "quality")?;
    }
    for name in ["background", "moderation"] {
        if extra.contains_key(name) {
            require_string_enum(&extra, name)?;
        }
    }
    if extra.contains_key("output_compression") {
        require_type(&extra, "output_compression", "integer")?;
    }
    if extra.contains_key("user") {
        require_type(&extra, "user", "string")?;
    }
    let validator = jsonschema::validator_for(carrier_schema).map_err(|error| error.to_string())?;
    let model = properties
        .get("model")
        .and_then(|value| value.get("const"))
        .and_then(Value::as_str)
        .ok_or_else(|| "model const is required".to_owned())?;
    let allowed = restrictions
        .get("allowed_branches")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| {
            vec![
                Value::String("prompt_only".to_owned()),
                Value::String("image_conditioned".to_owned()),
                Value::String("masked".to_owned()),
            ]
        });
    // 最小请求用的图片取值是生成入口收的公网 URL 形态。
    // 平台不再有"资产引用"这种值，承载面也不该按它校验。
    // 取值要跟着**这份承载面自己声明的形态**走：声明成数组就给只装一张的数组，否则最小请求本身
    // 就被这份 schema 判成非法，拒它的理由（"连最小请求都过不了"）是假的。
    let image = "https://example.invalid/reference.png";
    let mask = "https://example.invalid/mask.png";
    let mut cases = vec![(
        "prompt_only",
        serde_json::json!({"model": model, "prompt": "x"}),
    )];
    // 收图分支的最小请求只在承载面声明了参考图字段时才有判据：纯文生图的候选没有这个名字。
    if let Some(reference) = reference {
        let reference_value = match properties
            .get(reference)
            .and_then(|field| field.get("type"))
            .and_then(Value::as_str)
        {
            Some("array") => serde_json::json!([image]),
            _ => serde_json::json!(image),
        };
        let mut image_case = Map::new();
        image_case.insert("model".to_owned(), Value::String(model.to_owned()));
        image_case.insert("prompt".to_owned(), Value::String("x".to_owned()));
        image_case.insert(reference.to_owned(), reference_value);
        let mut mask_case = image_case.clone();
        mask_case.insert("mask".to_owned(), Value::String(mask.to_owned()));
        cases.push(("image_conditioned", Value::Object(image_case)));
        cases.push(("masked", Value::Object(mask_case)));
    }
    for (branch, instance) in cases {
        if allowed.iter().any(|value| value.as_str() == Some(branch))
            && !validator.is_valid(&instance)
        {
            return Err(format!(
                "carrier schema rejects the adapter's minimal {branch} request"
            ));
        }
    }
    for invalid in [
        serde_json::json!({"model": model}),
        serde_json::json!({"model": model, "prompt": 1}),
        serde_json::json!({"model": model, "prompt": "x", "unknown": true}),
        // 遮罩不能脱离参考图：这份承载面必须自己把这种请求判成非法。
        serde_json::json!({"model": model, "prompt": "x", "mask": mask}),
    ] {
        if validator.is_valid(&invalid) {
            return Err(
                "carrier schema accepts an input the adapter cannot safely execute".to_owned(),
            );
        }
    }
    Ok(())
}

/// 候选声明的分支里有收图的那两个（图生图 / 带遮罩）吗。
fn lets_in_reference_image(restrictions: &Value) -> bool {
    restrictions
        .get("allowed_branches")
        .and_then(Value::as_array)
        .is_some_and(|branches| {
            branches
                .iter()
                .any(|branch| matches!(branch.as_str(), Some("image_conditioned" | "masked")))
        })
}

fn require_type(properties: &Map<String, Value>, name: &str, expected: &str) -> Result<(), String> {
    if properties
        .get(name)
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str)
        == Some(expected)
    {
        Ok(())
    } else {
        Err(format!("native parameter {name} must have type {expected}"))
    }
}

fn require_const_string(properties: &Map<String, Value>, name: &str) -> Result<(), String> {
    if properties
        .get(name)
        .and_then(|value| value.get("const"))
        .and_then(Value::as_str)
        .is_some()
    {
        Ok(())
    } else {
        Err(format!(
            "native parameter {name} must declare a string const"
        ))
    }
}

/// 参考图参数：单值字符串、或字符串数组（同一个部件名重复出现，就是 multipart 里的列表形态）。
///
/// 两种都表示得出来，所以两种都算声明得成形状；声明成别的（数字、对象、数组里不是字符串）就是
/// 这条供给说了本 Driver 发不出去的形态，发布期直接拒绝。
fn require_string_or_string_array(
    properties: &Map<String, Value>,
    name: &str,
) -> Result<(), String> {
    let field = properties.get(name).expect("caller checked the field");
    match field.get("type").and_then(Value::as_str) {
        Some("string") => Ok(()),
        Some("array") => {
            let items = field
                .get("items")
                .ok_or_else(|| format!("native parameter {name} array must declare items"))?;
            if items.get("type").and_then(Value::as_str) == Some("string") {
                Ok(())
            } else {
                Err(format!(
                    "native parameter {name} array items must be strings"
                ))
            }
        }
        _ => Err(format!(
            "native parameter {name} must be a string or an array of strings"
        )),
    }
}

fn require_string_enum(properties: &Map<String, Value>, name: &str) -> Result<(), String> {
    let values = properties
        .get(name)
        .and_then(|value| value.get("enum"))
        .and_then(Value::as_array)
        .ok_or_else(|| format!("native parameter {name} must declare an enum"))?;
    if values.is_empty() || values.iter().any(|value| !value.is_string()) {
        Err(format!("native parameter {name} enum must contain strings"))
    } else {
        Ok(())
    }
}

/// 字符串类型的参数：枚举可有可无（端点 schema 对 `size` / `output_format` 没给枚举）。
fn require_string_with_optional_enum(
    properties: &Map<String, Value>,
    name: &str,
) -> Result<(), String> {
    let Some(field) = properties.get(name) else {
        return Ok(());
    };
    if field.get("type").and_then(Value::as_str) != Some("string") {
        return Err(format!("native parameter {name} must have type string"));
    }
    match field.get("enum").and_then(Value::as_array) {
        None => Ok(()),
        Some(values) if values.is_empty() || values.iter().any(|value| !value.is_string()) => {
            Err(format!("native parameter {name} enum must contain strings"))
        }
        Some(_) => Ok(()),
    }
}

/// 按传输策略复用的进程级 HTTP Client。
///
/// 策略目前只有单次调用超时：TLS、代理与连接池都用 `reqwest` 默认值，不随渠道变化。
/// 同一策略的 Client 全局共用，连接池随之跨请求、跨 Channel 复用；凭证仍按请求设置
/// （`bearer_auth`），绝不放进 Client 的默认头（RFC 0017 §4）。
///
/// 不复用另一家 adapter 的 Client：两者的超时来源不同，跨 crate 共用需要一个额外的共享属主，
/// 只增加耦合而不改变"按传输策略复用"这一要求。
fn shared_client(timeout: Duration) -> Result<Arc<Client>, AdapterError> {
    static CLIENTS: OnceLock<Mutex<HashMap<Duration, Arc<Client>>>> = OnceLock::new();
    let clients = CLIENTS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut clients = clients
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(client) = clients.get(&timeout) {
        return Ok(Arc::clone(client));
    }
    let client = Arc::new(
        Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|error| AdapterError::Configuration(error.to_string()))?,
    );
    clients.insert(timeout, Arc::clone(&client));
    Ok(client)
}

#[derive(Debug, Clone)]
pub struct AihubmixImageAdapter {
    client: Arc<Client>,
    base_url: Url,
    /// 单次 HTTP 调用的配置超时；新协议再用总期限剩余夹一次。
    timeout: Duration,
}

impl AihubmixImageAdapter {
    pub fn new(base_url: &str, timeout: Duration) -> Result<Self, AdapterError> {
        let normalized = format!("{}/", base_url.trim_end_matches('/'));
        let base_url = Url::parse(&normalized)
            .map_err(|error| AdapterError::Configuration(error.to_string()))?;
        let client = shared_client(timeout)?;
        Ok(Self {
            client,
            base_url,
            timeout,
        })
    }

    /// 上游只有一个图片端点：文生图与图生图 / 编辑都走它，分支由请求里有没有图片字段决定。
    fn endpoint(&self) -> Result<Url, AdapterError> {
        self.base_url
            .join("ai/v1/images/generations")
            .map_err(|error| AdapterError::Configuration(error.to_string()))
    }
}

/// 一次执行的上游请求体：文生图与图生图是同一份形状，差别只在有没有图片字段。
///
/// 普通参数逐字进字段（名字带点的落进容器，见 [`insert_wire_parameter`]），模型专属字段由
/// 承载面与映射落在 `extra` 里。参考图与遮罩是**媒体引用**：公网 URL 字符串或字符串数组，
/// 名字取自 [`GatewayInput::image_sites`]，Driver 不改写；写数组还是写单值按承载面的声明。
fn gateway_body(input: &GatewayInput) -> Result<Value, AdapterError> {
    let mut object = Map::new();
    insert_wire_parameter(
        &mut object,
        "model",
        Value::String(input.provider_model_id.clone()),
    )?;
    insert_wire_parameter(
        &mut object,
        "prompt",
        Value::String(required_string(&input.native_parameters, "/prompt")?),
    )?;
    for (name, value) in gateway_passthrough_parameters(input) {
        insert_wire_parameter(&mut object, name, value.clone())?;
    }
    if !input.reference_images.is_empty() {
        let site = input.image_sites.reference.as_ref().ok_or_else(|| {
            AdapterError::UnsupportedInput(
                "the offering declares no reference image parameter, so the request has no field \
                 to carry it"
                    .to_owned(),
            )
        })?;
        insert_wire_parameter(
            &mut object,
            &site.parameter,
            image_value(site.shape, &input.reference_images, "reference image")?,
        )?;
    }
    if let Some(mask) = &input.mask {
        let site = input.image_sites.mask.as_ref().ok_or_else(|| {
            AdapterError::UnsupportedInput(
                "the offering declares no mask parameter, so the request has no field to carry it"
                    .to_owned(),
            )
        })?;
        insert_wire_parameter(
            &mut object,
            &site.parameter,
            image_value(site.shape, std::slice::from_ref(mask), "mask")?,
        )?;
    }
    Ok(Value::Object(object))
}

/// 一处图片位的写入形态：承载面声明数组就写字符串数组，声明单值就写单个 URL；
/// 单值位收到多张时明确失败——那是承载面与发布声明互相矛盾，不静默取一张。
fn image_value(
    shape: ImageValueShape,
    images: &[InputImage],
    role: &str,
) -> Result<Value, AdapterError> {
    match shape {
        ImageValueShape::Array => Ok(Value::Array(
            images
                .iter()
                .map(|image| Value::String(image.as_str().to_owned()))
                .collect(),
        )),
        ImageValueShape::Scalar => match images {
            [image] => Ok(Value::String(image.as_str().to_owned())),
            _ => Err(AdapterError::UnsupportedInput(format!(
                "the {role} parameter carries a single value, but {} were given",
                images.len()
            ))),
        },
    }
}

/// 一次成功执行 → 内存载荷与强类型账务事实（RFC 0017 §2）；图片与账务事实分开，结果不进日志。
fn gateway_output(success: ProviderSuccess) -> ProviderOutput {
    let image_count = u32::try_from(success.images.len()).unwrap_or(u32::MAX);
    ProviderOutput {
        response_payload: ResponsePayload {
            images: success.images,
        },
        accounting_facts: AccountingFacts {
            usage: success.usage,
            provider_cost: success.provider_cost,
            image_count,
            response_digest: success.response_digest,
            provider_trace_id: success.provider_trace_id,
        },
    }
}

impl AihubmixImageAdapter {
    /// 一次同步执行：组请求体 → 过发送闸 → POST → 解码完成态任务对象。
    async fn gateway_call(
        &self,
        input: &GatewayInput,
        credential: &ProviderCredential,
        context: &dyn ExecutionContext,
    ) -> Result<ProviderOutput, AdapterError> {
        let body = gateway_body(input)?;
        ensure_external_call_allowed(context)?;
        // 生成发送的最后资格：与取消线性化。此后到 .send() 之间没有可取消的等待。
        begin_generation_send(context)?;
        let response = self
            .client
            .post(self.endpoint()?)
            .timeout(external_call_timeout(self.timeout, context))
            .bearer_auth(credential.expose())
            .json(&body)
            .send()
            .await
            .map_err(ambiguous_transport_error)?;
        parse_response(response, &input.cost_currency)
            .await
            .map(gateway_output)
    }
}

/// 同步网关协议（RFC 0017 §4）：同步渠道不伪造可恢复句柄，也不声明按句柄查询计量。
#[async_trait]
impl GatewayAdapter for AihubmixImageAdapter {
    fn key(&self) -> &'static str {
        ADAPTER_KEY
    }

    /// 同步通路没有已验证的按 trace 恢复能力：不声明可查询计量。
    fn query_accounting_capability(&self) -> QueryAccountingCapability {
        QueryAccountingCapability::Unsupported
    }

    async fn execute(
        &self,
        input: Arc<GatewayInput>,
        context: &dyn ExecutionContext,
        credential: &ProviderCredential,
    ) -> Result<ProviderOutput, AdapterError> {
        // 同步渠道只在响应里体现已受理：没有可持久化、可按 trace 恢复的句柄，
        // 因此绝不调用 `context.accepted`。
        // 分支不选端点：上游只有一个图片端点，请求里有没有图片字段就是分支。
        let output = self.gateway_call(&input, credential, context).await;
        output.map_err(gateway_error)
    }
}

/// 新协议的错误出口（RFC 0017 §4）：Provider 报文自由文本与 reqwest 原始串可能带响应正文、
/// URL 查询或 multipart，这里只保留平台判出的码与处置，不把原文带出去。
fn gateway_error(error: AdapterError) -> AdapterError {
    match error {
        AdapterError::Provider(mut call) => {
            call.message = "the provider call failed".to_owned();
            AdapterError::Provider(call)
        }
        other => other,
    }
}

fn required_string(parameters: &Value, pointer: &str) -> Result<String, AdapterError> {
    parameters
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            AdapterError::UnsupportedInput(format!("missing required string at {pointer}"))
        })
}

fn ambiguous_transport_error(error: reqwest::Error) -> AdapterError {
    ProviderCallError {
        code: "provider_transport_unknown".to_owned(),
        message: error.to_string(),
        trace_id: None,
        retry_safety: RetrySafety::AcceptanceUnknown,
        kind: ProviderFailureKind::UpstreamUnavailable,
        // 响应都没读到，成本无从谈起。
        provider_cost: None,
    }
    .into()
}

async fn parse_response(
    response: reqwest::Response,
    cost_currency: &str,
) -> Result<ProviderSuccess, AdapterError> {
    let status = response.status();
    // 对账标识：上游的逐请求标识，只用于对账，不参与计价（见 CONTEXT.md 的 Generation Attempt）。
    // 响应头可能带回 URL 或任意正文，入口就按有界标识构造，非法值按"没有可信 trace"丢弃。
    let provider_trace_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .and_then(ProviderTraceId::parse);
    if response
        .content_length()
        .is_some_and(|length| length > MAX_PROVIDER_RESPONSE_BYTES as u64)
    {
        return Err(provider_response_too_large());
    }
    let mut body = BytesMut::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(ambiguous_transport_error)?;
        if body.len().saturating_add(chunk.len()) > MAX_PROVIDER_RESPONSE_BYTES {
            return Err(provider_response_too_large());
        }
        body.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        return Err(parse_provider_error(status, &body).into());
    }
    success_from_body(&body, provider_trace_id, cost_currency)
}

/// 完成态任务对象 → 成功结果。结构、金额与结果信封都按上游给的原形；token 分项这条通路没有。
///
/// 与传输分开，是为了让“响应读成了、却不能用”这几种判定只在这一处发生，也能直接在用例里喂一份
/// 响应体验证——它们都要带上这次执行的成本事实。
fn success_from_body(
    body: &[u8],
    provider_trace_id: Option<ProviderTraceId>,
    cost_currency: &str,
) -> Result<ProviderSuccess, AdapterError> {
    let digest = sha256_hex(body);
    let parsed: TaskResponse = serde_json::from_slice(body).map_err(|error| {
        AdapterError::Provider(ProviderCallError {
            code: "provider_response_invalid".to_owned(),
            message: error.to_string(),
            trace_id: None,
            retry_safety: RetrySafety::AcceptanceUnknown,
            kind: ProviderFailureKind::UpstreamUnavailable,
            // 响应体读不成结构：金额也取不到。
            provider_cost: None,
        })
    })?;
    // HTTP 200 也可能是失败任务：终态由 status 说，error 给原因。
    if parsed.status != "completed" {
        return Err(task_failure(&parsed, cost_currency));
    }
    // 任务 id 优先取响应体里的；响应头那条 x-request-id 作兜底。
    let trace = parsed
        .id
        .as_deref()
        .and_then(ProviderTraceId::parse)
        .or(provider_trace_id);
    let provider_cost = declared_cost(parsed.usage.as_ref(), cost_currency);
    let images = images_from_task(parsed.output, &provider_cost)?;
    if images.is_empty() {
        return Err(AdapterError::Provider(ProviderCallError {
            code: "provider_result_empty".to_owned(),
            message: "provider returned no images".to_owned(),
            trace_id: trace,
            retry_safety: RetrySafety::AcceptanceUnknown,
            kind: ProviderFailureKind::UpstreamUnavailable,
            provider_cost: Some(provider_cost),
        }));
    }
    Ok(ProviderSuccess {
        images,
        // 这条通路没有 token 分项：声明了成本的渠道，成功件允许没有计量证据（设计 0022 §5）。
        usage: None,
        response_digest: digest,
        provider_trace_id: trace,
        provider_cost,
    })
}

/// 失败任务 → 平台错误：终态之后判定失败，上游声明的那笔金额照带。
fn task_failure(parsed: &TaskResponse, cost_currency: &str) -> AdapterError {
    let code = parsed
        .error
        .as_ref()
        .and_then(|error| error.code.clone())
        .unwrap_or_else(|| format!("provider_task_{}", parsed.status));
    let message = parsed
        .error
        .as_ref()
        .and_then(|error| error.message.clone())
        .unwrap_or_else(|| "the provider task ended without a result".to_owned());
    AdapterError::Provider(ProviderCallError {
        code,
        message,
        trace_id: parsed.id.as_deref().and_then(ProviderTraceId::parse),
        retry_safety: RetrySafety::AcceptanceUnknown,
        kind: ProviderFailureKind::UpstreamRejected,
        provider_cost: Some(declared_cost(parsed.usage.as_ref(), cost_currency)),
    })
}

/// 上游声明的金额 → 成本事实：cost 缺席或读不出来时按成本缺口记，不猜金额。
fn declared_cost(usage: Option<&TaskUsage>, cost_currency: &str) -> ProviderCost {
    let Some(value) = usage.and_then(|usage| usage.cost.as_ref()) else {
        return ProviderCost::Unavailable;
    };
    match declared_microusd(value) {
        Some(amount_microusd) => ProviderCost::Declared(DeclaredCost {
            amount_microusd,
            currency: cost_currency.to_owned(),
        }),
        None => ProviderCost::Unavailable,
    }
}

/// 结果信封：每项只取一种形态（Spec 0005 §3）。content_url 要平台凭据、且上游列为有下载次数
/// 上限的资源，因此既不交给调用方也不由平台下载；这里只认内联的 b64_json，缺了就按结果缺失失败，
/// 并把**已经读到的金额**随错误交回平台——终态之后判定失败不得丢掉成本事实。
fn images_from_task(
    output: Vec<TaskOutput>,
    provider_cost: &ProviderCost,
) -> Result<Vec<GeneratedImage>, AdapterError> {
    let mut images = Vec::with_capacity(output.len());
    for item in output {
        match item.b64_json {
            Some(b64_json) => images.push(GeneratedImage::from_base64(b64_json)),
            None => {
                return Err(AdapterError::Provider(ProviderCallError {
                    code: "provider_result_missing".to_owned(),
                    message: "response item carries no inline image".to_owned(),
                    trace_id: None,
                    retry_safety: RetrySafety::AcceptanceUnknown,
                    kind: ProviderFailureKind::UpstreamUnavailable,
                    provider_cost: Some(provider_cost.clone()),
                }));
            }
        }
    }
    Ok(images)
}

fn provider_response_too_large() -> AdapterError {
    ProviderCallError {
        code: "provider_response_too_large".to_owned(),
        message: "provider response exceeded the configured safety limit".to_owned(),
        trace_id: None,
        retry_safety: RetrySafety::AcceptanceUnknown,
        kind: ProviderFailureKind::UpstreamUnavailable,
        // 响应被截断/丢掉了，用量读不全，自算也就无从下手。
        provider_cost: None,
    }
    .into()
}

fn parse_provider_error(status: StatusCode, body: &[u8]) -> ProviderCallError {
    let parsed = serde_json::from_slice::<ErrorEnvelope>(body).ok();
    let code = parsed
        .as_ref()
        .and_then(|value| value.error.code.clone())
        .unwrap_or_else(|| format!("http_{}", status.as_u16()));
    let message = parsed
        .as_ref()
        .map(|value| value.error.message.clone())
        .unwrap_or_else(|| "provider returned an error without a JSON body".to_owned());
    // 错误体里的 tid 与成功响应头同源：同样按有界标识构造，非法值丢弃（Spec 0005 §2）。
    let trace_id =
        parsed.and_then(|value| value.error.tid.as_deref().and_then(ProviderTraceId::parse));
    let retry_safety = if status == StatusCode::TOO_MANY_REQUESTS
        || matches!(
            code.as_str(),
            "service_unavailable"
                | "upstream_rate_limited"
                | "upstream_unreachable"
                | "upstream_bad_response"
        )
        || status.is_server_error()
    {
        RetrySafety::AcceptanceUnknown
    } else {
        RetrySafety::NotRetryable
    };
    // 平台侧失败类别：与 `retry_safety` 用的是同一批信号，但结论是另一个维度。
    let http_status = status.as_u16();
    let kind = match code.as_str() {
        // 渠道侧账户余额不足，属于平台自己的账户问题。
        "insufficient_user_quota" => ProviderFailureKind::PlatformFunding,
        "http_401" | "http_403" => ProviderFailureKind::PlatformCredential,
        "http_429" => ProviderFailureKind::UpstreamRateLimited,
        "http_400" => ProviderFailureKind::UpstreamRejected,
        _ if status.is_server_error() => ProviderFailureKind::UpstreamUnavailable,
        // 没有可用 `code` 时只能看状态码。
        _ => match http_status {
            401 | 403 => ProviderFailureKind::PlatformCredential,
            429 => ProviderFailureKind::UpstreamRateLimited,
            400 => ProviderFailureKind::UpstreamRejected,
            _ => ProviderFailureKind::Unknown,
        },
    };
    ProviderCallError {
        code,
        message,
        trace_id,
        retry_safety,
        kind,
        // 渠道用错误响应回话：这次执行没有金额可读（本就没有金额字段的渠道更谈不上）。
        provider_cost: None,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[derive(Debug, Deserialize)]
struct TaskResponse {
    /// 上游任务 id：落到 Attempt 的 provider_trace_id。
    #[serde(default)]
    id: Option<String>,
    status: String,
    #[serde(default)]
    output: Vec<TaskOutput>,
    #[serde(default)]
    error: Option<TaskError>,
    #[serde(default)]
    usage: Option<TaskUsage>,
}

#[derive(Debug, Deserialize)]
struct TaskOutput {
    #[serde(default)]
    b64_json: Option<String>,
    /// 要平台凭据的结果地址：平台不下载、也不交给调用方。
    #[serde(default)]
    #[allow(dead_code)]
    content_url: Option<String>,
}

/// 上游声明的金额：这条通路只有 cost，没有 token 分项。
#[derive(Debug, Deserialize)]
struct TaskUsage {
    #[serde(default)]
    cost: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct TaskError {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    message: String,
    code: Option<String>,
    tid: Option<String>,
}

#[cfg(test)]
mod tests;
