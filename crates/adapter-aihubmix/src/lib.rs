use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use reqwest::{Client, StatusCode, multipart};
use seeai_adapter_sdk::{
    AccountingFacts, AdapterDescriptor, AdapterError, DecodedImage, ExecutionContext,
    GATEWAY_REQUEST_WIRE_BYTES, GatewayAdapter, GatewayByteLimits, GatewayInput, GeneratedImage,
    ImageAdapter, InputImage, PreparedImageRequest, ProviderCallError, ProviderCost,
    ProviderCredential, ProviderFailureKind, ProviderOutput, ProviderSuccess, ProviderTraceId,
    QueryAccountingCapability, ResponsePayload, RetrySafety, begin_generation_send,
    decode_data_url, ensure_external_call_allowed, external_call_timeout,
    gateway_passthrough_parameters, is_http_url,
};
use seeai_application::{AdapterFactory, ApplicationError};
use seeai_domain::{
    ImageBranch, ImageInputs, ImageParameterKind, TokenUsage, image_inputs,
    platform_image_parameter,
};
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
/// 单张输入图（参考图或遮罩）在内存里的上限：data URL 就地解码、公网 URL 自己下载，
/// 两种形态都不落盘，因此必须有上限兜住内存。
const MAX_INPUT_IMAGE_BYTES: usize = 16 * 1024 * 1024;

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
                "image",
                "mask",
                "n",
                "size",
                "output_format",
                "quality",
                "background",
                "output_compression",
                "moderation",
                "user",
            ],
            supported_extra_parameters: &[],
            supported_branches: &[
                ImageBranch::PromptOnly,
                ImageBranch::ImageConditioned,
                ImageBranch::Masked,
            ],
            max_reference_images: 16,
            // AIHubMix 只回四分项 `usage`，金额由平台按费率自算：声明"上游给金额"的候选
            // （成本或对客）在这条通路上发布期就拒。
            declares_cost: false,
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

    fn create(
        &self,
        adapter_key: &str,
        base_url: &str,
        timeout: Duration,
    ) -> Result<std::sync::Arc<dyn ImageAdapter>, ApplicationError> {
        if adapter_key != ADAPTER_KEY {
            return Err(ApplicationError::Configuration(format!(
                "unsupported adapter {adapter_key}"
            )));
        }
        AihubmixImageAdapter::new(base_url, timeout)
            .map(|adapter| std::sync::Arc::new(adapter) as std::sync::Arc<dyn ImageAdapter>)
            .map_err(|error| ApplicationError::Configuration(error.to_string()))
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
    // 参考图两种收法都表示得出来：单值（一次一张）与字符串数组（同一个部件名重复出现，
    // 就是 multipart 里的列表形态）。遮罩是一块编辑范围，没有数组形态。
    if properties.contains_key("image") {
        require_string_or_string_array(properties, "image")?;
    }
    if properties.contains_key("mask") {
        require_type(properties, "mask", "string")?;
    }
    if properties.contains_key("n") {
        require_type(properties, "n", "integer")?;
    }
    for name in ["size", "output_format", "quality"] {
        if properties.contains_key(name) {
            // 端点 schema 对这几项只声明类型（`size` / `output_format` 连枚举都没有），
            // 所以枚举可有可无：声明了就必须是字符串枚举，没声明就按普通字符串发出去。
            require_string_with_optional_enum(properties, name)?;
        }
    }
    for name in ["background", "moderation"] {
        if properties.contains_key(name) {
            require_string_enum(properties, name)?;
        }
    }
    if properties.contains_key("output_compression") {
        require_type(properties, "output_compression", "integer")?;
    }
    if properties.contains_key("user") {
        require_type(properties, "user", "string")?;
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
    let reference = match properties
        .get("image")
        .and_then(|field| field.get("type"))
        .and_then(Value::as_str)
    {
        Some("array") => serde_json::json!([image]),
        _ => serde_json::json!(image),
    };
    let mask = "https://example.invalid/mask.png";
    let cases = [
        (
            "prompt_only",
            serde_json::json!({"model": model, "prompt": "x"}),
        ),
        (
            "image_conditioned",
            serde_json::json!({"model": model, "prompt": "x", "image": reference}),
        ),
        (
            "masked",
            serde_json::json!({"model": model, "prompt": "x", "image": reference, "mask": mask}),
        ),
    ];
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

    fn endpoint(&self, branch: ImageBranch) -> Result<Url, AdapterError> {
        let path = match branch {
            ImageBranch::PromptOnly => "v1/images/generations",
            ImageBranch::ImageConditioned | ImageBranch::Masked => "v1/images/edits",
        };
        self.base_url
            .join(path)
            .map_err(|error| AdapterError::Configuration(error.to_string()))
    }

    async fn generate(
        &self,
        request: &PreparedImageRequest,
        credential: &ProviderCredential,
    ) -> Result<ProviderSuccess, AdapterError> {
        let body = generation_body(request)?;
        let response = self
            .client
            .post(self.endpoint(ImageBranch::PromptOnly)?)
            .bearer_auth(credential.expose())
            .json(&body)
            .send()
            .await
            .map_err(ambiguous_transport_error)?;
        parse_response(response).await
    }

    async fn edit(
        &self,
        request: &PreparedImageRequest,
        credential: &ProviderCredential,
    ) -> Result<ProviderSuccess, AdapterError> {
        let form = self.edit_form(request).await?;
        let response = self
            .client
            .post(self.endpoint(request.branch)?)
            .bearer_auth(credential.expose())
            .multipart(form)
            .send()
            .await
            .map_err(ambiguous_transport_error)?;
        parse_response(response).await
    }

    /// 组好编辑请求的 multipart 表单。
    ///
    /// 单独拆出来，是为了让"部件名从哪来"能被直接验证：测试把这份表单摊成将要发出去的字节，
    /// 看 `name="…"` 到底是谁（见测试里的 `multipart_body`）。
    async fn edit_form(
        &self,
        request: &PreparedImageRequest,
    ) -> Result<multipart::Form, AdapterError> {
        let inputs = reference_inputs(request)?;
        let reference_part = image_part_name(request, ImageParameterKind::Reference)?;
        let mut form = multipart::Form::new()
            .text("model", request.provider_model_id.clone())
            .text(
                "prompt",
                required_string(&request.native_parameters, "/prompt")?,
            );
        for (name, value) in passthrough_parameters(request) {
            // 标量转成文本部件；数组与对象**没有**可用的表示法，于是明确失败——
            // 见 [`multipart_text`] 里为什么不做"序列化成 JSON 文本"这种替代形态。
            form = form.text(name.clone(), multipart_text(name, value)?);
        }
        // 参考图逐张发，**部件名随张数变**：一张就是名单里那个名字（`image`），多张时用重复的
        // `image[]`——multipart 的重复字段才是这条渠道认的列表形态（实测：重复 `image` 会 400）。
        // 这是**传输细节**：合同与承载面只声明"这条供给能承载最多 16 张参考图"，怎么编码由这里
        // 承担，也不进 descriptor 的能力名单。收几张由发布物与选路定，这里不另设自己的上限。
        let reference_part = if inputs.reference_images.len() > 1 {
            format!("{reference_part}[]")
        } else {
            reference_part.to_owned()
        };
        for value in &inputs.reference_images {
            let image = self.image_bytes(value).await?;
            form = form.part(reference_part.clone(), image_part(image)?);
        }
        if let Some(mask) = &inputs.mask {
            let mask_part = image_part_name(request, ImageParameterKind::Mask)?;
            let mask = self.image_bytes(mask).await?;
            form = form.part(mask_part.to_owned(), image_part(mask)?);
        }
        Ok(form)
    }

    /// 把一种图片形态取成字节：`data:` URL 就地解码，公网 URL 自己下载（只在内存里）。
    ///
    /// 这一步发生在生成请求之前，所以失败时**上游什么都没收到**：按可证明未受理处理
    /// （Job 失败并释放预授权），不进对账。
    async fn image_bytes(&self, value: &str) -> Result<DecodedImage, AdapterError> {
        match classify_image_value(value)? {
            ImageValue::Inline(value) => decode_inline_image(value),
            ImageValue::Remote(url) => self.download_image(url).await,
        }
    }

    /// 公网 URL 自己下载：带超时（用本 Driver 的 HTTP 客户端）与体积上限，只在内存里。
    async fn download_image(&self, url: &str) -> Result<DecodedImage, AdapterError> {
        self.download_image_within(url, self.timeout).await
    }

    /// 带显式单次超时的下载；新协议用总期限剩余夹住它（RFC 0017 §6）。
    async fn download_image_within(
        &self,
        url: &str,
        timeout: Duration,
    ) -> Result<DecodedImage, AdapterError> {
        let response = self
            .client
            .get(url)
            .timeout(timeout)
            .send()
            .await
            .map_err(reference_image_unavailable)?;
        let status = response.status();
        if !status.is_success() {
            return Err(reference_image_unavailable(format!(
                "reference image returned HTTP {}",
                status.as_u16()
            )));
        }
        let media_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.split(';').next().unwrap_or(value).trim().to_owned())
            .unwrap_or_else(|| "image/png".to_owned());
        let bytes = read_limited(response, MAX_INPUT_IMAGE_BYTES)
            .await
            .map_err(|error| reference_image_unavailable(error.to_string()))?;
        Ok(DecodedImage { media_type, bytes })
    }
}

