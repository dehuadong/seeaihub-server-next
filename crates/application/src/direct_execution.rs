//! 应用层直接执行用例：API 在内存里完成选路、提交、上游执行与结算（RFC 0017 §1、§4、§6）。
//!
//! 这个用例替代"建 Job → Worker 领取 → 轮询结果"的旧流水线：业务载荷（参数、参考图、mask、
//! 结果图片）只在本进程内存里存在，持久化的只有最小执行与账务事实。它做四件事：
//!
//! 1. 按合同过滤参数、算请求指纹与幂等摘要；
//! 2. 复用既有选路、承载准备与定价冻结，组装不含图片值的 [`GatewayInput`]；
//! 3. 原子受理 → 先持久化提交声明（[`ExecutionRepository::begin_submission`]）再发外部请求；
//! 4. 成功先结算再返回载荷；失败按 [`RetrySafety`] 映射成处置落库，未知一律转对账、绝不重提。
//!
//! 凭证由 [`CredentialProvider`] 从环境解析，只交给 Adapter；不落库、不日志。
//! 期限与取消归调用方：总期限预算 `D − R`，`D` 是 `GENERATION_SYNC_WAIT_SECONDS`，
//! `R` 是 `GENERATION_SETTLE_RESERVE_SECONDS`（默认 10s），由
//! [`SupervisedExecutionContext`] 交给 Adapter。

use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use seeai_adapter_sdk::{
    AcceptanceError, AcceptedHandle, AdapterError, Deadline, DispatchGate, ExecutionContext,
    GatewayInput, ImageSite, ImageSites, ImageValueShape, InputImage, ProviderFailureKind,
    ProviderOutput, ProviderTaskState, ResponsePayload, RetrySafety,
};
use seeai_domain::{
    AccountId, AttemptId, ChargeFacts, ExecutionStage, FencingToken, ImageBranch,
    ImageParameterKind, JobId, MeteringEvidence, OfferingCandidate, ProviderCostFact,
    ProviderTaskHandle, ProviderTraceId, PublishedOffering, ReceiptCredential, RequestParameters,
    RouteStrategy, image_parameter_kind, platform_image_parameters,
};
use serde_json::{Map, Value};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

use crate::{
    AccelerationService, AdapterFactory, AdmitExecution, AdmitOffering, AdmitOutcome,
    ApplicationError, BalanceSource, BeginSubmission, CostInputs, CreateImageGenerationRequest,
    CredentialProvider, DirectExecutionLimits, ExecutionFinalization, ExecutionLookup,
    ExecutionReplay, ExecutionRepository, FailOrReconcileExecution, FailureDisposition,
    GenerationDailySpendLimit, HubRepository, LateFacts, PublicErrorCode, RequestCostCeiling,
    RequestFingerprintInput, RequestFingerprintKeys, RequestTimeoutPolicy, RetryPolicy,
    RouteChoice, RoutingDecision, SettleExecution, contract_parameter_face,
    daily_spend_limit_error, failure_provider_cost, freeze_offering_pricing,
    idempotency_key_digest, provider_cost_fact, public_error_code, requested_image_count,
    select_candidate, select_candidate_with_strategy, single_request_cost_cny,
    validate_idempotency_key,
};

/// 直接执行总期限里预留给证据持久化、结算与提交确认的默认预算（秒）。
pub const DEFAULT_SETTLE_RESERVE_SECONDS: u64 = 10;

/// 执行所有权租约的默认时长（秒）：`begin_submission` 按它落 `lease_expires_at`，
/// 独立续约任务按它的三分之一周期续约。部署用 `GENERATION_EXECUTION_LEASE_SECONDS` 覆盖。
pub const DEFAULT_EXECUTION_LEASE_SECONDS: i64 = 60;

/// 从 `GENERATION_SETTLE_RESERVE_SECONDS` 读结算预留预算（秒，默认
/// [`DEFAULT_SETTLE_RESERVE_SECONDS`]）。它必须是整数：读不懂的配置宁可让进程起不来，
/// 也不要拿一个猜出来的值去卡总期限。
pub fn settle_reserve_from_env() -> Result<Duration, ApplicationError> {
    match std::env::var("GENERATION_SETTLE_RESERVE_SECONDS") {
        Ok(value) if !value.trim().is_empty() => value
            .trim()
            .parse::<u64>()
            .map(Duration::from_secs)
            .map_err(|_| {
                ApplicationError::Configuration(
                    "GENERATION_SETTLE_RESERVE_SECONDS must be an integer number of seconds"
                        .to_owned(),
                )
            }),
        _ => Ok(Duration::from_secs(DEFAULT_SETTLE_RESERVE_SECONDS)),
    }
}

/// 已经解析好的一次同步图片请求。
///
/// 图片是**三态**（公网 URL / data URL / 上传字节），由接口层解析 multipart 与 JSON 后给出；
/// 本用例只把它们交给适配器，不落盘、不写日志。`endpoint` 进请求指纹：JSON 与 multipart
/// 两个端点不承诺共享指纹。
pub struct DirectExecutionRequest {
    pub account_id: AccountId,
    /// 对外的平台型号名（网关模型）。
    pub model: String,
    /// 请求指纹里的端点标识（例如 `/v1/images/generations`）。
    pub endpoint: String,
    /// 调用方的普通参数（已经摘掉 `model` 与图片字段）。
    ///
    /// 类型是 [`RequestParameters`]：只有**计过数**的请求参数面才进得来（RFC 0018 §2.2）。受理路径
    /// 因此不可能拿到一份"先建好再统计"的无界 `Value`；wire 入口的有界解析归接口层。
    pub native_parameters: RequestParameters,
    pub reference_images: Vec<InputImage>,
    pub mask: Option<InputImage>,
    pub idempotency_key: String,
}

/// 执行所有权的登记出口：直接执行用例在**首次提交声明落库后**把 `(job_id, fencing_token)` 交给调用方。
///
/// 调用方（API Supervisor）据此起独立续约任务；不提供实现时用例行为与从前逐位相同。
/// 登记时机在提交声明之后：Job 那时才进 executing 并带上租约，续约才有可延期的所有权。
pub trait ExecutionOwnershipRegistrar: Send + Sync {
    /// 登记本次执行的所有权。同一 Job 的一次执行只登记一次。
    fn registered(&self, job_id: JobId, fencing_token: FencingToken);
}

/// 调用方（API Supervisor）提供的执行身份与取消/发送闸。
///
/// 取消由调用方持有并可随时置位：Adapter 在每次新的外部调用前读它，停止提交、重试与轮询。
/// 生成发送的开始与取消竞争同一个原子状态，因此二者只有一个先成功：取消先赢时可证明这次
/// Attempt 没有发出生成请求，发送先赢时只能按"可能已提交"收尾（RFC 0018 §4）。
pub struct DirectExecutionCall {
    pub execution_owner: String,
    pub gate: Arc<DispatchGate>,
    /// 本次请求**收到头部时刻**起算的绝对总期限 `D`。
    ///
    /// 上层预算 `D − R` 由这里算，不许从"进入 handler"或"开始执行"重新起算：认证、读取准入与
    /// 慢读都算在同一个 `D` 里（RFC 0017 §6）。
    pub total_deadline: tokio::time::Instant,
    /// 所有权登记出口；`None` 表示这次执行不续约（测试与旧调用方）。
    pub ownership: Option<Arc<dyn ExecutionOwnershipRegistrar>>,
}

