# 同一 Vendor Model 可以有多个 active Offering，选择由已发布优先级与受理前限制决定

> **状态：未生效。** 本文随工作项 [seeaihub-server-next#2](https://github.com/dehuadong/seeaihub-server-next/issues/2) 的第二阶段规划起草；**在该规划通过 Plan Review 之前，本文不是实现依据**，也不得被下游工件当作已接受的决策引用。规划正文见 #2 的规划评论（以该 issue 上标注为「当前唯一可执行版本」的那条为准，不在此写死版本号）。

同一 Vendor Model 可以由多个 Offering 供应（不同 Provider、不同 Channel、或同一 Provider 的不同执行策略与价格）。平台不再限制「一个模型只能有一个生效供给」：`publication.runtime_entries` 允许同一 `native_model_id` 存在多条 active 记录，每条带一个 `routing_priority`，数字小者优先；同一模型下 active 候选的优先级必须唯一。

**选择规则**：受理时按优先级升序遍历候选，取**第一个合格候选**。候选合格是**合取**，两项都要成立：

1. 该候选的 `restrictions` 允许该请求（分支与输入图张数维度）；
2. 该请求满足该候选自己的已发布 `capability_schema`（原生参数取值维度）。

两项都只用受理前已知的事实，**不做任何上游探测**，也不让 Adapter 参与选路。请求没有任何合格候选时，Job 在**调用上游之前**失败；不回退到「能力更宽但优先级更低」的候选。**「试候选」只允许发生在 Job 建立之前**——一旦 Job 建立，选中的候选即固化。

**价格不参与选中**。选中完全由 `routing_priority` 与 restrictions 决定，不按最低价自动排序，也不在运行时比较 Price Plan。理由有两条：自动按价选会在**上游改价时静默改变被选中的供给**，而请求与 Job 固定的是受理时的版本，选择必须由发布决定而不是由随时可变的价格数据决定；其次候选之间往往不可相互替代（能力与限制不同），自动按价排序会把「更便宜但能力更窄」的候选推到前面。发布者若希望优先用更便宜的供给，就把这件事表达为**发布时的优先级顺序**，并在发布理由中记录依据——选择因此始终可审、可复现。

**预授权金额与计价口径是两个不同的量，不得互相推导**：预授权金额由调用方给出的授权上限 `max_cost_microusd` 决定，平台**不**根据候选价格反算预授权——因为按张计价时输出张数在受理时不可知（Provider 可能静默忽略 `n`），而 [0006](./0006-no-settlement-without-metering-evidence.md) 禁止用请求参数反推计量。被选中候选的 Price Snapshot 固化进 Job，是**结算口径**（实际费用按它计算）。两者不一致时（例如被选中候选的最小可能费用已超过授权上限）应在**受理前拒绝**，而不是等到结算才对账。将来若要引入价格参与选中，必须新增发布期字段并明确它与优先级的优先关系，不能让它隐式生效。

**一次 Attempt 只对一个 Offering 与 Channel 生效**。一旦 Attempt 进入 `submitting`，就禁止改选 Offering 或 Channel——上游可能已经生成并计费，改选等于重复出图与重复计费。该约束无条件成立，与错误分类无关。这与 [0007](./0007-reconciliation-instead-of-automatic-retry.md) 一致，区别是：在只有单一供给时该约束自动成立，多供给后它必须被显式实现和测试。

**限制只能收窄，但不需要「收窄证明」机制**。按 [0004](../design/0004-layered-architecture.md) 的分层：

- **Model Profile（③，运行时发布）**声明该 Provider 供应该型号的**实际支持面**（参数、值域、默认值、组合规则）——由抓取该 Provider 的机器 Schema 导入形成，粒度是 **(Vendor Model Revision × Provider)**；
- **Offering（④，运行时发布）**的 `restrictions` 只在该范围内**再收紧**（例如某 Offering 只开放 1 张图、只开放部分分支）。

二者是**同一事实在两个发布位置的落点**，不是「从全集推导子集」的证明关系。因此**不引入**"能力面 ∩ 收窄声明 → 合成有效 Schema"、"参数白名单唯一封锁来源"、"禁止正则类参数收窄" 这类机制——那会把一个数据发布问题做成形式化证明问题。发布期仍需校验「Offering 的 `restrictions` 不超出其 Profile 声明的范围」与「不超出 Adapter Descriptor 支持的能力」（后者由既有 `validate_adapter_compatibility` 覆盖），但这是**范围包含检查**，不是取值集合的推导。

**Provider 的支持面差异写在各自的 Profile 里**：同一型号由两个 Provider 供应时，是**两条 Offering 指向同一个稳定身份**，各自携带自己的 `provider_model_id`（[0004](../design/0004-layered-architecture.md) §3.3），差异（某参数有无、值域不同、上游生命周期不同）如实记录在各自的 Profile 与 Driver 能力上。上游的同步/异步与响应形态属 Adapter Driver 的内部实现（[0001](../design/0001-image-generation.md)「同步/异步差异封装在 Adapter」），**不进入本 ADR 的选择规则，也不产生领域类型分支**。

**一次发布携带该模型完整的候选集合**：发布以一个 Runtime Revision 为单位，`publication.runtime_entries` 为该模型写入**有序的、完整的**候选集合，并沿用既有的「发布即原子替换该模型既有 active 条目」语义（`UPDATE ... SET active = false WHERE native_model_id = $1`）。因此同一模型的 active 候选集**永远来自同一个 Revision**，不存在跨 Revision 的候选并存，也不会出现半套候选集。**优先级来自发布顺序**（候选数组下标），只有一个来源。

**选路必须可复现，因此判定要落一条独立记录**：Job 固化的 `runtime_revision_id` 指向的是**被选中供给所属的那次发布**，而 `active` 是**按 `native_model_id` 全局可变**的事实——新发布会把旧候选全部置为 inactive。所以事后无论是按 revision id 还是按模型名反查，都**无法再重建「受理那一刻还考虑过谁、为什么跳过」**。为此每次受理除固化选中 Offering 外，还要在同一事务写入一条 append-only 的路由判定记录，包含当时的候选列表、被选中者与每个被跳过候选的跳过原因。

**代价**：路由成为平台自己的责任，必须可观测、可复现；「谁被选中」不再能只从 Offering 表读出来；每次发布必须提交该模型的**完整**候选集合（不能只增量发布一个供给），并且发布是模型级的原子替换——新增一个供给会一并重写该模型的整个候选集合。收益是候选集合与顺序始终与某一个不可变 Revision 对应，选路输入因此可审计、可复现。

**来源**：第二阶段 Planning（`dehuadong/seeaihub-server-next#2`）。多供给路由与「Provider 限制只能收窄 Vendor Model 能力」由该工作项要求；受理不确定时禁止改选由 [0007](./0007-reconciliation-instead-of-automatic-retry.md) 与仓库根 `AGENTS.md` 要求。
