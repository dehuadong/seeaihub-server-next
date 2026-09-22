主题: 平台网关模型、对客定价与运营后台（含路由权重与 Redis 加速层）
当前修订: v1
状态: 待评审（Plan Review 两轮发现已收口；用户更正与批准已并入；计价口径更正与路由策略层已并入；路由策略层决策已立为 `docs/adr/0020`）
来源: 依据工作项「运营后台：平台网关模型、对客定价与路由权重」（提案正文 `.data/proposal-admin-console.md`）、`docs/adr/0003`/`0006`/`0009`/`0015`/`0017`/`0019`/`0020`、`docs/design/0005` 与仓库现状归纳；不引入未标注的新决策

# 平台网关模型、对客定价与运营后台

本文是运营后台工作的技术设计：**怎么落地**「管理员用 API 定义平台网关模型 → 定对客价 → 排路由顺序与权重 → 事后查得清售价、成本与毛利」，以及随之而来的 Redis 加速层。产品范围、验收与决策归属归提案正文（`.data/proposal-admin-console.md`）；本文只承载技术设计。

术语一律沿用 `CONTEXT.md`：**Gateway Model**（平台型号名，对外的 `model`）、**Vendor Model**（厂商的模型产品）、**Offering**（一条可调用供给）、**Channel**、**Runtime Revision**、**Price Plan**、**Price Snapshot**、**Routing Priority**、**Metering Evidence**。本文新引入的只有四个字段级说法：**渠道成本**（ADR-0006 的"成本按渠道各自的口径取数"）、**加价系数**、**汇率**、**保底额**（预授权冻结的那笔钱，按供给维度查表，§3.6）——理由见 §3；§5 的路由策略层另引入**策略**、**折扣率**与**账户标签**三个说法。

## 1. 网关模型对象

### 1.1 是什么

**网关模型 = 平台型号名 + 一个 Vendor Model Revision + 一组有序候选（Offering × Channel × 渠道模型名）+ 对客定价。**

它是 `CONTEXT.md` 里 **Gateway Model** 词条的完整化：词条已经定下"平台型号名与厂商原生名、渠道模型名是三个分开的角色"，但**今天这三个角色在库里是同一个值**——发布时用 `native_model_id` 当平台型号名（`crates/persistence` 的 `publish_runtime` 里 `gateway_model: native_model_id.clone()`），迁移 `0004_gateway_model.sql` 的注释也写明"今天两者同值"。本设计要做的，就是把这个已经预留好的角色**真正拆开**。

### 1.2 与现有对象的关系

| 现有对象 | 它拥有什么 | 与网关模型的关系 |
| --- | --- | --- |
| `catalog.vendor_models` | Vendor Model 身份（`vendor_id` + `native_model_id` + `native_revision`）+ **调用方合同**（模型级唯一一份、行不可变，`ADR-0015`） | 网关模型**指向**其中一行；合同**不复制**，多个网关模型可共享同一行 |
| `supply.offerings` | 一条候选供给：渠道、驱动、`provider_model_id`、承载面、参数映射、限制（`ADR-0015`） | 网关模型候选集的元素；候选自身的定义不改 |
| `publication.runtime_revisions` / `runtime_entries` | 一次发布的不可变修订；生效条目、`routing_priority`、`active` | 网关模型的**身份与候选集只由发布产生**（`ADR-0009`：一次发布携带完整有序候选集合，原子替换） |
| `pricing.price_plans` | 现状：四档 token 费率（USD）+ 来源 URL；现在它同时是对客结算基数 | 角色**收窄为渠道成本费率**：**两家渠道同一套四档美元 token 费率**（文本输入 $5 / 文本输出 $10 / 图像输入 $8 / 图像输出 $30，每 1M tokens；**美元只属成本平面**，§3.8），定价时的参考与毛利核算用，不再是对客结算基数（§3） |
| `generation.jobs` | 受理时的请求事实 + 被选中的 `PublishedOffering` + Price Snapshot | 受理时固化的 `gateway_model` 就是对外的那个名字 |

### 1.3 命名与唯一性

- **`gateway_model`（平台命名）**：全局唯一，且**同一时刻只有一个生效修订**。沿用现有唯一索引 `one_active_entry_per_model_and_priority`（`(gateway_model, routing_priority) WHERE active`）所保证的"同一名字的生效条目来自同一修订"，本设计只调整该索引（§4）。
- **`native_model_id`（厂商原生名）**：只属于 `catalog.vendor_models`，用于合同的身份与唯一键；**不进对客面**。
- **允许多个网关模型指向同一个 Vendor Model Revision**：同一份供给包成不同名字/不同价格档（`gpt-image-2.5-plus` 与 `gpt-image-2.5-lite` 都指向 `gpt-image-2.5-sunburst`）。唯一性在**名字**上，不在 Vendor Model 上。这是引入命名层的动机之一，因此不做"一个 Vendor Model 只能有一个网关模型"的约束。
- **发布命令新增 `gateway_model`**（`PublishRuntimeCommand`）：缺省时回退取 `native_model_id`，与现有形状**逐位兼容**——现有素材、现有已发布数据、现有测试都不用改。

### 1.4 对客名与 `native_model_id` 的边界

对客面只出现 `gateway_model`。有一处必须处理：**合同里的 `properties.model.const` 现在写的是厂商原生名**（例如 `config/bootstrap/gpt-image-2.5-flare.json` 的 `"const": "gpt-image-2.5-flare"`）。

- **存的那份合同不动**（合同行不可变，`ADR-0003`/`ADR-0015`），**对客投射时把 `model.const` 替换成该网关模型名**。理由：受理期平台本来就用调用方给的 `model` 覆盖这个字段（`crates/application` 的 `contract_parameter_face` 里 `parameters.insert("model", request.model)`），`model` 一直是**平台字段**，不是厂商字段；投射时替换既不改数据、也不改校验行为。
- **发布期新增一条校验**：合同的 `properties.model.const` 必须等于本次发布的 `native_model_id`。它保证素材不把网关名或别的名字写进合同正文，也让 §9 的"响应全文不含原生名"这条验收可判定（合同正文里没有第二个模型名来源）。
- `GET /v1/models` 的 `revision` 字段仍取 `native_revision`：它是**合同修订号**，不是模型名（`docs/design/0005` §8.1 已定下该响应形状，本设计只增 `vendor_id`、不删字段）。

### 1.5 历史修订与已发布数据：不回填

**不回填。** 已发布的 `runtime_entries.gateway_model` 已经等于当时的 `native_model_id`，语义自洽（"平台型号名恰好等于厂商原生名"是合法取值）。改历史行会破坏"Job 固定受理时版本"（`ADR-0003`），因此新发布才允许两者不同。

### 1.6 存储改动

| 改动 | 内容 | 理由 |
| --- | --- | --- |
| `publication.runtime_revisions` 增列 | `gateway_model text NOT NULL`、`vendor_model_id uuid NOT NULL`（**P1 落**）、`markup_bps integer`、`reference_cost_microusd bigint`（**USD**，成本平面）、`cost_basis text`、`tier_prices jsonb`（**CNY**，展示用）、`consumer_rates_cny jsonb`（**CNY**，对客四档 token 费率）、`floor_amounts jsonb`（**CNY**，保底表）、`fx_rate_usd_cny bigint`（**P2b 落**；后七列**可空**：只有新发布的修订带定价，见下） | 让"这次发布定义的是哪个网关模型、指向哪个 Vendor Model、按什么价卖、**成本按哪个来源算**、**预授权保底额从哪查**"在修订上可读，不必从条目反推 |
| `publication.runtime_entries` | 已有 `gateway_model`（迁移 0004 改名而来），不改 | 路由索引已经按它建好 |
| 新表 `publication.gateway_models` | `gateway_model text PRIMARY KEY`、`enabled boolean NOT NULL DEFAULT true`、`created_at`、`updated_at`、`updated_by` | **只放运维开关**，不放定义（定义只在不可变修订里） |
| 唯一性 | 沿用"同一名字同时只有一个生效修订"，由发布原子替换保证 | `ADR-0009` |

`publication.gateway_models` 刻意**不存** `vendor_model_id` / 候选 / 定价：那些是修订的内容，存第二份就等于造第二个权威（`ADR-0003`）。它只回答"这个名字现在开着吗、谁在什么时候改的"。

**`markup_bps`、`reference_cost_microusd` 与 `cost_basis` 随修订发布、随 Job 的 Price Snapshot 冻结**（§3.2/§3.3），**不放** `publication.gateway_models`：那张表是**运行状态**（开关），定价是**修订内容**——放进可变表就等于"改价不用发布"，而 `ADR-0003` 要求已受理 Job 固定受理时版本，定价必须能随修订被 Job 固化。其中 `cost_basis` 存的是**这次的成本来源口径**（**两态**：`Computed` = 我们按**实际 `usage`** 的分项 token × 四档费率自算；`Declared` = 上游**直接给 `cost`**，更权威、含折扣；**两态都在成本平面、币种 USD**，§3.8），取值面与 §3.7 的 `provider_cost_source` 同源但**不是同一个量**：它是**随修订发布**的定价侧口径（这次发布按哪种来源记成本），随快照冻结后使"这笔的成本是按哪种来源取的"事后可辨——**"成本来源可辨"是毛利核算的要求**（§3.5/§3.7）。

**写入方**：该名字**首次发布成功时**由发布事务插入一行（`enabled` 默认 `true`），此后只由 `PATCH` 改 `enabled`。没有发布过就 PATCH 不存在的名字 → 404。

**`tier_prices`（档位价目表）与 `floor_amounts`（保底表）同样是随修订发布、随 Job 快照冻结的发布数据**，但两者**角色完全不同**：

- **`tier_prices`（档位价目表）降级为参考**：`(size, quality)` → 每张价（**CNY**），**只用于定价参考与展示**（管理员核价、对客价目说明），**不参与预授权**（§3.6）。档位的主要影响因素是 **`size` 与 `quality`**；`resolution` 是 **APIMart 的包装参数**（调用方合同里没有"档位"形态，承载面也不声明它），因此**不作价目表 / 保底表的键**；
- **`floor_amounts`（保底表）是预授权的唯一来源**：**按供给（vendor + offering）维度**挂——不同 vendor / offering 计价不同，所以保底额必须**分别设定**，不能按网关模型或全平台一个数；每条供给下按 **`(size, quality)` 两维**给保底额，并另有一个**该供给的封顶保底值**（档位查不到时用它）。**保底额是人民币（CNY）**（§3.8）。OpenAI 系当前**只按 `size` 填**（**1K = ¥0.16**、**2K = ¥0.25**、**4K = ¥0.3**，**币种＝CNY**），**`quality` 维留空备用**——**留空即按 `size` 档**（§3.6）。

**为什么不编进代码**：档位结构、每张价、每档保底额都是**随模型与渠道变的数据**——换模型、换渠道、渠道调价都不该改代码、不该重新发版；它们与 `markup_bps` 同类，**随修订发布生效、随 Job 快照冻结**，已受理的 Job 不受后续改动影响（`ADR-0003`）。与现有两个量的关系：`reference_cost_microusd` 是**定价时的参考成本**（USD，单值、可核，用来体现"成本 + 加价系数"这条产品口径，§3.2），`floor_amounts` 是**受理时算预授权的查表依据**（CNY）——同源不同用：前者回答"这个网关模型的价是怎么定的"，后者回答"这一次请求先冻多少"。**两个币种平面**（对客 CNY / 成本 USD）见 §3.8。

**迁移的回填**（增量迁移，不改已应用的 `0001`–`0006`，沿用本仓库的迁移约定）：**迁移分两次，与切片对齐——P1 落命名两列，P2b 落定价列**（§9 的 P1/P2b 是同一套列，两处口径一致）：

- **P1 落的命名两列**（`gateway_model` / `vendor_model_id`）：在既有行上先按"同一 revision 的 `runtime_entries.gateway_model` / `vendor_model_id`"回填（同一次发布写下的条目同值，可直接取），再设 `NOT NULL`；
- **P2b 落的定价列**（`markup_bps` / `reference_cost_microusd` / `cost_basis` / `tier_prices` / `consumer_rates_cny` / `floor_amounts` / `fx_rate_usd_cny`）：在既有行上**留 NULL**——旧修订没有定价，因此那些修订受理出来的快照不带 `consumer_rates_cny` / `hold_microusd`，结算与预授权走旧口径、与今天逐位相同（§3.3/§3.6）；
- `publication.gateway_models` 按既有生效名字回填出对应行（`enabled = true`），使现有已发布数据在迁移后立刻可读、可停用（随 P1 一起落）。

## 2. 对客目录与读写路径

### 2.1 `GET /v1/models` 的新形状

```json
{
  "data": [
    {
      "name": "gpt-image-2.5-plus",
      "vendor_id": "OpenAI",
      "revision": "2026-09-20-contract-1.0",
      "contract": { "...": "发布的那份合同，其中 properties.model.const 已替换为 gpt-image-2.5-plus" }
    }
  ]
}
```

