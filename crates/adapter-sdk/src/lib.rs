use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;
use seeai_domain::{ImageBranch, ProviderCostSource, TokenUsage};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::{Debug, Formatter};
use thiserror::Error;

mod gateway;
pub use gateway::{
    AcceptanceError, AcceptedHandle, AccountingFacts, AccountingQuery, Deadline, DispatchGate,
    ExecutionContext, GatewayAdapter, GatewayInput, ImageSite, ImageSites, ImageValueShape,
    InputImage, ProviderOutput, ProviderTaskHandle, ProviderTaskState, ProviderTraceId,
    QueryAccountingCapability, ResponsePayload, begin_generation_send,
    ensure_external_call_allowed, ensure_read_call_allowed, external_call_timeout,
    gateway_passthrough_parameters,
};

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
    pub provider_trace_id: Option<ProviderTraceId>,
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

/// 一次执行要覆盖的**字节上限**：入口 wire、上游响应、编码后的对客正文。
///
/// 这些是 Driver 实际会接受/产出的上界，写成一份共享声明，调用方才能算出"一次执行最坏要预留
/// 多少内存"。各段单独相加会低估：上游原始响应、解析出的图片字符串与编码后的正文可能同时
/// 存活（RFC 0018 §2.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GatewayByteLimits {
    /// 入口请求 wire 上界：认证之后才解析，解析结果与它同时存活。
    pub request_wire_bytes: usize,
    /// 上游响应正文上界：一次读进内存以后再解析。
    pub provider_response_bytes: usize,
}

/// 入口请求 wire 的上限：与 API 的正文上限同一口径，解析结果与它同时存活。
pub const GATEWAY_REQUEST_WIRE_BYTES: usize = 16 * 1024 * 1024;

/// 实测：贴住 [`GATEWAY_REQUEST_WIRE_BYTES`] 的**已支持输入形态**（一个 data URL 参考图加普通
/// 参数）经 API 真实解析路径（`CreateGenerationBody`）之后，解析结构在峰值时额外持有的字节数，
/// 不含 wire 本身。
///
/// 测量命令（Linux、dev profile、一条用例一个进程，`VmHWM` 前后差值）：
///
/// ```text
/// cargo test -p seeai-api --bin seeai-api -- --ignored --nocapture --test-threads=1 \
///   memory_request_parse_peak_image
/// ```
///
/// 实测：wire 16777216 字节，额外峰值 17544–17992 KiB（四次）；取 18 MiB 收口。
///
/// **已知缺口（T1 剩余项，必须知道）**：节点密集的 JSON 在 §2.2 的节点/字段计数落地前不受这个
/// 常数约束。同一命令下的 `memory_request_parse_peak_small_object_nodes`（16 MiB 的
/// `[{"a":0},…]`）实测解析结构峰值 2065756 KiB（约 2.0 GiB），
/// `memory_request_parse_peak_scalar_nodes`（`[0,0,…]`）实测 526036 KiB。也就是说：在节点计数
/// 落地前，一份 16 MiB 的请求能把堆抬到远超本常数——那时这份预留不再是内存保证。这里是按
/// **已支持输入形态**实测的最大值钉的，节点/字段计数是收口这一缺口的既定机制。
pub const GATEWAY_REQUEST_PARSE_BYTES: usize = 18 * 1024 * 1024;

/// 实测：解析结构提升为 Adapter 输入（取参考图/遮罩并构造强类型 [`InputImage`]）时的额外峰值。
///
/// 测量命令：
///
/// ```text
/// cargo test -p seeai-api --bin seeai-api -- --ignored --nocapture --test-threads=1 \
///   memory_mapped_parameter_peak
/// ```
///
/// 实测：额外峰值 16472–16540 KiB（四次）；取 17 MiB 收口。其中约 16 MiB 是
/// `seeai_domain::take_contract_image_inputs` 对图片值的那一次拷贝——它与 map 里的原值在峰值时
/// 同时存活（RFC 0018 §2.2 要求图片输入共享，落地后应重测收窄）。
pub const GATEWAY_MAPPED_PARAMETER_BYTES: usize = 17 * 1024 * 1024;

