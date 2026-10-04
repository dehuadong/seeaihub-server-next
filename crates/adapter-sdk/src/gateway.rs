//! 同步网关执行协议（RFC 0017 §2、§4）：内存输入、执行上下文与生命周期接口。

use crate::{
    AdapterError, DecodedImage, GeneratedImage, ProviderCallError, ProviderCost,
    ProviderCredential, ProviderFailureKind, RetrySafety, decode_data_url, is_http_url,
};
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use seeai_domain::{ImageBranch, TokenUsage, is_bounded_provider_identifier};
use serde_json::Value;
use std::borrow::Cow;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

/// 一处输入图片的形态：公网 URL、未解码的 data URL，或已解码的字节。
///
/// 文件部件优先保留 DecodedImage（字节 + 声明媒体类型），需要 data URL 的渠道在 wire
/// 序列化阶段编码一次，需上传或 multipart 的渠道直接借用字节（RFC 0017 §2）。
#[derive(Clone)]
pub enum InputImage {
    /// 公网 http(s) 地址：平台不下载，原样交给上游。
    Url(String),
    /// 未解码的 data URL。
    DataUrl(String),
    /// 已解码的字节与声明的媒体类型。
    Bytes(DecodedImage),
}

impl InputImage {
    /// 把调用方给的图片值分成三态：data URL、公网 URL，其余一律拒绝（平台不猜内容）。
    pub fn from_raw(value: String) -> Result<Self, AdapterError> {
        if value.starts_with("data:") {
            Ok(Self::DataUrl(value))
        } else if is_http_url(&value) {
            Ok(Self::Url(value))
        } else {
            Err(AdapterError::UnsupportedInput(
                "an input image must be a public http(s) url or a data url".to_owned(),
            ))
        }
    }

    /// 需要 data URL 的渠道用它取 wire 值：URL 与 data URL 借用原值，字节只编码一次。
    pub fn to_data_url(&self) -> Result<Cow<'_, str>, AdapterError> {
        match self {
            Self::Url(url) | Self::DataUrl(url) => Ok(Cow::Borrowed(url)),
            Self::Bytes(image) => Ok(Cow::Owned(format!(
                "data:{};base64,{}",
                image.media_type,
                STANDARD.encode(&image.bytes)
            ))),
        }
    }

    /// 需要字节的渠道用它取解码结果：data URL 就地解码，公网 URL 没有字节。
    pub fn decoded(&self) -> Result<Cow<'_, DecodedImage>, AdapterError> {
        match self {
            Self::Bytes(image) => Ok(Cow::Borrowed(image)),
            Self::DataUrl(value) => decode_data_url(value)
                .map(Cow::Owned)
                .map_err(AdapterError::UnsupportedInput),
            Self::Url(_) => Err(AdapterError::UnsupportedInput(
                "a public url carries no bytes; the adapter must fetch it".to_owned(),
            )),
        }
    }
}

impl Debug for InputImage {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        // 图片值不进日志或 Debug（RFC 0017 §4）：只打印形态与长度。
        match self {
            Self::Url(value) => write!(formatter, "InputImage::Url({} chars)", value.len()),
            Self::DataUrl(value) => write!(formatter, "InputImage::DataUrl({} chars)", value.len()),
            Self::Bytes(image) => write!(
                formatter,
                "InputImage::Bytes({} bytes, {})",
                image.bytes.len(),
                image.media_type
            ),
        }
    }
}

/// 候选承载面上某处图片参数在 wire 上是单值还是数组。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageValueShape {
    Scalar,
    Array,
}

/// 候选承载面上的一处图片参数位：参数名由受理期冻结，Driver 不改写。
#[derive(Debug, Clone)]
pub struct ImageSite {
    pub parameter: String,
    pub shape: ImageValueShape,
}

/// 参考图与遮罩各自的参数位；没有该图片位时为 None。
#[derive(Debug, Clone, Default)]
pub struct ImageSites {
    pub reference: Option<ImageSite>,
    pub mask: Option<ImageSite>,
}

impl ImageSites {
    /// 这个名字是不是本请求装载的图片参数位（参考图或遮罩）。
    #[must_use]
    pub fn carries(&self, name: &str) -> bool {
        [self.reference.as_ref(), self.mask.as_ref()]
            .into_iter()
            .flatten()
            .any(|site| site.parameter == name)
    }
}

