# Provider 声明的扣费金额能否作为计量证据（**待实测结清后再决定**）

> **状态：未生效，未决。** 第二阶段规划已明确：**APIMart 的响应到底返回分项 token 还是只返回金额，是 Implementation Gate 之前必须完成的受控验证**（见工作项 #2 的规划 §5.1）。在实测结清之前，本文件**不产生任何生效规则**，也**不预先设计**金额型证据的领域类型——因为若 APIMart 实际返回分项 token，本条整个作废；若只返回金额，则按**最小改动**另行修订（可能只需扩 Adapter 的提取结果与 Price Plan 的口径，而不需要动领域模型）。
>
> 先前的表述曾把它写成「第二类计量证据」并给出领域联合类型与 `PriceFormula::UpstreamCharge` 分支。那是**在事实之前先造机制**，已撤回。

## 问题（为什么需要一条决定）

APIMart 的任务查询响应（首方文档示例）只给 `cost`（USD）与 `credits_cost`，**`usage` 在该页零出现**；而它同时声明「按实际 token 用量计费」。文档自相矛盾，只读无法结清。若它确实不返回分项用法，则该 Offering **只有金额这一个可核验事实**，此时：

- [0006](./0006-no-settlement-without-metering-evidence.md) 的门槛是「没有可核验 Metering Evidence 不得结算」——金额算不算可核验计量事实，需要明确；
- [0010](./0010-metering-evidence-is-unit-bearing.md) 规定证据是**带单位的计量量**，并且**金额不能替代计量量、不能由金额反推计量量**。

## 实测的三种结果与各自处置

| 实测结果 | 处置 |
| --- | --- |
| 含分项 token `usage` | 走既有 token 路径，**领域零改动**；本文件作废 |
| 只有金额 | 需要一条「金额可作为该 Offering 的结算事实」的决定。**最小改动**：Adapter 提取金额、Price Plan 声明「按上游声明金额计价」。**是否需要在领域层新增形态，届时按真实约束再定，不预先设计** |
| 两者都没有 | 该 Offering **不得以正式计费状态发布**（[0006](./0006-no-settlement-without-metering-evidence.md) 的门槛） |

## 若最终采用金额路径，必须满足的约束（先记原则）

1. **可逐笔关联**：金额必须能关联到具体 Attempt（APIMart 由 `task_id` 关联）；取不到 `task_id` 不得结算；
2. **不得由金额反推计量量**，也不得由 `n`/尺寸/请求参数反推（[0006](./0006-no-settlement-without-metering-evidence.md) 的禁止项）；
3. **差额是折扣不是误差**：APIMart 文档明确「实际扣费还会受到账号分组倍率和折扣影响」。因此上游声明金额与按公开单价计算会有**系统性差额**——对账必须容纳它，不得当异常反复追查；同时意味着仅凭公开单价无法核对实际扣费；
4. **聚合账不能替代本体**：账号级 `usage` 聚合不含 `task_id`，只作交叉校验。

**来源**：第二阶段 Planning。文档与实测记录见 `out-reference/apimart/billing-basis.md`、`tasks-status.cn.md`、`gpt-image-2.5-generation.cn.md`。