/// 一次直接执行的成功结果：`payload` 只在本进程内存里，不落库。
pub struct DirectExecutionSuccess {
    pub job_id: JobId,
    pub attempt_id: AttemptId,
    pub payload: ResponsePayload,
}

/// 一次收尾里**已经取得、可能来不及在当前所有权下正式结算**的账务事实。
///
/// 收尾确认失败（token Conflict 或提交结果不明）时用它构造 [`LateFacts`] 交给收件端口；它只含
/// Spec 0005 §2 允许的最小事实，不含图片、响应正文或请求参数（RFC 0018 §5.1）。
#[derive(Debug, Clone, Default)]
struct LateFactsInput {
    /// 上游终态快照；平台内部失败（根本没交到渠道）留 `None`。
    provider_state: Option<ProviderTaskState>,
    /// 上游任务句柄；同步渠道通常没有。
    provider_task_handle: Option<ProviderTaskHandle>,
    /// 上游实际产出的图片张数。
    image_count: Option<u32>,
    /// 计量证据（自带 Attempt 关联）。
    evidence: Option<MeteringEvidence>,
}

impl LateFactsInput {
    /// 按处置给出如实的上游终态：确定失败记 `Failed`，结果不明记 `Unknown`，未交到渠道留空。
    fn for_disposition(disposition: FailureDisposition) -> Self {
        let provider_state = match disposition {
            FailureDisposition::DeterminedFailure => Some(ProviderTaskState::Failed),
            FailureDisposition::Unknown => Some(ProviderTaskState::Unknown),
            FailureDisposition::SafeRetry => None,
        };
        Self {
            provider_state,
            ..Self::default()
        }
    }
}

/// 直接执行用例的处置结果。
///
/// 受理前的参数/资金/容量/期限错误按既有 [`ApplicationError`] 原样带出；同键重放的四种投影
/// 与"结果未知"各有独立变体，见 Spec 0005 §4。
#[derive(Debug, Error)]
pub enum DirectExecutionError {
    /// 同键原记录仍在执行：409 request_in_progress，不重复占用、不执行第二次。
    #[error("the original request with this idempotency key is still executing")]
    RequestInProgress { retry_after: Duration },
    /// 同键原记录已成功结算但结果不保留：409 result_not_retained。
    #[error("the original request completed and its result is not retained")]
    ResultNotRetained,
    /// 同键原记录已确定失败：返回原平台错误码与 HTTP 状态。
    #[error("the original request failed: {}", .code.as_str())]
    OriginalFailure { code: PublicErrorCode },
    /// 受理、结果或结算仍不确定：502 outcome_unknown，保留原处置与占用。
    #[error("the outcome of this request is unknown; it is retained for reconciliation")]
    OutcomeUnknown,
    /// 总期限到达时已确认上游未受理、占用与容量已释放：504 request_timeout（Spec 0005 §4）。
    #[error("the request timed out before the provider was asked to generate")]
    RequestTimeout,
    /// 已确认成功结算，但图片来不及在总期限内准备好返回：504 result_delivery_timeout。
    ///
    /// 这是"原请求已完成并收费、结果不保留"，不能改写成"结果未知"（Spec 0005 §4）。
    #[error(
        "the original request completed and was charged, but the result was not delivered in time"
    )]
    ResultDeliveryTimeout,
    /// 应用层的其他拒绝：参数、资金、容量、期限等。
    #[error(transparent)]
    Application(#[from] ApplicationError),
}

/// [`RetrySafety`] → [`FailureDisposition`] 的**唯一**映射。
///
/// 只有 Adapter 能证明上游未受理的失败才允许中间安全重试；确定性拒绝是终局失败；受理状态
/// 不明一律转对账、保留占用与渠道名额。缺少可证明未受理依据时绝不重提（Spec 0005 §5）。
#[must_use]
pub fn failure_disposition_for(retry_safety: RetrySafety) -> FailureDisposition {
    match retry_safety {
        RetrySafety::SafeBeforeAcceptance => FailureDisposition::SafeRetry,
        RetrySafety::NotRetryable => FailureDisposition::DeterminedFailure,
        RetrySafety::AcceptanceUnknown => FailureDisposition::Unknown,
    }
}

/// Adapter 能看到的执行上下文：绝对期限、取消/发送闸与异步接受确认。
pub struct SupervisedExecutionContext {
    executions: Arc<dyn ExecutionRepository>,
    job_id: JobId,
    attempt_id: AttemptId,
    execution_owner: String,
    fencing_token: FencingToken,
    /// 本 Attempt 的收件凭据原值：执行期间只在内存里，收尾交接时用它投递晚到事实。
    receipt_credential: ReceiptCredential,
    deadline: Deadline,
    gate: Arc<DispatchGate>,
}

impl SupervisedExecutionContext {
    /// 组装一次执行的上下文；`deadline` 是已经算好的绝对期限（`D − R`）。
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        executions: Arc<dyn ExecutionRepository>,
        job_id: JobId,
        attempt_id: AttemptId,
        execution_owner: String,
        fencing_token: FencingToken,
        receipt_credential: ReceiptCredential,
        deadline: Deadline,
        gate: Arc<DispatchGate>,
    ) -> Self {
        Self {
            executions,
            job_id,
            attempt_id,
            execution_owner,
            fencing_token,
            receipt_credential,
            deadline,
            gate,
        }
    }

    /// 这次 Attempt 的收件凭据原值；只交给收件端口，不写日志、不入库。
    #[must_use]
    pub fn receipt_credential(&self) -> &ReceiptCredential {
        &self.receipt_credential
    }

    /// 这次 Attempt 的生成发送是否已经开始：用于区分"闸口拒绝"与"发送已开始后才收到取消"。
    #[must_use]
    pub fn generation_started(&self) -> bool {
        self.gate.generation_started()
    }
}

#[async_trait]
impl ExecutionContext for SupervisedExecutionContext {
    fn deadline(&self) -> Deadline {
        self.deadline
    }

    fn is_cancelled(&self) -> bool {
        self.gate.is_cancelled()
    }

    fn try_begin_generation(&self) -> bool {
        self.gate.try_begin_generation()
    }

    async fn accepted(&self, handle: AcceptedHandle) -> Result<(), AcceptanceError> {
        // 取消只停止新的外部副作用，不阻止已经到达的句柄入库：句柄丢掉就再也对不了账，
        // 而记录它不产生新的渠道调用（RFC 0018 §4.2）。落库后 Adapter 会去看取消状态，
        // 已取消时立刻停止轮询。
        self.executions
            .record_acceptance(crate::RecordAcceptance {
                job_id: self.job_id,
                attempt_id: self.attempt_id,
                execution_owner: self.execution_owner.clone(),
                fencing_token: self.fencing_token,
                provider_task_handle: Some(handle.task_id),
                provider_trace_id: handle.trace_id,
            })
            .await
            .map_err(|error| AcceptanceError::Persist(error.to_string()))
    }
}

