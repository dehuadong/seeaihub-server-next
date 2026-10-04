//! 异常对账 Worker 用例：接管过期所有权、按已知句柄只读查询、证据幂等结算、缺口建案、孤儿回收、
//! 晚到事实消费与慢周期账务核对（RFC 0017 §3、§5、§6）。
//!
//! 只读查询受对账案例上的排期约束：`next_query_at` 未到的记录本轮跳过，`attempts` 到上限后
//! 转人工、不再自动查询（见 `ReconciliationPolicy`）。
//!
//! 它与旧 Worker 的生成队列路径分开：绝不重新提交生成请求，也不读请求或响应正文。每轮有界、
//! 可空转，动作顺序固定：接管 → 只读查询并按渠道状态结算、按确定失败释放或建案 → 孤儿回收 →
//! 晚到事实消费 → 慢周期账务核对。
//! 结算与失败提交之后把余额写穿缓存；平台侧事件只经 PlatformAlert::Execution 外发。
//!
//! 接管与查询都要求执行所有权：API 仍持有效租约时 Worker 不动它，只等租约过期后由数据库比较并
//! 交换所有权。晚到事实只在 Worker 本轮接管到该执行、或该执行已经终结时才处理；否则留给领取
//! TTL 到期重领。

use chrono::{Duration as ChronoDuration, Utc};
use seeai_adapter_sdk::{
    AcceptedHandle, AccountingFacts, AdapterError, Deadline, ProviderCost, ProviderTaskHandle,
    QueryAccountingCapability,
};
use seeai_domain::{
    AccountId, AttemptId, AttemptStage, ChargeFacts, ExecutionStage, JobId, MeteringEvidence,
    PriceSnapshot, PricingFormula, ProviderCostFact, ProviderCostSource, TokenUsage,
};
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use crate::{
    AccelerationService, AdapterFactory, ApplicationError, BalanceSource, ClaimedLateFact,
    CostInputs, CredentialProvider, ExecutionFinalization, ExecutionRepository,
    FailOrReconcileExecution, FailureDisposition, HubRepository, LateFactKind, LedgerAuditor,
    PlatformAlert, PlatformAlerter, ProviderFailureKind, RetryPolicy, SettleExecution,
    TakenOverExecution, failure_provider_cost, provider_cost_fact,
};

/// 异常对账每轮的边界、查询排期与慢周期节奏。都是运维取值：批大小决定一轮的最坏工作量，
/// 租约与领取 TTL 决定重试的疏密，查询超时是单次只读调用的上界，查询上限与退避决定一个对账
/// 案例会被自动查询多少次、每次隔多久。
#[derive(Debug, Clone, Copy)]
pub struct ReconciliationPolicy {
    /// 每轮接管、回收与领取最多处理多少条。
    pub batch_limit: u32,
    /// 从未写下提交声明的 admitted 超过这个年龄才回收。
    pub orphan_max_age: ChronoDuration,
    /// 晚到事实的领取 TTL：领取超过它、尚未消费的行可被另一领取者重领。
    pub late_fact_claim_ttl: ChronoDuration,
    /// 单次只读账务查询的上界。
    pub query_timeout: Duration,
    /// 一个对账案例最多自动发起多少次只读查询；到上限转人工，不再自动查询。
    pub query_max_attempts: u32,
    /// 查询退避基：第 n 次查询之后排「基 × 2^(n-1)」。
    pub query_backoff_base: Duration,
    /// 单次查询退避的上限（指数不许无限翻倍）。
    pub query_backoff_max: Duration,
    /// 每多少轮跑一次慢周期账务核对。
    pub ledger_audit_every_rounds: u64,
    /// 慢周期一次最多核对几个账户（本地截断）。
    pub ledger_audit_limit: u32,
    /// 慢周期按账户 updated_at 取的增量窗口。
    pub ledger_audit_window: Duration,
}

impl Default for ReconciliationPolicy {
    fn default() -> Self {
        Self {
            batch_limit: 16,
            orphan_max_age: ChronoDuration::minutes(15),
            late_fact_claim_ttl: ChronoDuration::minutes(5),
            query_timeout: Duration::from_secs(30),
            query_max_attempts: 5,
            query_backoff_base: Duration::from_secs(30),
            query_backoff_max: Duration::from_secs(3_600),
            ledger_audit_every_rounds: 10,
            ledger_audit_limit: 100,
            ledger_audit_window: Duration::from_secs(3_600),
        }
    }
}

