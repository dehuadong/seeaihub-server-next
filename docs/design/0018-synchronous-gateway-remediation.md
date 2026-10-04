主题: 同步图片网关的资源、收尾与传输补充设计
当前修订: v1
生效修订: v1
状态: 已接受
承接: [同步图片网关 Spec v2](../specs/0005-synchronous-image-gateway.md) §2–§8（§6 与 A11 为本次新增传输与部署边界）
依赖: [同步网关设计 v1](0017-synchronous-image-gateway.md)、[定价与结算](0007-pricing-floor-and-settlement.md)、[账户资金](0013-account-funds-and-reservations.md)

# 同步图片网关整改设计

本稿补充既有同步网关设计的接口与实现边界，不替代 Spec；资源和传输涉及 API 与 Adapter，集中在本稿说明这些跨模块约束。既有受理、路由、计价和数据库事务继续由依赖设计拥有。

## 1. 接口与职责

| 属主 | 接口与责任 |
| --- | --- |
| API transport | 从请求头进入服务开始建立请求存活信号；管理连接任务、客户端发送期限及 transport 实际持有的资源；将断开通知 Supervisor。 |
| API Supervisor | 持有读取、执行、字节及发送许可；监督独立执行；将所有权状态与调用方存活分别通知 Application；任何续约失败停止新的外部动作。 |
| Application | 先查幂等记录、按记录冻结的合同解释重发，再选路；原子受理；提交动作前检查资格；保留已取得账务事实并有限收尾；所有权失效后只投递晚到事实。 |
| Domain / SDK | 有界 Provider 标识、可核验计量与明确的 Provider 任务状态；共享图片输入；候选计划不包含图片副本。 |
| Persistence | 幂等身份查找与原子受理、带 token 的状态变更、受信任且有界的晚到事实接收；约束同 Attempt 收件冲突与行数。 |
| Worker | 仅查询原任务或消费最小事实；成功、失败、取消、未知分别处置；不恢复图片、不重提交生成。 |

执行许可与响应发送许可是两个概念。执行任务结束后，只有 transport 仍持有的结果、缓冲及发送许可继续占用；连接关闭不自动释放未知费用的 Hold 或渠道容量。

## 2. 预算必须覆盖实际持有的内存

### 2.1 一份配置产生所有字节上限

用 `GatewayByteLimits` 统一入口、Adapter、归一输出和发送限制，取消与真实响应无关的固定 32 MiB 执行预留。已支持的 16 MiB 请求及 AIHubMix 128 MiB Provider 响应仍是预算输入，不通过把输出缩到 32 MiB 掩盖缺陷。对账读取使用单独、更小但明确的上限，超限只留下证据缺口。

预算分阶段计算。令 `I` 为请求 wire 上限，`A` 为请求解析结构最大占用，`X` 为所选 Adapter 输入转换与上传占用，`U` 为 Provider wire 上限，`P` 为 Provider 解析结构最大占用，`C` 为客户端编码上限，`T` 为 reqwest/Hyper 用户态缓冲及有界连接开销：

```text
R_read = I + A + 读取/图片解析暂存
R_execute = 保留输入 + X + U + P + T
R_encode = 保留输入/输出 + C + 编码暂存 + T
R_send = C + T
R_request = max(可实际重叠的阶段之和)
本机最坏占用 <= 读请求预留 + 在飞执行预留 + 发送预留 + 有界连接开销 + 常驻内存
```

阶段转换采用先取得下一阶段所需预算、再释放上一阶段不再拥有的数据和预算的规则，过渡期同时存活的数据也计入。任何等待均有绝对期限；需要扩大预留但无法立即取得时，在受理前返回容量不足，不能先取得 Hold 再无限等待内存。最简单的首期实现是受理前保守预留 `R_request`，发送结束才释放；后续只在测量证明不重叠时拆分释放。

每个执行至少同时容纳原始响应和解析出的图片字符串。编码期间还可能存在归一结构与客户端正文，不能只计一个 128 MiB 缓冲。128 MiB 原始缓冲加 128 MiB 图片字符串加 128 MiB 正文已达 384 MiB，尚未包含输入、容器及 transport；这个数字是反例下界，不是最终预算。保守编码上界可先用 `C = 6U + 有界信封` 覆盖 JSON 转义膨胀；经验证的 URL/base64 字符约束可以使该上界更小，但不得用未验证的固定倍数充当保证。

