//! 同步网关执行协议的最小事实：协议版本、Job/Attempt 阶段与执行所有权。
//!
//! 新旧协议共用 generation.jobs / generation.attempts，用 ExecutionProtocol 区分：
//! 旧协议由 Worker 领取生成队列，新协议由 API 在内存里直接执行。两套阶段名互不复用，
//! 存储列直接存字符串；新协议记录只保存最小执行与账务事实，不保存请求或响应业务载荷。
//!
//! 规则见 docs/specs/0005-synchronous-image-gateway.md 的 §2–§6 与
//! docs/design/0017-synchronous-image-gateway.md 的 §3、§5。

use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};

use crate::DomainError;

/// 执行记录的协议版本。
///
/// 它决定这条记录按哪套阶段与收尾规则解释，也决定旧 Worker 能不能领取它——
/// 新协议记录不落在旧协议的可领取条件里。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionProtocol {
    /// 切换前的旧协议：API 建 Job、Worker 领取执行，正文与结果信封随 Job 落库。
    Legacy,
    /// 同步网关协议：API 内存直接执行，记录只留最小事实。
    V1,
}

impl ExecutionProtocol {
    /// 落库取值，与 generation.jobs.execution_protocol 的 CHECK 约束同一份取值面。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::V1 => "v1",
        }
    }

    /// 从落库取值还原。数据库 CHECK 保证取值面；解析不到说明存储被绕过，按错误处理。
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "legacy" => Some(Self::Legacy),
            "v1" => Some(Self::V1),
            _ => None,
        }
    }
}

impl Display for ExecutionProtocol {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 新协议 Job 阶段：受理 → 执行中 → 终态（成功 / 失败 / 需对账）。
///
/// 与旧的 JobState 分开：旧协议有 leased（Worker 租约）这个阶段，新协议没有领取这回事，
/// 只有 API 自己持有的执行所有权；两套阶段名不复用同一批取值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStage {
    /// 已原子受理：最小 Job、预授权与容量事实已提交，尚未发出外部请求。
    Admitted,
    /// 已写下提交声明并授权外部发送。
    Executing,
    /// 已取得有效计量证据并按冻结快照结算。
    Succeeded,
    /// 确定失败且占用已按事实处置。
    Failed,
    /// 受理、结果或结算仍不确定，保留原处置与占用，等待对账。
    ReconciliationRequired,
}

impl ExecutionStage {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admitted => "admitted",
            Self::Executing => "executing",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::ReconciliationRequired => "reconciliation_required",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "admitted" => Some(Self::Admitted),
            "executing" => Some(Self::Executing),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "reconciliation_required" => Some(Self::ReconciliationRequired),
            _ => None,
        }
    }

    /// 对账态可以再收成成功或失败（晚到证据仍按同 Attempt 与幂等结算收尾）；
    /// 其余转移只允许沿受理 → 执行 → 终态单向走。
    pub fn transition(self, next: Self) -> Result<Self, DomainError> {
        let allowed = matches!(
            (self, next),
            (Self::Admitted, Self::Executing)
                | (Self::Admitted, Self::Failed)
                | (Self::Admitted, Self::ReconciliationRequired)
                | (Self::Executing, Self::Succeeded)
                | (Self::Executing, Self::Failed)
                | (Self::Executing, Self::ReconciliationRequired)
                | (Self::ReconciliationRequired, Self::Succeeded)
                | (Self::ReconciliationRequired, Self::Failed)
        );
        if allowed {
            Ok(next)
        } else {
            Err(DomainError::InvalidExecutionStageTransition {
                from: self,
                to: next,
            })
        }
    }

    /// 终态只含成功与失败：对账态不是终态，终态时刻与用量归属都不按它落。
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed)
    }
}

impl Display for ExecutionStage {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 新协议 Attempt 阶段。
///
/// 关键区分是「提交声明已写下」与「上游已受理」：崩溃后 Submitting 与「正在发」不可区分，
/// 只有拿到可信 task/trace 标识并入库才进 Accepted；两者都不能凭「没有句柄」判定未受理。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptStage {
    /// 已写下提交声明、尚未发出外部请求。
    Prepared,
    /// 提交声明已提交、外部请求可能已发出或正在发。
    Submitting,
    /// 上游已受理，可信 task/trace 标识已入库。
    Accepted,
    /// 本次尝试已确定收尾（成功或确定失败）。
    Terminal,
    /// 提交结果未知，禁止自动重提。
    Unknown,
}

impl AttemptStage {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Submitting => "submitting",
            Self::Accepted => "accepted",
            Self::Terminal => "terminal",
            Self::Unknown => "unknown",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "prepared" => Some(Self::Prepared),
            "submitting" => Some(Self::Submitting),
            "accepted" => Some(Self::Accepted),
            "terminal" => Some(Self::Terminal),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Terminal)
    }
}

impl Display for AttemptStage {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 执行所有权的 fencing token：每次提交与持久收尾都核验它。
///
/// **续约只延期所有权，不改变 token；只有接管让 token 加一**：续约在同一所有者名下把
/// lease_expires_at 推到之后，接管才在数据库里比较并交换所有权。旧 token 不得再改所有权、
/// 查询或扣费，但已经收到的提交响应与证据仍可在独立收尾预算内交付（见 RFC 0017 §5 的
/// offer_late_facts）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FencingToken(u64);

impl FencingToken {
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }

    /// 接管一次，单调加一；溢出即错误，不静默回绕。
    ///
    /// 续约不走这里——它只延期租约、保留原 token，调用它会把同一次所有权误报成一次接管。
    pub fn next(self) -> Result<Self, DomainError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(DomainError::ArithmeticOverflow)
    }
}

impl Display for FencingToken {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[cfg(test)]
mod tests;
