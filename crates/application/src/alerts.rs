//! 平台故障告警出口：把**平台侧事件**外发出去，而且**只是旁路**。
//!
//! 出口由配置决定（配了才有），这一层只认端口 [`AlertSink`]：怎么送、送到哪由基础设施实现。
//! 这里负责"什么时候发、发什么"以及"发送失败绝不回传"——[`PlatformAlerter::notify`] 的返回类型是
//! `()`，调用点拿不到错误，因此不可能因为一次外发失败改写 Job 的处置、结算或对客结果。
//!
//! 外发的内容是**定位所需的最小集**（[`PlatformAlert`]）：告警离开仓库之后落在别人手里，所以
//! 凭证、提示词与图片一律不进去。
//!
//! 不做告警平台、不做聚合与静默期——那是接入方的事。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use seeai_domain::{AccountId, JobId};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use crate::{ApplicationError, ProviderFailureKind};

/// 一条外发的平台侧告警：**给定位所需的最小集**，字段名就是它的线上表示。
///
/// 两种形态共用一个出口：**某次执行**上的平台侧事件（[`ExecutionAlert`]），与**一次账实核对
/// 发现的不符**（[`LedgerMismatchAlert`]）。后者不是执行，所以它不借用前者的字段——一条对不上的
/// 余额既说不出 `job_id`，也说不出失败类别，硬塞进去只会让收到告警的人去查一个不存在的东西。
///
/// 序列化是 `untagged` 的：一种形态一个对象，加一种形态**不改**另一种的线上表示。
///
/// 它刻意不带凭证、提示词与图片：告警外发到仓库之外，对客内容不出门。要多带一个字段之前先问
/// 一句"收到它的人靠这条字段能不能做成一件事"——不能就不加。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(untagged)]
pub enum PlatformAlert {
    Execution(ExecutionAlert),
    LedgerMismatch(LedgerMismatchAlert),
}

/// 某次执行上的平台侧事件：失败本身，或这次失败把 Job 推进了对账。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ExecutionAlert {
    pub job_id: JobId,
    /// 渠道类别（例如 AIHubMix / APIMart）。
    pub provider_kind: String,
    pub failure_kind: ProviderFailureKind,
    pub occurred_at: DateTime<Utc>,
}

/// 一次账实核对发现的不符：某个账户**当前值**与它**自己的明细**对不上。
///
/// 四条数就是定位所需：哪个账户、账本说是多少、余额说是多少、active 预授权合计是多少、占用合计
/// 说是多少。差额是两边之差，收到的人自己会算，所以不另发差额字段。它**不带**"谁对谁错"——这条
/// 任务只发现不符，改账是人的决定。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LedgerMismatchAlert {
    pub account_id: AccountId,
    /// `ledger.entries` 按账户求和的符号金额。
    pub ledger_total_microusd: i64,
    /// `ledger.accounts` 那一行上的已结算余额。
    pub balance_microusd: i64,
    /// `ledger.holds` 里该账户 active 行的金额之和。
    pub holds_total_microusd: i64,
    /// `ledger.accounts` 那一行上的占用合计。
    pub held_microusd: i64,
    pub occurred_at: DateTime<Utc>,
}

impl PlatformAlert {
    /// 新协议（v1）执行上的那条告警：没有 [`GenerationJob`]，渠道类别由对账 Worker 从只读
    /// 投影读到的渠道事实给出。时间取**观测到它的时刻**。
    #[must_use]
    pub fn execution(
        job_id: JobId,
        provider_kind: String,
        failure_kind: ProviderFailureKind,
    ) -> Self {
        Self::Execution(ExecutionAlert {
            job_id,
            provider_kind,
            failure_kind,
            occurred_at: Utc::now(),
        })
    }

    /// 一次账实不符外发的那条告警。时间同样是**观测到它的时刻**。
    #[must_use]
    pub fn ledger_mismatch(
        account_id: AccountId,
        ledger_total_microusd: i64,
        balance_microusd: i64,
        holds_total_microusd: i64,
        held_microusd: i64,
    ) -> Self {
        Self::LedgerMismatch(LedgerMismatchAlert {
            account_id,
            ledger_total_microusd,
            balance_microusd,
            holds_total_microusd,
            held_microusd,
            occurred_at: Utc::now(),
        })
    }

