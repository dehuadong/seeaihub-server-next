use async_trait::async_trait;
use bytes::Bytes;
use seeai_domain::{ImageBranch, TokenUsage};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::{Debug, Formatter};
use thiserror::Error;

#[derive(Clone)]
pub struct ProviderCredential(String);

impl ProviderCredential {
    pub fn new(value: String) -> Result<Self, AdapterError> {
        if value.trim().is_empty() {
            return Err(AdapterError::Configuration(
                "provider credential is empty".to_owned(),
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl Debug for ProviderCredential {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ProviderCredential([REDACTED])")
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedAsset {
    pub native_parameter_path: String,
    pub position: u16,
    pub media_type: String,
    pub sha256: String,
    pub bytes: Bytes,
}

#[derive(Debug, Clone)]
pub struct PreparedImageRequest {
    pub provider_model_id: String,
    pub branch: ImageBranch,
    pub native_parameters: Value,
    pub assets: Vec<ResolvedAsset>,
}

#[derive(Debug, Clone)]
pub struct GeneratedImage {
    pub media_type: String,
    pub bytes: Bytes,
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub struct ProviderSuccess {
    pub images: Vec<GeneratedImage>,
    pub usage: TokenUsage,
    pub response_digest: String,
    /// 上游逐请求标识（例如任务式上游的 task id），**只用于对账**：
    /// 不参与计价，也不属于计量证据（见 `CONTEXT.md` 的 `Generation Attempt`）。
    ///
    /// 平台把它落到已存在的 `attempts.provider_trace_id` 列。注意：本仓库**不用它做
    /// 跨调用恢复**（那需要新增列与拆分端口，属后续工作项）——创建响应失联一律进对账。
    pub provider_trace_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrySafety {
    SafeBeforeAcceptance,
    NotRetryable,
    AcceptanceUnknown,
}

#[derive(Debug, Clone)]
pub struct AdapterDescriptor {
    pub key: &'static str,
    pub supported_top_level_parameters: &'static [&'static str],
    pub supported_extra_parameters: &'static [&'static str],
    pub supported_branches: &'static [ImageBranch],
    pub max_images: u64,
}

#[derive(Debug, Clone, Error)]
#[error("provider call failed: {code}: {message}")]
pub struct ProviderCallError {
    pub code: String,
    pub message: String,
    pub trace_id: Option<String>,
    pub retry_safety: RetrySafety,
}

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("adapter configuration error: {0}")]
    Configuration(String),
    #[error("unsupported provider input: {0}")]
    UnsupportedInput(String),
    #[error(transparent)]
    Provider(#[from] ProviderCallError),
}

#[async_trait]
pub trait ImageAdapter: Send + Sync {
    fn key(&self) -> &'static str;

    async fn execute(
        &self,
        request: PreparedImageRequest,
        credential: &ProviderCredential,
    ) -> Result<ProviderSuccess, AdapterError>;
}
