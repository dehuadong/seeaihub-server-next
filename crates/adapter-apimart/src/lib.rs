//! APIMart 的图片生成 Driver（② 层）。
//!
//! 本 Driver 的执行形态与渠道事实如下：
//! 上游是**任务式**（提交拿 `task_id`，轮询到终态，再拿结果地址）——这是**本 Driver 内部**
//! 的实现细节：平台对外是同步的，调用方既看不到 task id，也没有任何"去查任务"的协议。
//! 因此提交、轮询、取结果都在 `execute` 内完成。
//!
//! 计费相关：任务成功响应含**四分项 `usage`**（`input_tokens_details` 区分 text/image，
//! 另有 `cached_tokens`），归一到领域 `TokenUsage`。计量事实以这四分项 token 为准；
//! 终态里的 `cost` 另外**采纳为成本事实**（成本平面，币种按渠道声明）——它含渠道侧的账号
//! 折扣，比平台自算更权威，所以直接取它，不自己算。金额只进成本口径，**不替代计量事实**，
//! 也不参与对客金额。`credits_cost` 仍不采纳：它只是 `cost` 的另一个刻度，不带来新事实。

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use reqwest::{Client, StatusCode};
use seeai_adapter_sdk::{
    AcceptanceError, AcceptedHandle, AccountingFacts, AccountingQuery, AdapterDescriptor,
    AdapterError, Deadline, DeclaredCost, ExecutionContext, GATEWAY_REQUEST_WIRE_BYTES,
    GatewayAdapter, GatewayByteLimits, GatewayInput, GeneratedImage, ImageValueShape, InputImage,
    ProviderCallError, ProviderCost, ProviderCredential, ProviderFailureKind, ProviderOutput,
    ProviderTaskHandle, ProviderTaskState, ProviderTraceId, QueryAccountingCapability,
    ResponsePayload, RetrySafety, begin_generation_send, ensure_external_call_allowed,
    ensure_read_call_allowed, external_call_timeout, gateway_passthrough_parameters,
};
use seeai_application::{AdapterFactory, ApplicationError};
use seeai_domain::{ImageBranch, TokenUsage};
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use url::Url;

pub const ADAPTER_KEY: &str = "apimart-image-v1";

/// 上游响应正文上限：调用方据此计算一次执行的内存预留。
pub const MAX_PROVIDER_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// 生成路径读上游响应时的超限错误码。
const RESPONSE_TOO_LARGE_CODE: &str = "provider_response_too_large";
/// 只读对账查询响应超过它自己的上限时的错误码：按证据缺口处置，不重试重读。
const RECONCILIATION_READ_EXCEEDED_CODE: &str = "reconciliation_read_limit_exceeded";
/// 轮询间隔。文档建议 2~5 秒，取偏小值以缩短 Job 驻留时间。
const POLL_INTERVAL: Duration = Duration::from_secs(3);
/// 单次任务查询的瞬时失败重试次数上限（幂等读才允许重试）。
const QUERY_RETRY_LIMIT: u32 = 3;
/// 查询重试的退避基数：第 n 次失败后等待 `n × 基数`。
const QUERY_RETRY_BACKOFF: Duration = Duration::from_millis(500);
/// 单次 HTTP 调用的超时（提交与轮询各自适用）。整轮耗时由 `deadline` 约束。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// 本 Driver 声明的字节上限。
///
/// 生成响应上限与只读对账读取上限都从这里派生：对账读取用一条独立、更小但明确的上限，由
/// [`GatewayByteLimits::reconciliation_read_bytes`] 收口（RFC 0018 §2.1）。
#[must_use]
fn byte_limits() -> GatewayByteLimits {
    GatewayByteLimits {
        request_wire_bytes: GATEWAY_REQUEST_WIRE_BYTES,
        provider_response_bytes: MAX_PROVIDER_RESPONSE_BYTES,
    }
}

#[derive(Debug, Default)]
pub struct ApimartAdapterFactory;

impl AdapterFactory for ApimartAdapterFactory {
    fn descriptor(&self, adapter_key: &str) -> Option<AdapterDescriptor> {
        (adapter_key == ADAPTER_KEY).then_some(AdapterDescriptor {
            key: ADAPTER_KEY,
            supported_top_level_parameters: &[
                "model",
                "prompt",
                "n",
                "size",
                "resolution",
                "quality",
                "output_format",
                "output_compression",
                "background",
                "moderation",
                "image_urls",
                "mask_url",
            ],
            supported_extra_parameters: &[],
            // 参考图与遮罩只收**公网可访问的 HTTP(S) URL**：上游明确不再接受在生成请求里直接传
            // base64，所以 Driver 把取值逐字透传、不下载也不上传（属 ② 层内部实现，不外泄到平台）。
            //
            // 注意：这里声明的是**本 Driver 已实现的能力面**，不等于"已获准发布"。
            // 两个 APIMart 素材的 `allowed_branches` 已在受控验证后开放三条分支；
            // 未经真实 wire 验证的能力不开，且素材本身仍是"草案 · 未发布"。
            supported_branches: &[
                ImageBranch::PromptOnly,
                ImageBranch::ImageConditioned,
                ImageBranch::Masked,
            ],
            max_reference_images: 16,
            // APIMart 的终态另带 `cost`（实扣金额，含渠道侧折扣）：声明"上游给金额"的候选
            // 在这条通路上成立。
            declares_cost: true,
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
        validate_apimart_publication(carrier_schema, restrictions)
    }

    /// 同步网关协议：同一条供给换成 [`GatewayAdapter`] 交出同一个 Driver。
    fn create_gateway(
        &self,
        adapter_key: &str,
        base_url: &str,
        timeout: Duration,
    ) -> Result<Arc<dyn GatewayAdapter>, ApplicationError> {
        if adapter_key != ADAPTER_KEY {
            return Err(ApplicationError::Configuration(format!(
                "unsupported adapter {adapter_key}"
            )));
        }
        ApimartImageAdapter::new(base_url, timeout)
            .map(|adapter| Arc::new(adapter) as Arc<dyn GatewayAdapter>)
            .map_err(|error| ApplicationError::Configuration(error.to_string()))
    }
}

/// 校验这条供给的**承载面**（它声明要往线文里写的字段面）本 Driver 能不能执行。
///
/// 看承载面而不是合同：合同是客户端那一侧的面（模型级唯一一份），Driver 只关心
/// "这条供给实际要发的字段，本端点能不能收下"。
fn validate_apimart_publication(
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
            return Err(format!("APIMart adapter requires native parameter {name}"));
        }
    }
    match properties.get("model").and_then(|value| value.get("const")) {
        Some(Value::String(text)) if !text.is_empty() => {}
        _ => return Err("model must be declared as a non-empty const".to_owned()),
    }
    match properties.get("prompt").and_then(|value| value.get("type")) {
        Some(Value::String(kind)) if kind == "string" => {}
        _ => return Err("prompt must be declared as a string".to_owned()),
    }
    // 本 Driver 只支持文生图与图生图/编辑；其余原生参数按需校验类型。
    if let Some(n) = properties.get("n")
        && n.get("type").and_then(Value::as_str) != Some("integer")
    {
        return Err("n must be declared as an integer".to_owned());
    }
    if let Some(images) = properties.get("image_urls")
        && images.get("type").and_then(Value::as_str) != Some("array")
    {
        return Err("image_urls must be declared as an array".to_owned());
    }
    // 这里读的是**输入参考图**张数上限：APIMart 的 uploads 端点一次能换回的图 URL 就那么几张，
    // 与"这次要出几张图"（合同里的 `n`）无关。
    let max_reference_images = restrictions
        .get("max_reference_images")
        .and_then(Value::as_u64)
        .unwrap_or(1);
    if max_reference_images > 16 {
        return Err("APIMart supports at most 16 reference images".to_owned());
    }
    Ok(())
}

