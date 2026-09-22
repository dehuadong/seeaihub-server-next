主题: 平台网关模型、对客定价与运营后台（含路由权重与 Redis 加速层）
当前修订: v1
状态: 待评审（Plan Review 两轮发现已收口；用户更正与批准已并入）
来源: 依据工作项「运营后台：平台网关模型、对客定价与路由权重」（提案正文 `.data/proposal-admin-console.md`）、`docs/adr/0003`/`0006`/`0009`/`0015`/`0017`/`0019`、`docs/design/0005` 与仓库现状归纳；不引入未标注的新决策

# 平台网关模型、对客定价与运营后台

本文是运营后台工作的技术设计：**怎么落地**「管理员用 API 定义平台网关模型 → 定对客价 → 排路由顺序与权重 → 事后查得清售价、成本与毛利」，以及随之而来的 Redis 加速层。产品范围、验收与决策归属归提案正文（`.data/proposal-admin-console.md`）；本文只承载技术设计。

术语一律沿用 `CONTEXT.md`：**Gateway Model**（平台型号名，对外的 `model`）、**Vendor Model**（厂商的模型产品）、**Offering**（一条可调用供给）、**Channel**、**Runtime Revision**、**Price Plan**、**Price Snapshot**、**Routing Priority**、**Metering Evidence**。本文新引入的只有三个字段级说法：**渠道成本**（ADR-0006 的"成本按渠道各自的口径取数"）、**加价系数**、**汇率**——理由见 §3。

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
| `pricing.price_plans` | 现状：四档 token 费率（USD）+ 来源 URL；现在它同时是对客结算基数 | 角色**收窄为渠道成本费率**（AIHubMix 口径）：定价时的参考与毛利核算用，不再是对客结算基数（§3） |
| `generation.jobs` | 受理时的请求事实 + 被选中的 `PublishedOffering` + Price Snapshot | 受理时固化的 `gateway_model` 就是对外的那个名字 |

### 1.3 命名与唯一性

- **`gateway_model`（平台命名）**：全局唯一，且**同一时刻只有一个生效修订**。沿用现有唯一索引 `one_active_entry_per_model_and_priority`（`(gateway_model, routing_priority) WHERE active`）所保证的"同一名字的生效条目来自同一修订"，本设计只调整该索引（§4）。
- **`native_model_id`（厂商原生名）**：只属于 `catalog.vendor_models`，用于合同的身份与唯一键；**不进对客面**。
- **允许多个网关模型指向同一个 Vendor Model Revision**：同一份供给包成不同名字/不同价格档（`gpt-image-2.5-plus` 与 `gpt-image-2.5-lite` 都指向 `gpt-image-2.5-sunburst`）。唯一性在**名字**上，不在 Vendor Model 上。这是引入命名层的动机之一，因此不做"一个 Vendor Model 只能有一个网关模型"的约束。
- **发布命令新增 `gateway_model`**（`PublishRuntimeCommand`）：缺省时回退取 `native_model_id`，与现有形状**逐位兼容**——现有素材、现有已发布数据、现有测试都不用改。

### 1.4 对客名与 `native_model_id` 的边界

对客面只出现 `gateway_model`。有一处必须处理：**合同里的 `properties.model.const` 现在写的是厂商原生名**（例如 `config/bootstrap/gpt-image-2.5-flare.json` 的 `"const": "gpt-image-2.5-flare"`）。

- **存的那份合同不动**（合同行不可变，`ADR-0003`/`ADR-0015`），**对客投射时把 `model.const` 替换成该网关模型名**。理由：受理期平台本来就用调用方给的 `model` 覆盖这个字段（`crates/application` 的 `contract_parameter_face` 里 `parameters.insert("model", request.model)`），`model` 一直是**平台字段**，不是厂商字段；投射时替换既不改数据、也不改校验行为。
- **发布期新增一条校验**：合同的 `properties.model.const` 必须等于本次发布的 `native_model_id`。它保证素材不把网关名或别的名字写进合同正文，也让 §8 的"响应全文不含原生名"这条验收可判定（合同正文里没有第二个模型名来源）。
- `GET /v1/models` 的 `revision` 字段仍取 `native_revision`：它是**合同修订号**，不是模型名（`docs/design/0005` §8.1 已定下该响应形状，本设计只增 `vendor_id`、不删字段）。

### 1.5 历史修订与已发布数据：不回填

**不回填。** 已发布的 `runtime_entries.gateway_model` 已经等于当时的 `native_model_id`，语义自洽（"平台型号名恰好等于厂商原生名"是合法取值）。改历史行会破坏"Job 固定受理时版本"（`ADR-0003`），因此新发布才允许两者不同。

### 1.6 存储改动

| 改动 | 内容 | 理由 |
| --- | --- | --- |
| `publication.runtime_revisions` 增列 | `gateway_model text NOT NULL`、`vendor_model_id uuid NOT NULL`（**P1 落**）、`markup_bps integer`、`reference_cost_microusd bigint`、`cost_basis text`（**P2b 落**；后三列**可空**：只有新发布的修订带定价，见下） | 让"这次发布定义的是哪个网关模型、指向哪个 Vendor Model、按什么价卖、**成本按哪个口径算**"在修订上可读，不必从条目反推 |
| `publication.runtime_entries` | 已有 `gateway_model`（迁移 0004 改名而来），不改 | 路由索引已经按它建好 |
| 新表 `publication.gateway_models` | `gateway_model text PRIMARY KEY`、`enabled boolean NOT NULL DEFAULT true`、`created_at`、`updated_at`、`updated_by` | **只放运维开关**，不放定义（定义只在不可变修订里） |
| 唯一性 | 沿用"同一名字同时只有一个生效修订"，由发布原子替换保证 | `ADR-0009` |

`publication.gateway_models` 刻意**不存** `vendor_model_id` / 候选 / 定价：那些是修订的内容，存第二份就等于造第二个权威（`ADR-0003`）。它只回答"这个名字现在开着吗、谁在什么时候改的"。

**`markup_bps`、`reference_cost_microusd` 与 `cost_basis` 随修订发布、随 Job 的 Price Snapshot 冻结**（§3.2/§3.3），**不放** `publication.gateway_models`：那张表是**运行状态**（开关），定价是**修订内容**——放进可变表就等于"改价不用发布"，而 `ADR-0003` 要求已受理 Job 固定受理时版本，定价必须能随修订被 Job 固化。其中 `cost_basis` 存的是**这次定价参考的成本口径**（`Computed` = 平台按已发布费率 × 分项 token 自算；`Declared` = 上游终态声明的金额），取值面与 §3.7 的 `provider_cost_source` 同源但**不是同一个量**：它是**定价时**声明的口径，随快照冻结后使"这笔的售价是按哪种成本口径定的"事后可辨——**"成本来源可辨"是毛利核算的要求**（§3.5/§3.7）。

**写入方**：该名字**首次发布成功时**由发布事务插入一行（`enabled` 默认 `true`），此后只由 `PATCH` 改 `enabled`。没有发布过就 PATCH 不存在的名字 → 404。

**迁移的回填**（增量迁移，不改已应用的 `0001`–`0006`，沿用本仓库的迁移约定）：**迁移分两次，与切片对齐——P1 落命名两列，P2b 落定价三列**（§8 的 P1/P2b 是同一套列，两处口径一致）：

