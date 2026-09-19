use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use reqwest::{Client, StatusCode, multipart};
use seeai_adapter_sdk::{
    AdapterDescriptor, AdapterError, GeneratedImage, ImageAdapter, PreparedImageRequest,
    ProviderCallError, ProviderCredential, ProviderSuccess, ResolvedAsset, RetrySafety,
};
use seeai_application::{AdapterFactory, ApplicationError};
use seeai_domain::{ImageBranch, TokenUsage};
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::time::Duration;
use url::Url;

pub const ADAPTER_KEY: &str = "aihubmix-image-v1";
const MAX_PROVIDER_RESPONSE_BYTES: usize = 128 * 1024 * 1024;
const MAX_OUTPUT_IMAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOTAL_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

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
                "extra",
            ],
            supported_extra_parameters: &["quality", "background", "output_compression", "user"],
            supported_branches: &[
                ImageBranch::PromptOnly,
                ImageBranch::ImageConditioned,
                ImageBranch::Masked,
            ],
            max_images: 1,
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
        validate_aihubmix_publication(capability_schema, restrictions)
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

fn validate_aihubmix_publication(schema: &Value, restrictions: &Value) -> Result<(), String> {
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
            return Err(format!("AIHubMix adapter requires native parameter {name}"));
        }
    }
    require_const_string(properties, "model")?;
    require_type(properties, "prompt", "string")?;
    for name in ["image", "mask"] {
        if properties.contains_key(name) {
            require_type(properties, name, "string")?;
        }
    }
    if properties.contains_key("n") {
        require_type(properties, "n", "integer")?;
    }
    for name in ["size", "output_format"] {
        if properties.contains_key(name) {
            require_string_enum(properties, name)?;
        }
    }
    if let Some(extra) = properties.get("extra") {
        if extra.get("type").and_then(Value::as_str) != Some("object")
            || extra.get("additionalProperties").and_then(Value::as_bool) != Some(false)
        {
            return Err("extra must be a closed object schema".to_owned());
        }
        let extra_properties = extra
            .get("properties")
            .and_then(Value::as_object)
            .ok_or_else(|| "extra.properties is required".to_owned())?;
        for name in ["quality", "background"] {
            if extra_properties.contains_key(name) {
                require_string_enum(extra_properties, name)?;
            }
        }
        if extra_properties.contains_key("output_compression") {
            require_type(extra_properties, "output_compression", "integer")?;
        }
        if extra_properties.contains_key("user") {
            require_type(extra_properties, "user", "string")?;
        }
    }
    let validator = jsonschema::validator_for(schema).map_err(|error| error.to_string())?;
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
    let cases = [
        (
            "prompt_only",
            serde_json::json!({"model": model, "prompt": "x"}),
        ),
        (
            "image_conditioned",
            serde_json::json!({"model": model, "prompt": "x", "image": "asset://image"}),
        ),
        (
            "masked",
            serde_json::json!({"model": model, "prompt": "x", "image": "asset://image", "mask": "asset://mask"}),
        ),
    ];
    for (branch, instance) in cases {
        if allowed.iter().any(|value| value.as_str() == Some(branch))
            && !validator.is_valid(&instance)
        {
            return Err(format!(
                "capability schema rejects the adapter's minimal {branch} request"
            ));
        }
    }
    for invalid in [
        serde_json::json!({"model": model}),
        serde_json::json!({"model": model, "prompt": 1}),
        serde_json::json!({"model": model, "prompt": "x", "unknown": true}),
        serde_json::json!({"model": model, "prompt": "x", "mask": "asset://mask"}),
        serde_json::json!({"model": model, "prompt": "x", "extra": {"unknown": true}}),
    ] {
        if validator.is_valid(&invalid) {
            return Err(
                "capability schema accepts an input the adapter cannot safely execute".to_owned(),
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
        validate_edit_assets(request)?;
        let image = single_asset(request, "/image")?;
        let mut form = multipart::Form::new()
            .text("model", request.provider_model_id.clone())
            .text(
                "prompt",
                required_string(&request.native_parameters, "/prompt")?,
            );
        for field in [
            "n",
            "size",
            "quality",
            "output_format",
            "background",
            "output_compression",
            "user",
        ] {
            if let Some(value) = wire_value(&request.native_parameters, field) {
                form = form.text(field.to_owned(), value);
            }
        }
        form = form.part("image", asset_part(image, "image")?);
        if let Some(mask) = request
            .assets
            .iter()
            .find(|asset| asset.native_parameter_path == "/mask")
        {
            form = form.part("mask", asset_part(mask, "mask")?);
        }
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
}

fn validate_edit_assets(request: &PreparedImageRequest) -> Result<(), AdapterError> {
    if request
        .assets
        .iter()
        .any(|asset| asset.native_parameter_path.starts_with("/images/"))
    {
        return Err(AdapterError::UnsupportedInput(
            "the metered /v1 edit endpoint is published for one image only".to_owned(),
        ));
    }
    Ok(())
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
    for field in ["n", "size", "output_format"] {
        if let Some(value) = request.native_parameters.get(field) {
            object.insert(field.to_owned(), value.clone());
        }
    }
    for field in ["quality", "background", "output_compression", "user"] {
        if let Some(value) = request
            .native_parameters
            .pointer(&format!("/extra/{field}"))
            .or_else(|| request.native_parameters.get(field))
        {
            object.insert(field.to_owned(), value.clone());
        }
    }
    Ok(Value::Object(object))
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

fn wire_value(parameters: &Value, field: &str) -> Option<String> {
    let value = parameters
        .pointer(&format!("/extra/{field}"))
        .or_else(|| parameters.get(field))?;
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

fn single_asset<'a>(
    request: &'a PreparedImageRequest,
    path: &str,
) -> Result<&'a ResolvedAsset, AdapterError> {
    let matches = request
        .assets
        .iter()
        .filter(|asset| asset.native_parameter_path == path)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [asset] => Ok(*asset),
        [] => Err(AdapterError::UnsupportedInput(format!(
            "missing asset binding {path}"
        ))),
        _ => Err(AdapterError::UnsupportedInput(format!(
            "multiple assets bound to {path}"
        ))),
    }
}

fn asset_part(asset: &ResolvedAsset, name: &str) -> Result<multipart::Part, AdapterError> {
    multipart::Part::bytes(asset.bytes.to_vec())
        .file_name(format!("{name}.{}", extension_for(&asset.media_type)))
        .mime_str(&asset.media_type)
        .map_err(|error| AdapterError::UnsupportedInput(error.to_string()))
}

fn extension_for(media_type: &str) -> &'static str {
    match media_type {
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        _ => "png",
    }
}