/// 参考图/遮罩的两种允许形态。
#[derive(Debug)]
enum ImageValue<'a> {
    /// `data:image/…;base64,…`：就地解码。
    Inline(&'a str),
    /// 公网 http(s) 地址：自己下载。
    Remote(&'a str),
}

fn classify_image_value(value: &str) -> Result<ImageValue<'_>, AdapterError> {
    if value.starts_with("data:") {
        return Ok(ImageValue::Inline(value));
    }
    if is_http_url(value) {
        return Ok(ImageValue::Remote(value));
    }
    Err(AdapterError::UnsupportedInput(format!(
        "a reference image must be an http(s) url or a data url, got {value}"
    )))
}

fn decode_inline_image(value: &str) -> Result<DecodedImage, AdapterError> {
    let decoded = decode_data_url(value).map_err(AdapterError::UnsupportedInput)?;
    ensure_input_size(decoded.bytes.len())?;
    Ok(decoded)
}

/// 这个 Driver 的编辑端点要**至少**一张参考图（外加至多一张遮罩）：一张都不给就直接拒绝，不静默
/// 发一个没有图的编辑请求。
///
/// 多张**不在这里拦**：这条面按"最多 16 张参考图"发布（`restrictions.max_reference_images`），
/// 收几张是发布物与选路的事；这里再拦一道，等于让声明的能力与实现互相矛盾——而且被拦下的请求是
/// 平台侧故障，调用方完全无从判断。
fn reference_inputs(request: &PreparedImageRequest) -> Result<ImageInputs, AdapterError> {
    let inputs = image_inputs(&request.native_parameters, &request.platform_parameters)
        .map_err(AdapterError::UnsupportedInput)?;
    if inputs.reference_images.is_empty() {
        return Err(AdapterError::UnsupportedInput(
            "the edit endpoint needs one reference image".to_owned(),
        ));
    }
    Ok(inputs)
}

