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

R5 晚到事实交接余下（同步成功、明确失败与结算确认失败尚未交接；`provider_state=Failed` 仍进对账而非按确定失败释放）、R2 候选计划（仍持有映射后的完整参数）、R1 对账独立上限与预算观测、R6 `ProviderTraceId` 类型入口、R4 `client_gone` 与 `ownership_lost` 分离、T6 发送期限与断开监视、T7 其余验收取证，以及受理前 route 缓存的死路径清理。

### 环境发现

迁移守卫用例会连续让迁移以守卫报错收场，而 sqlx 的迁移排他锁是会话级的：多连接池下下一次 `run` 可能落到另一条连接上被前一次留下的锁挡住，表现为测试空转。该用例改用单连接池后稳定通过（0.54s）。同类现象可能是此前"测试二进制空转 15 分钟"的原因。

修改已应用过的迁移会改其 SHA-384 校验和，sqlx 会以"migration 38 was previously applied but has been modified"拒绝启动。本次 0038 的 DDL 未变（只补了守卫），因此开发库按新文件哈希更新了 `_sqlx_migrations` 的校验和记录，未动业务数据；重建库的环境不受影响。
