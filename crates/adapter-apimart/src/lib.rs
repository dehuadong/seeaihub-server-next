//! APIMart 的图片生成 Driver（② 层）。
//!
//! 本 Driver 的执行形态与渠道事实如下：
//! 上游是**任务式**（提交拿 `task_id`，轮询到终态，再拿结果地址）——这是**本 Driver 内部**
//! 的实现细节：平台对外是同步的，调用方既看不到 task id，也没有任何"去查任务"的协议。
//! 因此提交、轮询、取结果都在 `execute` 内完成。
//!
//! 计费相关：任务成功响应含**四分项 `usage`**（`input_tokens_details` 区分 text/image，
//! 另有 `cached_tokens`），归一到领域 `TokenUsage`。平台按 token × 费率计价，与 AIHubMix
//! 口径一致；响应里的 `cost`/`credits_cost` **本阶段既不采纳也不留存**（它们受账号折扣
//! 影响）——若将来要按上游声明金额结算，那需要先
//! 立一条新的持久决定并扩展领域证据形态，不是在 Driver 里顺手记下就算数。

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use reqwest::{Client, StatusCode};
use seeai_adapter_sdk::{
    AdapterDescriptor, AdapterError, GeneratedImage, ImageAdapter, PreparedImageRequest,
    ProviderCallError, ProviderCredential, ProviderFailureKind, ProviderSuccess, RetrySafety,
    decode_data_url, is_http_url,
};
use seeai_application::{AdapterFactory, ApplicationError};
use seeai_domain::{
    ImageBranch, ImageParameterKind, TokenUsage, image_inputs, image_parameter_kind,
    image_parameter_values, mask_value,
};
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;
use url::Url;

pub const ADAPTER_KEY: &str = "apimart-image-v1";