/// 按传输策略复用的进程级 HTTP Client。
///
/// 策略目前只有单次调用超时（本 Driver 固定为 [`REQUEST_TIMEOUT`]）：TLS、代理与连接池都用
/// `reqwest` 默认值，不随渠道变化。同一策略的 Client 全局共用，连接池随之跨请求、跨 Channel
/// 复用；凭证仍按请求设置（`bearer_auth`），绝不放进 Client 的默认头（RFC 0017 §4）。
///
/// 不复用另一家 adapter 的 Client：跨 crate 共用需要一个额外的共享属主，只增加耦合而不改变
/// "按传输策略复用"这一要求。
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

pub struct ApimartImageAdapter {
    client: Arc<Client>,
    base_url: Url,
    /// 只读对账查询的响应上限：独立于生成响应上限，超限只留证据缺口（RFC 0018 §2.1）。
    reconciliation_read_bytes: usize,
}

impl ApimartImageAdapter {
    /// 单次调用由执行期限与 [`REQUEST_TIMEOUT`] 夹住，装配期传入的 `_timeout` 不再单独持有。
    pub fn new(base_url: &str, _timeout: Duration) -> Result<Self, AdapterError> {
        let normalized = format!("{}/", base_url.trim_end_matches('/'));
        let base_url = Url::parse(&normalized)
            .map_err(|error| AdapterError::Configuration(error.to_string()))?;
        let client = shared_client(REQUEST_TIMEOUT)?;
        Ok(Self {
            client,
            base_url,
            reconciliation_read_bytes: byte_limits().reconciliation_read_bytes(),
        })
    }

    fn endpoint(&self, path: &str) -> Result<Url, AdapterError> {
        self.base_url
            .join(path)
            .map_err(|error| AdapterError::Configuration(error.to_string()))
    }

    /// 单次任务查询（含幂等读的有界退避重试）。`context` 给出取消与总期限时，每次尝试前
    /// 都过闸，单次超时用 min(自身配置, 剩余)（RFC 0017 §6）。
    ///
    /// `budget` 决定这次读取用哪条上限：生成路径用生成响应上限，只读对账用它自己更小的上限。
    /// 超限是否重试也随预算定：对账路径不重读同一份超限响应，直接交回给调用方按证据缺口处置。
    async fn query_task_with(
        &self,
        task_id: &str,
        credential: &ProviderCredential,
        context: Option<&dyn ExecutionContext>,
        budget: ReadBudget,
    ) -> Result<Bytes, AdapterError> {
        let mut attempt = 0_u32;
        loop {
            let timeout = match context {
                Some(context) => {
                    // 轮询可能发生在上游已经受理之后：这里的取消不证明未受理。
                    ensure_read_call_allowed(context)?;
                    external_call_timeout(REQUEST_TIMEOUT, context)
                }
                None => REQUEST_TIMEOUT,
            };
            let url = self.endpoint(&format!("v1/tasks/{task_id}"))?;
            let outcome = match self
                .client
                .get(url)
                .timeout(timeout)
                .bearer_auth(credential.expose())
                .send()
                .await
            {
                Ok(response) => read_body_within(response, budget).await,
                Err(error) => Err(ambiguous_transport_error(error)),
            };
            match outcome {
                Ok(body) => return Ok(body),
                Err(error) => {
                    if !budget.retry_on_overflow && budget.is_overflow(&error) {
                        return Err(error);
                    }
                    attempt += 1;
                    if attempt > QUERY_RETRY_LIMIT {
                        return Err(error);
                    }
                    let backoff = QUERY_RETRY_BACKOFF * attempt;
                    let backoff = match context {
                        Some(context) => backoff.min(context.deadline().remaining()),
                        None => backoff,
                    };
                    tokio::time::sleep(backoff).await;
                }
            }
        }
    }
}

/// 给"提交之后"的失败补上 task id，**不改** `code` / `message` / `retry_safety` / `provider_cost`。
///
/// 为什么需要：进对账的 Job 只能靠人工去上游查，而查的依据就是这个 task id。
/// 提交成功后它就在手里——不附上的话，`attempts.provider_trace_id` 会是空的，
/// 对账的人连"该查哪个任务"都不知道。（**这才是"对账标识"的用途**；
/// 拿它自动去补齐结果属于"跨调用恢复"，本阶段不做。）
fn with_task_id(error: AdapterError, task_id: &str) -> AdapterError {
    match error {
        AdapterError::Provider(mut provider) => {
            if provider.trace_id.is_none() {
                provider.trace_id = ProviderTraceId::parse(task_id);
            }
            AdapterError::Provider(provider)
        }
        other => other,
    }
}

