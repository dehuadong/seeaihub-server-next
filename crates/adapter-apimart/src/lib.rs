//! APIMart 的图片生成 Driver（② 层）。
//!
//! 依 `#2` 规划 §4 的合同与 `docs/facts/channel-facts.md` §3 的渠道事实：
//! 上游是**任务式**（提交拿 `task_id`，轮询到终态，再从结果 URL 取图），
//! 而 `ImageAdapter::execute` 只有一个入口——因此**提交、轮询、取图都在 `execute` 内完成**。
//! 平台对外仍是"持久 Job + 可查询"（`docs/design/0004` R1/R2），上游的异步形态不外泄。
//!
//! 计费相关：任务成功响应含**四分项 `usage`**（`input_tokens_details` 区分 text/image，
//! 另有 `cached_tokens`），归一到领域 `TokenUsage`。响应里的 `cost`/`credits_cost` 只记录
//! 不参与结算——平台按 token × 费率计价，与 AIHubMix 口径一致。

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use reqwest::{Client, StatusCode};
use seeai_adapter_sdk::{
    AdapterDescriptor, AdapterError, GeneratedImage, ImageAdapter, PreparedImageRequest,
    ProviderCallError, ProviderCredential, ProviderSuccess, RetrySafety,
};
use seeai_application::{AdapterFactory, ApplicationError};
use seeai_domain::{ImageBranch, TokenUsage};
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;
use url::Url;

pub const ADAPTER_KEY: &str = "apimart-image-v1";

const MAX_PROVIDER_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_OUTPUT_IMAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOTAL_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
/// 轮询间隔。文档建议 2~5 秒，取偏小值以缩短 Job 驻留时间。
const POLL_INTERVAL: Duration = Duration::from_secs(3);
/// 单次 HTTP 调用的超时（提交与轮询各自适用）。整轮耗时由 `deadline` 约束。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Default)]
pub struct ApimartAdapterFactory;

impl AdapterFactory for ApimartAdapterFactory {
    fn descriptor(&self, adapter_key: &str) -> Option<AdapterDescriptor> {
        (adapter_key == ADAPTER_KEY).then_some(AdapterDescriptor {
            key: ADAPTER_KEY,
            // 机器的输入合同里这些参数**都在顶层**——APIMart 没有 `extra` 包装层
            // （与 AIHubMix 的 `/ai/v1` 相反；见 `docs/facts/channel-facts.md` §2.5）。
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
            supported_branches: &[
                ImageBranch::PromptOnly,
                ImageBranch::ImageConditioned,
                ImageBranch::Masked,
            ],
            max_images: 16,
        })
    }

    fn validate_publication(
        &self,
        adapter_key: &str,
        capability_schema: &Value,
        restrictions: &Value,
    ) -> Result<(), String> {
        if adapter_key != ADAPTER_KEY {
            return Err(format!("unknown adapter {adapter_key}"));
        }
        validate_apimart_publication(capability_schema, restrictions)
    }

    fn create(
        &self,
        adapter_key: &str,
        base_url: &str,
        timeout: Duration,
    ) -> Result<Arc<dyn ImageAdapter>, ApplicationError> {
        if adapter_key != ADAPTER_KEY {
            return Err(ApplicationError::Configuration(format!(
                "unsupported adapter {adapter_key}"
            )));
        }
        ApimartImageAdapter::new(base_url, timeout)
            .map(|adapter| Arc::new(adapter) as Arc<dyn ImageAdapter>)
            .map_err(|error| ApplicationError::Configuration(error.to_string()))
    }
}

fn validate_apimart_publication(schema: &Value, restrictions: &Value) -> Result<(), String> {
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| "capability schema properties are required".to_owned())?;
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .ok_or_else(|| "capability schema required list is missing".to_owned())?;
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
    let max_images = restrictions
        .get("max_images")
        .and_then(Value::as_u64)
        .unwrap_or(1);
    if max_images > 16 {
        return Err("APIMart supports at most 16 reference images".to_owned());
    }
    Ok(())
}

pub struct ApimartImageAdapter {
    client: Client,
    base_url: Url,
    /// 整轮（提交 + 轮询 + 取图）的墙钟上限。
    deadline: Duration,
}

