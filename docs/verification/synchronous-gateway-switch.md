# 同步图片网关的切换与历史清理执行清单

- **用途**：执行 [RFC 0017](../design/0017-synchronous-image-gateway.md) §7 的切换与历史清理。机制与边界归该 RFC，本文只列执行步骤、停止条件与留档要求。
- **前置**：S1–S4 的证据齐备；旧 API/Worker 可停；有可回退的备份与维护窗口。
- **记录方式**：每步的查询结果、时间与执行人写进本文末尾的执行记录；破坏性操作另具执行记录。

## 1. 存量盘点（只读）

- 执行记录数量与状态分布：`select execution_protocol, state, count(*) from generation.jobs group by 1,2;`
- 在飞旧 Job：`… where execution_protocol='legacy' and state in ('accepted','leased','submitting');`
- 预授权：`select status, count(*), sum(amount_microusd) from ledger.holds group by 1;`
- 正文/结果存量：`select count(*) from generation.jobs where native_parameters is not null or result_images is not null or carrier_schema is not null;`
- 盘点反向代理、应用日志、WAL、副本与备份的保留策略。

## 2. 关闭旧生成准入并排空

- 停止新旧 Job 排队，等旧在飞排空或转对账。
- 逐个核对旧 `submitting`/`accepted`/`reconciliation_required` Attempt；仍可能在上游执行的绑定渠道槽位并核对 Job、Hold 与 Channel。
- 存量已超新限额时只拒绝新增，不删槽位使计数表面合规。

## 3. 启用直接执行协议

- 打开 `GENERATION_DIRECT_EXECUTION`。滚动部署必须有明确协议路由并禁止旧消费者领取 v1 记录；否则用维护窗口切换。
- 在飞图片无法跨进程搬运，切换造成的连接中断按收费与异常合同处理。

## 4. 历史载荷清理（核账后）

- 财务外键核实后分批清空正文/结果列，观察 WAL 与复制延迟，再删列。
- 证据或成本里的 JSON 与错误文本先按白名单回填再移除旧副本。
- 备份尚未自然过期时，不宣称全量历史载荷清除完成。

## 5. 回退边界

- 删列前可回退到修复后的无载荷版本；删列后只能回退到支持新协议且不写正文的版本。
- 不能恢复旧 Worker 队列，也不能用数据库备份回滚覆盖期间产生的账务。

## 停止条件

- 盘点发现无法可信归属的未决任务：冻结涉及 Channel 的新准入，查明后再继续。
- 核账不平、WAL/复制延迟超阈值、备份未就绪：停止清理。

## 执行记录

（执行后在此填日期、步骤、查询结果、异常与结论。）