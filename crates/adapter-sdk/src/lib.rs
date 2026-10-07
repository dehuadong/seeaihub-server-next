use seeai_domain::{ImageBranch, ProviderCostSource, TokenUsage};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::{Debug, Formatter};
use thiserror::Error;

mod gateway;
pub use gateway::{
    AcceptanceError, AcceptedHandle, AccountingFacts, AccountingQuery, Deadline, DispatchGate,
    ExecutionContext, ExternalActionRefused, GatewayAdapter, GatewayInput, ImageSite, ImageSites,
    ImageValueShape, InputImage, ProviderOutput, ProviderTaskHandle, ProviderTaskState,
    ProviderTraceId, QueryAccountingCapability, ResponsePayload, begin_generation_send,
    ensure_external_call_allowed, ensure_read_call_allowed, external_call_timeout,
    gateway_passthrough_parameters, insert_wire_parameter,
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

/// 一次生成的一张图：**渠道给什么就是什么**——给地址就留地址、给 base64 就留 `b64_json`。
///
/// 形状本身保证"恰好其一"：要么一组地址、要么一个内联串，没有"两项都在"或"一项都没有"的表示法。
/// 一个 `Url` 项就是渠道的一张图，`urls` 是渠道为这张图给出的全部地址；`expires_at` 是渠道
/// 声明的过期时刻（Unix 秒），渠道不给就是 `None`，平台不推算。平台不下载、不解码、不归档。
///
/// 对客的 wire 形状不在这里：它是客户合同，由 `apps/api` 拥有；这个类型只承载渠道事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeneratedImage {
    /// 上游给的是公网地址（或它自己的临时链接）；`expires_at` 是渠道给的过期时刻（Unix 秒）。
    Url {
        urls: Vec<String>,
        expires_at: Option<i64>,
    },
    /// 上游给的是内联 base64。
    B64Json(String),
}

impl GeneratedImage {
    /// 上游给一个地址、没给过期时刻。
    #[must_use]
    pub fn from_url(url: String) -> Self {
        Self::Url {
            urls: vec![url],
            expires_at: None,
        }
    }

    /// 上游给一组地址与（可选的）过期时刻。
    #[must_use]
    pub fn from_urls(urls: Vec<String>, expires_at: Option<i64>) -> Self {
        Self::Url { urls, expires_at }
    }

    /// 上游给的是内联 base64。
    #[must_use]
    pub fn from_base64(b64_json: String) -> Self {
        Self::B64Json(b64_json)
    }
}

/// 是不是公网 http(s) 地址：生成入口的参考图与遮罩只允许这一种形态。
#[must_use]
pub fn is_http_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