/// 编辑路径上图片部件的名字：**取自平台名单**，不写死。
///
/// 名单里的名字就是被选中候选自己声明的参数名（受理时按候选声明面与分支算好、随请求冻结），
/// 所以 Profile 把参考图声明成 `image_urls` 时，线上部件名跟着变成 `image_urls`——平台不改写
/// 渠道参数名。哪个名字是遮罩仍按候选面的判定函数分（`image_parameter_kind` 看名字的形状，
/// 与取值无关）。
///
/// 名单里找不到这个角色：这份候选表达不了这次请求，按"装不下/表达不了"明确失败。绝不退回一个
/// 写死的名字——那会把图塞进上游根本没声明过的字段，而且错得无声无息。
fn image_part_name(
    request: &PreparedImageRequest,
    kind: ImageParameterKind,
) -> Result<&str, AdapterError> {
    platform_image_parameter(&request.platform_parameters, kind).ok_or_else(|| {
        let role = match kind {
            ImageParameterKind::Reference => "reference image",
            ImageParameterKind::Mask => "mask",
        };
        AdapterError::UnsupportedInput(format!(
            "the offering declares no {role} parameter, so the edit endpoint has no part to carry it"
        ))
    })
}

fn ensure_input_size(bytes: usize) -> Result<(), AdapterError> {
    if bytes > MAX_INPUT_IMAGE_BYTES {
        return Err(AdapterError::UnsupportedInput(format!(
            "the reference image exceeds the {MAX_INPUT_IMAGE_BYTES}-byte limit"
        )));
    }
    Ok(())
}