/// 实测：上游响应阶段（原始响应缓冲峰值 + 解析出的图片字符串之和）相对声明响应上限的**百分比**。
///
/// 测量命令：
///
/// ```text
/// cargo test -p seeai-api --bin seeai-api -- --ignored --nocapture --test-threads=1 \
///   memory_provider_response_and_parsed_image_peak
/// ```
///
/// 实测（AIHubMix 128 MiB 响应上限）：正文 134213632 字节、解析出的图片字符串 134213380 字节、
/// 额外峰值 263236 / 262864 / 263116 KiB（三次）＝2.0084 / 2.0056 / 2.0083 × 正文；取 201% 收口。两个原因叠加成
/// 这两倍：读缓冲是 `BytesMut` 逐段扩容（上限是声明值的 2 的幂，所以最终容量正好等于正文长度，
/// 峰值里最坏再多一份），解析出的图片字符串又是一份独立副本。
///
/// 注意：Adapter 侧没有按 `Content-Length` 预分配读缓冲，扩容的瞬时峰值因此是"几何倍数"而不是
/// "正文长度"；声明上限都是 2 的幂（128 MiB / 8 MiB），实测落在 2.01 倍。换成非 2 的幂的上限时
/// 这个比例会变（`BytesMut` 会多跳一级容量），届时必须重测。
///
/// 测量只在 AIHubMix（128 MiB）上做过；APIMart（8 MiB）用同一段 `BytesMut` 逐段扩容读取，按同一
/// 常数计——8 MiB 那一档**没有单独实测**，换读取实现时要重测。
pub const GATEWAY_PROVIDER_STAGE_PERCENT: usize = 201;

/// 实测：对客编码正文在图片字符串之外的固定开销（信封）。
///
/// 测量命令：
///
/// ```text
/// cargo test -p seeai-api --bin seeai-api -- --ignored --nocapture --test-threads=1 \
///   memory_encoded_client_body_peak
/// ```
///
/// 实测：一张 134213380 字节的图编码后是 134213679 字节（比例 1.0000004），编码那一步本身的额外
/// 峰值 132292–132676 KiB（三次，约等于编码正文本身）；合同允许的产出张数上限
/// （`NO_CONTRACT_MAX_OUTPUT_IMAGES = 10`）下、图片字符串全为空时信封是 191 字节。
///
/// 编码后正文因此是**实测出来的 ≈ 1 倍上游正文加这个有界信封**，不再是猜测的倍数：客户端正文
/// 是对上游响应里那些字符串的再序列化，而 JSON 转义只会比原始转义更短或等长（`"`/`\`/控制字符
/// 在原始正文里本来就是转义形式），所以"1 倍 + 信封"是有依据的上界。
pub const GATEWAY_ENCODED_ENVELOPE_BYTES: usize = 191;

/// 只读对账查询响应上限的缺省值（1 MiB）。
///
/// 对账读取与生成读取是两条不同的路径：生成响应要装下结果图，对账只需要状态、计量与成本
/// 字段。它因此用一条独立、更小但明确的上限；超过这条上限的响应只留下证据缺口，不无界读图
/// （RFC 0018 §2.1）。
pub const GATEWAY_RECONCILIATION_READ_BYTES: usize = 1024 * 1024;

/// 覆盖对账读取上限（字节）的配置变量名。
pub const RECONCILIATION_READ_BYTES_ENV: &str = "GENERATION_RECONCILIATION_READ_BYTES";

/// `GENERATION_RECONCILIATION_READ_BYTES` 的配置值；缺省 [`GATEWAY_RECONCILIATION_READ_BYTES`]。
///
/// 读不出来或不是正数时给明确错误：配置错误由启动校验点名，不由运行时静默换一个数。
pub fn reconciliation_read_bytes_from_env() -> Result<usize, String> {
    match std::env::var(RECONCILIATION_READ_BYTES_ENV) {
        Ok(value) => {
            let parsed = value.trim().parse::<usize>().map_err(|error| {
                format!("{RECONCILIATION_READ_BYTES_ENV} is not a byte count: {error}")
            })?;
            if parsed == 0 {
                return Err(format!("{RECONCILIATION_READ_BYTES_ENV} must be positive"));
            }
            Ok(parsed)
        }
        Err(std::env::VarError::NotPresent) => Ok(GATEWAY_RECONCILIATION_READ_BYTES),
        Err(std::env::VarError::NotUnicode(_)) => Err(format!(
            "{RECONCILIATION_READ_BYTES_ENV} is not valid unicode"
        )),
    }
}

impl GatewayByteLimits {
    /// 这条通路上只读对账查询的响应上限。
    ///
    /// 缺省 [`GATEWAY_RECONCILIATION_READ_BYTES`]，`GENERATION_RECONCILIATION_READ_BYTES` 可覆盖；
    /// 无论怎么配都不会超过本 Adapter 声明的 [`GatewayByteLimits::provider_response_bytes`]——
    /// 对账读取不会比生成读取更大。配置读不出来时按缺省值走：运行时的读取仍然有界。
    #[must_use]
    pub fn reconciliation_read_bytes(self) -> usize {
        reconciliation_read_bytes_from_env()
            .unwrap_or(GATEWAY_RECONCILIATION_READ_BYTES)
            .min(self.provider_response_bytes)
            .max(1)
    }