/// 应用层直接执行用例。
pub struct DirectExecutionService {
    repository: Arc<dyn HubRepository>,
    executions: Arc<dyn ExecutionRepository>,
    adapters: Arc<dyn AdapterFactory>,
    credentials: Arc<dyn CredentialProvider>,
    keys: RequestFingerprintKeys,
    timeouts: RequestTimeoutPolicy,
    limits: DirectExecutionLimits,
    /// 总期限 `D` 里留给结算与提交确认的预算 `R`。
    settle_reserve: Duration,
    /// 本次执行所有权的租约时长：随 BeginSubmission 落库，并与 Supervisor 的续约间隔同源。
    ownership_lease: ChronoDuration,
    cost_ceiling: RequestCostCeiling,
    /// 可证明上游未受理时的请求内重投策略（次数与退避），与旧路径共用同一组配置。
    retry_policy: RetryPolicy,
    /// 该账户**当天**最多能花掉多少（microusd，运营取值，见 [`GenerationDailySpendLimit`]）。
    ///
    /// 它与两个容量名额守的不是同一件事：名额守的是"同时在跑几个"，钱烧光的形态却是**串行**的
    /// ——一个接一个地跑、每一个都合规，照样能在一天里把余额花完。判据见
    /// [`HubRepository::daily_spend_microusd`]：问的是当日已完成实收的合计，不是任何计数器。
    max_daily_spend_microusd: u64,
    /// 加速层：受理、结算与失败收尾都改余额，提交后要把新余额写穿缓存。
    acceleration: Arc<AccelerationService>,
}

impl DirectExecutionService {
    /// 装配用例。`timeouts` 提供总期限 `D`（`GENERATION_SYNC_WAIT_SECONDS`）与本次上游
    /// 超时；`limits` 提供两个容量名额与平台兜底保底额。
    #[must_use]
    pub fn new(
        repository: Arc<dyn HubRepository>,
        executions: Arc<dyn ExecutionRepository>,
        adapters: Arc<dyn AdapterFactory>,
        credentials: Arc<dyn CredentialProvider>,
        keys: RequestFingerprintKeys,
        timeouts: RequestTimeoutPolicy,
        limits: DirectExecutionLimits,
    ) -> Self {
        let acceleration = Arc::new(AccelerationService::disabled(repository.clone()));
        Self {
            repository,
            executions,
            adapters,
            credentials,
            keys,
            timeouts,
            limits,
            settle_reserve: Duration::from_secs(DEFAULT_SETTLE_RESERVE_SECONDS),
            ownership_lease: ChronoDuration::seconds(DEFAULT_EXECUTION_LEASE_SECONDS),
            cost_ceiling: RequestCostCeiling::default_ceiling(),
            // 缺省就是开着的请求内重投（次数有限、退避有上限）：关掉要显式配
            // GENERATION_RETRY_MAX_ATTEMPTS=1，与旧路径同一条纪律。
            retry_policy: RetryPolicy::default(),
            max_daily_spend_microusd: GenerationDailySpendLimit::default_limit()
                .max_daily_spend_microusd,
            acceleration,
        }
    }

    /// 装上运维给的**每日扣费上限**。
    ///
    /// 上限是配置项：它随部署形态与客户分级变，所以由调用方给，而不是写死在这里。
    #[must_use]
    pub fn with_daily_spend_limit(mut self, limit: GenerationDailySpendLimit) -> Self {
        self.max_daily_spend_microusd = limit.max_daily_spend_microusd;
        self
    }

    /// 装上运维给的结算预留预算 `R`。
    #[must_use]
    pub fn with_settle_reserve(mut self, settle_reserve: Duration) -> Self {
        self.settle_reserve = settle_reserve;
        self
    }

    /// 装上执行所有权租约时长：`begin_submission` 按它落 `lease_expires_at`，续约按它的三分之一周期。
    #[must_use]
    pub fn with_ownership_lease(mut self, lease: ChronoDuration) -> Self {
        self.ownership_lease = lease;
        self
    }

    /// 装上运维给的单次请求成本上限。
    #[must_use]
    pub fn with_cost_ceiling(mut self, cost_ceiling: RequestCostCeiling) -> Self {
        self.cost_ceiling = cost_ceiling;
        self
    }

    /// 装上请求内安全重投策略（次数与退避基）。
    ///
    /// 它只在 Adapter 报 `SafeBeforeAcceptance` 时生效：同一候选、同一冻结输入、同一请求预算内，
    /// 记下本次 Attempt 后换新 Attempt 重试；次数上限与退避都沿用旧路径那一组运维取值。
    #[must_use]
    pub fn with_retry_policy(mut self, retry_policy: RetryPolicy) -> Self {
        self.retry_policy = retry_policy;
        self
    }

    /// 装上加速层：受理与收尾提交后把余额写穿缓存（RFC 0017 §3）。
    #[must_use]
    pub fn with_acceleration(mut self, acceleration: Arc<AccelerationService>) -> Self {
        self.acceleration = acceleration;
        self
    }

