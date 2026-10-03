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
    AcceptanceError, AcceptedHandle, AdapterError, Deadline, ExecutionContext, GatewayInput,
    ImageSite, ImageSites, ImageValueShape, InputImage, ProviderFailureKind, ProviderOutput,
    ResponsePayload, RetrySafety,
};
use seeai_domain::{
    AccountId, AttemptId, ChargeFacts, ExecutionStage, FencingToken, ImageBranch,
    ImageParameterKind, JobId, MeteringEvidence, OfferingCandidate, ProviderCostFact,
    PublishedOffering, RouteStrategy, image_parameter_kind, platform_image_parameters,
};
use serde_json::Value;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use thiserror::Error;

use crate::{
    AccelerationService, AdapterFactory, AdmitExecution, AdmitOffering, AdmitOutcome,
    ApplicationError, BalanceSource, BeginSubmission, CostInputs, CreateImageGenerationRequest,
    CredentialProvider, DirectExecutionLimits, ExecutionFinalization, ExecutionReplay,
    ExecutionRepository, FailOrReconcileExecution, FailureDisposition, FingerprintKeys,
    HubRepository, LateFacts, PublicErrorCode, RequestCostCeiling, RequestFingerprintInput,
    RequestTimeoutPolicy, RetryPolicy, RouteChoice, RoutingDecision, SettleExecution,
    contract_parameter_face, failure_provider_cost, freeze_offering_pricing, provider_cost_fact,
    public_error_code, requested_image_count, select_candidate, select_candidate_with_strategy,
    single_request_cost_cny, validate_idempotency_key,
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
    pub native_parameters: Value,
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

/// 调用方（API Supervisor）提供的执行身份与取消标志。
///
/// 取消标志由调用方持有并可随时置位：Adapter 在每次新的外部调用前读它，停止提交、重试与轮询；
/// 已经发出的请求不因它被证明取消。
pub struct DirectExecutionCall {
    pub execution_owner: String,
    pub cancelled: Arc<AtomicBool>,
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

/// Adapter 能看到的执行上下文：绝对期限、调用方取消标志与异步接受确认。
pub struct SupervisedExecutionContext {
    executions: Arc<dyn ExecutionRepository>,
    job_id: JobId,
    attempt_id: AttemptId,
    execution_owner: String,
    fencing_token: FencingToken,
    deadline: Deadline,
    cancelled: Arc<AtomicBool>,
}

impl SupervisedExecutionContext {
    /// 组装一次执行的上下文；`deadline` 是已经算好的绝对期限（`D − R`）。
    #[must_use]
    pub fn new(
        executions: Arc<dyn ExecutionRepository>,
        job_id: JobId,
        attempt_id: AttemptId,
        execution_owner: String,
        fencing_token: FencingToken,
        deadline: Deadline,
        cancelled: Arc<AtomicBool>,
    ) -> Self {
        Self {
            executions,
            job_id,
            attempt_id,
            execution_owner,
            fencing_token,
            deadline,
            cancelled,
        }
    }
}

#[async_trait]
impl ExecutionContext for SupervisedExecutionContext {
    fn deadline(&self) -> Deadline {
        self.deadline
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    async fn accepted(&self, handle: AcceptedHandle) -> Result<(), AcceptanceError> {
        // 已取消就不再确认句柄：确认之后 Adapter 才会按句柄轮询，而取消的意思是停止副作用。
        if self.is_cancelled() {
            return Err(AcceptanceError::Cancelled);
        }
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
    keys: FingerprintKeys,
    timeouts: RequestTimeoutPolicy,
    limits: DirectExecutionLimits,
    /// 总期限 `D` 里留给结算与提交确认的预算 `R`。
    settle_reserve: Duration,
    /// 本次执行所有权的租约时长：随 BeginSubmission 落库，并与 Supervisor 的续约间隔同源。
    ownership_lease: ChronoDuration,
    cost_ceiling: RequestCostCeiling,
    /// 可证明上游未受理时的请求内重投策略（次数与退避），与旧路径共用同一组配置。
    retry_policy: RetryPolicy,
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
        keys: FingerprintKeys,
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
            acceleration,
        }
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
        let idempotency_key_digest = self.keys.idempotency_key_digest(&request.idempotency_key);

        // 选路、承载准备与冻价都复用旧路径的同一组函数：同一条请求、同一个账户与幂等键，两条路
        // 选出同一条候选、冻出同一份快照。
        let (mut offering, native_parameters, routing) =
            self.select(&routing_input, branch, &candidates).await?;
        let hold_microusd = freeze_offering_pricing(
            self.repository.as_ref(),
            self.limits.default_hold_microusd,
            &request.native_parameters,
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

        let outcome = self
            .executions
            .admit(AdmitExecution {
                account_id: request.account_id,
                branch,
                offering: admit_offering(&offering),
                price_snapshot: offering.price_snapshot.clone(),
                routing,
                idempotency_key_digest,
                idempotency_lookup_key_version: self.keys.lookup_key_version(),
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
            .upstream_timeout_for(requested_image_count(&request.native_parameters));
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
                    ProviderFailureKind::PlatformInternal,
                    FailureDisposition::DeterminedFailure,
                    None,
                    None,
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
                deadline,
                call.cancelled.clone(),
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
                                && !call.cancelled.load(Ordering::Relaxed)
                                && !deadline.is_expired()
                                && deadline.remaining() > backoff;
                        if can_retry {
                            self.record_failure(
                                request.account_id,
                                job_id,
                                attempt_id,
                                &call.execution_owner,
                                fencing_token,
                                provider.kind,
                                FailureDisposition::SafeRetry,
                                Some(provider_cost.clone()),
                                provider.trace_id.clone(),
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
                            if call.cancelled.load(Ordering::Relaxed) || deadline.is_expired() {
                                self.record_failure(
                                    request.account_id,
                                    job_id,
                                    attempt_id,
                                    &call.execution_owner,
                                    fencing_token,
                                    provider.kind,
                                    FailureDisposition::DeterminedFailure,
                                    Some(provider_cost),
                                    provider.trace_id.clone(),
                                )
                                .await?;
                                return Err(DirectExecutionError::RequestTimeout);
                            }
                            continue;
                        }
                        // 终局：释放占用与渠道名额。期限截止（或退避放不下）按 504 request_timeout，
                        // 次数耗尽按确定失败返回原平台错误码（Spec 0005 §4）。
                        let timed_out = call.cancelled.load(Ordering::Relaxed)
                            || deadline.is_expired()
                            || deadline.remaining() <= backoff;
                        self.record_failure(
                            request.account_id,
                            job_id,
                            attempt_id,
                            &call.execution_owner,
                            fencing_token,
                            provider.kind,
                            FailureDisposition::DeterminedFailure,
                            Some(provider_cost),
                            provider.trace_id.clone(),
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
                        provider.kind,
                        disposition,
                        Some(provider_cost),
                        provider.trace_id.clone(),
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
                    let late = LateFacts {
                        job_id,
                        attempt_id,
                        provider_task_handle: Some(handle.task_id),
                        provider_trace_id: handle.trace_id,
                        image_count: None,
                        evidence: None,
                        provider_cost: None,
                    };
                    if let Err(error) = self.executions.offer_late_facts(late).await {
                        tracing::warn!(
                            job_id = %job_id,
                            error = %error,
                            "could not record a late acceptance handle; the execution stays unknown"
                        );
                    }
                    self.record_failure(
                        request.account_id,
                        job_id,
                        attempt_id,
                        &call.execution_owner,
                        fencing_token,
                        ProviderFailureKind::PlatformInternal,
                        FailureDisposition::Unknown,
                        None,
                        None,
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
                        ProviderFailureKind::PlatformInternal,
                        FailureDisposition::DeterminedFailure,
                        None,
                        None,
                    )
                    .await?;
                    return Err(DirectExecutionError::OriginalFailure {
                        code: PublicErrorCode::PlatformUnavailable,
                    });
                }
                Err(AdapterError::Cancelled) => {
                    // 取消不能证明上游未受理：保留占用与渠道名额，交异常对账处置（RFC 0017 §5）。
                    self.record_failure(
                        request.account_id,
                        job_id,
                        attempt_id,
                        &call.execution_owner,
                        fencing_token,
                        ProviderFailureKind::PlatformInternal,
                        FailureDisposition::Unknown,
                        None,
                        None,
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
                        ProviderFailureKind::PlatformInternal,
                        FailureDisposition::DeterminedFailure,
                        None,
                        None,
                    )
                    .await?;
                    return Err(DirectExecutionError::OriginalFailure {
                        code: PublicErrorCode::PlatformUnavailable,
                    });
                }
            }
        }
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
    /// 结算提交结果未知时先按 Job/Attempt 只读确认，确认不到再重试同一幂等结算；仍不明时转异常
    /// 处置。已确认结算但总期限已过时返回交付超时——原请求已完成并收费，不能改写成"结果未知"
    /// （Spec 0005 §3–§5，RFC 0017 §3）。
    #[allow(clippy::too_many_arguments)]
    async fn finish_success(
        &self,
        account_id: AccountId,
        job_id: JobId,
        attempt_id: AttemptId,
        execution_owner: &str,
        fencing_token: FencingToken,
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
                ProviderFailureKind::PlatformInternal,
                FailureDisposition::Unknown,
                Some(provider_cost),
                output.accounting_facts.provider_trace_id.clone(),
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
                ProviderFailureKind::PlatformInternal,
                FailureDisposition::Unknown,
                Some(provider_cost),
                output.accounting_facts.provider_trace_id.clone(),
            )
            .await?;
            return Err(DirectExecutionError::OutcomeUnknown);
        }
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
        // 先结算后返回：结算提交成功之前，载荷不交给调用方（RFC 0017 §2）。
        let finalization = self
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
                provider_cost,
                charge_microusd: charge,
                provider_trace_id: output.accounting_facts.provider_trace_id.clone(),
            })
            .await?;
        self.refresh_balance(account_id).await;
        if finalization.stage != ExecutionStage::Succeeded {
            // 结算没有落成成功：不许把图片当成功交回。
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
    #[allow(clippy::too_many_arguments)]
    async fn record_failure(
        &self,
        account_id: AccountId,
        job_id: JobId,
        attempt_id: AttemptId,
        execution_owner: &str,
        fencing_token: FencingToken,
        kind: ProviderFailureKind,
        disposition: FailureDisposition,
        provider_cost: Option<ProviderCostFact>,
        provider_trace_id: Option<String>,
    ) -> Result<(), DirectExecutionError> {
        self.finalize_failure(FailOrReconcileExecution::for_failure(
            job_id,
            attempt_id,
            execution_owner.to_owned(),
            fencing_token,
            kind,
            disposition,
            provider_cost,
            provider_trace_id,
        ))
        .await?;
        self.refresh_balance(account_id).await;
        Ok(())
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
        native_parameters: request.native_parameters.clone(),
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
    let mut parameters = native_parameters.as_object().cloned().unwrap_or_default();
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