fn ambiguous_transport_error(error: reqwest::Error) -> AdapterError {
    ProviderCallError {
        code: "provider_transport_unknown".to_owned(),
        message: error.to_string(),
        trace_id: None,
        retry_safety: RetrySafety::AcceptanceUnknown,
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
        })
    })?;
    let usage = parsed.usage.into_domain()?;
    let media_type = media_type_for_format(parsed.output_format.as_deref());
    let mut images = Vec::with_capacity(parsed.data.len());
    let mut total_output_bytes = 0_usize;
    for item in parsed.data {
        let encoded = item.b64_json.ok_or_else(|| {
            AdapterError::Provider(ProviderCallError {
                code: "provider_result_missing".to_owned(),
                message: "response item has no b64_json".to_owned(),
                trace_id: None,
                retry_safety: RetrySafety::AcceptanceUnknown,
            })
        })?;
        if encoded.len() > MAX_OUTPUT_IMAGE_BYTES.saturating_mul(4).div_ceil(3) + 4 {
            return Err(provider_output_too_large());
        }
        let decoded = STANDARD.decode(encoded).map_err(|error| {
            AdapterError::Provider(ProviderCallError {
                code: "provider_result_invalid_base64".to_owned(),
                message: error.to_string(),
                trace_id: None,
                retry_safety: RetrySafety::AcceptanceUnknown,
            })
        })?;
        if decoded.len() > MAX_OUTPUT_IMAGE_BYTES {
            return Err(provider_output_too_large());
        }
        total_output_bytes = total_output_bytes.saturating_add(decoded.len());
        if total_output_bytes > MAX_TOTAL_OUTPUT_BYTES {
            return Err(provider_output_too_large());
        }
        validate_image_magic(&decoded, media_type)?;
        images.push(GeneratedImage {
            media_type: media_type.to_owned(),
            sha256: sha256_hex(&decoded),
            bytes: Bytes::from(decoded),
        });
    }
    if images.is_empty() {
        return Err(AdapterError::Provider(ProviderCallError {
            code: "provider_result_empty".to_owned(),
            message: "provider returned no images".to_owned(),
            trace_id: None,
            retry_safety: RetrySafety::AcceptanceUnknown,
        }));
    }
    Ok(ProviderSuccess {
        images,
        usage,
        response_digest: digest,
        provider_trace_id,
    })
}