    /// 执行一次同步图片请求。
    ///
    /// 顺序是固定的：指纹与摘要 → 选路/承载/冻价 → admit → 提交声明 → 外部执行 → 结算/处置。
    /// 成功时结算已提交才返回载荷；失败时按 [`RetrySafety`] 落处置，未知转对账并返回
    /// [`DirectExecutionError::OutcomeUnknown`]。
    pub async fn execute(
        &self,
        request: DirectExecutionRequest,
        call: &DirectExecutionCall,
    ) -> Result<DirectExecutionSuccess, DirectExecutionError> {
        validate_idempotency_key(&request.idempotency_key)?;
        if self.limits.default_hold_microusd == 0 {
            return Err(ApplicationError::Configuration(
                "the platform default hold must be positive".to_owned(),
            )
            .into());
        }
        let routing_input = routing_request(&request)?;
        let branch = routing_input.branch()?;
        let lookup_digest = idempotency_key_digest(&request.idempotency_key);
        // 同键预查在选路、候选截断与冻价之前：原记录一旦存在，型号下架、候选停用或选路失败
        // 都不能夺走它的 §4 重放投影（Spec 0005 §4）。命中后按记录冻结的合同与密钥版本比对，
        // 一致才投影；不一致或无法安全比对按 idempotency_conflict 拒绝，绝不新建。
        if let Some(lookup) = self
            .executions
            .lookup_execution(request.account_id, &lookup_digest)
            .await?
        {
            return Err(self.replay_projection(&request.endpoint, &routing_input, lookup)?);
        }
        // 合同是模型级唯一一份：先读候选取合同，按它过滤出"已识别的参数"，指纹在选路与候选
        // 截断之前形成，且不依赖当前价格、候选或修订（Spec 0005 §4）。
        let candidates = self.repository.active_offering(&request.model).await?;
        if candidates.is_empty() {
            return Err(ApplicationError::NotFound(format!(
                "no active offering for model {}",
                request.model
            ))
            .into());
        }
        let contract_parameters = Value::Object(contract_parameter_face(
            &routing_input,
            &candidates[0].capability_schema,
        )?);
        let request_digest = self
            .keys
            .request_fingerprint(
                self.keys.current_request_key_version(),
                &RequestFingerprintInput {
                    endpoint: &request.endpoint,
                    gateway_model: &request.model,
                    parameters: &contract_parameters,
                    reference_images: &routing_input.reference_images,
                    mask: routing_input.mask.as_deref(),
                    n: requested_image_count(&contract_parameters),
                },
            )?
            .ok_or_else(|| {
                ApplicationError::Configuration(
                    "the current request fingerprint key is not configured".to_owned(),
                )
            })?;

        // 选路、承载准备与冻价都复用旧路径的同一组函数：同一条请求、同一个账户与幂等键，两条路
        // 选出同一条候选、冻出同一份快照。
        let (mut offering, native_parameters, routing) =
            self.select(&routing_input, branch, &candidates).await?;
        let hold_microusd = freeze_offering_pricing(
            self.repository.as_ref(),
            self.limits.default_hold_microusd,
            request.native_parameters.as_value(),
            &mut offering,
        )
        .await?;
        if let Some(cost_cny) = single_request_cost_cny(
            offering.price_snapshot.formula,
            offering.price_snapshot.cost_unit_price_microusd,
            offering.price_snapshot.reference_cost_microusd,
            offering.price_snapshot.cost_currency.as_deref(),
            offering.price_snapshot.fx_rate.as_ref(),
            requested_image_count(&native_parameters),
        ) && self.cost_ceiling.exceeded_by(cost_cny)
        {
            return Err(ApplicationError::RequestCostCeilingExceeded(format!(
                "offering {} could cost up to {cost_cny} microusd of upstream cost for this request",
                offering.offering_id
            ))
            .into());
        }
        let input = Arc::new(build_gateway_input(
            &offering,
            branch,
            native_parameters,
            &request,
        )?);

        // 收到头部起算的总期限在受理前已经走完：不建任何记录，按"确定未提交"回应（Spec 0005 §4）。
        if call
            .total_deadline
            .saturating_duration_since(tokio::time::Instant::now())
            .is_zero()
        {
            return Err(DirectExecutionError::RequestTimeout);
        }
        // 受理之前就已经取消（停机、所有权失效，或调用方已经离开）：这次执行还没有任何记录、
        // 也没有发出过请求，直接按"确定未提交"回应，不建 Job、不占 Hold、不占渠道名额。
        // 这也让同键重试能作为一次正常的新请求处理，而不是命中一条被取消写死的失败记录。
        if call.gate.is_cancelled() {
            return Err(DirectExecutionError::RequestTimeout);
        }
        // 每日扣费上限：与两个容量名额是**三道不同的门**——账户名额守"同时在跑几个"、渠道名额守
        // "上游未决任务有几个"，这道守的是"今天已经花掉多少钱"（花钱可以是完全串行的，两个计数
        // 都看不见它）。判据见 [`HubRepository::daily_spend_microusd`]，超限按既有的 429 语义回。
        //
        // 它**放在 admit 事务之外**，理由是这道门与那笔扣减本来就不可能原子：当日合计只在**结算**
        // 那一笔里累加，而结算发生在受理之后很久，任何事务边界都圈不住"受理到结算"这段窗口；
        // 事务内再读一次不会让判定更准，只会把"到次日零点还有多久"这条对客事实（由
        // [`daily_spend_limit_error`] 按唯一一处规则算出）搬进 SQL 再写一遍。读的仍是已提交的
        // 权威事实，位置取在**受理之前**：不建 Job、不占 Hold、不占渠道名额。
        let spent_microusd = self
            .repository
            .daily_spend_microusd(request.account_id)
            .await?;
        if let Some(rejected) =
            daily_spend_limit_error(self.max_daily_spend_microusd, spent_microusd, Utc::now())
        {
            return Err(rejected.into());
        }

        let outcome = self
            .executions
            .admit(AdmitExecution {
                account_id: request.account_id,
                branch,
                offering: admit_offering(&offering),
                price_snapshot: offering.price_snapshot.clone(),
                routing,
                idempotency_key_digest: lookup_digest,
                request_digest,
                request_digest_key_version: self.keys.current_request_key_version(),
                max_cost_microusd: hold_microusd,
                max_account_in_flight: self.limits.max_account_in_flight,
                max_channel_in_flight: self.limits.max_channel_in_flight,
            })
            .await?;
        let (job_id, fencing_token) = match outcome {
            AdmitOutcome::Admitted { job, balance } => {
                // 预授权扣减已经提交：把变更后的余额写进缓存（RFC 0017 §3）。
                self.acceleration
                    .write_balance(&balance, BalanceSource::DbCommit)
                    .await;
                (job.job_id, job.fencing_token)
            }
            AdmitOutcome::Replayed(replay) => {
                return Err(project_replay(replay, self.timeouts.sync_wait));
            }
        };

        // 期限从收到请求头起算：上游预算 D 减 R，收尾另用 R。两个绝对时刻都在循环外算定，重试
        // 不许各自延长；数据库侧的提交期限也认这一个值。预算已经耗尽时不发任何外部调用，直接
        // 按"确定未提交"回应（Spec 0005 §4）。
        let remaining = call
            .total_deadline
            .saturating_duration_since(tokio::time::Instant::now());
        let budget = remaining
            .saturating_sub(self.settle_reserve)
            .min(self.timeouts.sync_wait.saturating_sub(self.settle_reserve));
        let deadline = Deadline::after(budget);
        let submission_deadline = db_deadline(remaining.min(self.timeouts.sync_wait));
        let provider_timeout = self
            .timeouts
            .upstream_timeout_for(requested_image_count(request.native_parameters.as_value()));
        let adapter = self.adapters.create_gateway(
            &offering.adapter_key,
            &offering.base_url,
            provider_timeout,
        )?;
        let credential = self.credentials.resolve(&offering.credential_env)?;

        // 可证明未受理的失败在同一请求内重投同一候选：每次先落 Attempt 再换新 Attempt，占用与
        // 渠道名额原样保留；只有终局确定失败、期限截止或次数耗尽才释放（Spec 0005 §5）。
        let mut ownership_registered = false;
        loop {
            let started = self
                .executions
                .begin_submission(BeginSubmission {
                    job_id,
                    execution_owner: call.execution_owner.clone(),
                    fencing_token,
                    deadline: submission_deadline,
                    lease: self.ownership_lease,
                })
                .await?;
            let attempt_id = started.attempt_id;
            // 提交声明已落库：Job 这才进 executing 并带上租约，此刻把所有权交给调用方起续约。
            if !ownership_registered {
                if let Some(registrar) = &call.ownership {
                    registrar.registered(job_id, fencing_token);
                }
                ownership_registered = true;
            }
            // 提交声明已落、但外部预算已到：这一次不发任何外部调用，直接释放，按 504 request_timeout。
            if deadline.is_expired() {
                self.record_failure(
                    request.account_id,
                    job_id,
                    attempt_id,
                    &call.execution_owner,
                    fencing_token,
                    &started.receipt_credential,
                    ProviderFailureKind::PlatformInternal,
                    FailureDisposition::DeterminedFailure,
                    None,
                    None,
                    LateFactsInput::default(),
                )
                .await?;
                return Err(DirectExecutionError::RequestTimeout);
            }
            let context = SupervisedExecutionContext::new(
                self.executions.clone(),
                job_id,
                attempt_id,
                call.execution_owner.clone(),
                fencing_token,
                started.receipt_credential.clone(),
                deadline,
                call.gate.clone(),
            );
            let output = adapter.execute(input.clone(), &context, &credential).await;
            match output {
                Ok(output) => {
                    return self
                        .finish_success(
                            request.account_id,
                            job_id,
                            attempt_id,
                            &call.execution_owner,
                            fencing_token,
                            &started.receipt_credential,
                            &offering,
                            output,
                            call.total_deadline,
                        )
                        .await;
                }
                Err(AdapterError::Provider(provider)) => {
                    let disposition = failure_disposition_for(provider.retry_safety);
                    let error_code = public_error_code(provider.kind, provider.retry_safety);
                    let provider_cost = failure_provider_cost(
                        &offering.price_snapshot,
                        provider.provider_cost.as_ref(),
                    );
                    if disposition == FailureDisposition::SafeRetry {
                        let backoff = self.retry_policy.backoff_for(started.attempt_no);
                        let can_retry =
                            self.retry_policy.allows_another_attempt(started.attempt_no)
                                && !call.gate.is_cancelled()
                                && !deadline.is_expired()
                                && deadline.remaining() > backoff;
                        if can_retry {
                            self.record_failure(
                                request.account_id,
                                job_id,
                                attempt_id,
                                &call.execution_owner,
                                fencing_token,
                                &started.receipt_credential,
                                provider.kind,
                                FailureDisposition::SafeRetry,
                                Some(provider_cost.clone()),
                                provider.trace_id.clone(),
                                LateFactsInput::default(),
                            )
                            .await?;
                            tracing::info!(
                                job_id = %job_id,
                                attempt_no = started.attempt_no,
                                backoff_ms = backoff.as_millis(),
                                max_attempts = self.retry_policy.max_attempts,
                                "the provider provably did not accept this request; retrying in-request"
                            );
                            tokio::time::sleep(backoff).await;
                            if call.gate.is_cancelled() || deadline.is_expired() {
                                self.record_failure(
                                    request.account_id,
                                    job_id,
                                    attempt_id,
                                    &call.execution_owner,
                                    fencing_token,
                                    &started.receipt_credential,
                                    provider.kind,
                                    FailureDisposition::DeterminedFailure,
                                    Some(provider_cost),
                                    provider.trace_id.clone(),
                                    LateFactsInput::default(),
                                )
                                .await?;
                                return Err(DirectExecutionError::RequestTimeout);
                            }
                            continue;
                        }
                        // 终局：释放占用与渠道名额。期限截止（或退避放不下）按 504 request_timeout，
                        // 次数耗尽按确定失败返回原平台错误码（Spec 0005 §4）。
                        let timed_out = call.gate.is_cancelled()
                            || deadline.is_expired()
                            || deadline.remaining() <= backoff;
                        self.record_failure(
                            request.account_id,
                            job_id,
                            attempt_id,
                            &call.execution_owner,
                            fencing_token,
                            &started.receipt_credential,
                            provider.kind,
                            FailureDisposition::DeterminedFailure,
                            Some(provider_cost),
                            provider.trace_id.clone(),
                            LateFactsInput::for_disposition(FailureDisposition::DeterminedFailure),
                        )
                        .await?;
                        if timed_out {
                            return Err(DirectExecutionError::RequestTimeout);
                        }
                        return Err(DirectExecutionError::OriginalFailure { code: error_code });
                    }
                    self.record_failure(
                        request.account_id,
                        job_id,
                        attempt_id,
                        &call.execution_owner,
                        fencing_token,
                        &started.receipt_credential,
                        provider.kind,
                        disposition,
                        Some(provider_cost),
                        provider.trace_id.clone(),
                        LateFactsInput::for_disposition(disposition),
                    )
                    .await?;
                    return Err(match disposition {
                        FailureDisposition::Unknown => DirectExecutionError::OutcomeUnknown,
                        FailureDisposition::DeterminedFailure | FailureDisposition::SafeRetry => {
                            DirectExecutionError::OriginalFailure { code: error_code }
                        }
                    });
                }
                Err(AdapterError::AcceptedUnpersisted { handle, .. }) => {
                    // 上游已受理、句柄没能入库：先按不可伪造的同一 Attempt 交付晚到事实，再转对账。
                    // 绝不重发（重发等于为同一个请求再付一次上游成本，RFC 0017 §4）。
                    // 只交付句柄：这一刻还不知道上游终态，收件行如实记「没有终态」。
                    let late = LateFactsInput {
                        provider_task_handle: Some(handle.task_id),
                        ..LateFactsInput::default()
                    };
                    self.hand_off_late_facts(
                        job_id,
                        attempt_id,
                        &started.receipt_credential,
                        handle.trace_id.clone(),
                        None,
                        late.clone(),
                    )
                    .await;
                    self.record_failure(
                        request.account_id,
                        job_id,
                        attempt_id,
                        &call.execution_owner,
                        fencing_token,
                        &started.receipt_credential,
                        ProviderFailureKind::PlatformInternal,
                        FailureDisposition::Unknown,
                        None,
                        handle.trace_id,
                        late,
                    )
                    .await?;
                    return Err(DirectExecutionError::OutcomeUnknown);
                }
                Err(
                    AdapterError::Configuration(message) | AdapterError::UnsupportedInput(message),
                ) => {
                    // 请求在交给渠道之前就被挡下：这次执行没有成本可采，也不再重投。
                    tracing::warn!(job_id = %job_id, reason = %message, "the adapter rejected this request");
                    self.record_failure(
                        request.account_id,
                        job_id,
                        attempt_id,
                        &call.execution_owner,
                        fencing_token,
                        &started.receipt_credential,
                        ProviderFailureKind::PlatformInternal,
                        FailureDisposition::DeterminedFailure,
                        None,
                        None,
                        LateFactsInput::default(),
                    )
                    .await?;
                    return Err(DirectExecutionError::OriginalFailure {
                        code: PublicErrorCode::PlatformUnavailable,
                    });
                }
                Err(AdapterError::CancelledBeforeSend) => {
                    // 闸口拒绝只有在生成发送确实没开始过时才等价于"确定未提交"。取消与发送竞争
                    // 同一个原子状态，发送先赢时闸口的拒绝可能来自更早的调用点，此时必须按
                    // 可能已提交收尾（Spec 0005 §5、RFC 0018 §4）。
                    let (disposition, outcome) = if context.generation_started() {
                        (
                            FailureDisposition::Unknown,
                            DirectExecutionError::OutcomeUnknown,
                        )
                    } else {
                        (
                            FailureDisposition::DeterminedFailure,
                            DirectExecutionError::RequestTimeout,
                        )
                    };
                    self.record_failure(
                        request.account_id,
                        job_id,
                        attempt_id,
                        &call.execution_owner,
                        fencing_token,
                        &started.receipt_credential,
                        ProviderFailureKind::PlatformInternal,
                        disposition,
                        None,
                        None,
                        LateFactsInput::for_disposition(disposition),
                    )
                    .await?;
                    return Err(outcome);
                }
                Err(AdapterError::Cancelled) => {
                    // 已经在等待上游结果的阶段收到取消，不能证明上游未受理：保留占用与渠道
                    // 名额，交异常对账处置（RFC 0017 §5）。
                    self.record_failure(
                        request.account_id,
                        job_id,
                        attempt_id,
                        &call.execution_owner,
                        fencing_token,
                        &started.receipt_credential,
                        ProviderFailureKind::PlatformInternal,
                        FailureDisposition::Unknown,
                        None,
                        None,
                        LateFactsInput::for_disposition(FailureDisposition::Unknown),
                    )
                    .await?;
                    return Err(DirectExecutionError::OutcomeUnknown);
                }
                Err(AdapterError::QueryAccountingUnsupported) => {
                    // 一次性 execute 不该报这个：防御性地按平台侧确定失败处置，不再重投。
                    self.record_failure(
                        request.account_id,
                        job_id,
                        attempt_id,
                        &call.execution_owner,
                        fencing_token,
                        &started.receipt_credential,
                        ProviderFailureKind::PlatformInternal,
                        FailureDisposition::DeterminedFailure,
                        None,
                        None,
                        LateFactsInput::default(),
                    )
                    .await?;
                    return Err(DirectExecutionError::OriginalFailure {
                        code: PublicErrorCode::PlatformUnavailable,
                    });
                }
            }
        }
    }