/// 一次执行的内存输入：普通参数与图片分开，图片用强类型三态携带。
///
/// 平台装载的参考图与遮罩已提升到 reference_images 与 mask；调用方自己传的、名字恰好像图片的
/// 普通参数仍留在 native_parameters 里，归属只看 ImageSites 的参数名，不看取值形状（§2）。
#[derive(Clone)]
pub struct GatewayInput {
    pub provider_model_id: String,
    pub branch: ImageBranch,
    /// 普通模型参数；不含平台装载的图片值。
    pub native_parameters: Value,
    pub reference_images: Vec<InputImage>,
    pub mask: Option<InputImage>,
    pub image_sites: ImageSites,
    /// 这条渠道声明的成本币种（受理时冻结）。
    pub cost_currency: String,
}

impl Debug for GatewayInput {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        // 参数里可能有 prompt，图片里可能有字节：Debug 只给形状与数量。
        formatter
            .debug_struct("GatewayInput")
            .field("provider_model_id", &self.provider_model_id)
            .field("branch", &self.branch)
            .field("parameters", &"<redacted>")
            .field("reference_images", &self.reference_images.len())
            .field("mask", &self.mask.is_some())
            .field("image_sites", &self.image_sites)
            .field("cost_currency", &self.cost_currency)
            .finish()
    }
}

/// 逐字交给上游的普通参数位：跳过平台自己落的 `model`/`prompt`、空值，以及
/// [`GatewayInput::image_sites`] 里的图片参数名。
///
/// 归属只看参数名，不看取值形状：名字像图但不在参数位名单里，它仍是普通参数（RFC 0017 §2）。
#[must_use]
pub fn gateway_passthrough_parameters(input: &GatewayInput) -> Vec<(&String, &Value)> {
    let Value::Object(parameters) = &input.native_parameters else {
        return Vec::new();
    };
    parameters
        .iter()
        .filter(|(name, value)| {
            !matches!(name.as_str(), "model" | "prompt")
                && !value.is_null()
                && !input.image_sites.carries(name)
        })
        .collect()
}

/// 绝对总期限：包 tokio::time::Instant，暂停时钟的用例能控制它（RFC 0017 §8）。
#[derive(Debug, Clone, Copy)]
pub struct Deadline(tokio::time::Instant);

impl Deadline {
    /// 从当前时刻起算 budget。
    #[must_use]
    pub fn after(budget: Duration) -> Self {
        Self(tokio::time::Instant::now() + budget)
    }

    #[must_use]
    pub fn remaining(&self) -> Duration {
        self.0
            .saturating_duration_since(tokio::time::Instant::now())
    }

    #[must_use]
    pub fn is_expired(&self) -> bool {
        tokio::time::Instant::now() >= self.0
    }
}

/// Provider 任务句柄：**有界标识**，只能经 [`ProviderTaskHandle::parse`] 构造。
///
/// 它是对账的唯一入口，也是唯一会被拼进上游查询 URL 的值。字段私有，URL、data URL、控制字符与
/// 超长正文在这里就被挡下，不能靠"记得先校验"绕过去（Spec 0005 §2、RFC 0018 §6）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderTaskHandle(String);

impl ProviderTaskHandle {
    /// 按标识白名单构造：不是有界标识就拒绝，且不保留原值。
    pub fn parse(value: String) -> Result<Self, String> {
        if is_bounded_provider_identifier(&value) {
            Ok(Self(value))
        } else {
            Err("the provider task handle is not a bounded identifier".to_owned())
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

/// 上游已受理的可信标识。不含上传地址：上传 URL 只用于当次内存执行（RFC 0017 §4）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedHandle {
    /// 任务式渠道的任务标识；只有任务式渠道会调用 `accepted`。类型保证它是有界标识。
    pub task_id: ProviderTaskHandle,
    /// 逐请求标识。
    pub trace_id: Option<String>,
}

/// accepted 确认失败的原因：平台侧入库失败，或执行已被取消。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceptanceError {
    Persist(String),
    Cancelled,
}

/// Adapter 能看到的执行上下文：绝对期限、取消状态与异步接受确认。
///
/// 它不暴露 Repository、凭证或财务规则：Adapter 只看这三处（RFC 0017 §4）。
#[async_trait]
pub trait ExecutionContext: Send + Sync {
    /// 绝对总期限；上传、下载、submit、poll 都用它做单次超时上界。
    fn deadline(&self) -> Deadline;

    /// 执行所有权失效或停机：Adapter 应停止新的提交、重试与轮询。
    fn is_cancelled(&self) -> bool;

