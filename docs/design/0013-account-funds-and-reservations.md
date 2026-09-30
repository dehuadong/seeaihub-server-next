主题: 账户资金与预授权的当前值模型
当前修订: v4
状态: 已评审通过
承接: [`账户余额、预授权与实际收支` v3](../specs/0002-account-funds-and-reservations.md) §1–§7
依赖: [`0007` 定价、保底与结算](0007-pricing-floor-and-settlement.md)、[`0008` 路由与缓存](0008-routing-strategy-and-caching.md)、[`0009` 运行基线](0009-operational-baseline.md)；[`ADR-0003`](../adr/0003-postgresql-is-source-of-truth.md)、[`ADR-0006`](../adr/0006-no-settlement-without-metering-evidence.md)

# 账户资金与预授权的当前值模型

本 RFC 负责账户当前金额、预授权状态、资金流水、余额缓存、每日消费检查与账务核对。保底额的查表、对客价格和计量证据仍由[`0007`](0007-pricing-floor-and-settlement.md)与[`ADR-0006`](../adr/0006-no-settlement-without-metering-evidence.md)拥有；路由候选缓存仍由[`0008`](0008-routing-strategy-and-caching.md)拥有。

## 1. 当前金额及不变量

消费者账户行保存 `balance_microusd`（已结算余额）、`held_microusd`（当前占用合计，初值 0）及单调递增的 `version`。可用额由这两列相减，不存第三份权威数。平台账户沿用余额列记录自担成本，`held_microusd` 恒为 0，不进入消费者缓存。

选择账户当前值作为受理与读取的依据，是为了让操作成本不随历史流水条数增长；代价是余额、占用与对应明细必须在同一事务内维护。继续用流水汇总生成每次余额虽可减少冗余数据，却使请求耗时随历史记录增长；仅把当前额放进 Redis 会使缓存故障和写入乱序影响资金判定。数据库当前值加独立核查保留了可追溯性，也使缓存失效时仍可受理。

| 不变量 | 用途 |
| --- | --- |
| `available = balance − held`，`held ≥ 0` | 每次受理和账户查询只读当前账户行；`balance` 与 `available` 可因按实际结算而为负。 |
| `held = SUM(active holds.amount)` | 每笔预授权的当前状态与账户合计一致；只用于后台核查，不在受理路径重算。 |
| `balance = SUM(actual entries.amount)` | 独立核查实际收支是否漏记；不用于生成余额或受理判定。 |

`ledger.holds` 保留每个 Job 唯一的一行、金额和 `active` / `captured` / `released` 状态。`ledger.entries` 只保留 `credit`、`capture`、`adjustment`、`cost` 科目。金额符号延续现状：入账为正，扣费和平台成本为负。零实收不插入 `capture`；请求与预授权记录仍可证明该次执行成功且未收费。`capture` 的业务键和 `job_id` 唯一约束防止同一 Job 出现两笔实收。

## 2. 写入事务

所有金额更新以 PostgreSQL 为权威，账户行是同账户资金变更的串行点；`version` 随每次金额变化递增。`UPDATE … RETURNING` 返回已结算余额、占用合计、计算出的可用额和版本，供提交后更新 Redis。算术转换及加减在整数范围内检查，不溢出、不截断。

### 2.1 开户与充值

初始充值和普通充值在账户余额上加金额，同事务写唯一业务键的 `credit`。业务键重复且参数一致时返回当前账户快照；参数不同则冲突，不重复加余额。充值不改变占用合计。

### 2.2 请求受理

现有 Job 幂等锁与 `(account_id, idempotency_key)` 唯一约束保留。新请求使用单条账户条件更新完成并发闸门：

```sql
UPDATE ledger.accounts
SET held_microusd = held_microusd + $hold,
    version = version + 1,
    updated_at = now()
WHERE id = $account
  AND kind = 'consumer'
  AND balance_microusd::numeric - held_microusd::numeric >= $hold
RETURNING balance_microusd, held_microusd,
          balance_microusd - held_microusd AS available_microusd,
          version;
```

条件不成立返回现有 `insufficient_balance`。同事务插入 Job、路由判定与一条 `active` Hold；任何后续写入失败使占用回滚。幂等重放不执行条件更新。保底额为 0 且可用额为负时条件仍不成立。

### 2.3 成功结算

依现有 Job 租约与状态锁定执行记录，取得本 Job 唯一的 `active` Hold，并用冻结价格与可核验证据得出实收。一个事务内：将账户 `balance_microusd` 减去实收、`held_microusd` 减去该 Hold 金额；将 Hold 从 `active` 转为 `captured`；更新 Job、Attempt 与成本事实；实收大于 0 时写唯一业务键的 `capture`，其 `job_id` 指向该 Job。Hold 状态更新必须恰好影响一行，否则回滚，不靠重复调用制造第二次扣费。实收超过 Hold 不设上限，负余额按既有规则保留。

### 2.4 失败、对账与恢复

确定无需对客收费时，账户占用合计减去该笔 Hold 金额，Hold 从 `active` 转为 `released`，Job 与 Attempt 同事务终结；不改已结算余额、不插入对客资金流水。结果不确定、租约在上游提交后过期或证据不足时，Hold 保持 `active`，账户占用不变，进入 `reconciliation_required`。尚未扣费的人工处置只关闭 Hold 并减少占用，运营处置记录写“解除预授权”，客户资金流水不出现该动作；已实际扣费后的退款另走正式 `adjustment`。所有状态转换检查受影响行数，重复请求按现有幂等规则返回结果，不再次改变金额。

上游已收费但消费者未收费时，平台账户仍按现行规则写 `cost` 并减少平台账户余额。这个写入与消费者 Hold 的保留或释放互不代替。

## 3. 账户读取与 Redis