### 2.2 结构与隐藏副本

请求解析对容器层数、节点、字段数和累计字符串字节计数。计数上限从支持的 wire 输入范围推导，默认应覆盖既有请求范围；如需缩小已支持范围，须先修订调用方限制说明并评审。不能先构造无界 `Value` 再检查节点数。使用带计数的反序列化 visitor 或等价的有界解析入口；对象/数组预分配、字符串 capacity 和扩容峰值也纳入上界。Provider 使用只抽取已知字段的类型，不为未知字段保留完整 `Value`。

图片输入在解析层与记录比对里保持三态（公网 URL / 内联 data URL / 文件字节 `Bytes`）：URL 与 data URL 是共享不可变字符串，文件字节保持 `Bytes`；三态都进新执行，生成入口收敛为只收公网 URL 的合同见 [Spec 0005](../specs/0005-synchronous-image-gateway.md) §3，随实现落地。执行重试重建请求体，不深拷贝图片。fingerprint 逐项规范化写入摘要，不产生完整 canonical 大 JSON。wire 序列化写入有界 buffer/stream；扩容前检查上限，不先生成超限 `Vec` 再拒绝。结果字符串移动到归一结构，结算事实不 clone 图片。

连接数、H2 同时流数、frame/写缓冲和后台任务数也必须有界，否则 `T` 无法计算。TCP 内核发送缓冲单独计入部署内存/连接容量，不把它冒充已送达客户端的数据。

### 2.3 启动与观测

启动校验预算能容纳至少一个最大请求的保守阶段占用，并校验读取、执行、发送与连接上限组合。容量不足给出明确配置错误，不静默把最大请求改小。发布参数按本机可用内存计算安全并发；保留 128 MiB 输出会使小内存实例的安全并发明显下降。

观测记录已预留字节、实际缓冲字节、活跃读取/执行/发送、连接数、拒绝次数及阶段，不记录图片或任意参数。验收同时看预算计数和峰值 RSS；RSS 还包含 allocator 保留和常驻内存，不能要求它等于 payload 长度，也不能只看预算计数宣布内存受控。

## 3. 候选只做承载判定，映射只做一次

请求经适用合同处理后生成借用普通参数、图片数量及分支的 `RequestFeatures`。每个候选产生 `CandidatePlan`：候选引用、承载结论、缺失项、有界原因、默认值/rename/size/enum 映射计划、该候选的 `n` 截断结果；计划不持有图片值或映射后的大 JSON。

候选判定仍覆盖原有全部语义：必填、承载参数、默认注入、尺寸转换可行性、enum 映射可行性、分支、参考图数量和候选较小 `n` 上限。它不能只检查图片数量，也不能额外引入通用类型或区间校验。路由对完整合格候选集使用现有 priority/weight/discount/user-tag 规则，保留完整轻量判定记录。

选中后 `materialize_selected(plan, shared_input)` 构造该候选的 `GatewayInput`。输入图保持共享引用，普通参数最多为选中候选构造一次。实际映射仍发现不能承载时，在受理前按既有机制排除该候选并重选；没有 Hold 或 Provider 副作用。在发过生成请求后不得换候选。候选与 Runtime Revision/活动开关的数据库核验沿用既有设计。

## 4. 断开、提交与所有权状态

### 4.1 分离三个状态

用可唤醒的信号区分 `client_gone`、`external_actions_stopped` 和 `ownership_lost`，不再让一个 boolean 同时表示所有事实。Supervisor 监听结果接收端关闭，以及 §8.2 的可靠 transport 关闭信号。入口在接收请求头时安装 guard 并固定 `received_at`，总期限从这里计算，覆盖正文读取、参数处理、受理和等待，不在 spawn 或 Adapter 调用时重新起算；只监听执行结果 oneshot 不足以覆盖提交前断开。

Application 在受理前、`begin_submission` 前、每次 upload/submit/retry/poll 前检查相应资格。断开已经发生且可以证明本进程没有开始生成发送时，不再调用 Provider。新增 fenced `cancel_unsubmitted(job_id, owner, token)` 端口，覆盖受理已提交但尚无 Attempt 的情况，原子释放 Hold/执行与渠道容量并保存确定未提交结论；有 Attempt 的未发送取消通过相应失败收尾。不能为了释放而先造提交中的 Attempt。数据库事务不随 handler Drop 被直接取消；COMMIT 结果未知先确认，不能声称释放成功。