- **P1 落的命名两列**（`gateway_model` / `vendor_model_id`）：在既有行上先按"同一 revision 的 `runtime_entries.gateway_model` / `vendor_model_id`"回填（同一次发布写下的条目同值，可直接取），再设 `NOT NULL`；
- **P2b 落的定价三列**（`markup_bps` / `reference_cost_microusd` / `cost_basis`）：在既有行上**留 NULL**——旧修订没有定价，因此那些修订受理出来的快照不带 `consumer_price_microusd`，结算与 hold 走旧口径、与今天逐位相同（§3.3/§3.6）；
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
      "pricing": { "reference_cost_microusd": "…", "markup_bps": "…", "fx_rate": "…" }
    }
  ]
}
```

它是**只读投影**：数据源是生效修订（`runtime_entries` + `runtime_revisions` + `catalog.vendor_models`）加运维开关（`publication.gateway_models`）。不新增"编辑态"，也不回显渠道凭证（`credential_env` 只记变量名，本来就不进响应）。示例里的 `pricing` 三项只占字段位：**加价系数由管理员创建网关模型时录入、汇率由管理员在后台维护，数值本身不属设计决策**（§3.2/§9）。

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

补一条管理员读：`GET /api/v1/accounts/{account_id}`，返回 `balance_microusd` 与 `updated_at`（`ledger.accounts` 的现有两列，不加新列）。**读的是 DB，不是缓存**——缓存不是事实源（`ADR-0003`），引入 Redis 之后这条也不变（§6.1）。切片归 §8 的 **P5**，验收在那里。

它同时收掉提案与工单 [`#9`](https://github.com/dehuadong/seeaihub-server-next/issues/9) 的重复：`#9` 的"账目可见"诉求与这里同源，实施时只做一处（提案"与其他工作项的关系"一节）。

## 3. 定价

### 3.1 现状：两个渠道的成本取数不同，且只有一个被留痕

`ADR-0006` 定的是"**成本按渠道各自的口径取数**"：

| 渠道 | 成本取数 | 现状 |
| --- | --- | --- |
| **AIHubMix** | 上游只返回四分项 token、**没有任何金额字段** ⇒ 成本 = Σ(分项 token × 已发布费率)，费率四档（文本输入 $5 / 图像输入 $8 / 文本输出 $10 / 图像输出 $30，每 1M） | 费率在 `pricing.price_plans`，现在**同时**当对客结算基数；本设计把它的角色**收窄为渠道成本费率**（定价时的参考口径 + 毛利核算），不再是对客结算基数（§3.2/§3.3） |
| **APIMart** | 上游任务终态**直接声明 `cost`**（USD，含账号 `Group ratio 0.8`，不可复现） | **当前既不采纳也不留存**（`crates/adapter-apimart` 头注明确），要算毛利必须开始采集 |

依据：`docs/facts/channel-facts.md` §2.4/§2.6、§3、§5。`ADR-0006` 同时定下"`cost` 只用于核成本，**不替代计量事实**"——所以采集 `cost` 不违反那条决定，也不是复活已被否决的"金额型计量证据"（`ADR-0012` 存根）。

### 3.2 定价公式与三个量的落点

```
对客售价(microUSD) = 参考渠道成本(microUSD) × (1 + 加价系数) × 汇率
```

**这条公式是"定价口径"，不是"结算公式"**：默认读法下（§3.4）对客售价在受理时算定并随 Job 冻结，**结算只读那份快照**（§3.3），不再读运行期的实际成本。公式里的"渠道成本"因此是**定价时参考的成本**（发布数据），不是"命中候选在运行期报出来的实际成本"——后者只进 `attempts`，只用于毛利核算（§3.5）。这样"预授权由售价派生"才成立（§3.6）：受理时要算得出售价，就不能等上游回来才定价。

| 量 | 放哪 | 为什么 |
| --- | --- | --- |
| **参考渠道成本** | 随修订发布的**发布数据** `runtime_revisions.reference_cost_microusd`（发布者按 `ADR-0006` 的渠道口径取一个可核的参考值：APIMart 取声明过的 `cost`、AIHubMix 按已发布费率取参考用量） | 两家口径不同（`ADR-0006`），实际金额要等上游回来才知道；拿实际成本定价等于把售价推迟到结算，预授权就无从派生 |
| **加价系数** | `markup_bps`（整数基点，避免浮点）：**每个网关模型一个**，**由管理员创建/发布该网关模型时录入**，**随修订发布**（`runtime_revisions.markup_bps`，§1.6），**随 Job 的 Price Snapshot 冻结**；不放 `publication.gateway_models`（那张表只存开关）。**具体数值由后台录入，不属设计决策** | 网关模型这一层就是"同一份供给包成不同价格档"的载体；全局系数会让这层失去意义。随快照冻结 ⇒ 已受理 Job 不受后续改价影响（`ADR-0003`） |
| **汇率** | **全局一条**（`pricing.fx_rates`：币种对 + 汇率 + 生效时间），**由管理员在后台维护**（入口 `PUT /api/v1/fx-rates`，写审计），**受理时快照进 Price Snapshot**。**具体数值与币种由后台录入，不属设计决策**。**这一处就地修订 `ADR-0006`**（它原文写的是"Price Plan 保留…发布时固定的汇率"，见 §9 的修订清单） | 汇率是**外部事实**，同一时刻全平台必须是同一个数才对账得起来；放进每个网关模型的发布里，改一次汇率要重发所有模型。不写配置文件（用户明确后台走 API） |

汇率**数值由后台管理员录入**（全局一条），设计只立字段与快照位**并规定录入入口与快照时机**（见 §9）；两家渠道的成本都是 USD 是既有事实（`docs/facts/channel-facts.md`），与 `ADR-0006` 的"以 USD 计价的计划原生价即 microUSD"口径不冲突。

### 3.3 售价快照随 Job 冻结

`PriceSnapshot`（`crates/domain`，现在只有 `price_plan_id` + 四档 `rates` + `captured_at`）扩展为：

```
PriceSnapshot {
    price_plan_id,
    cost_basis: Computed { rates } | Declared { currency },   // 定价时参考的成本口径（随修订发布：runtime_revisions.cost_basis，随快照冻结）
    reference_cost_microusd,                                   // 定价时参考的渠道成本（随修订发布）
    markup_bps,                                                // 随修订发布
    fx_rate,                                                   // 受理时从全局表快照；定点整数（如 1e6 分母），不使用浮点
    consumer_price_microusd,                                   // 受理时算定并冻结：对客单价（每计价单位；默认读法下同一网关模型一个固定价，不随命中候选变）
    captured_at,
}
```

`charge_microusd(usage)` 从"Σ token × 费率"改为"**只读 `consumer_price_microusd`** × 计价单位数（**实际产出张数**，口径见 §5）"，并**封顶在 hold**（§3.6）——对客结算不再读实际成本（§3.5）。

**历史兼容**：`price_snapshot` 是 jsonb，**缺 `consumer_price_microusd`** ⇒ 按旧口径（`Σ token × 费率`）解释，结算结果与今天逐位相同。缺它有两种来源，都走这条路：① 已受理的历史 Job（快照本身就是旧的）；② 迁移后仍生效、但**没有定价**的旧修订受理出的新 Job（§1.6：定价三列留 NULL）。两种来源的 **hold 口径也一致**：缺 `consumer_price_microusd` 时 hold **回落到 `GENERATION_MAX_COST_MICROUSD`**，即**今天的行为**，不由售价派生（§3.6）。历史 Job 的查询与结算行为不变（验收第 9 条）。

