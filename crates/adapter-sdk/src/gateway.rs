//! 同步网关执行协议（RFC 0017 §2、§4）：内存输入、执行上下文与生命周期接口。

use crate::{
    AdapterError, GeneratedImage, ProviderCallError, ProviderCost, ProviderCredential,
    ProviderFailureKind, RetrySafety,
};
use async_trait::async_trait;
use seeai_domain::{ImageBranch, TokenUsage};
use serde_json::Value;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

/// 生成发送前的最后资格被拒的原因：两种取消事实各自指名，不合并成一个"取消了"。
///
/// 两种拒因都证明本地没有发出生成请求，差别在收尾权限：客户端离开时本进程仍是这笔执行的所有者，
/// 所有权失效时不是。两个事实同时成立时返回 [`Self::OwnershipLost`]——那一条更严
/// （见 [`DispatchGate::try_begin_external_action`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalActionRefused {
    /// 客户端已断开：停止新的上传、生成、重试与轮询。
    ClientGone,
    /// 执行所有权已失效（租约/续约失败、被接管）：已收到的事实只做有限收尾或交还当前所有者。
    OwnershipLost,
}

/// Provider 有界标识与 trace：权威定义在 [`seeai_domain`]，SDK 只重导出，不另立一套校验。
pub use seeai_domain::{ProviderTaskHandle, ProviderTraceId};

/// 一处输入图片：公网 http(s) 地址。平台不下载，原样交给上游（RFC 0017 §2）。
#[derive(Clone)]
pub struct InputImage(String);

impl InputImage {
    /// 由公网 http(s) 地址构造。
    pub fn url(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// 取值：公网 http(s) 地址。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Debug for InputImage {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        // 图片值不进日志或 Debug（RFC 0017 §4）：只打印长度。
        write!(formatter, "InputImage({} chars)", self.0.len())
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

/// 上游已受理的可信标识。不含上传地址：上传 URL 只用于当次内存执行（RFC 0017 §4）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedHandle {
    /// 任务式渠道的任务标识；只有任务式渠道会调用 `accepted`。类型保证它是有界标识。
    pub task_id: ProviderTaskHandle,
    /// 逐请求标识；非法值在 Adapter 入口就被丢弃，因此这里要么是可信标识，要么为空。
    pub trace_id: Option<ProviderTraceId>,
}

/// accepted 确认失败的原因：平台侧入库失败，或执行已被取消。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceptanceError {
    Persist(String),
    Cancelled,
}

/// Adapter 能看到的执行上下文：绝对期限、两种取消事实与异步接受确认。
///
/// 它不暴露 Repository、凭证或财务规则：Adapter 只看这几处（RFC 0017 §4）。
#[async_trait]
pub trait ExecutionContext: Send + Sync {
    /// 绝对总期限；上传、下载、submit、poll 都用它做单次超时上界。
    fn deadline(&self) -> Deadline;

    /// 客户端已断开：transport 观察到调用方离开（RFC 0018 §4.1）。
    fn client_gone(&self) -> bool;

    /// 执行所有权已失效：租约/续约失败或被接管。
    fn ownership_lost(&self) -> bool;

    /// 生成发送的最后资格检查：与两种取消事实共用同一原子状态，二者只有一个先成功。
    ///
    /// 返回 `Ok(())` 表示这次生成发送可以开始（此后即使收到取消也只能按"可能已提交"处理）；
    /// 返回 `Err(_)` 表示取消先发生，调用方绝不能发出生成请求。轮询、上传等非生成调用不调用它。
    fn try_begin_external_action(&self) -> Result<(), ExternalActionRefused>;

    /// 上游已受理：只有平台确认句柄入库后返回 Ok，之后才允许按句柄查询。
    async fn accepted(&self, handle: AcceptedHandle) -> Result<(), AcceptanceError>;
}

/// 取消与"生成发送已经开始"的**线性化**状态。
///
/// 三个事实放在同一个原子字里，逐位可查：`client_gone`（transport 观察到客户端断开）、
/// `ownership_lost`（租约/续约失败、接管）与 `generation_started`。取消与发送资格竞争同一个
/// compare-exchange，因此只有两种可观察结局——取消先赢（这次 Attempt 没有发出生成请求），
/// 或发送先赢（此后只能按"可能已提交"处理）。拆成两个独立标志会让两边各自"成功"，
/// 从而把可能已提交的执行误判成确定未提交（RFC 0018 §4.1）。
///
/// 两个取消事实同时成立时报价按 `ownership_lost`：那条更严——所有权都不在了，没有任何
/// 按旧 token 正式收尾的余地，只能有限收尾或交还当前所有者。位只增不减，所以"读到的原因"
/// 只会更严，不会把已经判成"不能正式收尾"的执行又放回旧 token 通道。
#[derive(Debug, Default)]
pub struct DispatchGate {
    state: AtomicU8,
}