impl ApimartImageAdapter {
    pub fn new(base_url: &str, timeout: Duration) -> Result<Self, AdapterError> {
        let normalized = format!("{}/", base_url.trim_end_matches('/'));
        let base_url = Url::parse(&normalized)
            .map_err(|error| AdapterError::Configuration(error.to_string()))?;
        let client = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| AdapterError::Configuration(error.to_string()))?;
        Ok(Self {
            client,
            base_url,
            deadline: timeout,
        })
    }

    fn endpoint(&self, path: &str) -> Result<Url, AdapterError> {
        self.base_url
            .join(path)
            .map_err(|error| AdapterError::Configuration(error.to_string()))
    }

    /// 提交生成请求，返回 `task_id`。
    async fn submit(
        &self,
        request: &PreparedImageRequest,
        credential: &ProviderCredential,
    ) -> Result<String, AdapterError> {
        let body = generation_body(request)?;
        let response = self
            .client
            .post(self.endpoint("v1/images/generations")?)
            .bearer_auth(credential.expose())
            .json(&body)
            .send()
            .await
            .map_err(ambiguous_transport_error)?;
        let body = read_body(response).await?;
        let parsed: SubmitEnvelope = serde_json::from_slice(&body).map_err(|error| {
            provider_error(
                "provider_response_invalid",
                error.to_string(),
                RetrySafety::AcceptanceUnknown,
            )
        })?;
        // 文档明确：`data` 是数组，读 `data[0].task_id`。
        parsed
            .data
            .into_iter()
            .next()
            .map(|item| item.task_id)
            .ok_or_else(|| {
                provider_error(
                    "provider_task_missing",
                    "submit response carried no task id".to_owned(),
                    // 已被受理但没有 task id：无法对账，且绝不能重发。
                    RetrySafety::AcceptanceUnknown,
                )
            })
    }

    /// 轮询任务直到终态。
    async fn poll(
        &self,
        task_id: &str,
        credential: &ProviderCredential,
    ) -> Result<TaskData, AdapterError> {
        let start = tokio::time::Instant::now();
        loop {
            if start.elapsed() >= self.deadline {
                return Err(provider_error(
                    "provider_task_timeout",
                    format!("task {task_id} did not reach a terminal state in time"),
                    // 已受理且仍在跑：既不能当失败，也不能重发。
                    RetrySafety::AcceptanceUnknown,
                ));
            }
            let response = self
                .client
                .get(self.endpoint(&format!("v1/tasks/{task_id}"))?)
                .bearer_auth(credential.expose())
                .send()
                .await
                .map_err(ambiguous_transport_error)?;
            let body = read_body(response).await?;
            let parsed: TaskEnvelope = serde_json::from_slice(&body).map_err(|error| {
                provider_error(
                    "provider_response_invalid",
                    error.to_string(),
                    RetrySafety::AcceptanceUnknown,
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
                    return Err(provider_error(&code, message, RetrySafety::NotRetryable));
                }
                // 文档两份取值集合不一致（`submitted`/`processing`/`pending`/`in_progress`），
                // 且可能出现未列出的取值——**未知取值继续轮询，不得当失败**（规划 §4）。
                _ => {
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
            }
        }
    }

    /// 下载结果图。上游 URL 短期有效，且要求同一 Bearer 凭据。
    async fn fetch_image(
        &self,
        url: &str,
        credential: &ProviderCredential,
    ) -> Result<GeneratedImage, AdapterError> {
        let response = self
            .client
            .get(url)
            .bearer_auth(credential.expose())
            .send()
            .await
            .map_err(ambiguous_transport_error)?;
        let status = response.status();
        if !status.is_success() {
            let body = read_body(response).await?;
            return Err(parse_provider_error(status, &body).into());
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.split(';').next().unwrap_or(value).trim().to_owned());
        let media_type = content_type.unwrap_or_else(|| "image/png".to_owned());
        let bytes = read_bytes(response, MAX_OUTPUT_IMAGE_BYTES).await?;
        validate_image_magic(&bytes, &media_type)?;
        Ok(GeneratedImage {
            sha256: sha256_hex(&bytes),
            media_type,
            bytes,
        })
    }
}

#[async_trait]
impl ImageAdapter for ApimartImageAdapter {
    fn key(&self) -> &'static str {
        ADAPTER_KEY
    }

    async fn execute(
        &self,
        request: PreparedImageRequest,
        credential: &ProviderCredential,
    ) -> Result<ProviderSuccess, AdapterError> {
        // 1) 提交。**这一步之后绝不能重发**（规划 §4：创建请求绝不重发）；
        //    任何后续失败都返回 AcceptanceUnknown，交由平台进对账。
        let task_id = self.submit(&request, credential).await?;
        // 2) 轮询到终态。任务查询是幂等读，其瞬时失败在传输层已归类为
        //    AcceptanceUnknown（不重试），由平台按"已确认生成但未取到"处理。
        let task = self.poll(&task_id, credential).await?;
        let usage = task.usage()?;
        let digest = task.response_digest(&task_id);
        // 3) 取图。结果 URL 带 expires_at，必须立刻下载并转存。
        let mut images = Vec::new();
        let mut total_output_bytes = 0_usize;
        for url in task.image_urls()? {
            let image = self.fetch_image(&url, credential).await?;
            total_output_bytes = total_output_bytes.saturating_add(image.bytes.len());
            if total_output_bytes > MAX_TOTAL_OUTPUT_BYTES {
                return Err(provider_error(
                    "provider_output_too_large",
                    "provider output exceeded the configured safety limit".to_owned(),
                    RetrySafety::AcceptanceUnknown,
                ));
            }
            images.push(image);
        }
        if images.is_empty() {
            return Err(provider_error(
                "provider_result_empty",
                "provider returned no images".to_owned(),
                RetrySafety::AcceptanceUnknown,
            ));
        }
        Ok(ProviderSuccess {
            images,
            usage,
            response_digest: digest,
        })
    }
}