/// 给终态之后的失败附上"这一笔已经花了多少"：**只加** `provider_cost`，`code` / `message` /
/// `retry_safety` / `kind` / `trace_id` 原样。
///
/// 终态里的金额与结果无关：读到它之后才失败，那笔成本是既成事实，而失败件是它唯一的落点。
/// `AdapterError` 的其余变体没有位置承载成本，原样返回。
fn with_provider_cost(error: AdapterError, provider_cost: ProviderCost) -> AdapterError {
    match error {
        AdapterError::Provider(mut provider) => {
            if provider.provider_cost.is_none() {
                provider.provider_cost = Some(provider_cost);
            }
            AdapterError::Provider(provider)
        }
        other => other,
    }
}

fn required_string(parameters: &Value, pointer: &str) -> Result<Value, AdapterError> {
    match parameters.pointer(pointer) {
        Some(Value::String(text)) if !text.is_empty() => Ok(Value::String(text.clone())),
        _ => Err(AdapterError::UnsupportedInput(format!(
            "native parameter {pointer} must be a non-empty string"
        ))),
    }
}

/// 任务成功响应里的计量事实 → 领域 [`TokenUsage`]。
#[derive(Debug, Deserialize)]
struct UsageBody {
    input_tokens: u64,
    #[serde(default)]
    input_tokens_details: Option<UsageDetails>,
    output_tokens: u64,
    #[serde(default)]
    output_tokens_details: Option<UsageDetails>,
    total_tokens: u64,
}

#[derive(Debug, Deserialize)]
struct UsageDetails {
    #[serde(default)]
    image_tokens: u64,
    #[serde(default)]
    text_tokens: u64,
    #[serde(default)]
    cached_tokens: u64,
}

#[derive(Debug, Deserialize)]
struct SubmitEnvelope {
    #[serde(default)]
    data: Vec<SubmitData>,
}

#[derive(Debug, Deserialize)]
struct SubmitData {
    task_id: String,
}

#[derive(Debug, Deserialize)]
struct TaskEnvelope {
    data: TaskData,
}

#[derive(Debug, Deserialize)]
struct TaskData {
    /// 上游报告的任务标识：查询结果必须与句柄一致才算可信关联。
    #[serde(default)]
    id: Option<String>,
    status: String,
    #[serde(default)]
    result: Option<TaskResult>,
    #[serde(default)]
    usage: Option<UsageBody>,
    #[serde(default)]
    error: Option<TaskError>,
    /// 上游在终态直接声明的成本（十进制金额，币种由渠道声明）。
    ///
    /// 用 `Value` 而不是 `f64`：金额要走**精确**换算，浮点在这一步会悄悄差 1 微单位。
    /// 缺字段、负数、非数字都按"这次没拿到金额"处理，绝不猜。
    #[serde(default)]
    cost: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct TaskResult {
    #[serde(default)]
    images: Vec<TaskImage>,
}

#[derive(Debug, Deserialize)]
struct TaskImage {
    #[serde(default)]
    url: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct TaskError {
    #[serde(default)]
    code: Option<i64>,
    #[serde(default)]
    message: String,
}

impl TaskData {
    fn usage(&self) -> Result<TokenUsage, AdapterError> {
        let usage = self.usage.as_ref().ok_or_else(|| {
            provider_error(
                "provider_usage_missing",
                "task response carried no token usage".to_owned(),
                // 已生成但计量缺失：不得猜测费用，进对账。
                RetrySafety::AcceptanceUnknown,
                ProviderFailureKind::Unknown,
            )
        })?;
        // 分项缺失时**不猜测**文本/图片的划分——直接失败（缺字段不得猜测费用）。
        let input = usage.input_tokens_details.as_ref().ok_or_else(|| {
            provider_error(
                "provider_usage_incomplete",
                "input_tokens_details is missing".to_owned(),
                RetrySafety::AcceptanceUnknown,
                ProviderFailureKind::Unknown,
            )
        })?;
        let output = usage.output_tokens_details.as_ref().ok_or_else(|| {
            provider_error(
                "provider_usage_incomplete",
                "output_tokens_details is missing".to_owned(),
                RetrySafety::AcceptanceUnknown,
                ProviderFailureKind::Unknown,
            )
        })?;
        // 分项之和必须等于顶层计数，否则我们无法确信哪一项可信。
        if input.text_tokens + input.image_tokens + input.cached_tokens != usage.input_tokens
            || output.text_tokens + output.image_tokens != usage.output_tokens
        {
            return Err(provider_error(
                "provider_usage_inconsistent",
                "token detail buckets do not sum to the reported totals".to_owned(),
                RetrySafety::AcceptanceUnknown,
                ProviderFailureKind::Unknown,
            ));
        }
        Ok(TokenUsage {
            input_tokens: usage.input_tokens,
            input_text_tokens: input.text_tokens,
            input_image_tokens: input.image_tokens,
            output_tokens: usage.output_tokens,
            output_text_tokens: output.text_tokens,
            output_image_tokens: output.image_tokens,
            total_tokens: usage.total_tokens,
        })
    }

    fn image_urls(&self) -> Result<Vec<String>, AdapterError> {
        let urls: Vec<String> = self
            .result
            .as_ref()
            .map(|result| {
                result
                    .images
                    .iter()
                    .flat_map(|image| image.url.iter().cloned())
                    .collect()
            })
            .unwrap_or_default();
        if urls.is_empty() {
            return Err(provider_error(
                "provider_result_missing",
                "completed task carried no image url".to_owned(),
                RetrySafety::AcceptanceUnknown,
                ProviderFailureKind::Unknown,
            ));
        }
        Ok(urls)
    }

    fn response_digest(&self, task_id: &str) -> String {
        // 摘要用于证据可追溯：绑定 task id 与计量事实，**不含结果 URL**（短期且敏感）。
        let material = format!(
            "{task_id}|{}|{}|{}",
            self.usage.as_ref().map_or(0, |usage| usage.input_tokens),
            self.usage.as_ref().map_or(0, |usage| usage.output_tokens),
            self.status
        );
        sha256_hex(material.as_bytes())
    }