impl ReconciliationPolicy {
    /// 从环境变量读运维取值。每一项都在 Default 里有缺省。
    ///
    /// - RECONCILIATION_BATCH_LIMIT（16）
    /// - RECONCILIATION_ORPHAN_MAX_AGE_SECONDS（900）
    /// - RECONCILIATION_LATE_FACT_CLAIM_TTL_SECONDS（300）
    /// - RECONCILIATION_QUERY_TIMEOUT_SECONDS（30）
    /// - RECONCILIATION_QUERY_MAX_ATTEMPTS（5，查询重试上限）
    /// - RECONCILIATION_QUERY_BACKOFF_BASE_SECONDS（30，退避基）
    /// - RECONCILIATION_QUERY_BACKOFF_MAX_SECONDS（3600，退避上限）
    /// - RECONCILIATION_LEDGER_AUDIT_EVERY_ROUNDS（10）
    /// - RECONCILIATION_LEDGER_AUDIT_LIMIT（100）
    /// - RECONCILIATION_LEDGER_AUDIT_WINDOW_SECONDS（3600）
    ///
    /// 取值不合法（不是整数、批大小为 0、查询上限为 0、退避基为 0、退避上限小于基）时返回
    /// 配置错误，不让进程带着一条断链的对账循环起来。
    pub fn from_env() -> Result<Self, ApplicationError> {
        let default = Self::default();
        let policy = Self {
            batch_limit: env_number("RECONCILIATION_BATCH_LIMIT", default.batch_limit)?,
            orphan_max_age: ChronoDuration::seconds(env_number(
                "RECONCILIATION_ORPHAN_MAX_AGE_SECONDS",
                default.orphan_max_age.num_seconds(),
            )?),
            late_fact_claim_ttl: ChronoDuration::seconds(env_number(
                "RECONCILIATION_LATE_FACT_CLAIM_TTL_SECONDS",
                default.late_fact_claim_ttl.num_seconds(),
            )?),
            query_timeout: Duration::from_secs(env_number(
                "RECONCILIATION_QUERY_TIMEOUT_SECONDS",
                default.query_timeout.as_secs(),
            )?),
            query_max_attempts: env_number(
                "RECONCILIATION_QUERY_MAX_ATTEMPTS",
                default.query_max_attempts,
            )?,
            query_backoff_base: Duration::from_secs(env_number(
                "RECONCILIATION_QUERY_BACKOFF_BASE_SECONDS",
                default.query_backoff_base.as_secs(),
            )?),
            query_backoff_max: Duration::from_secs(env_number(
                "RECONCILIATION_QUERY_BACKOFF_MAX_SECONDS",
                default.query_backoff_max.as_secs(),
            )?),
            ledger_audit_every_rounds: env_number(
                "RECONCILIATION_LEDGER_AUDIT_EVERY_ROUNDS",
                default.ledger_audit_every_rounds,
            )?,
            ledger_audit_limit: env_number(
                "RECONCILIATION_LEDGER_AUDIT_LIMIT",
                default.ledger_audit_limit,
            )?,
            ledger_audit_window: Duration::from_secs(env_number(
                "RECONCILIATION_LEDGER_AUDIT_WINDOW_SECONDS",
                default.ledger_audit_window.as_secs(),
            )?),
        };
        if policy.batch_limit == 0 {
            return Err(ApplicationError::Configuration(
                "RECONCILIATION_BATCH_LIMIT must be at least 1".to_owned(),
            ));
        }
        if policy.query_max_attempts == 0 {
            return Err(ApplicationError::Configuration(
                "RECONCILIATION_QUERY_MAX_ATTEMPTS must be at least 1".to_owned(),
            ));
        }
        if policy.query_backoff_base.is_zero() {
            return Err(ApplicationError::Configuration(
                "RECONCILIATION_QUERY_BACKOFF_BASE_SECONDS must be positive".to_owned(),
            ));
        }
        if policy.query_backoff_max < policy.query_backoff_base {
            return Err(ApplicationError::Configuration(
                "RECONCILIATION_QUERY_BACKOFF_MAX_SECONDS must not be smaller than RECONCILIATION_QUERY_BACKOFF_BASE_SECONDS".to_owned(),
            ));
        }
        if policy.ledger_audit_every_rounds == 0 {
            return Err(ApplicationError::Configuration(
                "RECONCILIATION_LEDGER_AUDIT_EVERY_ROUNDS must be at least 1".to_owned(),
            ));
        }
        Ok(policy)
    }

    /// 第 attempts_after 次查询之后要把 next_query_at 推后多久。指数退避，封顶
    /// query_backoff_max；封顶是必要的，否则次数大时会溢出成负的时间间隔。
    #[must_use]
    fn query_backoff(&self, attempts_after: u32) -> ChronoDuration {
        let doublings = attempts_after.saturating_sub(1).min(31);
        let millis = self
            .query_backoff_base
            .as_millis()
            .saturating_mul(1_u128 << doublings)
            .min(self.query_backoff_max.as_millis());
        ChronoDuration::milliseconds(i64::try_from(millis).unwrap_or(i64::MAX))
    }
}

/// 读一个整数环境变量；没给或给空取默认值，给得不合法报配置错误。
fn env_number<T: std::str::FromStr>(name: &str, default: T) -> Result<T, ApplicationError> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => value
            .trim()
            .parse()
            .map_err(|_| ApplicationError::Configuration(format!("{name} must be an integer"))),
        _ => Ok(default),
    }
}

/// 一轮对账做了什么。只给日志与测试看，不参与任何处置；全为零表示这一轮空转。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconciliationReport {
    pub taken_over: u64,
    pub queried: u64,
    pub settled: u64,
    pub reconciled: u64,
    /// 按渠道确定的失败或取消释放的条数：这些执行实收为零，也不建对账案例。
    pub failed: u64,
    pub reaped_orphans: u64,
    pub late_facts_consumed: u64,
    pub ledger_accounts_audited: u64,
}

impl ReconciliationReport {
    /// 这一轮有没有真的动过什么东西。空转时主循环退避等待。
    #[must_use]
    pub fn did_work(&self) -> bool {
        self.taken_over > 0
            || self.queried > 0
            || self.settled > 0
            || self.reconciled > 0
            || self.failed > 0
            || self.reaped_orphans > 0
            || self.late_facts_consumed > 0
            || self.ledger_accounts_audited > 0
    }
}

/// 一条晚到事实被消费掉，还是留给 TTL 重领。
enum LateFactAction {
    Consumed,
    Keep,
}

