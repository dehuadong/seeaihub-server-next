use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use reqwest::{Client, StatusCode, multipart};
use seeai_adapter_sdk::{
    AdapterDescriptor, AdapterError, DecodedImage, GeneratedImage, ImageAdapter,
    PreparedImageRequest, ProviderCallError, ProviderCost, ProviderCredential, ProviderFailureKind,
    ProviderSuccess, RetrySafety, decode_data_url, is_http_url,
};
use seeai_application::{AdapterFactory, ApplicationError};
use seeai_domain::{
    ImageBranch, ImageInputs, ImageParameterKind, TokenUsage, image_inputs,
    platform_image_parameter,
};
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::time::Duration;
use url::Url;

pub const ADAPTER_KEY: &str = "aihubmix-image-v1";
const MAX_PROVIDER_RESPONSE_BYTES: usize = 128 * 1024 * 1024;
/// 单张输入图（参考图或遮罩）在内存里的上限：data URL 就地解码、公网 URL 自己下载，
/// 两种形态都不落盘，因此必须有上限兜住内存。
const MAX_INPUT_IMAGE_BYTES: usize = 16 * 1024 * 1024;

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
            max_images: 16,
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
    for name in ["output_compression"] {
        if properties.contains_key(name) {
            require_type(properties, name, "integer")?;
        }
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
    // 最小请求用的图片取值就是调用方能给的形态：内联 data URL 或公网 URL。
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
    let mask = "data:image/png;base64,AAAA";
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

#[derive(Debug, Clone)]
pub struct AihubmixImageAdapter {
    client: Client,
    base_url: Url,
}

impl AihubmixImageAdapter {
    pub fn new(base_url: &str, timeout: Duration) -> Result<Self, AdapterError> {
        let normalized = format!("{}/", base_url.trim_end_matches('/'));
        let base_url = Url::parse(&normalized)
            .map_err(|error| AdapterError::Configuration(error.to_string()))?;
        let client = Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|error| AdapterError::Configuration(error.to_string()))?;
        Ok(Self { client, base_url })
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
        let response = self
            .client
            .get(url)
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
/// 多张**不在这里拦**：这条面按"最多 16 张"发布（`restrictions.max_images`），收几张是发布物与
/// 选路的事；这里再拦一道，等于让声明的能力与实现互相矛盾——而且被拦下的请求是平台侧故障，
/// 调用方完全无从判断。
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
    }
    .into()
}

async fn parse_response(response: reqwest::Response) -> Result<ProviderSuccess, AdapterError> {
    let status = response.status();
    // 对账标识：上游的逐请求标识，只用于对账，不参与计价（见 CONTEXT.md 的 Generation Attempt）。
    let provider_trace_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
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
    let digest = sha256_hex(&body);
    let parsed: ImageResponse = serde_json::from_slice(&body).map_err(|error| {
        AdapterError::Provider(ProviderCallError {
            code: "provider_response_invalid".to_owned(),
            message: error.to_string(),
            trace_id: None,
            retry_safety: RetrySafety::AcceptanceUnknown,
            kind: ProviderFailureKind::UpstreamUnavailable,
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
    let trace_id = parsed.and_then(|value| value.error.tid);
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
mod tests {
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
        let adapter =
            AihubmixImageAdapter::new("https://api.inferera.com/", Duration::from_secs(10))
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
        let adapter =
            AihubmixImageAdapter::new("https://api.inferera.com/", Duration::from_secs(10))
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
        let body = generation_body(&request(ImageBranch::PromptOnly))
            .expect("request should be supported");
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
        let inputs =
            reference_inputs(&value).expect("this surface is published for several images");
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
        let error = classify_image_value("asset://not-a-thing")
            .expect_err("an unknown shape must be rejected");
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
        let body = br#"{"error":{"message":"maybe accepted","code":"upstream_unreachable","tid":"req_1"}}"#;
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
}