账户仓储一次读取同一账户行中的余额、占用、可用额和版本，避免分别查询余额与 Holds 时混入两个提交时点。API 将 `balance_microusd` 明确为已结算余额，增加 `available_microusd`，保留 `held_microusd`；管理员和客户使用同一语义。管理员账户页可分别标示三项；客户控制台只渲染 `balance_microusd` 为“已结算余额”，移除现有“持有中”卡片及金额提示，不渲染可用额；客户用量与资金流水也不展示预授权金额。需要即时核账的余额读取与管理员读使用 PostgreSQL 当前行。

Redis 的 `user_balance` 值改为 `{balance_microusd, held_microusd, available_microusd, version}`，继续在数据库提交后写入。写缓存时仅接受不低于缓存当前版本的快照：写回按「读当前版本 → 比较 → 写」执行，挡下「提交更早、写回更晚」的倒序。两条写回真正同时执行、都读到同一旧版本的窄窗口**不在缓存层消除**——它只让缓存暂时偏旧，不改变任何资金结果（受理一律由数据库条件更新确认），由数据库的串行提交、下一轮对账与缓存过期兜底；把该判定做成原子操作需要缓存实现理解「版本」，与「缓存语义留在用例层」的分工相悖，本设计不采用。Redis 不可用或写失败时数据库结果不回滚。缓存副本核对仍按数据库当前行校正，不与账实核对混为一项；缓存版本高于这次数据库读数时不动它（那次读发生在新提交之前）。

预检可以从缓存读取可用额以减少不必要的读取，但不直接完成受理；缓存报告不足时也继续交给数据库条件更新确认，再决定是否返回 402。PostgreSQL 是资金事实来源，遵守 [`ADR-0003`](../adr/0003-postgresql-is-source-of-truth.md)；路由候选、API Key 的缓存规则见[`0008`](0008-routing-strategy-and-caching.md) §7。

## 4. 每日消费限额

现有受理路径每次按账户和当天时间汇总 `capture`。改为 PostgreSQL 中每账户每 UTC 自然日一行的已结算消费合计，合计以正数记录实收；成功结算写负数 `capture` 的同一事务增加对应日期合计，零实收不增加。受理读取当天一行，不扫历史流水。结算日期由数据库时钟决定，与 `capture.created_at` 取同一事务时刻；缺行视为 0。该限额继续只看已经完成的消费，不预占未来消费，单笔与在飞请求仍可能使当日合计越过阈值。

每日合计是查询投影，不替代账户余额或资金流水。业务键及 Job 终态保证重放不重复累加。需要核查时可按日期汇总该日 `capture` 与每日合计比对，核查不在受理路径运行。

## 5. 用量、账单和账实核对

Job 上增加终态时刻；成功结算的终态时刻与 `capture.created_at` 由同一数据库事务确定。已完成请求的区间过滤按终态时刻，处理中请求按受理时刻显示。用量行返回请求时刻和终态时刻，逐笔扣费只关联该 Job 的 `capture`，不再同时用 Job 创建时间和流水入账时间筛选同一笔扣费。账单中扣费与调整按各自流水入账时刻归属；已完成请求与图片数按 Job 终态时刻归属。处理中请求不计账单请求数。跨天结算以结算日入账，不把次日的扣费投进昨日区间。

取消默认每 15 分钟对全库全部历史流水求和的账实任务，保留可按账户触发的后台核查：核对 `balance = SUM(actual entries)` 与 `held = SUM(active holds)`，发现差异只建案并告警，不自动改账。账户级差异另建可指向 `account_id` 的核查案例；现有必须关联 Job 和 Attempt 的对账案例只处理单笔请求，不承载账户级差异。生产规模增长后按账户分批调度，查询走账户与日期索引；该任务不参与任一资金写入或余额读取。Redis 副本的增量校正仍独立运行。

建议的查询索引按实际执行计划核定：`holds` 的有效行按账户、`entries` 的账户加入账时间及 `capture` 的 Job 关联、`jobs` 的账户加终态时间、每日合计的账户加 UTC 日期。索引服务查询，不改变任何金额规则；不因共用一张流水表预先分表。

## 6. 与现行文档和代码的交接

[`0001` 控制台 Spec](../specs/0001-admin-and-customer-consoles.md) v14 与本 RFC 使用同一客户金额语义；[`0007`](0007-pricing-floor-and-settlement.md) 继续负责保底额与价格计算，[`0008`](0008-routing-strategy-and-caching.md) 继续负责路由及非账务缓存，[`0009`](0009-operational-baseline.md) 负责运维入口，[`0010`](0010-identity-and-consoles.md) 负责身份与控制台读，[`0011`](0011-console-information-architecture.md) 负责管理端页面组织，[`0014`](0014-customer-console-navigation-and-history.md) 负责客户页面组织与历史浏览。账户资金写法、Redis 余额快照与账实核查由本 RFC 统一承接。

没有已上线账务数据需要转换。实现采用新建或调整数据库迁移并以干净开发库验证完整迁移链，不对已应用的迁移文件做历史改写，也不承担旧流水回填。账户页面布局工作与本方案的金额语义改动分别验收。

## 7. 验证切入点

用无费用的假上游和 PostgreSQL 合同用例覆盖：同账户并发占用、幂等重放、充值与结算竞争、Hold 为零、实收小于/等于/大于 Hold、确定失败与不确定结果、人工释放、重复收尾、零实收、平台成本、每日消费跨 UTC 日、跨天结算账单、Redis 断连和倒序写回。断言账户当前额、每笔 Hold、实际流水、Job 状态及客户和管理员响应逐项一致；浏览器用例检查客户页面只显示已结算余额，不出现可用额、持有中或单笔预授权金额；同时检查受理与余额读取的 SQL 不按历史流水求和。