    /// 终态里的成本 → 成本事实。
    ///
    /// 币种不在这里判：金额本身不带币种，币种是**渠道声明**（受理时随请求冻结），所以这里
    /// 只把金额精确换算成微单位，币种用传进来的那一份声明。缺字段、负数、非数字、超范围
    /// 一律 `Unavailable`——**不得猜测**：不记 0、不用费率顶替、也不用上一次的值。
    fn provider_cost(&self, cost_currency: &str) -> ProviderCost {
        match self.cost.as_ref().and_then(declared_microusd) {
            Some(amount_microusd) => ProviderCost::Declared(DeclaredCost {
                amount_microusd,
                currency: cost_currency.to_owned(),
            }),
            None => ProviderCost::Unavailable,
        }
    }
}

/// 上游报出来的金额 → 微单位整数。
///
/// **换算**这一步不经过浮点：钱乘 1e6 会在边界上悄悄差 1 微单位，而这种差正是"成本对不上账"
/// 的来源。（JSON 数字本身由 `serde_json` 按双精度解出，那点误差要到十亿量级的金额才会碰到
/// 微单位，远超这类金额的实际范围。）
///
/// 除数字外还接受**字符串形态与指数写法**：这只是**容忍上游的表示差异**——同一家的响应形状
/// 会随版本变，把可读的金额读出来总好过凭空记一笔成本缺口。它**不是行为承诺**：上游没有承诺
/// 过用哪种写法，平台也不因此就"支持"了这些形态，读不出来照样按"没拿到"处理。
///
/// 负数是上游在说"这笔倒找钱"，平台没有可记的对应事实，按"没拿到"处理。
fn declared_microusd(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => parse_decimal_microusd(&number.to_string()),
        Value::String(text) => parse_decimal_microusd(text),
        Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_) => None,
    }
}

/// 十进制字面量 → 微单位（小数超过 6 位时四舍五入到第 6 位）。
///
/// 判不出确切金额的一律返回 `None`：非数字、负数、指数越界、超出 `u64` 范围。
/// 只有"上游明说这笔是 0"才得到 `0`——它和"没有金额"是两件事。
fn parse_decimal_microusd(text: &str) -> Option<u64> {
    let text = text.trim();
    let (mantissa, exponent) = match text.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.trim().parse::<i32>().ok()?),
        None => (text, 0),
    };
    let mantissa = mantissa.strip_prefix('+').unwrap_or(mantissa);
    if mantissa.starts_with('-') {
        return None;
    }
    let (integer, fraction) = match mantissa.split_once('.') {
        Some((integer, fraction)) => (integer, fraction),
        None => (mantissa, ""),
    };
    if integer.is_empty() && fraction.is_empty() {
        return None;
    }
    if !integer.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    // 数字串去掉小数点，再按 10 的幂移到微单位：金额 × 1e6 = 数字 × 10^(指数 − 小数位数 + 6)。
    let digits = format!("{integer}{fraction}");
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Some(0);
    }
    let digits = digits.parse::<u128>().ok()?;
    let shift = exponent - i32::try_from(fraction.len()).ok()? + 6;
    let scaled = if shift >= 0 {
        digits.checked_mul(10_u128.checked_pow(u32::try_from(shift).ok()?)?)?
    } else {
        let dropped = usize::try_from(-shift).ok()?;
        let divisor = 10_u128.checked_pow(u32::try_from(dropped).ok()?)?;
        let quotient = digits / divisor;
        // 四舍五入：余数到半个除数就进位。够不到半微单位时结果就是 0，不是"猜了一个数"。
        if (digits % divisor) * 2 >= divisor {
            quotient + 1
        } else {
            quotient
        }
    };
    u64::try_from(scaled).ok()
}

/// 一次读上游响应时用的读取预算：上限、超限错误码，以及超限是否仍按瞬时失败重试。
///
/// 生成路径与只读对账路径是两条不同的读取：前者要装下结果图，后者只要有界的状态与计量字段。
/// 超限用各自的错误码交回，调用方据此区分"读不下这份生成响应"和"这次对账拿不到证据"。
#[derive(Debug, Clone, Copy)]
struct ReadBudget {
    limit: usize,
    overflow_code: &'static str,
    /// 超限是否仍按瞬时失败重试。生成路径保留原有的有界重试；对账路径不重读同一份超限响应。
    retry_on_overflow: bool,
}

impl ReadBudget {
    /// 生成路径：上游响应上限。
    const GENERATION: Self = Self {
        limit: MAX_PROVIDER_RESPONSE_BYTES,
        overflow_code: RESPONSE_TOO_LARGE_CODE,
        retry_on_overflow: true,
    };

    /// 只读对账路径：独立且更小的上限（RFC 0018 §2.1）。
    fn reconciliation(limit: usize) -> Self {
        Self {
            limit,
            overflow_code: RECONCILIATION_READ_EXCEEDED_CODE,
            retry_on_overflow: false,
        }
    }

    /// 这次错误是不是"超过本预算的上限"。
    fn is_overflow(self, error: &AdapterError) -> bool {
        matches!(error, AdapterError::Provider(call) if call.code == self.overflow_code)
    }
}

async fn read_body(response: reqwest::Response) -> Result<Bytes, AdapterError> {
    read_body_within(response, ReadBudget::GENERATION).await
}

async fn read_body_within(
    response: reqwest::Response,
    budget: ReadBudget,
) -> Result<Bytes, AdapterError> {
    let status = response.status();
    let body = read_bytes(response, budget.limit, budget.overflow_code).await?;
    if !status.is_success() {
        return Err(parse_provider_error(status, &body).into());
    }
    Ok(body)
}

async fn read_bytes(
    response: reqwest::Response,
    limit: usize,
    overflow_code: &'static str,
) -> Result<Bytes, AdapterError> {
    let mut body = BytesMut::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(ambiguous_transport_error)?;
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(provider_error(
                overflow_code,
                format!("provider response exceeded the {limit}-byte limit"),
                RetrySafety::AcceptanceUnknown,
                ProviderFailureKind::Unknown,
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body.freeze())
}

/// 上游错误体：`{"error":{"code":<number>,"message":...,"type":...}}`。
#[derive(Debug, Deserialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    #[serde(default)]
    code: Option<Value>,
    #[serde(default)]
    message: String,
    #[serde(default)]
    r#type: Option<String>,
}