    /// 生成发送的最后资格检查：与取消共用同一状态，二者只有一个先成功。
    ///
    /// 返回 `true` 表示这次生成发送可以开始（此后即使取消也只能按"可能已提交"处理）；
    /// 返回 `false` 表示取消先发生，调用方绝不能发出生成请求。轮询、上传等非生成调用不需要
    /// 也不能调用它。
    fn try_begin_generation(&self) -> bool;

    /// 上游已受理：只有平台确认句柄入库后返回 Ok，之后才允许按句柄查询。
    async fn accepted(&self, handle: AcceptedHandle) -> Result<(), AcceptanceError>;
}

/// 取消与"生成发送已经开始"的**线性化**状态。
///
/// 两个事实放在同一个原子字里：[`Self::cancel`] 与 [`Self::try_begin_generation`] 竞争同一个
/// compare-exchange，因此只有两种可观察结局——取消先赢（这次 Attempt 没有发出生成请求），
/// 或发送先赢（此后只能按"可能已提交"处理）。拆成两个独立标志会让两边各自"成功"，
/// 从而把可能已提交的执行误判成确定未提交（RFC 0018 §4.1）。
#[derive(Debug, Default)]
pub struct DispatchGate {
    state: AtomicU8,
}

const CANCELLED: u8 = 0b01;
const GENERATION_STARTED: u8 = 0b10;

impl DispatchGate {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: AtomicU8::new(0),
        }
    }

    /// 取消：无论生成是否已经开始都记录取消；已经开始的事实不回退。
    pub fn cancel(&self) {
        self.state.fetch_or(CANCELLED, Ordering::SeqCst);
    }

    /// 取消是否已经发生：用于停止后续上传、重试与轮询。生成已经开始本身不算取消。
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.state.load(Ordering::SeqCst) & CANCELLED != 0
    }

    /// 生成发送是否已经开始：开始之后即使取消，也只能按"可能已提交"收尾。
    #[must_use]
    pub fn generation_started(&self) -> bool {
        self.state.load(Ordering::SeqCst) & GENERATION_STARTED != 0
    }

    /// 生成发送前的最后资格：与取消线性化，二者只有一个先成功。
    #[must_use]
    pub fn try_begin_generation(&self) -> bool {
        let mut current = self.state.load(Ordering::SeqCst);
        loop {
            if current & CANCELLED != 0 {
                return false;
            }
            match self.state.compare_exchange_weak(
                current,
                current | GENERATION_STARTED,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return true,
                Err(observed) => current = observed,
            }
        }
    }
}

/// 调用之前的公共闸：已取消按给定错误返回；总期限已到返回 `execution_deadline_exceeded`。
fn ensure_call_allowed(
    context: &dyn ExecutionContext,
    cancelled: AdapterError,
) -> Result<(), AdapterError> {
    if context.is_cancelled() {
        return Err(cancelled);
    }
    if context.deadline().is_expired() {
        return Err(AdapterError::Provider(ProviderCallError {
            code: "execution_deadline_exceeded".to_owned(),
            message: "the execution deadline passed before a new provider call".to_owned(),
            trace_id: None,
            retry_safety: RetrySafety::NotRetryable,
            kind: ProviderFailureKind::PlatformInternal,
            provider_cost: None,
        }));
    }
    Ok(())
}

/// 生成之前的外部调用（上传、下载素材、提交生成）之前的闸：取消返回
/// [`AdapterError::CancelledBeforeSend`]，因为此时生成请求确实还没有发出。
///
/// 生成请求自身的最后资格由 [`begin_generation_send`] 判定；**接受之后的只读轮询必须改用**
/// [`ensure_read_call_allowed`]。
pub fn ensure_external_call_allowed(context: &dyn ExecutionContext) -> Result<(), AdapterError> {
    ensure_call_allowed(context, AdapterError::CancelledBeforeSend)
}

/// 只读调用（按已知句柄轮询、查询任务）之前的闸：取消返回 [`AdapterError::Cancelled`]。
///
/// 这类调用可能发生在**上游已经受理之后**，取消不能证明上游未受理，因此绝不能报成
/// [`AdapterError::CancelledBeforeSend`]，否则调用方会把已提交的执行当成"可证明未发送"而释放占用。
pub fn ensure_read_call_allowed(context: &dyn ExecutionContext) -> Result<(), AdapterError> {
    ensure_call_allowed(context, AdapterError::Cancelled)
}