/// 异常对账用例。
pub struct ExecutionReconciliationService {
    repository: Arc<dyn HubRepository>,
    executions: Arc<dyn ExecutionRepository>,
    adapters: Arc<dyn AdapterFactory>,
    credentials: Arc<dyn CredentialProvider>,
    ledger_auditor: Arc<LedgerAuditor>,
    worker_id: String,
    /// 接管后写回的租约时长：查询与结算要在这个窗口内完成。
    lease: ChronoDuration,
    policy: ReconciliationPolicy,
    /// 收尾提交结果未知时的确认重试次数与退避（与直接执行共用同一组运维取值）。
    retry_policy: RetryPolicy,
    acceleration: Arc<AccelerationService>,
    /// 平台故障告警出口：配了才有；没配时不外发，但照样建案。
    alerts: Option<Arc<PlatformAlerter>>,
    /// 已跑过的轮数，用来定慢周期。
    rounds: AtomicU64,
}

impl ExecutionReconciliationService {
    #[must_use]
    pub fn new(
        repository: Arc<dyn HubRepository>,
        executions: Arc<dyn ExecutionRepository>,
        adapters: Arc<dyn AdapterFactory>,
        credentials: Arc<dyn CredentialProvider>,
        worker_id: String,
        lease: ChronoDuration,
    ) -> Self {
        let acceleration = Arc::new(AccelerationService::disabled(repository.clone()));
        let ledger_auditor = Arc::new(LedgerAuditor::new(repository.clone()));
        Self {
            repository,
            executions,
            adapters,
            credentials,
            ledger_auditor,
            worker_id,
            lease,
            policy: ReconciliationPolicy::default(),
            retry_policy: RetryPolicy::default(),
            acceleration,
            alerts: None,
            rounds: AtomicU64::new(0),
        }
    }

