主题: 同步图片网关收敛为唯一直接执行路径
当前修订: v1
生效修订: v1
状态: 已接受
承接: [同步图片网关 Spec](../specs/0005-synchronous-image-gateway.md) §6–§7 的唯一路径合同
依赖: [同步网关设计 v1](0017-synchronous-image-gateway.md)、[整改设计](0018-synchronous-gateway-remediation.md)

# 同步图片网关唯一路径整改设计

本稿拥有"删除执行开关与旧生成路径、使 API 直接执行成为唯一路径、业务载荷彻底不再落库"的删除面与迁移。9 项 P1 的机制归[整改设计](0018-synchronous-gateway-remediation.md)；两份稿改动同一批文件，串行实施，不并行编辑。

## 1. 现状与偏差

[Spec 0005](../specs/0005-synchronous-image-gateway.md) 要求 API 直接执行 Provider 调用、不依赖 Worker 的生成队列或 PostgreSQL 结果轮询，没有定义执行开关，也没有把旧路径保留为长期选项。实现却保留了 `GENERATION_DIRECT_EXECUTION` 且默认关闭：关闭时图片入口仍建 Job、Worker 领取、API 轮询，请求参数与结果图片继续落库。[迁移 0031](../../migrations/0031_synchronous_gateway_minimal_facts.sql) 明确把删列推迟到切换切片。运行态因此与 Spec 合同不一致，A2/A9 不能按"开关打开时那条路"判通过。

开发阶段没有历史数据、没有迁移负担、不需要兼容旧记录。一次性切换后不再保留开关、旧路径与载荷列。

## 2. 目标状态

| 面 | 目标 |
| --- | --- |
| 入口 | 两个图片入口只有一条执行路径：API 进程内直接调 Provider，先确认结算再返回 |
| 载荷 | 请求参数、图片、结果图片与 Provider 错误原文都不进持久层、日志和 trace |
| 持久层 | `generation.jobs` / `generation.attempts` 只留最小执行与账务事实；载荷列不存在 |
| Worker | 只做过期所有权接管、已知句柄只读对账、账务核对与晚到事实收件 |
| 配置 | 不存在执行开关；直接执行所需配置无条件生效 |

## 3. 代码删除面

### 3.1 开关与入口分支

- `apps/api/src/main.rs`：`direct_execution_enabled()` 与 `GENERATION_DIRECT_EXECUTION` 读取、`direct_execution` 的 `if/else` 与 `None` 分支、只为分支存在的 `direct_misconfigured`。
- 图片路由中间件改为无条件挂载；`AppState.direct` 从 `Option<Arc<DirectGeneration>>` 改为必填，删除 `generations` 与 `sync_wait` 字段。
- 两个入口删掉旧分支；`run_sync_generation`、`sync_image_response`、`sync_error_response`、`sync_error_response_with` 整体删除。
- 启动与停机：Supervisor 无条件构造，停机时无条件 `begin_drain` 与 `drain`。

### 3.2 Application 旧执行栈

- 删 `GenerationService` 与 `WorkerService` 中的生成领取和执行。
- 删 `HubRepository` 中只服务旧路径的方法：`create_job`、`get_job`、`claim_next_job`、`recover_expired_leases`、`begin_attempt`、`requeue_after_unaccepted`、`renew_lease`、`complete_job`、`fail_job`、`count_in_flight_jobs`、`acceptance_probe`、`consecutive_offering_failures`。
- 删只服务旧路径的类型：落库命令 `CreateImageGeneration`、`ClaimedJob`、`JobView`、`LeaseRecovery`、`CompleteJob`、`UnacceptedAttempt`、`AttemptFailure`、`HoldDisposition`、`ExecutionProtocol` 枚举。**保留 `CreateImageGenerationRequest`**：它是接收入口的形状，直接执行在用。
- 保留被直接执行复用的 `freeze_offering_pricing`、`provider_cost_fact`、`self_computed_cost`、`validate_idempotency_key`、`requested_image_count`、`contract_parameter_face` 与候选选择。

### 3.3 Persistence 旧面

- 删旧 Job 写入、领取与收尾 SQL 及其行映射：`create_job`、`get_job`、`claim_next_job`、`recover_expired_leases`、`begin_attempt`、`requeue_after_unaccepted`、`renew_lease`、`complete_job`、`fail_job`、`count_in_flight_jobs`、`acceptance_probe`、`consecutive_offering_failures`、`load_generation_job`、`row_to_generation_job`。
- 删 `execution_protocol = 'legacy'` 的渠道容量统计，改为按状态统计。
- 保留新路径端口：`admit`、`lookup_execution`、`begin_submission`、`record_acceptance`、`settle`、`fail_or_reconcile`、`read_finalization`、`offer_late_facts`、`renew_execution_ownership`、`takeover_expired_executions`、`reap_unsubmitted_admissions`。

### 3.4 Worker 与其他调用方

- `apps/worker/src/main.rs`：去掉 `WorkerService` 构造与 `run_round` 里的 `worker.run_once()`，只跑对账。
- 删 `WORKER_LEASE_SECONDS` 并使 `crates/application/src/request_timeout.rs` 不再校验"租约 ≥ 上游超时"。
- 检索 `apps/web`、管理接口、脚本与告警是否读取旧 Job 的结果字段或旧状态；有则删除或改为最小投影。

## 4. 迁移：删除载荷与旧协议

用尾部迁移追加，不改已应用的迁移：`sqlx::migrate!` 在编译期嵌入并按 checksum 校验，改历史迁移会让运行期直接 `VersionMismatch`。

