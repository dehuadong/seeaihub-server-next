---
status: accepted
---

# 同一 Vendor Model 可以有多个 active Offering：候选集与合格性由发布定义，选中归路由策略

同一 Vendor Model 可以由多个 Offering 供应（不同 Provider、Channel 或执行策略）。`publication.runtime_entries` 允许同一型号存在多条 active 记录，各带一个 `routing_priority`；**一次发布携带该型号完整有序的候选集合，发布即原子替换该型号既有 active 条目**，因此候选集永远来自同一个 Revision。

受理时先按该型号取候选集，再按**合取**判据筛出**合格候选**：该候选的 `restrictions` 允许本次分支与输入图张数，**且**请求满足该候选自己已发布的 Schema。两项都只用受理前已知的事实——不做上游探测，不让 Adapter 参与选路；没有合格候选时在**调用上游之前**失败。在一批**合格候选**里挑哪一条由**运营配置的路由策略**决定——未配置策略时是 `priority_failover`：按 `routing_priority` 数字小者优先、该档没有合格候选时依次降级，同一档内按 `weight` 分摊。策略层本身归 [ADR-0020](./0020-routing-strategy-layer-configured-by-operations.md)，权重语义见 [`docs/design/0008`](../design/0008-routing-strategy-and-caching.md) §2。

决定的操作性条款：

- **合格判据不因策略或优先级放宽**：策略的取值空间只有合格候选，全部候选都不合格时仍是"调用上游之前失败"。
- **价格不进合格判据**：不按最低价自动排序，也不在运行时拿 Price Plan 比价；"挑更便宜的"只能由运营策略显式配置（`least_cost` 消费运营录入的 `discount_rate`，与实际扣费不一致时以实际扣费为准，见 [ADR-0020](./0020-routing-strategy-layer-configured-by-operations.md)）。发布者若希望优先用更便宜的供给，就把这件事表达为发布时的顺序并记录依据。
- **预授权只是保底额**：按**供给（vendor + offering）维度**的保底表（`floor_amounts`）查得，随修订发布、随 Job 快照冻结、**不编进代码**，**既不由候选价格派生、也不由调用方自报**；**受理闸门是"余额 ≥ 保底额"**（不成立 ⇒ `insufficient_balance`、对客 402，不产生 Job、不扣款）；**结算按实际扣费**，实际超过保底额时余额可为负——透支发生在结算，不在受理。`GENERATION_MAX_COST_MICROUSD` 退为"连该供给的封顶保底值都查不到时"的兜底保底额，不是任何形式的上限。保底表的形状与字段见 [`docs/design/0007`](../design/0007-pricing-floor-and-settlement.md) §6。
- **对客闸门只有客户余额**：不因为"售价高过某个服务端固定数"拒绝请求。平台自己的**单次请求成本上限**（`GENERATION_MAX_REQUEST_COST_MICROUSD`）是运营护栏，判的是**发布物与折算率**给出的单次成本上界，与客户余额无关；超限对客是 **503 平台侧故障**、不是 402（机制见 [`docs/design/0009`](../design/0009-operational-baseline.md) §7）。
- **一次 Attempt 一旦进入 `submitting`，就禁止改选 Offering 或 Channel**——上游可能已生成并计费，改选等于重复出图与重复计费。
- **受理时必须写一条 append-only 的路由判定记录**（候选列表、被选中者、每个被跳过者的原因）。`active` 是按型号全局可变的事实，只靠 revision id 或模型名事后无法重建"当时考虑过谁、为什么跳过"。
- **限制只能收窄，但不需要"收窄证明"机制**：Profile 声明该 Provider 的实际支持面，Offering 的 `restrictions` 只在其内再收紧——同一事实在两个发布位置的落点，不是从全集推导子集。

**修订史**：本条被取代的结论、失效原因与依据提交见 [`.agents/notes/implemented/platform/2026-09-27-adr-0009-revision-history.md`](../../.agents/notes/implemented/platform/2026-09-27-adr-0009-revision-history.md)。
