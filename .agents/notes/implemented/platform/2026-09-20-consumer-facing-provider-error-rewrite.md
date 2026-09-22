---
title: 渠道失败对客改写与平台侧失败清单
status: implemented
created: 2026-09-20
updated: 2026-09-20
approval: 用户在会话中明确输入「执行实现」；GitHub 上没有对应的授权语句记录，因此本条只陈述该授权的出处，不额外推定（工作项 [#7](https://github.com/dehuadong/seeaihub-server-next/issues/7)）
verification: 2026-09-20 本地执行：`cargo fmt --check`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`、`cargo test --workspace --all-features`、`node scripts/decisions/check.mjs` 全部通过；空库端到端 `cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1` **11 个用例全部通过**（含本次新增的 `channel_rejections_reach_consumers_as_platform_problems`）
---

# Agent Note：渠道失败对客改写与平台侧失败清单

## 问题

渠道失败的对客可见面是这条记录要处理的问题：渠道报的失败不再原样出现在消费者面前，消费者在 Job 上只能看到一个平台错误码（`platform_unavailable`、`outcome_unknown`、`content_rejected`，第三个目前是预留档，两个渠道都还没有已核实的审核类错误码）。语义按责任方判定：渠道因平台的参数、凭证、权限、额度或限流而拒绝即平台侧故障、不返回 4xx，只有消费者内容被拒才是消费者的 4xx，渠道 5xx 同样不透传，拿不准按平台侧处理。约束是渠道原始码与原文只留在内部（`generation.attempts.provider_error_code` / `provider_error_message`），消费者侧看不到渠道码、渠道原文或上游 trace id，平台欠费也不触发任何自动动作。

## 决定

渠道报的失败不再原样出现在消费者面前。消费者在 Job 上只能看到一个平台错误码：`platform_unavailable`、`outcome_unknown`、`content_rejected`（第三个目前是预留档——两个渠道都还没有已核实的审核类错误码）。

- **责任方决定语义**：渠道因平台的参数、凭证、权限、额度或限流而拒绝 ⇒ 平台侧故障，不返回 4xx；只有消费者内容被拒才是消费者的 4xx；渠道 5xx 同样不透传；拿不准按平台侧处理。规则与理由见 [`ADR-0017`](../../../../docs/adr/0017-provider-errors-are-rewritten-for-consumers.md)。
- **两层保证**：`PublicErrorCode` 三值类型 + 唯一派生函数 `public_error_code`；迁移 `0003_platform_failure_kind.sql` 归一历史行后加 CHECK，把 `jobs.error_code` 钉在白名单内——租约过期与对账退款这两个**纯 SQL 写点**也按同一映射写，不经过映射函数。
- **渠道码只留内部**：渠道原始码与原文写在 `generation.attempts.provider_error_code` / `provider_error_message`；Job 上只写平台码与平台侧文案，文案不含渠道原文。
- **失败类别随调用传出**：`ProviderFailureKind`（8 类）挂在 `ProviderCallError` 上，回答"这次失败算不算平台自己的事件"；它与 `RetrySafety` 正交（后者只管能不能重试、要不要进对账）。APIMart `409` 的幂等子类按第一方口径分：`in_progress` / `key_reused` 是"渠道拒了平台的请求"，`result_indeterminate` 拿不准，落 `Unknown`。
- **运营可发现面**：新增管理员只读接口 `GET /api/v1/provider-failures?kind=…&since=…&limit=…`（`kind` 逗号分隔、`limit` 默认 100 / 上限 500、不翻页），响应为 `{failures, count, truncated}`，每条带 job、账户、型号、Offering、渠道类别、失败类别、对客码、渠道原始码与原文（密钥片段已过滤）、上游标识与时间。**不传 `kind` 时只列平台侧事件**（渠道不可用、被限流、内容被拒要显式按类别查）。**平台欠费不触发任何自动动作**：路由启停由运营决定，代码不做自动路由变更。

- **与计划的差异（结论收敛）**：计划的"平台内部码固定映射"把 `result_delivery_failed` 与 `reconciliation_refunded` 都写成 `platform_unavailable`，但同一份计划的规则②（受理状态不明 → `outcome_unknown`）与迁移按 `state` 归一的口径都指向 `outcome_unknown`。按后两者收敛：**停在 `reconciliation_required` 的用 `outcome_unknown`**（结果可能已生成、只是取不回来），**退款结清后转 `failed` 并用 `platform_unavailable`**（已经处理完，不该再让消费者"等对账结论"）。两种码都在白名单内，对客都不含渠道信息。

## 验证

| 行为 | 证据 |
| --- | --- |
| 八个失败类别 × 三种 `RetrySafety` 都落在对客白名单内 | `every_failure_kind_stays_inside_the_public_error_whitelist` |
| 平台内部码（`adapter_rejected`、`worker_prepare_failed`、`credential_unavailable`、`adapter_configuration_failed`、`result_delivery_failed`、`worker_lease_expired`、`reconciliation_refunded`）逐个落白名单，且不说成"内容被拒" | `platform_internal_codes_stay_inside_the_public_error_whitelist` |
| 渠道场景逐行映射：欠费、凭证、以 500 承载的参数错误、限流、5xx、拿不准、受理不明（含 APIMart 的两个幂等子类）、内容被拒 | `channel_failures_are_reported_as_platform_problems` |
| `content_rejected` 只由消费者内容被拒产生 | `only_consumer_content_becomes_a_consumer_error` |
| 落库值与类型一一对应（与 DB CHECK 同一批字符串） | `stored_failure_kinds_and_public_codes_round_trip` |
| 缺省清单只覆盖平台侧事件 | `the_default_failure_list_covers_only_platform_side_events` |
| 渠道码留内部、对客码独立派生 | `adapter_failures_keep_the_channel_code_internal` |
| APIMart `409` 三类幂等子类按第一方口径分档 | `idempotency_subtypes_are_classified_by_the_first_party_criteria` |
| 管理端原文的密钥片段过滤（含括号未闭合时宁可截断） | `apps/api/src/main.rs` 的 `credential_fragments_are_removed_from_provider_text` 等三条 |
| **端到端**：假上游按 APIMart 402/403/500/503 与 AIHubMix 403 拒绝提交 ⇒ 消费者只看得到平台码、响应里没有渠道码/原文/上游 trace id；渠道原始码与上游标识留在 Attempt 上；运营面能按类别查到（含缺省只列平台侧），接口匿名 401、消费者 Key 403、未知类别 400 | `channel_rejections_reach_consumers_as_platform_problems` |
| 租约过期这类平台内部事件出现在 `?kind=platform_internal` 清单里，对客码是 `outcome_unknown` | `image_generation_http_contract` 中的 `verify_lease_recovery_contract` |
| 对账退款后对客码是 `platform_unavailable`（已结清，不再让消费者等对账）、类别是 `platform_internal`；对账清单语义没变 | `image_generation_http_contract` 中的 `verify_reconciliation_contract`、`post_acceptance_failure_keeps_the_task_id_for_reconciliation` |

<!-- agent-note-format: alternatives-not-recorded (pre-format Agent Note) -->

## 后果

- 运营今天**没有渠道启停接口**：`supply.channels.enabled` 没有写入路径，`publish_runtime` 每次新插渠道行都把 `enabled` 写死为 `true`。平台欠费时运营唯一可用的处置是重新发布该型号的候选集合、把对应 Offering 移出。"渠道启停"是一个待补的能力差距。
- APIMart 的 `429`、`409` 前两者、`503 idempotency_unavailable` 有第一方"未受理"依据，可以收窄成确定性拒绝。本项不动成败判定，该收窄单独处理（[#8](https://github.com/dehuadong/seeaihub-server-next/issues/8)）。
- 告警出口（平台欠费主动通知）不在本项范围。

## 依据与关联

决定正文由 [`ADR-0017`](../../../../docs/adr/0017-provider-errors-are-rewritten-for-consumers.md) 拥有；渠道错误码事实见 [`docs/facts/channel-facts.md`](../../../../docs/facts/channel-facts.md) §2.13 与 §3.10；接口与表结构索引同步在 [`docs/architecture.md`](../../../../docs/architecture.md)；工作项 [#7](https://github.com/dehuadong/seeaihub-server-next/issues/7)。
