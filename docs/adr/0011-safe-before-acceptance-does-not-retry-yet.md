---
status: accepted
---

# `SafeBeforeAcceptance` 暂不产生重试，映射为失败并释放预授权

`RetrySafety` 保持三态，Adapter 必须如实区分"可证明未受理""确定性拒绝""无法证明是否受理"——这是 Adapter 的对外语义，属于平台能力的一部分，不因为暂时不用就砍掉其中一态。本阶段 `SafeBeforeAcceptance` 与 `NotRetryable` 映射到同一处置：Job 进入 `failed` 并释放预授权，区别只保留在该 Attempt 的 `provider_error_code` 上，供将来启用重试时使用。

**为什么不是保守而是结构性的**：`generation.attempts` 有 `UNIQUE (job_id)`（一个 Job 只能容纳一个 Attempt），`JobState` 也没有从 `submitting` 回到 `accepted`/`leased` 的边，因此"同一 Job 内的安全重试"在当前数据模型里**不可表达**；启用它需要先改造 Attempt 模型（多 Attempt、租约与退避、取消语义、预授权是否跨 Attempt 保留）。这与 [0007](./0007-reconciliation-instead-of-automatic-retry.md) 一致：0007 允许连接前失败重试，本条只是在本阶段收窄，不推翻其安全原则。

**代价**：可证明未受理的失败也会终止 Job，可用性略降；换来的是不引入未经验证的重试状态机、不因重试重复产生上游成本。