/// 按上限读满一个响应体（下载参考图时用，避免把内存读穿）。
async fn read_limited(response: reqwest::Response, limit: usize) -> Result<Bytes, AdapterError> {
    let mut body = BytesMut::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(ambiguous_transport_error)?;
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(AdapterError::UnsupportedInput(format!(
                "the reference image exceeds the {limit}-byte limit"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body.freeze())
}

#[async_trait]
impl ImageAdapter for AihubmixImageAdapter {
    fn key(&self) -> &'static str {
        ADAPTER_KEY
    }

    async fn execute(
        &self,
        request: PreparedImageRequest,
        credential: &ProviderCredential,
    ) -> Result<ProviderSuccess, AdapterError> {
        match request.branch {
            ImageBranch::PromptOnly => self.generate(&request, credential).await,
            ImageBranch::ImageConditioned | ImageBranch::Masked => {
                self.edit(&request, credential).await
            }
        }
    }
}

/// 新协议的 JSON 入口：普通参数逐字进顶层字段，平台装载的图片参数名不参与（它们只是
/// 候选声明面里的名字，取值已提升到 `GatewayInput` 的图片字段）。
fn gateway_generation_body(input: &GatewayInput) -> Result<Value, AdapterError> {
    let mut object = Map::new();
    object.insert(
        "model".to_owned(),
        Value::String(input.provider_model_id.clone()),
    );
    object.insert(
        "prompt".to_owned(),
        Value::String(required_string(&input.native_parameters, "/prompt")?),
    );
    for (name, value) in gateway_passthrough_parameters(input) {
        object.insert(name.clone(), value.clone());
    }
    Ok(Value::Object(object))
}

/// 一次成功执行 → 内存载荷与强类型账务事实（RFC 0017 §2）；图片与账务事实分开，结果不进日志。
fn gateway_output(success: ProviderSuccess) -> ProviderOutput {
    let image_count = u32::try_from(success.images.len()).unwrap_or(u32::MAX);
    ProviderOutput {
        response_payload: ResponsePayload {
            // 应用层按自己的时钟兜底 `created`；这条渠道的响应信封没读它。
            created: None,
            images: success.images,
        },
        accounting_facts: AccountingFacts {
            usage: Some(success.usage),
            provider_cost: success.provider_cost,
            image_count,
            response_digest: success.response_digest,
            provider_trace_id: success.provider_trace_id,
        },
    }
}

impl AihubmixImageAdapter {
    /// 新协议的 JSON 入口：与旧路径同一份 wire 形状。
    async fn gateway_generate(
        &self,
        input: &GatewayInput,
        credential: &ProviderCredential,
        context: &dyn ExecutionContext,
    ) -> Result<ProviderOutput, AdapterError> {
        let body = gateway_generation_body(input)?;
        ensure_external_call_allowed(context)?;
        // 生成发送的最后资格：与取消线性化。此后到 .send() 之间没有可取消的等待。
        begin_generation_send(context)?;
        let response = self
            .client
            .post(self.endpoint(ImageBranch::PromptOnly)?)
            .timeout(external_call_timeout(self.timeout, context))
            .bearer_auth(credential.expose())
            .json(&body)
            .send()
            .await
            .map_err(ambiguous_transport_error)?;
        parse_response(response).await.map(gateway_output)
    }

    /// 新协议的编辑入口：图片直接来自 `input.reference_images` / `input.mask`，公网 URL 走既有下载。
    async fn gateway_edit(
        &self,
        input: &GatewayInput,
        credential: &ProviderCredential,
        context: &dyn ExecutionContext,
    ) -> Result<ProviderOutput, AdapterError> {
        let form = self.gateway_edit_form(input, context).await?;
        ensure_external_call_allowed(context)?;
        // 生成发送的最后资格：与取消线性化。此后到 .send() 之间没有可取消的等待。
        begin_generation_send(context)?;
        let response = self
            .client
            .post(self.endpoint(input.branch)?)
            .timeout(external_call_timeout(self.timeout, context))
            .bearer_auth(credential.expose())
            .multipart(form)
            .send()
            .await
            .map_err(ambiguous_transport_error)?;
        parse_response(response).await.map(gateway_output)
    }

    /// 组好新入口的编辑表单：部件名取自 [`GatewayInput::image_sites`]，不写死也不读
    /// `platform_parameters`。一张参考图是单值部件，多张是重复的 `image[]`（与旧入口同一条
    /// wire 规则）。
    async fn gateway_edit_form(
        &self,
        input: &GatewayInput,
        context: &dyn ExecutionContext,
    ) -> Result<multipart::Form, AdapterError> {
        let reference_site = input.image_sites.reference.as_ref().ok_or_else(|| {
            AdapterError::UnsupportedInput(
                "the offering declares no reference image parameter, so the edit endpoint has no \
                 part to carry it"
                    .to_owned(),
            )
        })?;
        if input.reference_images.is_empty() {
            return Err(AdapterError::UnsupportedInput(
                "the edit endpoint needs one reference image".to_owned(),
            ));
        }
        let mut form = multipart::Form::new()
            .text("model", input.provider_model_id.clone())
            .text(
                "prompt",
                required_string(&input.native_parameters, "/prompt")?,
            );
        for (name, value) in gateway_passthrough_parameters(input) {
            form = form.text(name.clone(), multipart_text(name, value)?);
        }
        let reference_part = if input.reference_images.len() > 1 {
            format!("{}[]", reference_site.parameter)
        } else {
            reference_site.parameter.clone()
        };
        for image in &input.reference_images {
            let bytes = self.gateway_image_bytes(image, context).await?;
            form = form.part(reference_part.clone(), image_part(bytes)?);
        }
        if let Some(mask) = &input.mask {
            let mask_site = input.image_sites.mask.as_ref().ok_or_else(|| {
                AdapterError::UnsupportedInput(
                    "the offering declares no mask parameter, so the edit endpoint has no part \
                     to carry it"
                        .to_owned(),
                )
            })?;
            let bytes = self.gateway_image_bytes(mask, context).await?;
            form = form.part(mask_site.parameter.clone(), image_part(bytes)?);
        }
        Ok(form)
    }

    /// 一份输入图取成字节：公网 URL 在自己下载前先过取消/期限闸，单次超时取 min(自身配置,
    /// 总期限剩余)。字节不落盘（RFC 0017 §2、§6）。
    async fn gateway_image_bytes(
        &self,
        image: &InputImage,
        context: &dyn ExecutionContext,
    ) -> Result<DecodedImage, AdapterError> {
        ensure_external_call_allowed(context)?;
        self.download_image_within(image.as_str(), external_call_timeout(self.timeout, context))
            .await
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
        let output = match input.branch {
            ImageBranch::PromptOnly => self.gateway_generate(&input, credential, context).await,
            ImageBranch::ImageConditioned | ImageBranch::Masked => {
                self.gateway_edit(&input, credential, context).await
            }
        };
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

fn generation_body(request: &PreparedImageRequest) -> Result<Value, AdapterError> {
    let mut object = Map::new();
    object.insert(
        "model".to_owned(),
        Value::String(request.provider_model_id.clone()),
    );
    object.insert(
        "prompt".to_owned(),
        Value::String(required_string(&request.native_parameters, "/prompt")?),
    );
    for (name, value) in passthrough_parameters(request) {
        object.insert(name.clone(), value.clone());
    }
    Ok(Value::Object(object))
}

/// 平台自己装好的参数名与图片参数之外的参数，**原样**交给上游。
///
/// 到手的参数面本身就是候选声明面里的子集（未声明的名字在受理期就按声明面丢掉了，见
/// `seeai_domain`），所以这里不做"认不认识"的判别，只跳过两类：平台自己落的 `model`/`prompt`，
/// 以及**平台装载进去的那些图片参数名**（名单由受理时算好，见
/// [`PreparedImageRequest::platform_parameters`]）——图片在这条链路上走文件部件，走了 JSON 体
/// 就会既重复又形态不对。其余名字逐字过去，取值一个都不改：归属不看取值的形状，
/// 所以名字像图也不会让它消失。
///
/// 唯一的例外是空值：`null` 表示"这一处没有给"，与平台的图片参数无关，也不是一个参数值，
/// 因此不进请求体（全平台共用的空值约定，见 `seeai_domain`）。
fn passthrough_parameters(request: &PreparedImageRequest) -> Vec<(&String, &Value)> {
    let Value::Object(parameters) = &request.native_parameters else {
        return Vec::new();
    };
    parameters
        .iter()
        .filter(|(name, value)| {
            !matches!(name.as_str(), "model" | "prompt")
                && !value.is_null()
                && !request
                    .platform_parameters
                    .iter()
                    .any(|declared| declared == *name)
        })
        .collect()
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

/// multipart 文本部件只接受标量：字符串、数字、布尔各自转成文本，**数组与对象明确失败**。
///
/// 为什么失败而不是找一个替代形态：multipart 的文本部件承载不了数组与对象，它们在这条线上
/// 没有可用的表示法。把数组序列化成一段 JSON 文本发过去，等于替上游发明一种它没有文档的形状
/// （多值究竟是重复同名字段、还是收一段 JSON，只有渠道文档说了才算数），上游很可能把它当成
/// 一个普通字符串——错得无声无息；而像从前那样返回 `None` 把它跳过，则是把调用方给的参数
/// 直接丢掉。两者都不做：这条路径上遇到不可承载的取值就报错，让调用方自己把它换成上游承认的
/// 形状（或改走 JSON 入口，那里任何 JSON 取值都能逐字过去）。
fn multipart_text(name: &str, value: &Value) -> Result<String, AdapterError> {
    scalar_text(value).ok_or_else(|| {
        AdapterError::UnsupportedInput(format!(
            "the multipart edit endpoint cannot carry `{name}`: {value} is not a scalar, and this \
             endpoint has no textual form for arrays or objects"
        ))
    })
}

fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

/// 一张输入图变成 multipart 文件部件：文件名与 MIME 按解码出的媒体类型给。
fn image_part(image: DecodedImage) -> Result<multipart::Part, AdapterError> {
    let name = format!("image.{}", extension_for(&image.media_type));
    multipart::Part::bytes(image.bytes.to_vec())
        .file_name(name)
        .mime_str(&image.media_type)
        .map_err(|error| AdapterError::UnsupportedInput(error.to_string()))
}

fn extension_for(media_type: &str) -> &'static str {
    match media_type {
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        _ => "png",
    }
}

/// 参考图取不到字节（公网 URL 下载失败、或它不是 http(s) 地址）。
///
/// 生成请求还没发出去，所以是**可证明未受理**：Job 直接失败并释放预授权，不进对账。
fn reference_image_unavailable(message: impl std::fmt::Display) -> AdapterError {
    ProviderCallError {
        code: "reference_image_unavailable".to_owned(),
        message: message.to_string(),
        trace_id: None,
        retry_safety: RetrySafety::SafeBeforeAcceptance,
        kind: ProviderFailureKind::Unknown,
        // 请求还没发出去：这次执行没有成本事实可带。
        provider_cost: None,
    }
    .into()
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

async fn parse_response(response: reqwest::Response) -> Result<ProviderSuccess, AdapterError> {
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
    success_from_body(&body, provider_trace_id)
}

/// 已读取的响应体 → 成功结果：结构、计量与结果信封都按渠道给的原形。
///
/// 与传输分开，是为了让"响应读成了、却不能用"这几种判定（结构不可读、用量自相矛盾、结果为空）
/// 只在这一处发生，也能直接在用例里喂一份响应体验证——它们都要带上这次执行的成本事实。
fn success_from_body(
    body: &[u8],
    provider_trace_id: Option<ProviderTraceId>,
) -> Result<ProviderSuccess, AdapterError> {
    let digest = sha256_hex(body);
    let parsed: ImageResponse = serde_json::from_slice(body).map_err(|error| {
        AdapterError::Provider(ProviderCallError {
            code: "provider_response_invalid".to_owned(),
            message: error.to_string(),
            trace_id: None,
            retry_safety: RetrySafety::AcceptanceUnknown,
            kind: ProviderFailureKind::UpstreamUnavailable,
            // 响应体读不成结构，用量与金额都取不到。
            provider_cost: None,
        })
    })?;
    let usage = parsed.usage.into_domain()?;
    let images = images_from_response(parsed.data)?;
    if images.is_empty() {
        return Err(AdapterError::Provider(ProviderCallError {
            code: "provider_result_empty".to_owned(),
            message: "provider returned no images".to_owned(),
            trace_id: None,
            retry_safety: RetrySafety::AcceptanceUnknown,
            kind: ProviderFailureKind::UpstreamUnavailable,
            // 响应读成了：这次执行确实发生过。这条渠道的响应里本就没有金额字段，所以成本事实
            // 与成功分支同口径——报"渠道不给金额"，而不是报"没采到"。
            provider_cost: Some(ProviderCost::Computed),
        }));
    }
    Ok(ProviderSuccess {
        images,
        usage,
        response_digest: digest,
        provider_trace_id,
        // 这条渠道的响应里**没有任何金额字段**（计量只给四分项 token）：成本只能由平台按
        // 实际用量与该渠道**成本费率**自算，所以这里明确报"这条渠道不给金额字段"，而不是
        // 报一个空的金额——"本就不报"与"报了没拿到"是两件事，处置也不同。
        provider_cost: ProviderCost::Computed,
    })
}

/// 上游给什么就留什么：给 `url` 就留 `url`、给 base64 就留 `b64_json`。
///
/// 两个都给时保留 `url`（体积小，且与本渠道的默认形态一致）；两个都没有说明这条响应
/// 不可用，按"结果缺失"失败——绝不猜一个空图出来。
fn images_from_response(data: Vec<ImageData>) -> Result<Vec<GeneratedImage>, AdapterError> {
    let mut images = Vec::with_capacity(data.len());
    for item in data {
        let image = match (item.url, item.b64_json) {
            (Some(url), _) => GeneratedImage::from_url(url),
            (None, Some(b64_json)) => GeneratedImage::from_base64(b64_json),
            (None, None) => {
                return Err(AdapterError::Provider(ProviderCallError {
                    code: "provider_result_missing".to_owned(),
                    message: "response item carries neither url nor b64_json".to_owned(),
                    trace_id: None,
                    retry_safety: RetrySafety::AcceptanceUnknown,
                    kind: ProviderFailureKind::UpstreamUnavailable,
                    // 单个结果项不可用就整次失败：此时响应已读进来，成本事实与成功分支同口径。
                    provider_cost: Some(ProviderCost::Computed),
                }));
            }
        };
        images.push(image);
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
struct ImageResponse {
    data: Vec<ImageData>,
    usage: UsageResponse,
}

#[derive(Debug, Deserialize)]
struct ImageData {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    b64_json: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UsageResponse {
    input_tokens: u64,
    input_tokens_details: TokenDetails,
    output_tokens: u64,
    output_tokens_details: TokenDetails,
    total_tokens: u64,
}

impl UsageResponse {
    fn into_domain(self) -> Result<TokenUsage, AdapterError> {
        let usage = TokenUsage {
            input_tokens: self.input_tokens,
            input_text_tokens: self.input_tokens_details.text_tokens,
            input_image_tokens: self.input_tokens_details.image_tokens,
            output_tokens: self.output_tokens,
            output_text_tokens: self.output_tokens_details.text_tokens,
            output_image_tokens: self.output_tokens_details.image_tokens,
            total_tokens: self.total_tokens,
        };
        usage.validate().map_err(|error| {
            AdapterError::Provider(ProviderCallError {
                code: "provider_usage_invalid".to_owned(),
                message: error.to_string(),
                trace_id: None,
                retry_safety: RetrySafety::AcceptanceUnknown,
                kind: ProviderFailureKind::UpstreamUnavailable,
                // 自算成本的输入自相矛盾：本该算得出金额却算不出来，记成缺口而不是猜一个数。
                provider_cost: Some(ProviderCost::Unavailable),
            })
        })?;
        Ok(usage)
    }
}

#[derive(Debug, Deserialize)]
struct TokenDetails {
    text_tokens: u64,
    image_tokens: u64,
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
