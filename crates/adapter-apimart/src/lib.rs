//! APIMart 的图片生成 Driver（② 层）。
//!
//! 依 `#2` 规划 §4 的合同与 `docs/facts/channel-facts.md` §3 的渠道事实：
//! 上游是**任务式**（提交拿 `task_id`，轮询到终态，再从结果 URL 取图），
//! 而 `ImageAdapter::execute` 只有一个入口——因此**提交、轮询、取图都在 `execute` 内完成**。
//! 平台对外仍是"持久 Job + 可查询"（`docs/design/0004` R1/R2），上游的异步形态不外泄。
//!
//! 计费相关：任务成功响应含**四分项 `usage`**（`input_tokens_details` 区分 text/image，
//! 另有 `cached_tokens`），归一到领域 `TokenUsage`。平台按 token × 费率计价，与 AIHubMix
//! 口径一致；响应里的 `cost`/`credits_cost` **本阶段既不采纳也不留存**（它们受账号折扣
//! 影响，见 `docs/facts/channel-facts.md` §3.3）——若将来要按上游声明金额结算，那需要先
//! 结清 `docs/adr/0012` 并扩展领域证据形态，不是在 Driver 里顺手记下就算数。

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use reqwest::{Client, StatusCode};
use seeai_adapter_sdk::{
    AdapterDescriptor, AdapterError, GeneratedImage, ImageAdapter, PreparedImageRequest,
    ProviderCallError, ProviderCredential, ProviderSuccess, ResolvedAsset, RetrySafety,
};
use seeai_application::{AdapterFactory, ApplicationError};
use seeai_domain::{
    AssetParameterKind, ImageBranch, TokenUsage, asset_parameter_name, set_native_parameter_at_path,
};
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
/// 单次任务查询的瞬时失败重试次数上限（幂等读才允许重试）。
const QUERY_RETRY_LIMIT: u32 = 3;
/// 查询重试的退避基数：第 n 次失败后等待 `n × 基数`。
const QUERY_RETRY_BACKOFF: Duration = Duration::from_millis(500);
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
            // 参考图与遮罩走 `POST /v1/uploads/images`：上游要求**公网可访问的 HTTP(S) URL**，
            // 且明确不再接受在生成请求里直接传 base64。因此 Driver 先把平台资产上传换 url，
            // 再用 url 组装生成请求（同样属 ② 层内部实现，不外泄到平台）。
            //
            // 注意：这里声明的是**本 Driver 已实现的能力面**，不等于"已获准发布"。
            // 两个 APIMart 发布素材当前仍只开放 `prompt_only`——按 `docs/adr/0002`，
            // 未经真实 wire 验证的能力不开（见 `config/bootstrap/apimart-*.json` 的 `_status`）。
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

    /// 把平台资产上传到上游，换取可用于生成请求的公网 URL。
    ///
    /// 上游明确不再接受在生成请求里直接传 base64，参考图与遮罩必须先上传。
    /// 这些上传都发生在**提交生成任务之前**，因此任何失败都只能推出同一个结论：
    /// 生成任务**可证明未受理**（`SafeBeforeAcceptance`）——按失败处置、释放预授权，
    /// 不进对账。至于那次上传请求本身有没有被上游受理，与平台的处置无关：
    /// 上传没有需要人工对账的副作用。
    async fn upload_assets(
        &self,
        assets: &[ResolvedAsset],
        credential: &ProviderCredential,
    ) -> Result<Vec<UploadedAsset>, AdapterError> {
        // 上游另有一条"单次请求上传总量"上限，先按平台侧能算出的部分挡住。
        let total: usize = assets.iter().map(|asset| asset.bytes.len()).sum();
        ensure_total_upload_within_limit(total)?;
        let mut uploaded = Vec::with_capacity(assets.len());
        for asset in assets {
            uploaded.push(self.upload_asset(asset, credential).await?);
        }
        Ok(uploaded)
    }

    async fn upload_asset(
        &self,
        asset: &ResolvedAsset,
        credential: &ProviderCredential,
    ) -> Result<UploadedAsset, AdapterError> {
        if asset.bytes.len() > MAX_UPLOAD_BYTES {
            return Err(AdapterError::UnsupportedInput(format!(
                "asset {} exceeds the provider upload limit of {MAX_UPLOAD_BYTES} bytes",
                asset.sha256
            )));
        }
        let part = reqwest::multipart::Part::bytes(asset.bytes.to_vec())
            .file_name(upload_filename(asset))
            .mime_str(&asset.media_type)
            .map_err(|error| {
                AdapterError::UnsupportedInput(format!(
                    "unsupported asset media type {}: {error}",
                    asset.media_type
                ))
            })?;
        let form = reqwest::multipart::Form::new().part("file", part);
        let response = self
            .client
            .post(self.endpoint("v1/uploads/images")?)
            .bearer_auth(credential.expose())
            .multipart(form)
            .send()
            .await
            .map_err(upload_transport_error)?;
        // 上传失败 ⇒ 生成任务可证明未受理（释放预授权），不是对账。
        let body = read_body(response).await.map_err(upload_failure)?;
        let parsed: UploadResponse = serde_json::from_slice(&body).map_err(|error| {
            upload_failure(provider_error(
                "provider_response_invalid",
                error.to_string(),
                RetrySafety::SafeBeforeAcceptance,
            ))
        })?;
        // 这个 URL 会被原样写进生成请求，因此先确认它真的是个 http(s) 地址。
        let url = validate_uploaded_url(&parsed.url).map_err(upload_failure)?;
        Ok(UploadedAsset {
            url,
            native_parameter_path: asset.native_parameter_path.clone(),
            position: asset.position,
        })
    }

    /// 提交生成请求，返回 `task_id`。
    async fn submit(
        &self,
        request: &PreparedImageRequest,
        uploaded: &[UploadedAsset],
        credential: &ProviderCredential,
    ) -> Result<String, AdapterError> {
        let body = generation_body(request, uploaded)?;
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

    /// 单次任务查询，对瞬时失败做有界退避重试。
    ///
    /// 只重试**读**：创建请求已经在别的路径上发过且绝不重发。
    async fn query_task(
        &self,
        task_id: &str,
        credential: &ProviderCredential,
    ) -> Result<Bytes, AdapterError> {
        let mut attempt = 0_u32;
        loop {
            let url = self.endpoint(&format!("v1/tasks/{task_id}"))?;
            let outcome = match self
                .client
                .get(url)
                .bearer_auth(credential.expose())
                .send()
                .await
            {
                Ok(response) => read_body(response).await,
                Err(error) => Err(ambiguous_transport_error(error)),
            };
            match outcome {
                Ok(body) => return Ok(body),
                Err(error) => {
                    attempt += 1;
                    if attempt > QUERY_RETRY_LIMIT {
                        return Err(error);
                    }
                    tokio::time::sleep(QUERY_RETRY_BACKOFF * attempt).await;
                }
            }
        }
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
            // 任务查询是**幂等读**，因此可以安全重试：同一次执行内对瞬时失败退避重试
            // 若干次（规划 §4 与 §6-9）。这与"创建请求绝不重发"不冲突——重试的是读。
            // 用完次数后仍失败，则按"已受理但没取到结果"进对账（见 `after_acceptance`）。
            let body = match self.query_task(task_id, credential).await {
                Ok(body) => body,
                Err(error) => return Err(after_acceptance(error)),
            };
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
        // 0) 参考图与遮罩：上游只接受**公网可访问的 URL**（且不再接受 base64），
        //    因此先把平台资产上传换 url。失败时生成任务**尚未提交**，处置是失败，
        //    不是对账——预授权照常释放。
        let uploaded = if request.assets.is_empty() {
            Vec::new()
        } else {
            self.upload_assets(&request.assets, credential).await?
        };
        // 1) 提交。**这一步之后绝不能重发**（规划 §4：创建请求绝不重发）；
        //    任何后续失败都返回 AcceptanceUnknown，交由平台进对账。
        let task_id = self.submit(&request, &uploaded, credential).await?;
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
            // 对账标识：任务式上游的 task id。只写入 attempts.provider_trace_id 供人工对账，
            // **不用于跨调用自动恢复**（规划 §4/§5.3 的统一边界）。
            provider_trace_id: Some(task_id),
            response_digest: digest,
        })
    }
}

