use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;
use seeai_domain::{ImageBranch, ProviderCostSource, TokenUsage};
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

/// 一次执行的输入：被选中候选的原生参数（图片输入已经在里面，落在该候选自己的参数名上）。
#[derive(Debug, Clone)]
pub struct PreparedImageRequest {
    pub provider_model_id: String,
    pub branch: ImageBranch,
    pub native_parameters: Value,
    /// **平台自己装好的参数名**，受理时按候选声明面与分支算好后随请求一起冻结。
    ///
    /// Driver 靠它分清"哪些参数是平台的图"与"哪些参数是普通的声明参数"：名单里的名字才是
    /// 平台装载的参考图/遮罩，其余名字按原样交给上游。归属因此不看取值的形状——
    /// 调用方给一个叫 `images` 的字符串数组（渠道文档里的原生名）不会被误认成平台的图。
    ///
    /// 到手的参数面本身已经是候选声明面里的子集（没声明的名字在受理期就丢掉了），
    /// 所以名单只用来分角色，不用来判"这个参数认不认识"。
    ///
    /// 名单与 [`PreparedImageRequest::native_parameters`] 是同一份快照的两半：前者说"哪些名字
    /// 是平台的"，后者说"这些名字下的取值是什么"，两者都来自受理时固化的那一次请求。
    pub platform_parameters: Vec<String>,
    /// 这条渠道**声明的成本币种**（受理时随请求冻结，与供给声明同源）。
    ///
    /// 上游报出来的金额本身不带币种，所以 Driver 拿不到"这个数是什么钱"——它只能把受理时
    /// 冻结的那份声明带回来。这里**不假定任何币种**（USD 也只是某个渠道的声明值）。
    ///
    /// 它不参与请求摘要：币种不改变发往上游的任何字节，换一个币种不是另一次请求。
    pub cost_currency: String,
}

/// 一次生成的一张图：**渠道给什么就是什么**——给 `url` 就留 `url`、给 base64 就留 `b64_json`。
///
/// 形状本身保证"恰好其一"：只有两种取图方式，没有"两项都在"或"一项都没有"的表示法。
/// 平台不下载、不解码、不归档，因此也不需要第三种形态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GeneratedImage {
    /// 上游给的是公网地址（或它自己的临时链接）。
    #[serde(rename = "url")]
    Url(String),
    /// 上游给的是内联 base64。
    #[serde(rename = "b64_json")]
    B64Json(String),
}

impl GeneratedImage {
    /// 上游给的是公网地址。
    #[must_use]
    pub fn from_url(url: String) -> Self {
        Self::Url(url)
    }

    /// 上游给的是内联 base64。
    #[must_use]
    pub fn from_base64(b64_json: String) -> Self {
        Self::B64Json(b64_json)
    }
}

/// 是不是公网 http(s) 地址：参考图与遮罩允许的另一种形态（另一种是 data URL）。
#[must_use]
pub fn is_http_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

/// 一份**在内存里**的输入图：媒体类型加上字节。
///
/// 平台不落盘，所以一张输入图在 Driver 手里的形态就是这两样；用具名类型而不是
/// `(String, Bytes)`，免得调用处把两者写反、也免得读代码的人要去数第几个位置是什么。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedImage {
    pub media_type: String,
    pub bytes: Bytes,
}

/// 解一个 `data:<media-type>;base64,<payload>`。
///
/// 不是 data URL、或者载荷不是 base64，都返回 `Err`：调用方据此改按公网 URL 处理，
/// 而不是拿到一堆坏字节。
pub fn decode_data_url(value: &str) -> Result<DecodedImage, String> {
    let rest = value
        .strip_prefix("data:")
        .ok_or_else(|| "not a data url".to_owned())?;
    let (meta, payload) = rest
        .split_once(',')
        .ok_or_else(|| "data url carries no comma".to_owned())?;
    let (media_type, encoding) = match meta.split_once(';') {
        Some((media_type, encoding)) => (media_type, encoding),
        None => (meta, ""),
    };
    if encoding != "base64" {
        return Err(format!("unsupported data url encoding `{encoding}`"));
    }
    let bytes = STANDARD
        .decode(payload)
        .map_err(|error| format!("data url payload is not valid base64: {error}"))?;
    let media_type = if media_type.is_empty() {
        "application/octet-stream".to_owned()
    } else {
        media_type.to_owned()
    };
    Ok(DecodedImage {
        media_type,
        bytes: Bytes::from(bytes),
    })
}

#[derive(Debug, Clone)]
pub struct ProviderSuccess {
    pub images: Vec<GeneratedImage>,
    pub usage: TokenUsage,
    pub response_digest: String,
    /// 上游逐请求标识（例如任务式上游的 task id），**只用于对账**：
    /// 不参与计价，也不属于计量证据（见 `CONTEXT.md` 的 `Generation Attempt`）。
    ///
    /// 平台把它落到已存在的 `attempts.provider_trace_id` 列。注意：本仓库**只用它做人工
    /// 对账**，不用它自动把结果取回来（那需要另一套模型与列）——创建响应失联一律进对账。
    pub provider_trace_id: Option<String>,
    /// 这次执行看到的**成本事实**（成本平面，币种按渠道声明）。
    ///
    /// 它是**成本口径**，不是计量证据：计量事实仍然是 [`ProviderSuccess::usage`] 的四分项
    /// token，成本不替代它，也不参与对客金额。
    pub provider_cost: ProviderCost,
}

/// 上游终态直接给出的成本金额。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredCost {
    /// 上游给的金额，按**该渠道声明的币种**的微单位（1e-6）。
    pub amount_microusd: u64,
    /// 该渠道声明的成本币种（受理时随请求冻结的那一份声明）。
    pub currency: String,
}