### 3.4 扣费与命中渠道的关系（默认读法待用户确认）

用户原话是"与命中渠道无关"。本设计按**默认读法**落地，并且这一处**待用户确认**（§9 未决里的"与命中渠道无关"那条）：

- **默认读法（本设计按此落地）：金额也无关。** 同一个网关模型**不管命中哪条候选，对客户都是同一个固定售价**——售价随修订发布、受理时快照冻结（§3.3），渠道成本只在**定价时**参考（§3.2），运行期的实际渠道成本只用于毛利核算（§3.5）。因此"同一请求命中两条成本不同的候选时对客扣费不同"**不再成立**：扣费逐位相同，差异只体现在毛利。扣的仍然是**同一个用户余额**（`ledger.accounts`），不按渠道分账。
- **另一种读法（一句话）**：若用户的意思是"只有**扣费对象**与渠道无关、金额仍随实际命中的候选成本变"，那么售价在受理时算不出来，预授权也就无法由售价派生（§3.6），要改回"按候选成本在结算时计价"，并接受实收可能超过授权额（而 `ADR-0006` 下对账只能退款）。

现状确实随命中候选变：`pricing.price_plans` 按候选挂、费率就是结算基数（§3.1）——默认读法要改掉的正是这一点。

### 3.5 毛利记录