fn generation_body(
    request: &PreparedImageRequest,
    uploaded: &[UploadedAsset],
) -> Result<Value, AdapterError> {
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
    // 参考图与遮罩用**上传后拿到的公网 URL**回填到它们各自的参数路径。
    // 参数名（`image_urls`、`mask_url`）由 Profile 声明，Driver 不自己决定名字。
    // 认不出的路径直接报错：宁可失败，也不能把一张图悄悄丢掉。
    for asset in uploaded {
        if asset.kind().is_none() {
            return Err(AdapterError::UnsupportedInput(format!(
                "asset binding path {} is not an image parameter this driver recognises",
                asset.native_parameter_path
            )));
        }
    }
    let mut images: Vec<&UploadedAsset> = uploaded
        .iter()
        .filter(|asset| asset.kind() == Some(AssetParameterKind::Image))
        .collect();
    // 按调用方给的位置排序（`sort_by_key` 稳定，同位置时保持绑定顺序），
    // 与平台注入资产占位符时的数组顺序一致。
    images.sort_by_key(|asset| asset.position);
    for asset in images {
        insert_url_at_path(&mut object, &asset.native_parameter_path, &asset.url)?;
    }
    if let Some(mask) = uploaded
        .iter()
        .find(|asset| asset.kind() == Some(AssetParameterKind::Mask))
    {
        insert_url_at_path(&mut object, &mask.native_parameter_path, &mask.url)?;
    }
    Ok(Value::Object(object))
}