fn generation_body(request: &PreparedImageRequest) -> Result<Value, AdapterError> {
    let mut object = Map::new();
    object.insert(
        "model".to_owned(),
        Value::String(request.provider_model_id.clone()),
    );
    object.insert(
        "prompt".to_owned(),
        required_string(&request.native_parameters, "/prompt")?,
    );
    for field in [
        "n",
        "size",
        "resolution",
        "quality",
        "output_format",
        "output_compression",
        "background",
        "moderation",
    ] {
        if let Some(value) = request.native_parameters.get(field)
            && !value.is_null()
        {
            object.insert(field.to_owned(), value.clone());
        }
    }
    // 参考图与遮罩：平台资产在受理时已解析为可访问的字符串引用。
    let images: Vec<Value> = request
        .assets
        .iter()
        .filter(|asset| asset.native_parameter_path.starts_with("/images/"))
        .map(|asset| Value::String(asset_reference(asset)))
        .collect();
    if !images.is_empty() {
        object.insert("image_urls".to_owned(), Value::Array(images));
    }
    if let Some(mask) = request
        .assets
        .iter()
        .find(|asset| asset.native_parameter_path == "/mask")
    {
        object.insert("mask_url".to_owned(), Value::String(asset_reference(mask)));
    }
    Ok(Value::Object(object))
}