/// 分类以 `error.code` 与消息前缀为主，状态码只在它们给不出信息时兜底。
///
/// 理由：APIMart 的参数校验错误可能以 `500` 承载（`build_request_failed: …`），一概按状态码
/// 判断会把"不可重试的参数错误"误判成"受理状态不确定"；反过来，鉴权失败实测返回的是
/// `code: ""`，只看 `code` 又会把"根本没进到生成"的请求送进人工对账。创建阶段的成败收窄
/// 另见 [`narrow_submit_rejection`]。
fn parse_provider_error(status: StatusCode, body: &[u8]) -> ProviderCallError {
    let parsed = serde_json::from_slice::<ErrorEnvelope>(body).ok();
    let raw_code = parsed
        .as_ref()
        .and_then(|value| value.error.code.as_ref())
        .and_then(Value::as_i64);
    let message = parsed
        .as_ref()
        .map(|value| value.error.message.clone())
        .unwrap_or_else(|| {
            format!(
                "provider returned HTTP {} without a JSON error body",
                status.as_u16()
            )
        });
    let type_name = parsed.as_ref().and_then(|value| value.error.r#type.clone());
    // `error.code` 的类型随端点而异：数字、字符串、也可能是不在场的空串。
    // 上游自己给了标识符就留住它（排查与幂等子类判定都要用），只有空串才退化成 `type`。
    let text_code = parsed
        .as_ref()
        .and_then(|value| value.error.code.as_ref())
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    let code = match (raw_code, text_code, type_name.as_deref()) {
        (Some(code), _, _) => code.to_string(),
        (None, Some(text), _) => text.to_owned(),
        (None, None, Some(name)) => name.to_owned(),
        (None, None, None) => format!("http_{}", status.as_u16()),
    };
    let retry_safety = if message.starts_with("build_request_failed") {
        // 参数校验错误被 5xx 承载：**判据是消息前缀，不是状态码也不是 code**。
        // 文档只承诺这个前缀；若它出现在别的 code 下，同样说明请求未被接受。
        RetrySafety::NotRetryable
    } else {
        match raw_code {
            // 400 参数错误：请求未被接受。
            Some(400) => RetrySafety::NotRetryable,
            // 401/402/403：凭据、余额或权限问题，重试同一配置无意义。
            Some(401..=403) => RetrySafety::NotRetryable,
            // 429 限流：**不能证明未生成**。
            Some(429) => RetrySafety::AcceptanceUnknown,
            // 其它 5xx：上游可能已受理。
            Some(500..=599) => RetrySafety::AcceptanceUnknown,
            // 没有可用的 `error.code`（实测：鉴权失败返回的是 `code: ""` 与
            // `type: "apimart_error"`）。
            // 此时只有凭据/权限类状态码还能证明"请求根本没进到生成"，按确定性拒绝处置；
            // **5xx 仍然不看状态码**——那正是"只依据 code"这条规则要防的情况。
            _ if matches!(status.as_u16(), 401..=403) => RetrySafety::NotRetryable,
            // 其余（无 code、未知 code、解析失败）：一律按"受理状态不确定"处理。
            _ => RetrySafety::AcceptanceUnknown,
        }
    };
    // 平台侧失败类别：与 `retry_safety` 用的是同一批信号，但结论是另一个维度。
    // 幂等子类的标识符可能落在 `error.code`（字符串）或消息文本里，两处都看。
    let idempotency_text = format!(
        "{} {}",
        parsed
            .as_ref()
            .and_then(|value| value.error.code.as_ref())
            .and_then(Value::as_str)
            .unwrap_or_default(),
        message
    );
    let kind = if message.starts_with("build_request_failed") {
        // 用 5xx 承载的参数错误仍然是"渠道拒绝了平台的请求"。
        ProviderFailureKind::UpstreamRejected
    } else if unaccepted_idempotency(status, &idempotency_text)
        == Some(UnacceptedIdempotency::Conflict)
    {
        // `409` 的两个子类：渠道拒了平台的请求。`503 idempotency_unavailable` 不在此列，
        // 它按普通 5xx 归到"渠道不可用"。
        ProviderFailureKind::UpstreamRejected
    } else {
        match raw_code {
            Some(400) => ProviderFailureKind::UpstreamRejected,
            Some(401 | 403) => ProviderFailureKind::PlatformCredential,
            Some(402) => ProviderFailureKind::PlatformFunding,
            Some(429) => ProviderFailureKind::UpstreamRateLimited,
            Some(500..=599) => ProviderFailureKind::UpstreamUnavailable,
            // 没有可用的 `error.code` 时，只有凭据/余额类状态码还能说明是谁的问题。
            _ => match status.as_u16() {
                401 | 403 => ProviderFailureKind::PlatformCredential,
                402 => ProviderFailureKind::PlatformFunding,
                _ => ProviderFailureKind::Unknown,
            },
        }
    };
    ProviderCallError {
        code,
        message,
        trace_id: None,
        retry_safety,
        kind,
        // 这一段在终态之前：金额还没读到，所以这里没有成本事实。
        provider_cost: None,
    }
}

fn provider_error(
    code: &str,
    message: String,
    retry_safety: RetrySafety,
    kind: ProviderFailureKind,
) -> AdapterError {
    ProviderCallError {
        code: code.to_owned(),
        message,
        trace_id: None,
        retry_safety,
        kind,
        // 建这个错误的地方都还没拿到终态，也就还没有金额可带；终态之后的失败由
        // [`with_provider_cost`] 补上。
        provider_cost: None,
    }
    .into()
}

/// 创建**已成功**之后发生的错误：一律按"受理状态不确定"处理，交给平台进对账。
///
/// 与 [`ambiguous_transport_error`] 的区别只在语义来源：这里明确是"已确认生成、
/// 只是这次没取到结果"，而不是"请求可能没发出去"。两者都落到同一个安全处置。
fn after_acceptance(error: AdapterError) -> AdapterError {
    with_retry_safety(error, RetrySafety::AcceptanceUnknown)
}

/// 只改处置，其余原样：`code` / `message` / `trace_id` / `kind` / `provider_cost` 都是已经判出
/// 的事实，重建错误对象时漏掉任何一个都会丢证据。
fn with_retry_safety(error: AdapterError, retry_safety: RetrySafety) -> AdapterError {
    match error {
        AdapterError::Provider(provider) => AdapterError::Provider(ProviderCallError {
            retry_safety,
            ..provider
        }),
        other => other,
    }
}

/// 第一方写明的"请求未执行"信号：只有状态码与幂等子类**配对**时才算依据。
///
/// 两个变体的**含义不同**，因此平台侧类别也不同：`409` 的两个子类说明是调用方的幂等逻辑
/// 把请求弄重了（渠道拒了平台的请求）；`503 idempotency_unavailable` 是渠道不可用期间的状态。
/// `idempotency_result_indeterminate` 不在表里——第一方明确要求停止自动重试、不要换 Key，
/// 它不能证明请求未被受理。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnacceptedIdempotency {
    /// `409` 的 `idempotency_in_progress` / `idempotency_key_reused`。
    Conflict,
    /// `503` 的 `idempotency_unavailable`。
    Unavailable,
}

