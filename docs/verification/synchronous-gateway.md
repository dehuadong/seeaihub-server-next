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
| A4 | persistence `execution_admit`（同键/异指纹/并发名额）、`execution_finalization`（不重复扣费）、application `direct_execution` 同键四投影与轮换后重放（`a_rotated_fingerprint_key_replays_the_same_request`、`a_replay_without_the_recorded_key_version_is_an_idempotency_conflict`、`a_replay_survives_a_disabled_candidate`） | 通过 |
| A5–A6 | persistence `execution_submission`（句柄先入库再轮询、fencing、期限）、application `execution_reconciliation`（只读查询、缺口建案、孤儿回收、晚到事实） | 通过 |
| A7 | 真库 408 慢读用例；application `direct_execution` 期限与断开处置；`execution_reconciliation::api_and_worker_finalizations_charge_at_most_once`（S3 与 Worker 竞争不重复收费） | 通过 |
| A8 | 直接执行不再每 250ms 查询（A1 用例不启 Worker）；`direct_sql_count_does_not_grow_with_provider_wait`（长短等待的事务增量不随等待增长）；`direct_slow_provider_does_not_occupy_a_database_connection` 与 `direct_slow_provider_keeps_more_requests_than_pool_connections_in_flight`（等待期间连接池空闲）；`direct_memory_budget_rejects_a_second_concurrent_execution`（字节预算拒绝第二个在飞执行）与 `direct_peak_rss_stays_within_the_memory_budget`（8MiB 大图请求后 VmHWM 在预算内，实测 76–84MiB）；adapter 共享 Client 复用单测；执行/读取/发送许可单测；`two_api_replicas_share_the_account_and_channel_capacity`（两 API 进程共享账户/渠道容量，held 恒为 1）与 `a_peer_replica_falls_back_to_the_database_when_the_route_cache_is_stale`（缓存陈旧回源；直接执行每次受理直接读 `active_offering()`、不读 route 缓存）；两态吞吐/延迟基线见「验收记录」；顺序直接执行的渠道耗时与网关耗时拆分见「验收记录」 | 部分（网关新增延迟 p50/p99、WAL 字节/请求与对账延迟待做） |
| A9–A10 | A9：`admit` 渠道容量并入 legacy 在飞 Job + `execution_admit::legacy_in_flight_jobs_count_toward_the_channel_capacity`；A10：`cases_migrations` 增量迁移用例与 metadata 投影的既有用例 | 部分（A10 旧库清理证据待 S5） |

## 待做

- **A8 剩余**：账务行大小不随图片增长与候选不复制大图已有代码路径结论，可补测量。
- **性能基线剩余**：两态的总耗时、平均与 p95 延迟、吞吐见「验收记录」；顺序直接执行的渠道耗时（固定 D）与网关净耗时（总耗时 − D）拆分见「验收记录」；网关新增延迟的 p50/p99、WAL 字节/请求与对账延迟尚无本机观测。
- **S5 破坏性清理与生产切换**：受控窗口、核账后执行，另具执行记录；历史未清完不宣称清除完成。

## 结构性判断（非测量，供复核代码路径）

- 直接执行路径的事务只包仓库端口内的写，Provider 等待发生在端口之外，因此没有事务跨 Provider 等待；A1 用例不启 Worker 即返回 200，说明没有结果轮询。
- 账务行只由强类型列与快照组成，图片不进任何持久列（A2 的列断言），因此账务行大小不随图片增长。
- 图片在内存里以 `Bytes` 或原字符串按 `Arc<GatewayInput>` 共享一次、重试复用同一份，不产生逐候选大图副本。
- 以上是**代码路径的结论**；SQL 次数、等待期间连接占用、峰值 RSS、两态吞吐/延迟基线与顺序直接执行的渠道/网关耗时拆分已有测量用例（后两者见「验收记录」）。

## 验收记录

### A8 两态性能基线（2026-10-04）

同一棵树、同一台机器、同一份假上游延迟下的**单次**本机观测，只作记录，不构成性能门槛。

| 项 | 取值 |
| --- | --- |
| 用例 | `cases_performance_baseline::direct_execution_two_state_throughput_baseline` |
| 命令 | `HTTP_CONTRACT_DATABASE_URL=<本机 .env 的 DATABASE_URL> cargo test -p seeai-api --test http_contract cases_performance_baseline -- --ignored --nocapture` |
| 构建 | `cargo test` 测试 profile（未优化 + debuginfo） |
| 机器 | 11th Gen Intel Core i7-11700 @ 2.50GHz（16 逻辑核）、约 15.5GiB 内存、WSL2 x86_64；rustc/cargo 1.99.0 |
| 负载 | 并发 8、每态 48 个成功请求（预热 1 次不计）；假上游每次生成请求固定延迟 200ms |
| 两态 | 关：`GENERATION_DIRECT_EXECUTION` 关闭，夹具起一个真实 Worker 领任务（旧路径）；开：直接执行，不启 Worker |
| 夹具 | 两态各用一份独立一次性库与 1e9 微美元余额账户；每态延迟只统计成功请求，任何非 200 会让用例失败 |

| 态 | 总耗时 | 平均延迟 | p95 延迟 | 每秒完成数 |
| --- | --- | --- | --- | --- |
| 关（旧 Worker 路径） | 12.442s | 1939.0ms | 2313.4ms | 3.86 |
| 开（同步直接执行） | 2.384s | 370.1ms | 528.2ms | 20.13 |

用例不写性能断言；吞吐倍数与延迟阈值由整改前基线与部署目标确定（RFC 0017 §8）。

### A8 渠道耗时与网关耗时拆分（2026-10-04）

关闭并发（顺序发），只测直接执行开后那条路径：假上游每次生成请求固定延迟 D，一次请求的总耗时就是 D 加上网关与执行路径给它加的时间。单次本机观测，只作记录，不构成性能门槛。

| 项 | 取值 |
| --- | --- |
| 用例 | `cases_performance_baseline::direct_execution_gateway_overhead_split` |
| 命令 | `HTTP_CONTRACT_DATABASE_URL=<本机 .env 的 DATABASE_URL> cargo test -p seeai-api --test http_contract cases_performance_baseline::direct_execution_gateway_overhead_split -- --ignored --nocapture` |
| 构建 | `cargo test` 测试 profile（未优化 + debuginfo） |
| 机器 | 11th Gen Intel Core i7-11700 @ 2.50GHz（16 逻辑核）、约 15.5GiB 内存、WSL2 x86_64；rustc/cargo 1.99.0 |
| 负载 | 并发 1（顺序，前一个返回才发下一个）、10 个成功请求（预热 1 次不计）；假上游每次生成请求固定延迟 200ms |
| 夹具 | 一份独立一次性库与 1e9 微美元余额账户；延迟只统计成功请求，任何非 200 会让用例失败 |

| 口径 | 均值 | p95 |
| --- | --- | --- |
| 总耗时（含上游 D = 200ms） | 286.1ms | 310.8ms |
| 网关净耗时（总耗时 − D） | 86.1ms | 110.8ms |

网关净耗时含受理与结算的数据库往返、选路与参数映射、序列化、连接获取、响应序列化，以及顺序循环自身的调度与排队。p95 取向上取整那一档；10 个样本时它就是最大值。