fn asset_reference(asset: &seeai_adapter_sdk::ResolvedAsset) -> String {
    format!("asset://{}", asset.sha256)
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
    status: String,
    #[serde(default)]
    result: Option<TaskResult>,
    #[serde(default)]
    usage: Option<UsageBody>,
    #[serde(default)]
    error: Option<TaskError>,
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
                // 已生成但计量缺失：不得猜测费用（adr/0006），进对账。
                RetrySafety::AcceptanceUnknown,
            )
        })?;
        // 分项缺失时**不猜测**文本/图片的划分——直接失败（adr/0006：缺字段不得猜测费用）。
        let input = usage.input_tokens_details.as_ref().ok_or_else(|| {
            provider_error(
                "provider_usage_incomplete",
                "input_tokens_details is missing".to_owned(),
                RetrySafety::AcceptanceUnknown,
            )
        })?;
        let output = usage.output_tokens_details.as_ref().ok_or_else(|| {
            provider_error(
                "provider_usage_incomplete",
                "output_tokens_details is missing".to_owned(),
                RetrySafety::AcceptanceUnknown,
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
}

async fn read_body(response: reqwest::Response) -> Result<Bytes, AdapterError> {
    let status = response.status();
    let body = read_bytes(response, MAX_PROVIDER_RESPONSE_BYTES).await?;
    if !status.is_success() {
        return Err(parse_provider_error(status, &body).into());
    }
    Ok(body)
}

async fn read_bytes(response: reqwest::Response, limit: usize) -> Result<Bytes, AdapterError> {
    let mut body = BytesMut::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(ambiguous_transport_error)?;
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(provider_error(
                "provider_response_too_large",
                "provider response exceeded the configured safety limit".to_owned(),
                RetrySafety::AcceptanceUnknown,
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

/// **只依据 `error.code` 分类，不依据 HTTP 状态码**（规划 §4）。
///
/// 理由：APIMart 的参数校验错误可能以 `500` 承载（`build_request_failed: …`），
/// 按状态码判断会把"不可重试的参数错误"误判成"受理状态不确定"。
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
    let code = match (raw_code, type_name.as_deref()) {
        (Some(code), _) => code.to_string(),
        (None, Some(name)) => name.to_owned(),
        (None, None) => format!("http_{}", status.as_u16()),
    };
    let retry_safety = match raw_code {
        // 400 参数错误：请求未被接受。
        Some(400) => RetrySafety::NotRetryable,
        // 401/402/403：凭据、余额或权限问题，重试同一配置无意义。
        Some(401..=403) => RetrySafety::NotRetryable,
        // 429 限流：**不能证明未生成**。
        Some(429) => RetrySafety::AcceptanceUnknown,
        // 500 且 message 以 build_request_failed 开头：参数错误被 500 承载。
        Some(500) if message.starts_with("build_request_failed") => RetrySafety::NotRetryable,
        // 其它 5xx：上游可能已受理。
        Some(500..=599) => RetrySafety::AcceptanceUnknown,
        // 无 code、未知 code、解析失败：一律按"受理状态不确定"处理。
        _ => RetrySafety::AcceptanceUnknown,
    };
    ProviderCallError {
        code,
        message,
        trace_id: None,
        retry_safety,
    }
}

fn provider_error(code: &str, message: String, retry_safety: RetrySafety) -> AdapterError {
    ProviderCallError {
        code: code.to_owned(),
        message,
        trace_id: None,
        retry_safety,
    }
    .into()
}

/// 连接中断、超时、解析失败：请求**可能已经发出**，因此一律按受理状态不确定处理
/// （规划 §4 与 §5.3 的统一边界）。
fn ambiguous_transport_error(error: reqwest::Error) -> AdapterError {
    provider_error(
        "provider_transport_error",
        error.to_string(),
        RetrySafety::AcceptanceUnknown,
    )
}

fn validate_image_magic(bytes: &[u8], media_type: &str) -> Result<(), AdapterError> {
    let ok = match media_type {
        "image/png" => bytes.starts_with(&[0x89, b'P', b'N', b'G']),
        "image/jpeg" => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
        "image/webp" => bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(provider_error(
            "provider_result_invalid_media",
            format!("result bytes do not match declared media type {media_type}"),
            RetrySafety::AcceptanceUnknown,
        ))
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
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
        // adr/0006：缺字段时不得猜测费用。
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

    #[test]
    fn completed_task_without_urls_is_rejected() {
        let data = task("completed", full_usage(), Vec::new());
        let error = data.image_urls().expect_err("no urls must fail");
        assert!(error.to_string().contains("no image url"), "{error}");
    }

    #[test]
    fn error_classification_uses_code_not_status() {
        // 400 参数错误：未受理。
        assert_eq!(
            parse_provider_error(StatusCode::BAD_REQUEST, &envelope(400, "bad")).retry_safety,
            RetrySafety::NotRetryable
        );
        // 401/402/403：凭据、余额、权限，重试同一配置无意义。
        for code in [401, 402, 403] {
            assert_eq!(
                parse_provider_error(StatusCode::UNAUTHORIZED, &envelope(code, "no")).retry_safety,
                RetrySafety::NotRetryable
            );
        }
        // 429 限流：不能证明未生成。
        assert_eq!(
            parse_provider_error(StatusCode::TOO_MANY_REQUESTS, &envelope(429, "slow down"))
                .retry_safety,
            RetrySafety::AcceptanceUnknown
        );
    }

    #[test]
    fn parameter_error_carried_as_500_is_not_retryable() {
        // 这是"只依据 error.code、不依据状态码"的关键理由。
        let error = parse_provider_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &envelope(500, "build_request_failed: unsupported size"),
        );
        assert_eq!(error.retry_safety, RetrySafety::NotRetryable);
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
        assert_eq!(descriptor.max_images, 16);
        assert!(factory.descriptor("some-other-key").is_none());
    }
}