生成发送的开始由 `DispatchGate` 原子决定：取消信号与 `try_begin_external_action` 共用短锁/CAS，开始资格检查与标记不可分离。Adapter 完成可取消的参数/body 构造后，在第一次 poll 实际 send future 之前进入 gate；gate 不跨数据库 await 持锁。取消赢得 gate 时证明没有生成发送；send 赢得 gate 后即使紧接着发生取消，也只能按可能提交处理，不能承诺此后绝不发送。上传、只读 poll 与可能产生生成费用的 submit 分别标记，预提交取消是否能释放取决于是否存在生成发送，不取决于是否曾上传。Adapter 错误明确区分 `CancelledBeforeExternalSend` 与已可能发送的取消，不再将全部 Cancelled 自动映射未知。

`begin_submission` 持久化之后、生成发送开始之前，当前 owner 若观察到断开仍能证明本地未发送，可以按确定未提交释放；此时进程崩溃则数据库只有提交声明，Worker 必须保守转对账。停止提交与数据库事务不可能组成 Provider 共同事务，这个崩溃窗口不能消除。

### 4.2 任何续约失败停止新动作

续约失败包括 Conflict、数据库不可用及超时。任何一种失败立即锁住 `DispatchGate`，停止新的上传、生成、重试、轮询；续约调用本身有绝对超时，不等到下一轮才取消。资格判断还检查已知租约到期时刻，运行时调度延迟不能无限延长资格。续约后即使数据库恢复，也不自动恢复已经停止的生成动作。

取消 reqwest future 不能证明 Provider 未接受。已开始的动作可以在原有严格读取预算内提取已经到达的句柄或计量事实；禁止因此启动下一次 poll。收到接受句柄后即使取消，也必须有限尝试 `record_acceptance` 或 `offer_late_facts`，不能把停止 poll 当作拒收已有句柄。`select!` 不应直接丢弃已读到的成功结果。执行停止后进入独立的、有绝对结束时刻的 finalization budget：只做数据库确认、受当前 token 允许的收尾或晚到事实接收，不再产生 Provider 副作用。

已结算成功不因 client 断开或续约失败改为未收费；未开始生成且释放确认后是确定未提交；生成可能发生但无法确认结果时保留 Hold 与未知渠道容量，转对账。

## 5. 成功事实在所有权切换后仍能交接

### 5.1 事实类型

提取 `FinalizationFacts`，包含内部 Job/Attempt 身份、Provider 的成功/失败/取消状态、有效计量证据、产出张数、成本事实与合法 Provider 标识。它不包含图片、完整响应、用户字段或任意 JSON 扩展。响应摘要使用固定长度摘要类型。收件包含 Provider 终态，不能仅凭“有 usage”推断成功。

Application 持有这份小事实直到确定结算完成或晚到事实已可靠接收。常规成功先按当前 token 结算；提交结果未知先读取该 Job/Attempt 的账务事实。只有 `Succeeded` 且匹配这次结算事实的提交才可返回正常成功；读取到 `reconciliation_required` 不意味着成功事实已经保存，更不能据此停止交接。

token Conflict、所有权丢失、有限结算确认失败或当前处置无法正式结算时，在 finalization budget 内调用 `offer_late_facts`。这个端口允许原提交者 token 过期，只接收与原 Attempt 可信关联的最小事实，不允许改所有权、重开终态或按旧 token 扣费。句柄写入失败、同步成功、明确失败成本均走同一交接规则，不仅处理 APIMart 句柄。

### 5.2 可信身份与可靠收件

`begin_submission` 原子生成与该 Attempt 绑定的随机收件凭据，数据库只保存其摘要，执行上下文持有原值；晚到提交验证凭据、Job/Attempt/Provider 关联和证据 Attempt。凭据不是环境变量，不进入日志，不授权正式结算。既有已在飞 Attempt 没有此凭据时先排空或转现有对账，不伪造身份。

收件按 `(attempt_id, fact_kind)` 串行，并校验事实内容摘要。完全相同的投递幂等；合法单调补全可以更新同一条最小记录；互相矛盾的终态、句柄、计量或成本建有界冲突案例并冻结自动消费者扣费。每 Attempt 每种事实只保留一个规范记录及一个有界冲突摘要，防止不同摘要不断插入形成无界 inbox。收件事务与消费方的 token/状态变更竞争仍由数据库锁及幂等结算保护。