const CLIENT_GONE: u8 = 0b001;
const OWNERSHIP_LOST: u8 = 0b010;
const GENERATION_STARTED: u8 = 0b100;
/// 任何原因都停止：两位一起置，用于停机这类"不区分原因、一律不许证明未提交"的场景。
const STOPPED: u8 = CLIENT_GONE | OWNERSHIP_LOST;

impl DispatchGate {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: AtomicU8::new(0),
        }
    }

    /// 记录"客户端已断开"；生成是否已经开始的事实不回退。
    pub fn client_gone(&self) {
        self.state.fetch_or(CLIENT_GONE, Ordering::SeqCst);
    }

    /// 记录"执行所有权已失效"。
    pub fn ownership_lost(&self) {
        self.state.fetch_or(OWNERSHIP_LOST, Ordering::SeqCst);
    }

    /// 无论什么原因，停止新的上传、生成、重试与轮询；同时放弃"本地未发送"的证明。
    ///
    /// 用于停机与接管这类既不是本进程还能证明的客户端断开、也不是一次可指名的续约失败的场景：
    /// 调用方不再声称自己没发过请求，让结论交给当前所有者。
    pub fn stop_all(&self) {
        self.state.fetch_or(STOPPED, Ordering::SeqCst);
    }

    /// 客户端是否已断开：用于停止后续上传、重试与轮询。
    #[must_use]
    pub fn is_client_gone(&self) -> bool {
        self.state.load(Ordering::SeqCst) & CLIENT_GONE != 0
    }

    /// 执行所有权是否已失效。
    #[must_use]
    pub fn is_ownership_lost(&self) -> bool {
        self.state.load(Ordering::SeqCst) & OWNERSHIP_LOST != 0
    }

    /// 任一取消事实成立：停止新的上传、生成、重试与轮询。
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.state.load(Ordering::SeqCst) & STOPPED != 0
    }

    /// 生成发送是否已经开始：开始之后即使取消，也只能按"可能已提交"收尾。
    #[must_use]
    pub fn generation_started(&self) -> bool {
        self.state.load(Ordering::SeqCst) & GENERATION_STARTED != 0
    }

    /// 已生效的取消原因；两个都成立时报 `ownership_lost`（更严的那一条）。
    #[must_use]
    pub fn stop_reason(&self) -> Option<ExternalActionRefused> {
        stop_reason_of(self.state.load(Ordering::SeqCst))
    }

    /// 生成发送前的最后资格：与两次取消线性化，二者只有一个先成功。
    ///
    /// 检查与标记不可分离：同一原子字上退回重试，取消在中间落下时这一轮必然看到并使用它。
    pub fn try_begin_external_action(&self) -> Result<(), ExternalActionRefused> {
        let mut current = self.state.load(Ordering::SeqCst);
        loop {
            if let Some(refused) = stop_reason_of(current) {
                return Err(refused);
            }
            match self.state.compare_exchange_weak(
                current,
                current | GENERATION_STARTED,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return Ok(()),
                Err(observed) => current = observed,
            }
        }
    }
}

/// 取一个已加载状态字里的取消原因；两个都成立时报 `ownership_lost`。
fn stop_reason_of(state: u8) -> Option<ExternalActionRefused> {
    if state & OWNERSHIP_LOST != 0 {
        Some(ExternalActionRefused::OwnershipLost)
    } else if state & CLIENT_GONE != 0 {
        Some(ExternalActionRefused::ClientGone)
    } else {
        None
    }
}

/// 调用之前的公共闸：任一取消事实成立按给定错误返回；总期限已到返回 `execution_deadline_exceeded`。
fn ensure_call_allowed(
    context: &dyn ExecutionContext,
    cancelled: AdapterError,
) -> Result<(), AdapterError> {
    if context.client_gone() || context.ownership_lost() {
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
///
/// 被拒时具体是哪一条取消事实由 [`DispatchGate::stop_reason`] 给出：调用方据此区分
/// "客户端离开、本进程仍持有所有权"与"所有权已失效、只能有限收尾"（RFC 0018 §4.1）。
pub fn begin_generation_send(context: &dyn ExecutionContext) -> Result<(), AdapterError> {
    context
        .try_begin_external_action()
        .map_err(|_| AdapterError::CancelledBeforeSend)
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
    /// 上游逐请求标识；Adapter 入口已按有界标识构造，非法原值不会到这里。
    pub provider_trace_id: Option<ProviderTraceId>,
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
