---
title: 模型类型：事实落点与用量展示口径
status: implemented
created: 2026-10-04
updated: 2026-10-05
approval: 用户 2026-10-04 在讨论中确认模型加 type（image/video/chat）、由素材声明、对外客户端据此判断用量单位，并明确不按类型做发布闸门、账单与用量按类型分开给量而不相加；合同与设计随后接受（Spec 0006 v1 生效、设计 0020 v1 已接受），执行实现获授权
verification: 迁移回填 the_model_type_migration_backfills_existing_vendor_models；素材导入 a_material_must_declare_a_known_type、importing_the_same_material_twice_adds_no_rows、changing_the_type_of_an_existing_revision_is_rejected、a_material_without_a_type_is_rejected_without_writing_a_row；发布归一 normalize_rejects_an_inline_publication_without_a_known_type；接口合同 cases_model_type.rs 六条；账务读 cases_billing/cases_pricing/cases_cost_facts；浏览器 portal-history.spec.ts 六条；cargo check --workspace --all-targets、cargo fmt --all -- --check、npm run typecheck、npm run build、node scripts/decisions/check.mjs 通过
---

# Agent Note：模型类型：事实落点与用量展示口径

## 问题

[用量记录页](../../../../apps/web/src/portal/pages/Usage.tsx)的用量列写死「张数」，事实只有 `generation.jobs.image_count` 一处；[账单汇总](../../../../apps/web/src/portal/pages/Billing.tsx)同样只有一个产出图片数。仓库当前的网关模型只有图片模型（[素材](../../../../config/bootstrap/)两家渠道、OpenAI `gpt-image-2.5` 两款），接到视频与对话模型后秒数与 token 数会被画成张数。对客目录 `GET /v1/models` 也没有任何字段告诉客户端这个模型是什么类型。

## 决定

行为由[模型类型 Spec](../../../../docs/specs/0006-model-type-and-usage-records.md)拥有，取数与展示由[模型类型设计](../../../../docs/design/0020-model-type.md)拥有。术语「模型类型」记在[词汇表](../../../../CONTEXT.md)。本记录保存选择理由、备选与后果，不重复合同。

类型挂在 Vendor Model 上，与合同同层同生命周期，来源是工程侧的发布素材。理由：类型是上游模型自身的事实（`gpt-image-2.5` 就是图片模型），不是运营的定价或打包选择；素材已经在声明这个模型的合同，类型与它同类。运营的发布页只做「选模型、排候选、给价」，多一个手填字段就多一处可以填错而与模型实际不符的地方。

用量记录与账单汇总的类型都不给 Job 加列：Job 已经冻结 `vendor_model_id`，而 Vendor Model 行按身份不可变，读取时连过去拿到的就是受理当时那一行。这样网关模型以后改绑到别的 Vendor Model 也不会污染历史记录，同时省掉一次迁移与回填。用量事实（张 / 秒 / token）由各自执行路径在结算时写落点，读取只装配，不重算。

用量按类型分开给：图片给张数，视频给秒数，对话给输入与输出 token；账单汇总是同一区间内各类型各自的合计，不合并成单个数。张、秒、token 是三种量，相加得不到有意义的数；分开之后每一列都能与逐条记录对上。

## 备选方案

- **类型挂在网关模型上、由运营发布时选**：对客目录这一层看着更直接，但同一份渠道事实可能被包成多个网关模型，运营要为同一个模型重复填同一个类型，且填错时平台无从发现。落选。
- **类型由 `capability_schema` 或驱动推断**：不加字段，从合同里有没有 `n`、`image` 或驱动名反推类型。视频与对话模型同样会有 `prompt`，`image` 参数也不是图片模型独有；推断规则会随新模型不断打补丁。落选。
- **按类型做发布闸门**：只允许发布今天已有执行路径的类型。类型不参与受理与目录判据，一个供给齐备的模型不会因为类型而不可调，闸门只是把新类型的接入与发布耦合起来。落选。
- **把不同类型的量相加成单个数**：账单只给一个「用量」数。张、秒、token 单位不同，相加得不到有意义的数，也没法与逐条记录对上。落选。
- **给 `generation.jobs` 加类型列**：与已冻结的供给身份同级，读起来不连表。写入时多一处可能与 Vendor Model 不一致的副本，且要迁移与回填。落选。

## 后果

- 视频秒数与对话 token 的落点在其执行路径落地前不存在，非图片类型的记录只能显示占位。类型字段先就位是有意的：客户端据此判断用量单位的规则不必等执行路径。
- 类型取值收在三种，将来接入音频或其他模态要加迁移与取值面；取值面收敛是刻意的，不预留没有事实支撑的取值。
- 类型不是计费口径。同一条渠道按 token 计量量计价的图片模型（[AIHubMix 素材](../../../../config/bootstrap/gpt-image-2.5-flare.json)）类型仍是 `image`，用量记录显示张数，收费仍按对客计价形态与冻结快照计算；两者混起来会让客户以为按张扣费。

## 验证

- 迁移回填与列形状：`apps/api/tests/http_contract/cases_migrations.rs` 的 `the_model_type_migration_backfills_existing_vendor_models`。
- 素材导入：`crates/persistence/src/material_import/tests.rs` 的 `a_material_must_declare_a_known_type`；`crates/persistence/tests/material_import_idempotency.rs` 的 `importing_the_same_material_twice_adds_no_rows`、`changing_the_type_of_an_existing_revision_is_rejected`、`a_material_without_a_type_is_rejected_without_writing_a_row`。
- 发布归一：`crates/application/src/tests/publish_normalize.rs` 的 `normalize_rejects_an_inline_publication_without_a_known_type`。
- 接口合同：`apps/api/tests/http_contract/cases_model_type.rs`（目录 `type`、内联类型校验、改类型被拒、改绑后类型不变、video 记录 `usage` 为空、账单只按类型给量）。
- 账务读回归：`cases_billing.rs`、`cases_pricing.rs`、`cases_cost_facts.rs`。
- 浏览器：`apps/web/e2e/portal-history.spec.ts` 的「用量与账单按模型类型显示：缺量的类型显示占位而不是 0」。
- 静态与构建：`cargo check --workspace --all-targets`、`cargo fmt --all -- --check`、`npm run typecheck --prefix apps/web`、`npm run build --prefix apps/web`、`node scripts/decisions/check.mjs`。