    /// 装上运维给的每轮边界与慢周期节奏。
    #[must_use]
    pub fn with_policy(mut self, policy: ReconciliationPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// 装上收尾确认用的重试策略。
    #[must_use]
    pub fn with_retry_policy(mut self, retry_policy: RetryPolicy) -> Self {
        self.retry_policy = retry_policy;
        self
    }

    /// 装上加速层：结算与失败提交后要把余额写穿缓存。
    #[must_use]
    pub fn with_acceleration(mut self, acceleration: Arc<AccelerationService>) -> Self {
        self.acceleration = acceleration;
        self
    }

    /// 装上平台告警出口：执行建案与账实不符共用它。
    #[must_use]
    pub fn with_platform_alerts(mut self, alerter: Arc<PlatformAlerter>) -> Self {
        self.ledger_auditor =
            Arc::new(LedgerAuditor::new(self.repository.clone()).with_alerts(alerter.clone()));
        self.alerts = Some(alerter);
        self
    }

    /// 跑一轮对账。顺序固定：接管 → 孤儿回收 → 晚到事实消费 → 慢周期账务核对。
    ///
    /// 单条执行的处置失败只记日志、不中断整轮；只有顶层端口的读取失败才向上报错。
    pub async fn run_once(&self) -> Result<ReconciliationReport, ApplicationError> {
        let mut report = ReconciliationReport::default();
        // 1) 接管过期所有权，并立即按只读投影处置。接管的执行同时留给第 3 步的晚到事实核对。
        let taken = self
            .executions
            .takeover_expired_executions(
                &self.worker_id,
                self.lease,
                self.policy.batch_limit,
                self.policy.query_max_attempts,
            )
            .await?;
        report.taken_over = u64::try_from(taken.len()).unwrap_or(u64::MAX);
        let mut owned: HashMap<JobId, TakenOverExecution> = HashMap::new();
        for execution in taken {
            if let Err(error) = self.reconcile_taken_over(&execution, &mut report).await {
                tracing::warn!(
                    job_id = %execution.job_id,
                    error = %error,
                    "could not reconcile a taken-over execution; it stays in reconciliation"
                );
            }
            owned.insert(execution.job_id, execution);
        }

        // 2) 孤儿 admitted 回收：没有提交声明就确定没有外部副作用。
        match self
            .executions
            .reap_unsubmitted_admissions(self.policy.orphan_max_age, self.policy.batch_limit)
            .await
        {
            Ok(count) => report.reaped_orphans = count,
            Err(error) => tracing::warn!(
                error = %error,
                "could not reap unsubmitted admissions this round"
            ),
        }

        // 3) 晚到事实：按 kind 处理；处理失败不 mark，等领取 TTL 到期重领。
        match self
            .executions
            .claim_unconsumed_late_facts(
                &self.worker_id,
                self.policy.batch_limit,
                self.policy.late_fact_claim_ttl,
            )
            .await
        {
            Ok(facts) => {
                for fact in &facts {
                    if let Err(error) = self.consume_late_fact(fact, &owned, &mut report).await {
                        tracing::warn!(
                            fact_id = %fact.id,
                            job_id = %fact.job_id,
                            error = %error,
                            "could not consume a claimed late fact; it stays unconsumed for a later claim"
                        );
                    }
                }
            }
            Err(error) => tracing::warn!(
                error = %error,
                "could not claim late facts this round"
            ),
        }

        // 4) 慢周期账务核对：只建案告警，不改账。
        if self.slow_cycle_due() {
            self.audit_ledgers(&mut report).await;
        }
        Ok(report)
    }

    /// 处置一台刚接管的执行：有可信句柄走只读查询，否则保留占用并建案。
    async fn reconcile_taken_over(
        &self,
        execution: &TakenOverExecution,
        report: &mut ReconciliationReport,
    ) -> Result<(), ApplicationError> {
        let Some(attempt_id) = execution.attempt_id else {
            // executing/reconciliation_required 理应有 Attempt；真没有时无从收尾，留到下一轮。
            tracing::warn!(
                job_id = %execution.job_id,
                "an execution was taken over with no attempt; leaving it for the next round"
            );
            return Ok(());
        };
        // 查询排期守门（RFC 0017 §5）：接管语句已经挡过一层，这里是同一判据的兜底——额度
        // 用尽的案例不再自动查询、转人工；没到 next_query_at 的记录本轮跳过。
        if execution.query_attempts >= self.policy.query_max_attempts {
            tracing::warn!(
                job_id = %execution.job_id,
                attempts = execution.query_attempts,
                "the reconciliation query budget is exhausted; the case needs manual handling"
            );
            return Ok(());
        }
        if execution
            .next_query_at
            .is_some_and(|next| next > Utc::now())
        {
            return Ok(());
        }
        let trusted_handle = matches!(execution.attempt_state, Some(AttemptStage::Accepted))
            && execution.provider_task_handle.is_some();
        if !trusted_handle {
            // 无句柄、或提交状态为 submitting/unknown（崩溃后与正在发不可区分）：
            // 绝不凭没有句柄判未受理，保留 Hold 与渠道槽位并建案。
            return self
                .reconcile_unknown(execution, attempt_id, None, report)
                .await;
        }
        let Some(handle) = execution.provider_task_handle.clone() else {
            return Ok(());
        };
        let query = match self
            .read_only_accounting(
                execution,
                &handle,
                execution.provider_trace_id.clone(),
                report,
            )
            .await
        {
            QueryOutcome::Facts(query) => query,
            QueryOutcome::Unsupported | QueryOutcome::Unavailable => {
                return self
                    .reconcile_unknown(execution, attempt_id, None, report)
                    .await;
            }
            QueryOutcome::Transient => return Ok(()),
        };
        match query_disposition(query.state) {
            // 任务仍在跑：不结算、不建案、不消费任何事实；租约到期后再接管重查。
            QueryDisposition::Retain => Ok(()),
            // 渠道终态不能确认：保留资金与渠道占用，进入对账。
            QueryDisposition::Reconcile => {
                self.reconcile_unknown(execution, attempt_id, None, report)
                    .await
            }
            QueryDisposition::Settle => {
                self.settle_query_facts(execution, attempt_id, query.accounting_facts, report)
                    .await
            }
            QueryDisposition::Fail => {
                self.fail_query_task(execution, attempt_id, query.accounting_facts, report)
                    .await
            }
        }
    }

    /// 渠道确认失败或取消：按既有失败规则释放消费者占用与渠道容量，实收为零。
    ///
    /// 渠道捎带返回的用量与成本只用于记录平台成本，不构成向消费者收费的依据——失败不是成功，
    /// 有计量也不能把终态翻过来（Spec 0005 §5）。
    async fn fail_query_task(
        &self,
        execution: &TakenOverExecution,
        attempt_id: AttemptId,
        facts: Option<AccountingFacts>,
        report: &mut ReconciliationReport,
    ) -> Result<(), ApplicationError> {
        let provider_cost = failure_provider_cost(
            &execution.price_snapshot,
            facts.as_ref().map(|facts| &facts.provider_cost),
        );
        let provider_trace_id = facts
            .as_ref()
            .and_then(|facts| facts.provider_trace_id.clone())
            .or_else(|| execution.provider_trace_id.clone());
        let command = FailOrReconcileExecution::for_failure(
            execution.job_id,
            attempt_id,
            self.worker_id.clone(),
            execution.fencing_token,
            // 与同步执行看到同一终态时的分类保持一致，不因为观察时机不同而另判一套。
            ProviderFailureKind::Unknown,
            FailureDisposition::DeterminedFailure,
            Some(provider_cost),
            provider_trace_id,
        );
        let finalization = self.fail_with_confirmation(command).await?;
        report.failed += 1;
        self.refresh_balance(execution.account_id).await;
        tracing::info!(
            job_id = %execution.job_id,
            stage = %finalization.stage,
            "the reconciliation confirmed an upstream failure or cancellation; the consumer is not charged"
        );
        Ok(())
    }

    /// 按只读查询结果收尾：有效计量证据结算一次，证据缺失或算不出对客价就保留占用并建案。
    async fn settle_query_facts(
        &self,
        execution: &TakenOverExecution,
        attempt_id: AttemptId,
        facts: Option<AccountingFacts>,
        report: &mut ReconciliationReport,
    ) -> Result<(), ApplicationError> {
        let Some(facts) = facts else {
            // 终态但没有账务事实：证据缺失，成本落 unavailable 进缺口。
            let provider_cost = failure_provider_cost(&execution.price_snapshot, None);
            return self
                .reconcile_unknown(execution, attempt_id, Some(provider_cost), report)
                .await;
        };
        let Some(usage) = facts.usage.clone() else {
            // 有账务信封但没有可用计量：同样不按估计收费。
            let provider_cost = failure_provider_cost(&execution.price_snapshot, None);
            return self
                .reconcile_unknown(execution, attempt_id, Some(provider_cost), report)
                .await;
        };
        let images = usize::try_from(facts.image_count).unwrap_or(usize::MAX);
        let provider_cost = provider_cost_fact(
            &execution.price_snapshot,
            &facts.provider_cost,
            CostInputs::Succeeded {
                usage: &usage,
                images,
            },
        );
        let charge = execution.price_snapshot.charge_microusd(ChargeFacts {
            usage: &usage,
            images,
            declared_cost_microusd: provider_cost.amount_microusd,
        });
        let Ok(charge_microusd) = charge else {
            // 冻结快照算不出对客价：成本照落，占用保留，交人工。
            tracing::warn!(
                job_id = %execution.job_id,
                "the frozen price snapshot cannot price this accounting fact; leaving a cost gap"
            );
            return self
                .reconcile_unknown(execution, attempt_id, Some(provider_cost), report)
                .await;
        };
        let command = SettleExecution {
            job_id: execution.job_id,
            attempt_id,
            execution_owner: self.worker_id.clone(),
            fencing_token: execution.fencing_token,
            evidence: MeteringEvidence {
                attempt_id,
                provider_response_digest: facts.response_digest.clone(),
                usage,
            },
            provider_cost,
            charge_microusd,
            image_count: Some(facts.image_count),
            provider_trace_id: facts.provider_trace_id.clone(),
        };
        let finalization = self.settle_with_confirmation(command).await?;
        report.settled += 1;
        self.refresh_balance(execution.account_id).await;
        tracing::info!(
            job_id = %execution.job_id,
            stage = %finalization.stage,
            charge_microusd = finalization.charge_microusd,
            "the reconciliation settled the execution from accounting facts; the image is discarded"
        );
        Ok(())
    }

    /// 结果未知的处置：保留 Hold 与渠道槽位、建对账案例，成本事实按传入落库。
    async fn reconcile_unknown(
        &self,
        execution: &TakenOverExecution,
        attempt_id: AttemptId,
        provider_cost: Option<ProviderCostFact>,
        report: &mut ReconciliationReport,
    ) -> Result<(), ApplicationError> {
        let command = FailOrReconcileExecution::for_failure(
            execution.job_id,
            attempt_id,
            self.worker_id.clone(),
            execution.fencing_token,
            ProviderFailureKind::PlatformInternal,
            FailureDisposition::Unknown,
            provider_cost,
            execution.provider_trace_id.clone(),
        );
        let was_executing = execution.stage == ExecutionStage::Executing;
        let finalization = self.fail_with_confirmation(command).await?;
        report.reconciled += 1;
        self.refresh_balance(execution.account_id).await;
        // 只有从 executing 第一次转对账才是新的平台侧事件；已在 reconciliation_required 的
        // 重试不再刷同一条告警。
        if was_executing {
            self.raise_execution_alert(execution, ProviderFailureKind::PlatformInternal)
                .await;
        }
        tracing::info!(
            job_id = %execution.job_id,
            stage = %finalization.stage,
            "the execution stays unknown: the hold and channel slot are retained and a reconciliation case is open"
        );
        Ok(())
    }

    /// 一次性只读查询：能力、凭证、成本币种或适配器任缺一律不查，查询失败按可重试处理。
    async fn read_only_accounting(
        &self,
        execution: &TakenOverExecution,
        handle: &str,
        trace_id: Option<String>,
        report: &mut ReconciliationReport,
    ) -> QueryOutcome {
        // 存量里可能有不是标识的旧句柄（URL、data URL、超长值）：拿它去拼上游查询 URL 等于把
        // 一段正文当任务名发出去。这类执行不具备只读查询条件，保留占用并建案（Spec 0005 §5）。
        if !seeai_domain::is_bounded_provider_identifier(handle) {
            tracing::warn!(
                job_id = %execution.job_id,
                "the stored provider task handle is not a bounded identifier; the read-only query is refused"
            );
            return QueryOutcome::Unavailable;
        }
        let adapter = match self.adapters.create_gateway(
            &execution.adapter_key,
            &execution.base_url,
            self.policy.query_timeout,
        ) {
            Ok(adapter) => adapter,
            Err(error) => {
                tracing::warn!(
                    job_id = %execution.job_id,
                    adapter_key = %execution.adapter_key,
                    error = %error,
                    "the read-only query adapter is unavailable; the execution keeps its current facts"
                );
                return QueryOutcome::Unavailable;
            }
        };
        if adapter.query_accounting_capability() == QueryAccountingCapability::Unsupported {
            // 声明不支持的渠道只建案，不伪造可恢复句柄。
            return QueryOutcome::Unsupported;
        }
        let Some(cost_currency) = execution.price_snapshot.cost_currency().map(str::to_owned)
        else {
            tracing::warn!(
                job_id = %execution.job_id,
                "the frozen snapshot carries no cost currency for the read-only query"
            );
            return QueryOutcome::Unavailable;
        };
        let credential = match self.credentials.resolve(&execution.credential_env) {
            Ok(credential) => credential,
            Err(error) => {
                tracing::warn!(
                    job_id = %execution.job_id,
                    error = %error,
                    "the provider credential for the read-only query is unavailable"
                );
                return QueryOutcome::Unavailable;
            }
        };
        // 上面的有界校验已经把不是标识的值拦下；这里把同一条不变量变成类型。
        let Ok(task_id) = ProviderTaskHandle::parse(handle.to_owned()) else {
            return QueryOutcome::Unavailable;
        };
        let handle = AcceptedHandle { task_id, trace_id };
        let deadline = Deadline::after(self.policy.query_timeout);
        report.queried += 1;
        // 记一次查询尝试并排下次退避（RFC 0017 §5）。没有未结案例（仍在 executing）时不落任何
        // 行，那种查询的间隔由所有权租约本身给出；额度刚用尽时告警一次、转人工。
        let backoff = self
            .policy
            .query_backoff(execution.query_attempts.saturating_add(1));
        match self
            .executions
            .record_reconciliation_query_attempt(execution.job_id, backoff)
            .await
        {
            Ok(Some(attempts)) if attempts >= self.policy.query_max_attempts => {
                tracing::warn!(
                    job_id = %execution.job_id,
                    attempts,
                    "the reconciliation query budget is exhausted; the case needs manual handling"
                );
                self.raise_execution_alert(execution, ProviderFailureKind::PlatformInternal)
                    .await;
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(
                job_id = %execution.job_id,
                error = %error,
                "could not record the reconciliation query attempt; the schedule may not advance"
            ),
        }
        match adapter
            .query_accounting(&handle, &cost_currency, deadline, &credential)
            .await
        {
            Ok(query) => QueryOutcome::Facts(query),
            Err(AdapterError::QueryAccountingUnsupported) => QueryOutcome::Unsupported,
            Err(error) => {
                // 查询本身失败不证明任何处置：保留当前所有权与状态，租约到期后再试。
                tracing::warn!(
                    job_id = %execution.job_id,
                    error = %error,
                    "the read-only accounting query failed; the execution is retried after the lease expires"
                );
                QueryOutcome::Transient
            }
        }
    }

    /// 消费一条晚到事实。返回 Consumed 才由调用方 mark。
    async fn consume_late_fact(
        &self,
        fact: &ClaimedLateFact,
        owned: &HashMap<JobId, TakenOverExecution>,
        report: &mut ReconciliationReport,
    ) -> Result<(), ApplicationError> {
        // 未知取值在持久化映射处解析报错，到不了这里；这个 match 因此是穷尽的。
        let outcome = match fact.kind {
            LateFactKind::TaskHandle => self.consume_task_handle_fact(fact, owned, report).await,
            LateFactKind::Accounting => self.consume_accounting_fact(fact, owned, report).await,
        };
        match outcome {
            Ok(LateFactAction::Consumed) => {
                if self.executions.mark_late_fact_consumed(fact.id).await? {
                    report.late_facts_consumed += 1;
                }
            }
            Ok(LateFactAction::Keep) => {}
            // 处理失败不 mark：领取 TTL 到期后由下一轮重领。
            Err(error) => tracing::warn!(
                fact_id = %fact.id,
                job_id = %fact.job_id,
                error = %error,
                "a claimed late fact could not be consumed; it stays unconsumed for a later claim"
            ),
        }
        Ok(())
    }

    /// 账务晚到事实：Worker 本轮接管到该执行时按现有幂等结算端口收尾；否则只消费已终结的。
    async fn consume_accounting_fact(
        &self,
        fact: &ClaimedLateFact,
        owned: &HashMap<JobId, TakenOverExecution>,
        report: &mut ReconciliationReport,
    ) -> Result<LateFactAction, ApplicationError> {
        let Some(execution) = owned.get(&fact.job_id) else {
            // 不拥有：已经终结就消费掉，否则留给 TTL 重领（API 仍持有效所有权时 Worker 不查）。
            if !self.finalized_terminal(fact).await? {
                return Ok(LateFactAction::Keep);
            }
            // 终态后晚到成本：不重开终态，只按既有成本缺口口径补到 Attempt 的成本四列上。
            // 没有成本事实但有计量证据，说明请求确实发出过，成本按"拿不到"落缺口；两者都没有
            // 时不碰成本列。补录失败时返回错误、不消费，等下一轮再试，避免把成本丢掉。
            let late_cost = fact.provider_cost.clone().or_else(|| {
                fact.evidence
                    .is_some()
                    .then_some(unavailable_provider_cost())
            });
            if let Some(cost) = late_cost {
                self.record_terminal_late_cost(fact, &cost).await?;
            }
            return Ok(LateFactAction::Consumed);
        };
        // 收件只说明上游给过什么，不能凭"有 usage"推断成功：没有成功终态就没有向消费者收费的
        // 依据。缺失或未知一律保留占用并进对账；但已核实的上游成本照记——带证据与张数时按冻结
        // 单价能算出来，就不能把它降级成"拿不到"（Spec 0005 §5、RFC 0018 §7）。
        if fact.provider_state != Some(seeai_domain::ProviderTaskState::Succeeded) {
            let provider_cost = match (&fact.provider_cost, fact.evidence.as_ref()) {
                (Some(cost), _) => cost.clone(),
                (None, Some(evidence)) => {
                    let images = fact
                        .image_count
                        .map(|count| usize::try_from(count).unwrap_or(usize::MAX));
                    self_computed_late_cost(&execution.price_snapshot, &evidence.usage, images)
                }
                (None, None) => failure_provider_cost(&execution.price_snapshot, None),
            };
            self.reconcile_unknown(execution, fact.attempt_id, Some(provider_cost), report)
                .await?;
            return Ok(LateFactAction::Consumed);
        }
        let Some(evidence) = fact.evidence.as_ref() else {
            // 没有计量证据不结算；成本事实照落缺口并转对账。
            let provider_cost = fact
                .provider_cost
                .clone()
                .or_else(|| Some(failure_provider_cost(&execution.price_snapshot, None)));
            self.reconcile_unknown(execution, fact.attempt_id, provider_cost, report)
                .await?;
            return Ok(LateFactAction::Consumed);
        };
        let usage = evidence.usage.clone();
        let images = fact
            .image_count
            .map(|count| usize::try_from(count).unwrap_or(usize::MAX));
        let provider_cost = match &fact.provider_cost {
            Some(cost) => cost.clone(),
            None => self_computed_late_cost(&execution.price_snapshot, &usage, images),
        };
        // 收件事实带产出张数就用它；不带时按张计价算不出金额，成本落缺口，不拿 0 张顶替。
        let charge = execution.price_snapshot.charge_microusd(ChargeFacts {
            usage: &usage,
            images: images.unwrap_or(0),
            declared_cost_microusd: provider_cost.amount_microusd,
        });
        let Ok(charge_microusd) = charge else {
            self.reconcile_unknown(execution, fact.attempt_id, Some(provider_cost), report)
                .await?;
            return Ok(LateFactAction::Consumed);
        };
        let command = SettleExecution {
            job_id: fact.job_id,
            attempt_id: fact.attempt_id,
            execution_owner: self.worker_id.clone(),
            fencing_token: execution.fencing_token,
            evidence: evidence.clone(),
            provider_cost,
            charge_microusd,
            // 收件带来张数就记它；没带就是没带，留 NULL，不拿 0（RFC 0019 §5.3）。
            image_count: fact.image_count,
            provider_trace_id: fact.provider_trace_id.clone(),
        };
        match self.settle_with_confirmation(command).await {
            Ok(finalization) => {
                report.settled += 1;
                self.refresh_balance(execution.account_id).await;
                tracing::info!(
                    job_id = %fact.job_id,
                    stage = %finalization.stage,
                    charge_microusd = finalization.charge_microusd,
                    "a late accounting fact settled the execution; the image is discarded"
                );
                Ok(LateFactAction::Consumed)
            }
            Err(error) => {
                tracing::warn!(
                    job_id = %fact.job_id,
                    error = %error,
                    "the late accounting fact could not be settled; it stays unconsumed"
                );
                Ok(LateFactAction::Keep)
            }
        }
    }

    /// 句柄晚到事实：用只读查询确认任务终态；未到终态不 mark。
    async fn consume_task_handle_fact(
        &self,
        fact: &ClaimedLateFact,
        owned: &HashMap<JobId, TakenOverExecution>,
        report: &mut ReconciliationReport,
    ) -> Result<LateFactAction, ApplicationError> {
        if self.finalized_terminal(fact).await? {
            // 执行已经终结，这条晚到句柄不再有用。
            return Ok(LateFactAction::Consumed);
        }
        let Some(execution) = owned.get(&fact.job_id) else {
            // 不拥有：留给 TTL 重领，等接管之后再查。
            return Ok(LateFactAction::Keep);
        };
        let Some(handle) = fact.provider_task_handle.clone() else {
            return Ok(LateFactAction::Keep);
        };
        let query = match self
            .read_only_accounting(execution, &handle, fact.provider_trace_id.clone(), report)
            .await
        {
            QueryOutcome::Facts(query) => query,
            // 能力不支持、凭证或币种缺失：不伪造恢复，也不消费这条事实。
            QueryOutcome::Unsupported | QueryOutcome::Unavailable => {
                return Ok(LateFactAction::Keep);
            }
            QueryOutcome::Transient => return Ok(LateFactAction::Keep),
        };
        match query_disposition(query.state) {
            // 未到终态：不 mark，等下一轮重查。
            QueryDisposition::Retain => Ok(LateFactAction::Keep),
            QueryDisposition::Reconcile => {
                self.reconcile_unknown(execution, fact.attempt_id, None, report)
                    .await?;
                Ok(LateFactAction::Consumed)
            }
            QueryDisposition::Settle => {
                self.settle_query_facts(execution, fact.attempt_id, query.accounting_facts, report)
                    .await?;
                Ok(LateFactAction::Consumed)
            }
            QueryDisposition::Fail => {
                self.fail_query_task(execution, fact.attempt_id, query.accounting_facts, report)
                    .await?;
                Ok(LateFactAction::Consumed)
            }
        }
    }

    /// 这台执行是否已经收成终态（成功或确定失败）。
    async fn finalized_terminal(&self, fact: &ClaimedLateFact) -> Result<bool, ApplicationError> {
        Ok(self
            .executions
            .read_finalization(fact.job_id, fact.attempt_id)
            .await?
            .is_some_and(|finalization| finalization.stage.is_terminal()))
    }

    /// 把一条晚到成本补到已经收尾的 Attempt 上。端口只在四列为空或来源是 unavailable 时写入，
    /// 不重开终态、不覆盖已有真实成本；写失败时向上报错，让这条事实留在收件箱下一轮重试。
    async fn record_terminal_late_cost(
        &self,
        fact: &ClaimedLateFact,
        cost: &ProviderCostFact,
    ) -> Result<(), ApplicationError> {
        let written = self
            .executions
            .record_terminal_provider_cost(fact.job_id, fact.attempt_id, cost)
            .await?;
        if written {
            tracing::info!(
                job_id = %fact.job_id,
                attempt_id = %fact.attempt_id,
                source = cost.source.as_str(),
                "a late cost fact was added to a finished attempt without reopening it"
            );
        }
        Ok(())
    }

    /// 一次幂等收尾的「提交未知 → 只读确认 → 重试同一提交」骨架。
    ///
    /// 结算与失败处置只差提交端口与日志措辞：骨架负责有界重试与读确认，端口调用由 `submit`
    /// 闭包给出。闭包拿到的是**新的仓储句柄与命令克隆**（都归它返回的未来所有），所以未来不借
    /// 调用方、也不借捕获的局部变量，重试不会纠缠生命周期。
    async fn confirm_finalization<C, F, Fut>(
        &self,
        command: C,
        job_id: JobId,
        attempt_id: AttemptId,
        label: &str,
        submit: F,
    ) -> Result<ExecutionFinalization, ApplicationError>
    where
        C: Clone,
        F: Fn(Arc<dyn ExecutionRepository>, C) -> Fut,
        Fut: Future<Output = Result<ExecutionFinalization, ApplicationError>>,
    {
        let max_attempts = self.retry_policy.max_attempts.max(1);
        let mut last_error = None;
        for attempt in 1..=max_attempts {
            match submit(self.executions.clone(), command.clone()).await {
                Ok(finalization) => return Ok(finalization),
                Err(error) => {
                    tracing::warn!(
                        job_id = %job_id,
                        error = %error,
                        "the {} commit result is unknown; confirming the committed finalization",
                        label
                    );
                    last_error = Some(error);
                }
            }
            match self.executions.read_finalization(job_id, attempt_id).await {
                Ok(Some(finalization)) => return Ok(finalization),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(
                        job_id = %job_id,
                        error = %error,
                        "could not confirm the {} result",
                        label
                    );
                    last_error = Some(error);
                }
            }
            if attempt < max_attempts {
                tokio::time::sleep(self.retry_policy.backoff_for(attempt)).await;
            }
        }
        Err(last_error.unwrap_or_else(|| {
            ApplicationError::Reconciliation(format!(
                "job {job_id} {label} stayed unknown after bounded confirmation"
            ))
        }))
    }

    /// 结算提交结果未知时先只读确认，再重试同一幂等收尾；仍不明时报错交下一轮。
    async fn settle_with_confirmation(
        &self,
        command: SettleExecution,
    ) -> Result<ExecutionFinalization, ApplicationError> {
        let job_id = command.job_id;
        let attempt_id = command.attempt_id;
        self.confirm_finalization(
            command,
            job_id,
            attempt_id,
            "settle",
            |executions, command| async move { executions.settle(command).await },
        )
        .await
    }

    /// 失败处置的同一套确认：提交结果未知先读确认，再重试同一幂等处置。
    async fn fail_with_confirmation(
        &self,
        command: FailOrReconcileExecution,
    ) -> Result<ExecutionFinalization, ApplicationError> {
        let job_id = command.job_id;
        let attempt_id = command.attempt_id;
        self.confirm_finalization(
            command,
            job_id,
            attempt_id,
            "failure finalization",
            |executions, command| async move { executions.fail_or_reconcile(command).await },
        )
        .await
    }

    /// 提交后把数据库当前余额写穿缓存。读不到只记日志：缓存不是事实来源。
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

    /// 外发一条新协议执行上的平台侧告警；没配出口时一条也不发。
    async fn raise_execution_alert(
        &self,
        execution: &TakenOverExecution,
        failure_kind: ProviderFailureKind,
    ) {
        let Some(alerter) = &self.alerts else {
            return;
        };
        alerter
            .notify(PlatformAlert::execution(
                execution.job_id,
                execution.provider_kind.clone(),
                failure_kind,
            ))
            .await;
    }

    /// 这一轮要不要跑慢周期账务核对。
    fn slow_cycle_due(&self) -> bool {
        let every = self.policy.ledger_audit_every_rounds.max(1);
        let round = self.rounds.fetch_add(1, Ordering::Relaxed) + 1;
        round.is_multiple_of(every)
    }

    /// 慢周期账务核对：按增量窗口取账户，本地截断，只建案告警、不改账。
    async fn audit_ledgers(&self, report: &mut ReconciliationReport) {
        let accounts = match self
            .repository
            .accounts_updated_within(self.policy.ledger_audit_window)
            .await
        {
            Ok(accounts) => accounts,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "could not list recently updated accounts for the ledger audit"
                );
                return;
            }
        };
        for change in accounts
            .into_iter()
            .take(usize::try_from(self.policy.ledger_audit_limit).unwrap_or(usize::MAX))
        {
            report.ledger_accounts_audited += 1;
            if let Err(error) = self.ledger_auditor.audit_account(change.account_id).await {
                tracing::warn!(
                    account_id = %change.account_id,
                    error = %error,
                    "the ledger audit of one account failed"
                );
            }
        }
    }
}

