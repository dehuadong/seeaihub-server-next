//! 账实核对：周期比对**账本汇总**与**账户余额**，不一致就告警并建一条对账案例。
//!
//! **它不是缓存对账**（[`crate::AccelerationService::reconcile_once`]）：那个问的是"缓存里的值
//! 还是不是库里的值"，做法是以数据库为准把缓存覆盖回去——改的是缓存，两边是同一个事实的两份
//! 副本。这条问的是"库里那个余额，还是不是它自己那本账的和"：两边**都是数据库里的事实**，缓存
//! 不参与，也不存在"以谁为准覆盖谁"。两条都会在不一致时留下痕迹，但只有这一条能发现"钱和账
//! 本身对不上"。
//!
//! **只发现，不改账**：不一致时只外发一条平台侧告警、建一条对账案例。余额与账本的任何改动都是
//! 人的决定——自动把余额"修正"成账本的和，等于把一个原因未知的错悄悄换成一个结论已知的错，
//! 而那个被抹掉的差额正是要查的东西。

use seeai_domain::AccountId;
use std::{env, sync::Arc, time::Duration};

use crate::{ApplicationError, HubRepository, PlatformAlert, PlatformAlerter};

/// 一次账实不符：某个账户**行上的余额**与它自己**账本条目的符号和**对不上。
///
/// 两个数都取自数据库：`ledger.accounts.balance_microusd` 与 `ledger.entries.amount_microusd`
/// 按账户求和。哪一边是"对的"不在这里判定——这条任务只发现不符。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedgerBalanceMismatch {
    pub account_id: AccountId,
    /// `ledger.entries` 按账户求和的符号金额。
    pub ledger_total_microusd: i64,
    /// `ledger.accounts` 那一行上的余额。
    pub balance_microusd: i64,
}

impl LedgerBalanceMismatch {
    /// 外发用的那条告警：字段就取这三个数加一个观测时刻。
    #[must_use]
    pub fn alert(&self) -> PlatformAlert {
        PlatformAlert::ledger_mismatch(
            self.account_id,
            self.ledger_total_microusd,
            self.balance_microusd,
        )
    }
}

/// 给一个对不上的账户建一条对账案例。
///
/// 它**没有 Job、也没有 Attempt**：被核对的是某个账户的余额与它的账本，不是某一次执行。案例的
/// 状态沿用既有那两个取值（`open` / `resolved`），这里不新造状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenLedgerCaseCommand {
    pub account_id: AccountId,
    pub ledger_total_microusd: i64,
    pub balance_microusd: i64,
}

/// 账实核对的周期。
///
/// 它是**运维取值**，不是产品档位：一轮核对要扫过全部账户与它们的账本，跑多勤取决于账本有多大、
/// 运维多想早点发现不符。给默认值只是让"没配"也能跑起来，不是推荐值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedgerAuditPolicy {
    pub interval: Duration,
}

impl LedgerAuditPolicy {
    /// 默认周期：15 分钟一轮。核对是兜底审计，不是热路径——账实不符不会自己好，晚十几分钟发现
    /// 与早十几分钟发现的处置方式一样，而每轮都要扫全表。
    pub const DEFAULT_INTERVAL_MS: u64 = 900_000;

    pub fn new(interval: Duration) -> Result<Self, ApplicationError> {
        if interval.is_zero() {
            return Err(ApplicationError::Configuration(
                "the ledger audit interval must be positive".to_owned(),
            ));
        }
        Ok(Self { interval })
    }

    /// 从环境变量读这一层：`LEDGER_AUDIT_ENABLED` 是**开关**（默认开），
    /// `LEDGER_AUDIT_INTERVAL_MS` 是周期（默认见 [`Self::DEFAULT_INTERVAL_MS`]）。
    ///
    /// 关掉时返回 `None`：这一层**根本不挂起来**，不是"挂起来但什么都不做"——一个跑着却不干活
    /// 的循环会让人觉得它在看着账。两个值都是运维取值。
    pub fn from_env() -> Result<Option<Self>, ApplicationError> {
        if !audit_flag_env("LEDGER_AUDIT_ENABLED", true)? {
            return Ok(None);
        }
        let interval_ms = match env::var("LEDGER_AUDIT_INTERVAL_MS") {
            Ok(value) if !value.trim().is_empty() => value.trim().parse::<u64>().map_err(|_| {
                ApplicationError::Configuration(
                    "LEDGER_AUDIT_INTERVAL_MS must be an integer".to_owned(),
                )
            })?,
            _ => Self::DEFAULT_INTERVAL_MS,
        };
        Self::new(Duration::from_millis(interval_ms)).map(Some)
    }
}