| 对象 | 处置 |
| --- | --- |
| `native_parameters`、`result_images`、`carrier_schema`、`parameter_mapping`、`idempotency_key`、`request_hash` | 删列；发布合同的 `supply.offerings.carrier_schema` 保留 |
| `lease_owner`、`next_attempt_at`、`version` | 删列（旧 Worker 租约与领取）。`version` 只有自增、没有读取点，删列要连同约二十处 `version = version + 1` 一起清理 |
| `lease_expires_at` | **保留**。v1 续约写它、接管候选按它筛选；删掉会让直接执行的所有权接管失效，且编译期发现不了 |
| `execution_protocol` 列与 `jobs_execution_protocol_known` | 删除；同时去掉 0033 两条部分索引与其它 SQL 里的 `execution_protocol = 'v1'` 条件 |
| `jobs_claimable` 索引 | 删除 |
| `jobs_account_id_idempotency_key_key` | 随列自动消失；此后唯一防重只剩 `jobs_idempotency_digest_key`，必须把 `idempotency_key_digest` 改为**无条件必填** |
| `jobs_state_check` | 收窄为 `admitted`、`executing`、`succeeded`、`failed`、`reconciliation_required` |
| `attempts_state_check` | 收窄为 `prepared`、`submitting`、`accepted`、`terminal`、`unknown` |
| `attempts.provider_error_message` | **保留**：v1 对账路径也写平台生成的有界文本 |

迁移先做守卫：`generation.jobs` 存在 `execution_protocol = 'legacy'` 且 `state IN ('accepted','leased','submitting','reconciliation_required')` 的行，或 `generation.attempts` 存在 `state IN ('succeeded','failed','reconciliation_required')` 的行时整条迁移失败，由人决定重建开发库或先手工处置。不能自动标失败：`submitting` 与 `reconciliation_required` 表示上游可能已受理，标失败等于平台单方认赔，而且删掉开关后没有任何现存路径能收尾 legacy 的对账态。

七张引用 `generation.jobs(id)` 的外键都挂主键，与待删列无关；全部迁移无视图、无触发器、无函数或规则；财务表不引用待删列，`ledger.holds`、`ledger.entries`、`generation.execution_capacity`、`operations.reconciliation_cases`、晚到事实表与账务外键保持不动。

## 5. 删除会静默带走的行为

这三处不会被编译发现，必须在同一个变更里补回：

1. **每日扣费上限。** 唯一判定点在旧 `GenerationService`，v1 受理只判在飞名额与渠道容量。必须把当日累计判定移进 v1 受理（推荐放进 admit 事务），保留到顶返回 429 的语义。
2. **对客用量的状态口径。** 用量查询按字面匹配状态名：进行中过滤 `('accepted','leased','submitting','reconciliation_required')`，而 v1 写 `admitted`/`executing`，切换后客户控制台「处理中」永远为空。状态解析还必须认识 v1 五个阶段，否则有在飞 v1 请求时用量接口直接报错。
3. **产出张数。** 用量明细与账单汇总额按 `result_images` 的张数计算，v1 没有任何落点，张数只活在内存。必须新增最小落点（明确列或等价结构），在结算时写入实际产出张数。

## 6. 配置收敛

- 删除 `GENERATION_DIRECT_EXECUTION`。
- `REQUEST_FINGERPRINT_KEY_V1` 变成无条件必填，照 `CUSTOMER_HISTORY_CURSOR_KEY` 的启动校验写法。
- 其余容量与期限项保留现有默认值，只删掉"关着时不读"的条件加载。
- 删除 `WORKER_LEASE_SECONDS`；`GENERATION_EXECUTION_LEASE_SECONDS` 仍由 API 与 Worker 共同读取。
- 同步 `.env.example` 与 [`docs/operations/configuration.md`](../operations/configuration.md)。

## 7. 测试与验收判定重写

默认夹具目前**不开**直接执行，靠真实 Worker 走旧路径：`sync_json`/`sync_multipart` 每次请求都 `spawn_worker`，约 110 条合同用例因此走旧路径。删列之前必须先把这批整体改判。

- 夹具：`harness.rs` 去掉开关分支，`start_direct*` 成为唯一入口，删掉 `WorkerProcess`/`spawn_worker*`，按幂等摘要而非明文键查 Job。
- 重写：断言旧列或旧状态名的用例改判为对假上游收到的请求体或最小事实。
- 迁移用例改为断言新迁移之后这些列不存在、守卫拒绝旧行。
- 两态基线改为单态。
- crate 级：`worker_and_cost.rs`、`ledger_audit/tests.rs` 的 `HubRepository` 替身随 trait 收窄；`worker_loop_tests.rs` 收窄为对账循环。
- 改：A2/A9 的判定对象是**不设任何开关的默认进程**。
- 加：不设开关即可完成一次直接执行；Worker 不再领取生成；每日上限到顶返回 429；用量张数等于实际张数；迁移守卫拒绝旧行。

## 8. 交付顺序

1. 直接执行无条件生效：删开关、旧分支、中间件与 drain 条件。
2. 把每日上限、用量状态口径与产出张数落进 v1。
3. 删 Application、Persistence 旧栈与 Worker 生成领取。
4. 改夹具与依赖旧路径的用例，重写两态基线。
5. 追加迁移：守卫 → 收窄 CHECK → 删索引与协议条件 → 删列。
6. 同步配置与文档。
7. 与[整改设计](0018-synchronous-gateway-remediation.md)的 9 项 P1 串行：本稿先收敛路径，再实施 0018 余下项。

## 9. 验证边界

删除完成以三类证据判定：编译与全量测试无对旧路径的引用；默认配置（不设开关）启动后图片请求直接执行且载荷列不存在；对账、资金与晚到事实用例保持通过。未取得这些证据前不判定完成。
