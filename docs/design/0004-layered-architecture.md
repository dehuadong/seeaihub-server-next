> 归属状态（2026-10-09）：归属切换未完成；本文件适用且已接受的范围暂保留旧属主，原待评审／待接受内容不因此获批准。依赖它的新工作先核对有效内容及评审缺口，不得默认沿用。 全部映射与未决影响见[归属切换登记](../agents/document-ownership-transition.md)。以下原文与状态头保留其历史身份，不扩大本文权威。

主题: 分层规则的依据与边界细则
当前修订: v1
状态: 生效（使用中）· 评审记录缺口待用户确认（2026-09-20 由「待评审」更正，工作项 [#6](https://github.com/dehuadong/seeaihub-server-next/issues/6)）。按 [`docs/agents/artifacts.md`](../agents/artifacts.md)（`docs/design/` 的状态头表示**设计评审状态**、批准不由文件推断）的约定，该缺口**待用户事后确认**，不由收口代理单方推定。
来源: 依据 `docs/design/0001-image-generation.md`（实现映射）、`docs/design/0002-image-generation-tech-design.md`（技术设计 v5）、`docs/adr/0001`–`0015` 与仓库现状归纳，不引入新决策

# 分层规则的依据与边界细则

分层、每层的职责与更新方式、依赖方向、边界规则（R1–R5）与扩展纪律的正文归[架构治理](../architecture.md)拥有。本文保留这些规则的依据、边界细则与合法例外的理由；它不重复规则正文，也不承载 crate、文件、表与端点清单。

## 1. 五层

规则正文见[架构治理](../architecture.md) §1–§2。

**① 层的已知差距（2026-09-20 登记，同日部分收口）**：要求 ① 是"对外一致（消费侧合同）"，接入新 Provider 时无需改动。**已做掉的**：对客请求体改成**扁平**（不再有 `native_parameters` 外壳）、图片改用 OpenAI 合同的 `image` / `mask`（值为公网 URL，平台不再托管素材），参数路径与 `position` 不再出现在调用方面前；AIHubMix 素材与 Adapter 的 `extra` 包装已去掉（`quality` 顶层）。**仍存在的**：其余字段名与取值仍随候选不同（例如同一个 `size`，AIHubMix 收 `1024x1024`、APIMart 收 `1:1` 且另有 `resolution`），调用方仍要看命中哪个候选；合同归属与目标形态由 [`docs/adr/0015`](../adr/0015-vendor-model-contract-and-offering-parameter-mapping.md) 确定，剩余收口对应的差距登记在工作项 [`#6`](https://github.com/dehuadong/seeaihub-server-next/issues/6)。

### 1.1 外部服务适配器的判据

规则正文见[架构治理](../architecture.md) §4。判定看事实是谁的：上游的调用路径与响应形态属 ②；平台自己选定的外部服务的协议与凭证属这一类。把外部服务塞进 ② 的外延会让「Provider 一族」这个边界失效——每接一个外部依赖都像多了一条供应渠道，§4 的「接入一个新 Provider」清单也覆盖不了它。上传素材用的对象存储属这一类，落点见[对象存储上传设计](./0021-object-storage-upload.md) §1。

## 2. 边界规则的依据

规则正文（R1–R5）见[架构治理](../architecture.md) §3。

**R1 的依据**：0001「Provider 调用、结果格式和**同步/异步差异**封装在 Adapter」；0002 §4「`generations`/`edits` 的 endpoint 与 JSON/multipart 差异只存在于 Adapter」「Provider 同步**不等于**平台同步」。
R1 的边界（2026-09-20，按 [`docs/adr/0015`](../adr/0015-vendor-model-contract-and-offering-parameter-mapping.md) 收窄）：R1 说的是**传输与响应形态**；调用方可见的参数名与布局不属于 ② 的内部实现，那是 **Offering Parameter Mapping** 的职责，Adapter 只按**已经定形**的参数装载传输。此前把"渠道怎么布局参数"当成 ② 内部实现、进而把渠道字段名暴露给调用方，正是 `#6` 记录的 G1 病根。

**R2 的依据**：0002 §4「调用方先得到持久 Job，Worker 在后台等待 Provider 响应并维护 lease/heartbeat」「以后是否增加同步等待型公开 API，**不影响这个执行模型**」。
本文不决定「消费侧是否可以额外选择等结果」——那是一个**尚未作出的产品选择**，属后续工作项（见工作项 #2 的规划 §7）；原文只说明「即使将来增加，也不影响执行模型」，**不等于已经决定要增加**。

**R3 的依据**：0002 §5「以机器 Schema 为上游证据，导入后形成平台自己的不可变修订」；更新流程是「抓取候选 → 差异检查 → 审核 → 发布新 Runtime Revision」。

**R4 的依据**：0002 §9 Adapter 能力含 `parse_result` / `extract_evidence` / `classify_error`；§11「价格是运行时 Price Plan 数据，不写进 Adapter 代码」。（2026-09-20 补注：金额型证据这一支经实测**不需要**——两家渠道都已返回分项 token；该候选决策已被否决并退役，见 [`.agents/notes/rejected/domain/2026-09-19-provider-declared-charge-as-metering-evidence.md`](../../.agents/notes/rejected/domain/2026-09-19-provider-declared-charge-as-metering-evidence.md)。）

**R5 的依据**：0001「平台 `native_model_id` 与供应商调用所需 `provider_model_id` 分开固化」、`ADR-0004`。

## 3. 边界上三件容易搞错的事

### 3.1 「渠道限制」不是「能力收窄」

Profile（③）声明该 Provider 实际支持什么；Offering（④）的渠道限制只是**在该范围内再收紧**（例如「这个 Offering 只开放 1 张」）。二者是**同一事实的发布位置不同**，不是「从全集推导子集」的证明关系。因此不需要「合成有效 Schema」「禁止正则类参数收窄」这类机制——那是把一个数据发布问题做成了形式化证明问题。

### 3.2 对账标识与恢复能力不是渠道问题，是可靠性边界

依据：0002 §7「本地幂等键和数据库唯一约束保证一个 Job 只建立一个 Attempt；Worker 提交前写入 `submitting` 并持续维护租约，通用重试器**不得重放 Provider POST**」。
因此：无论上游同步还是任务式，「创建请求绝不重发」是统一规则；任务式上游带来的差别只是「任务查询是幂等读操作，可在同一次执行内重试」——那是 ② 内部的事。

### 3.3 同一型号由两个 Provider 供应

按 [`docs/adr/0015`](../adr/0015-vendor-model-contract-and-offering-parameter-mapping.md)，**调用方合同是 Vendor Model 级的唯一一份**；③ 保留的是**该供给能承载的挂载面**（它对哪些参数、哪些值域、哪些分支可执行），差异只在**能力子集**上，不随渠道各自定义一份调用方合同。

据此，「两个 Provider 供同一型号」在目录里表现为：**两条 Offering，共同指向稳定身份，各自携带自己的 `provider_model_id`**。`native_model_id` 是**厂商的**型号身份，稳定、用于合同身份与执行固化（0001：「平台 `native_model_id` 与供应商调用所需 `provider_model_id` 分开固化」、ADR-0004），**不进对客面**——对客看到的那个名字是 Gateway Model（[`0006`](./0006-gateway-models-and-consumer-surface.md) §1.4）；`provider_model_id` 由 Offering 携带，**只有 Adapter 使用**。

**结构性差距 G5**：合同需要唯一载体、`capability_schema` 的双重身份要拆开，登记为工作项 [`#6`](https://github.com/dehuadong/seeaihub-server-next/issues/6) 的结构性差距。

## 4. 接入一个新 Provider 的清单与合法例外

规则的正文见[架构治理](../architecture.md) §6。逐项能指出落在哪一层的清单如下：

| 步骤 | 层 | 动作 | 是否改代码 |
| --- | --- | --- | --- |
| 1 | ② | 新增该 Provider 的 Adapter（路径、封装、解析、证据提取、错误分类、轮询/取图） | **是**（代码发版） |
| 2 | ③ | 抓取该 Provider 的机器 Schema → 差异检查 → 审核 → 发布 Profile 修订 | 否（运行时发布） |
| 3 | ④ | 为每个型号发布一个 Offering（Provider、上游模型名、用哪个 Adapter、渠道限制、优先级） | 否（运行时发布） |
| 4 | ⑤ | 发布该 Offering 的 Price Plan（计价单位、单价或金额口径、成本侧的渠道币种） | 否（PG 配置） |
| 5 | ① | **无需改动**（对外合同不变） | 否 |

三类合法例外的理由：

| 例外 | 内容 | 为什么合法 |
| --- | --- | --- |
| **E1 平台侧新能力** | 多 Offering 路由需要加列、换唯一索引、改选中入口的返回类型 | 这是平台自己的新能力（既有约束只允许一个生效供给），不是渠道差异泄漏 |
| **E2 上游事实带来的形态扩展** | 若某上游只给「扣费金额」而既有证据类型是具体 token 结构，则领域类型必须扩展 | 新事实需要新形态；但**形态本身应待实测后再定**，不得在事实之前预造 |
| **E3 装配点** | 多个 Adapter 需要按 `adapter_key` 派发的组合工厂 | 纯装配，不涉及领域与表结构 |

外部服务适配器（§1.1）不走这张清单：它不供应任何 Vendor Model，接入它等于平台自己新增一个外部依赖，端口与适配 crate 的落点各自评估。

## 5. 与既有工件的关系

- 本文只归纳，**不取代** `0001`（实现映射）与 `0002`（技术设计，v5 已评审通过）；冲突时以 `0002` 为准。
- 持久决策仍在 `docs/adr/`。本文引用的依据来自 `0001`、`0002`、`adr/0001`（统一 Command）、`adr/0003`（PG 事实权威）、`adr/0004`（Vendor/Provider 身份分离）、`adr/0006`（证据门槛）、`adr/0007`（不自动重提）、`adr/0019`（图片按渠道原形进原形出、不落盘静态资产，取代 `adr/0008`）。
- 分层规则的正文归 [`docs/architecture.md`](../architecture.md)，本文只保留依据与细则；冲突时以那份为准。落点不在两份文档里：表与写入方见 `crates/persistence` 的模块文档，端点见 `apps/api` 的路由定义。