消费方结算提交后再标记已消费，崩溃重领不会再 capture。Worker 只有当前有效 token 才能正式收尾；没有所有权时保留未消费事实。失败已终结或人工解除后来的成功事实不重开消费者账务，只补成本或建冲突案例；成功已终结时校验相同事实后消费，矛盾事实需建案，不能盲目吞掉。

### 5.3 无法消除的窗口

数据库长期不可用或进程在事实提取与收件提交之间被强杀时，不能在禁止其他持久通道的条件下保证零丢失。已知任务句柄可只读对账补事实；AIHubMix 无查询能力时保留人工缺口。这是既有 Spec 允许的崩溃边界。必须对“有机会收件但代码丢弃”的缺陷作修正，不把该不可消除窗口当作免做收件的理由。

## 6. Provider 标识与账务数据只接受有界类型

`ProviderTaskHandle` 和 `ProviderTraceId` 的构造器按 Adapter 白名单验证字符、字节上限及实际供应商标识格式。首期候选上限为 128 ASCII 字节；最终格式以仓库渠道事实及合法 fixture 核定。允许的字符集合需要排除 URL、data URL、控制字符和任意正文；仅做长度限制或检测 `http` 不足以证明是标识。trace 标识不能被日志提前打印。

无效可选 trace 直接丢弃，只留下平台生成的分类。已接受任务的必需 handle 无效时不保存原值，不继续按原值 poll，不再次生成，转未知对账及有界告警。不得截断 handle 后查询另一任务，也不得将 handle 哈希当作可查询句柄。新协议的错误 `tid`、header request-id、成功与查询路径使用相同类型入口。

Persistence 在 `record_acceptance`、`settle`、`fail_or_reconcile`、`offer_late_facts` 及成本补录入口再次校验；SQL 对各处 task/trace 字段设置字节长度、格式约束。Currency、摘要、证据来源及计量字段也用枚举/定长/范围约束，JSONB 证据只序列化已知类型且有总字节上限。账户字段或通用 `String` 不能绕过类型构造器进入这些列。

新增迁移前扫描旧值，只输出内部行标识、长度与违规类别。历史非法 trace 清为 NULL，非法 handle 清为 NULL 并将相关未结清执行置为人工对账；保留 Job、Attempt、证据、账本和 Hold。确有业务载荷的历史值须按既有备份/WAL/日志清理机制取得证据，不能用加 CHECK 宣称历史载荷已消失。

## 7. Provider 失败终态不得成功收费

SDK 用穷尽状态替代 `AccountingQuery.terminal: bool`：

```rust
ProviderTaskState = Pending | Succeeded | Failed | Cancelled | Unknown
AccountingQuery = { state, accounting_facts: Option&lt;AccountingFacts&gt; }
```

未知供应商状态映射 `Unknown`，不得默认成功。APIMart 的 `completed`、`failed`、`cancelled` 分别映射明确终态；状态及原 task handle 的可信关联是计量的一部分。查询及晚到事实共享这一判据。

| Provider 事实 | 消费者收尾 | 平台成本 |
| --- | --- | --- |
| 成功 + 有效证据 + 原 Attempt 关联 | 按冻结价格至多一次 capture、释放 Hold 与终态渠道名额 | 冻结成本/汇率口径 |
| 成功但证据缺失、非法或关联不可信 | 保留资金占用，进入对账；不估算消费者扣费 | 已知成本保存，其余明确缺口 |
| 可信失败/取消，即便带 usage | 按既有失败规则释放消费者 Hold 与渠道名额，实收为零 | 上游声明成本按已有失败成本口径记录；没有可靠金额时用 unavailable，不伪造零成本 |
| Pending / Unknown | 不 capture、不重新生成；按原预算继续只读查询或保留对账 | 保存已核实成本，不拿 cost 推断成功 |

`SettleExecution` 必须携带非可选的计量证据，Repository 按 Job 状态拒绝不可结算的结算请求：`admitted` 与已终结的 `failed` 都不能被收成成功。明确失败通过 `fail_or_reconcile(DeterminedFailure)` 收尾，不调用 `CostInputs::Succeeded`。状态冲突走有界案例，不靠最后写入覆盖。

## 8. 客户端发送期限由连接层执行

### 8.1 连接层控制