const MAX_PROVIDER_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
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
            // 两个 APIMart 素材的 `allowed_branches` 已在受控验证后开放三条分支；
            // 未经真实 wire 验证的能力不开，且素材本身仍是"草案 · 未发布"。
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
    /// 整轮（提交 + 轮询到终态）的墙钟上限。
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

    /// 把一种图片输入换成"可以直接发给上游"的公网 URL。
    ///
    /// 上游只接受**公网可访问的 URL**，不接受生成请求里直接带 base64。因此：
    /// - `data:` URL 就地解码后上传换 URL（内存里做，平台不保存）；
    /// - 公网 URL 原样透传（平台不下载、不搬运，也不改写它）。
    ///
    /// 这一步发生在**提交生成任务之前**，因此任何失败都只能推出同一个结论：
    /// 生成任务**可证明未受理**（`SafeBeforeAcceptance`）——按失败处置、释放预授权，
    /// 不进对账。至于那次上传请求本身有没有被上游受理，与平台的处置无关：
    /// 上传没有需要人工对账的副作用。
    async fn resolve_url(
        &self,
        value: &str,
        credential: &ProviderCredential,
    ) -> Result<String, AdapterError> {
        if value.starts_with("data:") {
            let decoded = decode_data_url(value).map_err(AdapterError::UnsupportedInput)?;
            if decoded.bytes.len() > MAX_UPLOAD_BYTES {
                return Err(AdapterError::UnsupportedInput(format!(
                    "an inline image is {} bytes, above the provider upload limit of {MAX_UPLOAD_BYTES} bytes",
                    decoded.bytes.len()
                )));
            }
            return self
                .upload_image(&decoded.bytes, &decoded.media_type, credential)
                .await;
        }
        if is_http_url(value) {
            return Ok(value.to_owned());
        }
        Err(AdapterError::UnsupportedInput(format!(
            "a reference image must be an http(s) url or a data url, got {value}"
        )))
    }

    /// 把一次请求里的所有图片输入换算成公网 URL，保持它们各自的参数名与形状。
    ///
    /// 只走**平台自己的名单**（受理时按候选声明面与分支算好，见
    /// [`PreparedImageRequest::platform_parameters`]）：名单里的参数名是 Profile 声明、平台受理时
    /// 写进去的，这里只替换取值；名单外的名字一个都不碰——它们只是到手的普通参数，既不上传也不
    /// 改写，逐字交给上游（没声明的名字在受理期就已经按声明面丢掉了，根本到不了这里）。
    /// 形状（标量 / 数组）保持不变，数组顺序即调用方给的顺序。
    async fn resolve_images(
        &self,
        request: &PreparedImageRequest,
        credential: &ProviderCredential,
    ) -> Result<Map<String, Value>, AdapterError> {
        let parameters = request.native_parameters.as_object().ok_or_else(|| {
            AdapterError::UnsupportedInput("parameters must be an object".to_owned())
        })?;
        let mut resolved = Map::new();
        for name in &request.platform_parameters {
            let Some(value) = parameters.get(name) else {
                continue;
            };
            // 名单里的名字按候选声明面分清角色；取值、名字与空值约定都来自 `seeai_domain`：
            // `null` 与空串是"这一处没有图"，不上行；非字符串条目在那里已经被拒，这里不再自己判一遍。
            match image_parameter_kind(name) {
                None => {}
                Some(ImageParameterKind::Reference) => {
                    let values = image_parameter_values(name, value)
                        .map_err(AdapterError::UnsupportedInput)?;
                    let mut urls = Vec::with_capacity(values.len());
                    for text in values {
                        urls.push(Value::String(self.resolve_url(text, credential).await?));
                    }
                    if urls.is_empty() {
                        continue;
                    }
                    // 形状（标量 / 数组）保持调用方给的样子；数组里空出来的格子不占位。
                    let replaced = if matches!(value, Value::Array(_)) {
                        Value::Array(urls)
                    } else {
                        urls.into_iter().next().expect("just checked non-empty")
                    };
                    resolved.insert(name.clone(), replaced);
                }
                Some(ImageParameterKind::Mask) => {
                    let Some(text) =
                        mask_value(name, value).map_err(AdapterError::UnsupportedInput)?
                    else {
                        continue;
                    };
                    let url = self.resolve_url(text, credential).await?;
                    resolved.insert(name.clone(), Value::String(url));
                }
            }
        }
        Ok(resolved)
    }

    /// 上传一份字节，换回可用于生成请求的公网 URL。
    async fn upload_image(
        &self,
        bytes: &[u8],
        media_type: &str,
        credential: &ProviderCredential,
    ) -> Result<String, AdapterError> {
        let part = reqwest::multipart::Part::bytes(bytes.to_vec())
            .file_name(upload_filename(bytes, media_type))
            .mime_str(media_type)
            .map_err(|error| {
                AdapterError::UnsupportedInput(format!(
                    "unsupported image media type {media_type}: {error}"
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
                ProviderFailureKind::Unknown,
            ))
        })?;
        // 这个 URL 会被原样写进生成请求，因此先确认它真的是个 http(s) 地址。
        validate_uploaded_url(&parsed.url).map_err(upload_failure)
    }

    /// 一次请求里所有内联图片的总量上限（上游口径：单次生成请求 256MB）。
    ///
    /// 只统计**平台自己的名单**里那些图片参数：名单外的取值平台不上传，也就不该为它申请内存。
    fn ensure_total_upload_within_limit(
        request: &PreparedImageRequest,
    ) -> Result<(), AdapterError> {
        let inputs = image_inputs(&request.native_parameters, &request.platform_parameters)
            .map_err(AdapterError::UnsupportedInput)?;
        let mut total = 0_usize;
        for text in inputs.reference_images.iter().chain(inputs.mask.iter()) {
            if let Ok(decoded) = decode_data_url(text) {
                total = total.saturating_add(decoded.bytes.len());
            }
        }
        if total > MAX_TOTAL_UPLOAD_BYTES {
            return Err(AdapterError::UnsupportedInput(format!(
                "inline images total {total} bytes, above the provider limit of {MAX_TOTAL_UPLOAD_BYTES} bytes"
            )));
        }
        Ok(())
    }

    /// 提交生成请求，返回 `task_id`。
    async fn submit(
        &self,
        request: &PreparedImageRequest,
        resolved: &Map<String, Value>,
        credential: &ProviderCredential,
    ) -> Result<String, AdapterError> {
        let body = generation_body(request, resolved)?;
        let response = self
            .client
            .post(self.endpoint("v1/images/generations")?)
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
                    ProviderFailureKind::Unknown,
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
                    ProviderFailureKind::Unknown,
                ));
            }
            // 任务查询是**幂等读**，因此可以安全重试：同一次执行内对瞬时失败退避重试
            // 若干次。这与"创建请求绝不重发"不冲突——重试的是读。
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
                // 文档两份取值集合不一致（`submitted`/`processing`/`pending`/`in_progress`），
                // 且可能出现未列出的取值——**未知取值继续轮询，不得当失败**。
                _ => {
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
            }
        }
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
        //    所以内联 data URL 先上传换 url、公网 URL 原样透传。失败时生成任务**尚未提交**，
        //    处置是失败，不是对账——预授权照常释放。
        Self::ensure_total_upload_within_limit(&request)?;
        let resolved = self.resolve_images(&request, credential).await?;
        // 1) 提交。**这一步之后绝不能重发**（创建请求绝不重发）；
        //    任何后续失败都返回 AcceptanceUnknown，交由平台进对账。
        let task_id = self.submit(&request, &resolved, credential).await?;
        // 2) 提交之后的每一步，都把这个 task id 附在错误上：对账的人至少能拿它去上游查。
        self.finish(&task_id, credential)
            .await
            .map_err(|error| with_task_id(error, &task_id))
    }
}