/// 读一个布尔开关。只认 `true`/`false`（大小写不敏感）与 `1`/`0`：写错了就**响亮失败**，
/// 不让一个拼错的开关悄悄按默认值走。
fn audit_flag_env(name: &str, default: bool) -> Result<bool, ApplicationError> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => match value.trim().to_ascii_lowercase().as_str() {
            "true" | "1" => Ok(true),
            "false" | "0" => Ok(false),
            other => Err(ApplicationError::Configuration(format!(
                "{name} must be true or false, got {other}"
            ))),
        },
        _ => Ok(default),
    }
}

/// 一轮核对的结果。只给日志与测试看，不参与任何处置。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LedgerAuditReport {
    /// 这一轮发现的对不上账户数。
    pub mismatches_found: u64,
    /// 其中**新建**了案例的几个（已经有未结案案例的不算，见 [`LedgerAuditor::audit_once`]）。
    pub cases_opened: u64,
}

/// 账实核对：把**账本汇总**与**账户余额**比一遍，对不上的账户告警并建案。
pub struct LedgerAuditor {
    repository: Arc<dyn HubRepository>,
    policy: LedgerAuditPolicy,
    /// 告警出口：**配了才有**。没配时照样发现、照样建案，只是不外发——发现不一致本身不依赖
    /// 有没有接收器。
    alerts: Option<Arc<PlatformAlerter>>,
}

impl LedgerAuditor {
    #[must_use]
    pub fn new(repository: Arc<dyn HubRepository>, policy: LedgerAuditPolicy) -> Self {
        Self {
            repository,
            policy,
            alerts: None,
        }
    }

    #[must_use]
    pub fn with_alerts(mut self, alerter: Arc<PlatformAlerter>) -> Self {
        self.alerts = Some(alerter);
        self
    }

    /// 跑一轮：把对不上的账户报出来。
    ///
    /// 每个对不上的账户做两件事，别的什么都不做：**建案**（已经有未结案案例时数据库会把这次插入
    /// 挡掉，那一轮就不再建第二条），以及**只有真建了新案时**才外发一条告警。之所以不是"每轮都
    /// 告警"：这与既有的那条口径一致——新建一条对账案例才是一条平台侧事件；一个长期没人修的
    /// 账户每轮刷一条告警，只会让收告警的人学会忽略它。发现本身每轮都有日志。
    ///
    /// 这条路径**不写**余额与账本：改账是人的决定（模块头）。
    pub async fn audit_once(&self) -> Result<LedgerAuditReport, ApplicationError> {
        let mismatches = self.repository.accounts_with_ledger_mismatch().await?;
        let mut report = LedgerAuditReport {
            mismatches_found: u64::try_from(mismatches.len()).unwrap_or(u64::MAX),
            cases_opened: 0,
        };
        for mismatch in mismatches {
            let opened = self
                .repository
                .open_ledger_reconciliation_case(OpenLedgerCaseCommand {
                    account_id: mismatch.account_id,
                    ledger_total_microusd: mismatch.ledger_total_microusd,
                    balance_microusd: mismatch.balance_microusd,
                })
                .await?;
            if opened {
                report.cases_opened += 1;
            }
            tracing::error!(
                account_id = %mismatch.account_id,
                ledger_total_microusd = mismatch.ledger_total_microusd,
                balance_microusd = mismatch.balance_microusd,
                case_opened = opened,
                "an account balance does not match its ledger entries; neither side was changed"
            );
            if opened {
                // 告警跟着**已提交的事实**走：案例先落库，再外发。外发失败收口在出口里，
                // 不影响这一轮，也不会回退刚建的那条案例。
                if let Some(alerter) = &self.alerts {
                    alerter.notify(mismatch.alert()).await;
                }
            }
        }
        Ok(report)
    }

    /// 定时循环：由进程在启动时挂起来，与请求路径无关。
    ///
    /// 第一轮立刻跑（`interval` 的第一次 tick 立即完成），之后每 `interval` 一轮。
    pub async fn run(self: Arc<Self>) {
        let interval = self.policy.interval;
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match self.audit_once().await {
                Ok(report) if report.mismatches_found > 0 => {
                    tracing::error!(
                        mismatches_found = report.mismatches_found,
                        cases_opened = report.cases_opened,
                        "the ledger and the account balances disagree"
                    );
                }
                Ok(_) => {}
                Err(error) => tracing::error!(error = %error, "the ledger audit failed"),
            }
        }
    }
}

#[cfg(test)]
mod tests;
