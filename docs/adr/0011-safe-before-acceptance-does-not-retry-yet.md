---
status: accepted
---

# `SafeBeforeAcceptance` 在额度内重投；`AcceptanceUnknown` 与 `NotRetryable` 不重投

`RetrySafety` 保持三态，Adapter 必须如实区分"可证明未受理""确定性拒绝""无法证明是否受理"——这是 Adapter 的对外语义，属于平台能力的一部分。处置按三态分流：

- **`SafeBeforeAcceptance`**（可证明上游没有受理、还没开始计费）**重投**：同一 Job 内新起一次执行（`attempt_no` 递增），两次之间按指数退避等待，退避基与执行次数上限是**运维取值**（`GENERATION_RETRY_MAX_ATTEMPTS`、`GENERATION_RETRY_BACKOFF_BASE_MS`，单次退避有封顶）。预授权在重投期间原样保留，到成功结算或用尽额度失败时才释放；一次请求只结算一次。
- **`AcceptanceUnknown`**（超时、`5xx`、响应读不出）**绝不重投**：进 `reconciliation_required` 并保留预授权。提交阶段的"连不上"同样按这一态处理——传输层不能严格证明上游没有收到请求（超时与连接中断都可能发生在请求已经发出之后）。
- **`NotRetryable`**（参数/凭证类确定性拒绝）不重投：Job 失败并释放预授权——重投同一份请求只会得到同一个答复。

判据只有一条：能不能重投只由 Driver 报出的那一态决定，编排层不按状态码另猜一遍；**宁可进对账也不重投**，"不会为同一个请求付两次上游成本"这条纪律不变。这与 [ADR-0007](./0007-reconciliation-instead-of-automatic-retry.md) 一致。

**代价**：重投把可证明未受理的失败救回来，但一次请求占用对客同步窗口更久——次数与退避因此交给运营按上游抖动与窗口自定。

**修订史**：本条翻转掉的旧结论、它失效的原因与依据提交见 [`.agents/notes/implemented/platform/2026-09-27-adr-0011-retry-reversal.md`](../../.agents/notes/implemented/platform/2026-09-27-adr-0011-retry-reversal.md)。

**可行性备选有三条，都落选**（依据同上记录，提交 `4ecd36b` 与迁移 `0018`）：**维持旧结论**（`SafeBeforeAcceptance` 也直接失败并释放预授权）——可证明上游没有开始计费时放弃重投等于白丢可用性，而判据不需要放松就能安全重投；**把 `AcceptanceUnknown` 一并纳入重投**——传输层证不出上游没收到请求，重投可能为同一个请求付两次上游成本（`4ecd36b` 的用例把执行次数上限放宽到 5 次，仍断言只发生一次上游调用）；**把提交阶段的"连不上"纳入可重投**——超时与连接中断都可能发生在请求已经发出之后。