/// 把 URL 写到 `native_parameter_path` 指向的位置。
///
/// 写入规则（标量赋值 / 数组追加）与平台注入资产占位符时共用同一份实现
/// （[`set_native_parameter_at_path`]），不允许两边各判一套。
fn insert_url_at_path(
    object: &mut Map<String, Value>,
    path: &str,
    url: &str,
) -> Result<(), AdapterError> {
    set_native_parameter_at_path(object, path, Value::String(url.to_owned()))
        .map(|_| ())
        .map_err(AdapterError::UnsupportedInput)
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
            // `type: "apimart_error"`，见 `docs/facts/channel-facts.md` §3.8）。
            // 此时只有凭据/权限类状态码还能证明"请求根本没进到生成"，按确定性拒绝处置；
            // **5xx 仍然不看状态码**——那正是"只依据 code"这条规则要防的情况。
            _ if matches!(status.as_u16(), 401..=403) => RetrySafety::NotRetryable,
            // 其余（无 code、未知 code、解析失败）：一律按"受理状态不确定"处理。
            _ => RetrySafety::AcceptanceUnknown,
        }
    };
    ProviderCallError {
        code,
        message,
        trace_id: None,
        retry_safety,
    }
}

/// 上传接口的文件大小上限（上游文档：20MB）。
const MAX_UPLOAD_BYTES: usize = 20 * 1024 * 1024;
/// 单次生成请求里参考图的总量上限（上游文档：256MB）。
const MAX_TOTAL_UPLOAD_BYTES: usize = 256 * 1024 * 1024;

/// 一次请求里所有参考图的字节总量是否在上游允许范围内。
fn ensure_total_upload_within_limit(total: usize) -> Result<(), AdapterError> {
    if total > MAX_TOTAL_UPLOAD_BYTES {
        return Err(AdapterError::UnsupportedInput(format!(
            "assets total {total} bytes, above the provider limit of {MAX_TOTAL_UPLOAD_BYTES} bytes"
        )));
    }
    Ok(())
}

/// 已上传到上游的资产：拿到公网 URL 后回填进生成请求。
#[derive(Debug, Clone)]
struct UploadedAsset {
    url: String,
    native_parameter_path: String,
    position: u16,
}

impl UploadedAsset {
    /// 这个资产装的是参考图还是遮罩——判定与平台用的是同一个函数。
    fn kind(&self) -> Option<AssetParameterKind> {
        AssetParameterKind::classify(asset_parameter_name(&self.native_parameter_path))
    }
}

#[derive(Debug, Deserialize)]
struct UploadResponse {
    url: String,
}

/// 上传接口的失败分类：它发生在**生成任务提交之前**，因此一律按"可证明未受理"
/// 处理（失败并释放预授权），而不是像生成那样进对账。
///
/// 这里**只改 `retry_safety`，不改 `code`/`message`**：`code` 仍由
/// [`parse_provider_error`] 按 `error.code` 判定（上游上传失败的错误体多数没有
/// `error.code`，只有 `type`/`message`，429 例外）。两件事不冲突——生成侧"错误分类
/// 只依据 `error.code`"说的是**创建请求**的分类，而这里的结论来自"创建请求根本没发出去"。
fn upload_failure(error: AdapterError) -> AdapterError {
    match error {
        AdapterError::Provider(provider) => provider_error(
            &provider.code,
            provider.message,
            RetrySafety::SafeBeforeAcceptance,
        ),
        other => other,
    }
}

fn upload_transport_error(error: reqwest::Error) -> AdapterError {
    provider_error(
        "provider_transport_error",
        error.to_string(),
        RetrySafety::SafeBeforeAcceptance,
    )
}

/// 上传返回的 URL 会被原样写进生成请求，因此必须是可用的 http(s) 绝对地址。
fn validate_uploaded_url(raw: &str) -> Result<String, AdapterError> {
    match Url::parse(raw) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => Ok(raw.to_owned()),
        _ => Err(provider_error(
            "provider_response_invalid",
            format!("upload response carried no usable http(s) url: {raw}"),
            RetrySafety::SafeBeforeAcceptance,
        )),
    }
}