fn unaccepted_idempotency(status: StatusCode, text: &str) -> Option<UnacceptedIdempotency> {
    match status {
        StatusCode::CONFLICT
            if text.contains("idempotency_in_progress")
                || text.contains("idempotency_key_reused") =>
        {
            Some(UnacceptedIdempotency::Conflict)
        }
        StatusCode::SERVICE_UNAVAILABLE if text.contains("idempotency_unavailable") => {
            Some(UnacceptedIdempotency::Unavailable)
        }
        _ => None,
    }
}

/// 创建请求**被上游拒绝**时的分类收窄：只认第一方明文写明"请求未执行"的三个**组合**。
///
/// - `429` 限流：第一方口径"能证明未受理"；
/// - `409` + `idempotency_in_progress` / `idempotency_key_reused`：第一方口径"能"；
/// - `503` + `idempotency_unavailable`：第一方原文"当前请求未执行"。
///
/// 其余一律**保持原判**（`AcceptanceUnknown` → 对账）：判错的代价是"其实已受理却当失败"，
/// 上游成本由平台自己承担，所以宁可多进一次人工对账。状态码与文本不配对（例如 `500` 却带
/// `idempotency_unavailable`）同样不算依据，第一方只承诺了上面那三个组合。
///
/// `429` 用 HTTP 状态码而不是 `error.code`：该码可能是空串；幂等子类的标识符落在 `code`
/// 或消息文本里，两处都看（`code` 已由 [`parse_provider_error`] 归一）。
///
/// **只在创建请求这一处收窄**：轮询与上传阶段的同名状态码都不适用——
/// 那时任务已经受理（见 [`after_acceptance`]），或者压根不是"创建"这个动作。
fn narrow_submit_rejection(status: StatusCode, error: AdapterError) -> AdapterError {
    let AdapterError::Provider(provider) = &error else {
        return error;
    };
    let proves_unaccepted = status == StatusCode::TOO_MANY_REQUESTS
        || unaccepted_idempotency(status, &format!("{} {}", provider.code, provider.message))
            .is_some();
    if !proves_unaccepted {
        return error;
    }
    with_retry_safety(error, RetrySafety::SafeBeforeAcceptance)
}