- `name`：**网关模型名**，客户端提交 `model` 时用的唯一身份；
- `vendor_id`：厂商标识（目录属性；现有响应把同一个值叫 `vendor`，按用户口径改名 `vendor_id`——这**修订**了 `docs/design/0005` §8.1 已定的响应形状 `{name, vendor, revision, contract}`，评审时按该节的处理方式确认）；
- `revision`：合同修订号；
- `contract`：发布的那份合同（`model.const` 已替换，见 §1.4）。
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
          "routing_priority": 0, "weight": 1 },
        { "offering_id": "…", "provider_kind": "AIHubMix", "provider_model_id": "…",
          "routing_priority": 1, "weight": 1 }
      ],
      "pricing": { "consumer_rates_cny": "…", "reference_cost_microusd": "…", "markup_bps": "…", "fx_rate_usd_cny": "…" }
    }
  ]
}
```

它是**只读投影**：数据源是生效修订（`runtime_entries` + `runtime_revisions` + `catalog.vendor_models`）加运维开关（`publication.gateway_models`）。不新增"编辑态"，也不回显渠道凭证（`credential_env` 只记变量名，本来就不进响应）。示例里的 `pricing` 各项只占字段位，并标出**币种平面**（§3.8）：`consumer_rates_cny` 是对客 CNY 售价、`reference_cost_microusd` 是美元参考成本、`fx_rate_usd_cny` 是只服务毛利折算的汇率；**加价系数由管理员创建网关模型时录入、汇率由管理员在后台维护，数值本身不属设计决策**（§3.2/§10）。

### 2.3 写路径：沿用整份发布，新增一个字段

`POST /api/v1/runtime-revisions`（管理员，已有）新增 `gateway_model`；其余形状不变——一次请求 = 一个网关模型的**完整定义**（合同引用、候选数组、顺序、权重、定价），**原子替换**该名字的生效条目，落一份**不可变修订**。

### 2.4 为什么不做分步 CRUD / 草稿态

1. **`ADR-0009` 要求候选集永远来自同一个 Revision**（"一次发布携带该型号完整有序的候选集合，发布即原子替换"）。分步 CRUD 会让"先加候选 A、再加候选 B"中间出现**半套候选**的生效窗口。
2. **`ADR-0003` 要求 Job 受理时固定版本**。草稿态会引入第三个状态（草稿 / 生效 / 历史）与"现在对客生效的到底是哪一份"的歧义。
3. **任何一步失败都会留下不一致的对外目录**（目录里已有该模型，候选却没配齐），而目录一旦列出就必须真的受理得起来（`docs/design/0005` §8.1 的取数判据）。
4. **唯一的可变位是运维开关**：`PATCH /api/v1/gateway-models/{name}` **只允许改 `enabled`**。它和 `supply.channels.enabled` / `supply.offerings.enabled` 同类——是**运行状态**，不是定义；`ADR-0009` 也把 `active` 当"按型号全局可变的事实"处理。因此它不构成"分步 CRUD"。

`PATCH` 的语义：关闭 → 该名字从 `GET /v1/models` 消失、受理得到"模型不存在"（现有 `select_candidate` 在无候选时返回 `NotFound`）；已受理的 Job 不受影响（`ADR-0003`）。写 `operations.audit_events`。

### 2.5 管理员读：查看余额

提案第 4 条要的"查看余额"**今天没有落点**：`apps/api/src/main.rs` 的路由表里 `/api/v1/accounts` 只有 `POST`（建账户）、`/credits`（充值）、`/api-keys`（发 Key），没有任何读余额的入口，运营只能直查 `ledger.accounts`。

补一条管理员读：`GET /api/v1/accounts/{account_id}`，返回 `balance_microusd` 与 `updated_at`（`ledger.accounts` 的现有两列，不加新列）。**读的是 DB，不是缓存**——缓存不是事实源（`ADR-0003`），引入 Redis 之后这条也不变（§7.1）。切片归 §9 的 **P5**，验收在那里。

它同时收掉提案与工单 [`#9`](https://github.com/dehuadong/seeaihub-server-next/issues/9) 的重复：`#9` 的"账目可见"诉求与这里同源，实施时只做一处（提案"与其他工作项的关系"一节）。

## 3. 定价

### 3.1 现状：两个渠道都是 token 计费，成本来源有两态

**两家渠道都是 token 计费**，费率就是同一套四档**美元**（每 1M tokens）：**文本输入 $5 / 文本输出 $10 / 图像输入 $8 / 图像输出 $30**。差别只在**成本从哪来**，所以成本来源只有两态（都在**成本平面、币种 USD**，§3.8）：

| 渠道 | 成本来源 | 现状 |
| --- | --- | --- |
| **AIHubMix** | **`Computed`**：上游只返回四分项 token、**没有任何金额字段** ⇒ 成本 = Σ(**实际** 分项 token × 四档费率) | 费率在 `pricing.price_plans`，现在**同时**当对客结算基数；本设计把它的角色**收窄为渠道成本费率**（定价时的参考口径 + 毛利核算），不再是对客结算基数（§3.2/§3.3） |
| **APIMart** | **`Declared`**：上游任务终态**直接返回 `cost`**（USD，含账号 `Group ratio 0.8`，不可复现）⇒ **直接取它，不需要我们自己算**——它比自算**更权威**（含折扣） | **当前既不采纳也不留存**（`crates/adapter-apimart` 头注明确），要算毛利必须开始采集 |

**实测事实**：APIMart 的 `cost` 我们实测过，返回 `cost = 0.011354`（`docs/facts/channel-facts.md` §3）。

依据：`docs/facts/channel-facts.md` §2.4/§2.6、§3、§5。`ADR-0006` 同时定下"`cost` 只用于核成本，**不替代计量事实**"——所以采集 `cost` 不违反那条决定，也不是复活已被否决的"金额型计量证据"（`ADR-0012` 存根）。**"按张计费"这一形态不存在**：两家都是 token 计费，因此 §3 不再有"按张 / 按 token"的二分（§3.6 的预授权保底额是另一回事，见 §1.6）。

### 3.2 定价公式与三个量的落点

```
对客售价(CNY) = 后台按网关模型设定的 CNY 售价
参考算法（定价时用，不是运行时换算）：参考渠道成本(USD) × (1 + 加价系数) × 汇率(USD → CNY)
```

**对客只有 CNY 一个币种**（§3.8）：售价以人民币表达、由后台按网关模型设定；上面第二行只是**后台定价时的参考算法**，运行时**不做实时汇率换算**。**这条公式是"定价口径"，不是"结算公式"**：默认读法下（§3.4）对客 CNY 费率在受理时算定并随 Job 冻结，**结算只读那份快照**（§3.3），不再读运行期的实际成本。公式里的"渠道成本"因此是**定价时参考的成本**（USD，发布数据），不是"命中候选在运行期报出来的实际成本"——后者只进 `attempts`，只用于毛利核算（§3.5）。受理时要算得出对客费率，就不能等上游回来才定价；**预授权**则另走保底表（§3.6），与这条公式无关。

| 量 | 放哪 | 为什么 |
| --- | --- | --- |
| **参考渠道成本** | 随修订发布的**发布数据** `runtime_revisions.reference_cost_microusd`（**USD**；发布者取一个可核的参考值：`Computed` 按四档费率 × 参考用量、`Declared` 取上游声明过的 `cost`） | 实际金额要等上游回来才知道；拿实际成本定价等于把售价推迟到结算 |
| **加价系数** | `markup_bps`（整数基点，避免浮点）：**每个网关模型一个**，**由管理员创建/发布该网关模型时录入**，**随修订发布**（`runtime_revisions.markup_bps`，§1.6），**随 Job 的 Price Snapshot 冻结**；不放 `publication.gateway_models`（那张表只存开关）。**具体数值由后台录入，不属设计决策** | 网关模型这一层就是"同一份供给包成不同价格档"的载体；全局系数会让这层失去意义。随快照冻结 ⇒ 已受理 Job 不受后续改价影响（`ADR-0003`） |
| **汇率** | **全局一条**（`pricing.fx_rates`：**USD → CNY** + 生效时间），**由管理员在后台维护**（入口 `PUT /api/v1/fx-rates`，写审计），**受理时快照进 Price Snapshot**（`fx_rate_usd_cny`）。**只用于把美元成本折成人民币**、服务**毛利核算**（§3.5/§3.8），**不参与对客金额的计算**。**具体数值由后台录入，不属设计决策**。**这一处就地修订 `ADR-0006`**（它原文写的是"Price Plan 保留…发布时固定的汇率"，见 §10 的修订清单） | 汇率是**外部事实**，同一时刻全平台必须是同一个数才对账得起来；放进每个网关模型的发布里，改一次汇率要重发所有模型。不写配置文件（用户明确后台走 API） |

汇率**数值由后台管理员录入**（全局一条，**USD → CNY**），设计只立字段与快照位**并规定录入入口与快照时机**（见 §10）；两家渠道的成本都是 USD 是既有事实（`docs/facts/channel-facts.md`），与 `ADR-0006` 的"以 USD 计价的计划原生价即 microUSD"口径不冲突；**对客平面一律 CNY**（§3.8）。

### 3.3 售价与保底快照随 Job 冻结

`PriceSnapshot`（`crates/domain`，现在只有 `price_plan_id` + 四档 `rates` + `captured_at`）扩展为：

```
PriceSnapshot {
    price_plan_id,
    // ---- 对客平面：全部 CNY（§3.8）----
    consumer_rates_cny,                                        // 随修订发布：**对客四档 token 费率（CNY）**，后台按网关模型设定（实收依据）
    tier_prices,                                               // 随修订发布：档位价目表（**CNY**）——**仅定价参考/展示，不参与预授权**（§1.6/§3.6）
    floor_amounts,                                             // 随修订发布：**保底表（CNY）**——按供给（vendor + offering）维度、(size, quality) → 保底额 + 该供给封顶保底值（§1.6/§3.6）
    hold_microusd,                                             // 受理时算定并冻结：本次请求的**保底额（CNY 微单位）**（§3.6）
    hold_source,                                               // 保底额来源：供给档位查表 / size=auto 取最大档 / 该供给封顶保底值 / 平台兜底（§3.6，事后可辨"这次为什么冻这么多"）
    // ---- 成本平面：USD，以及折算用的汇率（§3.8）----
    cost_basis: Computed { rates } | Declared { currency },    // 成本来源两态（随修订发布：runtime_revisions.cost_basis，随快照冻结）；rates 是**美元**四档渠道成本费率
    reference_cost_microusd,                                   // 定价时参考的渠道成本（**USD**，随修订发布）
    markup_bps,                                                // 随修订发布：**只用于后台定价时的参考算法**，不参与运行时换算（§3.2）
    fx_rate_usd_cny,                                           // 受理时从全局表快照：**USD → CNY**，定点整数（如 1e6 分母），不使用浮点；**只用于成本折算（毛利）**，不参与对客金额
    captured_at,
}
```

`charge_microusd(usage)` 从"Σ token × 费率"改为"**只读快照 + 本次实际用量**"：实收 = **快照里的对客四档 token 费率（`consumer_rates_cny`，CNY）× 实际 `usage` 的分项 token（真值）**，**不封顶在保底额**——实际超过保底额时差额把余额扣成负数（**透支发生在结算**，§3.6）。**对客金额全程 CNY、不做实时汇率换算**（§3.8）。成本侧按 `cost_basis` 取数（**USD**）：`Computed` = 实际分项 token × 四档渠道成本费率自算；`Declared` = 直接取上游声明的 `cost`（更权威、含折扣）——**成本只进毛利口径，不改对客金额**（§3.5）。毛利核算时用快照里的 `fx_rate_usd_cny` 把美元成本折成 CNY（§3.5/§3.8）。

**"档位 → 每张价"不再是计价单位**：两家渠道都是 token 计费（§3.1），所以实收只有 token 一个口径；`tier_prices` 里的每张价**只用于定价参考与展示**，不参与受理时的预授权，也不参与结算。

**历史兼容**：`price_snapshot` 是 jsonb，**缺 `consumer_rates_cny` / `hold_microusd`** ⇒ 按旧口径（`Σ token × 已发布费率`）结算，结果与今天逐位相同。缺它有两种来源，都走这条路：① 已受理的历史 Job（快照本身就是旧的）；② 迁移后仍生效、但**没有定价**的旧修订受理出的新 Job（§1.6：定价列留 NULL）。两种来源的 **保底口径也一致**：缺 `hold_microusd` 时回落到 `GENERATION_MAX_COST_MICROUSD`，即**今天的行为**（§3.6）。历史 Job 的查询与结算行为不变（验收第 9 条）。

### 3.4 扣费与命中渠道的关系（默认读法待用户确认）

用户原话是"与命中渠道无关"。本设计按**默认读法**落地，并且这一处**待用户确认**（§10 未决里的"与命中渠道无关"那条）：

- **默认读法（本设计按此落地）：金额也无关。** 同一个网关模型**不管命中哪条候选，对客户都是同一个固定售价**——售价随修订发布、受理时快照冻结（§3.3），渠道成本只在**定价时**参考（§3.2），运行期的实际渠道成本只用于毛利核算（§3.5）。因此"同一请求命中两条成本不同的候选时对客扣费不同"**不再成立**：扣费逐位相同，差异只体现在毛利。扣的仍然是**同一个用户余额**（`ledger.accounts`），不按渠道分账。
- **另一种读法（一句话）**：若用户的意思是"只有**扣费对象**与渠道无关、金额仍随实际命中的候选成本变"，那实收就不能只读受理时冻结的对客费率快照（`consumer_rates_cny`，§3.3），得改成"结算时按命中候选的成本计价"，并接受实收随渠道变。

现状确实随命中候选变：`pricing.price_plans` 按候选挂、费率就是结算基数（§3.1）——默认读法要改掉的正是这一点。

### 3.5 毛利记录