/// 一次执行读到的上游事实：图片、用量、响应摘要、上游请求标识与成本。
///
/// 这是**解码结果**，不是落库形状：同步网关把它装成 [`ProviderOutput`]，平台据此写计量证据
/// 与账务；请求与响应载荷本身不落库。
#[derive(Debug, Clone)]
pub struct ProviderSuccess {
    pub images: Vec<GeneratedImage>,
    /// 有效计量证据。声明成本的渠道给不出 token 分项时为 `None`（设计 0022 §4）；
    /// 有 token 的渠道照旧给 `Some`。
    pub usage: Option<TokenUsage>,
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
///
/// 唯一的取值来源是 [`seeai_domain::SUPPORTED_REQUEST_WIRE_BYTES`]：请求结构的四条计数上限
/// （节点数、容器层数、对象字段数、累计字符串字节）就是从它推导的（RFC 0018 §2.2），两处不能各写
/// 一个数。
pub const GATEWAY_REQUEST_WIRE_BYTES: usize = seeai_domain::SUPPORTED_REQUEST_WIRE_BYTES;

/// 实测：贴住 [`GATEWAY_REQUEST_WIRE_BYTES`] 的**已支持输入形态**（一个贴满上限的大字符串
/// 参数）经 API 真实解析路径（[`seeai_domain::RequestParameters::parse`]）之后，解析结构在峰值时
/// 额外持有的字节数，不含 wire 本身。
///
/// 测量命令（Linux、dev profile、一条用例一个进程，`VmHWM` 前后差值）：
///
/// ```text
/// cargo test -p seeai-api --bin seeai-api -- --ignored --nocapture --test-threads=1 \
///   memory_request_parse_peak_image
/// ```
///
/// 实测（有界解析落地后）：wire 16777216 字节，额外峰值 17160 / 17224 KiB（两次）；上限仍是 18 MiB。
///
/// **节点密集输入的缺口已封堵**（T1）：解析入口现在边解析边计数（RFC 0018 §2.2），节点数、容器
/// 层数、对象字段数、累计字符串字节任一超限都在**构造过程中**失败，不会出现"wire 只有 16 MiB、
/// 解析结构却有 2 GiB"的形态。同一命令下重测两种节点密集形态（它们现在都在节点数上限处被拒）：
///
/// | 形态 | 封堵前峰值 | 封堵后峰值（本次实测） |
/// | --- | --- | --- |
/// | `[0,0,…]`（标量节点） | 526036 KiB | 1228 / 1356 / 1356 KiB（三次） |
/// | `[{"a":0},…]`（单字段对象节点） | 2065756 KiB | 1864–1992 KiB（四次） |
///
/// 这里钉的 18 MiB 仍然成立，因为它就是按这四条上限推导的：累计字符串 ≤ 16 MiB，节点数
/// 2048 × 1 KiB/节点（实测最贵一档 ≈ 1008 B/节点）= 2 MiB，合计 18 MiB。推导写在
/// `crates/domain/src/request_structure.rs` 的模块注释里，探针在 `apps/api/src/tests.rs`。
///
/// 剩余边界：累计字符串字节那条上限等于 wire 上限（解码后的字符串字节恒 ≤ wire 字节，再紧就会
/// 拒绝已支持的最大字符串参数），所以它在缺省配置下永远不会是先被撞到的一条——真正的收口
/// 是正文上限。转义密集的字符串在 `serde_json` 的内部暂存里可能短暂多占一份，其大小仍被同一
/// wire 上限约束（转义序列的解码产出 ≤ 其 wire 占用的一半），未单独实测。
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
/// 实测：额外峰值 16472–16540 KiB（四次；有界解析落地后复测一次 16604 KiB）；取 17 MiB 收口。
/// 其中约 16 MiB 是
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
    /// 金额 × 倍率）发得出去。`false`：金额由平台按该供给声明的计费形态自算，声明
    /// `upstream_declared` 的候选发布期就拒——否则受理时收不到金额，结算只能记成本缺口。
    pub declares_cost: bool,
    /// 这条通路的**成功件会不会带四分项 token 用量**。
    ///
    /// 对客选按 token 四档卖的候选要有它：拿不到用量就算不出该收多少钱。`false` 时那种候选
    /// 发布期就拒，不让每一笔请求都落到对账里。
    pub provides_token_usage: bool,
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

#[cfg(test)]
mod tests;

// ── 上游声明的金额：十进制 → 微单位（两个渠道共用一处换算）────────────────────────

/// 上游报出来的金额 → 微单位整数。
///
/// **换算**这一步不经过浮点：钱乘 1e6 会在边界上悄悄差 1 微单位，而这种差正是"成本对不上账"
/// 的来源。（JSON 数字本身由 `serde_json` 按双精度解出，那点误差要到十亿量级的金额才会碰到
/// 微单位，远超这类金额的实际范围。）
///
/// 除数字外还接受**字符串形态与指数写法**：这只是**容忍上游的表示差异**——同一家的响应形状
/// 会随版本变，把可读的金额读出来总好过凭空记一笔成本缺口。它**不是行为承诺**：上游没有承诺
/// 过用哪种写法，平台也不因此就"支持"了这些形态，读不出来照样按"没拿到"处理。
///
/// 负数是上游在说"这笔倒找钱"，平台没有可记的对应事实，按"没拿到"处理。
pub fn declared_microusd(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => parse_decimal_microusd(&number.to_string()),
        Value::String(text) => parse_decimal_microusd(text),
        Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_) => None,
    }
}

/// 十进制字面量 → 微单位（小数超过 6 位时四舍五入到第 6 位）。
///
/// 判不出确切金额的一律返回 `None`：非数字、负数、指数越界、超出 `u64` 范围。
/// 只有"上游明说这笔是 0"才得到 `0`——它和"没有金额"是两件事。
pub fn parse_decimal_microusd(text: &str) -> Option<u64> {
    let text = text.trim();
    let (mantissa, exponent) = match text.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.trim().parse::<i32>().ok()?),
        None => (text, 0),
    };
    let mantissa = mantissa.strip_prefix('+').unwrap_or(mantissa);
    if mantissa.starts_with('-') {
        return None;
    }
    let (integer, fraction) = match mantissa.split_once('.') {
        Some((integer, fraction)) => (integer, fraction),
        None => (mantissa, ""),
    };
    if integer.is_empty() && fraction.is_empty() {
        return None;
    }
    if !integer.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    // 数字串去掉小数点，再按 10 的幂移到微单位：金额 × 1e6 = 数字 × 10^(指数 − 小数位数 + 6)。
    let digits = format!("{integer}{fraction}");
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Some(0);
    }
    let digits = digits.parse::<u128>().ok()?;
    let shift = exponent - i32::try_from(fraction.len()).ok()? + 6;
    let scaled = if shift >= 0 {
        digits.checked_mul(10_u128.checked_pow(u32::try_from(shift).ok()?)?)?
    } else {
        let dropped = usize::try_from(-shift).ok()?;
        let divisor = 10_u128.checked_pow(u32::try_from(dropped).ok()?)?;
        let quotient = digits / divisor;
        // 四舍五入：余数到半个除数就进位。够不到半微单位时结果就是 0，不是"猜了一个数"。
        if (digits % divisor) * 2 >= divisor {
            quotient + 1
        } else {
            quotient
        }
    };
    u64::try_from(scaled).ok()
}