/// 查询状态决定该走哪条收尾路径：只有渠道明确成功才允许结算，确定的失败/取消按确定失败释放，
/// 状态不可信时保留占用并进入对账，仍在跑则留给下一次重查。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueryDisposition {
    Retain,
    Settle,
    Fail,
    Reconcile,
}

fn query_disposition(state: seeai_adapter_sdk::ProviderTaskState) -> QueryDisposition {
    use seeai_adapter_sdk::ProviderTaskState;
    match state {
        ProviderTaskState::Pending => QueryDisposition::Retain,
        // 渠道终态不能确认（未列出的状态，或响应无法与句柄关联）：不猜、不结算，进入对账。
        ProviderTaskState::Unknown => QueryDisposition::Reconcile,
        ProviderTaskState::Succeeded => QueryDisposition::Settle,
        ProviderTaskState::Failed | ProviderTaskState::Cancelled => QueryDisposition::Fail,
    }
}

/// 一次只读查询的三种结局：拿到账务事实、渠道不支持、或不具备查询条件/查询失败。
enum QueryOutcome {
    Facts(seeai_adapter_sdk::AccountingQuery),
    Unsupported,
    Unavailable,
    Transient,
}

/// 拿不到金额的成本事实：来源 unavailable，四列不半填。用于"请求发出过但成本未知"的缺口。
fn unavailable_provider_cost() -> ProviderCostFact {
    ProviderCostFact {
        source: ProviderCostSource::Unavailable,
        amount_microusd: None,
        currency: None,
        cny_microusd: None,
    }
}

/// 收件事实的成本自算：没有随事实交回的金额时按计价形态算。
///
/// 按张计价必须有产出张数：收件事实带了就用它算；没带就是缺口，绝不用 0 张凑一个金额。
/// 其余形态不依赖张数（token 量、每次单价），按用量或单价算得出。
fn self_computed_late_cost(
    snapshot: &PriceSnapshot,
    usage: &TokenUsage,
    images: Option<usize>,
) -> ProviderCostFact {
    match snapshot.formula {
        PricingFormula::PerImage => match images {
            Some(count) => provider_cost_fact(
                snapshot,
                &ProviderCost::Computed,
                CostInputs::Succeeded {
                    usage,
                    images: count,
                },
            ),
            None => provider_cost_fact(snapshot, &ProviderCost::Unavailable, CostInputs::Failed),
        },
        _ => provider_cost_fact(
            snapshot,
            &ProviderCost::Computed,
            CostInputs::Succeeded {
                usage,
                images: images.unwrap_or(0),
            },
        ),
    }
}