impl ApimartImageAdapter {
    /// 提交**已经成功**之后的部分：轮询到终态 → 抽计量 → 收集结果信封。
    ///
    /// 单独拆出来，是为了让"给失败附上 task id"这件事只有一处
    /// （见 [`with_task_id`]）——这一段的任何失败都会进对账，没有 task id 就查不了。
    async fn finish(
        &self,
        task_id: &str,
        credential: &ProviderCredential,
    ) -> Result<ProviderSuccess, AdapterError> {
        // 轮询到终态。任务查询是幂等读，其瞬时失败在传输层已归类为
        // AcceptanceUnknown（不重试），由平台按"已确认生成但未取到"处理。
        let task = self.poll(task_id, credential).await?;
        let usage = task.usage()?;
        let digest = task.response_digest(task_id);
        // 结果地址原样交给调用方：平台不下载、不转存，也不替上游承诺链接的有效期。
        let images = task
            .image_urls()?
            .into_iter()
            .map(GeneratedImage::from_url)
            .collect::<Vec<_>>();
        if images.is_empty() {
            return Err(provider_error(
                "provider_result_empty",
                "provider returned no images".to_owned(),
                RetrySafety::AcceptanceUnknown,
                ProviderFailureKind::Unknown,
            ));
        }
        Ok(ProviderSuccess {
            images,
            usage,
            // 对账标识：任务式上游的 task id。只写入 attempts.provider_trace_id 供人工对账，
            // **不用于跨调用自动恢复**——拿它自动补齐结果需要另一套模型。
            provider_trace_id: Some(task_id.to_owned()),
            response_digest: digest,
        })
    }
}

/// 给"提交之后"的失败补上 task id，**不改** `code` / `message` / `retry_safety`。
///
/// 为什么需要：进对账的 Job 只能靠人工去上游查，而查的依据就是这个 task id。
/// 提交成功后它就在手里——不附上的话，`attempts.provider_trace_id` 会是空的，
/// 对账的人连"该查哪个任务"都不知道。（**这才是"对账标识"的用途**；
/// 拿它自动去补齐结果属于"跨调用恢复"，本阶段不做。）
fn with_task_id(error: AdapterError, task_id: &str) -> AdapterError {
    match error {
        AdapterError::Provider(mut provider) => {
            if provider.trace_id.is_none() {
                provider.trace_id = Some(task_id.to_owned());
            }
            AdapterError::Provider(provider)
        }
        other => other,
    }
}