    /// 同键命中后的安全比对与 Spec 0005 §4 投影。
    ///
    /// 用记录**冻结的合同**与它写下的密钥版本重算这次请求的指纹：一致才按原阶段投影；版本不同、
    /// 旧密钥未配置，或这次请求在冻结合同下根本识别不出来时，都无法安全比对，一律按
    /// idempotency_conflict 拒绝——旧记录仍按它自己的规则解释，不拿新修订重定义（RFC 0017 §2）。
    fn replay_projection(
        &self,
        endpoint: &str,
        request: &CreateImageGenerationRequest,
        lookup: ExecutionLookup,
    ) -> Result<DirectExecutionError, ApplicationError> {
        let recognized = match contract_parameter_face(request, &lookup.capability_schema) {
            Ok(parameters) => Value::Object(parameters),
            Err(error) => {
                tracing::warn!(
                    job_id = %lookup.job_id,
                    error = %error,
                    "the current request does not fit the recorded contract; treating it as an idempotency conflict"
                );
                return Ok(idempotency_conflict());
            }
        };
        let recomputed = self.keys.request_fingerprint(
            lookup.request_digest_key_version,
            &RequestFingerprintInput {
                endpoint,
                gateway_model: &request.model,
                parameters: &recognized,
                reference_images: &request.reference_images,
                mask: request.mask.as_deref(),
                n: requested_image_count(&recognized),
            },
        )?;
        if recomputed.as_deref() != Some(lookup.request_digest.as_str()) {
            return Ok(idempotency_conflict());
        }
        Ok(project_replay(
            ExecutionReplay {
                job_id: lookup.job_id,
                stage: lookup.stage,
                error_code: lookup.error_code,
                created_at: lookup.created_at,
                updated_at: lookup.updated_at,
            },
            self.timeouts.sync_wait,
        ))
    }