图片响应结算确认后才交给 transport，并在交接时设置绝对 `send_deadline`。deadline 不因部分写成功、body 被 poll、重试或流量波动重置。到期后，即使 Hyper 不再 poll body，也必须停止写、销毁相应 transport 持有的缓冲，并释放其持有的结果与许可；超时不改变账务事实。200 已经发出后不能改发 JSON 错误，客户端会看到截断/连接终止，同键再请求仍按原事实投影。

现有 `axum::serve` 封装不提供每响应 socket 完成控制；实现采用 API 自管 `TcpListener` accept loop、Hyper connection future 和连接 registry，将 Axum Router 通过公开 service 适配接口接入。Tokio `Sleep` 由连接管理任务主动 poll，deadline 到达时销毁 connection，而不是只在 body 的 `poll_next` 中读时钟。

HTTP/1 图片响应设置 `Connection: close`。连接 registry 持有响应/发送许可，直到该 connection 完成正常 flush 并退出，或期限到达后销毁。非图片路由保留现有 keep-alive 行为。写缓冲、最大 header、连接数和任务数都需明确上限。

HTTP/2 保留协议支持，首期采用连接级绝对关闭：该连接任一图片响应进入发送阶段，就设置该响应 deadline；多个图片响应取最早截止时刻。permit 和预算由 registry 保守持有至连接销毁；到期关闭整连接。Hyper auto builder 使用每连接自定义 `hyper::rt::Executor`，将创建的 stream/service future 归属有限容量的 `JoinSet`；持续回收已完成 task handle。任务组满时拒绝新 task 并关闭连接，不开队列。关闭顺序为设置 closing fence、禁止 spawn、取得任务组并 abort、销毁 connection/IO、join 至空、移除关闭监视、最后释放 registry。只发 abort 或 drop 主 connection future 均不能宣布资源归零。图片路由禁止 Upgrade/CONNECT，以免独立持有 IO 的任务绕过跟踪；其他支持 upgrade 的路由必须纳入其既有生命周期管理。

HTTP/2 关闭会同时终止该连接其他流，这些执行按各自是否可能提交的事实继续有限收尾。对客能力没有每流发送隔离保证，但该影响必须在调用方运维说明中明确并作为本草案审批内容。若要求保留同连接其他流，则需另行验证 h2 逐流队列、RST 与完成确认能力；不能用 Body drop 假称已有该保证。

### 8.2 请求断开的可靠信号

Hyper 的 HTTP/1 pending service/已有 pipeline 缓冲可能不再读取 socket，因此只靠 service future Drop 检测 FIN 有盲区。首期直接执行限定 Linux：accept 后、Hyper 读取前，通过 `rustix` 安全公开接口 duplicate CLOEXEC fd 并向进程唯一 epoll monitor 注册，收到注册确认才启动 connection。monitor 只监听 `RDHUP | HUP | ERR | ONESHOT`，不监听普通 IN、不读取 HTTP 字节；可观察对端关闭而不与 Hyper 抢读。HTTP/2 单流取消另外由 pending service future 的 RST/drop 信号通知该请求。

monitor 使用一个专用线程、固定 event batch、连接上限、有界控制队列和 eventfd 唤醒。单调连接 ID 防 fd 重用旧事件误取消；收到关闭事件先置取消并通知 owner，再 DEL/drop duplicate，避免重复 HUP 热循环。正常关闭也必须 DEL/drop duplicate 并确认，保留 duplicate 会延长 socket 生命周期；发送截止先对共享 socket `shutdown(Both)`，随后执行 connection/task group 清理。控制队列为 cleanup 留出容量；注册失败不接纳直接执行，monitor 致命故障关闭相关连接并暂停新准入。

FIN 按与 Hyper `half_close(false)` 一致的策略终止连接，不把关闭写半边继续等待响应解释成仍存活。正常 service future 返回 Response 时，request guard 转交响应/连接阶段，不能因 Ready 后 Drop 误报断开。静默网络失联由原有 deadline 收口；反向代理若不转发客户端关闭，API 不能声称已感知终端离开，部署验收须检查代理取消传播。