fn generation_body(
    request: &PreparedImageRequest,
    resolved: &Map<String, Value>,
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
    // 其余参数**原样**交给上游：到手的参数面已经是候选声明面里的子集（未声明的名字在受理期
    // 就按声明面丢掉了），所以这里不做"认不认识"的判别，只跳过多出来的 `model`/`prompt` 与
    // **平台装载过的那些图片参数名**（名单由受理时算好，见
    // [`PreparedImageRequest::platform_parameters`]）——图片在下面回填成换算后的公网 URL，
    // 原值再进一次只会把 data URL 一起发上去。其余名字逐字过去，取值一个都不改：归属不看取值
    // 的形状，所以名字像图也不会被认领或改写。`null` 表示"这一处没有给"，不是参数值，照旧不进请求体。
    if let Value::Object(parameters) = &request.native_parameters {
        for (name, value) in parameters {
            if matches!(name.as_str(), "model" | "prompt")
                || value.is_null()
                || request
                    .platform_parameters
                    .iter()
                    .any(|declared| declared == name)
            {
                continue;
            }
            object.insert(name.clone(), value.clone());
        }
    }
    // 参考图与遮罩回填到**它们各自的参数名**上（`image_urls`、`mask_url` 由 Profile 声明，
    // 平台受理时写到那里，Driver 不自己决定名字），取值已经全部是公网 URL。
    for (name, value) in resolved {
        object.insert(name.clone(), value.clone());
    }
    Ok(Value::Object(object))
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
    }
}

/// 上传接口的文件大小上限（上游文档：20MB）。
const MAX_UPLOAD_BYTES: usize = 20 * 1024 * 1024;
/// 单次生成请求里图片的总量上限（上游文档：256MB）。只对**内联**图片有意义：
/// 公网 URL 是原样透传的，平台不去读它的字节。
const MAX_TOTAL_UPLOAD_BYTES: usize = 256 * 1024 * 1024;

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
/// 以 `error.code` 为主"说的是**创建请求**的分类，而这里的结论来自"创建请求根本没发出去"。
fn upload_failure(error: AdapterError) -> AdapterError {
    with_retry_safety(error, RetrySafety::SafeBeforeAcceptance)
}

fn upload_transport_error(error: reqwest::Error) -> AdapterError {
    provider_error(
        "provider_transport_error",
        error.to_string(),
        RetrySafety::SafeBeforeAcceptance,
        ProviderFailureKind::UpstreamUnavailable,
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
            ProviderFailureKind::Unknown,
        )),
    }
}

/// 上传时的文件名。上游按扩展名与 MIME 判定类型，因此按媒体类型给一个规整的名字。
fn upload_filename(bytes: &[u8], media_type: &str) -> String {
    let extension = match media_type {
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "png",
    };
    format!("image-{}.{extension}", sha256_hex(bytes))
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

/// 只改处置，其余原样：`code` / `message` / `trace_id` / `kind` 都是已经判出的事实，
/// 重建错误对象时漏掉任何一个都会丢证据。
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

    #[test]
    fn completed_task_without_urls_is_rejected() {
        let data = task("completed", full_usage(), Vec::new());
        let error = data.image_urls().expect_err("no urls must fail");
        assert!(error.to_string().contains("no image url"), "{error}");
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
        let error =
            parse_provider_error(StatusCode::TOO_MANY_REQUESTS, &envelope(429, "slow down"));
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
        let body = serde_json::json!({"error": {"code": "idempotency_in_progress", "message": "in flight"}});
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
        assert_eq!(descriptor.max_images, 16);
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
        };
        assert!(
            ApimartImageAdapter::ensure_total_upload_within_limit(&inline("A".repeat(1024)))
                .is_ok()
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
}
