---
status: accepted
---

# 路由策略层：候选合格性优先，选哪一条由运营配置

**决定**：在一批**合格候选**里"挑哪一条"由**运营配置的路由策略**决定。策略存**运行期可改的表**（`route_policies`），**不进不可变 Runtime Revision**——发布物定的是"有哪些候选、按什么顺序"（[0009](./0009-multiple-active-offerings-and-routing.md)），策略定的是"在合格候选里怎么挑"，两者不是同一个量。作用域**全局一条 + 可按网关模型覆盖**（按模型取"有覆盖用覆盖、没有用全局"）；**未配置时默认 `priority_failover`**——按 `routing_priority` 数字小者优先、该档没有合格候选时依次降级、同一档内按 `weight` 分摊，即**今天的行为**。因此零配置下选路逐位相同：策略层是"加旋钮"，不是"换引擎"。

**策略只是"选一条合格候选"，候选合格性优先于策略**：承载面表达不了这次请求、分支/张数不被该候选允许的候选**先被排除**（合格是合取判据，见 [0009](./0009-multiple-active-offerings-and-routing.md)），**任何策略都不得选中不合格候选**——策略的取值空间只有合格候选，没有"策略指定了就绕过承载校验"这回事；全部候选都不合格时仍是 [0009](./0009-multiple-active-offerings-and-routing.md) 的结论（调用上游之前失败），策略不改这个结果。策略**不改写参数映射与承载面**（调用方合同与承载面归 [0015](./0015-vendor-model-contract-and-offering-parameter-mapping.md) 的 Offering Parameter Mapping），也不改请求参数、对客价格与成本口径。

**策略的输入**：`priority`（数字小者优先）、`weight`（加权随机）、`discount_rate`（**只作 `least_cost` 的比较输入，不进成本**）、**账户标签**（`user_tag`，为此账户对象要加标签字段）。四个量都只是**输入**、单独不生效——只有某条生效的策略在消费它时，改它才改变选路。**成本永远记实际扣费**（`Declared` 取上游 `cost`、`Computed` 按实际 `usage` 分项 token × 费率，见 [0006](./0006-no-settlement-without-metering-evidence.md)）；`discount_rate` 只是 `least_cost` 用的**估算**，**与实际扣费不一致时以实际扣费为准**——它不进账本、不改成本事实、不改对客金额。

**分流确定性可重放**：`weighted_random` 按 **`(账户, 幂等键)`** 哈希取落点，**同一请求重放必落同一条候选**——不引入随机数发生器，选路结果照旧写判定记录，事后可重建（[0009](./0009-multiple-active-offerings-and-routing.md) 要求判定可重建）。

**与 [0009](./0009-multiple-active-offerings-and-routing.md) 的关系**：它定的"哪些候选存在、按什么顺序"是**发布内容**，**继续成立**；本条补上它没定的那一半——在一批合格候选里怎么挑。因此 [0009](./0009-multiple-active-offerings-and-routing.md) 标为**部分被取代**（保留原文与结论段，只加标注）。[0015](./0015-vendor-model-contract-and-offering-parameter-mapping.md) 的"核心服务不内置择优"同样**继续成立**：策略是**运营配置**（运行期可改、不进修订），不是核心服务里硬编码的价格/优先级/健康度规则。

**影响**：账户要加标签字段（**机制**要做；标签值由运营设，不属本决策）；策略可**热改**、改完即时生效、**不需要发修订**；**已受理 Job 不受后续改策略影响**（受理时选定的候选随 Job 固定，见 [0003](./0003-postgresql-is-source-of-truth.md) 的"受理时固定版本"）；**route 缓存要带策略版本校验**——缓存值与当前策略版本比对，不一致（或值里没有这个标识）即当未命中、回源 DB，使策略改动的陈旧可检。

**批准依据**：用户 2026-09-20 在讨论中确认本条定案并要求记录（原话「现在不是在定了吗？」）。落地机制（`route_policies` 的形状、作用域取值、各策略的取值空间、缓存与生效口径）见 `docs/design/0006-gateway-models-pricing-and-admin-console.md` §5，本条只拥有决策正文。
