# 同步图片网关整改验证

本清单验证[整改设计](../design/0018-synchronous-gateway-remediation.md)对[同步网关 Spec v2](../specs/0005-synchronous-image-gateway.md#8-验收条件)（含 A11）的落实。它说明验证方法，不表示整改实现或以下用例已经通过。普通用例只使用本地假 Provider；生产清理、切换与真实 Provider 调用需要各自授权。

## 1. 故障与验收映射

| 故障 | 必须取得的证据 | Spec |
| --- | --- | --- |
| 预算小于最大响应及副本 | 最大输入、128 MiB Provider 响应、编码膨胀、结构节点/容器边界的实际预留与 RSS；超预算无 Hold、无生成；容量在受理前立即拒绝；读请求和 transport 隐藏缓冲计入 | A3/A8 |
| 续约异常继续执行 | Conflict、数据库断开、超时三类续约失败后零新增 upload/submit/retry/poll；已收到的事实仍可有限收尾；已知租约到期阻断资格 | A5/A7 |
| 接管后成功事实丢失 | 取得同步成功后暂停 API、Worker 接管、再恢复 API；晚到计量与成功状态可靠收件，当前 owner capture 一次；API 不按旧 token 结算 | A4/A5/A6/A7 |
| 任意 Provider ID 落库 | 在 task_id、request-id、error.tid、late facts 投入 URL、data URL、控制字符、超长值和 marker；原值不出现在 DB、日志、trace、缓存或告警；合法格式仍能对账 | A2/A6 |
| socket 写阻塞不超时 | 使用真实 TCP/HTTP2 客户端停止接收足够大响应，使服务端 send buffer/flow-control 真正阻塞；不再 poll body 时仍到期关闭、transport 任务退出、permit 和预算释放 | A7/A8 |
| 提交前断开不取消 | 正文前、中、解析后、受理后、begin_submission 后生成发送前分别断开；已确认未提交零生成且释放确认；开始发送竞争只能确定一方获资格 | A5/A7 |
| 每候选复制图 | 同一接近上限图配 1/8/32 候选，候选数增长只增加轻量计划；累计图片分配不线性增长；实际映射仅选中一次，所有路由/映射兼容用例保持行为 | A1/A8 |
| 合同冻结与指纹轮换 | 记录保存受理时的合同与指纹版本；轮换密钥或合同变化后同键按记录规则投影，移除旧密钥返回 409 不新建；并发同键至多一次受理/capture；空库不需要查找密钥 | A4/A9 |
| 失败/取消携 usage 被成功收费 | APIMart failed/cancelled 各带 usage 与 cost，消费者实收零，Hold/终态渠道容量释放，成本依失败口径；completed 有有效证据才成功结算 | A6/A7/A10 |

## 2. 传输与并发

socket 测试要能证明写实际处于 Pending，而非仅在首次 `body.poll_next` 前推进时钟。覆盖 HTTP/1 一块/多块响应、已交出最后 body 块但尚未 flush、客户端零读/慢读/半途断开；观测 connection 任务、关联 task group、字节预算及 sends 数目。正常 HTTP/1 的 close 响应应在正常 flush 后释放资源。

HTTP/2 设置小 flow-control window，使流被真实阻塞；覆盖一个图片流与多个同连接图片/非图片流。最早发送期限应终止整连接及关联任务；其他流的预提交执行停止、可能提交执行仅有限收尾；不同连接不受影响。验证已成功交 DATA 的响应仍保守持有预算至连接终止，发送名额没有提前释放，也没有关闭后滞留。不能用单元测试中的 Body drop 替代此证据。

请求断开覆盖 HTTP/1 pending service 时已有 pipeline bytes 缓存在 Hyper read buffer 的场景，防止仅靠 handler Drop 漏掉 FIN；HTTP/2 覆盖 header 后 RST_STREAM、请求正文未读完时 reset 和整连接终止。客户端断开与 `DispatchGate` 开始动作并发，用屏障而非定时猜测控制竞态。monitor 覆盖注册确认前不启动连接、注册/控制队列满、cleanup 保留容量、fd 重用、重复关闭事件、duplicate fd 注销确认和线程故障；不消费 HTTP 字节、不热轮询，故障时零新增生成。H2 task group 覆盖流反复创建/结束、容量满及关闭期间拒绝新 spawn，验证完成 handle 持续回收。

Supervisor 超时、停机及 transport 关闭不能直接强杀已可能提交的 Application 收尾。独立 finalization deadline 到期后任务及图片释放；未知 Hold/渠道容量按合同保留，账户执行名额按既有规则释放。

## 3. 收尾与晚到事实

用真实 PostgreSQL 事务屏障控制 API 与 Worker 的所有权切换，分别注入：admit 成功尚无 Attempt 的取消、`cancel_unsubmitted` COMMIT 响应丢失、Provider 成功已提取事实、settle 前、COMMIT 响应丢失、read_finalization 返回 reconciliation、late-fact commit 后消费前、capture 后 mark consumed 前。核对账本至多一条非零 capture、Hold 不重复释放、同一 Attempt 证据关联和成本不会丢弃。

收件测试包含错误 Job/Attempt、错误凭据、旧 token、正确的过期 token、重复相同事实、单调补全、矛盾成功/失败或计量及重复冲突。接收旧 token 仅限 inbox，不修改 owner、不直接结算；每 Attempt 的 inbox/case 行数有界。已 failed/人工释放记录的晚到成功不重开扣费，已有成功的矛盾晚到事实建案。

数据库不可用超出 finalization budget、进程在事实提取与持久化之间强杀时，验证残余状态符合查询能力边界：APIMart 仅按已知原句柄查询，AIHubMix 保留人工缺口；无任何生成重提。该场景不能宣称零事实丢失。

## 4. 合同冻结、指纹轮换与旧库升级

### 4.1 合同与指纹版本

构造带账户余额、Hold、Attempt、计量与外键关联的 fixture，记录受理时写入的合同版本与请求指纹版本。全部 fixture 密钥为本地随机测试值，不使用环境 Provider 凭证。

覆盖：抬升 `REQUEST_FINGERPRINT_KEY_VERSION` 并保留旧版本后，同键同请求仍按记录版本比对并投影原结果，不新建 Job/Hold 或 capture；移除旧版本密钥时同键返回 `409 idempotency_conflict`，不当作新请求执行；合同在两次请求之间变化（新增必填项、默认值或图片规则）时，较早记录仍按自己的规则比对。

并发相同 key、不同请求指纹以及查找与受理同时发生时，都不得产生第二次占用或生成；无 key 请求每次独立受理。空库与全新部署只用无密钥 SHA 身份，不需要任何查找密钥。

### 4.2 旧库升级

构造带账户余额、Hold、Attempt、计量、账本外键、不同 Job 状态及活动渠道占用的 fixture，演练从保留业务载荷的旧路径升级。比较升级前后的余额、Hold、Attempt/证据、capture 数量与外键关联；旧在飞执行排空或转对账，仍可能运行的旧任务计入渠道容量或冻结新增准入。

旧执行端不能再写正文；历史载荷与备份保留策略按运维范围留清理证据，测试数据库清列不代表生产历史清理完成。历史非法标识清理只删除非法字段，不删除最小幂等或账本记录。

## 5. 完整验收覆盖

A1 同时验证 generations/edits 的 URL/base64、multipart/JSON、已有分支/默认/必填/参数过滤、候选较小 n 与映射/路由策略；关闭 Worker 生成循环而保留异常处理，不存在公开异步接口。

A2 在请求和响应中使用不同 marker，搜索所有允许/禁止持久介质及观测出口；合法 task/trace/计量可查，其他载荷不可查。禁止输出 marker 到测试日志以免测试本身污染观察。反向代理正文记录配置、生产 dump/swap 策略的核查按既有验收单留证据。

A3 检查无效凭证、大正文、容量不足，无解析/解码/生成或新 Hold；在 Provider 长等待期间查询数据库活跃连接/事务，确认不持有连接。

A4 包含同账户资金并发、同键指纹冲突、全部原状态投影、请求指纹密钥轮换、多副本 keyring 一致性和安全重试保留原 Hold。

A5 对提交前、提交中、句柄入库前、轮询、证据到达、结算前后逐处注入子进程强杀；验证只有可证未受理允许原请求内重试，未知接受不得重提。

A6 同时覆盖先持久化句柄再 poll、持久化失败及只读异常对账；成功/失败/取消/未知和缺证据分别判断，不恢复图片。

A7 使用本清单 transport、断开和收尾竞争用例，验证 timeout 不重写已确认账务，成功已收费但交付失败符合调用方错误说明。

A8 在多 API 副本下观察账户/渠道全局计数与本机资源上限；转对账只释放账户执行名额，未知渠道名额/Hold 保留；缓存通知丢失时不能选旧修订/停用候选。分别测 Provider 耗时、准备/数据库/编码/发送耗时和不同响应大小的峰值内存；SQL 数量不随等待时长周期增长。

A9 使用本清单第 4.2 节旧库升级用例，另覆盖旧在飞任务排空/转对账与容量登记及历史载荷清理证据。

A10 核对管理端/客户调用记录只依赖最小投影，字段无请求/图片；金额、跨 UTC 日、失败成本缺口、人工解除及正式调整沿用[账户资金 Spec](../specs/0002-account-funds-and-reservations.md)。

## 6. 证据记录要求

实现期记录固定提交、配置的非敏感上限、假 Provider 场景、实际执行命令、数据库前后核对及观测结果。只把实际通过的项目标为 PASS；未执行的项目标为待验证，环境或设计前提未具备则说明具体条件。此前小响应基准不能充当本清单大响应、H2 或迁移结果。


## 8. 唯一路径收敛的实施证据

本节记录[唯一路径整改设计](../design/0019-synchronous-gateway-single-path.md)的落地证据。基线 `606398ade1cd1c414320a5ec68cd295c88d5bf24`，改动未提交。

### 已落地

| 面 | 结果 |
| --- | --- |
| 开关 | `GENERATION_DIRECT_EXECUTION` 与 `direct_execution_enabled()` 已删除；入口中间件无条件挂载；启动与停机无条件建 Supervisor 并 drain |
| 旧执行栈 | `GenerationService`、`WorkerService`、旧 `HubRepository` 端口、`JobView`/`ClaimedJob`/`LeaseRecovery`/`CompleteJob`/`UnacceptedAttempt`/`AttemptFailure`/`HoldDisposition`/`CreateImageGeneration`/`ExecutionProtocol` 已删除 |
| Worker | 只跑异常对账；生成领取与 `WORKER_LEASE_SECONDS` 已删除 |
| 持久层 | 迁移 0038：三条守卫（旧协议在飞 Job、v1 取值面之外的 Job 或 Attempt、无 `idempotency_key_digest` 的行）→ 新增 `image_count` → 删除 10 个载荷/协议/领取列 → 删 `jobs_claimable` 并重建两条索引 → 摘要 NOT NULL → 收窄两个状态 CHECK |
| 静默行为 | 每日扣费上限移入 v1 受理（`DirectExecutionService::execute`，admit 之前）；用量状态口径认识 `admitted`/`executing`；产出张数由 `settle` 写入 `generation.jobs.image_count` 并由用量与账单读取 |

### 我实际跑过的命令与结果

| 命令 | 结果 |
| --- | --- |
| `cargo check --workspace --all-targets` | 无 error、无警告 |
| `cargo fmt --check` | 通过 |
| `cargo test -p seeai-api --test http_contract cases_migrations -- --ignored --test-threads=1` | 9 passed |
| `cargo test -p seeai-api --test http_contract cases_billing -- --ignored --test-threads=1` | 8 passed |
| `cargo test -p seeai-application --test execution_reconciliation -- --ignored --test-threads=1` | 18 passed |
| `cargo test -p seeai-persistence --test execution_late_facts -- --ignored --test-threads=1` | 5 passed |
| `cargo test -p seeai-application --test direct_execution -- --ignored --test-threads=1` | 15 passed |

### 实施期由子代理执行、我未逐条复跑的声明

`cargo clippy --workspace --all-targets --all-features -- -D warnings` 通过；`cargo test -p seeai-application --lib` 157 passed；`http_contract --ignored` 全套 189 passed；`direct_execution` + `execution_reconciliation` 32 passed；`seeai-persistence --ignored` 23 passed；迁移列集/索引/约束用 psql 在临时库原始核对。这些是执行方提供的证据，标记为待独立复核。

### 已知未完成（不计入本次收敛）

R2 候选计划（仍持有映射后的完整参数）、R1 对账独立上限与预算观测、T1 预留常数与上限组合校验、T7 其余验收取证，以及受理前 route 缓存的死路径清理。R4（`client_gone` 与 `ownership_lost` 分离、`cancel_unsubmitted`）见第 9 节；R5（晚到事实交接与可信失败的确定处置）、R6（Provider 身份类型收口）与 R7/T6（连接层发送期限与断开监视）见对应提交。

### 环境发现

迁移守卫用例会连续让迁移以守卫报错收场，而 sqlx 的迁移排他锁是会话级的：多连接池下下一次 `run` 可能落到另一条连接上被前一次留下的锁挡住，表现为测试空转。该用例改用单连接池后稳定通过（0.54s）。同类现象可能是此前"测试二进制空转 15 分钟"的原因。

修改已应用过的迁移会改其 SHA-384 校验和，sqlx 会以"migration 38 was previously applied but has been modified"拒绝启动。本次 0038 的 DDL 未变（只补了守卫），因此开发库按新文件哈希更新了 `_sqlx_migrations` 的校验和记录，未动业务数据；重建库的环境不受影响。

## 9. 取消事实分离与未提交取消的落地证据

本节记录[整改设计](../design/0018-synchronous-gateway-remediation.md) §4.1（R4）的落地证据：`DispatchGate` 上的 `client_gone`、`ownership_lost` 与 `generation_started` 三位分开，未提交的执行由带 fencing 的 `cancel_unsubmitted` 按"确定未提交"释放。

### 已落地

| 面 | 结果 |
| --- | --- |
| 状态机 | `seeai_adapter_sdk::DispatchGate` 三个事实各占一位，任一取消都停止新的上传/生成/重试/轮询；`try_begin_external_action` 与两次取消在同一个 compare-exchange 上线性化，两个原因同时成立时报 `ownership_lost` |
| 接线 | transport 的 `ConnectionScope::mark_client_gone` 只置 `client_gone`；续约 Conflict/数据库不可用/超时与停机置 `ownership_lost`（停机 `stop_all` 两位一起置，不声称本地未发送）；已发出的 Provider 调用仍由 Supervisor 的独立收尾路径按既有预算处置 |
| 端口 | `ExecutionRepository::cancel_unsubmitted`：同一事务释放 active Hold、账户占用与渠道容量槽位，把 `prepared`/`submitting` 的 Attempt 收成 `terminal`，Job 落 `failed` 并盖 `terminal_at`；`accepted`/`unknown` 的 Attempt 一律冲突，不为了释放新建 Attempt |
| 提交结果未知 | 取消接口的失败先按 `read_finalization` 只读确认（无 Attempt 的执行按 Job 阶段回答），确认不到再有界重试；确认不了就返回 outcome_unknown，不声称释放成功 |
| 收尾差异 | `client_gone` 与 token 仍有效时的 `ownership_lost` 走同一段带 fencing 的释放并回 504；发送先赢时按"可能已提交"转对账并保留占用；接管之后旧 token 的释放/结算一律冲突 |

### 我实际跑过的命令与结果

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --check` | 通过 |
| `cargo check --workspace --all-targets` | 无 error、无警告 |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | 通过 |
| `cargo test -p seeai-api` | 42 passed、6 ignored；http_contract 无库用例 4 passed |
| `cargo test -p seeai-application --lib` | 162 passed |
| `cargo test -p seeai-application --test direct_execution -- --ignored --test-threads=1` | 24 passed |
| `cargo test -p seeai-application --test execution_reconciliation -- --ignored --test-threads=1` | 20 passed |
| `cargo test -p seeai-persistence --test execution_submission -- --ignored --test-threads=1` | 9 passed |
| `cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1 cases_direct_execution` | 16 passed |

### 未跑通与待验证

- 提交前断开的端到端取证（真实 socket 在正文前/中/解析后/受理后断开）与 A5 的子进程强杀矩阵不在本次范围内，仍按第 2、5 节单独取证。
- 续约三类失败目前的证据是单元用例：容器的 `ownership_lost` 置位、以及被拒后零新增外部动作；真实数据库断开与超时下的端到端零新增调用仍待取证。
- 本节的 `direct_execution` 用例用假渠道与一次性库；没有调用任何真实 Provider。

## 10. 先查记录再解释请求（R9）的落地证据与 A1/A5/A10 取证

本节记录[整改设计](../design/0018-synchronous-gateway-remediation.md) §9.1（R9）的落地证据，以及本轮实际取得的 A1/A10 证据与 A5 的缺口。基线 `0ca365b`，改动未提交。

### 已落地

| 面 | 结果 |
| --- | --- |
| 查找位置 | 两条入口都先做完有界解析（JSON 走 `RequestParameters::parse_with_limits`，multipart 逐字段计数），再用账户与 `Idempotency-Key` 查记录；图片字段抽取、`model` 判定与分支判定、候选处理都在命中判定之后 |
| 命中判定 | `DirectExecutionService::lookup_recorded` 只读账户与键；命中后 `replay_recorded` 从**原始参数面**按记录冻结的 `capability_schema` 与 `request_digest_key_version` 重算指纹，一致才投影 |
| 缺材料 | `ExecutionLookup` 的请求指纹、密钥版本与冻结合同都是 `Option`：`lookup_execution` 改用 `LEFT JOIN catalog.vendor_models`，记录在而材料不在时返回 `Some`，由用例层按 `409 idempotency_conflict` 拒绝——不再返回 500，也不再当作未命中 |
| 未命中 | 只有 `lookup_recorded` 返回 `None` 才按当前合同解释这次请求；`execute` 内的同键预查仍保留，覆盖入口未命中之后、受理之前的并发 |
| 容量 | 命中判定读的是有界解析后的参数面，不为比对复制图片；本机执行/发送许可仍在未命中之后才预留 |

### 新增用例与结果

| 用例 | 结果 | 判别力 |
| --- | --- | --- |
| `cases_direct_execution::direct_replay_uses_the_recorded_contract_after_a_republish` | passed | 新修订要求新必填参数后，同键同正文仍按记录冻结的合同投影 `409 result_not_retained`；同一正文换新键则按当前合同回 `400 validation_error`，不建记录、不调上游 |
| `cases_direct_execution::direct_replay_without_comparison_material_is_a_conflict` | passed | 记录还在、`request_digest` 被抹掉时回 `409 idempotency_conflict`，不执行；改前这条路回 500 |
| `cases_direct_execution::direct_replay_is_checked_before_the_current_contract_interprets_the_body` | passed | 同键正文带一个非公网 URL、非 data URL 的图片值时回 `409 idempotency_conflict`（查找发生在图片字段抽取之前）；改前是 `400 invalid_parameter` |
| `cases_direct_execution::direct_replay_with_a_reference_image_uses_the_recorded_fingerprint` | passed | 同键重发带着记录里已受理的那份参考图值时仍命中同一条记录（图片形态判定发生在幂等预查之后，只对未命中的请求适用）：比对从原始参数面里摘图片字段，指纹取值与受理时逐字相同 |

### 我实际跑过的命令与结果

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --check` | 通过 |
| `cargo check --workspace --all-targets` | 无 error、无警告 |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | 通过 |
| `cargo test -p seeai-api` | 42 passed、6 ignored；`http_contract` 无库用例 4 passed、201 ignored |
| `cargo test -p seeai-application --lib` | 162 passed |
| `HTTP_CONTRACT_DATABASE_URL` 取自 `.env` 的 `DATABASE_URL`、`--test-threads=1`：`cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1 cases_direct_execution cases_funds` | 25 passed（20 direct_execution + 5 funds） |
| 同环境：`cargo test -p seeai-application --test direct_execution -- --ignored --test-threads=1` | 24 passed |
| 同环境：`cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1 aihubmix_sync_entries_accept_images_and_return_the_provider_envelope synchronous_gateway_migration_adds_minimal_facts_on_an_existing_database finished_requests_page_within_a_pinned_window` | 3 passed |

以上真库用例全部走进程内假上游，没有调用任何真实 Provider；`HTTP_CONTRACT_DATABASE_URL` 的值没有打印，也没有并行跑。

### A1

| 入口 | 上游形态 | 用例 | 结果 |
| --- | --- | --- | --- |
| `/v1/images/generations`（JSON） | `url` | `cases_direct_execution::direct_json_generation_returns_url_without_a_worker` | passed |
| `/v1/images/generations`（JSON） | `b64_json` | `cases_direct_execution::direct_json_generation_returns_base64_without_a_worker` | passed |
| `/v1/images/edits`（multipart 文件部件） | 同一条闭环 | `cases_aihubmix::aihubmix_sync_entries_accept_images_and_return_the_provider_envelope` | passed |

三条都不启 Worker：`200` 本身就是"这条路不依赖 Worker 生成队列或结果轮询"的判据。前两条各钉一种上游形态（`url` 原样交回、`b64_json` 与 `STANDARD.encode(PNG_FIXTURE)` 逐字相等）；第三条把两条入口放在同一个进程里跑通，另覆盖公网 URL 参考图、data URL 参考图与 multipart 文件部件三种输入形态。

生成入口已收敛：`/v1/images/edits` 与 `/v1/images/generations` 现在接受同一个 JSON 请求，multipart 正文不再被接受（[Spec 0005](../specs/0005-synchronous-image-gateway.md) v4）。上表的 `edits` 行记录的是收敛前那一次运行。

### A5

强杀与恢复矩阵**本轮没有跑**：仓库里没有"在提交前 / 提交中 / 句柄入库前 / 轮询中 / 取得证据后 / 结算提交前后注入子进程强杀"的用例（`harness.rs` 里的 `child.kill()` 只是夹具收尾），本机因此拿不出这一矩阵的任何一条子项，A5 仍待单独取证。

本轮跑到的是相邻的数据库故障注入，不是子进程强杀，只能算 A5 的部分前置：`cargo test -p seeai-application --test direct_execution`（24 passed）里的 `an_unknown_settle_commit_is_confirmed_instead_of_assumed_failed`、`the_same_key_replays_settled_failed_and_unknown_outcomes`、`an_unknown_acceptance_reconciles_and_keeps_the_hold`、`accepted_unpersisted_offers_late_facts_and_reconciles` 覆盖"结算提交结果未知先只读确认、不先假定失败"与"未知受理保留 Hold 并转对账"。

### A10

| 面 | 用例 | 结果 |
| --- | --- | --- |
| 记录字段面没有请求与图片 | `cases_migrations::synchronous_gateway_migration_adds_minimal_facts_on_an_existing_database` | passed：`generation.jobs` 在场的是 `idempotency_key_digest`、`request_digest`、`provider_task_handle`、`image_count` 等最小事实；`native_parameters`、`result_images`、`carrier_schema`、`parameter_mapping`、`idempotency_key`、`request_hash` 等载荷列已不在 |
| 客户读只依赖最小投影 | `cases_customer_history::finished_requests_page_within_a_pinned_window` | passed：客户用量视图按游标分页读已完成的调用记录（状态、金额、产出张数） |
| 对客响应没有内部记录 | `cases_direct_execution` 的 `direct_*` 各条 | passed：`assert_sync_success` 先跑 `assert_public_only`，钉住响应里不出现 job / offering / channel 等内部词汇，且每张图恰好只有 `url` 或 `b64_json` 一个字段 |

管理端与客户的用量、成本、金额读（`cases_cost_facts`、`cases_billing`、`cases_customer_history` 其余各条）**本轮没有跑**。

### 未跑通与待验证

- A5 的子进程强杀与恢复矩阵：没有用例、本轮没跑，仍按第 2、5 节单独取证。
- A1 的 `edits` 入口只跑了返回 `url` 形态那条端到端用例；`edits` 配 `b64_json` 的闭环没有单独用例，本轮的 base64 证据来自 JSON 入口那一条。
- A10 的管理端用量/成本读与金额口径本轮没跑（`cases_cost_facts`、`cases_billing`）。
- `direct_replay_uses_the_recorded_contract_after_a_republish` 走通的是 Application 层已有的"按记录冻结合同投影"路径，改前也会通过；本轮真正判别的两条是缺材料 409 与查找先于图片解释。

## 11. A5 子进程强杀矩阵的实施证据

本节记录 [Spec 0005 §8](../specs/0005-synchronous-image-gateway.md) A5 的进程级强杀取证。用例在 `apps/api/tests/http_contract/cases_kill_matrix.rs`（7 条，全部 `#[ignore]`），基线 `7923fb4`，改动未提交。

第 10 节缺的是"运行到某一格再真的 SIGKILL"那一半，这里补上；恢复那一半复用第 10 节的数据库级用例。恢复跑的是生产同一份 `ExecutionReconciliationService` 与真渠道 Driver（`AdapterRegistry` 装 AIHubMix / APIMart 工厂），在测试进程里对着被杀进程的那个一次性库。验收只连进程内假上游，没有调用任何真实 Provider。

### 夹具与用例

| 面 | 结果 |
| --- | --- |
| 强杀用例 | 7 条：提交前两格（受理后未写提交声明 / 已写提交声明未发生成请求）、提交中、接受后句柄未写入、轮询中、取得证据后与结算提交前、结算提交后 |
| 假上游闸门 | `UpstreamGate`：命中 `HeldRequest::{Upload,Create,Query}` 时先记到达再停住，用例 `wait_for_arrival` 等到"请求真的发出去了"才杀 |
| 查询序号 | 假上游的查询序号改在读完请求时定下，闸门停住响应不改变"被杀那次查询"与"恢复那次查询"各拿第几个状态 |
| 假 Redis 闸门 | `BalanceWriteGate`：武装后下一条 `SET user_balance:` 停住、永不应答；`CacheSettings.operation_timeout_ms` 可把缓存命令上限调长到等得起 |
| SIGKILL | `ApiProcess::sigkill`：`Child::kill()`（Unix 上是 SIGKILL），并核对退出状态记的确实是信号 9 |
| 夹具自检 | `harness_check::the_upstream_gate_signals_arrival_before_releasing`：不启平台进程、不用库，随 workspace 单测跑 |

### 注入方式与屏障

| 手段 | 证明的事实 | 为什么不是 sleep 猜时间 |
| --- | --- | --- |
| 假上游闸门 | 上游真的收到了目标请求（上传 / 生成提交 / 任务查询），且响应还没写回 | 到达计数由假上游在读完请求时置位，用例等在 `Notify` 上 |
| 数据库表锁 | 受理已提交、`begin_submission` 还没写出提交声明 | `LOCK TABLE generation.attempts IN ACCESS EXCLUSIVE MODE` 挡住 INSERT；信号是 `pg_stat_activity.wait_event_type = 'Lock'` |
| 数据库行锁 | `record_acceptance` 或 `settle` 已经走到、但提交不了 | 用例先持 Job 行 `FOR UPDATE`；信号同样是锁等待 |
| 缓存写回闸门 | 结算已提交、`refresh_balance` 写回未回 | 假 Redis 在收到那条 `SET user_balance:` 时置信号并永不应答 |
| 死进程后端清理 | "崩溃即事务未提交" | SIGKILL 后 `pg_terminate_backend` 掉仍等在锁上的那条后端，再放掉用例自己的锁；否则后端会在锁放行后替死进程把事务提交掉 |

### 逐格结果

杀进程前的库内事实是用例在屏障信号之后读到的真实行；生成计数是假上游记录的 `POST .../images/*` 次数。

| 格 | 用例 | 屏障信号 | 杀进程前的库内事实 | 恢复动作 | 恢复后断言 | 结果 |
| --- | --- | --- | --- | --- | --- | --- |
| 提交前（受理后未写提交声明） | `sigkill_before_the_submission_declaration_reaps_the_orphan_admission` | `attempts` 整表锁 + 锁等待 | Job `admitted`、无 Attempt、生成 0、上传 0 | 终止等锁后端 → 放锁 → Worker 一轮 | `reaped_orphans = 1`；Job `failed`、Attempt 0；Hold 释放为 0；渠道槽位 `released`；对账案例 0；生成 0；同键重发（另一个 API 副本）`502 platform_unavailable` 且生成计数仍 0 | passed |
| 提交前（已写提交声明、生成请求未发） | `sigkill_with_the_submission_declared_but_before_the_create_request_keeps_the_hold` | 上游停住 APIMart 的 `POST /v1/uploads/images`（夹具先上传参考图换 URL） | Job `executing`、Attempt `submitting`、无句柄、生成 0、上传 1 | 杀 → 放行闸门 → 推租约过期 → Worker 一轮 | `reconciled = 1`；Job `reconciliation_required`、Attempt `unknown`；对账案例 1；Hold 原样保留、渠道槽位 `held`；生成 0、查询 0、capture 0 | 退役 |
| 提交中／未知接受 | `sigkill_while_the_create_response_is_missing_keeps_the_hold_and_never_resends` | 上游停住生成请求的响应 | Job `executing`、Attempt `submitting`、无句柄、生成 1 | 同上 | `reconciled = 1`；`reconciliation_required` / `unknown`；案例 1；Hold 保留、槽位 `held`；capture 0；生成计数仍为 1、查询 0 | passed |
| 接受后句柄未写入 | `sigkill_after_acceptance_before_the_handle_is_stored_keeps_the_hold` | 上游先停住生成请求 → 锁 Job 行 → 放行提交应答 → 锁等待（`record_acceptance`） | 无句柄、查询 0、生成 1 | 终止等锁后端 → 放锁 → 推租约过期 → Worker 一轮 | `reconciled = 1`；`reconciliation_required` / `unknown`；案例 1；Hold 保留、槽位 `held`；capture 0；生成 1、查询 0 | passed |
| 轮询中（终态未回） | `sigkill_while_polling_settles_once_on_recovery_without_resubmitting` | 上游停住第一次任务查询 | Job `executing`、Attempt `accepted`、句柄已入库、生成 1 | 杀 → 放行闸门并等那次挂起查询走出闸门 → 推租约过期 → Worker 一轮（按句柄只读查同一任务） | `taken_over = 1`、`settled = 1`；Job `succeeded`、Attempt `terminal`；capture 1；Hold 0；槽位 `released`；案例 0；生成 1、查询 2（被杀进程 1 次 + 恢复 1 次） | passed |
| 取得证据后／结算提交前 | `sigkill_after_the_evidence_arrives_before_the_settlement_commit_settles_once` | 上游停住第一次查询 → 锁 Job 行 → 放行带证据的终态 → 锁等待（结算事务） | 句柄已入库、查询 1、capture 0 | 终止等锁后端 → 放锁 → 推租约过期 → Worker 一轮 | `taken_over = 1`、`settled = 1`；`succeeded`；capture 1；Hold 0；槽位 `released`；案例 0；生成 1、查询 2 | passed |
| 结算提交后 | `sigkill_after_the_settlement_commit_replays_as_result_not_retained` | 上游收到 AIHubMix 生成请求 → 武装余额写回闸门 → 放行 → 等写回被停住 | capture 1、Job `succeeded`、Hold 0（提交已落、响应未交） | 杀 → 另起一个 API 副本连同一库 → 同键同正文重发 | `409 result_not_retained`；capture 仍为 1；生成计数仍为 1 | passed |

「提交前（已写提交声明、生成请求未发）」一格**退役**，不按 passed 计：它的屏障是 APIMart 的内联上传（`POST /v1/uploads/images`），生成入口收敛为只收公网 URL 后这条通路连同夹具的上传闸门一起删除（[设计 0021](../design/0021-object-storage-upload.md) §2），该状态在进程外没有可钉的屏障。该格要证的恢复结论由「提交中／接受后句柄未写入」格承担。

### 我实际跑过的命令与结果

真库串取自 `.env` 的 `DATABASE_URL`（没有打印），一律 `--test-threads=1`、不并行。命令都在仓库根执行。

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --check` | 通过 |
| `cargo check --workspace --all-targets` | 无 error、无警告 |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | 通过 |
| `cargo test -p seeai-api --test http_contract` | 5 passed、208 ignored（含新的闸门自检） |
| `cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1 harness::cases_kill_matrix` | 7 passed；多次连跑稳定，每次约 10–11s（其中「提交前（已写提交声明、生成请求未发）」一格此后退役，见上） |
| 同环境 `cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1 harness::cases_direct_execution` | 20 passed（86.33s），无回归 |
| 同环境 `... -- --ignored --test-threads=1 harness::cases_apimart harness::cases_retry` | 7 passed（12.71s）；假上游查询序号与调用计数是改动面 |
| 同环境 `... -- --ignored --test-threads=1 harness::cases_cache` | 12 passed（22.55s）；假 Redis 与 `CACHE_OPERATION_TIMEOUT_MS` 是改动面 |

7 格中有 6 格符合 §5，余下一格（提交前、已写提交声明、生成请求未发）随生成入口收敛退役、不再取证（见上）；本轮没有抓到"恢复后重发"或占用错误，因此没有需要裁决的产品缺陷与最小复现。

### 未覆盖与待验证

- 结算提交中（COMMIT 应答丢失、提交结果未知）没有进程级强杀：要在提交那一瞬注入得让 `COMMIT` 的应答在网络里丢掉或连接被切断，进程内假上游与行锁都做不到稳定复现。相邻证据仍是数据库级的 `an_unknown_settle_commit_is_confirmed_instead_of_assumed_failed` 与 `an_unknown_acceptance_reconciles_and_keeps_the_hold`（`crates/application/tests/direct_execution.rs`），那是注入数据库错误、不是子进程强杀。
- "上游已接受、提交响应在网络里丢失"没有单独格：从平台看它与格 2（响应没回来）同属接受状态未知、走同一分支，要分开需要网络层丢包注入。
- 句柄相关的三格只覆盖任务式渠道 APIMart：同步渠道 AIHubMix 没有任务句柄（§3 的"先存句柄再轮询"对它不适用），这三格无法造；格 6 用的正是 AIHubMix。
- A5 的"只有可证明未受理的失败允许重试"只取证了一半：矩阵证明的是"强杀恢复不重发生成请求"；原请求内的安全重投（证明未受理后重投同一候选、保留 Hold 与容量）由 `cases_retry` 与 `crates/application/tests/direct_execution.rs` 的相邻用例覆盖，本轮没有为它新增强杀用例——进程都死了，原请求不存在。
- 观察记录：格 1a 里孤儿受理被回收后 Job 收成 `failed`、库里没有原平台错误码，同键重发按既有回退口径投影 `502 platform_unavailable`，上游计数仍为 0。若要把"可证未受理的键允许重试"扩展到崩溃后的同键重发（重新受理而不是投影失败），那是新的合同决定，本轮不改产品代码。
