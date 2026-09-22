---
title: 路由策略层：选哪条合格候选由运营配置
status: implemented
created: 2026-09-22
updated: 2026-09-22
approval: 用户 2026-09-22 授权继续执行提案 #13 的剩余切片；策略层的决策已落 ADR-0020
verification: 用例 weighted_random_ignores_tiers_and_replays_to_the_same_candidate、least_cost_compares_discounted_estimates、user_tag_takes_the_mapped_candidate_and_falls_back_when_it_cannot_carry（应用层）与 route_policies_are_runtime_configuration_and_do_not_touch_revisions、route_policy_inputs_round_trip_through_the_admin_api（端到端）
---

# Agent Note：路由策略层：选哪条合格候选由运营配置

## 问题

选路此前只有"顺序"一个旋钮：按 `routing_priority` 数字小者优先，同档内按 `weight` 分摊。运营要按成本挑、要按客户标签指定渠道、要在两条渠道之间按比例分流，都得改代码或重发修订。用户确认"路由这层是**运营的事**"，所以平台提供的是**机制**：策略由运营在后台配，平台不预设任何"哪家优先"的结论。

## 决定

策略表 `routing.route_policies`：**全局一条 + 可按网关模型覆盖**（`gateway_model` 为空即全局、按模型取"有覆盖用覆盖、没有用全局"），唯一索引建在 `coalesce(gateway_model, '')` 上——一个作用域只能有一行，否则"哪条生效"取决于行序。策略**不进不可变修订**：它是运行期配置，写入成功即刻影响之后的受理；已受理的 Job 候选早已固定在快照里。

四种策略：`priority_failover`（默认，按档位顺序、档内按权重）、`weighted_random`（不看档位，在全部合格候选里按权重）、`least_cost`（按**折后成本估算**最小：候选的参考成本 × 它配的折扣率，没配就是不打折，没有参考成本的排最后）、`user_tag`（账户标签经 `tag_channel_map` 指定的候选）。选路仍先算合格集合：承载面表达不了、分支或张数不被允许的候选先被排除，策略的取值空间只有合格候选，**指定不了不合格的那种**。按权重分摊的两种沿用同一个哈希输入 `(account_id, idempotency_key)` 与同一套按 `offering_id` 升序的区间划分，不引入随机数发生器。

`least_cost` 与 `user_tag` 给不出答案时**退回默认顺序**（前者是谁都没有成本估算，后者是标签没配映射、或映射指向的候选这次不合格），不判失败：别的候选明明能承载这次请求，把它们一起判掉没有好处；退回的顺序是确定的，仍然满足可重放。

管理面：`GET` / `PUT /api/v1/route-policies`（`strategy` 只接受本层已实现的取值，其余 400——落成默认会把"配置没生效"伪装成生效），账户标签走 `PUT /api/v1/accounts/{account_id}/tag`（不动 `updated_at`：那一列是余额最后一次变动的时刻）。每次写入换新的版本标识。

**零配置时的行为由构造保证**：受理路径在一条策略都没有时走原来的选路函数，而不是靠某个默认参数"应该等价"。

## 备选方案

**把策略做进不可变修订（每次改策略发一份新修订）。** 落选：策略不是"卖什么"，把它塞进修订会让"改一次分流比"变成"重发一次商品定义"，还会让已受理 Job 的固定版本跟着抖动。运行期配置有自己的生命周期，与发布面分开才说得清。

**一次把四种策略都做完。** 落选：`least_cost` 与 `user_tag` 各自要新的输入面（折扣率表、账户标签），混在一起会让同一步同时承担数据模型改动。分成两批之后，"机制成立"与"新的输入面"各自有独立验收。

**未实现的策略先接受、按默认策略跑。** 落选：那会让运营以为配置生效了，而实际走的是别的策略——选路出错是最难从现象反推原因的一类。

**`least_cost` / `user_tag` 给不出答案时报平台侧故障。** 落选：没有成本估算不是"平台承载不了这次请求"，标签没配映射也只是运营没把配置做完；这两种情况都还有合格候选可用，判失败等于把一次能成的请求拒掉。退回默认顺序是确定的，也不会让"配置没配好"变成一次静默的错选。

## 后果

策略读取目前**直查数据库**（每次受理多一次查询），与 route 缓存同构的版本比对留给后续：正确性不依赖缓存，代价只是一次查询。改策略立即影响之后的受理；已受理 Job 因为候选已冻结而不受影响。

`discount_rates` 与 `tag_channel_map` 只在对应策略生效时被消费：改它们（或给账户改标签）在别的策略下不改变任何选路结果——这一点让"改了配置却没变化"有正当解释。

## 验证

应用层用例：`weighted_random_ignores_tiers_and_replays_to_the_same_candidate`（跨档 + 重放同一条 + 不得选中不合格）、`least_cost_compares_discounted_estimates`（折扣把贵的那条变便宜时它赢，区分"折后估算"与"原始成本"）、`user_tag_takes_the_mapped_candidate_and_falls_back_when_it_cannot_carry`（映射命中 + 映射指向不存在时退回默认顺序）。

端到端用例：`route_policies_are_runtime_configuration_and_do_not_touch_revisions`（管理员读写、未实现策略 400、无凭证 401、**写策略不产生新修订**）、`route_policy_inputs_round_trip_through_the_admin_api`（账户标签落库与 401/404、两种策略的输入表原样读回）。
