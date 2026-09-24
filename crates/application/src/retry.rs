//! 重投策略：**只在可证明上游没有受理**时才会有的第二次上游调用，以及两次之间等多久。
//!
//! **什么算"可证明未受理"**：Driver 已经在错误上报了 `RetrySafety::SafeBeforeAcceptance`
//! ——连不上、上传失败、参考图取不到、上游明确拒绝受理这类，上游**没开始计费**。平台不在这里
//! 另立一套判据：能用哪一态只有 Driver 手里的报文说得清，编排层拿到的就是那一态。反过来，
//! `AcceptanceUnknown`（超时、5xx、响应读不出）**一律不重投**，按既有口径进
//! `reconciliation_required`；`NotRetryable`（参数/凭证类确定性拒绝）也绝不重投。
//!
//! 这条纪律就是"不会为同一个请求付两次上游成本"的保证：**宁可进对账，也不重投**。
//!
//! 上限与退避基都是**运维取值**（见 [`RetryPolicy::from_env`]）：同一个上游在不同时间段的
//! 抖动程度差得远，写死在代码里就只能靠改代码调；而重投次数直接关系到"最坏情况下一个请求会
//! 占用对客同步窗口多久"，这个数必须由运营按自己的上游与窗口定。

use std::{env, time::Duration};

use crate::ApplicationError;

/// 一次用户请求最多允许的上游调用次数（含第一次），默认值。运维取值。
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;

/// 第一次重投前等多久（毫秒），默认值。运维取值。
pub const DEFAULT_BACKOFF_BASE_MS: u64 = 1_000;

/// 单次退避的**上限**（秒）：指数退避不许无限翻倍。
///
/// 它不是"运维可能需要改"的量，所以不单列一个环境变量：它是这条链的对客同步窗口（分钟级）
/// 的下界保护——退避涨到这个数以上时，重投已经不可能在窗口内完成，涨下去只会白占一条在飞
/// Job，对客结果还是超时。
pub const MAX_BACKOFF_SECONDS: u64 = 60;

/// 重投策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// 一次请求最多允许的上游调用次数（含第一次）。`1` 表示关掉重投。
    pub max_attempts: u32,
    /// 退避基：第 `attempt_no` 次执行失败后等 `基 × 2^(attempt_no-1)`，封顶
    /// [`MAX_BACKOFF_SECONDS`]。
    pub backoff_base: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            backoff_base: Duration::from_millis(DEFAULT_BACKOFF_BASE_MS),
        }
    }
}

impl RetryPolicy {
    /// 第 `attempt_no` 次执行**失败之后**要等多久才重投。
    ///
    /// 指数退避而不是固定间隔：可证明未受理的失败里，连不上与上游拒绝受理这两种的恢复时间
    /// 差着量级——固定间隔要么对前者太密（一直打在还没恢复的上游上），要么对后者太疏（白白
    /// 占着对客窗口）。`saturating_mul` 与封顶都要有：指数在次数大时会溢出，而超过封顶的等待
    /// 与封顶的等待对结果没有区别。
    #[must_use]
    pub fn backoff_for(&self, attempt_no: u32) -> Duration {
        let doublings = attempt_no.saturating_sub(1).min(31);
        let millis = self
            .backoff_base
            .as_millis()
            .saturating_mul(1_u128 << doublings)
            .min(u128::from(MAX_BACKOFF_SECONDS) * 1_000);
        Duration::from_millis(u64::try_from(millis).unwrap_or(u64::MAX))
    }

    /// 还需要重投吗：已经用掉的执行次数（`attempt_no`）没到上限就还能再来一次。
    ///
    /// 只回答"还有额度吗"，**不回答"这次失败该不该重投"**——后者看的是失败分类
    /// （`RetrySafety::SafeBeforeAcceptance`），那一态只有 Driver 给得出来。
    #[must_use]
    pub fn allows_another_attempt(&self, attempt_no: u32) -> bool {
        attempt_no < self.max_attempts
    }

    /// 从环境变量读。
    ///
    /// 两个变量都是运维取值：
    /// - `GENERATION_RETRY_MAX_ATTEMPTS`（默认 [`DEFAULT_MAX_ATTEMPTS`]，必须 ≥ 1）
    /// - `GENERATION_RETRY_BACKOFF_BASE_MS`（默认 [`DEFAULT_BACKOFF_BASE_MS`]，毫秒）
    ///
    /// 取值不合法时报配置错误、不让进程起来：`0` 次执行意味着一条请求一次上游都不调，那是把
    /// 服务关掉而不是"少重试一次"，静默接受它只会让故障看起来像上游的问题。
    pub fn from_env() -> Result<Self, ApplicationError> {
        let max_attempts = env_number("GENERATION_RETRY_MAX_ATTEMPTS", DEFAULT_MAX_ATTEMPTS)?;
        if max_attempts == 0 {
            return Err(ApplicationError::Configuration(
                "GENERATION_RETRY_MAX_ATTEMPTS must be at least 1".to_owned(),
            ));
        }
        let backoff_base = env_number("GENERATION_RETRY_BACKOFF_BASE_MS", DEFAULT_BACKOFF_BASE_MS)?;
        Ok(Self {
            max_attempts,
            backoff_base: Duration::from_millis(backoff_base),
        })
    }
}

fn env_number<T>(name: &str, default: T) -> Result<T, ApplicationError>
where
    T: std::str::FromStr,
{
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => value
            .trim()
            .parse()
            .map_err(|_| ApplicationError::Configuration(format!("{name} must be an integer"))),
        _ => Ok(default),
    }
}

#[cfg(test)]
mod tests;