/// 连接中断、超时、解析失败：请求**可能已经发出**，因此一律按受理状态不确定处理
/// 这是贯穿本 Driver 的统一边界。
fn ambiguous_transport_error(error: reqwest::Error) -> AdapterError {
    provider_error(
        "provider_transport_error",
        error.to_string(),
        RetrySafety::AcceptanceUnknown,
        ProviderFailureKind::UpstreamUnavailable,
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

// ── 同步网关协议（RFC 0017 §2/§4）──────────────────────────────────────────────

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

/// 新协议的生成请求体：普通参数逐字进顶层字段，图片参数位由 `image_sites` 给出、取值来自
/// 换算后的公网 URL，名单外的普通参数一个都不改写。
fn gateway_generation_body(
    input: &GatewayInput,
    resolved: &Map<String, Value>,
) -> Result<Value, AdapterError> {
    let mut object = Map::new();
    object.insert(
        "model".to_owned(),
        Value::String(input.provider_model_id.clone()),
    );
    object.insert(
        "prompt".to_owned(),
        required_string(&input.native_parameters, "/prompt")?,
    );
    for (name, value) in gateway_passthrough_parameters(input) {
        object.insert(name.clone(), value.clone());
    }
    for (name, value) in resolved {
        object.insert(name.clone(), value.clone());
    }
    Ok(Value::Object(object))
}

impl ApimartImageAdapter {
    /// 公网 URL 原样透传：生成入口只收公网 URL，平台不下载、不上传、不改写。
    fn gateway_resolve_url(&self, image: &InputImage) -> Result<String, AdapterError> {
        Ok(image.as_str().to_owned())
    }

    /// 按 [`GatewayInput::image_sites`] 换算图片：参考图与遮罩各自的参数名与 wire 形状来自参数位，
    /// 取值来自强类型图片输入。名单里没有的普通参数一个都不碰。
    async fn gateway_resolve_images(
        &self,
        input: &GatewayInput,
    ) -> Result<Map<String, Value>, AdapterError> {
        if !input.reference_images.is_empty() && input.image_sites.reference.is_none() {
            return Err(AdapterError::UnsupportedInput(
                "the offering declares no reference image parameter, so the request cannot carry its reference images"
                    .to_owned(),
            ));
        }
        if input.mask.is_some() && input.image_sites.mask.is_none() {
            return Err(AdapterError::UnsupportedInput(
                "the offering declares no mask parameter, so the request cannot carry its mask"
                    .to_owned(),
            ));
        }
        let mut resolved = Map::new();
        if let Some(site) = &input.image_sites.reference
            && !input.reference_images.is_empty()
        {
            let mut urls = Vec::with_capacity(input.reference_images.len());
            for image in &input.reference_images {
                urls.push(Value::String(self.gateway_resolve_url(image)?));
            }
            // 形状跟候选声明走：数组参数发数组，单值参数只发第一张。
            let value = match site.shape {
                ImageValueShape::Array => Value::Array(urls),
                ImageValueShape::Scalar => urls.into_iter().next().expect("just checked non-empty"),
            };
            resolved.insert(site.parameter.clone(), value);
        }
        if let Some(site) = &input.image_sites.mask
            && let Some(mask) = &input.mask
        {
            resolved.insert(
                site.parameter.clone(),
                Value::String(self.gateway_resolve_url(mask)?),
            );
        }
        Ok(resolved)
    }

    /// 提交生成请求、返回 `task_id`。与旧入口同一份请求形状与提交收窄，只是输入换成新协议。
    async fn gateway_submit(
        &self,
        input: &GatewayInput,
        resolved: &Map<String, Value>,
        credential: &ProviderCredential,
        context: &dyn ExecutionContext,
    ) -> Result<String, AdapterError> {
        let body = gateway_generation_body(input, resolved)?;
        ensure_external_call_allowed(context)?;
        // 生成发送的最后资格：与取消线性化。此后到 .send() 之间没有可取消的等待。
        begin_generation_send(context)?;
        let response = self
            .client
            .post(self.endpoint("v1/images/generations")?)
            .timeout(external_call_timeout(REQUEST_TIMEOUT, context))
            .bearer_auth(credential.expose())
            .json(&body)
            .send()
            .await
            .map_err(ambiguous_transport_error)?;
        let status = response.status();
        let body = read_body(response)
            .await
            .map_err(|error| narrow_submit_rejection(status, error))?;
        let parsed: SubmitEnvelope = serde_json::from_slice(&body).map_err(|error| {
            provider_error(
                "provider_response_invalid",
                error.to_string(),
                RetrySafety::AcceptanceUnknown,
                ProviderFailureKind::Unknown,
            )
        })?;
        parsed
            .data
            .into_iter()
            .next()
            .map(|item| item.task_id)
            .ok_or_else(|| {
                provider_error(
                    "provider_task_missing",
                    "submit response carried no task id".to_owned(),
                    RetrySafety::AcceptanceUnknown,
                    ProviderFailureKind::Unknown,
                )
            })
    }

    /// 轮询到终态：截止判据来自 `execution_context` 的绝对期限，每轮先看取消状态。
    async fn poll_with_context(
        &self,
        task_id: &str,
        credential: &ProviderCredential,
        context: &dyn ExecutionContext,
    ) -> Result<TaskData, AdapterError> {
        loop {
            // 已经受理之后的轮询：任一取消事实成立都停下，且绝不能报成"发送前取消"。
            if context.client_gone() || context.ownership_lost() {
                return Err(AdapterError::Cancelled);
            }
            if context.deadline().is_expired() {
                return Err(provider_error(
                    "provider_task_timeout",
                    format!("task {task_id} did not reach a terminal state in time"),
                    RetrySafety::AcceptanceUnknown,
                    ProviderFailureKind::Unknown,
                ));
            }
            let body = match self
                .query_task_with(task_id, credential, Some(context), ReadBudget::GENERATION)
                .await
            {
                Ok(body) => body,
                Err(error) => return Err(after_acceptance(error)),
            };
            let parsed: TaskEnvelope = serde_json::from_slice(&body).map_err(|error| {
                provider_error(
                    "provider_response_invalid",
                    error.to_string(),
                    RetrySafety::AcceptanceUnknown,
                    ProviderFailureKind::Unknown,
                )
            })?;
            match parsed.data.status.as_str() {
                "completed" => return Ok(parsed.data),
                "failed" | "cancelled" => {
                    let (code, message) = parsed
                        .data
                        .error
                        .as_ref()
                        .map(|error| {
                            (
                                error.code.map_or_else(
                                    || "provider_task_failed".to_owned(),
                                    |c| c.to_string(),
                                ),
                                error.message.clone(),
                            )
                        })
                        .unwrap_or_else(|| {
                            (
                                "provider_task_failed".to_owned(),
                                parsed.data.status.clone(),
                            )
                        });
                    return Err(provider_error(
                        &code,
                        message,
                        RetrySafety::NotRetryable,
                        ProviderFailureKind::Unknown,
                    ));
                }
                _ => {
                    let remaining = context.deadline().remaining();
                    tokio::time::sleep(POLL_INTERVAL.min(remaining)).await;
                }
            }
        }
    }

    /// 提交已成功、句柄也已确认入库之后的部分：轮询到终态 → 抽计量与成本 → 内存结果 + 账务事实。
    async fn gateway_finish(
        &self,
        task_id: &str,
        cost_currency: &str,
        credential: &ProviderCredential,
        context: &dyn ExecutionContext,
    ) -> Result<ProviderOutput, AdapterError> {
        let task = self.poll_with_context(task_id, credential, context).await?;
        let provider_cost = task.provider_cost(cost_currency);
        let usage = task
            .usage()
            .map_err(|error| with_provider_cost(error, provider_cost.clone()))?;
        let digest = task.response_digest(task_id);
        let images = task
            .image_urls()
            .map_err(|error| with_provider_cost(error, provider_cost.clone()))?
            .into_iter()
            .map(GeneratedImage::from_url)
            .collect::<Vec<_>>();
        if images.is_empty() {
            return Err(with_provider_cost(
                provider_error(
                    "provider_result_empty",
                    "provider returned no images".to_owned(),
                    RetrySafety::AcceptanceUnknown,
                    ProviderFailureKind::Unknown,
                ),
                provider_cost,
            ));
        }
        let image_count = u32::try_from(images.len()).unwrap_or(u32::MAX);
        Ok(ProviderOutput {
            response_payload: ResponsePayload {
                // 应用层按自己的时钟兜底 `created`；任务面不提供它。
                created: None,
                images,
            },
            accounting_facts: AccountingFacts {
                usage: Some(usage),
                provider_cost,
                image_count,
                response_digest: digest,
                provider_trace_id: ProviderTraceId::parse(task_id),
            },
        })
    }
}

#[async_trait]
impl GatewayAdapter for ApimartImageAdapter {
    fn key(&self) -> &'static str {
        ADAPTER_KEY
    }

    /// 任务式上游有已知的同一任务状态端点：可按句柄只读查询计量。
    fn query_accounting_capability(&self) -> QueryAccountingCapability {
        QueryAccountingCapability::Supported
    }

    async fn execute(
        &self,
        input: Arc<GatewayInput>,
        context: &dyn ExecutionContext,
        credential: &ProviderCredential,
    ) -> Result<ProviderOutput, AdapterError> {
        // 0) 参考图与遮罩是公网 URL，原样透传。
        let resolved = self
            .gateway_resolve_images(&input)
            .await
            .map_err(gateway_error)?;
        // 1) 提交。这一步之后绝不重发；后续任何失败都进对账。
        let task_id = self
            .gateway_submit(&input, &resolved, credential, context)
            .await
            .map_err(gateway_error)?;
        // 2) A6 barrier：拿到 task id 后先让应用层确认句柄入库，成功之前绝不轮询。
        //    task id 同时是这条通路已知的逐请求对账标识。
        // 上游受理了，但它给的任务标识必须是有界标识：不是就既不保存原值、也不按它轮询，
        // 按"已受理但取不到可信标识"转未知对账（Spec 0005 §2、RFC 0018 §6）。
        let Ok(typed_task_id) = ProviderTaskHandle::parse(task_id.clone()) else {
            return Err(provider_error(
                "provider_task_unusable",
                "the submitted task id is not a bounded identifier".to_owned(),
                RetrySafety::AcceptanceUnknown,
                ProviderFailureKind::Unknown,
            ));
        };
        let handle = AcceptedHandle {
            task_id: typed_task_id,
            trace_id: ProviderTraceId::parse(&task_id),
        };
        if let Err(error) = context.accepted(handle.clone()).await {
            let reason = match error {
                AcceptanceError::Persist(message) => message,
                AcceptanceError::Cancelled => "the execution was cancelled".to_owned(),
            };
            return Err(AdapterError::AcceptedUnpersisted { handle, reason });
        }
        // 3) 句柄已入库：轮询到终态并交回内存结果与账务事实。
        self.gateway_finish(&task_id, &input.cost_currency, credential, context)
            .await
            .map_err(|error| with_task_id(error, &task_id))
            .map_err(gateway_error)
    }

    /// 只读同一任务的终态与账务事实：绝不 submit 或 upload，也不取结果图正文。
    async fn query_accounting(
        &self,
        handle: &AcceptedHandle,
        cost_currency: &str,
        deadline: Deadline,
        credential: &ProviderCredential,
    ) -> Result<AccountingQuery, AdapterError> {
        let task_id = handle.task_id.as_str();
        if task_id.is_empty() {
            return Err(provider_error(
                "provider_task_missing",
                "accepted handle carries no task id to query".to_owned(),
                RetrySafety::NotRetryable,
                ProviderFailureKind::Unknown,
            ));
        }
        if deadline.is_expired() {
            return Err(provider_error(
                "provider_task_timeout",
                format!("task {task_id} query deadline has expired"),
                RetrySafety::AcceptanceUnknown,
                ProviderFailureKind::Unknown,
            ));
        }
        let budget = ReadBudget::reconciliation(self.reconciliation_read_bytes);
        let body = match self
            .query_task_with(task_id, credential, None, budget)
            .await
        {
            Ok(body) => body,
            // 响应超过对账读取自己的上限：整份响应不可信，也不无界重读；按证据缺口（Unknown）
            // 交回，由收尾方保留占用并建对账案例，绝不从读了一半的内容里推断成功或失败
            // （RFC 0018 §2.1）。
            Err(error) if budget.is_overflow(&error) => {
                return Ok(AccountingQuery {
                    state: ProviderTaskState::Unknown,
                    accounting_facts: None,
                });
            }
            Err(error) => return Err(after_acceptance(error)),
        };
        let parsed: TaskEnvelope = serde_json::from_slice(&body).map_err(|error| {
            provider_error(
                "provider_response_invalid",
                error.to_string(),
                RetrySafety::AcceptanceUnknown,
                ProviderFailureKind::Unknown,
            )
        })?;
        let task = parsed.data;
        // 响应必须自证是这次句柄指向的那个任务：读不到标识、或标识与句柄不一致时整份响应
        // 都不可信。把它的状态或计量算到已知任务头上，正是"不可信关联"被当成成功结算的入口
        // （Spec 0005 §5、RFC 0018 §7）。这里不记录上游原值，只把状态降为不可信交给收尾方。
        if task.id.as_deref() != Some(task_id) {
            return Ok(AccountingQuery {
                state: ProviderTaskState::Unknown,
                accounting_facts: None,
            });
        }
        let state = match task.status.as_str() {
            "completed" => ProviderTaskState::Succeeded,
            "failed" => ProviderTaskState::Failed,
            "cancelled" => ProviderTaskState::Cancelled,
            // 未完成状态按仍在执行处理；其余未列出的取值不能当终态、更不能当成功——
            // 标成不可信，由收尾方保留占用，按原预算重查或转对账（渠道事实的任务状态只有
            // pending/processing/completed/failed/cancelled）。
            "pending" | "processing" => ProviderTaskState::Pending,
            _ => ProviderTaskState::Unknown,
        };
        if matches!(
            state,
            ProviderTaskState::Pending | ProviderTaskState::Unknown
        ) {
            return Ok(AccountingQuery {
                state,
                accounting_facts: None,
            });
        }
        let image_count = u32::try_from(task.image_urls().map(|urls| urls.len()).unwrap_or(0))
            .unwrap_or(u32::MAX);
        Ok(AccountingQuery {
            state,
            accounting_facts: Some(AccountingFacts {
                // 终态不一定带得回计量：查询只如实交回拿得到的部分，缺的留空而不是猜。
                usage: task.usage().ok(),
                // 金额与结果无关：终态读到就先报走。上游金额不带币种，币种取受理时冻结的
                // `cost_currency`；上游确实没给或读不出时才落 `unavailable`（不猜、不硬编码币种）。
                provider_cost: task.provider_cost(cost_currency),
                image_count,
                response_digest: task.response_digest(task_id),
                provider_trace_id: ProviderTraceId::parse(task_id),
            }),
        })
    }
}

#[cfg(test)]
mod tests;