    /// 选路：按策略在合格候选里挑一条，返回命中供给、承载参数面与判定记录。
    async fn select(
        &self,
        request: &CreateImageGenerationRequest,
        branch: ImageBranch,
        candidates: &[OfferingCandidate],
    ) -> Result<(PublishedOffering, Value, RoutingDecision), ApplicationError> {
        let policy = self.repository.route_policy(&request.model).await?;
        match policy {
            Some(policy) => {
                let account_tag = if policy.strategy == RouteStrategy::UserTag {
                    self.repository.account_tag(request.account_id).await?
                } else {
                    None
                };
                let choice = RouteChoice {
                    strategy: policy.strategy,
                    discount_rates: &policy.discount_rates,
                    tag_channel_map: &policy.tag_channel_map,
                    account_tag: account_tag.as_deref(),
                };
                select_candidate_with_strategy(request, branch, candidates, None, &choice)
            }
            None => select_candidate(request, branch, candidates, None),
        }
    }

    /// 成功收尾：先结算再返回载荷；证据缺失或没有产出图时转对账。
    ///
    /// 结算提交结果未知时先按 Job/Attempt 只读确认，确认不到再重试同一幂等结算；仍不明时在有限
    /// finalization 预算内把已经取得的事实交回收件端口，再按异常处置——**不丢内存里的成功事实**
    /// （RFC 0018 §5.1）。已确认结算但总期限已过时返回交付超时——原请求已完成并收费，不能改写成
    /// "结果未知"（Spec 0005 §3–§5，RFC 0017 §3）。
    #[allow(clippy::too_many_arguments)]
    async fn finish_success(
        &self,
        account_id: AccountId,
        job_id: JobId,
        attempt_id: AttemptId,
        execution_owner: &str,
        fencing_token: FencingToken,
        receipt_credential: &ReceiptCredential,
        offering: &PublishedOffering,
        output: ProviderOutput,
        total_deadline: tokio::time::Instant,
    ) -> Result<DirectExecutionSuccess, DirectExecutionError> {
        let snapshot = &offering.price_snapshot;
        let images = output.response_payload.images.len();
        let Some(usage) = output.accounting_facts.usage.clone() else {
            // 成功但证据缺失：不得按估计收费，转对账（Spec 0005 §5）。
            let provider_cost =
                failure_provider_cost(snapshot, Some(&output.accounting_facts.provider_cost));
            self.record_failure(
                account_id,
                job_id,
                attempt_id,
                execution_owner,
                fencing_token,
                receipt_credential,
                ProviderFailureKind::PlatformInternal,
                FailureDisposition::Unknown,
                Some(provider_cost),
                output.accounting_facts.provider_trace_id.clone(),
                LateFactsInput {
                    provider_state: Some(ProviderTaskState::Succeeded),
                    ..LateFactsInput::default()
                },
            )
            .await?;
            return Err(DirectExecutionError::OutcomeUnknown);
        };
        if images == 0 {
            let provider_cost =
                failure_provider_cost(snapshot, Some(&output.accounting_facts.provider_cost));
            self.record_failure(
                account_id,
                job_id,
                attempt_id,
                execution_owner,
                fencing_token,
                receipt_credential,
                ProviderFailureKind::PlatformInternal,
                FailureDisposition::Unknown,
                Some(provider_cost),
                output.accounting_facts.provider_trace_id.clone(),
                LateFactsInput {
                    provider_state: Some(ProviderTaskState::Succeeded),
                    ..LateFactsInput::default()
                },
            )
            .await?;
            return Err(DirectExecutionError::OutcomeUnknown);
        }
        // 产出张数是落库事实：用量明细与账单汇总按它报"几张"，所以取内存里的实际张数，
        // 不取请求的 `n`（RFC 0019 §5.3）。
        let image_count = u32::try_from(images).map_err(|_| {
            ApplicationError::Validation("the produced image count is out of range".to_owned())
        })?;
        let provider_cost = provider_cost_fact(
            snapshot,
            &output.accounting_facts.provider_cost,
            CostInputs::Succeeded {
                usage: &usage,
                images,
            },
        );
        let charge = snapshot
            .charge_microusd(ChargeFacts {
                usage: &usage,
                images,
                declared_cost_microusd: provider_cost.amount_microusd,
            })
            .map_err(|error| ApplicationError::Reconciliation(error.to_string()))?;
        // 结算确认失败时要交接的同一份最小事实；先构造好，提交路径不借它。
        let late = LateFactsInput {
            provider_state: Some(ProviderTaskState::Succeeded),
            provider_task_handle: None,
            image_count: Some(image_count),
            evidence: Some(MeteringEvidence {
                attempt_id,
                provider_response_digest: output.accounting_facts.response_digest.clone(),
                usage: usage.clone(),
            }),
        };
        let provider_trace_id = output.accounting_facts.provider_trace_id.clone();
        // 先结算后返回：结算提交成功之前，载荷不交给调用方（RFC 0017 §2）。
        let finalization = match self
            .finalize_settle(SettleExecution {
                job_id,
                attempt_id,
                execution_owner: execution_owner.to_owned(),
                fencing_token,
                evidence: MeteringEvidence {
                    attempt_id,
                    provider_response_digest: output.accounting_facts.response_digest.clone(),
                    usage,
                },
                provider_cost: provider_cost.clone(),
                charge_microusd: charge,
                image_count: Some(image_count),
                provider_trace_id: provider_trace_id.clone(),
            })
            .await
        {
            Ok(finalization) => finalization,
            Err(error) => {
                // token Conflict 或提交结果不明：当前所有权下无法正式结算，但成功事实已经取得，
                // 在有限 finalization 预算内交回收件端口，绝不随返回值丢掉（RFC 0018 §5.1）。
                self.hand_off_late_facts(
                    job_id,
                    attempt_id,
                    receipt_credential,
                    provider_trace_id,
                    Some(provider_cost),
                    late,
                )
                .await;
                return Err(error);
            }
        };
        self.refresh_balance(account_id).await;
        if finalization.stage != ExecutionStage::Succeeded {
            // 结算没有落成成功：不许把图片当成功交回；事实仍交回收件端口。
            self.hand_off_late_facts(
                job_id,
                attempt_id,
                receipt_credential,
                provider_trace_id,
                None,
                late,
            )
            .await;
            return Err(DirectExecutionError::OutcomeUnknown);
        }
        // 已确认结算、但总期限已过：图片来不及准备返回，按交付超时回应（结果不保留）。
        if tokio::time::Instant::now() >= total_deadline {
            return Err(DirectExecutionError::ResultDeliveryTimeout);
        }
        let mut payload = output.response_payload;
        if payload.created.is_none() {
            payload.created = Some(Utc::now().timestamp());
        }
        Ok(DirectExecutionSuccess {
            job_id,
            attempt_id,
            payload,
        })
    }

