---
title: 5xx 落一条带来源的日志
status: implemented
created: 2026-09-24
updated: 2026-09-24
approval: 工单 #29 的需求与期望已定（5xx 路径留一条带错误来源的日志，对客文案不泄漏细节，4xx 保持现状），仓库侧按该口径实现。
verification: `cargo test -p seeai-api --bin seeai-api`（5 passed，含本件两条；一条证明 500 留下带 category 与错误内容的 ERROR，一条证明两个 503 仍只走各自的 warn）；`cargo clippy -p seeai-api --all-targets --all-features -- -D warnings` 通过；`cargo fmt --all -- --check` 通过。
---

# Agent Note：5xx 落一条带来源的日志

## 问题

`ApiError` 的 `From<ApplicationError>` 对 5xx 一律回固定文案、**且不记日志**（只有两个 503 分支各有一条 `warn`）。后果是一次 `Persistence` / `Configuration` / `Reconciliation` 错误会变成**对客 500、仓库里没有任何痕迹**：排障只能靠测试里的聚合断言反推——限流那一件落地时就踩到过（一次 `sum` 解码失败只表现为 500）。

## 决定

- 5xx 兜底路径留一条 **ERROR**：带 `category`（`configuration` / `persistence` / `reconciliation`，一眼看出该去查哪一层）与 `error`（完整错误内容）。
- **平台侧故障（503 `platform_unavailable`）不重复记**：它已有更具体的 `warn`（"一条候选都承载不了"），再补一条 ERROR 会让同一个错误有两条日志，其中一条还说不清是哪一类。
- 对客文案与状态码一字不动：5xx 仍然只回 `The server could not complete the request`。4xx 不落这类日志——那是调用方自己的问题，数量由调用方决定。

## 备选方案

- **只记 `error = %error`，不带类别**：落选。运维看到一条"persistence error: ..."还得自己从文本里认出是哪一层；类别是结构化字段，聚合与告警都用得上。
- **给每个 5xx 变体各写一条日志**：落选。三个变体的处置完全相同（都是 500、都给同一句对客文案），分成三条只会让"5xx 有没有留下的痕迹"这件事变成三处要各自维护。
- **顺手把 4xx 也记上**：落选。4xx 的量由调用方决定（一次参数写错可以被重试风暴放大），把它记成服务端日志既刷屏又不指向平台侧问题。

## 后果

- 5xx 的排障入口是日志：`category` 直接指向层，`error` 是那条链上带上来的完整内容。
- 两个 503 的日志条数不变（仍各一条 `warn`），不会为了"统一"而多出一条说不清类别的 ERROR。
- 别处仍可能有无痕的失败（例如 Driver 侧的错误最终走的是 Job 的失败记录，不走这个映射）：本件只收口 `ApplicationError` 这一条路径。

## 验证

| 行为 | 证据 |
| --- | --- |
| 500 留下带类别与错误内容的 ERROR，对客文案不泄漏内部细节 | `a_server_error_is_logged_with_its_category_and_message`（`apps/api/src/tests.rs`，内存 writer 捕获 tracing 输出） |
| 两个 503 仍只走各自的 warn，不被兜底 ERROR 重复记 | `the_platform_side_failures_keep_their_own_warning`（同文件） |
| 门禁（本改动面） | `cargo fmt --all -- --check`、`cargo clippy -p seeai-api --all-targets --all-features -- -D warnings`、`cargo test -p seeai-api --bin seeai-api` |

## 依据与关联

- 需求与期望来自工单 #29（范围外发现的可观测性缺口，与 #24 的探活、#27 的告警出口同一类关注点）。
- 映射本身与两条已有的 warn：[`apps/api/src/main.rs`](../../../../apps/api/src/main.rs) 的 `From<ApplicationError> for ApiError`。
