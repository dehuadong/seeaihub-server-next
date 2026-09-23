主题: 图片生成服务端的分层架构与更新方式
当前修订: v1
状态: 生效（使用中）· 评审记录缺口待用户确认（2026-09-20 由「待评审」更正，工作项 [#6](https://github.com/dehuadong/seeaihub-server-next/issues/6)）。本文随第二阶段交付落地，并被实现与多份文档引用；**但仓库内没有可复核的设计评审通过记录**——按 [`docs/agents/artifacts.md`](../agents/artifacts.md)（`docs/design/` 的状态头表示**设计评审状态**、批准不由文件推断）的约定，该缺口**待用户事后确认**，不由收口代理单方推定。2026-09-19 起草时用于纠正第二阶段规划中把渠道差异下沉到领域模型的偏差。**评审指出的两处越界已修**：§4 判据由公理改为「应当能逐项说明理由」并列出三类合法例外 E1/E2/E3；R2 已如实收窄，不再替「消费侧是否可等结果」作决定（该选择列为后续工作项）。**本文与 [`docs/adr/0015`](../adr/0015-vendor-model-contract-and-offering-parameter-mapping.md) 的冲突已就地标注**：§1（示意图与表格的 ③ 行）、§2 R1、§3.2、§3.3 的旧表述与 0015 不一致，见各节标注。
来源: 依据 `docs/design/0001-image-generation.md`（实现映射）、`docs/design/0002-image-generation-tech-design.md`（技术设计 v5）、`docs/adr/0001`–`0015` 与仓库现状归纳，不引入新决策

# 图片生成服务端的分层架构与更新方式

本文回答一件事：**每一层负责什么、由谁拥有、用什么方式更新**。它不新增决策，只把既有实现映射（0001）与技术设计（0002）里的职责边界写成一张可核对的层级表——因为第二阶段规划一度把本该属于 Adapter 的渠道差异下沉到了领域模型，需要一份权威表述来防止再次走偏。

## 1. 五层

```text
① Model Protocol ── 对外一致（消费侧契约）            代码发版
② Adapter Driver ── 一个 Provider 一族                代码发版
③ Model Profile  ── 调用方合同（Vendor Model 级）+ 该 Offering 的承载面  运行时版本发布
④ Offering       ── 把 Profile 绑到可售供给           运行时发布
⑤ Price          ── 钱的事                            PG 动态配置
```

| 层 | 负责什么 | 由谁拥有 | 如何更新 | 在本仓库的落点 |
| --- | --- | --- | --- | --- |
| **① Model Protocol** | 对外路由、请求/响应外壳、同步/异步**对外形态** | 应用层 | 代码发版 | `apps/api` 的 HTTP 适配层；`CreateImageGeneration` 命令；`docs/design/0002` §4 |
| **② Adapter Driver** | 上游路径、封装格式、响应解析、Evidence 提取、错误分类、轮询与取图 | Adapter crate | 代码发版 | `crates/adapter-sdk` + `crates/adapter-*` |
| **③ Model Profile** | 型号的**调用方参数合同**（Vendor Model 级，唯一一份），以及**该 Offering 能承载的面**（能力子集）：支持参数、值域、默认值、组合规则、说明 | 目录 | **运行时版本发布** | `catalog.vendor_models.capability_schema`（随 Runtime Revision 发布）。**当前实现仍是"每候选各带一份合同"——合同与承载面尚未拆开，见 [`docs/adr/0015`](../adr/0015-vendor-model-contract-and-offering-parameter-mapping.md) 与工作项 [`#6`](https://github.com/dehuadong/seeaihub-server-next/issues/6) 的 G5** |
| **④ Offering** | Provider、上游模型名、用哪个 Driver、渠道限制、优先级 | 供给面 | 运行时发布 | `supply.offerings` + `publication.runtime_entries` |
| **⑤ Price** | 渠道**计价形态**（按 token 计量量 / 按产出张数 / 按调用次数 / 上游直接给实扣金额）、费率与单价、**成本侧的渠道币种**、生效区间（"上游金额口径"不另立形态：上游直接给金额的情形由成本侧的 `declared` 承接）。**汇率不在这一层固定**：它是按币种维护的折算率表（渠道币种 → CNY），受理时取生效那一行并快照，只用于把成本折算成 CNY 做毛利核算；**对客一律 CNY** | 价格 | PG 动态配置 | `supply.offerings.formula` / `cost_unit_price_microusd` + `pricing.price_plans`（按 token 计量量计价时的费率表） |

**① 层的已知差距（2026-09-20 登记，同日部分收口）**：本表要求 ① 是"对外一致（消费侧契约）"，接入新 Provider 时无需改动。**已做掉的**：对客请求体改成**扁平**（不再有 `native_parameters` 外壳）、图片改用 OpenAI 契约的 `image` / `mask`（值为公网 URL 或 data URL，平台不再托管素材），参数路径与 `position` 不再出现在调用方面前；AIHubMix 素材与 Adapter 的 `extra` 包装已去掉（`quality` 顶层）。**仍存在的**：其余字段名与取值仍随候选不同（例如同一个 `size`，AIHubMix 收 `1024x1024`、APIMart 收 `1:1` 且另有 `resolution`），调用方仍要看命中哪个候选；合同归属与目标形态由 [`docs/adr/0015`](../adr/0015-vendor-model-contract-and-offering-parameter-mapping.md) 确定，剩余收口对应的差距登记在工作项 [`#6`](https://github.com/dehuadong/seeaihub-server-next/issues/6)。

## 2. 四条不会违反的规则

这四条都能在既有设计里找到原文依据；它们是防止「渠道差异污染领域」的边界。

**R1 · 上游的同步/异步与响应形态，全部封装在 ②。**
依据：0001「Provider 调用、结果格式和**同步/异步差异**封装在 Adapter」；0002 §4「`generations`/`edits` 的 endpoint 与 JSON/multipart 差异只存在于 Adapter」「Provider 同步**不等于**平台同步」。

因此：上游是同步响应还是任务式轮询、返回 Base64 还是 URL、返回 token 还是金额——**都是 ② 的内部实现**，不产生领域类型分支，不改状态机，不改表结构。

**⚠️ R1 的边界（2026-09-20，按 [`docs/adr/0015`](../adr/0015-vendor-model-contract-and-offering-parameter-mapping.md) 收窄）**：R1 说的是**传输与响应形态**。**调用方可见的参数名与布局不属于 R1 的"内部实现"**——那是 **Offering Parameter Mapping** 的职责（见 [`docs/adr/0015`](../adr/0015-vendor-model-contract-and-offering-parameter-mapping.md)）；Adapter 只按**已经定形**的参数装载传输。此前把"渠道怎么布局参数"当成 ② 内部实现、进而把渠道字段名暴露给调用方，正是 #6 记录的 G1 病根。

**R2 · 对外生命周期只有一种。**
依据：0002 §4「调用方先得到持久 Job，Worker 在后台等待 Provider 响应并维护 lease/heartbeat」「以后是否增加同步等待型公开 API，**不影响这个执行模型**」。

因此：**平台对外始终是「持久 Job + 可查询」**，上游是同步还是任务式都不改变这一点。

**本文不决定**「消费侧是否可以额外选择等结果」——那是一个**尚未作出的产品选择**，属后续工作项（见工作项 #2 的规划 §7）。原文只说明「即使将来增加，也不影响执行模型」，**不等于已经决定要增加**。此处如实收窄，避免本文声称「不引入新决策」却引入一个。

**R3 · Profile 是「数据」，不是代码。**
依据：0002 §5「以机器 Schema 为上游证据，导入后形成平台自己的不可变修订」；更新流程是「抓取候选 → 差异检查 → 审核 → 发布新 Runtime Revision」。

因此：新增一个 Provider 或型号，**不应触发领域模型或数据面的编译/迁移**——发布新的 Profile 与 Offering 即可。

**R4 · 钱的形态由 ⑤ 声明，证据由 ② 归一提取。**
依据：0002 §9 Adapter 能力含 `parse_result` / `extract_evidence` / `classify_error`；§11「价格是运行时 Price Plan 数据，不写进 Adapter 代码」。

因此：Adapter 把上游响应**归一成该 Offering 声明的计价单位所需的证据**；Price Plan 声明按什么计价。上游给 token 就给 token 型证据，上游只给金额就给金额型证据——**这是 ② 的职责，不是领域模型的分叉**。（2026-09-20 补注：金额型证据这一支经实测**不需要**——两家渠道都已返回分项 token；该候选决策已被否决并退役，见 `.agents/notes/rejected/domain/2026-09-19-provider-declared-charge-as-metering-evidence.md`。）

## 3. 边界上四件容易搞错的事

**3.1 「渠道限制」不是「能力收窄」。**
Profile（③）声明该 Provider 实际支持什么；Offering（④）的渠道限制只是**在该范围内再收紧**（例如「这个 Offering 只开放 1 张」）。二者是**同一事实的发布位置不同**，不是「从全集推导子集」的证明关系。因此不需要「合成有效 Schema」「禁止正则类参数收窄」这类机制——那是把一个数据发布问题做成了形式化证明问题。

**3.2 同一型号由两个 Provider 供应时，Profile 的粒度。**

> **⚠️ 本节已被 [`docs/adr/0015`](../adr/0015-vendor-model-contract-and-offering-parameter-mapping.md) 取代（2026-09-20）**：把 Profile 的粒度定为 **(Vendor Model Revision × Provider)**、并"必要时发布两份 capability schema 修订"的做法，会让**调用方合同**随 Provider 分叉——这正是 0015 判定为错位的形状。按 0015，**调用方合同是 Vendor Model 级的唯一一份**；③ 保留的是**该 Offering 能承载的挂载面**（它对哪些参数、哪些值域、哪些分支可执行），差异只在**能力子集**上，不再各自定义一份调用方合同。下面保留原文，仅作历史理由；落实所需的模型改动（合同需要唯一载体、`capability_schema` 的双重身份要拆开）登记为工作项 [`#6`](https://github.com/dehuadong/seeaihub-server-next/issues/6) 的结构性差距 G5。

0001 已定「Native Capability Schema、Offering、Channel 和 Price Plan 通过 Runtime Revision 发布」，因此 Profile 是**发布物**，粒度是 **(Vendor Model Revision × Provider)**。（**已被取代**，见上方标注。）两种等效的落法：

- **落法一（推荐，改动最小）**：Profile 仍随 `catalog.vendor_models` 的 capability schema 发布，但其内容按 **Offering 所声明的 `provider_model_id`** 声明——即「本 Offering 实际可发的参数面」。两个 Provider 供同一型号时，发布两条 Offering，必要时发布两份 capability schema 修订。
- **落法二**：把 capability schema 直接做成「按 Provider 分列」的一张发布物。

无论哪种，**共同点是：Provider 的支持面差异写在发布物里，而不是靠「从能力全集收窄」的证明机制**。

**3.3 `native_model_id` 与 `provider_model_id` 的分工（0001 明确要求分开固化）。**
- `native_model_id`：平台**对外的**型号身份，稳定、用于路由与 Job 固化（0001：「平台 `native_model_id` 与供应商调用所需 `provider_model_id` 分开固化」、ADR-0004）；
- `provider_model_id`：**该 Provider 实际调用的模型字符串**，由 Offering 携带，**只有 Adapter 使用**（0001：「Adapter 只使用后者组装供应商请求」）。

因此「两个 Provider 供同一型号」在目录里表现为：**两条 Offering，共同指向稳定身份，各自携带自己的 `provider_model_id`**；Provider 的**承载面**（能力子集）差异落在各自的发布物上，而**调用方合同不随 Provider 分叉**（[`docs/adr/0015`](../adr/0015-vendor-model-contract-and-offering-parameter-mapping.md)）。这既满足了「同一 Vendor Model 多 Offering」，又不把渠道实现暴露到对外身份上。

**具体形态**（以第二阶段的真实供给为例）：

```text
对外稳定身份（路由与 Job 固化用）        provider_model_id（只有 Adapter 用）
gpt-image-2.5-flare                      gpt-image-2.5-flare      ← AIHubMix 的 Offering
gpt-image-2.5-flare                      <APIMart 实际接受的模型名>  ← APIMart 的 Offering
```

两条 Offering 的 `native_model_id` 相同（因此是同一个「型号」的多个供给），`provider_model_id` 各自独立（因此各渠道的实际调用字符串可以不同）。**这正是「同一 Vendor Model 由多个 Provider 供应」的落地形态**，也是本层设计要支撑的目标。

**3.4 对账标识与恢复能力不是渠道问题，是可靠性边界。**
依据：0002 §7「本地幂等键和数据库唯一约束保证一个 Job 只建立一个 Attempt；Worker 提交前写入 `submitting` 并持续维护租约，通用重试器**不得重放 Provider POST**」。
因此：无论上游同步还是任务式，「创建请求绝不重发」是统一规则；任务式上游带来的差别只是「任务查询是幂等读操作，可在同一次执行内重试」——那是 ② 内部的事。

## 4. 一个新 Provider 的接入清单（本表的用途）

按层落位，接入一个新 Provider 只需要动这几处，**逐项都能指出落在哪一层**：

| 步骤 | 层 | 动作 | 是否改代码 |
| --- | --- | --- | --- |
| 1 | ② | 新增该 Provider 的 Adapter（路径、封装、解析、证据提取、错误分类、轮询/取图） | **是**（代码发版） |
| 2 | ③ | 抓取该 Provider 的机器 Schema → 差异检查 → 审核 → 发布 Profile 修订 | 否（运行时发布） |
| 3 | ④ | 为每个型号发布一个 Offering（Provider、上游模型名、用哪个 Adapter、渠道限制、优先级） | 否（运行时发布） |
| 4 | ⑤ | 发布该 Offering 的 Price Plan（计价单位、单价或金额口径、成本侧的渠道币种） | 否（PG 配置） |
| 5 | ① | **无需改动**（对外契约不变） | 否 |

**判据**：接入一个新 Provider 时，若改动落在 ① 层或领域模型上，**应当能逐项说明理由**——判据是「能否说明」，不是「一旦改动就是落错层」。以下三类是**合法例外**，本阶段逐项出现且都有理由：

| 例外 | 内容 | 为什么合法 |
| --- | --- | --- |
| **E1 平台侧新能力** | 多 Offering 路由需要加列、换唯一索引、改选中入口的返回类型 | 这是平台自己的新能力（现有约束只允许一个生效供给），不是渠道差异泄漏 |
| **E2 上游事实带来的形态扩展** | 若某上游只给「扣费金额」而既有证据类型是具体 token 结构，则领域类型必须扩展 | 新事实需要新形态；但**形态本身应待实测后再定**，不得在事实之前预造 |
| **E3 装配点** | 多个 Adapter 需要按 `adapter_key` 派发的组合工厂 | 纯装配，不涉及领域与表结构 |

**除这三类之外**，接入一个新 Provider（加 Driver、发 Profile/Offering/Price、按优先级路由）**不应触发 ① 层或领域模型的改动**。若实现中发现确实触发，应回到 Planning 说明理由，而不是就地扩模型。

## 5. 与既有工件的关系

- 本文只归纳，**不取代** `0001`（实现映射）与 `0002`（技术设计，v5 已评审通过）；冲突时以 `0002` 为准。
- 持久决策仍在 `docs/adr/`。本文引用的依据来自 `0001`、`0002`、`adr/0001`（统一 Command）、`adr/0003`（PG 事实权威）、`adr/0004`（Vendor/Provider 身份分离）、`adr/0006`（证据门槛）、`adr/0007`（不自动重提）、`adr/0019`（图片按渠道原形进原形出、不落盘静态资产，取代 `adr/0008`）。
- 本文的用途是**约束后续规划**：新增 Provider 的工作量应按 §4 的清单评估，不得把渠道差异下沉为领域模型改动。
- **落点索引**：本文说"每层负责什么、由谁拥有"；具体落在哪个 crate、哪个文件、哪张表，见 [`docs/architecture.md`](../architecture.md)（那份文档只做索引，职责与规则仍以本文为准，冲突时以本文为准）。