- **对客结算只读 Job 固化的费率快照**：实收 = `price_snapshot.consumer_rates_cny`（对客四档 token 费率，**CNY**）× **实际 `usage` 的分项 token（真值）**，**不封顶在保底额**（§3.3/§3.6：按实际扣费，超出部分在结算时透支）。`attempts` 里的渠道成本**只用于毛利核算**——它**不参与对客结算**，不改对客金额，也不改预授权额（§3.6）。
- **售价**：`ledger.entries`（`kind = 'capture'`，金额为负）+ `ledger.holds`（授权额）——账本是权威（`ADR-0003`）；另在 `generation.jobs` 加 `charge_microusd` 列（结算时写入）作为**投影**，便于按 job 直接查，权威仍是账本。**这一列缓做**：账本已经查得到，它只是查询便利，不阻塞任何切片（§9 的 P2b）。
- **成本**：`generation.attempts` 新增 `provider_cost_microusd`（**USD**）、`provider_cost_currency`、`provider_cost_source`（**两态 + 异常态**：`computed` / `declared` / `unavailable`，判据见 §3.7；`computed` = 实际分项 token × 四档费率自算，`declared` = 直接取上游 `cost`）与 `provider_cost_cny_microusd`（**折算后 CNY**，用快照的 `fx_rate_usd_cny` 折出，毛利用）。**异步写入**（结算时才拿得到）。
- **毛利** = 售价（快照，**CNY**）− 成本**折算后 CNY**（`attempts.provider_cost_cny_microusd`，由美元原值 × 快照的 `fx_rate_usd_cny` 折出），按 job 可查；**两条线分开留痕**（售价/扣费记 CNY、成本记 USD 原值 + 折算汇率 + 折算后 CNY，§3.8）；成本缺失（`unavailable`）时标"成本未知"，不猜（§3.7）。
- **边界**：把成本**写进账本**（`ledger.entries` 的 `adjustment` 分录）与账实核对归工单 [`#11`](https://github.com/dehuadong/seeaihub-server-next/issues/11)（成本进账本与账实核对那部分），**本设计不做**——同一件事不做两遍，也不在这里预先决定成本条目的会计语义。

### 3.6 预授权只是保底：按供给查保底表冻结，结算按实际、可透支

现状：`max_cost_microusd` 是服务端固定数（`GENERATION_MAX_COST_MICROUSD`，默认 $0.02，读在 `apps/api/src/main.rs`；**币种语义为 CNY**，§3.8），受理时按它扣预授权；结算时 `charge > max_cost` ⇒ 进对账（`crates/application` 的 `complete_success`）。

本设计的口径（**用户更正，上一轮的"预授权由售价派生"作废**）：**预授权只是保底**——受理时按**保底表**冻一笔**保底额**，结算**按实际扣费**，**实际超过保底额时余额可为负（透支）**。

**保底额从哪来：按供给（vendor + offering）维度查随修订发布的保底表**（`floor_amounts`，§1.6）——不同 vendor / offering 计价不同，所以保底额**分别设定**，**不编进代码**、随修订发布、随 Job 快照冻结：

- **保底表挂在供给维度**：键是**候选供给**（`vendor` + `offering`），受理时已经选定了候选（`select_candidate` 在 `create_job` 之前，§4.2），所以供给当场可查；
- **供给内按 `(size, quality)` 两维给保底额**：**`size` 与 `quality` 是主要影响因素**；`quality` 维**支持但可留空备用**——**留空即按 `size` 档**（OpenAI 系当前只按 `size` 填：**1K = ¥0.16、2K = ¥0.25、4K = ¥0.3**，**币种＝CNY**，§3.8）；
- **`resolution` 不作键**：它是 **APIMart 的包装参数**（合同里没有档位形态，承载面不声明它，§1.6）；
- **`size = auto` ⇒ 取该供给保底表里的最大档**；
- **表里没有这个档位 ⇒ 回落该供给的封顶保底值**（同一条供给录入的兜底数）；
- **连封顶保底值也没有 ⇒ 回落 `GENERATION_MAX_COST_MICROUSD`**（即**今天的行为**）；
- 查表结果随快照冻结（`hold_microusd` + `hold_source`，§3.3），事后可辨"这次为什么冻这么多"。

**保底表只用于预授权，不用于计价**：`tier_prices`（档位 → 每张价）**降级为定价参考/展示**，**不参与预授权**（§1.6/§3.3）。

**保底额的两个身份**（用户澄清）：① **准入闸门**——受理时 `balance_microusd >= $保底额` 才放行，**不成立 ⇒ `insufficient_balance`、对客 402 余额不足**（§7.4；**这条硬拒绝保留**）；② **结算的参考下限**——它不是精确值：**估小了由结算透支吸收**（实际 > 保底额 ⇒ 差额把余额扣成负数），**估大了结算释放差额**（实际 < 保底额 ⇒ 差额退回余额）。

**结算按实际，不封顶在保底额**（§3.3/§3.5）：

- **对客实收** = `consumer_rates_cny`（对客四档 token 费率，**CNY**）× **实际 `usage` 的分项 token（真值）**；**不再取 min(算出额, hold)**——保底额只是预授权，实际多少就扣多少；
- **成本侧**按 `cost_basis` 取数（§3.7，**USD**）：`Computed` = 实际分项 token × 四档渠道成本费率自算；`Declared` = **直接取上游 `cost`**（更权威、含折扣）；
- **余额可为负（透支发生在结算，不在受理）**：实际超过保底额时余额被扣成负数，这是**允许的结果**，不是错误；**下一次受理按当时的余额判**（可能已为负）⇒ 402。透支的追补属**运营 / 充值流程**（本设计不展开）。

`GENERATION_MAX_COST_MICROUSD` **只作"连供给封顶保底值都没有时的兜底保底额"**，不再是任何形式的上限：

- **查得到保底额 ⇒ 它不参与判定**。此前写的"派生出的 hold 超过它就受理前拒绝"**已删除**：它的默认值只有 $0.02，当上限用会把正常请求全拒（用户指出）。**不截断、不拒绝、不写审计**——查得到时这个数一次都不读。
- **兜底链**：① 供给档位查表 ⇒ ② `size=auto` 取该供给最大档 ⇒ ③ 该供给封顶保底值 ⇒ ④ **平台级默认 `GENERATION_MAX_COST_MICROUSD`**（快照里**缺 `hold_microusd`** 时——已受理的历史 Job，或迁移后仍生效但**没有定价**的旧修订受理出的新 Job，§3.3）。
- **它的正当用途就是这一条**：**连供给封顶保底值都没有时的兜底保底额**——不是"超限即拒"的门槛。

**受理的唯一上限是客户余额**：`balance_microusd >= $保底额` 不成立 ⇒ `insufficient_balance`，对客 **402 余额不足**，不产生 Job、不扣款（§7.4）。因此"售价高过某个服务端固定数"不是拒绝理由，"余额不够"才是。**运营要设成本护栏（服务端成本上限）属 [`#9`](https://github.com/dehuadong/seeaihub-server-next/issues/9)，不在本设计**（§10 范围边界）。

**这一处就地修订 `ADR-0009`**：它写"预授权金额与计价口径是两个量，不得互相推导：预授权由调用方给出的 `max_cost_microusd` 决定，不由候选价格反算"。本设计的读法不同（预授权由**供给维度的保底表**查得，不由候选价格反算，也不由调用方自报），而且该句现状与仓库也不一致（`GenerationService` 用的是服务端固定数）。按仓库约定**先经用户确认并修订该 ADR**（§10 的修订清单），本设计不自行改 ADR。

### 3.7 落地改动的边界（一处新成本事实）与"成本来源可辨"

采集 APIMart 的 `cost` 需要同时改三处：`adapter-sdk` 的 `ProviderSuccess` 新增可选 `declared_cost`（+ 币种）、`adapter-apimart` 从任务终态读出并回传、`generation.attempts` 增列。**这不新增"金额型计量证据"**：计量事实仍是四分项 token（`ADR-0006`），`cost` 只作成本口径——`ADR-0006` 明确允许它"只用于核成本"。

**成本来源怎么判（判据是"成本从哪来"，不是"金额对不对"）**：

| `provider_cost_source` | 判据 | 谁是这样 |
| --- | --- | --- |
| `declared` | 渠道在终态**直接给了 `cost`**，且解析成功（金额与币种都拿得到）——**直接取它，不需要我们自己算**（比自算更权威、含折扣） | APIMart（实测 `cost = 0.011354`，`ADR-0006` 的渠道口径） |
| `computed` | 渠道**不给金额字段**，平台按**实际 `usage` 的分项 token × 四档费率**自算 | AIHubMix |
| `unavailable` | 声明了但**缺字段 / 负数 / 解析失败**，或该次执行根本没拿到终态金额 | 任一渠道的异常情形 |

**`unavailable` 的处置（"APIMart 未声明 `cost` 或解析失败"）**：**不得猜测**——不记 0、不用"token × 费率"顶替、不用上一次的值（`ADR-0006`：证据缺字段、负数或解析失败时不得猜测费用，进对账）。具体是：`provider_cost_microusd` / `provider_cost_currency` 留 NULL、`provider_cost_source` 记 `unavailable`，该笔**成本缺口进对账**、毛利标"成本未知"，人工核对上游账单后再补录（补录与账实核对归 `#11`）。

它**不**把 Job 推进 `reconciliation_required`：那个状态是"受理/执行状态不明"，会把消费者的钱扣在对账里（`ADR-0006`：对账只能退款），而成本缺口是**平台侧的账务缺口**——对客结算照常按费率快照完成（§3.5）。

### 3.8 两个币种平面：对客 CNY，成本 USD

**平台对客只有人民币（CNY）单币种**（用户澄清："用户充值难道还是多币种？"）——用户充值、余额、售价、保底额、扣费**一律人民币**；**美元（USD）只是平台与渠道之间的结算口径**（四档 token 费率 $5/$10/$8/$30 每 1M、上游返回的 `cost`），**与用户无关**。两个平面**分开记、分开核**：

| 平面 | 币种 | 包含 | 落在哪 |
| --- | --- | --- | --- |
| **对客平面** | **CNY** | 充值、余额、售价（对客四档 token 费率 / 档位价目表）、**保底额（hold）**、扣费（实收） | `ledger.accounts.balance_microusd`、`ledger.entries`、`PriceSnapshot.consumer_rates_cny` / `tier_prices` / `hold_microusd`、`runtime_revisions.consumer_rates_cny` / `floor_amounts` |
| **成本平面** | **USD** | 渠道成本（`Computed` = `usage` 分项 × 四档**美元**费率；`Declared` = 上游 `cost`） | `generation.attempts.provider_cost_microusd` / `provider_cost_currency`、`runtime_revisions.reference_cost_microusd` |

- **对客金额不做实时汇率换算**：受理与结算**只读 CNY 的快照**（`consumer_rates_cny` × 实际 `usage` 分项 token；保底额直接就是 CNY），全程不出现美元；
- **汇率只用于把美元成本折成人民币**，服务于**毛利核算**（售价 CNY − 成本折算后 CNY）：`pricing.fx_rates` 的汇率是 **USD → CNY**，受理时随快照冻结（`fx_rate_usd_cny`），**不参与对客金额的计算**（§3.2）；
- **售价由后台按网关模型设定、以 CNY 表达**："参考渠道成本(USD) × (1 + 加价系数) × 汇率" 若用，**只是后台定价时的参考算法**，最终落在 **CNY** 的售价上（§3.2）；
- **保底额是 CNY**：OpenAI 系 **1K = ¥0.16 / 2K = ¥0.25 / 4K = ¥0.3**（**币种＝CNY，已确认**，§1.6/§3.6）；
- **快照写清两条线**：**售价 / 保底 / 扣费记 CNY**；**成本记 USD**，并记**折算汇率**与**折算后 CNY**（毛利用）——两条线分开，便于核对；
- **既有列名的币种语义**：`ledger.accounts.balance_microusd` 与 `GENERATION_MAX_COST_MICROUSD` 的**币种语义为 CNY**（列名里的 `usd` 是历史命名；实施时可按需改名，语义以本节为准），`reference_cost_microusd` / `provider_cost_microusd` 属**成本平面**、币种 USD。

## 4. 路由

### 4.1 排序（现状，不改）

`routing_priority` = 发布时候选数组的下标（`crates/application` 的 `normalize_array`：`routing_priority = index`），数字小者优先。`ADR-0009`：选中顺序是**发布决定**，不由请求参数、Adapter 或价格决定。

### 4.2 权重（新）：档位内分流，不是内置择优

- `weight`：正整数，随候选发布，默认 `1`。
- **语义**：**优先级是档位，权重只在同一档内分流**。受理时按 `routing_priority` 升序找到**第一个至少有一条合格候选的档**，在该档的合格候选里按 `weight` 分摊。
- **分摊用确定性哈希，输入是 `(account_id, idempotency_key)`**：`hash(account_id ‖ idempotency_key)` 映射到 `[0, Σweight)`，落在哪条候选的区间就选哪条。不用随机数发生器。理由：判定可复现、离线可断言分布、`routing_decisions` 事后能重建"为什么是它"（`ADR-0009` 要求判定记录可重建）。
- **为什么不是 job id**：选路发生在生成 JobId **之前**——`GenerationService::create` 先 `select_candidate`、再 `create_job`，JobId 是在 `create_job` 里才 `JobId::new()` 出来的（`crates/persistence`）。拿一个当时还不存在的值当哈希输入是因果倒置；而 `(account_id, idempotency_key)` 在受理前就已知。
- **重放语义**：同一个 `(account_id, idempotency_key)` 的**重放必然分到同一条候选**——哈希输入相同 ⇒ 分流结果相同；而 `create_job` 本来就把同键重发去重成原 Job（`UNIQUE (account_id, idempotency_key)`），所以重放既不改选路、也不新建 Job、不重复计费。**不同幂等键各自独立分摊**，哪怕在同一个账户下。`account_id` 也进哈希：幂等键只在自己账户内唯一，不同账户用同一个键时不该相关。
- **权重不做的事**：不改变档位顺序、不看价格、不看健康度/延迟/成功率。因此它**不是** `ADR-0015`/`ADR-0009` 禁止的"核心服务内置价格、优先级或健康度择优"——它只是发布者给出的**分流比**，与 `routing_priority` 同为发布数据。
- **`weight` 是策略的输入**（§5.4）：它只在生效策略消费它时起作用；**未配置策略时默认 `priority_failover`，权重按本节口径在同档内分流**（＝今天的行为）。
- **索引调整**：现有 `one_active_entry_per_model_and_priority`（`(gateway_model, routing_priority) WHERE active`）不允许同档多候选，与"档内分流"冲突 ⇒ 换成 `(gateway_model, offering_id) WHERE active`。**这一处直接改动 `ADR-0009` 的操作性条款**（原文写的是"数字小者优先**且同一型号内唯一**"），因此按仓库约定需要**先经用户确认并修订该 ADR**（§10 的 ADR 候选清单），本设计不自行改 ADR。
- **换索引后的守卫**：新索引只防"同一次发布里同一个 Offering 出现两行"，比原索引弱——"同一名字的生效条目来自同一修订"由发布事务的原子替换（`UPDATE runtime_entries SET active = false WHERE active AND gateway_model = $1`）加 `crates/persistence` 里既有的"active 候选跨修订即报错"防御共同保证，不靠索引。
- 若用户要的其实是"权重只作排序的次级依据"，那与"优先级唯一"可以并存，但同档只有一个候选、权重不起作用（§10 未决里的"权重语义"那条）。

### 4.3 回退链与全不合格

- 逐条候选试：请求**实际用到**的字段必须被该候选承载（`docs/design/0005` §4 的 **R5**），不合格 → 试下一条；
- **全部候选都不合格** → `ApplicationError::NoEligibleOffering` → 对客 **503 `platform_unavailable`**（`ADR-0017` 责任方原则：这是平台供给面问题，不是参数错），**不改**；
- 该型号没有任何生效候选 → `NotFound`（对客"模型不存在"）。

### 4.4 「不可用时回退」按阶段判定（重要）

用户原话里的"APIMart 优先，**不可用时**回退 AIHubMix"，必须先回答一个更基本的问题：**"不可用"发生在哪一步**。能不能回退、回退之后会不会重复出图与重复计费，只取决于**这次上游请求有没有可能已经被受理**——而这由"失败发生在哪个阶段"决定，不由错误码是否好看决定。

| 阶段 | 情形 | 能否回退另一候选 | 理由 |
| --- | --- | --- | --- |
| **受理前（选路）** | 承载面表达不了这次请求、分支/张数不被该候选允许（§4.3 的 R5 不合格） | **能**，无副作用 | 还没发出任何上游请求 |
| **提交前（可证明未受理）** | 连不上、`401`/`402`/`403`（凭证/额度）、`400`/`422`（参数）、`429`（明确未受理） | **技术上能**（上游没接单，不会重复出图），但**当前策略是"失败不重试"**（`ADR-0011`）；改成"回退下一候选"属**策略变更**，归 [`#11`](https://github.com/dehuadong/seeaihub-server-next/issues/11) | 上游没接单，不存在"已出图并已计费"的风险 |
| **提交后（不确定）** | **超时、连接中断、`5xx`、拿不到响应** | **不能** | 上游**可能已经出图并已经计费**；再让另一家出一张 = **平台成本翻倍**、客户可能拿到两张或对不上账——这就是"**重复出图、重复计费**"的确切含义。现状处置是 `reconciliation_required` 人工对账 |

**判据（`4xx` / `5xx` 的分界）**：

- **`4xx` = 确定性拒绝、可证明未受理**。细分：`401`/`402`/`403`（凭证或额度）对客按**平台侧故障**处理（`ADR-0017`）；`400`/`422` 是渠道对参数的拒绝；`429` 是"明确未受理"的限流。它们的共同点是**上游不会因此出一张图**。
- **`5xx` 与超时/断连 = 不确定**：请求可能已经落地执行。**不得改道、不得自动重提**（`ADR-0009`/`ADR-0011`），一律进对账（`reconciliation_required`）。
- 分界的意义：**"能不能回退"看的是"能否证明上游没受理"，不是"错误码是 4 开头还是 5 开头"**——`4xx` 只是目前唯一能给出这种证明的一类；某条 `4xx` 若无法证明未受理，同样按"不确定"处理。

**本设计的落地**：只做**受理前的候选不合格回退**（现状，§4.3 的 R5 与"全部候选都不合格 → 503 `platform_unavailable`"）；**运行期回退**（提交前改道下一候选、提交后改道）**本设计不做**——"提交后"被 `ADR-0009` 明确禁止（"一次 Attempt 一旦进入 `submitting`，就禁止改选 Offering 或 Channel——上游可能已生成并计费，改选等于重复出图与重复计费"），"提交前回退"是策略变更。**"运行期回退"是否要做，属 [`#11`](https://github.com/dehuadong/seeaihub-server-next/issues/11)，不在本设计范围**（§10 已定案同步）。

### 4.5 路由判定记录

`generation.routing_decisions.considered` 每项增加 `weight` 与本次的**分流取值**（按权重选中的依据：`hash(account_id ‖ idempotency_key)` 的落点；哈希输入本身已随 Job 落库，不重复记），使"当时考虑过谁、为什么跳过、为什么是它"事后可重建（`ADR-0009`）。

## 5. 路由策略层

**定位：策略是运营配置，不是发布内容。** 用户确认「路由这层是**运营的事**」——本设计只提供**机制**：**选哪种策略、填什么折扣、给谁打什么标签都由运营在后台配**，设计不把任何一种策略写成产品决策（不写"APIMart 优先"这类结论）。因此：

- 策略存 `route_policies` 表，**运行期可改、改完即时生效**；
- 策略**不进不可变修订**（`runtime_revisions` / `runtime_entries`）——它不是"卖什么"的定义，而是"在已发布的合格候选里怎么选"的运行期配置；
- **已受理的 Job 不受后续改策略影响**：受理时选定的候选随 Job 固定（`ADR-0003` 的"受理时固定版本"，与 §3.3 的售价快照同一条道理）；改策略只影响之后的受理。

### 5.1 作用域与默认（零配置下行为不变）

- **作用域两档**：**全局一条** + **可按网关模型覆盖**（`route_policies.gateway_model` 为空即全局、非空即覆盖该名字；按模型取"有覆盖用覆盖、没有用全局"）；
- **未配置时默认 `priority_failover`**——按 `routing_priority` 数字小的优先、该档没有合格候选时依次降级、**同一档内按 `weight` 分摊**，即 **§4.1/§4.2 今天的行为**。因此**零配置下行为与今天逐位相同**：策略层是"加了旋钮"，不是"换了引擎"，没有配置就没有新行为。

### 5.2 策略类型（举例，机制上可扩展）

| 策略 | 怎么选（取值空间**只有合格候选**） | 吃什么输入 |
| --- | --- | --- |
| `priority_failover`（**默认**） | 按 `priority`（＝`routing_priority`）数字小的优先；该档没有合格候选时依次降级；**同档的合格候选按 `weight` 分摊**（＝ §4.2 现状） | `priority`、`weight` |
| `weighted_random` | **不看 `priority`**，在**全部合格候选**里按 `weight` 分摊 | `weight` |
| `least_cost` | 在合格候选里取**折后成本估算**最小的一条 | `discount_rate`（+ 成本口径，§5.4） |
| `user_tag` | 按**账户标签**指定渠道：标签命中哪条合格候选就用哪条 | 账户标签 |

**策略层不放宽"不可用"的边界**：它只在**受理前**决定"选哪条合格候选"；**运行期回退**（上游请求已发出之后改道）仍按 §4.4 的按阶段判定——提交前回退属策略变更、提交后回退被禁止（重复出图与重复计费）。"不可用"在选路这一步的含义就是 §4.3 的候选不合格，不由策略另立一套。

### 5.3 三条硬约束（写死，任何策略都不得越过）

① **候选合格性优先于策略**：承载面表达不了这次请求、分支/张数不被该候选允许的候选（§4.3 的 R5 不合格）**先被排除**；**任何策略都不得选中不合格候选**——策略的取值空间**只有合格候选**，没有例外、没有"策略指定了就绕过承载校验"这回事。全部候选都不合格时仍是 §4.3 的结果（503 `platform_unavailable`），策略不改这个结论。

② **策略只决定"选哪条合格候选"**：它**不改写参数映射与承载面**（`docs/design/0005` 的合同 / 承载面 / 映射分层），也不改请求参数、不改对客价格、不改成本口径——选路之外的一切仍由发布数据与 §3 的定价口径决定。

③ **分流确定性可重放**：`weighted_random` 按 **`(账户, 幂等键)`**（`account_id` ‖ `idempotency_key`）哈希取落点，**同一请求重放必落同一条候选**——与 §4.2 已有的口径一致（同一个哈希输入、同一套区间划分），不引入随机数发生器；选路结果照旧写 `routing_decisions`（§4.5），事后可重建。

### 5.4 策略的输入：`priority` / `weight` / `discount_rate` / 账户标签

这四个量都是**策略的输入**，**不是各自独立生效的行为**：`priority` 单独不做事，只有 `priority_failover` 消费它；`weight` 单独不做事，只有 `priority_failover`（同档内）与 `weighted_random` 消费它；`discount_rate` 只有 `least_cost` 消费；**账户标签**只有 `user_tag` 消费。因此**改这些输入本身不改变选路**，除非某条生效的策略正在消费它。

**折扣率的定位（用户明确纠正）：折扣率不进成本。** 成本**永远记实际扣费**，**不乘折扣率**：

- `Declared`（上游直接给 `cost`）：成本就是上游那个 `cost`（§3.1/§3.7）；
- `Computed`（上游不给金额字段）：成本 = **实际 `usage` 分项 token × 费率**（§3.3/§3.5）。

**折扣率只作 `least_cost` 的比较输入**：`least_cost` 比的是**折后成本估算**（在参考成本之上乘折扣率这类**估算**），**是估算、不是事实**。**两者不一致时以实际扣费为准**——策略只吃估算，**不改写成本事实、不进账本、不改对客金额**：`attempts` 的成本列仍按 §3.7 记**实际扣费**，对客金额仍只读受理时冻结的费率快照（§3.3/§3.5）。折扣率按**候选供给（vendor + offering）**配（与保底表同维度），与策略同属运营配置（运行期可改、即时生效、**不进修订**）；**填多少由运营决定，不属设计决策**。

**账户标签**：为支持 `user_tag`，**账户上加标签字段**（机制要做：账户对象 `ledger.accounts` 增列；**标签值由运营设**）；它属**管理员面配置**（管理员 API 写入、写审计），**不是发布内容**。标签是策略的输入，本身不改变任何受理结果——没有生效的 `user_tag` 策略时，标签不影响选路。

### 5.5 缓存与生效（改策略不需要发修订）

- **策略读取与 route 缓存同一条口径**：缓存值带**策略版本标识**，受理时比对，**不一致（或值里没有这个标识）即当未命中、回源 DB**（与 §7.2 的 route 缓存按 `runtime_revision_id` 比对同构）；
- **改策略不需要发修订**：策略写入（管理员 API）成功后失效并重建缓存即可——**不进不可变修订、不需要 `POST /api/v1/runtime-revisions`**，也不产生新的修订标识；
- Redis 不可用时直查 DB（§7.1），策略读取同样降级，**正确性不依赖缓存**。

### 5.6 与 `ADR-0009` 的关系：已立 `ADR-0020`，不改旧条原文

`ADR-0009` 与 `ADR-0015` 现状写的是"**选中顺序是发布决定**、不由请求参数 / Adapter / 价格决定"与"**核心服务不内置价格、优先级或健康度择优**"。**路由策略层与这两句冲突**：`least_cost` 就是按价格（折后成本估算）选，`weighted_random` / `user_tag` 也不是"发布决定"。

处理方式：**已立** `docs/adr/0020-routing-strategy-layer-configured-by-operations.md`（**`ADR-0020`**；承接"选路可以由**运营配置的策略**决定，但**候选合格性优先于策略**、**分流确定性可重放**"），**不改 `ADR-0009` / `ADR-0015` 的旧条原文**，两条旧 ADR 的实际状态是：

- **`ADR-0009` 部分被取代**：**原文与结论段一字未改，仅在末尾追加标注**；它定的"**哪些候选存在、按什么顺序**"（一次发布携带完整有序候选集、合格是合取判据、不合格在调用上游前失败、`submitting` 后禁止改选）**继续成立**，被补上的只是它没定的那一半——"在一批合格候选里挑哪一条"；
- **`ADR-0015` 的"核心服务不内置择优"仍成立**：策略是**运营配置**（运行期可改、不进修订），不是核心服务里硬编码的价格 / 优先级 / 健康度规则。

`docs/design/0005` 里的两处旧口径**已随本层落地**（属**设计级修订**——改 `docs/design/`，不动 ADR）：

- **§8 第 2 条**由"路由策略层：暂不引入"改为**已引入**，指向 `ADR-0020` 与本节；
- **§4 的 R5 登记**由"等路由策略层落地时一并改"改为**已落地**：**R5 的语义不变**——**候选合格性优先于策略**，承载面表达不了、分支 / 张数不允许的候选**先被排除**；变的只是"在合格候选里挑哪一条"由**运营策略**决定（§5.3 ①）。

## 6. 记录与日志：六项信息现在落在哪

提案第 3 条要求的六项，逐项对照现状：

| 需要的信息 | 现在落在哪 | 缺什么 / 补什么 |
| --- | --- | --- |
| **模型** | `generation.jobs.gateway_model`（迁移 0004 已把列名收口为"平台型号名"） | 无需新列；语义随本设计变成"网关模型名" |
| **张数** | 请求侧：`jobs.native_parameters->>'n'`；结果侧：`jobs.result_images` 的数组长度 | 两个口径都要写死：**预授权（保底额）按供给查保底表**（不按张数乘单价，§3.6）；**实收（charge）按实际 `usage` 的分项 token**（**两家渠道都是 token 计费**，不按张数，§3.3/§3.6），**不封顶在保底额**（超出部分在结算时透支）。张数两处都在库里，不新增列，作为记录与核对信息保留 |
| **扣费金额** | `ledger.entries`（`kind='capture'`）+ `ledger.holds`（授权额）；**Job 上没有** charge 列。**币种 CNY**（对客平面，§3.8） | 账本是权威（`ADR-0003`）；**补** `generation.jobs.charge_microusd`（结算时写入）作为投影，便于按 job 查——**缓做**（§3.5） |
| **平台成本价** | **没有**：APIMart 的 `cost` 不采纳不留存；AIHubMix 的金额要自算也没存 | **补** `generation.attempts.provider_cost_microusd`（**USD**）/ `provider_cost_currency` / `provider_cost_source`（**两态 + 异常态**：`declared` 直接取上游 `cost`、`computed` 按实际 `usage` × 四档费率自算、`unavailable` 不猜，§3.5/§3.7）与 `provider_cost_cny_microusd`（**折算后 CNY**，毛利用，§3.8） |
| **请求时间戳** | `jobs.created_at`（受理）、`attempts.started_at` / `completed_at` | 够 |
| **上游 request_id** | `attempts.provider_trace_id`（AIHubMix 的 `x-request-id`；APIMart 的 task id） | 够 |

### 6.1 异步路由日志的落点

"异步记录路由日志与渠道成本"由**两张既有表**承担，不新建日志表：

- `generation.routing_decisions`——**受理时同步**写（候选、优先级、权重、取舍原因、被选中者）；
- `generation.attempts`——**执行后异步**写（渠道原始错误、对账标识 `provider_trace_id`、计量证据，本次再加**渠道成本**）。

不合并成一张表：两张表的事实归属不同（受理事实 vs 执行事实），合并会造出第二个权威（`ADR-0003`）。

### 6.2 不开查询接口

用户已撤回"调用记录查询"。上面的字段是**为了查得出来**（运营侧直查，或后续再开接口），不是现在开接口。现有的**对账清单**（`GET /api/v1/reconciliation-cases`）与**平台侧失败清单**（`GET /api/v1/provider-failures`）够用。

## 7. Redis

### 7.1 定位

**缓存，不是事实源。** `ADR-0003` 逐字适用：目录、发布、Job、结算与审计的事实权威是 PostgreSQL。因此本设计的硬约束是：

- 所有**金额判定**与**选路结果**的正确性**不依赖 Redis**（唯一一处"缓存判定直接决定对客响应"的是 §7.4 的"凭新鲜缓存提前拒绝"：只读、无副作用、必留审计，且**扣减与余额事实仍只在 PG 里发生**，因此**不是**对 `ADR-0003` 的例外）；
- Redis 不可用时平台**照常工作**（降级直查 DB），只是变慢；
- 缓存与 DB 不一致时，**以 DB 为准**。

### 7.2 缓存什么

| 键 | 值 | 失效 |
| --- | --- | --- |
| `route:<gateway_model>` | 生效修订的候选集：合同、承载面、参数映射、限制、`routing_priority`、`weight`、定价输入（`reference_cost_microusd`、`markup_bps`），**加发布修订标识 `runtime_revision_id`** | 发布成功后主动失效；另设 TTL |
| `route_policy:<gateway_model\|global>` | 生效策略：策略类型、作用域、折扣率表与账户标签命中规则，**加策略版本标识** | 改策略成功后主动失效；另设 TTL |
| `api_key:<sha256(key)>` | `account_id` | 吊销时主动删；另设 TTL |
| `user_balance:<account_id>` | 余额（**CNY** 微单位，**可为负**——透支发生在结算，§3.6/§3.8）+ 写入时间 + **来源标记**（`db_commit` / `reconciler`，§7.4） | 充值/受理/结算后立即写；另设 TTL |

**route 缓存的值必须带发布修订标识，受理时比对（否则陈旧不可检）**：

- 值里带 `runtime_revision_id`——写这条缓存的**那次发布**的修订标识；
- 受理时先做一次**轻量 DB 读**取"当前生效修订标识"（按 `gateway_model` + `active` 取 `runtime_revision_id`，走 §4.2 换过的新索引），与缓存值里的标识**比对**：一致才用缓存，**不一致（或值里根本没有这个标识）就当未命中，回源 DB** 取候选集并重建缓存。因此 route 缓存的陈旧是**可检的**，不再依赖"发布后的失效一定成功"；
- 同时按主键读一行 `publication.gateway_models.enabled`（§2.1 的判据）：`enabled=false` 是可变表里的事、**不改变修订标识**，所以这一项不交给 route 缓存判定，`PATCH` 之后照旧从目录与受理里消失；
- **策略缓存同构**：`route_policy` 的值带**策略版本标识**，受理时比对，不一致即当未命中、回源 DB（§5.5）；**改策略不需要发修订**，它的失效只由策略写入路径触发。
- **SET / 失效失败时的行为**：失败**不影响正确性**——失败只是让缓存里留着旧值（带旧修订标识），下一次受理的比对必然不一致 ⇒ 回源 DB；`PATCH enabled` 的失效失败同理，由受理时那一次按主键读 `enabled` 兜住。失败本身记一条日志，运营可发现（`ADR-0017`）；
- **可接受的陈旧窗口**：**正确性上的窗口是 0**——陈旧缓存永远不会被用来选路，因为受理时一定会比对修订标识。剩下的只是"命中率"窗口：发布后到下一次受理之间缓存里可能还是旧值，最长 TTL（默认 60 秒）内每次受理都会回源一次并重建缓存。TTL 因此只用来兜住"发布后没人调用、旧值白占内存"与"缓存层自身故障后的恢复"，不影响任何对客结果。

### 7.3 写入时机（先 DB，后 Redis，且只在提交成功后）

- **发布成功**（事务提交后）：失效并重建 `route:<gateway_model>`；
- **充值**：DB 事务提交后 `SET user_balance:<id>`（用户要求：充值后立即 `SET`），来源标记 `db_commit`；
- **结算**：DB 事务提交后 `SET user_balance:<id>`（用户要求：扣减成功后立即 `SET`），来源标记 `db_commit`。写的是**扣减后的余额值**，不用 `DECRBY`——`DECRBY` 表达不了"以 DB 为准"，重放还会漂移；
- **受理（预授权扣减）**：DB 扣减成功后同样刷新（来源标记 `db_commit`），否则缓存会滞后一个预授权额；
- **改策略成功**（事务提交后）：失效并重建 `route_policy:*`（§5.5）——**改策略不发修订**，这条失效没有"发布"事件可依附。

### 7.4 扣费流程：Redis 只做加速，权威在 DB

**受理**（现状，`crates/persistence` 的 `create_job`，本设计只改"预授权额从哪来"）：

```sql
UPDATE ledger.accounts SET balance_microusd = balance_microusd - $保底额
WHERE id = $1 AND balance_microusd >= $保底额
```
`rows_affected != 1` ⇒ `insufficient_balance`（对客 **402 余额不足**）——**这条硬拒绝保留**（用户澄清）。`$保底额` = **按供给维度查保底表**（§3.6：键是 vendor + offering，档位 `(size, quality)`；`size=auto` 取该供给最大档；缺档回落该供给封顶保底值，再回落 `GENERATION_MAX_COST_MICROUSD`），不再是"永远一个固定数"。**余额是这里唯一的上限**——查得到保底额时 `GENERATION_MAX_COST_MICROUSD` 不参与判定（§3.6）。**下一次受理按当时的余额判**：结算透支后余额可能已为负，此时 `balance_microusd >= $保底额` 不成立 ⇒ 同样 402。

**结算**（现状，`complete_job`，本设计只改实收口径）：同一事务里 `release` 剩余授权 + `capture` 实收。**实收 = 对客四档 token 费率（CNY）× 实际 `usage` 的分项 token**（§3.3/§3.5），**不封顶在保底额**——**实际 > 保底额时差额由余额吸收，`capture` 之后余额可为负（这才是"透支"：发生在结算，不在受理）**；实际 < 保底额时差额释放回余额。透支的追补属运营 / 充值流程（本设计不展开）。**对客金额全程 CNY**（§3.8）。

Redis 的位置：

- 缓存命中且余额充足 → **仍走 DB 条件更新**（正确性在 DB，缓存只是少一次读）；
- 缓存命中且余额不足 → **这是全设计唯一一处允许"缓存的判定结果直接决定对客响应"的地方**：仅在缓存**新鲜**时提前返回 `insufficient_balance`；不新鲜一律交给 DB。

**"新鲜"的判据（两条都要满足）**：

1. 值带**来源标记 `db_commit`**——由 DB 提交后的写入产生（§7.3 的充值 / 受理预授权扣减 / 结算三条写穿路径）。定时对账写回的条目标 `reconciler`（§7.5），**不用于提前拒绝**；
2. 写入时间距当前 < **新鲜窗口**（默认 5 秒，可配），且这个窗口必须显著小于定时对账周期（默认 3 分钟），保证"能用来拒绝的值"实际都来自写穿路径。

来源不明、没有写入时间戳的旧格式条目，一律**视为不新鲜**（宁可多打一次 DB）。

**提前拒绝必须落审计**：每次提前拒绝写一条 `operations.audit_events`（`account_id`、缓存余额、写入时间与来源、判定结果），使误拒**可发现、可对账**（`ADR-0017` 的"平台侧事件必须可发现"）。

**这一处不需要新 ADR**（用户更正，撤回上一轮的"需新 ADR"）：Redis 在这里就是"判断用户余额做预检"，**扣费仍然只在 PG 里发生**，不涉及资金安全，因此 `ADR-0003` 的"缓存不是事实源"已经覆盖。设计上写明两条即可：

1. **缓存永不作为扣费依据**：扣减只在 PG 事务里做（受理的预授权扣减、结算的 `release` + `capture`，见本节开头）；Redis 的写入一律是"**DB 提交成功之后、写扣减后的值**"，**不是 `DECRBY`**（`DECRBY` 表达不了"以 DB 为准"，重放还会漂移）；不一致时**以 DB 覆盖**（§7.6）。缓存里"够不够"的结论**从不决定扣减**——扣减由 DB 的条件更新决定。
2. **预检拒绝要留审计**：预检**没有 DB 记录**（不建 Job、不扣款、不写状态），事后必须能解释"为什么拒了这个客户"——这是**可解释性**要求（落点就是上面那条 `operations.audit_events`），不是资金安全要求。

因此"Redis 说够、DB 说不够"由 DB 兜住；"Redis 说不够"只在新鲜窗口内发生、必然留下审计，且拒绝不产生任何副作用（不写状态、不扣款，调用方重试即可）。

### 7.5 定时对账兜底

独立定时任务，每 N 分钟（默认 3）：

1. 把 DB 的 `ledger.accounts.balance_microusd`（按 `updated_at` 增量，必要时全量）写回 `user_balance:*`，来源标记 `reconciler`（§7.4：这种条目**不用于提前拒绝**）；
2. 校正 `route:*`（以当前生效修订为准）；
3. 校正 `api_key:*`（以 `identity.api_keys.revoked_at` 为准）。

### 7.6 不一致的处置

**以 DB 为准**：发现 Redis 与 DB 不一致时，用 DB 的值**覆盖**缓存，并记一条 `operations.audit_events` + 一条日志（运营要能发现，参照 `ADR-0017` 对"平台侧事件必须可发现"的要求）。不尝试"合并"或"取中间值"。

### 7.7 引入成本与风险

新增一个运行时依赖（`compose.yaml` 加服务、`.env.example` 加地址、多一条故障路径）。**仓库今天完全没有 Redis**（全仓无任何 `redis` 引用）。**用户已批准引入**（纯加速层，见 §10「已定案」）；即便不引入，本设计其余部分也不受影响，只是余额与路由全部直查 DB（即现状）。

## 8. 不做

- **渠道/供给启停接口**（提案第 1 条，用户撤回）；
- **调用记录查询接口**（提案第 4 条后半，用户撤回）；
- **跨厂商统一图片参数语义**（`ADR-0015`：属后期独立规划）；
- **通用参数映射引擎**（沿用 `#10` 口径：只做"合同 → 该候选承载面"的校验与装载）；
- **发布的分步 CRUD / 草稿态**（§2.4 理由）；
- **自动重试与失败后改道**（`ADR-0009`/`ADR-0011`，属 [`#11`](https://github.com/dehuadong/seeaihub-server-next/issues/11)）；
- **成本进账本与账实核对**（[`#11`](https://github.com/dehuadong/seeaihub-server-next/issues/11) 的成本进账本与账实核对那部分）；
- **对外价策略本身与具体数值**（[`#5`](https://github.com/dehuadong/seeaihub-server-next/issues/5)）；
- **把 `config/bootstrap/*.json` 当后台**（用户明确：只作初始化种子与测试夹具）。

## 9. 切片与验收

每片**独立可验收**，且全部**离线**（进程内假上游 + 直接查库，见 `apps/api/tests/http_contract.rs` 的既有做法），**不产生任何计费调用**。

### P1 网关模型命名层与对客目录

**改动**：发布命令加 `gateway_model`；`runtime_revisions` 增 `gateway_model` / `vendor_model_id` **两列（P1 落的命名两列）**；`publication.gateway_models`；`GET /v1/models` 投射（网关名 + `vendor_id` + 替换 `model.const`）；`GET /api/v1/gateway-models`；`PATCH .../enabled`；发布期 `model.const` 校验。**定价列（`markup_bps` / `reference_cost_microusd` / `cost_basis` / `tier_prices` / `consumer_rates_cny` / `floor_amounts` / `fx_rate_usd_cny`）在 P2b 落**（§1.6 同一套口径：迁移**分两次**，P1 一次、P2b 一次）。

**验收（离线）**：
- 发布一份 `gateway_model = gpt-image-2.5-plus`、`native_model_id = gpt-image-2.5-sunburst` 的素材 → `GET /v1/models` 的 `name` 是 `gpt-image-2.5-plus`、含 `vendor_id`、**响应全文不含 `gpt-image-2.5-sunburst`**（含合同正文）；
- 用网关模型名能受理（假上游跑通）、用厂商原生名得到 404；
- `PATCH enabled=false` 后目录消失且受理 404，重新启用恢复；
- **旧素材**（两者同值，如现有 `config/bootstrap/gpt-image-2.5-flare.json`）发布后，`GET /v1/models` 与受理行为与今天逐位一致；
- 管理员读接口能列出候选、顺序、权重与定价，不需要直查库。

### P2a 成本事实采集（**不依赖任何未决**）

**改动**：`adapter-sdk` 的 `ProviderSuccess` 加可选 `declared_cost`（+ 币种）；`adapter-apimart` 从任务终态读出并回传；`generation.attempts` 增 `provider_cost_microusd` / `provider_cost_currency` / `provider_cost_source` 三列。**快照的成本口径列 `runtime_revisions.cost_basis` 不在这里**——它是**定价侧**的列（随修订发布、随快照冻结），归 **P2b**（§1.6/§3.3）；P2a 只落 `attempts` 这三列。

**为什么能先做**：它只落"上游说了什么"，不碰定价公式、汇率、Redis，也不依赖任何 ADR 修订，因此可以**单独实施、单独验收**（§3.7）。

**验收（离线）**：
- `declared` 路径：假上游终态**直接返回 `cost`**（用实测样例 `cost = 0.011354`）→ `provider_cost_source = declared`，**直接取它、不自己算**，金额与币种逐位落库；
- `computed` 路径：AIHubMix 不给金额字段 → `provider_cost_source = computed`，金额按**实际 `usage` 的分项 token × 四档费率**自算；
- `unavailable` 路径：声明了但**缺字段 / 负数 / 解析失败** → 金额列留 NULL、**不猜**（不写 0、不用费率顶替），该笔成本缺口可发现（§3.7）；
- 采集 `cost` **不改变**对客结算金额与计量事实（同一个用例里断言 `charge` 与今天逐位相同）。

### P2b 定价、保底表与售价快照

**改动**：`runtime_revisions` 增 `markup_bps` / `reference_cost_microusd`（USD）/ `cost_basis` / `tier_prices`（CNY）/ `consumer_rates_cny`（CNY）/ `floor_amounts`（CNY）/ `fx_rate_usd_cny`（**P2b 落的定价列**；`cost_basis` 两态 `Computed` / `Declared`；`tier_prices` 降级为定价参考、`floor_amounts` 是保底表，随快照冻结）；`pricing.fx_rates` + `PUT /api/v1/fx-rates`（**USD → CNY**，只服务毛利折算）；`PriceSnapshot` 扩展（`consumer_rates_cny` / `tier_prices` / `floor_amounts` / `hold_microusd` / `hold_source` / `cost_basis` / `reference_cost_microusd` / `markup_bps` / `fx_rate_usd_cny`）；`charge_microusd` 改为只读快照（**对客四档 CNY token 费率 × 实际 `usage` 分项 token**，**不封顶在保底额**）；**预授权 = 按供给维度查保底表**（`floor_amounts`：键 vendor + offering，档位 `(size, quality)`，`size=auto` 取最大档、缺档回落该供给封顶保底值、再回落 `GENERATION_MAX_COST_MICROUSD`；§3.6），受理仍按 `balance >= 保底额` 判准入（不足 ⇒ 402）；`jobs.charge_microusd` 投影列**缓做**。

**验收（离线）**：
- **成本来源两态各一条**：① `computed`——AIHubMix 不给金额字段 → 成本 = **实际 `usage` 分项 token × 四档美元费率**自算，`provider_cost_source = computed`；② `declared`——假上游终态**直接返回 `cost`**（实测样例 `cost = 0.011354`）→ **直接取它、不自己算**，`provider_cost_source = declared`，金额与币种（USD）逐位落库（§3.1/§3.7）；
- **两个币种平面分开**：断言对客侧（`consumer_rates_cny`、`hold_microusd`、`charge`、余额）**全是 CNY**、响应与账本里**不出现美元**；成本侧记 **USD** 原值，并同时记下**折算汇率**与**折算后 CNY**（`provider_cost_cny_microusd`）；**改一次汇率不影响对客金额**（只影响折算后的成本与毛利）（§3.8）；
- **保底按供给维度查表**：给定发布在**某条供给**（vendor + offering）下的保底表与请求的 `(size, quality)` → 受理时 `hold_microusd` **逐位等于**该档保底额、`hold_source` 记的是**供给档位查表**；**`size = auto` → 取该供给表里的最大档**；**表里没有这个档位 → 回落该供给的封顶保底值**；连封顶值也没有 → 回落 `GENERATION_MAX_COST_MICROUSD`（§3.6）；
- **`quality` 维留空即按 `size` 档**：保底表只填 `size` 维（OpenAI 系当前形态，`1K = ¥0.16` / `2K = ¥0.25` / `4K = ¥0.3`，**CNY**）→ 请求带任意 `quality` 都查到同一个 `size` 档保底额（§3.6）；
- **透支**：构造"结算实收 > 保底额"→ 受理照常（当时 `balance >= 保底额`），结算 `capture` 后 `ledger.accounts.balance_microusd` **可为负**；**随后用同一个账户再发一次请求 → 按当时（负）余额判 `balance >= 保底额` 不成立 ⇒ 402 `insufficient_balance`**，不产生 Job、不扣款（§3.6/§7.4）；**受理时 `balance < 保底额` 仍然是硬拒绝**；
- **对客费率是后台设定的 CNY 售价**：给定发布时录入的 `consumer_rates_cny` → 受理时快照**逐位相同**（不因运行时汇率变动）；后台用"参考成本 ×(1 + markup)× fx"算出来的**只是参考值**，与最终录入的 CNY 售价可以不同（§3.2/§3.8）；
- **同一网关模型**命中两条成本不同的候选 → 对客扣费**逐位相同**（默认读法，§3.4），差异只体现在毛利（用 P2a 的成本列断言）；扣的是**同一个账户**；
- **实收按实际、不封顶**：结算 `charge` = `consumer_rates_cny` × **实际 `usage` 分项 token（真值）**；**不取 min(算出额, hold)**——实收超过保底额时按实际扣，差额由余额透支吸收；实收低于保底额时差额释放回余额（§3.3/§3.6）；
- **旧修订 + 新 Job → 走旧口径**：迁移后仍生效、但**没有定价**（缺 `hold_microusd` / `consumer_rates_cny`）的旧修订受理出的新 Job，预授权**回落到 `GENERATION_MAX_COST_MICROUSD`**（今天的行为），结算也走旧口径（§3.3/§3.6）；
- **售价不受固定数限制**：构造 `保底额 > GENERATION_MAX_COST_MICROUSD` 的定价 → **照常受理**（不拒绝、不截断、不写审计）（§3.6）；
- 历史 `price_snapshot`（缺 `consumer_rates_cny`）的结算结果与今天**逐位相同**；
- 毛利 = 售价（CNY）− 成本折算后 CNY 可逐笔算出，`provider_cost_source` 区分 `declared` / `computed` / `unavailable`，各一个用例；
- 汇率改一次不影响已受理 Job（快照生效），也**不影响任何对客金额**。

### P3 权重与路由日志

**改动**：`weight` 列；唯一索引换成 `(gateway_model, offering_id) WHERE active`；档位内确定性哈希分流（输入 `(account_id, idempotency_key)`）；`routing_decisions` 记权重依据与分流取值。

**验收（离线）**：
- 同一档两条候选权重 1:3，**构造同一账户下不同的幂等键** → 分流比例落在确定区间，且**同一批输入可复现**；
- **同一幂等键重放 → 分到同一条候选**，且去重成原 Job（不新建、不重复计费）；
- 跨档时权重**不改变**档位顺序（档 0 有合格候选就一定选档 0）；
- 全部档都不合格 → 503 `platform_unavailable`（不是 400）；
- 判定记录能重建"考虑过谁、为什么跳过、为什么是它"。

### P4 Redis 加速层（**用户已批准**）

**改动**：缓存层、写入时机（含来源标记与新鲜窗口）、route 缓存值的发布修订标识与受理时比对、提前拒绝的审计、定时对账、降级路径。

**验收（本地 Redis，离线）**：
- 充值后缓存立即可见；结算后缓存立即可见；
- **停掉 Redis**：受理与结算结果与不停时逐位相同（降级直查 DB）；
- **route 缓存陈旧不可用**：发布新修订后让失效失败（或手工把缓存值里的修订标识改旧）→ 受理**回源 DB** 读到新候选集，选路结果与"完全没有缓存"时逐位相同；
- **陈旧缓存不得拒绝**：把缓存余额改低，但写入时间超出新鲜窗口（或来源标记为 `reconciler`）→ **不提前拒绝**，判定交给 DB；
- **误拒有审计**：构造"缓存说不够、DB 说够"且缓存新鲜 → 拒绝发生，同时留下一条 `operations.audit_events`（缓存余额、写入时间与来源）；
- 人为把缓存余额改错 → 定时对账以 DB 覆盖并留审计。

### P5 查看余额（管理员读）

**改动**：`GET /api/v1/accounts/{account_id}`（管理员，返回 `balance_microusd` 与 `updated_at`，读 `ledger.accounts`）。

**验收（离线）**：
- 充值后该接口读到的余额与 DB 逐位一致，不需要直查库；
- 受理预授权后余额减少、结算后变成实收后的余额，接口反映的都是 DB 的值；
- **引入 Redis 后同一用例仍然逐位一致**（它读 DB，不读缓存，§2.5）。

### P6 路由策略层（运营配置，**不进不可变修订**）

**改动**：`route_policies`（全局一条 + 按网关模型覆盖、策略类型、折扣率表、版本标识）；策略读取（带版本校验、不一致回源，§5.5）；`least_cost` 的折扣率输入（按候选供给配，**只作估算**）；账户标签字段（`ledger.accounts` 增列）与管理员面写入；`weighted_random` 按 `(account_id, idempotency_key)` 哈希（与 P3 同一套哈希与区间划分）；`routing_decisions` 记生效策略与选路依据。**不新增修订内容**——策略不进不可变修订。

**验收（离线）**：
- **未配置策略 ⇒ 与今天逐位一致**：一条策略都不配时（默认 `priority_failover`），同一批输入（账户 + 幂等键 + 请求参数）的选路结果与今天**逐位相同**（含同档按 `weight` 分流）；
- **策略不得选中承载不了的候选**：构造"某条候选承载不了这次请求"（§4.3 的 R5 不合格）且策略指向它——`least_cost` / `weighted_random` / `user_tag` **各一例** → 该候选**先被排除**，选路落在合格候选上；全部候选都不合格仍是 503 `platform_unavailable`（不是 400）；
- **重放确定性**：`weighted_random` 下同一 `(account_id, idempotency_key)` 重放落**同一条候选**，且去重成原 Job（不新建、不重复计费）；
- **折扣率不进成本**：`least_cost` 用**折后成本估算**选路，而 `attempts` 的成本列仍按 §3.7 记**实际扣费**（`Declared` 取上游 `cost`、`Computed` 按实际 `usage` × 费率）；构造"估算与实际不一致"→ **以实际扣费为准**，策略只影响选路、不影响成本列与对客金额；
- **改策略不需要发修订**：把策略从 `priority_failover` 改成 `weighted_random`（或改折扣率 / 标签）→ **不发布新修订**，下一次受理即按新策略选路；**已受理 Job 的选路与结算不变**；
- **策略缓存带版本校验**：让策略缓存失效失败（或手工把值里的版本标识改旧）→ 受理**回源 DB** 读到新策略，选路结果与"完全没有缓存"时逐位相同。

## 10. 未决（用户持有，2 条）

1. **权重语义**：接受"同优先级多候选、档内按权重分流"（需换唯一索引，**就地修订 `ADR-0009`**，本设计建议），还是"权重只作次级排序依据"（则同档只有一个候选，权重不起作用）；
2. **"与命中渠道无关"的确切含义（默认读法待确认）**：默认读法是"**金额也无关**"——同一个网关模型不管命中哪条渠道，对客户都用**同一个对客 CNY token 费率**（`consumer_rates_cny`，受理时冻结，§3.3），渠道成本只在**定价时**参考（§3.2/§3.4，本设计按此落地）；另一种读法是"只有**扣费对象**无关、金额仍随实际命中的候选成本变"，若按它落地，实收就不能只读受理时冻结的费率快照，要改回按命中候选的成本在结算时计价。

现状（`pricing.price_plans` 按候选挂、费率即结算基数）售价确实随命中候选变——默认读法要改掉的正是这一点。

**加价系数与汇率的数值不属设计决策**（用户更正）：**加价系数由管理员创建网关模型时录入**（每个网关模型一个，只用于后台定价时的参考算法），**汇率由管理员在后台维护**（全局一条 **USD → CNY**，只服务毛利折算）；设计只规定字段位、录入入口与快照时机（§3.2/§3.8）。因此这两条**不再列入未决**。

### 已定案（本设计直接决定，不再待决）

- **`markup_bps` 归属**：每个网关模型一个，**数值由管理员创建网关模型时录入**，随修订发布、随 Job 的 Price Snapshot 冻结（§1.6/§3.2）；**它只参与后台定价时的参考算法，不参与运行时换算**（§3.8）；
- **汇率维护入口与字段**：**全局一条** `pricing.fx_rates` + `PUT /api/v1/fx-rates`（管理员、写审计），**现在就立字段**，**币种对固定为 USD → CNY**、数值由后台管理员录入（不属设计决策）；**它只用于把美元成本折成人民币、服务毛利核算，不参与对客金额的计算**（§3.2/§3.8）；
- **币种平面**：**对客只有 CNY 单币种**（充值、余额、售价、保底额、扣费一律人民币，不做实时汇率换算）；**成本平面是 USD**（渠道四档费率与上游 `cost`）；快照里售价/保底/扣费记 CNY、成本记 USD 并记**折算汇率**与**折算后 CNY**（毛利用），两条线分开核对（§3.8）；
- **Redis**：**已批准引入**（用户批准；**纯加速层**——缓存不是事实源，见 §7；**预检拒绝不需要新 ADR**——`ADR-0003` 的"缓存不是事实源"已覆盖，见 §7.4）。因此"是否批准引入 Redis"**不再待决**；
- **网关模型启停粒度**：**整个模型一个开关**（`publication.gateway_models.enabled`），不做候选级开关（§2.1/§2.4）；
- **路由策略层**：**已定引入**（用户要求——「路由这层是运营的事」）：策略是**运营配置**（`route_policies`，运行期可改、改完即时生效、**不进不可变修订**），作用域为**全局一条 + 可按网关模型覆盖**，**未配置时默认 `priority_failover`（＝今天的行为）**；三条硬约束（**候选合格性优先于策略**、策略不改参数映射与承载面、**分流确定性可重放**）与折扣率定位（**折扣率不进成本，只作 `least_cost` 的比较输入**）见 §5。因此"**是否引入路由策略层**"**不再是未决、也不在范围边界**（旧口径"暂不引入"在 `docs/design/0005`，**已随本层落地改毕**，见 §5.6）；
- **预授权口径**：**保底 + 允许透支**——预授权**只是保底**，按**供给（vendor + offering）维度**查**随修订发布的保底表**（`floor_amounts`：供给内按 `(size, quality)` 两维，`quality` 维留空即按 `size` 档；OpenAI 系当前 `1K = ¥0.16` / `2K = ¥0.25` / `4K = ¥0.3`，**币种＝CNY**；`size=auto` 取该供给最大档；缺档回落该供给封顶保底值，再回落 `GENERATION_MAX_COST_MICROUSD`），**不编进代码**（§3.6）。保底额**两个身份**：① **准入闸门**——受理时 `balance >= 保底额` 才放行，不足 ⇒ 402 `insufficient_balance`（**硬拒绝保留**）；② **结算的参考下限**——估小了由结算透支吸收、估大了结算释放差额。结算**按实际**——**两家渠道都是 token 计费**，实收 = 对客四档 **CNY** token 费率 × 实际 `usage` 分项 token，**不封顶在保底额**，**实际超过保底额时余额可为负（透支发生在结算）**；透支后**下一次受理按当时余额判 ⇒ 402**；透支的追补属运营 / 充值流程。**运营要设成本护栏属 [`#9`](https://github.com/dehuadong/seeaihub-server-next/issues/9)、不在本设计**（§3.6）——**预授权口径本身不再归 [`#9`](https://github.com/dehuadong/seeaihub-server-next/issues/9)**；
- **"不可用时回退"的含义**：§4.4 已按**阶段**写清——受理前的候选不合格回退落地（现状）；提交前的失败技术上能回退，但当前策略是"失败不重试"（`ADR-0011`），改"回退下一候选"属**策略变更**；提交后的不确定（超时、断连、`5xx`）**不得回退**（会重复出图与重复计费），进对账。**"运行期回退"是否要做属 [`#11`](https://github.com/dehuadong/seeaihub-server-next/issues/11)，不在本设计范围**——因此这一条也不再待决。

### 范围边界（不待决，归其他工作项）

- **对外价策略与具体数值**（含是否分档、加价系数与汇率的具体取值）归 [`#5`](https://github.com/dehuadong/seeaihub-server-next/issues/5)，本设计只给机制（数值由后台录入，见上）；
- **成本护栏**（服务端成本上限一类的运营护栏）归 [`#9`](https://github.com/dehuadong/seeaihub-server-next/issues/9)——本设计只把**客户余额**当受理上限（§3.6）；
- **成本进账本与账实核对**归 [`#11`](https://github.com/dehuadong/seeaihub-server-next/issues/11)；
- **渠道/供给启停接口**与**调用记录查询接口**按用户口径不做（§8）。

### 需要 ADR 的决定（**须经用户确认后才立 / 才改**）

按仓库约定（`AGENTS.md`：记录持久 ADR 必须获得用户确认）**先确认再落 ADR**，本设计不自行创建、也不自行修订。

**新立**：

- **成本事实的落点**（`attempts` 承载渠道成本）与它与 `ADR-0006` 的关系；
- **对客售价的构成与"同一个网关模型一个固定售价"这一产品口径**（含 §10 未决第 2 条的含义），以及**对客单币种 CNY / 成本平面 USD** 这条币种口径（§3.8）；
- **路由策略层**（§5）：**策略是运营配置**（运行期可改、即时生效、**不进不可变修订**）、**候选合格性优先于策略**（任何策略都不得选中不合格候选）、**分流确定性可重放**（`weighted_random` 按 `(账户, 幂等键)` 哈希）——**已立**：`docs/adr/0020-routing-strategy-layer-configured-by-operations.md`（**`ADR-0020`**，详见 §5.6）。

**与 `ADR-0009` 的关系**：`ADR-0009`（与 `ADR-0015`）现状写的是"**选中顺序是发布决定**、不由请求参数 / Adapter / 价格决定""**核心服务不内置价格、优先级或健康度择优**"，**与路由策略层冲突**（`least_cost` 就是按价格选）。处理方式是**已立一条新 ADR**（`docs/adr/0020-routing-strategy-layer-configured-by-operations.md`，**`ADR-0020`**，见上一条与 §5.6），**不改 `ADR-0009` / `ADR-0015` 的旧条原文**——`ADR-0009` 标为**部分被取代**（保留原文与结论段、仅末尾追加标注；它定的"哪些候选存在、按什么顺序"**继续成立**），`ADR-0015` 的"核心服务不内置择优"**仍成立**（策略是运营配置、非硬编码）。

**就地修订既有 ADR**：

- `ADR-0006`：汇率从"Price Plan 发布时固定"改成"**全局表 + 受理时快照**"，并写清**它是 USD → CNY、只用于把美元成本折成人民币（毛利核算），不参与对客金额**（§3.2/§3.8）；
- `ADR-0009` ①：权重的语义与"优先级是档位、档内分流"这一选路模型——"数字小者优先**且同一型号内唯一**"要改成"档位内可多候选、按权重分流"（§4.2）；
- `ADR-0009` ②：预授权口径——"预授权金额与计价口径是两个量，不得互相推导…不由候选价格反算"要改成"**预授权只是保底**，按**供给（vendor + offering）维度的保底表**查得（随修订发布、随 Job 快照冻结、不编进代码），它**既是准入闸门**（`balance >= 保底额` 才受理，不足 ⇒ 402）**也是结算的参考下限**；结算**按实际用量 / 实际成本**、**实际超过保底额时余额可为负（透支发生在结算）**；`GENERATION_MAX_COST_MICROUSD` 只作**连供给封顶保底值都没有时的兜底保底额**"（§3.6）。

另外几处是**设计级修订**（改 `docs/design/`，不动 ADR）：`GET /v1/models` 的字段名 `vendor` → `vendor_id`（`docs/design/0005` §8.1 已定该形状）、合同 `model.const` 的对客投射替换规则（§1.4）、以及 `docs/design/0005` §8 第 2 条（原"路由策略层：暂不引入"）与 §4 的 R5 登记（**已随本修订落地**，见 §5.6）。

## 评审记录

- 状态：**待评审**（Plan Review 两轮发现已收口；用户更正与批准已并入，见下节第 15–20 条）。本文尚不构成实施依据。
- 评审完成后在此记录结论与批准依据（按 `docs/agents/artifacts.md`：`docs/design/` 的状态头表示**设计评审状态**，批准不由文件推断）。

## Plan Review 处置

两轮 Plan Review 的发现逐条落点——**第一轮 8 条**，另加**用户两处更正**（第 9、10 条），**第二轮（收敛轮）4 条**（第 11–14 条），**上一轮用户更正、批准与补写 4 条**（第 15–18 条），**再上一轮用户更正 1 条**（第 19 条），**本轮新增 1 条**（第 20 条：路由策略层）（本节编号只为对照评审清单，不构成正文引用）：

| # | 发现 | 落在哪 |
| --- | --- | --- |
| 1 | 权重无哈希输入（选路早于 JobId 生成） | §4.2 改为按 `(account_id, idempotency_key)` 确定性哈希，并写明**重放语义**（同键重放必然同候选、且去重成原 Job）；§4.5 记分流取值；§9 P3 验收改成"同一账户下不同的幂等键 + 同键重放同候选" |
| 2 | 加价撞固定预授权（对账只能退款、差额收不回） | §3.6 改为**预授权由 Price Snapshot 的售价派生**（`hold = 快照单价 × 请求 n`）；§9 P2b 加对应验收；§10 登记**就地修订 `ADR-0009`** 的预授权条款。**该轮对 `GENERATION_MAX_COST_MICROUSD` 的处置已由第 17 条整体改写**——它现在只作"没有售价可算时"的兜底 hold，**不再是受理上限**；**预授权口径本身已由第 19 条整体改写**（改为按供给维度查保底表 + 结算可透支） |
| 3 | Redis 提前拒绝与"缓存不是事实源"自相矛盾 | §7.4 保留性能收益，但**不写成"例外"**（与第 9 行及 §7.1/§7.4 的"不是例外"一致——`ADR-0003` 的"缓存不是事实源"已覆盖）：① 只有新鲜（来源标记 `db_commit` + 新鲜窗口）才允许提前拒绝；② 提前拒绝**必须落** `operations.audit_events`；③ 曾拟"需要**一条新 ADR**（缓存可用于拒绝的唯一条件）"，**该 ③ 已由第 9 条撤回**；④ §9 P4 补"陈旧缓存不得拒绝、误拒有审计" |
| 4 | route 缓存陈旧不可检 | §7.2 的值带**发布修订标识**，受理时与当前生效修订比对、不一致即**回源 DB**；写明 SET / 失效失败 ⇒ **当未命中、不影响正确性**；给出**陈旧窗口**（正确性上是 0，剩下的只是 TTL 命中率窗口）与理由；§9 P4 加"让失效失败 ⇒ 回源读到新候选集" |
| 5 | `ADR-0006` 就地修订未登记 | §3.2 注明汇率改动**就地修订 `ADR-0006`**；§10 新增"就地修订既有 ADR"清单；§3.5 写死结算口径——**对客结算只读 Job 固化的售价快照**，`attempts` 里的渠道成本**只用于毛利核算** |
| 6 | `markup_bps` 无落点 | §1.6 字段清单加 `markup_bps` / `reference_cost_microusd`，写明**随修订发布、随 Job 快照冻结**，并明确**不放** `publication.gateway_models`（那张表只存开关）；§3.2 同步 |
| 7 | 范围/事实缺口 | ① §2.5 + §9 P5 补"查看余额"的切片与验收，提案同步；② 提案背景改正"网关模型只能靠 `config/bootstrap/*.json` 发布"（`POST /api/v1/runtime-revisions` 早已是管理员 API）；③ §3.7 补 `unavailable` 的处置（不猜、进对账、不推进 `reconciliation_required`）与"成本来源可辨"的判据；④ 本文与提案对 `#9`/`#11`/`#5` 的引用一律去掉"第 N 项" |
| 8 | 最小性 | §9 拆成 **P2a 成本事实采集**（不依赖任何未决）与 **P2b 定价公式**；`jobs.charge_microusd` 投影列标**缓做**；§10 把三项技术上可自定的（维护入口/是否立字段、启停粒度、预授权口径）**直接定案**，留给用户收敛（当时 5 条）；§10 的"与命中渠道无关"那条（当时第 5 条）按评审建议把**"金额也无关"写成待确认的默认读法**（保留另一种读法一句话） |
| 9 | **用户更正：Redis 不需要新 ADR**（Redis 只是判断余额做预检，扣费仍在 PG，不涉及资金安全） | **撤回**"需新 ADR（缓存可用于拒绝的唯一条件）"：§7.4 删掉该条，改成写明**两条**——① **缓存永不作为扣费依据**（扣减只在 PG 事务里做；Redis 一律在 DB 提交成功后写**扣减后的值**、**不是 `DECRBY`**；不一致以 DB 覆盖）；② **预检拒绝要留审计**（预检没有 DB 记录，事后必须能解释"为什么拒了这个客户"，属**可解释性**、不是资金安全），并写明理由：`ADR-0003` 的"缓存不是事实源"**已覆盖**。§10「新立」清单去掉该条、未决里的"是否批准引入 Redis"那条（当时第 4 条）去掉"还要一条新 ADR"；§7.1 的"例外"措辞同步改为"不是例外"。§7.4 其余保留（新鲜窗口的来源标记、陈旧不得拒绝、误拒审计、P4 的验收） |
| 10 | **用户更正：回退语义按"阶段"写清，并给出 4xx/5xx 的判断口径** | §4.4 重写为**按阶段判定**表（**受理前** = 能、无副作用；**提交前** = 技术上能，但当前策略是"失败不重试"（`ADR-0011`），改"回退下一候选"属**策略变更**、归 `#11`；**提交后** = **不能**，上游可能已出图并已计费，再出一张就是"**重复出图、重复计费**"，进 `reconciliation_required` 人工对账），并写死判据——**`4xx` = 确定性拒绝、可证明未受理**（`401`/`402`/`403` 对客按平台侧故障，`ADR-0017`；`400`/`422` 渠道拒绝；`429` 明确未受理），**`5xx` 与超时/断连 = 不确定**、不得改道、不得自动重提；§10「已定案」与提案「开放决策」同步补"**运行期回退是否要做属 `#11`**"（不在本设计/提案范围） |
| 11 | **第二轮：hold 与 charge 口径不同**（§3.6 用"请求 `n`"、§6 用"实际产出张数"，谁说了算没写死） | **定案**：**hold 按请求 `n`**（× 快照单价）；**实收封顶在 hold**——若上游产出多于请求张数（异常），**不向客户加收**，多出的部分只记**渠道成本与毛利缺口**并**留痕**。§3.6 写死"hold 按请求 `n` / 实收按实际产出张数并取 min(算出额, hold) / 超产出不加收"三条，并**删掉"实收 ≤ 授权由构造保证"这种依赖假设的话**；§6「张数」行同步两个口径；§9 P2b 验收补"**超产出不加收、留痕**"。**该条已被第 19 条整体作废**（两家渠道都是 token 计费，实收按实际 `usage` 分项 token、**不封顶在保底额**） |
| 12 | **第二轮：无定价的旧修订受理新 Job 的 hold 未定义** | **定案**：缺 `consumer_price_microusd` 时，hold **回落到 `GENERATION_MAX_COST_MICROUSD`**（即今天的行为）。§3.3 历史兼容写明两种来源（历史 Job / 无定价旧修订受理的新 Job）**hold 口径一致**；§3.6 补"没有售价可派生的历史口径"一条；§9 P2b 验收补"**旧修订 + 新 Job → 走旧口径 hold**"（第 19 条后判据字段改为缺 `hold_microusd`，**结论不变**） |
| 13 | **第二轮：`PriceSnapshot.cost_basis` 无落点** | **定案**：**加一列** `runtime_revisions.cost_basis`（口径 `Computed` / `Declared`）随修订发布、**随快照冻结**。§1.6 字段清单加该列并写明它**不放** `publication.gateway_models`；§3.3 快照字段注明来源；§9 **P2a** 注明该列不属它（P2a 只落 `attempts` 三列）、**P2b** 列清单加该列。理由是**"成本来源可辨"是毛利核算的要求** |
| 14 | **第二轮：残留措辞统一**（①"唯一例外" ② 提案 4xx 注缺限定 ③ 迁移列数表述不一） | ① 本节第 3 行改成"**不写成'例外'**"，与第 9 行及 §7.1/§7.4 的"不是例外"一致；② 提案「开放决策」注补一句限定——**某条 `4xx` 若无法证明未受理，同样按"不确定"处理**（与 §4.4 一致）；③ §1.6 与 §9 P1/P2b 统一为"**P1 落命名两列、P2b 落定价三列（`markup_bps` / `reference_cost_microusd` / `cost_basis`），迁移可分两次**" |
| 15 | **用户更正：加价系数与汇率不是"现在要定的数值"，而是后台管理员录入**（加价系数**创建网关模型时设置**、每网关模型一个；汇率**全局一条、由后台维护**） | ① 全文**不出现任何具体数值建议**：§2.2 的 `pricing` 示例改成占位并注明数值由后台录入；§3.2 两个量的落点行改为"**由管理员创建/发布时录入**""**由管理员在后台维护**，具体数值与币种不属设计决策"；§3.2 汇率段删掉"取恒等值"、改为"数值由后台录入，设计只立字段与快照位"；§10 范围边界把"具体取值"归 `#5`。② §10 未决**删去"加价系数数值""汇率数值/币种"两条**，改为一句话"**加价系数与汇率的数值不属设计决策：由后台录入**"；§10「已定案」两条同步。③ 提案「开放决策」由 5 条收敛为 2 条，并在「已定案」补录入方与入口 |
| 16 | **用户批准：引入 Redis** | §10 未决**删去"是否批准引入 Redis"**，改记"**已批准引入（纯加速层，缓存不是事实源；预检拒绝不需要新 ADR）**"并移入 §10「已定案」；§7.7 由"是否引入需用户批准"改为"**用户已批准**"；§9 **P4** 标题由"待用户批准后"改为"**用户已批准**"；状态头与「评审记录」相应说明 |
| 17 | **用户更正：预授权口径有坑**——hold 就是**本次请求的售价快照**（快照单价 × 请求张数 `n`）；`GENERATION_MAX_COST_MICROUSD` 默认值只有 $0.02，**当上限用会把正常请求全拒** | ① **删掉"超过 `GENERATION_MAX_COST_MICROUSD` 即受理前拒绝（503 + 审计）"**这条（§3.6 正文与 §9 P2b 验收同步删除）；② `GENERATION_MAX_COST_MICROUSD` **只作"没有售价可算时"的兜底 hold**（旧修订 / 历史 Job 缺 `consumer_price_microusd` 时回落到它，§3.3/§3.6）；③ **真正的上限是客户余额本身**——`hold > 余额` ⇒ 402 `insufficient_balance`，不产生 Job、不扣款（§3.6/§7.4）；④ **运营要设成本护栏属 [`#9`](https://github.com/dehuadong/seeaihub-server-next/issues/9)、不在本设计**（§10 范围边界）；⑤ §3.6 标题、§3.6 的 `ADR-0009` 修订段、§7.4、§9 P2b 改动行与验收、§10「已定案」预授权口径同步改写。**第 19 条把 hold 的来源改为按供给维度查保底表**（③ 的"余额是唯一上限、不足即 402"结论保留） |
| 18 | **上一轮补写：hold 的单价/上界来源没写**（§3.6 只写"hold = 快照单价 × 请求 `n`"，**没说单价/上界从哪来**；尤其**按 token 计费的渠道**（AIHubMix 这类）受理时无法知道确切 token 数） | §3.6 补写 hold 的**单价/上界来源**——**按张计费**（`Declared`，APIMart 这类）按 `(size, resolution, quality, n)` 查**发布的档位价目表**（`tier_prices`）**精确算出**；**按 token 计费**（`Computed`，AIHubMix 这类）取**估算上界**（输入侧 prompt 长度 + 参考图按尺寸估、输出侧按档位查**发布的档位估算表** `tier_token_estimates` 取最坏值；`size=auto` 取该表**最大值**；**连上界都给不出**用**运营录入的每网关模型封顶值** `hold_cap_microusd` 兜底），并写明 `GENERATION_MAX_COST_MICROUSD` 的**正当用途就是"估不出时的 hold 值"**（不是"超限即拒"）；**hold 只冻结、结算按真值实收并释放差额**（估上界只多冻、不多扣）。§1.6 字段清单加 `tier_prices` / `tier_token_estimates` / `hold_cap_microusd`（**随修订发布、随 Job 快照冻结**；**不编进代码**——换模型/换渠道不该改代码），并写清与现有 `PriceRates` / `reference_cost_microusd` 的关系（**每张价目表**与 **token 费率 + 档位估算表**是**两种成本口径各自的表达**）；§3.3 快照加这几项与 `hold_microusd` / `hold_source`，结算口径按成本口径分支（§3.5 同步）；§9 P2b 验收补两条（**按张口径 hold 可精确断言**；**按 token 口径 hold 是按档位估算上界**、`auto` 取最大档、估不出用封顶值，且**结算按实际 `usage` 实收、冻结差额释放**）；§1.6/§9 的"定价三列"计数措辞同步改为"定价列"（P2b 落六列，§1.6 行注明"后六列可空"）。**该条的"按张 / 按 token"二分与 `tier_token_estimates` / `hold_cap_microusd` 已被第 19 条整体作废** |
| 19 | **本轮用户更正：计价与预授权口径改口径（两处）**——① 上一轮的"按张计费 vs 按 token 计费"二分**作废**，**两家渠道都是 token 计费**（四档：文本输入 $5 / 文本输出 $10 / 图像输入 $8 / 图像输出 $30，每 1M tokens）；APIMart **直接返回 `cost`**（实测 `cost = 0.011354`），**不需要我们自己算** ⇒ 成本来源改为**两态** `Computed`（按实际 `usage` × 四档费率自算）/ `Declared`（直接取上游 `cost`，更权威、含折扣）。② "档位 → 每张价"的表（`tier_prices`）**降级为定价参考/展示，不参与预授权**；`size`、`quality` 是主要影响因素，`resolution` 是 APIMart 的包装参数（合同里没有档位形态、承载面不声明它）。③ **预授权只是保底**：受理时按保底额查余额，**余额 < 保底额 ⇒ 402 `insufficient_balance`（硬拒绝保留）**；**结算按实际扣费**（实际 `usage` 分项 token × 对客费率；`Declared` 时成本直接取上游 `cost`），**实际 > 保底额时余额可为负——这才是"透支"（发生在结算，不在受理）**；**下一次受理按当时（可能已为负）的余额判 ⇒ 402**；**Redis 预检仍可拒绝**（余额不足是硬规则），保留"新鲜窗口 + 必留审计"（§7.4 口径不变）。④ **保底额按供给（vendor + offering）分别设定**：保底表挂供给维度、随修订发布、随 Job 快照冻结、**不编进代码**；OpenAI 系按 `size` 给保底（**1K = ¥0.16 / 2K = ¥0.25 / 4K = ¥0.3**），`size=auto` 取最大档，缺档回落该供给封顶保底值、再回落 `GENERATION_MAX_COST_MICROUSD`；保底表**支持 `(size, quality)` 两维**，OpenAI 系当前只按 `size` 填、`quality` 维留空备用（**留空即按 `size` 档**）。⑤ **两个币种平面**：**对客只有 CNY 单币种**（充值、余额、售价、保底额、扣费一律人民币，不做实时汇率换算）；**USD 只是平台与渠道之间的结算口径**（四档费率与上游 `cost`）；**汇率（USD → CNY）只用于把美元成本折成人民币、服务毛利核算，不参与对客金额**；**售价由后台按网关模型设定、以 CNY 表达**（"成本 × 加价系数"若用只是后台定价时的参考算法）；**保底额币种＝CNY**（确认，去掉上一轮的"币种待确认"）；快照里**售价/保底/扣费记 CNY、成本记 USD 并记折算汇率与折算后 CNY**，两条线分开 | ① §3.1 重写为"两家都是 token 计费 + 成本来源两态"（含实测 `cost = 0.011354`）；§3.7 判据表改口径；§3.5 成本列、§6「平台成本价」行、§9 P2a 验收同步。② §1.6 字段清单把 `tier_token_estimates` / `hold_cap_microusd` 换成 `floor_amounts`，`tier_prices` 注明**只作参考、不参与预授权**；§3.3 快照把 `consumer_price_microusd` 换成 `consumer_rates_cny`（对客四档 CNY token 费率）、`hold_microusd` 改称**保底额**、`hold_source` 改按供给查表来源；§9 P2b 改动与验收同步；§3.4 的"另一种读法"改写。③ §3.6 整节重写为"**预授权只是保底**：按供给查保底表冻结，结算按实际、可透支"（兜底链改为 供给档位 → `size=auto` 最大档 → 供给封顶保底值 → `GENERATION_MAX_COST_MICROUSD`），写明保底额的**两个身份**（准入闸门 + 结算参考下限），**保留** `balance >= 保底额` 与 402；§3.3/§3.5/§6 删掉"封顶在 hold / 超产出不加收"；§7.2 余额注明**币种 CNY 且可为负**、§7.4 的 `$预授权` 改为按供给查表的 `$保底额` 并写清"透支发生在结算、下一次受理按当时余额判 402"（**Redis 预检可拒绝的写法保留不变**）；§9 P2b 加"**透支**"验收；§10 已定案与 `ADR-0009` ② 修订措辞同步。④ §1.6 定义 `floor_amounts` 结构（供给维度 + `(size, quality)` 两维 + 该供给封顶保底值，OpenAI 系样例 `¥` 标注为 **CNY**）；§3.6 写死查表与兜底链；§9 P2b 加"**保底按供给维度查表**"与"`quality` 维留空即按 `size` 档"两条验收。⑤ 新增 **§3.8 两个币种平面**（对客 CNY / 成本 USD 的落点表 + 汇率只服务毛利 + 既有列名的币种语义）；§3.2 定价公式改为"CNY 售价 + 参考算法"、汇率行改为 USD → CNY 且不参与对客金额；§3.3 快照分**对客平面 / 成本平面**两组并加 `fx_rate_usd_cny`；§3.5 毛利改为"售价 CNY − 成本折算后 CNY"、`attempts` 加 `provider_cost_cny_microusd`；§6「扣费金额」行注明 CNY；§9 P2b 加"**两个币种平面分开**"验收；§10「已定案」加**币种平面**一条、汇率条与 `ADR-0006` 修订条同步；提案同步 |
| 20 | **本轮新增：路由策略层**（用户确认「路由这层是**运营的事**」——设计只给**机制**：选哪种策略 / 填什么折扣 / 给谁打什么标签都由运营在后台配） | 新增 **§5 路由策略层**：① **策略是运营配置、不是发布内容**——`route_policies`（运行期可改、即时生效）、**不进不可变修订**、**已受理的 Job 不受后续改策略影响**；② **作用域**＝全局一条 + 可按网关模型覆盖，**未配置时默认 `priority_failover`（＝今天的行为）**；③ **策略类型**（举例、机制上可扩展）`priority_failover` / `weighted_random` / `least_cost` / `user_tag`；④ **三条硬约束**——**候选合格性优先于策略**（不合格候选先被排除，任何策略都不得选中）、**策略只决定"选哪条合格候选"**（不改写参数映射与承载面）、**分流确定性可重放**（`weighted_random` 按 `(账户, 幂等键)` 哈希）；⑤ `priority` / `weight` / `discount_rate` / 账户标签都是**策略的输入**，不是各自独立生效的行为；⑥ **折扣率不进成本**（成本永远记实际扣费——`Declared` 取上游 `cost`、`Computed` 按实际 `usage` × 费率；折扣率**只作 `least_cost` 的比较输入**＝折后成本**估算**，两者不一致时**以实际扣费为准**）；⑦ **账户标签**（`ledger.accounts` 增列，**管理员面配置**、值由运营设）；⑧ **缓存**与 route 缓存同构（带版本校验、不一致回源），**改策略不需要发修订**。同步：§4.2 加"`weight` 是策略的输入"、§7.2/§7.3 加 `route_policy` 缓存与失效、§9 新增 **P6** 切片与验收、§10「已定案」加一条（"是否引入路由策略层"不再是未决 / 范围边界）、§10「新立 ADR」加"路由策略层"并写明**与 `ADR-0009` 的关系＝新立一条 ADR、不改旧条**（**该 ADR 现已立为 `docs/adr/0020-routing-strategy-layer-configured-by-operations.md`**）、设计级修订登记 `docs/design/0005` 两处旧口径（**现已随本修订落地**）；提案加范围第 10 条、已定案一条、验收第 12/13 条 |

**留给用户的 2 条**：**权重语义**（档内确定性分流需就地修订 `ADR-0009`，还是只作次级排序）、**"与命中渠道无关"的确切含义**（默认读法＝同一网关模型一个对客 CNY 费率，渠道成本只在定价时参考）——**两轮发现、两处更正、上一轮 4 条与本轮 1 条都未新增待决项**；**第 20 条（路由策略层）同样不新增待决项**，但它要求**新立一条 ADR**（承接 `ADR-0009`/`ADR-0015` 的"选路是发布决定、不内置择优"）并**改 `docs/design/0005` 两处旧口径**——**两者均已落地**（ADR 已立为 `docs/adr/0020-routing-strategy-layer-configured-by-operations.md`，`ADR-0009` 随之标为**部分被取代**、`ADR-0015` 的"不内置择优"仍成立；`docs/design/0005` 两处旧口径已改，见 §5.6）；加价系数与汇率的数值由后台录入，**不属设计决策**（§10）。

**本文相对上一修订新增的两处字段级说法**（评审时请一并看）：`runtime_revisions.reference_cost_microusd`（定价时参考的渠道成本，**USD**，发布数据）与 `PriceSnapshot.consumer_rates_cny`（对客四档 token 费率，**CNY**）——它们是"成本 + 加价系数"与"金额也无关"这两条的前提，理由在 §3.2/§3.3。**第 20 条另新增路由策略层的三个说法**：`route_policies`（**策略**，运行期配置、**不进不可变修订**）、`discount_rate`（**折扣率**，只作 `least_cost` 的比较输入、**不进成本**）与**账户标签**（**管理员面配置**，支持 `user_tag`）——理由在 §5。**第 18 条曾新增的 `tier_token_estimates` / `hold_cap_microusd` 已被第 19 条移除**（那套"按张 / 按 token"的 hold 口径作废）；第 19 条改为新增 `runtime_revisions.floor_amounts`（**保底表**，**CNY**，按供给维度挂、随修订发布、随 Job 快照冻结、**不编进代码**）与 `PriceSnapshot.fx_rate_usd_cny`（**只服务毛利折算，不参与对客金额**）——前者是**预授权保底额的唯一来源**，后者是**两个币种平面**（§3.8）的落点，理由在 §1.6/§3.6/§3.8。
