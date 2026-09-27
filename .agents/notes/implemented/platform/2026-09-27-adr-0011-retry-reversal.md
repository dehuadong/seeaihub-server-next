---
title: ADR-0011 的修订史：可证明未受理的失败改为重投
status: implemented
created: 2026-09-27
updated: 2026-09-27
approval: 用户 2026-09-27 授权整理 ADR-0011 正文：现行处置留在 ADR，翻转前的结论与依据搬进本记录
verification: `node scripts/decisions/check.mjs` 通过；引用的提交 `4ecd36b`、`3a6f7d9` 与迁移 `0018` 已核对；重投行为的既有证据是 `4ecd36b` 记录的 `apps/api/tests/http_contract/cases_retry.rs` 五条用例（本次只改文档，未重跑）
---

# Agent Note：ADR-0011 的修订史：可证明未受理的失败改为重投

## 问题

[`ADR-0011`](../../../../docs/adr/0011-safe-before-acceptance-does-not-retry-yet.md) 原文的结论是"`SafeBeforeAcceptance` 与 `NotRetryable` 映射到同一处置（Job 进 `failed` 并释放预授权）"。2026-09-24 的实现把前者改成重投（提交 `4ecd36b`），随后按就地修订惯例在 ADR 里追加了一段"上文结论整体被本节取代"的叙述（提交 `3a6f7d9`）。那段叙述是变更过程，按 [`docs/AGENTS.md`](../../../../docs/AGENTS.md) 归本记录；ADR 正文写当前处置。

## 决定

ADR-0011 正文重写为当前处置（三态分流：可证明未受理在额度内重投、`AcceptanceUnknown` 绝不重投、`NotRetryable` 失败），旧结论与它失效的原因搬进本记录：

- **旧结论为什么不再成立**：它给的理由是结构性的——`generation.attempts` 有 `UNIQUE (job_id)`（一个 Job 只能容纳一次执行），`JobState` 也没有从 `submitting` 回到 `accepted`/`leased` 的边，因此"同一 Job 内的安全重投"在数据模型里不可表达。迁移 `0018` 去掉了那条唯一约束、加上 `attempt_no`（回填 1，并用 `UNIQUE (job_id, attempt_no)` 接住"同一 Job 内不许两次同号执行"），"这是第几次执行"因此可判定，重投变得可表达。
- **判据没有放松**：能不能重投仍然只由 Driver 报出的 `RetrySafety` 决定，编排层不按状态码另猜一遍；`AcceptanceUnknown`（超时、`5xx`、响应读不出）绝不重投，按既有口径进 `reconciliation_required`。"不会为同一个请求付两次上游成本"这条纪律不变。
- **一并确认的边界**：提交阶段的"连不上"仍按 `AcceptanceUnknown` 处理——传输层不能严格证明上游没有收到请求（超时与连接中断都可能发生在请求已经发出之后）。将来要把它纳入可重投，是**独立的一次判定变更、需要显式授权**，不是本条的延伸。
- **文件名保留旧 slug**：`0011-safe-before-acceptance-does-not-retry-yet.md` 与编号一起保留，旧链接可解析；标题已写成当前处置。

## 备选方案

- **维持旧结论（可证明未受理也直接失败）**：落选。可证明上游没有开始计费时放弃重投等于白丢可用性，而判据本身不需要放松就能安全重投。
- **把 `AcceptanceUnknown` 也纳入重投**：落选。传输层证不出上游没收到请求，重投可能为同一个请求付两次上游成本——`4ecd36b` 的用例把上限放宽到 5 次也断言只发生一次上游调用。
- **把提交阶段的"连不上"纳入可重投**：落选。超时与连接中断都可能发生在请求已经发出之后。

## 后果

- 一次请求占用对客同步窗口更久：退避是 `基 × 2^(attempt_no-1)`、单次退避有封顶，执行次数上限与退避基都是运维取值（`GENERATION_RETRY_MAX_ATTEMPTS`、`GENERATION_RETRY_BACKOFF_BASE_MS`）。
- 预授权跨执行保留，结算与释放只发生在最后一次成功或用尽额度失败时（`capture` 分录唯一 ⇒ 一次请求只结算一次）。
- 库层不再有"一个 Job 恰好一次上游调用"这条保证；同一性改由 `UNIQUE (job_id, attempt_no)` 表达。
- [`2026-09-20-apimart-failure-narrowing`](./2026-09-20-apimart-failure-narrowing.md) 里"`SafeBeforeAcceptance` 仍然不产生重试"那句已改为指向本记录。

## 验证

- `node scripts/decisions/check.mjs` 通过；本次改动只有 `docs/adr/` 与 `.agents/notes/`，未动代码，因此没有重跑 crate 测试。
- 重投行为的已有证据（提交 `4ecd36b` 的记录，本次未重跑）：`apps/api/tests/http_contract/cases_retry.rs` 的五条用例——可证明未受理的失败重投后成功、只结算一次、两行 `attempt_no`、hold 自始至终一行且最终 captured；不确定的失败绝不重投（上限放宽到 5 也是 `create_calls() == 1`）；确定性拒绝不重投；到上限即停（`attempt_no` 恰为 `[1,2]`、生成请求 0 次）。
- 迁移与库层事实用 `migrations/0018_attempt_retry.sql` 核对。