fn provider_response_too_large() -> AdapterError {
    ProviderCallError {
        code: "provider_response_too_large".to_owned(),
        message: "provider response exceeded the configured safety limit".to_owned(),
        trace_id: None,
        retry_safety: RetrySafety::AcceptanceUnknown,
    }
    .into()
}

fn provider_output_too_large() -> AdapterError {
    ProviderCallError {
        code: "provider_output_too_large".to_owned(),
        message: "provider output image exceeded the configured safety limit".to_owned(),
        trace_id: None,
        retry_safety: RetrySafety::AcceptanceUnknown,
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
    ProviderCallError {
        code,
        message,
        trace_id,
        retry_safety,
    }
}

fn media_type_for_format(format: Option<&str>) -> &'static str {
    match format {
        Some("jpeg") | Some("jpg") => "image/jpeg",
        Some("webp") => "image/webp",
        _ => "image/png",
    }
}

fn validate_image_magic(bytes: &[u8], media_type: &str) -> Result<(), AdapterError> {
    let valid = match media_type {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/webp" => bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(AdapterError::Provider(ProviderCallError {
            code: "provider_result_media_mismatch".to_owned(),
            message: format!("result does not match declared media type {media_type}"),
            trace_id: None,
            retry_safety: RetrySafety::AcceptanceUnknown,
        }))
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[derive(Debug, Deserialize)]
struct ImageResponse {
    data: Vec<ImageData>,
    output_format: Option<String>,
    usage: UsageResponse,
}

#[derive(Debug, Deserialize)]
struct ImageData {
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
    use seeai_adapter_sdk::ResolvedAsset;

    fn request(branch: ImageBranch) -> PreparedImageRequest {
        PreparedImageRequest {
            provider_model_id: "gpt-image-2".to_owned(),
            branch,
            native_parameters: serde_json::json!({
                "prompt": "test",
                "n": 1,
                "size": "1024x1024",
                "output_format": "png",
                "extra": {"quality": "low"}
            }),
            assets: Vec::new(),
        }
    }

    /// 发布素材现在是"数组形式"：Profile 与 restrictions 在 `offerings[0]` 内。
    fn published_config() -> Value {
        serde_json::from_str(include_str!(
            "../../../config/bootstrap/aihubmix-gpt-image-2.json"
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
                &offering["capability_schema"],
                &offering["restrictions"],
            )
            .expect("bootstrap contract should be executable");
    }

    #[test]
    fn rejects_schema_with_wrong_prompt_type() {
        let mut config = published_config();
        config["offerings"][0]["capability_schema"]["properties"]["prompt"]["type"] =
            Value::String("integer".to_owned());
        let offering = published_offering(&config).clone();
        let error = AihubmixAdapterFactory
            .validate_publication(
                ADAPTER_KEY,
                &offering["capability_schema"],
                &offering["restrictions"],
            )
            .expect_err("wrong prompt type must be rejected");
        assert!(error.contains("prompt"));
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
    fn maps_extra_quality_to_openai_wire_field() {
        let body = generation_body(&request(ImageBranch::PromptOnly))
            .expect("request should be supported");
        assert_eq!(
            body.pointer("/quality"),
            Some(&Value::String("low".to_owned()))
        );
        assert!(body.get("extra").is_none());
    }

    #[test]
    fn rejects_multiple_images_on_metered_edit_contract() {
        let mut value = request(ImageBranch::ImageConditioned);
        value.assets = vec![ResolvedAsset {
            native_parameter_path: "/images/0".to_owned(),
            position: 0,
            media_type: "image/png".to_owned(),
            sha256: "abc".to_owned(),
            bytes: Bytes::from_static(b"image"),
        }];
        let error = validate_edit_assets(&value).expect_err("multi-image edit must be rejected");
        assert!(error.to_string().contains("one image only"));
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
