---
title: 平台网关模型命名层：对客名与厂商原生名分离
status: implemented
created: 2026-09-22
updated: 2026-09-23
approval: 用户在会话中授权实施 P1（平台网关模型命名层）；范围与验收见提案 [#13](https://github.com/dehuadong/seeaihub-server-next/issues/13) 的 P1 工单 [#14](https://github.com/dehuadong/seeaihub-server-next/issues/14)
verification: 2026-09-22 本地：`cargo fmt --all` 无差异；`cargo clippy --workspace --all-targets --all-features -- -D warnings` exit 0；`cargo test --workspace --all-features` 全绿（各 crate 单测 22 / 29 / 2 / 3 / 54 / 44 通过，40 条端到端用例按设计 ignore）；空库端到端 `cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1` **40 passed / 0 failed**，含本次新增的命名层三条用例（零真实计费调用）。另做一次反向探针：临时去掉对客投射后 `gateway_model_naming_keeps_the_vendor_name_off_the_consumer_surface` 立刻失败（对客目录正文里出现了厂商原生名），确认用例钉得住这条规则、不是靠断言互相抵消过关。`node scripts/decisions/check.mjs` 通过。2026-09-23：夹具退役后 `legacy_material_without_a_gateway_name_falls_back_to_the_vendor_name` 的素材改为测试内构造，随空库端到端重跑 **72 passed / 0 failed**。
---

# Agent Note：平台网关模型命名层：对客名与厂商原生名分离

## 问题

发布时平台型号名直接取厂商原生名（发布事务里 `gateway_model: native_model_id.clone()`），两个角色在库里是同一个值。后果有两层：厂商原生名因此出现在对客目录与合同正文里；平台也没法把同一份供给包成另一个对外名字（换名字等于换型号）。本项把这两个角色真正拆开，范围只到**命名层与对客面**——定价、保底、权重、路由策略、缓存都不在本次交付里。

## 决定

- **对客名与厂商原生名分开**：发布命令新增顶层 `gateway_model`（**平台对客名**）。**名字由管理员发布时自己填，平台不预设、也不固定任何名字**；缺省（不写或只写空白）时回退取 `native_model_id`，因此现有素材、现有已发布数据与现有用例的形状逐位不变。
- **原子替换的对象是这个名字**：`publication.runtime_entries.gateway_model`、修订上新增的 `gateway_model` / `vendor_model_id` 两列都按对客名写。同一份供给因此可以包成两个网关模型，各自一份定义；重发一个名字只动它自己。
- **合同正文不动**：合同里的 `properties.model.const` 仍是厂商原生名（合同是厂商模型的身份，行不可变），只有**投射给调用方那一步**替换成对客名。发布期另加一条校验：该常量必须等于本次发布的 `native_model_id`——素材把对客名写进合同正文会被拒。
- **`GET /v1/models` 新形状**：`{data:[{name, vendor_id, revision, contract}]}`。`name` 是网关模型名、`vendor_id` 是厂商标识（原名 `vendor`）、`revision` 仍是合同修订号、`contract` 是发布的那份（其中 `model.const` 已换成对客名）。厂商原生名不进对客面；目录仍公开、不校验 Key。
- **管理端读写两条**：`GET /api/v1/gateway-models`（管理员）是只读投影，一条网关模型一项——对客名、运维开关、厂商与原生名、这次发布的修订与时间、候选清单（供给、渠道类别、上游模型名、顺序、这条候选能不能走、承载面与映射），**不回显渠道凭证**；`PATCH /api/v1/gateway-models/{name}`（管理员）**只改 `enabled`** 并写审计事件，没发布过的名字是 404（定义只能由发布产生，这里不创建任何东西）。
- **新表 `publication.gateway_models`**：只放运维开关（`enabled` / 时间 / 改动人）。定义（候选集、合同、定价）只在不可变修订里，不存第二份。名字首次发布成功时由发布事务落一行，此后只由 PATCH 改。
- **受理与目录同一条判据**：`active_offering(gateway_model)` 也要求"网关模型开着"，因此关掉一个名字会同时让它从目录消失、受理得到"模型不存在"；已受理的 Job 不受影响（它们固定的是受理时那一版）。
- **迁移 `0007_gateway_model_naming.sql`**：修订两列按同一次发布的条目回填后设 `NOT NULL`；开关表按既有生效名字回填（`enabled = true`），既有已发布数据在迁移后立刻可读、可停用。历史 `runtime_entries.gateway_model` **不回填**（当时等于原生名，语义自洽；改历史行会破坏"Job 固定受理时版本"）。
- **命名层之外的两处收口**（同一变更内完成）：合同型号身份字段的位置（`properties.model.const`）抽成领域层一个助手，发布期校验、对客投射与端到端用例的期望值三处共用，替换语义只定义一次（"合同没声明这个字段时不动它"）；"这条候选能不能走"（供给启用且渠道启用）收成持久层唯一一份 SQL 片段，对客目录、受理取候选、管理端候选的 `enabled` 三处共用。

## 验证

| 行为 | 证据 |
| --- | --- |
| 对客名与原生名不同时：目录 `name` 是对客名、带 `vendor_id`、**响应全文不含原生名**（含合同正文） | `gateway_model_naming_keeps_the_vendor_name_off_the_consumer_surface` |
| 用对客名能受理（假上游跑通），用厂商原生名是"模型不存在" | 同上 |
| 存的那份合同不动：库里 `model.const` 仍是原生名，只有投射给调用方时替换 | 同上 |
| 运维开关一关，目录与受理同时消失，管理端照样列得出来；PATCH 未发布过的名字 404 | 同上 |
| 素材把对客名写进合同正文被发布期拒掉 | 同上 |
| 不写对客名的命令发布后目录与受理与今天逐位一致 | `legacy_material_without_a_gateway_name_falls_back_to_the_vendor_name` |
| 迁移后既有已发布数据立刻可读、可停用；修订两列在既有行上非空 | `gateway_model_naming_migration_backfills_existing_publications` |
| 目录形状只有 `name` / `vendor_id` / `revision` / `contract`，且不含内部词汇 | `the_model_catalog_lists_only_callable_models_with_their_published_contract` + `assert_public_only` |
| 两条候选共享一份合同、候选与顺序按对客名生效 | `the_2_5_materials_route_by_carrier_surface_and_wire_names`、`stage_two_bootstrap_material_publishes_one_contract_with_per_candidate_carriers` |
| 对客名的回退与空白处理（不写、给值、只写空白三种） | `the_gateway_name_falls_back_to_the_vendor_name_when_absent`、`a_publish_must_declare_a_gateway_name` |

<!-- agent-note-format: alternatives-not-recorded (pre-format Agent Note) -->

## 后果

- **候选的 `weight` 不在本片**：权重属路由策略层，库里也没有这一列，因此管理端候选清单只给顺序（`routing_priority`），不给权重。
- **定价、保底与售价快照不在本片**：修订上的定价列不落，管理端响应里也没有 `pricing`。
- **命名层留了两道今天走不到的守卫**：发布期"对客名不得为空白"与发布事务里"候选必须同值"。名字目前只有一个来源（命令顶层那一个字段），因此正常路径到不了它们；保留是为了让"同一次发布只定义一个网关模型"这条要求在形状变化时（例如允许候选各自报名）先被拦住，而不是先写进去再发现。
- **种子素材保持中性**：`config/bootstrap/*.json` 不写 `gateway_model`（它们是初始化种子与测试夹具），"对客名与厂商原生名不同"这条路由由端到端用例自己造素材来证明。
- **旧素材与新素材并存**：已发布的老修订仍按当时的对客名（等于原生名）受理；换名字要发新的发布，历史行不动。

## 依据与关联

- 设计：网关模型对象、对客目录形状与读写路径见 [`docs/design/0006`](../../../../docs/design/0006-gateway-models-and-consumer-surface.md) §1/§2 与 §1.6（存储改动与迁移）；目录的公开性与形状沿用 [`docs/design/0005`](../../../../docs/design/0005-vendor-model-contract-and-offering-mapping.md) §8.1。
- 决策：一次发布携带完整有序候选集、发布即原子替换见 [`ADR-0009`](../../../../docs/adr/0009-multiple-active-offerings-and-routing.md)；Job 受理时固定版本见 [`ADR-0003`](../../../../docs/adr/0003-postgresql-is-source-of-truth.md)；合同与承载面分层见 [`ADR-0015`](../../../../docs/adr/0015-vendor-model-contract-and-offering-parameter-mapping.md)。
- 迁移：[`0007_gateway_model_naming.sql`](../../../../migrations/0007_gateway_model_naming.sql)。
- 索引同步：[`docs/architecture.md`](../../../../docs/architecture.md) §3（对外接口）、§5（表）、§6（文件与迁移）。
- 上一层的合同与承载面分层见 [合同与承载面分层、映射声明化与对客目录](./2026-09-20-vendor-model-contract-and-offering-mapping.md)：本项在它之上把"对客名"从厂商原生名里分出来。
- 工作项：提案 [#13](https://github.com/dehuadong/seeaihub-server-next/issues/13) 的 P1 工单 [#14](https://github.com/dehuadong/seeaihub-server-next/issues/14)。
