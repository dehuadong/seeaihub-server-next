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
use seeai_domain::{GenerationJob, JobId, JobState};
use std::{
    num::NonZeroU64,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use crate::{ApplicationError, AttemptFailure, ProviderFailureKind};

/// 一条外发的平台侧告警：**给定位所需的最小集**，字段名就是它的线上表示。
///
/// 它刻意不带凭证、提示词与图片：告警外发到仓库之外，对客内容不出门。要多带一个字段之前先问
/// 一句"收到它的人靠这条字段能不能做成一件事"——不能就不加。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PlatformAlert {
    pub job_id: JobId,
    /// 渠道类别（例如 AIHubMix / APIMart）。
    pub provider_kind: String,
    pub failure_kind: ProviderFailureKind,
    pub occurred_at: DateTime<Utc>,
}

impl PlatformAlert {
    /// 就这一次失败外发的那条告警。时间取**观测到它的时刻**：这是一条外发的运维消息，
    /// 与库里的业务时刻不是同一件事。
    #[must_use]
    pub fn of(job: &GenerationJob, failure_kind: ProviderFailureKind) -> Self {
        Self {
            job_id: job.id,
            provider_kind: job.offering.provider_kind.clone(),
            failure_kind,
            occurred_at: Utc::now(),
        }
    }
}

/// 这次失败**本身**是不是平台侧事件：第 1、2 个触发条件看的就是它。
///
/// 平台欠费 / 凭证类失败用**既有的失败类别**判定，不另立一套类别；对账案例新增看的是终态——
/// 把 Job 写成 `reconciliation_required` 就是建案的那条路径，与失败类别无关。
#[must_use]
pub fn is_platform_event(failure: &AttemptFailure) -> bool {
    matches!(
        failure.kind,
        ProviderFailureKind::PlatformFunding | ProviderFailureKind::PlatformCredential
    ) || failure.target_state == JobState::ReconciliationRequired
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
        let subject = (
            alert.failure_kind.as_str(),
            alert.provider_kind.as_str(),
            alert.job_id,
        );
        match self.sink.send(&alert).await {
            Ok(()) => {
                let delivered = self.delivered.fetch_add(1, Ordering::Relaxed) + 1;
                tracing::debug!(
                    job_id = %subject.2,
                    provider_kind = subject.1,
                    failure_kind = subject.0,
                    delivered,
                    "platform alert delivered"
                );
            }
            Err(error) => {
                let failed = self.failed.fetch_add(1, Ordering::Relaxed) + 1;
                tracing::warn!(
                    job_id = %subject.2,
                    provider_kind = subject.1,
                    failure_kind = subject.0,
                    failed,
                    error = %error,
                    "platform alert could not be delivered"
                );
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

/// Worker 上的告警出口：**配了才有**。
///
/// 没配时调用点连"这个候选连续失败了几次"都不去读——那是告警才需要的判据，为它每次都查一次库
/// 等于让一条旁路给主流程加成本。
pub struct PlatformAlertExit {
    alerter: Arc<PlatformAlerter>,
    consecutive_failures: NonZeroU64,
}

impl PlatformAlertExit {
    #[must_use]
    pub fn new(alerter: Arc<PlatformAlerter>, consecutive_failures: NonZeroU64) -> Self {
        Self {
            alerter,
            consecutive_failures,
        }
    }

    /// 数连续失败时往回看多少次：阈值是 N，就只需要看最近 N 次终态。
    #[must_use]
    pub fn window(&self) -> u32 {
        u32::try_from(self.consecutive_failures.get()).unwrap_or(u32::MAX)
    }

    /// 某候选连续失败到这个次数才外发。它是**配置项**，不是常量。
    #[must_use]
    pub fn threshold(&self) -> u64 {
        self.consecutive_failures.get()
    }

    pub async fn notify(&self, alert: PlatformAlert) {
        self.alerter.notify(alert).await;
    }
}

#[cfg(test)]
mod tests;
