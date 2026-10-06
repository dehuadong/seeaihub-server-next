主题: 平台网关模型、对客定价与运营后台（总览：网关模型与对客面）
状态: 待评审（原单文件 `0006-gateway-models-pricing-and-admin-console.md` 已按切片拆为本份与 `0007`/`0008`）
来源: 工作项「运营后台：平台网关模型、对客定价与路由权重」与提案 [#13](https://github.com/dehuadong/seeaihub-server-next/issues/13)；原单文件的 §1、§2、§8 与 §10 的总览部分
依赖: [`docs/design/0005`](./0005-vendor-model-contract-and-offering-mapping.md)（合同/承载面/映射分层）；`ADR-0003`、`ADR-0009`、`ADR-0015`、`ADR-0017`、`ADR-0020`

# 平台网关模型、对客定价与运营后台（总览：网关模型与对客面）

本文是运营后台工作的技术设计之一，承载**总览与对客面**：管理员用 API 定义平台网关模型 → 排候选顺序与权重 → 对客目录按网关模型名出牌 → 事后查得清。产品范围与验收归提案 [#13](https://github.com/dehuadong/seeaihub-server-next/issues/13)；改动点、迁移与验收清单归各切片工单（切片总表见 #13）；本文只承载设计决策与边界。

术语一律沿用 `CONTEXT.md`：**Gateway Model**（平台型号名，对外的 `model`）、**Vendor Model**（厂商的模型产品）、**Offering**（一条可调用供给）、**Channel**、**Runtime Revision**、**Price Plan**、**Price Snapshot**、**Routing Priority**、**Metering Evidence**。本主题新引入的字段级说法：**渠道成本**（`ADR-0006` 的"成本按渠道各自的口径取数"）、**加价系数**、**汇率**、**保底额**——理由见 [`0007`](./0007-pricing-floor-and-settlement.md)；**策略**、**折扣率**与**账户标签**——理由见 [`0008`](./0008-routing-strategy-and-caching.md)。

## 0. 本主题的三份设计分工

| 文档 | 负责 |
| --- | --- |
| **本份 `0006`** | **总览与对客面**：平台名 vs vendor 原生名的角色拆分、`/v1/models` 形状（`vendor_id`）、管理员读写路径（整份发布 + `GET /api/v1/gateway-models` + `PATCH enabled` + 查看余额）、对客面边界、本主题不做的事 |
| [`0007`](./0007-pricing-floor-and-settlement.md) | **定价、保底与结算**：成本两态（`Computed` / `Declared`）与渠道币种（`cost_currency`）、对客费率向量（`consumer_rates_cny`）、`floor_amounts` 保底表、hold 与透支（含迁移放宽三处 CHECK）、结算与毛利、与钱相关的记录与日志字段 |
| [`0008`](./0008-routing-strategy-and-caching.md) | **路由策略层与缓存**：`priority` / `weight`、`route_policies` 结构（含 `tag_channel_map`）、三条硬约束、与 `ADR-0009` / `ADR-0020` 的关系、Redis 路由与余额缓存 |

三份的来源都是拆分前的单文件 `docs/design/0006-gateway-models-pricing-and-admin-console.md`（**文件名已停用，内容已迁走**）；节号对照见提案评论中的评审记录。

## 1. 网关模型对象

### 1.1 是什么

**网关模型 = 平台型号名 + 一个 Vendor Model Revision + 一组有序候选（Offering × Channel × 渠道模型名）+ 对客定价。**

它是 `CONTEXT.md` 里 **Gateway Model** 词条的完整化：词条已经定下"平台型号名与厂商原生名、渠道模型名是三个分开的角色"，但**今天这三个角色在库里是同一个值**——发布时用 `native_model_id` 当平台型号名（`crates/persistence` 的 `publish_runtime` 里 `gateway_model: native_model_id.clone()`），迁移 `0004_gateway_model.sql` 的注释也写明"今天两者同值"。本设计要做的，就是把这个已经预留好的角色**真正拆开**。

### 1.2 与现有对象的关系

| 现有对象 | 它拥有什么 | 与网关模型的关系 |
| --- | --- | --- |
| `catalog.vendor_models` | Vendor Model 身份（`vendor_id` + `native_model_id` + `native_revision`）+ **调用方合同**（模型级唯一一份、行不可变，`ADR-0015`） | 网关模型**指向**其中一行；合同**不复制**，多个网关模型可共享同一行 |
| `supply.offerings` | 一条候选供给：渠道、驱动、`provider_model_id`、承载面、参数映射、限制（`ADR-0015`） | 网关模型候选集的元素。**发布时它的技术定义被快照进这次修订的条目**（`publication.runtime_entries`），受理从条目读、不从活表读——否则共享同一个厂商模型的两个网关模型会互相改活候选。改价时沿用渠道字段的那个查询同样从条目取。见 [`0012`](./0012-platform-model-publishing.md) §5 |
| `publication.runtime_revisions` / `runtime_entries` | 一次发布的不可变修订；生效条目、`routing_priority`、`active` | 网关模型的**身份与候选集只由发布产生**（`ADR-0009`：一次发布携带完整有序候选集合，原子替换） |
| `pricing.price_plans` | 现状：四档 token 费率 + 来源 URL；现在它同时是对客结算基数 | 角色**收窄为渠道成本费率**，而且**只是"按 token 计量量计价"这一种计价形态的参数**（供给上的 `formula` 说这个渠道按什么计价，按张 / 按次或由上游直接给金额的供给**没有** Price Plan）：**费率表按渠道各自记、币种按该渠道的 `currency` 标注**（当前记的四档 `$5 / $10 / $8 / $30` 每 1M tokens 是 **AIHubMix** 的费率表、币种 **USD**；APIMart 的成本由上游 `cost` 直接给出，币种以渠道声明为准——**不是"全平台统一美元"**），定价时的参考与毛利核算用，不再是对客结算基数（[`0007`](./0007-pricing-floor-and-settlement.md) §1） |
| `generation.jobs` | 受理时的请求事实 + 被选中的 `PublishedOffering` + Price Snapshot | 受理时固化的 `gateway_model` 就是对外的那个名字 |

### 1.3 命名与唯一性

- **`gateway_model`（平台命名）**：全局唯一，且**同一时刻只有一个生效修订**。沿用现有唯一索引 `one_active_entry_per_model_and_priority`（`(gateway_model, routing_priority) WHERE active`）所保证的"同一名字的生效条目来自同一修订"，本设计只调整该索引（[`0008`](./0008-routing-strategy-and-caching.md) §2）——**该调整已落地**，现为 `one_active_entry_per_model_and_offering`（`(gateway_model, offering_id) WHERE active`），"生效条目来自同一修订"由发布事务的原子替换与仓库层的跨修订防御保证。**名字由管理员创建（发布）时自己填，平台不预设、也不固定任何名字**——本文出现的 `gpt-image-2.5-plus` 一类取值都是示例，不是默认名或规范名。
- **`native_model_id`（厂商原生名）**：只属于 `catalog.vendor_models`，用于合同的身份与唯一键；**不进对客面**。
- **允许多个网关模型指向同一个 Vendor Model Revision**：同一份供给包成不同名字/不同价格档（例如 `gpt-image-2.5-plus` 与 `gpt-image-2.5-lite` 都指向 `gpt-image-2.5-sunburst`，三者为示例值）。唯一性在**名字**上，不在 Vendor Model 上。这是引入命名层的动机之一，因此不做"一个 Vendor Model 只能有一个网关模型"的约束。
- **发布命令新增 `gateway_model`**（`PublishRuntimeCommand`）：缺省时回退取 `native_model_id`，与现有形状**逐位兼容**——现有素材、现有已发布数据、现有测试都不用改。

### 1.4 对客名与 `native_model_id` 的边界

对客面只出现 `gateway_model`。有一处必须处理：**合同里的 `properties.model.const` 现在写的是厂商原生名**（例如 `config/bootstrap/gpt-image-2.5-flare.json` 的 `"const": "gpt-image-2.5-flare"`）。

- **存的那份合同不动**（合同行不可变，`ADR-0003`/`ADR-0015`），**对客投射时把 `model.const` 替换成该网关模型名**。理由：受理期平台本来就用调用方给的 `model` 覆盖这个字段（`crates/application` 的 `contract_parameter_face` 里 `parameters.insert("model", request.model)`），`model` 一直是**平台字段**，不是厂商字段；投射时替换既不改数据、也不改校验行为。
- **发布期新增一条校验**：合同的 `properties.model.const` 必须等于本次发布的 `native_model_id`。它保证素材不把网关名或别的名字写进合同正文，也让 P1 工单的"响应全文不含原生名"这条验收可判定（合同正文里没有第二个模型名来源）。
- `GET /v1/models` 的 `revision` 字段仍取 `native_revision`：它是**合同修订号**，不是模型名（`docs/design/0005` §8.1 已定下该响应形状，本设计增 `vendor_id` 与 `type`、不删字段；`type` 的取值与展示归[模型类型 Spec](../specs/0006-model-type-and-usage-records.md)，落地见[模型类型设计](0020-model-type.md) §3）。

### 1.5 历史修订与已发布数据：不回填

**不回填。** 已发布的 `runtime_entries.gateway_model` 已经等于当时的 `native_model_id`，语义自洽（"平台型号名恰好等于厂商原生名"是合法取值）。改历史行会破坏"Job 固定受理时版本"（`ADR-0003`），因此新发布才允许两者不同。

### 1.6 存储改动

| 改动 | 内容 | 理由 |
| --- | --- | --- |
| `publication.runtime_revisions` 增列 | `gateway_model text NOT NULL`、`vendor_model_id uuid NOT NULL`（**P1 落**）、`markup_bps integer`、`reference_cost_microusd jsonb`（**按候选键**：该候选的渠道成本，**原币种**微单位，列名里的 `usd` 是历史命名；**只作定价参考**）、`cost_currency jsonb`（**按候选键**：该候选的成本币种，[`0007`](./0007-pricing-floor-and-settlement.md) §8）、`consumer_rates_cny jsonb`（**按候选键**：该候选的**对客四档 CNY 费率向量**，管理员设定/推导，[`0007`](./0007-pricing-floor-and-settlement.md) §2 与本文 §4）、`consumer_formula jsonb`（**按候选键**：该候选的**对客计价形态**，[`0007`](./0007-pricing-floor-and-settlement.md) §1）、`cost_basis jsonb`（**按候选键**：该候选的成本来源两态，与 `cost_currency` 同处，[`0007`](./0007-pricing-floor-and-settlement.md) §7）、`tier_prices jsonb`（**CNY**，展示用）、`floor_amounts jsonb`（**CNY**，保底表）（**P2b 落**；后八列**可空**：只有新发布的修订带定价，见下） | 让"这次发布定义的是哪个网关模型、指向哪个 Vendor Model、**每个候选的渠道成本是多少（什么币种）**、**每个候选的对客费率是多少**、**成本按哪个来源算**、**预授权保底额从哪查**"在修订上可读，不必从条目反推 |
| `publication.runtime_entries` | 已有 `gateway_model`（迁移 0004 改名而来）、`routing_priority`、`weight`；**增列技术定义快照**：`adapter_key`、`provider_model_id`、`carrier_schema`、`parameter_mapping`、`restrictions`，以及渠道三要素 `provider_kind` / `base_url` / `credential_env` | 路由索引已经按 `gateway_model` 建好。快照列让"发布即冻结"成立：受理从条目读技术定义，不从 `supply.offerings` / `supply.channels` 的当前值读（`ADR-0009`）。两个 `enabled` 开关**不进快照**，受理仍按活表判定 |
| 新表 `publication.gateway_models` | `gateway_model text PRIMARY KEY`、`enabled boolean NOT NULL DEFAULT true`、`created_at`、`updated_at`、`updated_by` | **只放运维开关**，不放定义（定义只在不可变修订里） |
| 唯一性 | 沿用"同一名字同时只有一个生效修订"，由发布原子替换保证 | `ADR-0009` |

`publication.gateway_models` 刻意**不存** `vendor_model_id` / 候选 / 定价：那些是修订的内容，存第二份就等于造第二个权威（`ADR-0003`）。它只回答"这个名字现在开着吗、谁在什么时候改的"。

**定价列（`markup_bps` / `reference_cost_microusd` / `cost_currency` / `consumer_rates_cny` / `consumer_formula` / `cost_basis` / `tier_prices` / `floor_amounts`）的币种、角色与取值口径归 [`0007`](./0007-pricing-floor-and-settlement.md) §2 与本文 §6/§8/§9**：本份只登记它们在修订上的位置，以及"随修订发布、随 Job 快照冻结"这条性质，不重复定价口径。

**写入方**：该名字**首次发布成功时**由发布事务插入一行（`enabled` 默认 `true`），此后只由 `PATCH` 改 `enabled`。没有发布过就 PATCH 不存在的名字 → 404。

**迁移的回填**（增量迁移，不改已应用的 `0001`–`0006`，沿用本仓库的迁移约定）：**迁移分两次，与切片对齐——P1 落命名两列，P2b 落定价列**（P1/P2b 工单是同一套列，两处口径一致）：

- **P1 落的命名两列**（`gateway_model` / `vendor_model_id`）：在既有行上先按"同一 revision 的 `runtime_entries.gateway_model` / `vendor_model_id`"回填（同一次发布写下的条目同值，可直接取），再设 `NOT NULL`；
- **P2b 落的定价列**（`markup_bps` / `reference_cost_microusd` / `cost_currency` / `consumer_rates_cny` / `consumer_formula` / `cost_basis` / `tier_prices` / `floor_amounts`）：在既有行上**留 NULL**——旧修订没有定价，因此那些修订受理出来的快照不带 `consumer_rates_cny` / `hold_microusd`，结算与预授权走旧口径、与今天逐位相同（[`0007`](./0007-pricing-floor-and-settlement.md) §3 与本文 §6）；
- `publication.gateway_models` 按既有生效名字回填出对应行（`enabled = true`），使现有已发布数据在迁移后立刻可读、可停用（随 P1 一起落）。

## 2. 对客目录与读写路径

### 2.1 `GET /v1/models` 的新形状

```json
{
  "object": "list",
  "data": [
    {
      "id": "gpt-image-2.5-plus",
      "object": "model",
      "created": 1789000000,
      "owned_by": "OpenAI",
      "name": "gpt-image-2.5-plus",
      "vendor_id": "OpenAI",
      "revision": "2026-09-20-contract-1.0",
      "type": "image",
      "contract": { "...": "发布的那份合同，其中 properties.model.const 已替换为 gpt-image-2.5-plus" },
      "documentation_url": "/v1/models/gpt-image-2.5-plus/llms.txt?version=…"
    }
  ]
}
```

- `vendor_id`：厂商标识（目录属性；现有响应把同一个值叫 `vendor`，按用户口径改名 `vendor_id`——这**修订**了 `docs/design/0005` §8.1 已定的响应形状 `{name, vendor, revision, contract}`，评审时按该节的处理方式确认）；
- `contract`：发布的那份合同（`model.const` 已替换，见 §1.4）。
- 目录字段的当前清单与取值归 [模型类型 Spec](../specs/0006-model-type-and-usage-records.md)、[模型使用文档 Spec](../specs/0008-model-usage-documentation.md) §2 与 [OpenAI 兼容的模型列表 Spec](../specs/0009-openai-compatible-model-list.md)（顶层 `object`；每条 `id` / `object` / `created` / `owned_by`）。
- **不出现**：`native_model_id`、`provider_model_id`、渠道、供给、驱动、优先级、权重、价格、任何执行记录。
- **仍公开、不校验 Key**：沿用 `docs/design/0005` §8.1 的裁定（建表单之前先要凭证等于逼调用方为了看一眼目录去开户；目录只有型号身份与合同）。
- **只列当前真的能调的**：判据与受理期同一条（生效条目 + 启用的供给 + 启用的渠道 + **网关模型 `enabled`**），取数与判据都在仓库层。
- **受理取数也要加同一条判据**：`active_offering(gateway_model)` 同样要过滤 `publication.gateway_models.enabled`。否则"关闭"只影响目录、不影响受理——目录里消失的模型仍能调用，等于没关。

### 2.2 管理员读：`GET /api/v1/gateway-models`

管理员视图，一条网关模型一项：

```json
{
  "gateway_models": [
    {
      "gateway_model": "gpt-image-2.5-plus",
      "enabled": true,
      "vendor_id": "OpenAI",
      "native_model_id": "gpt-image-2.5-sunburst",
      "native_revision": "2026-09-20-contract-1.0",
      "runtime_revision_id": "…",
      "published_at": "…",
      "candidates": [
        { "offering_id": "…", "provider_kind": "APIMart", "provider_model_id": "…",
          "routing_priority": 0, "weight": 1, "reference_cost_microusd": "…", "cost_currency": "…",
          "consumer_formula": "token_rates", "consumer_rates_cny": { "…": "该候选的四档对客 CNY 费率" }, "cost_basis": "Declared" },
        { "offering_id": "…", "provider_kind": "AIHubMix", "provider_model_id": "…",
          "routing_priority": 1, "weight": 1, "reference_cost_microusd": "…", "cost_currency": "…",
          "consumer_formula": "token_rates", "consumer_rates_cny": { "…": "该候选的四档对客 CNY 费率" }, "cost_basis": "Computed" }
      ],
      "pricing": { "markup_bps": "…" }
    }
  ]
}
```

它是**只读投影**：数据源是生效修订（`runtime_entries` + `runtime_revisions` + `catalog.vendor_models`）加运维开关（`publication.gateway_models`）。不新增"编辑态"，也不回显渠道凭证（`credential_env` 只记变量名，本来就不进响应）。响应回显每条候选的 `routing_priority` 与 `weight`（路由策略层见 [`0008`](./0008-routing-strategy-and-caching.md)）。示例里的 `pricing` 各项只占字段位，并标出**币种平面**（[`0007`](./0007-pricing-floor-and-settlement.md) §8）：每个候选各带**该候选的渠道成本**（`reference_cost_microusd`，**原币种**微单位，**只作定价参考**）、**它的成本币种**（`cost_currency`）、**按候选的对客费率向量**（`consumer_rates_cny`，**四档 CNY**，管理员设定/推导）与**它的成本口径**（`cost_basis`），`markup_bps` 是**加价系数**（**每网关模型一个**）；汇率是**全局按币种维护**的折算率（`pricing.fx_rates`，渠道币种 → CNY），不随修订发布、**受理时按该候选的 `cost_currency` 取"受理时刻生效的那一行"并快照进 Price Snapshot**（[`0007`](./0007-pricing-floor-and-settlement.md) §2）。**对客费率向量由管理员按"该 vendor/模型已知的渠道价目 × 倍率 × 该币种 → CNY 的折算率"设定/推导、运营可改（倍率 = 1 + `markup_bps` / 10000），随修订发布、随 Job 快照冻结**（[`0007`](./0007-pricing-floor-and-settlement.md) §2 与本文 §4），**因此同一网关模型的不同候选价格不同**；`reference_cost_microusd` 只作定价参考、**不是售价的被乘数**；**加价系数由管理员创建网关模型时录入、汇率由管理员在后台维护，数值本身不属设计决策**（[`0007`](./0007-pricing-floor-and-settlement.md) §2 与本文 §10）。对客选上游金额的候选没有这份向量，对客价按声明金额 × 冻结倍率 × 冻结折算率算出（[`0007`](./0007-pricing-floor-and-settlement.md) §2）。

### 2.3 写路径：发布一次，原子替换

`POST /api/v1/runtime-revisions`（管理员，已有）接受 `gateway_model` 与这次要的候选集合；一次请求 = 一个网关模型的**完整定义**（它指向哪个 Vendor Model Revision、由哪组 Offering 供应、顺序与权重、定价），**原子替换**该名字的生效条目，落一份**不可变修订**。

**候选怎么给，见 [`0012`](./0012-platform-model-publishing.md)**：运营给的是"选中的 Offering + 这条候选的价"，渠道三要素、驱动器、承载面与参数映射由 Offering 决定、从库里取；不在命令里内联。

### 2.4 为什么不做分步 CRUD / 草稿态

1. **`ADR-0009` 要求候选集永远来自同一个 Revision**（"一次发布携带该型号完整有序的候选集合，发布即原子替换"）。分步 CRUD 会让"先加候选 A、再加候选 B"中间出现**半套候选**的生效窗口。
2. **`ADR-0003` 要求 Job 受理时固定版本**。草稿态会引入第三个状态（草稿 / 生效 / 历史）与"现在对客生效的到底是哪一份"的歧义。
3. **任何一步失败都会留下不一致的对外目录**（目录里已有该模型，候选却没配齐），而目录一旦列出就必须真的受理得起来（`docs/design/0005` §8.1 的取数判据）。
4. **唯一的可变位是运维开关**：`PATCH /api/v1/gateway-models/{name}` **只允许改 `enabled`**。它和 `supply.channels.enabled` / `supply.offerings.enabled` 同类——是**运行状态**，不是定义；`ADR-0009` 也把 `active` 当"按型号全局可变的事实"处理。因此它不构成"分步 CRUD"。

`PATCH` 的语义：关闭 → 该名字从 `GET /v1/models` 消失、受理得到"模型不存在"（现有 `select_candidate` 在无候选时返回 `NotFound`）；已受理的 Job 不受影响（`ADR-0003`）。写 `operations.audit_events`。

### 2.5 管理员读：查看余额

提案第 4 条要的"查看余额"**今天没有落点**：`apps/api/src/main.rs` 的路由表里 `/api/v1/accounts` 只有 `POST`（建账户）、`/credits`（充值）、`/api-keys`（发 Key），没有任何读余额的入口，运营只能直查 `ledger.accounts`。

补一条管理员读：`GET /api/v1/accounts/{account_id}`，返回 `balance_microusd` 与 `updated_at`（`ledger.accounts` 的现有两列，不加新列）。**读的是 DB，不是缓存**——缓存不是事实源（`ADR-0003`），引入 Redis 之后这条也不变（[`0008`](./0008-routing-strategy-and-caching.md) §7.1）。切片归 P5 工单，验收在那里。

它同时收掉提案与工单 [`#9`](https://github.com/dehuadong/seeaihub-server-next/issues/9) 的重复：`#9` 的"账目可见"诉求与这里同源，实施时只做一处（提案"与其他工作项的关系"一节）。

## 3. 不做

- **调用记录查询接口**（提案第 4 条后半，用户撤回）；
- **跨厂商统一图片参数语义**（`ADR-0015`：属后期独立规划）；
- **通用参数映射引擎**（沿用 `#10` 口径：只做"合同 → 该候选承载面"的校验与装载）；
- **发布的分步 CRUD / 草稿态**（§2.4 理由）；
- **失败后改道（换候选）**：上游请求一旦失败就改选另一条候选，仍不做（`ADR-0009`/`ADR-0011`，属 [`#11`](https://github.com/dehuadong/seeaihub-server-next/issues/11)）——"可证明未受理"的失败重投的是同一份请求、同一个候选，不属本项；
- **成本进账本与账实核对**（[`#11`](https://github.com/dehuadong/seeaihub-server-next/issues/11) 的成本进账本与账实核对那部分）；
- **对外价策略本身与具体数值**（[`#5`](https://github.com/dehuadong/seeaihub-server-next/issues/5)）；
- **把 `config/bootstrap/*.json` 当后台**（用户明确：它是初始化种子与测试夹具，不是运营的去处）。**它不是后台，但它是工程师写渠道与 Offering 的地方**：这些技术定义由素材**导入**到库里，之后运营在后台**选**它们，而不是在素材里配价（[`0012`](./0012-platform-model-publishing.md) §3）。

## 4. 决策归属与 ADR

**未决（用户持有）：0 条**——本主题原两条未决已由用户答复定案（权重语义见 [`0008`](./0008-routing-strategy-and-caching.md) §8，对客价口径见 [`0007`](./0007-pricing-floor-and-settlement.md) §10）；加价系数与汇率的数值由后台录入，**不属设计决策**。

### 已定案（本份直接决定）

- **启停有两个粒度**：整个网关模型一个开关（`publication.gateway_models.enabled`），以及**供给级**（`supply.offerings.enabled` / `supply.channels.enabled`，`PATCH` 只改 `enabled`）。两者都是**运行状态、不进不可变修订**：写入即对之后的受理生效，已受理的 Job 不受影响；
- **命名层与对客面的边界**：对客面只出现 `gateway_model`，`native_model_id` 不进对客面；合同 `model.const` 的对客投射替换（§1.3/§1.4）；
- **历史不回填**：已发布的 `runtime_entries.gateway_model` 保持原值（§1.5）。

### 范围边界（不待决，归其他工作项）

- **对外价策略与具体数值**（含是否分档、加价系数与汇率的具体取值）归 [`#5`](https://github.com/dehuadong/seeaihub-server-next/issues/5)——本主题只给机制；
- **成本护栏**（服务端成本上限）不在本主题（见 [运维底线与运行面](./0009-operational-baseline.md) §7）——本主题只把**客户余额**当受理上限（[`0007`](./0007-pricing-floor-and-settlement.md) §6）；
- **成本进账本与账实核对**不在本主题（见 `crates/persistence` 与 `CONTEXT.md` 的 `Platform Account`）；
- **运行期回退**（上游请求已发出之后改道另一条候选）归 [`#11`](https://github.com/dehuadong/seeaihub-server-next/issues/11)（[`0008`](./0008-routing-strategy-and-caching.md) §4）；
- **调用记录查询接口**按用户口径不做（§3）。

### 需要 ADR 的决定

本份不新增 ADR、也不就地修订任何 ADR；与本主题相关的 ADR 动作按归属登记在 [`0007`](./0007-pricing-floor-and-settlement.md) §10 与 [`0008`](./0008-routing-strategy-and-caching.md) §8。**两条就地修订已完成**（2026-09-22）：`ADR-0006` 追加了"币种段"与"汇率段"两条修订说明；`ADR-0009` 追加了"预授权/上限条款"修订说明，"部分被取代"标注里也点明了"同一型号内唯一"已由 `ADR-0020` 取代。仍待确认新立的两条（成本事实的落点、对客售价的构成与币种口径）见 [`0007`](./0007-pricing-floor-and-settlement.md) §10——按仓库约定，记录持久 ADR 必须获得用户确认。

### 设计级修订（改 `docs/design/`，不动 ADR）

- `GET /v1/models` 的字段名 `vendor` → `vendor_id`、新增 `type`（[`docs/design/0005`](./0005-vendor-model-contract-and-offering-mapping.md) §8.1 已定该形状，该节第 1 条已同步改毕；`type` 的取值与展示归[模型类型 Spec](../specs/0006-model-type-and-usage-records.md)）；
- 合同 `model.const` 的对客投射替换规则（§1.4）。

> 评审过程与逐条处置见提案 [#13](https://github.com/dehuadong/seeaihub-server-next/issues/13) 的评论。
