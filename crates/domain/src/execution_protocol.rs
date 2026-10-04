//! 同步网关执行的最小事实：Job/Attempt 阶段与执行所有权。
//!
//! 记录落在 generation.jobs / generation.attempts：由 API 在内存里直接执行，图片与请求参数只在
//! 内存里经过。阶段名与 JobState 互不复用，存储列直接存字符串；记录只保存最小执行与账务事实，
//! 不保存请求或响应业务载荷。
//!
//! 规则见 docs/specs/0005-synchronous-image-gateway.md 的 §2–§6 与
//! docs/design/0017-synchronous-image-gateway.md 的 §3、§5。

use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};

use crate::DomainError;

/// Job 阶段：受理 → 执行中 → 终态（成功 / 失败 / 需对账）。
///
/// 没有"领取"这个阶段：执行所有权由发起执行的 API 进程自己持有。
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

/// Provider 任务句柄与 trace 标识的字节上限。
pub const MAX_PROVIDER_IDENTIFIER_BYTES: usize = 128;

/// Provider 任务句柄或 trace 标识是不是有界标识。
///
/// 句柄会进入持久层、对账查询与告警：URL、data URL、控制字符、任意正文与超长值都不是标识，
/// 必须在写入前拒绝，否则业务载荷会从"任务标识"这个口子回到数据库（Spec 0005 §2、RFC 0018 §6）。
#[must_use]
pub fn is_bounded_provider_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_PROVIDER_IDENTIFIER_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
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

/// Provider 任务的**供应商状态**：查询与后续晚到事实共用同一判据。
///
/// 只判"能不能按证据向消费者结算"不够：失败与取消是确定的终态，必须释放占用并按失败成本口径
/// 记录，绝不能因为捎带返回了计量就被当成成功（Spec 0005 §5、ADR 0006）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderTaskState {
    /// 渠道仍在执行：不结算、不释放、不建案，按原预算继续只读查询。
    Pending,
    /// 渠道确认成功：这是唯一允许按计量证据正式结算的状态。
    Succeeded,
    /// 渠道确认失败：按确定失败释放消费者占用，实收为零；上游声明的成本照记。
    Failed,
    /// 渠道确认取消：处置同失败。
    Cancelled,
    /// 状态缺失或不可信：不结算、不释放、不按成功处理；保留占用，由后续重查或对账处置。
    Unknown,
}

impl ProviderTaskState {
    /// 存储与线路上的稳定表示。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
        }
    }

    /// 解析稳定表示；未列出的取值返回 `None`，由调用方按"不可信"处理。
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}