- **对客结算只读 Job 固化的售价快照**：结算金额 = `price_snapshot.consumer_price_microusd`（§3.3）。`attempts` 里的渠道成本**只用于毛利核算**——它**不参与对客结算**，不改对客金额，也不改授权额（§3.6）。
- **售价**：`ledger.entries`（`kind = 'capture'`，金额为负）+ `ledger.holds`（授权额）——账本是权威（`ADR-0003`）；另在 `generation.jobs` 加 `charge_microusd` 列（结算时写入）作为**投影**，便于按 job 直接查，权威仍是账本。**这一列缓做**：账本已经查得到，它只是查询便利，不阻塞任何切片（§8 的 P2b）。
- **成本**：`generation.attempts` 新增 `provider_cost_microusd`、`provider_cost_currency`、`provider_cost_source`（`declared` / `computed` / `unavailable`，判据见 §3.7）。**异步写入**（结算时才拿得到）。
- **毛利** = 售价（快照）− 成本（`attempts`），按 job 可查；成本缺失（`unavailable`）时标"成本未知"，不猜（§3.7）。
- **边界**：把成本**写进账本**（`ledger.entries` 的 `adjustment` 分录）与账实核对归工单 [`#11`](https://github.com/dehuadong/seeaihub-server-next/issues/11)（成本进账本与账实核对那部分），**本设计不做**——同一件事不做两遍，也不在这里预先决定成本条目的会计语义。

### 3.6 预授权由售价派生（`GENERATION_MAX_COST_MICROUSD` 退为"无售价可算时"的兜底 hold）

现状：`max_cost_microusd` 是服务端固定数（`GENERATION_MAX_COST_MICROUSD`，默认 $0.02，读在 `apps/api/src/main.rs`），受理时按它扣预授权；结算时 `charge > max_cost` ⇒ 进对账（`crates/application` 的 `complete_success`）。**加价后更容易触顶**，而 `ADR-0006` 定下对账**只能退款、收不回差额** ⇒ 实收一旦超过固定授权额，差额就收不回来。

本设计把预授权改为**由 Price Snapshot 的售价派生**：受理时的 hold 就是**本次请求的售价快照**——**快照单价（§3.3，受理时已算定）× 请求张数 `n`**（受理时已知的请求事实）。**hold 与实收是两个口径**，各自写死，不再互相假设：

- **hold = 本次请求的售价快照**：受理时只拿得到请求事实，因此授权额 = 快照单价 × 请求张数 `n`。快照单价在受理时已算定，所以 hold 当场算得出，不再有"加价撞固定预授权"的缺口。
- **实收按实际产出张数，并封顶在 hold**：结算只读同一份快照、同一单价（§3.5），实收 = 快照单价 × **实际产出张数**（口径见 §5），且**取 min(算出额, hold)**。正常路径下渠道按合同给的上限产出、不多于请求张数，实收自然 ≤ hold。
- **超产出（上游产出多于请求张数）不加收**：这是异常路径（渠道没守合同上限）。多出的张数**不对客计费**，实收**封顶在 hold**、**不向客户加收**；多出的部分只记**渠道成本**、形成**毛利缺口**并**留痕**（进 `attempts` 的成本列，§3.5/§5），供运营发现——对客的钱一分不多收，缺口记在平台侧。
- **不再用"实收 ≤ 授权额由构造保证"这类依赖假设的话**：授权额与实收的关系由上面的**封顶规则**决定，不由"渠道一定不超产"这个假设决定；封顶之后也不会出现"实收超授权 ⇒ 进对账只能退款"的缺口。

`GENERATION_MAX_COST_MICROUSD` **只作"没有售价可算时的兜底 hold"**，不再是任何形式的上限：

- **有售价可算 ⇒ 它不参与判定**。此前写的"派生出的 hold 超过它就受理前拒绝"**这条已删除**：它的默认值只有 $0.02，当上限用会把正常请求全拒（用户指出）。**不截断、不拒绝、不写审计**——有售价时这个数一次都不读。
- **没有售价可派生**：快照里**缺 `consumer_price_microusd`**（已受理的历史 Job，或迁移后仍生效但**没有定价**的旧修订受理出的新 Job，§3.3）⇒ 派生不出 hold，**回落到 `GENERATION_MAX_COST_MICROUSD`**，即**今天的行为**（受理时按这个固定数扣预授权），结算走旧口径、与今天逐位相同。

**真正的上限是客户余额本身**：受理时的条件更新（§6.4）要求 `balance_microusd >= $hold`，不成立 ⇒ `insufficient_balance`，对客 **402 余额不足**，不产生 Job、不扣款。因此"售价高过某个服务端固定数"不是拒绝理由，"余额不够"才是。**运营要设成本护栏（服务端成本上限）属 [`#9`](https://github.com/dehuadong/seeaihub-server-next/issues/9)，不在本设计**（§9 范围边界）。

**这一处就地修订 `ADR-0009`**：它写"预授权金额与计价口径是两个量，不得互相推导：预授权由调用方给出的 `max_cost_microusd` 决定，不由候选价格反算"。本设计的读法不同（预授权由**对客售价**派生——即本次请求的售价快照，不由候选成本反算），而且该句现状与仓库也不一致（`GenerationService` 用的是服务端固定数，不由调用方自报）。按仓库约定**先经用户确认并修订该 ADR**（§9 的修订清单），本设计不自行改 ADR。

### 3.7 落地改动的边界（一处新成本事实）与"成本来源可辨"

采集 APIMart 的 `cost` 需要同时改三处：`adapter-sdk` 的 `ProviderSuccess` 新增可选 `declared_cost`（+ 币种）、`adapter-apimart` 从任务终态读出并回传、`generation.attempts` 增列。**这不新增"金额型计量证据"**：计量事实仍是四分项 token（`ADR-0006`），`cost` 只作成本口径——`ADR-0006` 明确允许它"只用于核成本"。

**成本来源怎么判（判据是"成本从哪来"，不是"金额对不对"）**：

| `provider_cost_source` | 判据 | 谁是这样 |
| --- | --- | --- |
| `declared` | 渠道在终态**声明了金额**，且解析成功（金额与币种都拿得到） | APIMart（`ADR-0006` 的渠道口径） |
| `computed` | 渠道**不给金额字段**，平台按已发布费率 × 分项 token 自算 | AIHubMix |
| `unavailable` | 声明了但**缺字段 / 负数 / 解析失败**，或该次执行根本没拿到终态金额 | 任一渠道的异常情形 |

**`unavailable` 的处置（"APIMart 未声明 `cost` 或解析失败"）**：**不得猜测**——不记 0、不用"token × 费率"顶替、不用上一次的值（`ADR-0006`：证据缺字段、负数或解析失败时不得猜测费用，进对账）。具体是：`provider_cost_microusd` / `provider_cost_currency` 留 NULL、`provider_cost_source` 记 `unavailable`，该笔**成本缺口进对账**、毛利标"成本未知"，人工核对上游账单后再补录（补录与账实核对归 `#11`）。

它**不**把 Job 推进 `reconciliation_required`：那个状态是"受理/执行状态不明"，会把消费者的钱扣在对账里（`ADR-0006`：对账只能退款），而成本缺口是**平台侧的账务缺口**——对客结算照常按售价快照完成（§3.5）。

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
- **索引调整**：现有 `one_active_entry_per_model_and_priority`（`(gateway_model, routing_priority) WHERE active`）不允许同档多候选，与"档内分流"冲突 ⇒ 换成 `(gateway_model, offering_id) WHERE active`。**这一处直接改动 `ADR-0009` 的操作性条款**（原文写的是"数字小者优先**且同一型号内唯一**"），因此按仓库约定需要**先经用户确认并修订该 ADR**（§9 的 ADR 候选清单），本设计不自行改 ADR。
- **换索引后的守卫**：新索引只防"同一次发布里同一个 Offering 出现两行"，比原索引弱——"同一名字的生效条目来自同一修订"由发布事务的原子替换（`UPDATE runtime_entries SET active = false WHERE active AND gateway_model = $1`）加 `crates/persistence` 里既有的"active 候选跨修订即报错"防御共同保证，不靠索引。
- 若用户要的其实是"权重只作排序的次级依据"，那与"优先级唯一"可以并存，但同档只有一个候选、权重不起作用（§9 未决里的"权重语义"那条）。

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

**本设计的落地**：只做**受理前的候选不合格回退**（现状，§4.3 的 R5 与"全部候选都不合格 → 503 `platform_unavailable`"）；**运行期回退**（提交前改道下一候选、提交后改道）**本设计不做**——"提交后"被 `ADR-0009` 明确禁止（"一次 Attempt 一旦进入 `submitting`，就禁止改选 Offering 或 Channel——上游可能已生成并计费，改选等于重复出图与重复计费"），"提交前回退"是策略变更。**"运行期回退"是否要做，属 [`#11`](https://github.com/dehuadong/seeaihub-server-next/issues/11)，不在本设计范围**（§9 已定案同步）。

### 4.5 路由判定记录

`generation.routing_decisions.considered` 每项增加 `weight` 与本次的**分流取值**（按权重选中的依据：`hash(account_id ‖ idempotency_key)` 的落点；哈希输入本身已随 Job 落库，不重复记），使"当时考虑过谁、为什么跳过、为什么是它"事后可重建（`ADR-0009`）。

## 5. 记录与日志：六项信息现在落在哪

提案第 3 条要求的六项，逐项对照现状：

| 需要的信息 | 现在落在哪 | 缺什么 / 补什么 |
| --- | --- | --- |
| **模型** | `generation.jobs.gateway_model`（迁移 0004 已把列名收口为"平台型号名"） | 无需新列；语义随本设计变成"网关模型名" |
| **张数** | 请求侧：`jobs.native_parameters->>'n'`；结果侧：`jobs.result_images` 的数组长度 | **两个口径都要写死**：**hold（授权额）按请求 `n`**（受理时已知的请求事实，× 快照单价）；**charge（实收）按实际产出张数**（结果信封长度，× 同一快照单价），**且封顶在 hold**。两者都在库里，不新增列。**上游产出多于请求张数（异常）时不向客户加收**：多出的张数不计费，只记**渠道成本与毛利缺口**并**留痕**（§3.6/§3.5），供运营发现 |
| **扣费金额** | `ledger.entries`（`kind='capture'`）+ `ledger.holds`（授权额）；**Job 上没有** charge 列 | 账本是权威（`ADR-0003`）；**补** `generation.jobs.charge_microusd`（结算时写入）作为投影，便于按 job 查——**缓做**（§3.5） |
| **平台成本价** | **没有**：APIMart 的 `cost` 不采纳不留存；AIHubMix 的金额要自算也没存 | **补** `generation.attempts.provider_cost_microusd` / `provider_cost_currency` / `provider_cost_source`（§3.5） |
| **请求时间戳** | `jobs.created_at`（受理）、`attempts.started_at` / `completed_at` | 够 |
| **上游 request_id** | `attempts.provider_trace_id`（AIHubMix 的 `x-request-id`；APIMart 的 task id） | 够 |

### 5.1 异步路由日志的落点

"异步记录路由日志与渠道成本"由**两张既有表**承担，不新建日志表：

- `generation.routing_decisions`——**受理时同步**写（候选、优先级、权重、取舍原因、被选中者）；
- `generation.attempts`——**执行后异步**写（渠道原始错误、对账标识 `provider_trace_id`、计量证据，本次再加**渠道成本**）。

不合并成一张表：两张表的事实归属不同（受理事实 vs 执行事实），合并会造出第二个权威（`ADR-0003`）。

### 5.2 不开查询接口

用户已撤回"调用记录查询"。上面的字段是**为了查得出来**（运营侧直查，或后续再开接口），不是现在开接口。现有的**对账清单**（`GET /api/v1/reconciliation-cases`）与**平台侧失败清单**（`GET /api/v1/provider-failures`）够用。

## 6. Redis

### 6.1 定位

**缓存，不是事实源。** `ADR-0003` 逐字适用：目录、发布、Job、结算与审计的事实权威是 PostgreSQL。因此本设计的硬约束是：

- 所有**金额判定**与**选路结果**的正确性**不依赖 Redis**（唯一一处"缓存判定直接决定对客响应"的是 §6.4 的"凭新鲜缓存提前拒绝"：只读、无副作用、必留审计，且**扣减与余额事实仍只在 PG 里发生**，因此**不是**对 `ADR-0003` 的例外）；
- Redis 不可用时平台**照常工作**（降级直查 DB），只是变慢；
- 缓存与 DB 不一致时，**以 DB 为准**。

### 6.2 缓存什么

| 键 | 值 | 失效 |
| --- | --- | --- |
| `route:<gateway_model>` | 生效修订的候选集：合同、承载面、参数映射、限制、`routing_priority`、`weight`、定价输入（`reference_cost_microusd`、`markup_bps`），**加发布修订标识 `runtime_revision_id`** | 发布成功后主动失效；另设 TTL |
| `api_key:<sha256(key)>` | `account_id` | 吊销时主动删；另设 TTL |
| `user_balance:<account_id>` | 余额（microUSD）+ 写入时间 + **来源标记**（`db_commit` / `reconciler`，§6.4） | 充值/受理/结算后立即写；另设 TTL |

**route 缓存的值必须带发布修订标识，受理时比对（否则陈旧不可检）**：

- 值里带 `runtime_revision_id`——写这条缓存的**那次发布**的修订标识；
- 受理时先做一次**轻量 DB 读**取"当前生效修订标识"（按 `gateway_model` + `active` 取 `runtime_revision_id`，走 §4.2 换过的新索引），与缓存值里的标识**比对**：一致才用缓存，**不一致（或值里根本没有这个标识）就当未命中，回源 DB** 取候选集并重建缓存。因此 route 缓存的陈旧是**可检的**，不再依赖"发布后的失效一定成功"；
- 同时按主键读一行 `publication.gateway_models.enabled`（§2.1 的判据）：`enabled=false` 是可变表里的事、**不改变修订标识**，所以这一项不交给 route 缓存判定，`PATCH` 之后照旧从目录与受理里消失；
- **SET / 失效失败时的行为**：失败**不影响正确性**——失败只是让缓存里留着旧值（带旧修订标识），下一次受理的比对必然不一致 ⇒ 回源 DB；`PATCH enabled` 的失效失败同理，由受理时那一次按主键读 `enabled` 兜住。失败本身记一条日志，运营可发现（`ADR-0017`）；
- **可接受的陈旧窗口**：**正确性上的窗口是 0**——陈旧缓存永远不会被用来选路，因为受理时一定会比对修订标识。剩下的只是"命中率"窗口：发布后到下一次受理之间缓存里可能还是旧值，最长 TTL（默认 60 秒）内每次受理都会回源一次并重建缓存。TTL 因此只用来兜住"发布后没人调用、旧值白占内存"与"缓存层自身故障后的恢复"，不影响任何对客结果。

### 6.3 写入时机（先 DB，后 Redis，且只在提交成功后）

- **发布成功**（事务提交后）：失效并重建 `route:<gateway_model>`；
- **充值**：DB 事务提交后 `SET user_balance:<id>`（用户要求：充值后立即 `SET`），来源标记 `db_commit`；
- **结算**：DB 事务提交后 `SET user_balance:<id>`（用户要求：扣减成功后立即 `SET`），来源标记 `db_commit`。写的是**扣减后的余额值**，不用 `DECRBY`——`DECRBY` 表达不了"以 DB 为准"，重放还会漂移；
- **受理（预授权扣减）**：DB 扣减成功后同样刷新（来源标记 `db_commit`），否则缓存会滞后一个预授权额。

### 6.4 扣费流程：Redis 只做加速，权威在 DB

**受理**（现状，`crates/persistence` 的 `create_job`，本设计只改"预授权额从哪来"）：

```sql
UPDATE ledger.accounts SET balance_microusd = balance_microusd - $预授权
WHERE id = $1 AND balance_microusd >= $预授权
```
`rows_affected != 1` ⇒ `insufficient_balance`（对客 402 余额不足）。`$预授权` = **本次请求的售价快照**（快照单价 × 请求 `n`，§3.6），不再是"永远一个固定数"；**缺售价快照单价时回落为 `GENERATION_MAX_COST_MICROUSD`**（§3.3/§3.6）。**余额是这里唯一的上限**——有售价可算时 `GENERATION_MAX_COST_MICROUSD` 不参与判定（§3.6）。

**结算**（现状，`complete_job`，本设计不改）：同一事务里 `release` 剩余授权 + `capture` 实收。

Redis 的位置：

- 缓存命中且余额充足 → **仍走 DB 条件更新**（正确性在 DB，缓存只是少一次读）；
- 缓存命中且余额不足 → **这是全设计唯一一处允许"缓存的判定结果直接决定对客响应"的地方**：仅在缓存**新鲜**时提前返回 `insufficient_balance`；不新鲜一律交给 DB。

**"新鲜"的判据（两条都要满足）**：

1. 值带**来源标记 `db_commit`**——由 DB 提交后的写入产生（§6.3 的充值 / 受理预授权扣减 / 结算三条写穿路径）。定时对账写回的条目标 `reconciler`（§6.5），**不用于提前拒绝**；
2. 写入时间距当前 < **新鲜窗口**（默认 5 秒，可配），且这个窗口必须显著小于定时对账周期（默认 3 分钟），保证"能用来拒绝的值"实际都来自写穿路径。

来源不明、没有写入时间戳的旧格式条目，一律**视为不新鲜**（宁可多打一次 DB）。

**提前拒绝必须落审计**：每次提前拒绝写一条 `operations.audit_events`（`account_id`、缓存余额、写入时间与来源、判定结果），使误拒**可发现、可对账**（`ADR-0017` 的"平台侧事件必须可发现"）。

**这一处不需要新 ADR**（用户更正，撤回上一轮的"需新 ADR"）：Redis 在这里就是"判断用户余额做预检"，**扣费仍然只在 PG 里发生**，不涉及资金安全，因此 `ADR-0003` 的"缓存不是事实源"已经覆盖。设计上写明两条即可：

1. **缓存永不作为扣费依据**：扣减只在 PG 事务里做（受理的预授权扣减、结算的 `release` + `capture`，见本节开头）；Redis 的写入一律是"**DB 提交成功之后、写扣减后的值**"，**不是 `DECRBY`**（`DECRBY` 表达不了"以 DB 为准"，重放还会漂移）；不一致时**以 DB 覆盖**（§6.6）。缓存里"够不够"的结论**从不决定扣减**——扣减由 DB 的条件更新决定。
2. **预检拒绝要留审计**：预检**没有 DB 记录**（不建 Job、不扣款、不写状态），事后必须能解释"为什么拒了这个客户"——这是**可解释性**要求（落点就是上面那条 `operations.audit_events`），不是资金安全要求。

因此"Redis 说够、DB 说不够"由 DB 兜住；"Redis 说不够"只在新鲜窗口内发生、必然留下审计，且拒绝不产生任何副作用（不写状态、不扣款，调用方重试即可）。

### 6.5 定时对账兜底

独立定时任务，每 N 分钟（默认 3）：

1. 把 DB 的 `ledger.accounts.balance_microusd`（按 `updated_at` 增量，必要时全量）写回 `user_balance:*`，来源标记 `reconciler`（§6.4：这种条目**不用于提前拒绝**）；
2. 校正 `route:*`（以当前生效修订为准）；
3. 校正 `api_key:*`（以 `identity.api_keys.revoked_at` 为准）。

### 6.6 不一致的处置

**以 DB 为准**：发现 Redis 与 DB 不一致时，用 DB 的值**覆盖**缓存，并记一条 `operations.audit_events` + 一条日志（运营要能发现，参照 `ADR-0017` 对"平台侧事件必须可发现"的要求）。不尝试"合并"或"取中间值"。

### 6.7 引入成本与风险

新增一个运行时依赖（`compose.yaml` 加服务、`.env.example` 加地址、多一条故障路径）。**仓库今天完全没有 Redis**（全仓无任何 `redis` 引用）。**用户已批准引入**（纯加速层，见 §9「已定案」）；即便不引入，本设计其余部分也不受影响，只是余额与路由全部直查 DB（即现状）。

## 7. 不做

- **渠道/供给启停接口**（提案第 1 条，用户撤回）；
- **调用记录查询接口**（提案第 4 条后半，用户撤回）；
- **跨厂商统一图片参数语义**（`ADR-0015`：属后期独立规划）；
- **通用参数映射引擎**（沿用 `#10` 口径：只做"合同 → 该候选承载面"的校验与装载）；
- **发布的分步 CRUD / 草稿态**（§2.4 理由）；
- **自动重试与失败后改道**（`ADR-0009`/`ADR-0011`，属 [`#11`](https://github.com/dehuadong/seeaihub-server-next/issues/11)）；
- **成本进账本与账实核对**（[`#11`](https://github.com/dehuadong/seeaihub-server-next/issues/11) 的成本进账本与账实核对那部分）；
- **对外价策略本身与具体数值**（[`#5`](https://github.com/dehuadong/seeaihub-server-next/issues/5)）；
- **把 `config/bootstrap/*.json` 当后台**（用户明确：只作初始化种子与测试夹具）。

## 8. 切片与验收

每片**独立可验收**，且全部**离线**（进程内假上游 + 直接查库，见 `apps/api/tests/http_contract.rs` 的既有做法），**不产生任何计费调用**。

### P1 网关模型命名层与对客目录

**改动**：发布命令加 `gateway_model`；`runtime_revisions` 增 `gateway_model` / `vendor_model_id` **两列（P1 落的命名两列）**；`publication.gateway_models`；`GET /v1/models` 投射（网关名 + `vendor_id` + 替换 `model.const`）；`GET /api/v1/gateway-models`；`PATCH .../enabled`；发布期 `model.const` 校验。**定价三列 `markup_bps` / `reference_cost_microusd` / `cost_basis` 在 P2b 落**（§1.6 同一套口径：迁移**分两次**，P1 一次、P2b 一次）。

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
- `declared` 路径：假上游终态声明 `cost` → `provider_cost_source = declared`，金额与币种逐位落库；
- `computed` 路径：AIHubMix 不给金额字段 → `provider_cost_source = computed`，金额按已发布费率 × 分项 token 自算；
- `unavailable` 路径：声明了但**缺字段 / 负数 / 解析失败** → 金额列留 NULL、**不猜**（不写 0、不用费率顶替），该笔成本缺口可发现（§3.7）；
- 采集 `cost` **不改变**对客结算金额与计量事实（同一个用例里断言 `charge` 与今天逐位相同）。

### P2b 定价公式与售价快照

**改动**：`runtime_revisions` 增 `markup_bps` / `reference_cost_microusd` / `cost_basis` **三列（P2b 落的定价三列，口径 `Computed` / `Declared`，随快照冻结）**；`pricing.fx_rates` + `PUT /api/v1/fx-rates`；`PriceSnapshot` 扩展（`cost_basis` / `reference_cost_microusd` / `markup_bps` / `fx_rate` / `consumer_price_microusd`）；`charge_microusd` 改为只读快照单价；**预授权由售价派生**（**hold = 本次请求的售价快照 = 快照单价 × 请求 `n`**）且 `GENERATION_MAX_COST_MICROUSD` 退为**"无售价可算时"的兜底 hold**（**缺 `consumer_price_microusd` 时 hold 回落到它**，不再是受理上限）；`jobs.charge_microusd` 投影列**缓做**。

**验收（离线）**：
- 给定已知参考成本 / `markup_bps` / 汇率 → 受理时冻结的 `consumer_price_microusd` 等于"参考成本 ×(1 + markup)× fx"，**逐位断言**；
- **同一网关模型**命中两条成本不同的候选 → 对客扣费**逐位相同**（默认读法，§3.4），差异只体现在毛利（用 P2a 的成本列断言）；扣的是**同一个账户**；
- 预授权（hold）= 快照单价 × **请求 `n`**（即**本次请求的售价快照**）；结算 `charge` = 快照单价 × **实际产出张数**，**封顶在 hold**（不出现"实收超授权 ⇒ 进对账"）；
- **超产出不加收、留痕**：构造假上游**产出多于请求 `n`**（异常）→ 实收仍**封顶在 hold**、**不向客户加收**；多出的张数的渠道成本与毛利缺口能从 `attempts` 与快照算出来并**留痕**（§3.6/§5）；
- **旧修订 + 新 Job → 走旧口径 hold**：迁移后仍生效、但**没有定价**（`consumer_price_microusd` 缺）的旧修订受理出的新 Job，hold **回落到 `GENERATION_MAX_COST_MICROUSD`**（今天的行为），结算也走旧口径（§3.3/§3.6）；
- **售价不受固定数限制**：构造 `hold > GENERATION_MAX_COST_MICROUSD` 的定价 → **照常受理**（不拒绝、不截断、不写审计）；**受理的唯一上限是客户余额**——`hold > 余额` ⇒ 对客 **402 `insufficient_balance`**，不产生 Job、不扣款（§3.6/§6.4）；
- 历史 `price_snapshot`（缺 `consumer_price_microusd`）的结算结果与今天**逐位相同**；
- 毛利 = 售价（快照）− 成本（`attempts`）可逐笔算出，`provider_cost_source` 区分 `declared` / `computed` / `unavailable`，各一个用例；
- 汇率改一次不影响已受理 Job（快照生效）。

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

## 9. 未决（用户持有，2 条）

1. **权重语义**：接受"同优先级多候选、档内按权重分流"（需换唯一索引，**就地修订 `ADR-0009`**，本设计建议），还是"权重只作次级排序依据"（则同档只有一个候选，权重不起作用）；
2. **"与命中渠道无关"的确切含义（默认读法待确认）**：默认读法是"**金额也无关**"——同一个网关模型不管命中哪条渠道，对客户都是**同一个固定售价**，渠道成本只在**定价时**参考（§3.2/§3.4，本设计按此落地）；另一种读法是"只有**扣费对象**无关、金额仍随实际命中的候选成本变"，若按它落地，售价在受理时算不出来、预授权也就无法由售价派生（§3.6），要改回按候选成本在结算时计价。

现状（`pricing.price_plans` 按候选挂、费率即结算基数）售价确实随命中候选变——默认读法要改掉的正是这一点。

**加价系数与汇率的数值不属设计决策**（用户更正）：**加价系数由管理员创建网关模型时录入**（每个网关模型一个），**汇率由管理员在后台维护**（全局一条）；设计只规定字段位、录入入口与快照时机（§3.2）。因此这两条**不再列入未决**。

### 已定案（本设计直接决定，不再待决）

- **`markup_bps` 归属**：每个网关模型一个，**数值由管理员创建网关模型时录入**，随修订发布、随 Job 的 Price Snapshot 冻结（§1.6/§3.2）；
- **汇率维护入口与字段**：**全局一条** `pricing.fx_rates` + `PUT /api/v1/fx-rates`（管理员、写审计），**现在就立字段**，**数值与币种由后台管理员录入**（不属设计决策）（§3.2）；
- **Redis**：**已批准引入**（用户批准；**纯加速层**——缓存不是事实源，见 §6；**预检拒绝不需要新 ADR**——`ADR-0003` 的"缓存不是事实源"已覆盖，见 §6.4）。因此"是否批准引入 Redis"**不再待决**；
- **网关模型启停粒度**：**整个模型一个开关**（`publication.gateway_models.enabled`），不做候选级开关（§2.1/§2.4）；
- **预授权口径**：**由 Price Snapshot 的售价派生**——hold = **本次请求的售价快照**（快照单价 × **请求 `n`**）；实收按**实际产出张数**并封顶在 hold，超产出不加收、只留痕；`GENERATION_MAX_COST_MICROUSD` 只作**"没有售价可算时"的兜底 hold**（缺售价快照单价时回落到它），**不再是受理上限**——受理的唯一上限是**客户余额**（`hold > 余额` ⇒ 402 `insufficient_balance`），**运营要设成本护栏属 [`#9`](https://github.com/dehuadong/seeaihub-server-next/issues/9)、不在本设计**（§3.6）——**预授权口径本身不再归 [`#9`](https://github.com/dehuadong/seeaihub-server-next/issues/9)**；
- **"不可用时回退"的含义**：§4.4 已按**阶段**写清——受理前的候选不合格回退落地（现状）；提交前的失败技术上能回退，但当前策略是"失败不重试"（`ADR-0011`），改"回退下一候选"属**策略变更**；提交后的不确定（超时、断连、`5xx`）**不得回退**（会重复出图与重复计费），进对账。**"运行期回退"是否要做属 [`#11`](https://github.com/dehuadong/seeaihub-server-next/issues/11)，不在本设计范围**——因此这一条也不再待决。

### 范围边界（不待决，归其他工作项）

- **对外价策略与具体数值**（含是否分档、加价系数与汇率的具体取值）归 [`#5`](https://github.com/dehuadong/seeaihub-server-next/issues/5)，本设计只给机制（数值由后台录入，见上）；
- **成本护栏**（服务端成本上限一类的运营护栏）归 [`#9`](https://github.com/dehuadong/seeaihub-server-next/issues/9)——本设计只把**客户余额**当受理上限（§3.6）；
- **成本进账本与账实核对**归 [`#11`](https://github.com/dehuadong/seeaihub-server-next/issues/11)；
- **渠道/供给启停接口**与**调用记录查询接口**按用户口径不做（§7）。

### 需要 ADR 的决定（**须经用户确认后才立 / 才改**）

按仓库约定（`AGENTS.md`：记录持久 ADR 必须获得用户确认）**先确认再落 ADR**，本设计不自行创建、也不自行修订。

**新立**：

- **成本事实的落点**（`attempts` 承载渠道成本）与它与 `ADR-0006` 的关系；
- **对客售价的构成与"同一个网关模型一个固定售价"这一产品口径**（含 §9 未决第 2 条的含义）。

**就地修订既有 ADR**：

- `ADR-0006`：汇率从"Price Plan 发布时固定"改成"**全局表 + 受理时快照**"（§3.2）；
- `ADR-0009` ①：权重的语义与"优先级是档位、档内分流"这一选路模型——"数字小者优先**且同一型号内唯一**"要改成"档位内可多候选、按权重分流"（§4.2）；
- `ADR-0009` ②：预授权口径——"预授权金额与计价口径是两个量，不得互相推导…不由候选价格反算"要改成"**预授权由 Job 固化的售价快照派生**（hold = 快照单价 × 请求张数 `n`），`GENERATION_MAX_COST_MICROUSD` 只作**没有售价可算时的兜底 hold**，不再是受理上限（上限是客户余额）"（§3.6）。

另外两处是**设计级修订**（改 `docs/design/`，不动 ADR）：`GET /v1/models` 的字段名 `vendor` → `vendor_id`（`docs/design/0005` §8.1 已定该形状）、合同 `model.const` 的对客投射替换规则（§1.4）。

## 评审记录

- 状态：**待评审**（Plan Review 两轮发现已收口；用户更正与批准已并入，见下节第 15–17 条）。本文尚不构成实施依据。
- 评审完成后在此记录结论与批准依据（按 `docs/agents/artifacts.md`：`docs/design/` 的状态头表示**设计评审状态**，批准不由文件推断）。

## Plan Review 处置

两轮 Plan Review 的发现逐条落点——**第一轮 8 条**，另加**用户两处更正**（第 9、10 条），**第二轮（收敛轮）4 条**（第 11–14 条），**本轮用户更正与批准 3 条**（第 15–17 条）（本节编号只为对照评审清单，不构成正文引用）：

| # | 发现 | 落在哪 |
| --- | --- | --- |
| 1 | 权重无哈希输入（选路早于 JobId 生成） | §4.2 改为按 `(account_id, idempotency_key)` 确定性哈希，并写明**重放语义**（同键重放必然同候选、且去重成原 Job）；§4.5 记分流取值；§8 P3 验收改成"同一账户下不同的幂等键 + 同键重放同候选" |
| 2 | 加价撞固定预授权（对账只能退款、差额收不回） | §3.6 改为**预授权由 Price Snapshot 的售价派生**（`hold = 快照单价 × 请求 n`）；§8 P2b 加对应验收；§9 登记**就地修订 `ADR-0009`** 的预授权条款。**该轮对 `GENERATION_MAX_COST_MICROUSD` 的处置已由第 17 条整体改写**——它现在只作"没有售价可算时"的兜底 hold，**不再是受理上限** |
| 3 | Redis 提前拒绝与"缓存不是事实源"自相矛盾 | §6.4 保留性能收益，但**不写成"例外"**（与第 9 行及 §6.1/§6.4 的"不是例外"一致——`ADR-0003` 的"缓存不是事实源"已覆盖）：① 只有新鲜（来源标记 `db_commit` + 新鲜窗口）才允许提前拒绝；② 提前拒绝**必须落** `operations.audit_events`；③ 曾拟"需要**一条新 ADR**（缓存可用于拒绝的唯一条件）"，**该 ③ 已由第 9 条撤回**；④ §8 P4 补"陈旧缓存不得拒绝、误拒有审计" |
| 4 | route 缓存陈旧不可检 | §6.2 的值带**发布修订标识**，受理时与当前生效修订比对、不一致即**回源 DB**；写明 SET / 失效失败 ⇒ **当未命中、不影响正确性**；给出**陈旧窗口**（正确性上是 0，剩下的只是 TTL 命中率窗口）与理由；§8 P4 加"让失效失败 ⇒ 回源读到新候选集" |
| 5 | `ADR-0006` 就地修订未登记 | §3.2 注明汇率改动**就地修订 `ADR-0006`**；§9 新增"就地修订既有 ADR"清单；§3.5 写死结算口径——**对客结算只读 Job 固化的售价快照**，`attempts` 里的渠道成本**只用于毛利核算** |
| 6 | `markup_bps` 无落点 | §1.6 字段清单加 `markup_bps` / `reference_cost_microusd`，写明**随修订发布、随 Job 快照冻结**，并明确**不放** `publication.gateway_models`（那张表只存开关）；§3.2 同步 |
| 7 | 范围/事实缺口 | ① §2.5 + §8 P5 补"查看余额"的切片与验收，提案同步；② 提案背景改正"网关模型只能靠 `config/bootstrap/*.json` 发布"（`POST /api/v1/runtime-revisions` 早已是管理员 API）；③ §3.7 补 `unavailable` 的处置（不猜、进对账、不推进 `reconciliation_required`）与"成本来源可辨"的判据；④ 本文与提案对 `#9`/`#11`/`#5` 的引用一律去掉"第 N 项" |
| 8 | 最小性 | §8 拆成 **P2a 成本事实采集**（不依赖任何未决）与 **P2b 定价公式**；`jobs.charge_microusd` 投影列标**缓做**；§9 把三项技术上可自定的（维护入口/是否立字段、启停粒度、预授权口径）**直接定案**，留给用户收敛（当时 5 条）；§9 的"与命中渠道无关"那条（当时第 5 条）按评审建议把**"金额也无关"写成待确认的默认读法**（保留另一种读法一句话） |
| 9 | **用户更正：Redis 不需要新 ADR**（Redis 只是判断余额做预检，扣费仍在 PG，不涉及资金安全） | **撤回**"需新 ADR（缓存可用于拒绝的唯一条件）"：§6.4 删掉该条，改成写明**两条**——① **缓存永不作为扣费依据**（扣减只在 PG 事务里做；Redis 一律在 DB 提交成功后写**扣减后的值**、**不是 `DECRBY`**；不一致以 DB 覆盖）；② **预检拒绝要留审计**（预检没有 DB 记录，事后必须能解释"为什么拒了这个客户"，属**可解释性**、不是资金安全），并写明理由：`ADR-0003` 的"缓存不是事实源"**已覆盖**。§9「新立」清单去掉该条、未决里的"是否批准引入 Redis"那条（当时第 4 条）去掉"还要一条新 ADR"；§6.1 的"例外"措辞同步改为"不是例外"。§6.4 其余保留（新鲜窗口的来源标记、陈旧不得拒绝、误拒审计、P4 的验收） |
| 10 | **用户更正：回退语义按"阶段"写清，并给出 4xx/5xx 的判断口径** | §4.4 重写为**按阶段判定**表（**受理前** = 能、无副作用；**提交前** = 技术上能，但当前策略是"失败不重试"（`ADR-0011`），改"回退下一候选"属**策略变更**、归 `#11`；**提交后** = **不能**，上游可能已出图并已计费，再出一张就是"**重复出图、重复计费**"，进 `reconciliation_required` 人工对账），并写死判据——**`4xx` = 确定性拒绝、可证明未受理**（`401`/`402`/`403` 对客按平台侧故障，`ADR-0017`；`400`/`422` 渠道拒绝；`429` 明确未受理），**`5xx` 与超时/断连 = 不确定**、不得改道、不得自动重提；§9「已定案」与提案「开放决策」同步补"**运行期回退是否要做属 `#11`**"（不在本设计/提案范围） |
| 11 | **第二轮：hold 与 charge 口径不同**（§3.6 用"请求 `n`"、§5 用"实际产出张数"，谁说了算没写死） | **定案**：**hold 按请求 `n`**（× 快照单价）；**实收封顶在 hold**——若上游产出多于请求张数（异常），**不向客户加收**，多出的部分只记**渠道成本与毛利缺口**并**留痕**。§3.6 写死"hold 按请求 `n` / 实收按实际产出张数并取 min(算出额, hold) / 超产出不加收"三条，并**删掉"实收 ≤ 授权由构造保证"这种依赖假设的话**；§5「张数」行同步两个口径；§8 P2b 验收补"**超产出不加收、留痕**" |
| 12 | **第二轮：无定价的旧修订受理新 Job 的 hold 未定义** | **定案**：缺 `consumer_price_microusd` 时，hold **回落到 `GENERATION_MAX_COST_MICROUSD`**（即今天的行为）。§3.3 历史兼容写明两种来源（历史 Job / 无定价旧修订受理的新 Job）**hold 口径一致**；§3.6 补"没有售价可派生的历史口径"一条；§8 P2b 验收补"**旧修订 + 新 Job → 走旧口径 hold**" |
| 13 | **第二轮：`PriceSnapshot.cost_basis` 无落点** | **定案**：**加一列** `runtime_revisions.cost_basis`（口径 `Computed` / `Declared`）随修订发布、**随快照冻结**。§1.6 字段清单加该列并写明它**不放** `publication.gateway_models`；§3.3 快照字段注明来源；§8 **P2a** 注明该列不属它（P2a 只落 `attempts` 三列）、**P2b** 列清单加该列。理由是**"成本来源可辨"是毛利核算的要求** |
| 14 | **第二轮：残留措辞统一**（①"唯一例外" ② 提案 4xx 注缺限定 ③ 迁移列数表述不一） | ① 本节第 3 行改成"**不写成'例外'**"，与第 9 行及 §6.1/§6.4 的"不是例外"一致；② 提案「开放决策」注补一句限定——**某条 `4xx` 若无法证明未受理，同样按"不确定"处理**（与 §4.4 一致）；③ §1.6 与 §8 P1/P2b 统一为"**P1 落命名两列、P2b 落定价三列（`markup_bps` / `reference_cost_microusd` / `cost_basis`），迁移可分两次**" |
| 15 | **用户更正：加价系数与汇率不是"现在要定的数值"，而是后台管理员录入**（加价系数**创建网关模型时设置**、每网关模型一个；汇率**全局一条、由后台维护**） | ① 全文**不出现任何具体数值建议**：§2.2 的 `pricing` 示例改成占位并注明数值由后台录入；§3.2 两个量的落点行改为"**由管理员创建/发布时录入**""**由管理员在后台维护**，具体数值与币种不属设计决策"；§3.2 汇率段删掉"取恒等值"、改为"数值由后台录入，设计只立字段与快照位"；§9 范围边界把"具体取值"归 `#5`。② §9 未决**删去"加价系数数值""汇率数值/币种"两条**，改为一句话"**加价系数与汇率的数值不属设计决策：由后台录入**"；§9「已定案」两条同步。③ 提案「开放决策」由 5 条收敛为 2 条，并在「已定案」补录入方与入口 |
| 16 | **用户批准：引入 Redis** | §9 未决**删去"是否批准引入 Redis"**，改记"**已批准引入（纯加速层，缓存不是事实源；预检拒绝不需要新 ADR）**"并移入 §9「已定案」；§6.7 由"是否引入需用户批准"改为"**用户已批准**"；§8 **P4** 标题由"待用户批准后"改为"**用户已批准**"；状态头与「评审记录」相应说明 |
| 17 | **用户更正：预授权口径有坑**——hold 就是**本次请求的售价快照**（快照单价 × 请求张数 `n`）；`GENERATION_MAX_COST_MICROUSD` 默认值只有 $0.02，**当上限用会把正常请求全拒** | ① **删掉"超过 `GENERATION_MAX_COST_MICROUSD` 即受理前拒绝（503 + 审计）"**这条（§3.6 正文与 §8 P2b 验收同步删除）；② `GENERATION_MAX_COST_MICROUSD` **只作"没有售价可算时"的兜底 hold**（旧修订 / 历史 Job 缺 `consumer_price_microusd` 时回落到它，§3.3/§3.6）；③ **真正的上限是客户余额本身**——`hold > 余额` ⇒ 402 `insufficient_balance`，不产生 Job、不扣款（§3.6/§6.4）；④ **运营要设成本护栏属 [`#9`](https://github.com/dehuadong/seeaihub-server-next/issues/9)、不在本设计**（§9 范围边界）；⑤ §3.6 标题、§3.6 的 `ADR-0009` 修订段、§6.4、§8 P2b 改动行与验收、§9「已定案」预授权口径同步改写 |

**留给用户的 2 条**：**权重语义**（档内确定性分流需就地修订 `ADR-0009`，还是只作次级排序）、**"与命中渠道无关"的确切含义**（默认读法＝同一网关模型一个固定售价，渠道成本只在定价时参考）——**两轮发现、两处更正与本轮 3 条都未新增待决项**；加价系数与汇率的数值由后台录入，**不属设计决策**（§9）。

**本文相对上一修订新增的两处字段级说法**（评审时请一并看）：`runtime_revisions.reference_cost_microusd`（定价时参考的渠道成本，发布数据）与 `PriceSnapshot.consumer_price_microusd`（受理时算定的对客单价）——它们是"预授权由售价派生"与"金额也无关"这两条的前提，理由在 §3.2/§3.3。