/// 上传时的文件名。上游按扩展名与 MIME 判定类型，因此按媒体类型给一个规整的名字。
fn upload_filename(asset: &ResolvedAsset) -> String {
    let extension = match asset.media_type.as_str() {
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "png",
    };
    format!("asset-{}.{extension}", asset.sha256)
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

/// 创建**已成功**之后发生的错误：一律按"受理状态不确定"处理，交给平台进对账。
///
/// 与 [`ambiguous_transport_error`] 的区别只在语义来源：这里明确是"已确认生成、
/// 只是这次没取到结果"，而不是"请求可能没发出去"。两者都落到同一个安全处置。
fn after_acceptance(error: AdapterError) -> AdapterError {
    match error {
        AdapterError::Provider(provider) => provider_error(
            &provider.code,
            provider.message,
            RetrySafety::AcceptanceUnknown,
        ),
        other => other,
    }
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

    #[test]
    fn query_phase_errors_are_routed_to_reconciliation_not_to_failure() {
        // 跨阶段例外（规划 §4）：创建已成功后，查询阶段返回「无效的任务ID」（HTTP 400）
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
        // 无 code 但带前缀：同样判为未受理。
        let body = br#"{"error":{"message":"build_request_failed: bad field"}}"#;
        let error = parse_provider_error(StatusCode::INTERNAL_SERVER_ERROR, body);
        assert_eq!(error.retry_safety, RetrySafety::NotRetryable);
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
    fn upload_filename_follows_the_media_type() {
        let asset = |media_type: &str| ResolvedAsset {
            native_parameter_path: "/images/0".to_owned(),
            position: 0,
            media_type: media_type.to_owned(),
            sha256: "abc123".to_owned(),
            bytes: Bytes::from_static(b"image"),
        };
        assert_eq!(upload_filename(&asset("image/jpeg")), "asset-abc123.jpg");
        assert_eq!(upload_filename(&asset("image/webp")), "asset-abc123.webp");
        assert_eq!(upload_filename(&asset("image/gif")), "asset-abc123.gif");
        assert_eq!(upload_filename(&asset("image/png")), "asset-abc123.png");
    }

    #[test]
    fn upload_failures_are_safe_before_acceptance() {
        // 上传发生在提交生成任务之前，所以它的失败不是"受理状态不确定"，
        // 而是**可证明未受理**（`SafeBeforeAcceptance`）——本阶段同样映射为
        // 失败并释放预授权（`docs/adr/0011`），但不该被标成"确定性拒绝"。
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
    fn total_upload_size_is_capped_like_the_per_file_limit() {
        assert!(ensure_total_upload_within_limit(MAX_TOTAL_UPLOAD_BYTES).is_ok());
        assert!(ensure_total_upload_within_limit(MAX_TOTAL_UPLOAD_BYTES + 1).is_err());
    }

    #[test]
    fn generation_body_refuses_asset_paths_it_cannot_classify() {
        // 受理期已经会拒 `/seed_image` 这类路径；这里再兜一层——绝不静默丢掉一张图。
        let request = PreparedImageRequest {
            provider_model_id: "gpt-image-2.5-flare".to_owned(),
            branch: ImageBranch::ImageConditioned,
            native_parameters: serde_json::json!({"prompt": "x"}),
            assets: Vec::new(),
        };
        let uploaded = vec![UploadedAsset {
            url: "https://up.example/a.png".to_owned(),
            native_parameter_path: "/seed_image".to_owned(),
            position: 0,
        }];
        let error = generation_body(&request, &uploaded).expect_err("unknown path must fail");
        assert!(error.to_string().contains("/seed_image"), "{error}");
    }

    #[test]
    fn generation_body_uses_the_uploaded_urls_at_their_parameter_paths() {
        let request = PreparedImageRequest {
            provider_model_id: "gpt-image-2.5-flare".to_owned(),
            branch: ImageBranch::Masked,
            native_parameters: serde_json::json!({"prompt": "edit this"}),
            assets: Vec::new(),
        };
        // 故意打乱顺序：装配时按位置排序，线上数组顺序必须与调用方一致。
        let uploaded = vec![
            UploadedAsset {
                url: "https://up.example/second.png".to_owned(),
                native_parameter_path: "/image_urls/1".to_owned(),
                position: 1,
            },
            UploadedAsset {
                url: "https://up.example/mask.png".to_owned(),
                native_parameter_path: "/mask_url".to_owned(),
                position: 0,
            },
            UploadedAsset {
                url: "https://up.example/first.png".to_owned(),
                native_parameter_path: "/image_urls/0".to_owned(),
                position: 0,
            },
        ];
        let body = generation_body(&request, &uploaded).expect("body");
        assert_eq!(
            body["image_urls"],
            serde_json::json!([
                "https://up.example/first.png",
                "https://up.example/second.png"
            ]),
            "参考图必须保持调用方顺序，且只能是上传后的公网 URL"
        );
        assert_eq!(body["mask_url"], "https://up.example/mask.png");
        assert!(
            !body.to_string().contains("asset://"),
            "本地资产引用绝不能出现在上行请求里"
        );
    }
}
