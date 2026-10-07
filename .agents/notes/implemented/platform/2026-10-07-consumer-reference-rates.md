---
title: 对客 token 初始价取供给声明的参考价目
status: implemented
created: 2026-10-07
updated: 2026-10-07
approval: 用户 2026-10-07 裁定：恢复 APIMart 候选的四档初始价（上一次针对 AIHubMix 的变更改到它，没有授权），并修掉 AIHubMix 候选落在不可选形态上的默认值；同日给出执行授权
verification: `cargo test -p seeai-persistence --lib`（13 条）、`cargo test -p seeai-persistence --test material_import_idempotency -- --ignored`（5 条，含参考价目就地更新）、`cargo test -p seeai-api --test http_contract -- --ignored --exact harness::cases_publication::the_offering_list_reports_the_channel_capabilities`（1 条）、`cargo clippy --workspace --all-targets --all-features -- -D warnings`、`cargo fmt --check`、`npm run e2e --prefix apps/web`（85 条）；端到端用例先红后绿（`platform-model-consumer-form.spec.ts`）
---

# Agent Note：对客 token 初始价取供给声明的参考价目

## 问题

AIHubMix 改走 `/ai/v1` 后只回上游声明的金额（[那次变更](2026-10-06-aihubmix-ai-v1-execution-path.md)），它的成本形态从 `token_rates` 变成 `upstream_declared`；而 Price Plan 是 `token_rates` 的成本参数，别的形态带着它导入期与发布期都拒，素材只能删掉那张四档表。那张表同时是"该 vendor／模型已知价目"的唯一来源，而按设计它与勾哪条候选无关——于是**同模型下按 token 四档卖的 APIMart 候选也一起失去了初始价**：一次针对 AIHubMix 的变更改到了 APIMart。

同一处还留着一个更早的缺口：发布页勾选候选时把对客形态硬编码成 `token_rates`，而能力过滤只作用在下拉的可选项上。AIHubMix 的能力反转后，那条候选的默认值落在不可选的形态上——界面显示原值、照渲染四个空的四档输入框，照原样发布会被前端或发布期拒。

## 决定

对客 token 四档的初始价改取供给声明的**对客参考价目**（`consumer_reference_rates`：渠道原币种四档 + 出处）：它不是成本参数，任何成本形态都可以声明，缺了也不影响发布；落 `supply.offerings` 那一列，素材导入与内联发布形状都写它，可选供给清单带出来（规则见[设计 0007 §2](../../../../docs/design/0007-pricing-floor-and-settlement.md)、[设计 0012 §3](../../../../docs/design/0012-platform-model-publishing.md)）。发布页的**对客形态默认值**同处收口：取这条通路可选的第一种（按 token 四档优先），改价载入的历史值也按这条收口。

## 备选方案

- **放宽 Price Plan 的语义**，允许声明金额的供给也挂一份四档费率当参考。否决：那正是"给了这种形态用不到的参数"——成本费率表在声明金额的渠道上永远不会被读，留着只会让人以为它在生效；参考价目与成本费率是两件事，各要一个名字。
- **把参考价目挂到 vendor 模型层**。否决：价目的出处是渠道的公开价目（谁家挂谁家），挂到模型层说不清它是谁的事实；模型层共享的语义由读侧按渠道名与模型名定序取一条来维持，与旧口径一致。
- **从已发布修订的对客 CNY 费率反推初始价**。不成立：首次发布没有上一版可推。

## 后果

- 参考价目**不进修订、也不进 Job 快照**：冻结的是由它推导出的对客 CNY 费率向量，参考价目只在发布页当默认值用。
- 内联发布的兼容形状对它是"省略即保留"（那条路是改价的形状）；素材导入直接覆盖（素材是这一列的属主）。
- APIMart 的供给定义逐字段未动，它的四档可选项与初始价恢复原状。

## 验证

- `cargo test -p seeai-persistence --lib`：13 条，含"参考价目与成本形态无关"（`upstream_declared` 可声明、缺省可、币种必须说得清）与计价形态和参数配套那条。
- `cargo test -p seeai-persistence --test material_import_idempotency -- --ignored`：5 条，含同一份素材导两遍不增行、改过参考价目的素材就地更新那一列。
- `cargo test -p seeai-api --test http_contract -- --ignored --exact harness::cases_publication::the_offering_list_reports_the_channel_capabilities`：清单带出参考价目，没声明的供给是 `null`。
- `npm run e2e --prefix apps/web`：85 条。其中 `platform-model-consumer-form.spec.ts` 两条覆盖默认形态跟随可选集合、照原样发布成功、初始价取参考价目与人民币预览、没有参考价目与缺折算率两句提示分开；改动前把默认形态临时改回恒为 `token_rates`，第一条在 `spec.ts:151` 以 `unexpected value "token_rates"` 失败（先红后绿）。
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`、`cargo fmt --check`、`node scripts/decisions/check.mjs`。