/// 生成类请求发送前的**最后**一道闸：与取消线性化，二者只有一个先成功。
///
/// 必须在真正 poll 发送 future 之前调用，且此后到发送之间不能再有可取消的等待：
/// - 返回 `Ok(())`：这次生成发送可以开始；此后即使收到取消，也只能按"可能已提交"处理。
/// - 返回 [`AdapterError::CancelledBeforeSend`]：取消先发生，**绝不能**发出生成请求，
///   调用方可以按"确定未提交"释放占用与渠道名额。
pub fn begin_generation_send(context: &dyn ExecutionContext) -> Result<(), AdapterError> {
    if context.try_begin_generation() {
        Ok(())
    } else {
        Err(AdapterError::CancelledBeforeSend)
    }
}

/// 单次外部调用的超时上界：`min(自身配置超时, 总期限剩余)`（RFC 0017 §6）。
#[must_use]
pub fn external_call_timeout(configured: Duration, context: &dyn ExecutionContext) -> Duration {
    configured.min(context.deadline().remaining())
}

/// Adapter 是否具备按已知句柄只读查询计量的能力。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryAccountingCapability {
    Supported,
    Unsupported,
}

/// 一次执行要交回对客响应的内存载荷；它不落库、不写日志。
#[derive(Clone)]
pub struct ResponsePayload {
    /// 上游给的 created（若有）；没有时由应用层兜底，不由 Adapter 造假值。
    pub created: Option<i64>,
    /// 每张图只保留上游给的 url 或 b64_json。
    pub images: Vec<GeneratedImage>,
}

impl Debug for ResponsePayload {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "ResponsePayload {{ created: {:?}, images: {} }}",
            self.created,
            self.images.len()
        )
    }
}

/// 有界强类型账务事实：计量计数、成本、产出张数、响应摘要与 Provider 标识。
///
/// Attempt 关联与 Provider 身份由应用层补齐——Adapter 不持有平台内部标识（RFC 0017 §2）。
#[derive(Debug, Clone)]
pub struct AccountingFacts {
    /// 有效计量证据；成功件必须为 Some（ADR 0006），对账查询拿不到时为 None。
    pub usage: Option<TokenUsage>,
    pub provider_cost: ProviderCost,
    pub image_count: u32,
    pub response_digest: String,
    pub provider_trace_id: Option<String>,
}

/// 一次执行的输出：内存载荷与账务事实分开（RFC 0017 §2）。
#[derive(Debug, Clone)]
pub struct ProviderOutput {
    pub response_payload: ResponsePayload,
    pub accounting_facts: AccountingFacts,
}

pub use seeai_domain::ProviderTaskState;

/// 按已知句柄只读查询的结果：渠道状态，以及可得时的账务事实。
///
/// `accounting_facts` 与状态相互独立：失败的任务也可能带回用量与成本，收尾按 `state` 决定，
/// 不能由"有没有事实"反推成功。
#[derive(Debug, Clone)]
pub struct AccountingQuery {
    pub state: ProviderTaskState,
    pub accounting_facts: Option<AccountingFacts>,
}

/// 同步网关的 Adapter 生命周期接口（RFC 0017 §4）。
///
/// 事务边界、所有权与财务规则不在 Adapter：它只做传输、接受阶段、查询能力、归一与计量提取。
#[async_trait]
pub trait GatewayAdapter: Send + Sync {
    fn key(&self) -> &'static str;

    /// 这条通路能不能按已知句柄只读查询计量。
    fn query_accounting_capability(&self) -> QueryAccountingCapability;

    /// 执行一次生成；context 提供期限、取消与接受确认。
    async fn execute(
        &self,
        input: Arc<GatewayInput>,
        context: &dyn ExecutionContext,
        credential: &ProviderCredential,
    ) -> Result<ProviderOutput, AdapterError>;

    /// 只读查询同一任务的计量；默认声明不支持（同步通路）。
    ///
    /// cost_currency 是受理时冻结的渠道成本币种：上游金额本身不带币种，Adapter 不能假定 USD，
    /// 只能把冻结的那份声明带回来（RFC 0017 §2、§4）。
    async fn query_accounting(
        &self,
        _handle: &AcceptedHandle,
        _cost_currency: &str,
        _deadline: Deadline,
        _credential: &ProviderCredential,
    ) -> Result<AccountingQuery, AdapterError> {
        Err(AdapterError::QueryAccountingUnsupported)
    }
}

#[cfg(test)]
mod tests;