    /// 结算的"先确认、再重试同一幂等收尾"：连接断开或 COMMIT 结果未知时，绝不先假定失败
    /// （RFC 0017 §3）。
    async fn finalize_settle(
        &self,
        command: SettleExecution,
    ) -> Result<ExecutionFinalization, DirectExecutionError> {
        let job_id = command.job_id;
        let attempt_id = command.attempt_id;
        let max_attempts = self.retry_policy.max_attempts.max(1);
        for attempt in 1..=max_attempts {
            match self.executions.settle(command.clone()).await {
                Ok(finalization) => return Ok(finalization),
                Err(error) => {
                    tracing::warn!(
                        job_id = %job_id,
                        error = %error,
                        "the settle commit result is unknown; confirming the committed finalization"
                    );
                }
            }
            match self.executions.read_finalization(job_id, attempt_id).await {
                Ok(Some(finalization)) => return Ok(finalization),
                Ok(None) => {}
                Err(error) => tracing::warn!(
                    job_id = %job_id,
                    error = %error,
                    "could not confirm the settle commit result"
                ),
            }
            if attempt < max_attempts {
                tokio::time::sleep(self.retry_policy.backoff_for(attempt)).await;
            }
        }
        tracing::warn!(
            job_id = %job_id,
            attempt_id = %attempt_id,
            "the settle commit stayed unknown after bounded confirmation; leaving it to reconciliation"
        );
        Err(DirectExecutionError::OutcomeUnknown)
    }

    /// 失败处置的同一套确认：提交结果未知先读确认，再重试同一幂等处置；仍不明时返回未知。
    async fn finalize_failure(
        &self,
        command: FailOrReconcileExecution,
    ) -> Result<ExecutionFinalization, DirectExecutionError> {
        let job_id = command.job_id;
        let attempt_id = command.attempt_id;
        let max_attempts = self.retry_policy.max_attempts.max(1);
        for attempt in 1..=max_attempts {
            match self.executions.fail_or_reconcile(command.clone()).await {
                Ok(finalization) => return Ok(finalization),
                Err(error) => {
                    tracing::warn!(
                        job_id = %job_id,
                        error = %error,
                        "the failure finalization result is unknown; confirming the committed record"
                    );
                }
            }
            match self.executions.read_finalization(job_id, attempt_id).await {
                Ok(Some(finalization)) => return Ok(finalization),
                Ok(None) => {}
                Err(error) => tracing::warn!(
                    job_id = %job_id,
                    error = %error,
                    "could not confirm the failure finalization result"
                ),
            }
            if attempt < max_attempts {
                tokio::time::sleep(self.retry_policy.backoff_for(attempt)).await;
            }
        }
        tracing::warn!(
            job_id = %job_id,
            attempt_id = %attempt_id,
            "the failure finalization stayed unknown after bounded confirmation; leaving it to reconciliation"
        );
        Err(DirectExecutionError::OutcomeUnknown)
    }

    /// 把一次失败按处置落库并写穿余额。同一处置重复调用幂等；换了处置由仓储报冲突。
    ///
    /// 收尾提交结果不明或所有权已切换（token Conflict）时，当前所有权下已经无法正式结算：在有限
    /// finalization 预算内把 `late` 里的有界事实交回收件端口，再如实返回未知——不把内存里的事实
    /// 随返回值丢掉（RFC 0018 §5.1）。
    #[allow(clippy::too_many_arguments)]
    async fn record_failure(
        &self,
        account_id: AccountId,
        job_id: JobId,
        attempt_id: AttemptId,
        execution_owner: &str,
        fencing_token: FencingToken,
        receipt_credential: &ReceiptCredential,
        kind: ProviderFailureKind,
        disposition: FailureDisposition,
        provider_cost: Option<ProviderCostFact>,
        provider_trace_id: Option<ProviderTraceId>,
        late: LateFactsInput,
    ) -> Result<(), DirectExecutionError> {
        let command = FailOrReconcileExecution::for_failure(
            job_id,
            attempt_id,
            execution_owner.to_owned(),
            fencing_token,
            kind,
            disposition,
            provider_cost.clone(),
            provider_trace_id.clone(),
        );
        if let Err(error) = self.finalize_failure(command).await {
            self.hand_off_late_facts(
                job_id,
                attempt_id,
                receipt_credential,
                provider_trace_id,
                provider_cost,
                late,
            )
            .await;
            return Err(error);
        }
        self.refresh_balance(account_id).await;
        Ok(())
    }

    /// 把**已经取得、但当前所有权下无法正式结算**的有界事实交回收件端口。
    ///
    /// 它只是收件：不改所有权、不重开终态、不直接结算，所以原 token 已失效也能投递；凭据在库里
    /// 只以摘要存在，投递不携带 token（RFC 0018 §5.2）。投递幂等，因此按同一份收尾确认骨架的次数
    /// 与退避**有界重试**：多投一次不会重复扣费，漏投一次这些事实就没了。没有可交付的有界事实
    /// （请求根本没交到渠道）时直接返回，不写空收件。
    async fn hand_off_late_facts(
        &self,
        job_id: JobId,
        attempt_id: AttemptId,
        receipt_credential: &ReceiptCredential,
        provider_trace_id: Option<ProviderTraceId>,
        provider_cost: Option<ProviderCostFact>,
        late: LateFactsInput,
    ) {
        if late.provider_task_handle.is_none() && late.evidence.is_none() && provider_cost.is_none()
        {
            return;
        }
        let facts = LateFacts {
            job_id,
            attempt_id,
            receipt_credential: receipt_credential.clone(),
            provider_task_handle: late.provider_task_handle,
            provider_trace_id,
            image_count: late.image_count,
            evidence: late.evidence,
            provider_cost,
            provider_state: late.provider_state,
        };
        let max_attempts = self.retry_policy.max_attempts.max(1);
        for attempt in 1..=max_attempts {
            match self.executions.offer_late_facts(facts.clone()).await {
                Ok(_) => return,
                Err(error) => tracing::warn!(
                    job_id = %job_id,
                    attempt_id = %attempt_id,
                    error = %error,
                    "could not hand off late facts; retrying within the bounded budget"
                ),
            }
            if attempt < max_attempts {
                tokio::time::sleep(self.retry_policy.backoff_for(attempt)).await;
            }
        }
        tracing::warn!(
            job_id = %job_id,
            attempt_id = %attempt_id,
            "late facts could not be handed off within the bounded finalization budget"
        );
    }

