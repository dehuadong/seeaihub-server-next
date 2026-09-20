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

/// 平台侧失败类别：回答"这次失败对客户该怎么说、算不算平台自己的事件"。
///
/// 与 [`RetrySafety`] **正交**：后者只回答"能不能重试、要不要进对账"，
/// 两者可以任意组合（例如渠道 429 既可能可重试，也明确是渠道侧限流）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderFailureKind {
    /// 平台在渠道侧的账户欠费或额度不足。
    PlatformFunding,
    /// 平台与渠道之间的凭证、权限、IP 白名单、令牌范围，或渠道本身被禁用。
    PlatformCredential,
    /// 不是渠道返回的失败：平台自己的参数、配置或代码问题，租约过期，结果交付失败。
    PlatformInternal,
    /// 渠道拒绝了平台的请求，包含用 500 承载的参数错误。
    UpstreamRejected,
    /// 渠道 5xx、网关错误、网络中断，或响应形状不可用。
    UpstreamUnavailable,
    /// 渠道对平台限流。
    UpstreamRateLimited,
    /// 渠道明确因消费者内容而拒绝（审核类）。
    ConsumerContent,
    /// 拿不准；一律按平台侧处理。
    Unknown,
}

impl ProviderFailureKind {
    /// 全部类别：供枚举遍历的测试与校验使用。
    pub const ALL: [Self; 8] = [
        Self::PlatformFunding,
        Self::PlatformCredential,
        Self::PlatformInternal,
        Self::UpstreamRejected,
        Self::UpstreamUnavailable,
        Self::UpstreamRateLimited,
        Self::ConsumerContent,
        Self::Unknown,
    ];

    /// 落库/落日志用的稳定字符串，与 `serde` 的 `snake_case` 表示一致。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PlatformFunding => "platform_funding",
            Self::PlatformCredential => "platform_credential",
            Self::PlatformInternal => "platform_internal",
            Self::UpstreamRejected => "upstream_rejected",
            Self::UpstreamUnavailable => "upstream_unavailable",
            Self::UpstreamRateLimited => "upstream_rate_limited",
            Self::ConsumerContent => "consumer_content",
            Self::Unknown => "unknown",
        }
    }

    /// 从落库值还原。存储层有 CHECK 约束保证取值；解析不到说明存储被绕过，按错误处理。
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "platform_funding" => Some(Self::PlatformFunding),
            "platform_credential" => Some(Self::PlatformCredential),
            "platform_internal" => Some(Self::PlatformInternal),
            "upstream_rejected" => Some(Self::UpstreamRejected),
            "upstream_unavailable" => Some(Self::UpstreamUnavailable),
            "upstream_rate_limited" => Some(Self::UpstreamRateLimited),
            "consumer_content" => Some(Self::ConsumerContent),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    /// 是否属**平台侧事件**：运营需要据此处置（充值、改配置、修平台自己的 bug）。
    ///
    /// 渠道不可用、被限流、消费者内容被拒都不算平台侧事件——它们可观测，但不是平台要去修的。
    #[must_use]
    pub fn is_platform_side(self) -> bool {
        match self {
            Self::PlatformFunding
            | Self::PlatformCredential
            | Self::PlatformInternal
            | Self::UpstreamRejected
            | Self::Unknown => true,
            Self::UpstreamUnavailable | Self::UpstreamRateLimited | Self::ConsumerContent => false,
        }
    }
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
    /// 平台侧失败类别（见 [`ProviderFailureKind`]），与 `retry_safety` 正交。
    pub kind: ProviderFailureKind,
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