非 Linux 启用直接执行时给启动配置错误，不回到旧载荷持久化路径；这是本草案待确认的部署约束。新增直接依赖 `hyper`、`hyper-util` 的公开 server/service 接口及 `rustix` event/net 功能，不修改依赖私有内部或写 unsafe；具体锁定版本与 Rust 编译验证在实施期完成。`rustix` 的 [epoll API](https://raw.githubusercontent.com/bytecodealliance/rustix/v1.1.4/src/event/epoll.rs)与 [Linux 关闭事件语义](https://man7.org/linux/man-pages/man2/epoll_ctl.2.html)可支撑上述机制；这些来源只证明接口/OS 行为，不证明本项目 transport 集成已通过。

### 8.3 发送阶段资源

归一输出先写入有界编码 buffer。发送拆成有界 chunk，限制 Hyper 每次取得的块大小，但 chunking 本身不构成发送期限。响应 body EOS 只表示应用不再产生块，不能认为 socket 已发送完或内核缓冲已被客户端读取。registry 持有最终许可，body 可以仅持其共享句柄；只有 transport 缓冲和关联任务销毁后才释放。

正常发送完成的定义是服务端用户态写缓冲完成写入并终止相应 transport 生命周期，不是客户端确认已保存图片。HTTP/2 首期没有每流 flush 证明，因此即便早已交完 DATA 也保守保留许可至连接关闭；这会降低长连接吞吐，是安全上界的明确代价。

## 9. 幂等身份与合同冻结

幂等键的身份是**无密钥** SHA-256（固定领域前缀 `seeai/idempotency-lookup/v1` 后接键），只要求跨 API 副本稳定与不可逆，不读密钥、不轮换、不需要环境变量；仓库当前实现即此。同账户同身份的受理由数据库唯一约束串行，事务内先查后建，命中返回原记录。

请求指纹是另一件事：它用按版本轮换的 HMAC 密钥 `REQUEST_FINGERPRINT_KEY_V<n>`，避免库泄露后从摘要反推提示词或图片内容。开启直接执行时必填，多副本一致，轮换时保留旧版本密钥；这是当前合同，不是历史兼容。

本阶段没有需要延续的旧摘要数据，因此不引入历史查找密钥、不建摘要别名表、不为算法身份不明的账户暂停准入，`IDEMPOTENCY_LOOKUP_KEY` 不回到配置。若将来接入任何运行过旧 HMAC 幂等代码的数据库，那些摘要在没有明文键或原密钥时无法重新匹配，属于一次性数据处置（重置或拒绝），不作为长期配置项，也不删除账本或最小幂等记录。

### 9.1 先查记录再解释请求

查找在当前合同的图片字段抽取、默认值和候选处理之前完成；API 仍必须有界解析 JSON 与 multipart，先查键不免除容量保证。命中的记录用它自己冻结的合同版本与请求指纹版本比对，未命中才按当前合同校验、选路与计价。

因此合同变化或指纹密钥轮换后，同键重发仍按记录当时的规则比对并投影原事实，不把状态强行当另一版本解析、也不重新解释。记录所需的请求指纹版本密钥缺失时返回 `409 idempotency_conflict`，不当作新请求执行。

### 9.2 原子受理

受理事务在同一账户与同一幂等身份上串行：先查该身份，命中则不新建并返回原记录，未命中才在同一事务内建立 Job、Hold 与容量占用，数据库唯一索引兜底。并发相同键的不同请求由指纹比对判定投影或 `409 idempotency_conflict`。查找与受理共用同一实现，命中返回完整记录交给 Application 按冻结规则比较，不拿当前指纹版本直接比较记录。

Attempt 收件凭据由数据库原子生成，不增加环境变量。`REQUEST_FINGERPRINT_KEY_V1` 在启用直接执行时稳定配置并按版本轮换，多个 API 副本一致；关闭直接执行时不需要它。

摘要算法在无密钥 SHA 之后不再变化。把写入端回滚到引入无密钥 SHA 之前的版本会让同键摘要不匹配并可能重复受理，回滚须连同数据一起处理；这是回滚约束，不是需要保留的旧数据兼容层。

## 10. 验证边界

具体执行步骤与证据记入[整改验证清单](../verification/synchronous-gateway-remediation.md)。必须覆盖最大 Provider 响应及编码、多个候选、大 JSON 结构、真实 socket 慢读、H2 多流关闭、预提交断开、所有续约失败、API/Worker 接管竞争、失败终态带 usage、旧库升级与合同冻结/指纹轮换。

验收不能仅以原有单元测试通过代替新的故障反例。A1/A5/A10 的缺失证据也必须补齐；既有延迟基准只覆盖小响应和固定假 Provider，不推导最大响应或 transport 期限已满足。
