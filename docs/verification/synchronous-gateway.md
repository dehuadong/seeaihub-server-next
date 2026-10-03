# 同步图片网关的验收证据清单

- **用途**：执行 [Spec 0005](../specs/0005-synchronous-image-gateway.md) 的 A1–A10 与 [RFC 0017](../design/0017-synchronous-image-gateway.md) §8 的证据采集。验收条件归该 Spec 拥有，本文只列命令、用例与剩余步骤，不重复条件。
- **读者**：执行验收的人。
- **前置**：本机 PostgreSQL 与 `HTTP_CONTRACT_DATABASE_URL`；假上游由测试夹具提供，无真实计费调用。
- **记录方式**：结论写进本节末尾或对应工作项评论。

## 已产出的机器证据

| 验收 | 命令 / 用例 | 状态 |
| --- | --- | --- |
| A1 | `cargo test -p seeai-api --test http_contract cases_direct_execution -- --ignored`（url/base64 闭环、全程不启 Worker） | 通过 |
| A2 | persistence `execution_admit` 断言载荷列为 NULL；`execution_finalization` 断言不写 Provider 原文；adapter `gateway_error` 脱敏用例 | 通过 |
| A3 | `cases_direct_execution::direct_invalid_key_is_rejected_before_the_body_is_parsed` | 通过 |
| A4 | persistence `execution_admit`（同键/异指纹/并发名额）、`execution_finalization`（不重复扣费）、application `direct_execution` 同键四投影 | 通过 |
| A5–A6 | persistence `execution_submission`（句柄先入库再轮询、fencing、期限）、application `execution_reconciliation`（只读查询、缺口建案、孤儿回收、晚到事实） | 通过 |
| A7 | 真库 408 慢读用例；application `direct_execution` 期限与断开处置；`execution_reconciliation::api_and_worker_finalizations_charge_at_most_once`（S3 与 Worker 竞争不重复收费） | 通过 |
| A8 | 直接执行不再每 250ms 查询（A1 用例不启 Worker）；adapter 共享 Client 复用单测；执行/读取/发送许可单测 | 部分 |
| A9–A10 | `cases_migrations` 增量迁移用例；管理/客户读取走 metadata 投影的既有用例 | 部分 |

## 待做

- **A8 结构性结果**：SQL 次数不随 Provider 等待时长增长、账务行大小不随图片增长、候选增多不产生逐候选大图副本、峰值 RSS 落在预算内。
- **性能基线**：同一棵树两态对比——`GENERATION_DIRECT_EXECUTION=false` 是整改前的旧 Worker 路径，`true` 是整改后的直接执行；记录吞吐、网关新增延迟、Provider 耗时、峰值 RSS、SQL 次数与连接等待。
- **A9**：旧在飞任务计入渠道容量（S5 初始化）。
- **S5 破坏性清理与生产切换**：受控窗口、核账后执行，另具执行记录；历史未清完不宣称清除完成。

## 验收记录

（执行后在此填结论。）