    /// 提交后把数据库的当前余额写穿缓存（RFC 0017 §3）。读不到只记日志：缓存不是事实来源。
    async fn refresh_balance(&self, account_id: AccountId) {
        match self.repository.read_account_balance(account_id).await {
            Ok(change) => {
                self.acceleration
                    .write_balance(&change, BalanceSource::DbCommit)
                    .await;
            }
            Err(error) => tracing::warn!(
                account_id = %account_id,
                error = %error,
                "could not read the account balance to refresh the cache"
            ),
        }
    }
}

/// 已解析请求 → 旧路径的受理请求形状：图片统一成字符串（URL 或 data URL）只用于指纹与选路，
/// 上传字节在这里编码成 data URL。真正的三态图片仍原样交给 [`GatewayInput`]。
fn routing_request(
    request: &DirectExecutionRequest,
) -> Result<CreateImageGenerationRequest, ApplicationError> {
    let reference_images = request
        .reference_images
        .iter()
        .map(image_text)
        .collect::<Result<Vec<_>, _>>()?;
    let mask = request.mask.as_ref().map(image_text).transpose()?;
    Ok(CreateImageGenerationRequest {
        account_id: request.account_id,
        model: request.model.clone(),
        native_parameters: request.native_parameters.as_value().clone(),
        reference_images,
        mask,
        idempotency_key: request.idempotency_key.clone(),
    })
}

/// 图片进指纹与选路的字符串形态：URL / data URL 借用原值，上传字节编码一次。
fn image_text(image: &InputImage) -> Result<String, ApplicationError> {
    image
        .to_data_url()
        .map(|value| value.into_owned())
        .map_err(|error| ApplicationError::Validation(error.to_string()))
}

/// 组装 Adapter 的执行输入：普通参数去掉图片参数位上的取值，图片提升为强类型三态。
///
/// 选中候选的映射参数面在这里**移动**进 [`GatewayInput`]：只摘掉图片参数位的键，普通参数不会
/// 为了这一步再构造一遍（RFC 0018 §3）。
fn build_gateway_input(
    offering: &PublishedOffering,
    branch: ImageBranch,
    native_parameters: Value,
    request: &DirectExecutionRequest,
) -> Result<GatewayInput, ApplicationError> {
    let cost_currency = offering
        .price_snapshot
        .cost_currency()
        .ok_or_else(|| {
            ApplicationError::Configuration(
                "this offering declares no cost currency to hand the driver".to_owned(),
            )
        })?
        .to_owned();
    let image_sites = image_sites_for(&offering.carrier_schema, branch);
    // 映射后的参数面一定是对象（物化的产物）；别的形状按"没有普通参数"处理。
    let mut parameters = match native_parameters {
        Value::Object(parameters) => parameters,
        _ => Map::new(),
    };
    for name in platform_image_parameters(&offering.carrier_schema, branch) {
        parameters.remove(&name);
    }
    Ok(GatewayInput {
        provider_model_id: offering.provider_model_id.clone(),
        branch,
        native_parameters: Value::Object(parameters),
        reference_images: request.reference_images.clone(),
        mask: request.mask.clone(),
        image_sites,
        cost_currency,
    })
}

/// 受理时冻结的渠道身份：只含执行与账务需要的最小事实。
fn admit_offering(offering: &PublishedOffering) -> AdmitOffering {
    AdmitOffering {
        runtime_revision_id: offering.runtime_revision_id,
        vendor_model_id: offering.vendor_model_id,
        offering_id: offering.offering_id,
        channel_id: offering.channel_id,
        gateway_model: offering.gateway_model.clone(),
        adapter_key: offering.adapter_key.clone(),
        provider_model_id: offering.provider_model_id.clone(),
        base_url: offering.base_url.clone(),
        credential_env: offering.credential_env.clone(),
    }
}

/// 同键同请求之外的比对失败统一按 §4 的 idempotency_conflict 拒绝（对客 409）：
/// 旧密钥未配置、请求指纹不等，或请求在冻结合同下识别不出来时都走这里。
fn idempotency_conflict() -> DirectExecutionError {
    ApplicationError::Conflict("idempotency key was already used with different input".to_owned())
        .into()
}

/// 同键重放 → Spec 0005 §4 的四种投影。
fn project_replay(replay: ExecutionReplay, retry_after: Duration) -> DirectExecutionError {
    match replay.stage {
        ExecutionStage::Admitted | ExecutionStage::Executing => {
            DirectExecutionError::RequestInProgress { retry_after }
        }
        ExecutionStage::Succeeded => DirectExecutionError::ResultNotRetained,
        ExecutionStage::Failed => DirectExecutionError::OriginalFailure {
            code: replay
                .error_code
                .as_deref()
                .and_then(PublicErrorCode::parse)
                .unwrap_or(PublicErrorCode::PlatformUnavailable),
        },
        ExecutionStage::ReconciliationRequired => DirectExecutionError::OutcomeUnknown,
    }
}

/// 承载面上的图片参数位：名字与角色来自候选声明面与分支，wire 形状取自字段类型。
///
/// 归属只看名字（[`platform_image_parameters`]），与调用方这一次恰好给了什么取值无关；承运
/// 形状只影响 Adapter 怎么把它写到线上，不影响它是参考图还是遮罩。
fn image_sites_for(carrier_schema: &Value, branch: ImageBranch) -> ImageSites {
    let mut sites = ImageSites::default();
    for name in platform_image_parameters(carrier_schema, branch) {
        let shape = if carrier_field_is_array(carrier_schema, &name) {
            ImageValueShape::Array
        } else {
            ImageValueShape::Scalar
        };
        match image_parameter_kind(&name) {
            Some(ImageParameterKind::Reference) => {
                sites.reference = Some(ImageSite {
                    parameter: name,
                    shape,
                });
            }
            Some(ImageParameterKind::Mask) => {
                sites.mask = Some(ImageSite {
                    parameter: name,
                    shape,
                });
            }
            None => {}
        }
    }
    sites
}

/// 承载面把这个字段声明成数组了吗；没声明时按单值处理（发布期已校验过图片参数位）。
fn carrier_field_is_array(carrier_schema: &Value, name: &str) -> bool {
    carrier_schema
        .get("properties")
        .and_then(|properties| properties.get(name))
        .and_then(|field| field.get("type"))
        .and_then(Value::as_str)
        == Some("array")
}

/// 预算 → 数据库侧绝对期限。进程与数据库的时钟可能漂移，这里只做上限：真正到点由库的
/// `now()` 比较，提交声明不被"进程以为还没到"放行。
fn db_deadline(budget: Duration) -> DateTime<Utc> {
    Utc::now()
        + chrono::Duration::from_std(budget)
            .unwrap_or_else(|_| chrono::Duration::seconds(i64::MAX / 1000))
}

#[cfg(test)]
mod tests;