/// 一次执行看到的成本事实：判据是"成本从哪来"，不是"金额对不对"。
///
/// 做成三态而不是"可选金额"，是为了让两种"没有金额"分开：**这条渠道不报金额**（成本只能由
/// 平台自算）与**本该报却这次没拿到**（不得猜测）的处置完全不同——合成一个 `None`，前者会被
/// 误记成成本缺口，后者会被自算的费率悄悄顶替。
///
/// 三个成员名与领域侧 [`ProviderCostSource`] 的取值**逐字对齐**（`computed` / `declared` /
/// `unavailable`）：同一件事在两层只换一个类型，不换名字，免得读的人以为它们是两套判据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderCost {
    /// 上游直接给了金额：**取它，不自己算**（含渠道侧折扣，比自算更权威）。
    Declared(DeclaredCost),
    /// 这条渠道**不给金额字段**：成本由平台按实际用量与该渠道**成本费率**自算。
    Computed,
    /// 本该有金额，这次却拿不到（缺字段 / 负数 / 解析失败）：**不得猜测**，留作成本缺口。
    Unavailable,
}

/// SDK 的报告 → 领域的成本来源：**唯一的**一处映射。
///
/// 两层的取值面是同一件事的两种写法，所以转换只写在这里：调用方拿它取来源，不必再对
/// [`ProviderCost`] 判一次三态——判两处就会各自漂移，而漂移的表现是同一笔成本被记成两种来源。
impl From<&ProviderCost> for ProviderCostSource {
    fn from(report: &ProviderCost) -> Self {
        match report {
            ProviderCost::Declared(_) => Self::Declared,
            ProviderCost::Computed => Self::Computed,
            ProviderCost::Unavailable => Self::Unavailable,
        }
    }
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

/// 一个 Driver 的**传输能力**声明：它在线上能写什么。
///
/// `supported_top_level_parameters` 的语义是"这个 Driver 能写上线文的**字段名**"，
/// **不是**"调用方能提交哪些参数"——调用方看到的是该 Vendor Model 的合同，渠道包装的差异
/// 由供给的承载面与参数映射在平台内部吸收。发布期据此判"这条供给声明要写的字段，发得出去吗"。
#[derive(Debug, Clone)]
pub struct AdapterDescriptor {
    pub key: &'static str,
    /// 能写上线文的**顶层字段名**。
    pub supported_top_level_parameters: &'static [&'static str],
    /// 能写进某个嵌套容器里的字段名；容器本身同样必须是上表里的一个顶层字段名。
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
    /// 这次执行**已经看到**的成本事实（成本平面），随错误一起交回平台。
    ///
    /// 与 [`ProviderSuccess::provider_cost`] 同一组取值、同一套语义：失败件与成功件同源同形，
    /// 终态读到金额之后就算这次没有结果图，那笔钱也已经花了，没有理由跟着结果一起丢。
    /// `None` 表示**这次执行没采到**成本事实（例如请求根本没交到渠道），不是"成本是 0"，
    /// 也不是"这条渠道不报金额"——后者用 [`ProviderCost::Computed`] 明说。
    ///
    /// 改写错误时（补对账标识、改重试安全性）只加不改：它是已判出的事实，重建错误对象时
    /// 漏掉它，表现就是失败件的成本在账上与缺口清单两头都看不见。
    pub provider_cost: Option<ProviderCost>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_inline_data_urls_into_bytes() {
        let encoded = STANDARD.encode([0x89_u8, b'P', b'N', b'G']);
        let decoded =
            decode_data_url(&format!("data:image/png;base64,{encoded}")).expect("data url decodes");
        assert_eq!(decoded.media_type, "image/png");
        assert_eq!(&decoded.bytes[..], &[0x89, b'P', b'N', b'G']);
        // 不带媒体类型时保持中性，不假装知道格式。
        let decoded = decode_data_url("data:;base64,AAAA").expect("decodes");
        assert_eq!(decoded.media_type, "application/octet-stream");
        for bad in [
            "https://example.invalid/a.png",
            "data:image/png,notbase64",
            "data:image/png;base64",
        ] {
            assert!(decode_data_url(bad).is_err(), "`{bad}` is not a data url");
        }
        assert!(is_http_url("https://example.invalid/a.png"));
        assert!(!is_http_url("data:image/png;base64,AAAA"));
    }

    #[test]
    fn generated_images_keep_only_the_shape_the_provider_gave() {
        let url = GeneratedImage::from_url("https://example.invalid/a.png".to_owned());
        assert_eq!(
            serde_json::to_value(&url).expect("serializes"),
            serde_json::json!({"url": "https://example.invalid/a.png"})
        );
        let base64 = GeneratedImage::from_base64("AAAA".to_owned());
        assert_eq!(
            serde_json::to_value(&base64).expect("serializes"),
            serde_json::json!({"b64_json": "AAAA"})
        );
        // 读回来还是同一种形态（结果信封落库、再读出来）。
        assert_eq!(
            serde_json::from_value::<GeneratedImage>(serde_json::json!({"url": "u"}))
                .expect("a url reads back"),
            GeneratedImage::from_url("u".to_owned())
        );
        // 形状本身就是"恰好其一"：两项都在、一项都没有都不是合法的图。
        for impossible in [
            serde_json::json!({}),
            serde_json::json!({"url": "u", "b64_json": "b"}),
        ] {
            assert!(
                serde_json::from_value::<GeneratedImage>(impossible.clone()).is_err(),
                "{impossible} 不是恰好一项"
            );
        }
    }
}