    /// 日志里定位这条告警的那一行：执行告警给 Job 与失败类别，账实不符给账户与四个数。
    ///
    /// **按形态给它自己的字段名**，执行告警沿用原来那几个（`job_id` / `provider_kind` /
    /// `failure_kind`）——运营的日志查询是按它们写的；账实不符那条没有 Job，于是给账户与
    /// 四个数（流水合计、已结算余额、占用合计、持有中）。
    /// 这些字段只由载荷自己的字段拼出来：日志与线上表示说的是同一件事。
    fn log_delivered(&self, delivered: u64) {
        match self {
            Self::Execution(alert) => tracing::debug!(
                job_id = %alert.job_id,
                provider_kind = alert.provider_kind.as_str(),
                failure_kind = alert.failure_kind.as_str(),
                delivered,
                "platform alert delivered"
            ),
            Self::LedgerMismatch(alert) => tracing::debug!(
                account_id = %alert.account_id,
                ledger_total_microusd = alert.ledger_total_microusd,
                balance_microusd = alert.balance_microusd,
                holds_total_microusd = alert.holds_total_microusd,
                held_microusd = alert.held_microusd,
                delivered,
                "platform alert delivered"
            ),
        }
    }

    /// 送不出去的那条日志（字段同上）。
    fn log_undelivered(&self, failed: u64, error: &ApplicationError) {
        match self {
            Self::Execution(alert) => tracing::warn!(
                job_id = %alert.job_id,
                provider_kind = alert.provider_kind.as_str(),
                failure_kind = alert.failure_kind.as_str(),
                failed,
                error = %error,
                "platform alert could not be delivered"
            ),
            Self::LedgerMismatch(alert) => tracing::warn!(
                account_id = %alert.account_id,
                ledger_total_microusd = alert.ledger_total_microusd,
                balance_microusd = alert.balance_microusd,
                holds_total_microusd = alert.holds_total_microusd,
                held_microusd = alert.held_microusd,
                failed,
                error = %error,
                "platform alert could not be delivered"
            ),
        }
    }
}

/// 告警的发送端口。
///
/// 返回 `Err` 表示**这一次没送出去**。实现方可以有界重试，但失败不必吞掉——收口在
/// [`PlatformAlerter`]：它把失败记成日志与计数，不往上抛。
#[async_trait]
pub trait AlertSink: Send + Sync {
    async fn send(&self, alert: &PlatformAlert) -> Result<(), ApplicationError>;
}

/// 出口的累计计数：只给日志与观察用，不参与任何处置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlertCounters {
    pub delivered: u64,
    pub failed: u64,
}

/// 告警出口：把一条告警交给端口，并**吞掉**它的失败。
///
/// 这是"发送失败不阻塞主流程"的落点：失败只记日志与计数。调用点拿不到 `Result`，所以它没有
/// 机会让一次外发失败影响 Job 的处置、结算与对客结果。
pub struct PlatformAlerter {
    sink: Arc<dyn AlertSink>,
    delivered: AtomicU64,
    failed: AtomicU64,
}

impl PlatformAlerter {
    #[must_use]
    pub fn new(sink: Arc<dyn AlertSink>) -> Self {
        Self {
            sink,
            delivered: AtomicU64::new(0),
            failed: AtomicU64::new(0),
        }
    }

    /// 外发一条告警。**没有返回值**：发送失败在这里收口。
    pub async fn notify(&self, alert: PlatformAlert) {
        match self.sink.send(&alert).await {
            Ok(()) => {
                let delivered = self.delivered.fetch_add(1, Ordering::Relaxed) + 1;
                alert.log_delivered(delivered);
            }
            Err(error) => {
                let failed = self.failed.fetch_add(1, Ordering::Relaxed) + 1;
                alert.log_undelivered(failed, &error);
            }
        }
    }

    #[must_use]
    pub fn counters(&self) -> AlertCounters {
        AlertCounters {
            delivered: self.delivered.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests;
