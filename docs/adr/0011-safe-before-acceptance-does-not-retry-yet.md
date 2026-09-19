# `SafeBeforeAcceptance` 暂不产生重试：本阶段映射为失败并释放预授权

`RetrySafety` 保持三态，Adapter 必须如实区分「可证明未受理」「确定性拒绝」「无法证明是否受理」——这是 Adapter 的对外语义，随代码发布，属于平台能力的一部分，因此不因为暂时不用而砍掉其中一态。

但本阶段 `SafeBeforeAcceptance` **不触发重试**，与 `NotRetryable` 映射到同一处置：Job 进入 `failed` 并释放预授权。区别只保留在该 Attempt 的 `provider_error_code` 上，供将来启用重试时使用。

**为什么不是保守，而是结构性的**：`generation.attempts` 有 `UNIQUE (job_id)`，一个 Job 只能容纳一个 Attempt；`JobState` 也没有从 `submitting` 回到 `accepted`/`leased` 的边。因此「同一 Job 内的安全重试」在当前数据模型里**不可表达**。启用它需要先改造 Attempt 模型（多 Attempt、租约与退避、取消语义、预授权是否跨 Attempt 保留），那是独立的架构变更。

**这与 [0007](./0007-reconciliation-instead-of-automatic-retry.md) 的关系**：0007 写「只有明确的连接前失败可以重试」，本 ADR 是对该条在本阶段的**收窄**，不是推翻其安全原则。仓库里确实已存在一条真实的「安全重来」通道——`recover_expired_leases` 把 `leased` 且租约过期的 Job 退回 `accepted`，而把已进入 `submitting` 的 Job 送往对账——它体现的正是「租约过期早于提交才可安全重来」。本阶段的决定与之一致：不确定是否已提交时，绝不自作主张。

**代价**：可证明未受理的失败（例如明确的连接前失败）也会终止 Job，而不是自动重来，可用性略降。收益是不引入未经验证的重试状态机，也不会因为重试而重复产生上游成本。该缺口是**跨 Provider 的既有缺口**，不是某个 Provider 特有的问题。

**事实更正（2026-09-19）**：上一句原写「当前没有任何 Adapter 产生 `SafeBeforeAcceptance`」，现在已不成立——APIMart Driver 在**提交生成任务之前**的资产上传（`POST /v1/uploads/images`，见 `docs/facts/channel-facts.md` §3.7）失败时就是这一态：那次失败只能推出"生成任务可证明未受理"。处置映射不变（同样 `failed` + 释放预授权），本 ADR 的决策本身不变。这也顺带说明三态不是空话：分类如实，将来放开重试时才有可用的信号。

**来源**：第二阶段 Planning（`dehuadong/seeaihub-server-next#2`）。放开该分类不需要改 Adapter 接口，只需放开处置映射并完成 Attempt 模型改造。