    /// 一次执行的最坏占用上界（字节），由**实测常数**逐项相加（RFC 0018 §2.1）：
    ///
    /// ```text
    /// R = I                 入口 wire（声明的正文上限）
    ///   + GATEWAY_REQUEST_PARSE_BYTES            解析结构（实测）
    ///   + GATEWAY_MAPPED_PARAMETER_BYTES         提升为 Adapter 输入（实测）
    ///   + U × GATEWAY_PROVIDER_STAGE_PERCENT/100 原始响应缓冲峰值 + 解析出的图片字符串（实测）
    ///   + U + GATEWAY_ENCODED_ENVELOPE_BYTES     编码后的对客正文（实测信封）
    ///   + T                  transport 用户态缓冲（配置上界，见下）
    /// ```
    ///
    /// `U` 是本通路声明的上游响应上限。图片只在必要处计一次：请求侧的图片值按"已支持输入形态"
    /// 的实测常数计（[`GATEWAY_REQUEST_PARSE_BYTES`] 与 [`GATEWAY_MAPPED_PARAMETER_BYTES`] 已经
    /// 含了它），上游侧的图片字符串与编码正文各计一次——它们在峰值时确实是两份独立字节。
    ///
    /// `transport_buffer_bytes` 由调用方按 **transport 配置的上界**传入（HTTP/1 解析缓冲 +
    /// HTTP/2 发送缓冲，见运维配置的「连接与断开监视」）。它**没有独立实测峰值**：这里的取值是
    /// 配置写死的上限，不是估出来的峰值——Hyper 在用户态的写缓冲不会超过它。
    ///
    /// 用饱和加法，配置极大时退化为 `usize::MAX` 而不是回绕。
    #[must_use]
    pub const fn max_bytes_per_execution(self, transport_buffer_bytes: usize) -> usize {
        let provider_stage = self
            .provider_response_bytes
            .saturating_mul(GATEWAY_PROVIDER_STAGE_PERCENT)
            / 100;
        let encoded = self
            .provider_response_bytes
            .saturating_add(GATEWAY_ENCODED_ENVELOPE_BYTES);
        self.request_wire_bytes
            .saturating_add(GATEWAY_REQUEST_PARSE_BYTES)
            .saturating_add(GATEWAY_MAPPED_PARAMETER_BYTES)
            .saturating_add(provider_stage)
            .saturating_add(encoded)
            .saturating_add(transport_buffer_bytes)
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
    /// 一次调用**能带进去几张参考图**的上限。
    ///
    /// 只管输入：输出张数由合同声明的 `n` 表达，与这个数不是一回事，别拿它当输出上限。
    pub max_reference_images: u64,
    /// 这条通路的**上游响应会不会带金额**（`cost`）。
    ///
    /// `true`：终态里能读到实扣金额，`upstream_declared` 那两种形态（成本按声明取、对客按声明
    /// 金额 × 倍率）发得出去。`false`：上游只回四分项 `usage`，金额由平台按费率自算，声明
    /// `upstream_declared` 的候选发布期就拒——否则受理时收不到金额，结算只能记成本缺口。
    pub declares_cost: bool,
    /// 这条通路的字节上限：调用方据此计算一次执行的内存预留。
    pub byte_limits: GatewayByteLimits,
}

#[derive(Debug, Clone, Error)]
#[error("provider call failed: {code}: {message}")]
pub struct ProviderCallError {
    pub code: String,
    pub message: String,
    /// 上游逐请求标识；入口按有界标识构造，非法原值在 Adapter 内部就被丢弃。
    pub trace_id: Option<ProviderTraceId>,
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
    /// 上游已受理、但平台没能把句柄入库：句柄随错误带回，调用方在收尾预算内再试保存；
    /// 它**不等于**上游未受理，不得据此重发生成请求（RFC 0017 §4）。
    #[error("provider accepted the request but the handle could not be persisted: {reason}")]
    AcceptedUnpersisted {
        handle: gateway::AcceptedHandle,
        reason: String,
    },
    /// 执行所有权失效或停机，且发生在**生成请求发出之前**：可证明这次执行没有提交生成，
    /// 占用与渠道名额可以按确定失败释放（RFC 0018 §4）。
    #[error("the execution was cancelled before the generation request was sent")]
    CancelledBeforeSend,
    /// 执行所有权失效或停机：**已经在等待上游结果的阶段**收到取消。
    ///
    /// 它不能证明上游未受理，只能按"可能已提交"有限收尾（RFC 0017 §5）。
    #[error("the execution was cancelled")]
    Cancelled,
    /// 这条通路不支持按句柄查询计量。
    #[error("this adapter cannot query accounting by handle")]
    QueryAccountingUnsupported,
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
mod tests;
