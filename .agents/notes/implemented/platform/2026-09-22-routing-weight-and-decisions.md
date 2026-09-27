---
title: 权重与路由日志：档位内确定性分流与判定记录可重建
status: implemented
created: 2026-09-22
updated: 2026-09-22
approval: 用户在会话中授权实施 P3（权重与路由日志）；范围与验收见提案 [#13](https://github.com/dehuadong/seeaihub-server-next/issues/13) 的 P3 工单 [#18](https://github.com/dehuadong/seeaihub-server-next/issues/18)
verification: 验收合同为工单 [#18](https://github.com/dehuadong/seeaihub-server-next/issues/18) 的逐条可勾选验收清单（依据 `docs/design/0008` §1–§5）。**全部离线**（真实空库 + 真实 API 进程 + 本机假上游，零真实计费调用、零外网）。门禁：`cargo fmt --all -- --check` exit 0；`cargo clippy --workspace --all-targets --all-features -- -D warnings` exit 0；`cargo test --workspace --all-features` 全绿（22 / 32 / 2 / 3 / 67 / 59 各 crate 单测通过，58 条端到端按设计 ignore）；`cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1` 在**最终代码**上 **58 passed / 0 failed**（162.81s，含本片新增的 3 条）；`node scripts/decisions/check.mjs` 通过。分流期望值在用例里按设计规则（`sha256(账户 ‖ 幂等键)` 前 8 字节大端、对档内权重之和取模、按 `offering_id` 升序走区间）**独立重算**，不复用生产实现；并发发布那条另做**变异实测**（把发布事务里的按名字咨询锁换成 `SELECT 1` → 该用例实测失败：同一名字有 **4** 份修订的 active 条目并存；恢复后 1 份），证明该用例有检出能力。逐条证据见正文「验证」一节。
---

# Agent Note：权重与路由日志：档位内确定性分流与判定记录可重建

## 问题

同一型号的候选此前只有"顺序"一个旋钮：`routing_priority` 就是候选数组的下标，受理时按它升序取**第一个合格候选**，后面的候选只在前面不合格时才轮到。**同一档里按比例分流做不到**，而且库层的唯一索引 `(native_model_id, routing_priority) WHERE active` 直接把"同档多候选"挡在门外。

本片补的是"档位"与"分流比"的分工，范围与逐条验收见工单 [#18](https://github.com/dehuadong/seeaihub-server-next/issues/18)：

- **档位是顺序，权重只在档内分流**：受理时先按 `routing_priority` 升序找到**第一个至少有一条合格候选的档**，再在该档的合格候选里按 `weight` 分摊；
- **分流确定性可重放**：分摊用 `(account_id ‖ idempotency_key)` 的哈希落点，不引入随机数发生器——同一请求重放必然落同一条候选；
- **判定记录可重建**：`generation.routing_decisions.considered` 每条带上 `weight` 与本次的分流落点 `weight_draw`，事后能重建"考虑过谁、为什么跳过、为什么是它"；
- **零配置下行为不变**：不带 `weight` 时默认 `1`，不带显式档位时仍等于数组下标。

## 决定

- **领域层**：`OfferingCandidate` 增 `weight: u32`（正整数、随候选发布、默认 1；反序列化缺省也是 1——早于这个字段落库的发布快照里没有它）。它是**发布数据**，与 `routing_priority` 同处；只在选路用，选中后固化进 Job 的 `PublishedOffering` 不带它（与 `routing_priority` 一样，只有发布侧需要）。
- **应用层**：
  - 候选形状增两个**可缺省**字段：`weight`（缺省 1，显式 0 拒绝）与 `routing_priority`（缺省 = 数组下标，显式给值让**多条候选落在同一档**）；
  - `select_candidate` 改成"先定档位、再在档内分摊"。**合格性先算出来**：承载面表达不了、分支/张数不允许的候选不进权重之和，也不在区间里——这是"候选合格性优先于策略"这条硬约束在本层的落点；
  - **区间划分按 `offering_id` 升序**，不按数据库返回的行序：落点是哈希出来的一个数，行序不稳定时同一请求换个取数顺序就会分到另一条候选；
  - 分摊只决定"选中谁"，不改写参数映射、承载面、请求参数、对客价格与成本口径（冻结与结算路径一字未动）。
- **持久层**：`runtime_entries` 写入并读出 `weight`；发布快照的候选条目带上 `weight`；受理取数改为 `ORDER BY routing_priority ASC, o.id ASC`（同档定序）；管理员投影的每个候选带 `weight`；**同一网关模型名的发布串行化**——发布事务开头按名字取一把事务级咨询锁。
- **① 层**：**不改**——发布命令的两个新字段由既有 `Json<PublishRuntimeCommand>` 反序列化接住，没有新路由。

### 迁移

增量迁移 `migrations/0010_routing_weight_and_decisions.sql`：

- `publication.runtime_entries` 增 `weight integer NOT NULL DEFAULT 1` + `CHECK (weight > 0)`。默认 1 让既有行与老素材自动落在"权重 1"，行为不变；
- 唯一索引 `one_active_entry_per_model_and_priority`（`(native_model_id, routing_priority) WHERE active`；`0004` 把该列改名为 `gateway_model`，索引定义跟着改名后的列）**换成** `one_active_entry_per_model_and_offering`（`(gateway_model, offering_id) WHERE active`）：旧索引等价于"同一档只能有一条候选"，与档内分流直接冲突。换过之后索引只防"同一个网关模型下同一条供给出现两行"；
- 判定记录的新字段落在 `considered` jsonb 里，**不加列**。

### 换索引后补的那一处守卫（评审发现，超出工单原文）

设计 §2 把"同一个名字的生效条目来自同一次发布"交给"发布事务的原子替换 + 跨修订防御"共同保证。**换索引之前，这条性质其实由旧索引顺带兜着**：两次并发发布同一名字时，旧索引会直接拒掉后一次（两次发布用的是同一批 `routing_priority`）。换成 `(gateway_model, offering_id)` 之后这条兜底没有了——每次发布都给候选新建供给行，索引上永远不撞——于是两条并发发布的"先失效、后插入"会交错：两边都看不到对方的 active 行，各自插入自己的，**两份修订的 active 条目同时存在**。而"active 候选跨修订并存"是**读时**才暴露的（仓库层对它的报错发生在受理取数时）：表现是这个型号的所有请求一起失败（平台侧故障），直到有人重新发布一次。

因此发布事务开头加了一把按名字取的事务级咨询锁（`pg_advisory_xact_lock(hashtextextended($1, 0))`，与 `create_job` 里那把幂等锁同一种写法；随事务结束自动释放），让"替换"真的是一次替换；读时的跨修订防御**照旧留着**兜底。这条是评审阶段发现的，工单 [#18](https://github.com/dehuadong/seeaihub-server-next/issues/18) 的改动点清单与迁移说明已同步，并新增了一条端到端验收。

## 备选方案

- **哈希输入取 `(账户, 幂等键)` vs 取 JobId**：取前者。选路发生在 JobId 生成**之前**，拿一个当时还不存在的值当哈希输入是因果倒置；而这两个值受理前就已知。账户也进哈希：幂等键只在自己账户内唯一，不同账户用同一个键时不该相关。账户是定宽 UUID，直接拼在幂等键前面即可，不需要分隔符。
- **档内区间按 `offering_id` 定序 vs 按数据库行序**：按 `offering_id`。落点是哈希出来的一个数，区间划分若依赖行序，"可重放"就成了空话。定序键必须是与请求无关的发布数据。
- **档内权重之和用 `u64` 累加 vs `u32`**：`u64`。权重本身是 `u32`，多条候选相加可能溢出。
- **权重 0 允许 vs 发布期拒绝**：拒绝。0 不是"不参与分流"的表达——想不参与就不发这条候选；放行 0 之后那条候选会永远分不到，而"为什么分不到"要读一遍分摊代码才知道，等于把配置错误伪装成运行结果。
- **不合格候选"权重很大"时能否被选中 vs 一律排除**：一律排除。权重只在合格集合内部起作用，这是三条硬约束里第一条的落点；否则"权重"就成了绕过承载校验的后门。
- **判定记录只记落点 vs 每条候选都带上落点**：每条都带。落点是本次判定一个数（逐项同值），放在每条上是为了让每一条都能独立读出"当时是怎么分摊的"，不必先知道哪一条被选中。账户与幂等键本身随 Job 落库，不重复记。
- **并发发布加锁 vs 只留读时防御**：加锁。只留读时防御时，交错一旦发生就是"这个型号全挂"，而运营看到的现象是"请求全失败"、不是"有一次发布没生效"——排查成本远高于一把只在发布期取的锁。锁的代价是同一个名字的发布串行（发布是低频的管理动作），不同名字互不影响。

## 字段面的最小补齐（设计原文没写这一处，已同步）

设计 `docs/design/0008` §2 要求"同一档位内可以有多个候选，档内按 `weight` 分流"并据此把唯一索引换成 `(gateway_model, offering_id) WHERE active`——**旧索引按 `(native_model_id, routing_priority)` 唯一，而 `routing_priority` 又一直等于数组下标；下标天然互不相同，因此那个索引从来不会冲突**。这条论证本身说明设计预设了"发布者能把两条候选放进同一档"，只是**没有写这个字段**。

处理方式（**不新增决策，只把设计已经预设的那件事写出来**）：

- 候选上增一个**可缺省**的 `routing_priority`，**缺省仍等于数组下标**——零配置行为逐位不变，老素材与老测试不受影响；显式给值才能同档；
- `docs/design/0008` §1/§2 已在**同一变更**里记明这一处（§1 补"候选可以显式给出 `routing_priority`"、§2 补"档位怎么表达"与"档内定序"），`docs/design/0006` §1.5 同步了新索引名；
- 工单 [#18](https://github.com/dehuadong/seeaihub-server-next/issues/18) 的「需裁决」一节保留了这一处的说明，供用户确认。**若另有打算（例如不允许多候选同档、只留 P6 的 `weighted_random` 用权重），改这一处即可，其余不受影响。**

## 后果

- **同档多候选的发布形状是设计没写明的字段面**：见上「字段面的最小补齐」——按最小口径落地并已同步设计，**仍待用户确认**。
- **`weight` 目前只在默认策略（`priority_failover`）下被消费**：`weighted_random` / `least_cost` / `user_tag` 与 `route_policies` 属 P6；本片只落"档位 + 档内分流"这一层，策略层接的就是这里算出来的合格集合。
- **运行期回退仍然不做**：本片只保留受理前的候选不合格回退（全不合格 ⇒ 503 `platform_unavailable`）。上游请求发出之后改道属工单 [#11](https://github.com/dehuadong/seeaihub-server-next/issues/11)。
- **哈希算法一旦改动就是行为改动**：落点由 `sha256(账户 ‖ 幂等键)` 决定，换哈希等于换分流结果。端到端与单元用例都按同一条规则独立重算期望，改动会被挡下。
- **并发发布的守卫是"发布期串行"而不是"库层不可能"**：锁只在发布事务内生效，绕过这个入口直写 SQL 仍能造出跨修订并存（那时由读时的跨修订防御报错兜住）。这属"绕过发布入口"的情形，与既有边界一致。
- **`weight` 的"≥1"在应用层、读回路径与库层各判一次**：库约束别人也能绕过（直写 SQL），而权重为 0 的候选会让分摊区间少一段、表现为"某些请求分不到任何候选"——那种故障从结果上看不出来，所以在读回时也拒一次。`routing_priority` 的负值只在发布期判：负数只是一个更小的档位，不破坏任何不变量。

## 验证

验收按工单 [#18](https://github.com/dehuadong/seeaihub-server-next/issues/18) 的逐条可勾选清单，在**空库 + 真实 API 进程 + 本机假上游**上跑（离线、零计费调用）。分流相关的期望值在用例里按设计规则**独立重算**（不复用生产实现的辅助函数）；下表给出承担每条验收的用例。

| 验收（工单 [#18](https://github.com/dehuadong/seeaihub-server-next/issues/18)） | 证据 |
| --- | --- |
| 同档按权重分流、比例落在确定区间 | `routing_weight_splits_within_a_tier_and_is_replayable`（同一档权重 1:3，16 个固定幂等键逐条断言"选中项 = 按 `(账户, 幂等键)` 与权重重算的结果"；固定账户 ⇒ 结果确定，不是概率断言） |
| 同一批输入可复现 | `weight_splits_within_one_tier_deterministically`（`crates/application`：固定账户 + 固定幂等键跑两遍，64 条结果逐条相同；两条候选都被分到过） |
| 同一幂等键重放 → 同一条候选、去重成原 Job | 同一条端到端用例（重放后选中项不变、Job 总数不变、判定记录仍只有一条） |
| 跨档时权重不改变档位顺序 | 同一条端到端用例（档 0 权重 1、档 1 权重 1000 → 4 次请求全落档 0）+ `weight_never_outranks_a_tier_that_has_an_eligible_candidate`（`crates/application`） |
| 不合格候选不进分摊 | 同一条端到端用例（同一档：不合格权重 1000 + 合格权重 1 → 每次都落合格那条，落选原因写明分支限制）+ `an_ineligible_candidate_never_wins_the_split`（`crates/application`） |
| 全部档都不合格 → 503（不是 400），不建 Job、不写判定记录 | 同一条端到端用例（两条候选都收窄成只允许文生图 → 带图请求得 503 `platform_unavailable`，该型号 Job 与判定记录都为 0；不启动 Worker ⇒ 零上游调用） |
| 判定记录能重建"考虑过谁、为什么跳过、为什么是它" | 同一条端到端用例（`weight_draw` 等于独立重算的落点、逐项同值；用记录里的档位/权重/落点走区间复现选中项）+ `rebuild_from_decision_record`（`crates/application`） |
| 默认权重与默认档位不改行为 | `explicit_priority_and_weight_are_normalized_for_shared_tiers`（不给时档位 = 下标 0/1、权重 = 1）+ 既有 `multiple_active_offerings_route_by_priority` 等 58 条端到端全绿 |
| 权重 0 / 负档位在发布期被拒 | 同一条端到端用例（两次发布各得 400）+ `a_zero_weight_or_negative_priority_is_rejected`（`crates/application`）+ `the_routing_weight_migration_keeps_existing_entries_and_allows_shared_tiers`（库层 `CHECK` 也拒 0） |
| 并发发布不交错 | `concurrent_publications_of_one_gateway_model_leave_a_single_active_revision`（6 次并发发布同一名字全部成功；active 条目仍只来自 1 份修订；随后受理照常超时而非平台侧故障）。**变异实测**：把锁换成 `SELECT 1` → 同一断言实测得到 **4** 份修订并存（用例有检出能力），恢复后为 1 |
| 管理员读接口列出权重 | 同一条端到端用例（`GET /api/v1/gateway-models` 的候选集合带 `weight` 1 与 3） |
| 迁移增量路径 | `the_routing_weight_migration_keeps_existing_entries_and_allows_shared_tiers`（只应用 `0001`–`0009` 建库并造旧行 → 补 `0010`：旧条目逐字不变、权重取默认 1；旧索引消失、新索引建起；同一档第二条候选可落；`weight = 0` 与"同供给两行 active"被库层拒） |
| 历史行为不变 | 既有 55 条端到端用例在改动后仍逐位通过（整跑 58/58） |
| 门禁 | `cargo fmt --all -- --check` exit 0；`cargo clippy --workspace --all-targets --all-features -- -D warnings` exit 0；`cargo test --workspace --all-features` 全绿（22 / 32 / 2 / 3 / 67 / 59，58 条端到端按设计 ignore）；`cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1` 在最终代码上 **58 passed / 0 failed**（162.81s）；`node scripts/decisions/check.mjs` 通过 |

### 验证的边界（如实记录）

- 本轮**没有另起独立驱动**（不同于定价那一片的做法）：本片验收条目全部落在既有端到端边界（对客 HTTP + 直查库）上，而分流这类纯函数的期望值在用例里已按设计规则独立重算，另起驱动只会重复同一批断言。若后续阶段要求独立复跑，用例名与断言口径已在上表列全。
- 并发那条验收的**检出能力**是用变异实测证明的（去掉锁 → 失败），不是靠"跑一次过了"推断的；但它在 CI 上是否总能复现"不加锁就失败"取决于并发交错，**加锁后的通过是确定的**（串行化之后不存在交错窗口）。

## 依据与关联

- 设计：排序现状与权重语义、档位内分流、回退链与全不合格、按阶段判定回退、判定记录的落点见 [`docs/design/0008`](../../../../docs/design/0008-routing-strategy-and-caching.md) §1–§5；管理员投影见 [`docs/design/0006`](../../../../docs/design/0006-gateway-models-and-consumer-surface.md) §2.2。
- 决策：候选集来自同一次发布、合格是合取判据、`submitting` 后禁止改选见 [`ADR-0009`](../../../../docs/adr/0009-multiple-active-offerings-and-routing.md)；路由策略层由运营配置、候选合格性优先于策略、分流确定性可重放见 [`ADR-0020`](../../../../docs/adr/0020-routing-strategy-layer-configured-by-operations.md)。"同一档位内可以有多个候选"取代了 `ADR-0009` 原先的"同一型号内唯一"，该处修订史见 [`ADR-0009 的修订史`](./2026-09-27-adr-0009-revision-history.md)。
- 迁移：[`0010_routing_weight_and_decisions.sql`](../../../../migrations/0010_routing_weight_and_decisions.sql)。
- 索引同步：[`docs/architecture.md`](../../../../docs/architecture.md) §3（④ Offering）、§4（一次请求怎么走）、§5（表）、§6（文件与迁移）；词汇表 [`CONTEXT.md`](../../../../CONTEXT.md) 新增 `Routing Weight`（档内权重）、改写 `Routing Priority`。
- 相邻记录：[定价、保底与结算](../../implemented/platform/2026-09-22-pricing-floor-and-settlement.md)（选路改动不碰它的冻结与结算路径）、[第二阶段交付：多 Offering 路由与 APIMart Driver](../../implemented/platform/2026-09-19-multi-offering-routing-and-apimart-driver.md)（本片把"按优先级取第一个合格候选"改成"定档位 + 档内分摊"）。
- 工作项：提案 [#13](https://github.com/dehuadong/seeaihub-server-next/issues/13) 的 P3 工单 [#18](https://github.com/dehuadong/seeaihub-server-next/issues/18)。
