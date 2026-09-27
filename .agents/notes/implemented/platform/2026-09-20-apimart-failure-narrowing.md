---
title: APIMart 失败判定收窄：有第一方依据的三类改为"可证明未受理"
status: implemented
created: 2026-09-20
updated: 2026-09-20
approval: 用户在会话中同意执行本项（"#8 收窄判定可以执行"）；GitHub 上没有对应的授权语句记录，本条只陈述该授权的出处，不额外推定（工作项 [#8](https://github.com/dehuadong/seeaihub-server-next/issues/8)）
verification: 2026-09-20 本地执行：`cargo fmt --check`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`、`cargo test --workspace --all-features` 全部通过；空库端到端 `cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1` **11 个用例全部通过**（其中提交拒绝表新增 429 / 409 `idempotency_in_progress` / 409 `idempotency_result_indeterminate` 三行、并改判 503 `idempotency_unavailable` 一行，同时断言预授权"失败即释放、对账即保留"）
---

# Agent Note：APIMart 失败判定收窄：有第一方依据的三类改为"可证明未受理"

## 问题

APIMart 有三类创建阶段的失败——`429` 限流、`409` 的两个幂等子类、`503 idempotency_unavailable`——第一方明文写明请求未执行、未受理，此前却被判为受理状态不确定、送进人工对账。把其实已受理的请求判成失败等于平台白付一次生成，因此收窄只取有第一方明文依据的组合，且只发生在任务尚未受理的创建阶段。AIHubMix 的 `429` / `503` 没有同样的未受理承诺，结论不能跨渠道套用。

## 决定

APIMart 有三类创建阶段的失败，第一方明文写明"请求未执行 / 未受理"，此前却被判为"受理状态不确定"送进人工对账。现在这三个**组合**按 `SafeBeforeAcceptance` 处理（Job `failed` + 释放预授权）：

- `429` 限流；
- `409` + `idempotency_in_progress` / `idempotency_key_reused`；
- `503` + `idempotency_unavailable`（第一方原文"当前请求未执行"）。

状态码与文本**不配对**（例如 `500` 却带 `idempotency_unavailable`）不算依据：第一方只承诺了上面三个组合。

**只在创建请求这一处收窄**（新增 `narrow_submit_rejection`，与既有的 `upload_failure`、`after_acceptance` 并列）：轮询、取图与上传阶段的同名状态码都不适用——那时任务已经受理，判成失败会让平台白付一次生成。`409 idempotency_result_indeterminate`、普通 `503`、`500`、`502` 与超时**不动**（第一方没有"未受理"承诺，`result_indeterminate` 还明确要求停止自动重试）。

失败**类别**保持原口径不变：`503 idempotency_unavailable` 仍归"渠道不可用"，`409` 的两个子类才归"渠道拒了平台的请求"——收窄只改"进不进对账"，不改平台侧归类。

顺带修正一处保真度问题：`error.code` 为**非空字符串**时（幂等子类就长这样）此前会被丢掉、退化成 `type` 或 `http_409`，现在原样保留，只对空串保留原有的降级规则。

AIHubMix 的 `429` / `503` 不在此列：它没有第一方"未受理"承诺，不能跨渠道套用结论（[`docs/facts/channel-facts.md`](../../../../docs/facts/channel-facts.md) 的 AIHubMix 错误码节）。

## 验证

| 行为 | 证据 |
| --- | --- |
| `429` / `409` 两个幂等子类 / `503 idempotency_unavailable` 收窄为 `SafeBeforeAcceptance`（且收窄前确实是"不确定"；`429` 只看状态码，错误体给不出标识符时也收窄） | `submit_rejections_that_prove_no_execution_are_narrowed` |
| 没有第一方依据的一律保持原判：`result_indeterminate`、普通 `503`、`500`、`402`，以及状态码与文本不配对的组合 | `submit_rejections_without_a_first_party_basis_stay_unknown` |
| 已受理之后的同名状态码（轮询、取图）永远不收窄 | `post_acceptance_failures_are_never_narrowed` |
| 非空字符串 `error.code` 原样保留 | `submit_rejections_that_prove_no_execution_are_narrowed` |
| 失败类别保持原口径：`409` 的两个子类归"渠道拒了平台的请求"，`503 idempotency_unavailable` 仍归"渠道不可用" | `idempotency_subtypes_are_classified_by_the_first_party_criteria` |
| AIHubMix 的 `429` / `503` 仍是"不确定" | `provider_error_marks_rate_limit_as_unknown_without_acceptance_proof`、`provider_error_marks_service_unavailable_as_unknown` |
| **不对客可见的处置一致性**（默认测试套件内）：可证明未受理与确定性拒绝都落到 `failed` + 释放预授权 | `adapter_failures_keep_the_channel_code_internal` |
| **端到端**：假上游按上述状态拒绝提交 ⇒ Job `failed`、对客码 `platform_unavailable`、预授权已释放；`409 result_indeterminate` 仍进对账且预授权保留；**同一个 `429` 在 AIHubMix 上仍进对账**（不跨渠道套用） | `channel_rejections_reach_consumers_as_platform_problems` |

<!-- agent-note-format: alternatives-not-recorded (pre-format Agent Note) -->

## 后果

- **判错的代价由平台承担**：把"其实已受理"判成失败＝白付一次生成。控制手段是"只改有第一方明文依据的三类 + 逐条反向用例 + 收窄只发生在创建阶段"。
- **没有做付费实测**：`#8` 方案里提过"真实触发一次 429 或核对上游账单"，两者都需要计费调用、且 429 无法在不骚扰上游的前提下稳定触发，因此本项**只依据第一方文档**，未做计费验证。若将来要实证，最便宜的一次是"同一 idempotency key 提交两次"（一次生成的成本量级）。
- 本项只改"进不进对账"，没动重试；`SafeBeforeAcceptance` 后来在额度内产生重投——现行处置见 [`ADR-0011`](../../../../docs/adr/0011-safe-before-acceptance-does-not-retry-yet.md)，翻转掉的旧结论见 [`ADR-0011 的修订史`](./2026-09-27-adr-0011-retry-reversal.md)。

## 依据与关联

`docs/adr/0011`（三态语义与当前映射）、`docs/adr/0007`（失败关闭与对账）、[`docs/facts/channel-facts.md`](../../../../docs/facts/channel-facts.md) 的 APIMart 错误码节（第一方口径）；工作项 [#8](https://github.com/dehuadong/seeaihub-server-next/issues/8)。
