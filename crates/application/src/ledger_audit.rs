//! 账实核对：按账户比对**账户当前值**与它自己的明细，不一致就建案并告警。
//!
//! **它不是缓存对账**（[`crate::AccelerationService::reconcile_once`]）：那个问的是"缓存里的值
//! 还是不是库里的值"，做法是以数据库为准把缓存覆盖回去——改的是缓存，两边是同一个事实的两份
//! 副本。这条问的是"库里那个当前值，还是不是它自己那本账的和"：两边**都是数据库里的事实**，缓存
//! 不参与，也不存在"以谁为准覆盖谁"。两条都会在不一致时留下痕迹，但只有这一条能发现"钱和账
//! 本身对不上"。
//!
//! **按账户触发，不默认全库重算**：核对一次只问一个账户的两条等式——
//! `balance = SUM(entries)` 与 `held = SUM(active holds)`；没有默认周期，也不扫全库
//! （`docs/contracts/0002-account-funds-and-reservations.md` §5）。触发入口是管理员按账户发起的核查，
//! 由后台任务执行，不在受理、结算或余额读取的请求路径上。
//!
//! **只发现，不改账**：不一致时只外发一条平台侧告警、建一条对账案例。余额与账本的任何改动都是
//! 人的决定——自动把余额"修正"成账本的和，等于把一个原因未知的错悄悄换成一个结论已知的错，
//! 而那个被抹掉的差额正是要查的东西。

use seeai_domain::AccountId;
use std::sync::Arc;

use crate::{ApplicationError, HubRepository, PlatformAlert, PlatformAlerter};

/// 一次账实不符：某个账户的**当前值**与它自己的**明细**对不上。
///
/// 四条数都取自数据库：`ledger.accounts` 的 `balance_microusd` / `held_microusd`，以及
/// `ledger.entries` 按账户的符号和与 `ledger.holds` 里 active 行的金额和。哪一边是"对的"
/// 不在这里判定——这条任务只发现不符。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedgerMismatch {
    pub account_id: AccountId,
    /// `ledger.entries` 按账户求和的符号金额。
    pub ledger_total_microusd: i64,
    /// `ledger.accounts` 那一行上的已结算余额。
    pub balance_microusd: i64,
    /// `ledger.holds` 里该账户 active 行的金额之和。
    pub holds_total_microusd: i64,
    /// `ledger.accounts` 那一行上的占用合计。
    pub held_microusd: i64,
}

impl LedgerMismatch {
    /// 余额与账本对不上。
    #[must_use]
    pub fn balance_differs(&self) -> bool {
        self.balance_microusd != self.ledger_total_microusd
    }

    /// 占用合计与 active 预授权对不上。
    #[must_use]
    pub fn held_differs(&self) -> bool {
        self.held_microusd != self.holds_total_microusd
    }

    /// 建案时写进 `reason` 的那句话：**哪条等式断了、两边的数各是多少**，两条都断就都写。
    #[must_use]
    pub fn reason(&self) -> String {
        let mut parts = Vec::new();
        if self.balance_differs() {
            parts.push(format!(
                "ledger entries total {} microusd does not match the account balance {} microusd",
                self.ledger_total_microusd, self.balance_microusd
            ));
        }
        if self.held_differs() {
            parts.push(format!(
                "active holds total {} microusd does not match the account held total {} microusd",
                self.holds_total_microusd, self.held_microusd
            ));
        }
        parts.join("; ")
    }

    /// 外发用的那条告警：四条数加一个观测时刻。
    #[must_use]
    pub fn alert(&self) -> PlatformAlert {
        PlatformAlert::ledger_mismatch(
            self.account_id,
            self.ledger_total_microusd,
            self.balance_microusd,
            self.holds_total_microusd,
            self.held_microusd,
        )
    }
}

/// 给一个对不上的账户建一条对账案例。
///
/// 它**没有 Job、也没有 Attempt**：被核对的是某个账户的当前值与它的明细，不是某一次执行。案例的
/// 状态沿用既有那两个取值（`open` / `resolved`），这里不新造状态。`reason` 由
/// [`LedgerMismatch::reason`] 写清哪条等式断了。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenLedgerCaseCommand {
    pub account_id: AccountId,
    pub reason: String,
}

/// 一次按账户核对的结果。只给日志与测试看，不参与任何处置。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LedgerAuditReport {
    /// 这个账户的当前值与明细对不上。
    pub mismatch_found: bool,
    /// 对不上且**新建**了案例（已经有未结案案例时为 `false`，见 [`LedgerAuditor::audit_account`]）。
    pub case_opened: bool,
}

/// 账实核对：把一个账户的**当前值**与它自己的**明细**比一遍，对不上就告警并建案。
pub struct LedgerAuditor {
    repository: Arc<dyn HubRepository>,
    /// 告警出口：**配了才有**。没配时照样发现、照样建案，只是不外发——发现不一致本身不依赖
    /// 有没有接收器。
    alerts: Option<Arc<PlatformAlerter>>,
}

impl LedgerAuditor {
    #[must_use]
    pub fn new(repository: Arc<dyn HubRepository>) -> Self {
        Self {
            repository,
            alerts: None,
        }
    }

    #[must_use]
    pub fn with_alerts(mut self, alerter: Arc<PlatformAlerter>) -> Self {
        self.alerts = Some(alerter);
        self
    }

    /// 核对**一个账户**：把对不上的地方报出来。
    ///
    /// 账户对不上时做两件事，别的什么都不做：**建案**（已经有未结案案例时数据库会把这次插入挡掉，
    /// 那就不再建第二条），以及**只有真建了新案时**才外发一条告警。之所以不是"每次核对都告警"：
    /// 这与既有的那条口径一致——新建一条对账案例才是一条平台侧事件；一个长期没人修的账户反复
    /// 刷同一条告警，只会让收告警的人学会忽略它。发现本身每次都有日志。
    ///
    /// 这条路径**不写**余额、占用与账本：改账是人的决定（模块头）。
    pub async fn audit_account(
        &self,
        account_id: AccountId,
    ) -> Result<LedgerAuditReport, ApplicationError> {
        let Some(mismatch) = self.repository.account_ledger_mismatch(account_id).await? else {
            return Ok(LedgerAuditReport::default());
        };
        let opened = self
            .repository
            .open_ledger_reconciliation_case(OpenLedgerCaseCommand {
                account_id: mismatch.account_id,
                reason: mismatch.reason(),
            })
            .await?;
        tracing::error!(
            account_id = %mismatch.account_id,
            ledger_total_microusd = mismatch.ledger_total_microusd,
            balance_microusd = mismatch.balance_microusd,
            holds_total_microusd = mismatch.holds_total_microusd,
            held_microusd = mismatch.held_microusd,
            case_opened = opened,
            "an account does not match its own ledger entries or holds; neither side was changed"
        );
        if opened {
            // 告警跟着**已提交的事实**走：案例先落库，再外发。外发失败收口在出口里，
            // 不影响这一次核对，也不会回退刚建的那条案例。
            if let Some(alerter) = &self.alerts {
                alerter.notify(mismatch.alert()).await;
            }
        }
        Ok(LedgerAuditReport {
            mismatch_found: true,
            case_opened: opened,
        })
    }
}

#[cfg(test)]
mod tests;
