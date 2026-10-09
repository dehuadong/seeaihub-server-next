# 配置项

两个进程的配置**只从环境变量读**，没有配置文件。生产用 systemd 的 `EnvironmentFile`、容器的 `env_file`，或由密钥系统在启动前渲染；开发机写 `.env`。

每个变量按"不设会怎样"分三类：

- **必填**：不设进程起不来，报错点名。`DATABASE_URL`（两个进程）、`ADMIN_TOKEN`（API）、`SEE_BASEURL`（API）、`CUSTOMER_HISTORY_CURSOR_KEY`（API）与 `REQUEST_FINGERPRINT_KEY_V1`（API）。
- **不设＝关掉该能力**：进程照常启动，但某个功能静默失效，日志里只有一条容易漏掉的提示。这类最容易踩——供给导入、管理员登录、渠道密钥、上传存储配置、加速层、告警出口，以及多 worker 的 `WORKER_ID`；下面各表会点出来。
- **有缺省**：不设就走代码缺省，按需覆盖。

`RUST_LOG` 控制日志级别（缺省 `info`），输出是 **JSON**。各表"缺省"列是代码里的缺省，没设就走它。

## 1. 进程与连接

| 变量 | 缺省 | 生产怎么取 |
| --- | --- | --- |
| `DATABASE_URL` | 无（**必填**） | 两个进程都读。指向生产库；这个库用户要有 DDL 权限，因为迁移在进程启动时自动跑 |
| `API_BIND` | `127.0.0.1:8081` | 监听地址。反代与 API 同机时保持回环；只有反代在不同机器或网络命名空间里，才用 `0.0.0.0:8081` 并配防火墙 |
| `ADMIN_TOKEN` | 无（**必填**） | 强随机串，按密钥管：它不指向具体的人，拿到它就等于管理员权限 |
| `SEE_BASEURL` | 无（**必填**） | 平台对客基址，只写源、不带结尾斜杠（如 `https://app.example.com`）。模型说明与公共使用文档的链接按它写成绝对地址；若从请求主机取，会把管理端地址写进不可变的版本 |
| `ADMIN_EMAIL` / `ADMIN_PASSWORD` | 无 | 用来建或更新那个管理员账号。**不设＝运营后台登录不可用**（共享令牌仍能调接口）；生产应配上 |
| `WORKER_ID` | `worker-local-1` | **每个 worker 实例必须唯一**（租约按它归属）。多实例不显式给不同值＝共用同一个 ID，租约会互相抢 |
| `REDIS_URL` | 空＝不启用 | 与 API 同一个 Redis。**不设＝没有加速层**（功能不变、性能下降），全部回源数据库 |
| `CONSOLE_DEV_HOST` | 无 | **生产不要设**。它只为"本机没有域名可指"存在 |
| `HEALTH_PROBE_TIMEOUT_MS` | `2000` | 探活超时。读不出或为 0 退回缺省 |

## 2. 生成护栏

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `GENERATION_MAX_COST_MICROUSD` | `20000` | **兜底保底额**（CNY 微单位，`20000` = 0.02 元）：只有查不到该供给的保底表时，才用它算这次请求要预扣多少。名字里的 `usd` 是历史命名 |
| `AUTH_ATTEMPT_LIMIT_<ENDPOINT>_FAILURES_PER_WINDOW` | `10` | 公开鉴权端点（`<ENDPOINT>` 取 `REGISTER` / `LOGIN` / `REDEEM`）每窗口的失败尝试上限；超限对客 `429 rate_limit_exceeded` 带 `Retry-After` |
| `AUTH_ATTEMPT_LIMIT_<ENDPOINT>_WINDOW_MS` | `60000` | 上述三个端点各自的计数窗口 |
| `AUTH_SOURCE_HEADER` | 不设 | 公开鉴权来源维采信的受信头（如 `x-real-ip`）；**不设时退回连接对端地址**。采信它要求 API 不能被绕过代理直连——直连时这个头谁都能写 |
| `GENERATION_SYNC_WAIT_SECONDS` | `PROVIDER_TIMEOUT_SECONDS + 30` | 对客同步等待窗口。**必须 ≥ `PROVIDER_TIMEOUT_SECONDS`**，否则启动时拒绝并点名 |
| `PROVIDER_TIMEOUT_SECONDS` | 按合同最大输出张数算出 | 单次调用上游的超时上限：`基数 + max(0, n − 含张数) × 每张`。显式设了就以它为准 |
| `PROVIDER_TIMEOUT_BASE_SECONDS` | `180` | 超时链的固定基数 |
| `PROVIDER_TIMEOUT_INCLUDED_IMAGES` | `4` | 基数里已含的产出张数 |
| `PROVIDER_TIMEOUT_PER_IMAGE_SECONDS` | `30` | 超出基数后每张追加的秒数 |
| `GENERATION_RETRY_MAX_ATTEMPTS` | `3` | 一次请求内的安全重投上限。**只在能证明上游没有受理时重投**（连不上、上传失败、参考图取不到、上游明确拒绝）；状态不确定一律不重投，转人工对账 |
| `GENERATION_RETRY_BACKOFF_BASE_MS` | `1000` | 重投退避基数（同上） |

> 超时链是**启动时校验**的：API 读已发布合同声明的最大输出张数，算一遍整条链，**不一致就拒绝启动并点名**。所以改 `PROVIDER_TIMEOUT_*` 时要连同发布侧一起想清楚——它不是"调大就更快"。张数上限与超时链的关系见[输出张数取值面](../../.agents/notes/implemented/platform/2026-09-24-output-image-count-bounds.md)；保底额口径见[保底与结算](../../.agents/notes/implemented/platform/2026-09-22-pricing-floor-and-settlement.md)；重投口径见[重投的边界](../../.agents/notes/implemented/platform/2026-09-27-adr-0011-retry-reversal.md)。

### 直接同步执行

两条图片入口在 API 进程内直连 Provider：认证与读取准入在消费正文前完成，不建生成 Job、Worker 不领取、结果只在本进程内存里。上面的超时链与上游超时继续生效；下表只列这条路自己的容量与期限。

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `GENERATION_SETTLE_RESERVE_SECONDS` | `10` | 给"结算与收尾"预留的秒数：一次请求的总期限先扣掉这部分，剩下的才是上游能用的时间 |
| `GENERATION_EXECUTION_LEASE_SECONDS` | `60` | 执行所有权的租约时长（秒）。租约到期而没人续约，这次执行就归异常对账接管；API 按它的三分之一周期续约 |
| `GENERATION_MAX_CHANNEL_IN_FLIGHT` | `32` | **渠道全局**未决任务上限，多副本经数据库槽位共同遵守（不是单机限制） |
| `GENERATION_EXECUTION_SLOTS` | 按预算推导 | 本机同时在执行的生成任务数。**缺省不写**：按「内存预算 ÷ 单次预留」推导（缺省预算 2 GiB 下是 4）。显式写下的值必须装得进预算，否则拒绝启动并点名它与 `GENERATION_MAX_MEMORY_BYTES` |
| `GENERATION_MAX_MEMORY_BYTES` | `2147483648`（2 GiB） | 本机在飞执行可预占的内存总量（字节）。单次预留按各驱动声明的响应上限算出：AIHubMix（响应上限 128 MiB）当前是 458587040 字节≈437 MiB，所以缺省预算下并发是 4。逐项构成与测量命令写在 `crates/adapter-sdk` 的 `GATEWAY_*` 常量注释里；配得比一次预留小、或装不下所配名额时拒绝启动 |
| `GENERATION_READ_SLOTS` | `64` | 本机同时在读请求正文的准入名额；取不到直接拒绝，不排队。它不能超过连接容量 |
| `GENERATION_SEND_SLOTS` | `64` | 本机同时可持有的响应发送名额；在受理前预留。发送许可活到连接销毁，因此它同样不能超过连接容量 |
| `GENERATION_SLOW_READ_TIMEOUT_SECONDS` | `30` | 请求正文从开始接收到读完的上限；超时在受理前返回 408 `request_timeout`，不建记录 |
| `GENERATION_SEND_TIMEOUT_SECONDS` | `30` | 客户端发送的独立有界期限，不占用总期限 |
| `GENERATION_SHUTDOWN_GRACE_SECONDS` | `25` | 停机时给在飞任务有限收尾的宽限期；到点残余交异常对账 |
| `GENERATION_RECONCILIATION_READ_BYTES` | `1048576`（1 MiB） | 只读对账查询的响应上限（字节）。它独立于生成响应上限：超限的响应只留下证据缺口，不会无界读图。配得比任何 Adapter 声明的生成响应上限还大时启动失败 |
| `GENERATION_OBSERVABILITY_INTERVAL_SECONDS` | `30` | 预算观测记录周期（秒）；`0` 表示不做周期记录，逐次容量拒绝仍在拒绝点记录 |

启动时校验容量组合，不自洽就拒绝启动并点名相关配置：名额都取正数，内存预算至少够一次执行，执行名额 × 单次预留不超过内存预算，读取 / 发送名额不超过 `API_MAX_CONNECTIONS × API_H2_MAX_CONCURRENT_STREAMS`。组合校验不替运维调小任何上限。

预算观测每周期记一次已预留字节、实际缓冲字节、活跃读取 / 执行 / 发送、连接数与各段拒绝次数（只记数量，不记图片或参数）；容量拒绝当场各记一条。两者都走 `tracing`，不引入独立的指标库。

### 请求结构与正文上限

图片入口的正文上限是 16 MiB（与 `GATEWAY_REQUEST_WIRE_BYTES` 同一常量），超过返回 413。正文解析按结构计数，超限在**受理前**返回 400 `invalid_parameter`：不建记录、不取占用、不调用上游。

| 上限 | 缺省 | 环境变量 |
| --- | --- | --- |
| 容器嵌套层数 | 64 | `GENERATION_REQUEST_JSON_MAX_DEPTH` |
| 节点总数 | 2048 | `GENERATION_REQUEST_JSON_MAX_NODES` |
| 单对象字段数 | 256 | `GENERATION_REQUEST_JSON_MAX_OBJECT_FIELDS` |
| 累计字符串字节 | 16 MiB | `GENERATION_REQUEST_JSON_MAX_STRING_BYTES` |

缺省值由 16 MiB 正文上限推导（节点按实测最贵形态约 1 KiB/节点，两项合计落在 18 MiB 的解析预留内）。配置**只允许收紧**：写成 0 或大于推导值会在启动时报错。

代价是节点密集的请求会被拒——例如未声明字段里塞几千个元素的数组，它此前能解析、随后按合同丢弃。要放宽先改推导与调用方说明，不要只调环境变量。

### 连接与断开监视

API 自己驱动连接（accept loop + Hyper 连接 future），图片响应的发送许可与字节预算由连接注册表持有到连接销毁，客户端关闭由进程唯一的 epoll 线程独立观察。下表是这条路的容量与期限，全部有缺省。

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `API_MAX_CONNECTIONS` | `1024` | 同时在册连接上限；达到上限的新连接直接关闭，不进队列 |
| `API_MAX_CONNECTION_TASKS` | `64` | 每连接由 Hyper 派生的子任务上限；满员时拒绝新 task 并关闭整条连接 |
| `API_MONITOR_CONTROL_QUEUE` | `1024` | 断开监视控制队列容量，其中 64 个槽位固定留给清理；配得不大于 64 时启动失败 |
| `API_MONITOR_CONFIRM_MILLIS` | `2000` | 等待监视注册、清理确认的上限；注册拿不到确认的连接不启动 |
| `API_MAX_HEADERS` | `128` | HTTP/1 每次请求的 header 条数上限 |
| `API_MAX_BUFFER_BYTES` | `65536` | HTTP/1 解析缓冲上限（也限制已缓存的流水线字节）。它与 `API_H2_MAX_SEND_BUFFER_BYTES` 一起按配置计入单次执行预留的 transport 那一项 |
| `API_H2_MAX_CONCURRENT_STREAMS` | `128` | HTTP/2 每连接最大并发流；它与 `API_MAX_CONNECTIONS` 的乘积是读取 / 发送名额的上限 |
| `API_H2_MAX_SEND_BUFFER_BYTES` | `1048576` | HTTP/2 发送缓冲上限 |
| `API_H2_MAX_HEADER_LIST_BYTES` | `65536` | HTTP/2 header 列表上限 |

HTTP/1 的图片响应带 `Connection: close`：写完这条连接就结束，发送许可随连接销毁释放；非图片路由保留 keep-alive。HTTP/2 的发送期限按**连接**生效——同一连接内多个图片响应取最早期限，到期关闭整条连接（含其他流）。客户端只关写半边按完整连接取消处理。

直接执行只在 Linux 上启动：关闭事件检测用的是 epoll，其他平台即使显式配置也拒绝启动。反向代理若不在客户端断开时取消上游请求，API 观察不到终端离开，只由总期限收口。

## 3. Worker 轮询

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `WORKER_POLL_INTERVAL_MS` | `1000` | 没有活可干时的轮询间隔 |

### 异常对账查询调度

Worker 每轮跑异常对账：接管租约过期的执行、按已知句柄只读查询、按证据幂等结算或建案。查询排期落在 `operations.reconciliation_cases` 的 `next_query_at` / `attempts` 上：同一案例未到下次查询时刻的记录本轮跳过；自动查询到次数上限后转人工并告警，不再自动查询。配置这些值不会改变收费或占用释放语义。

只读查询读多少字节由渠道侧的 `GENERATION_RECONCILIATION_READ_BYTES` 定（见 §2「直接同步执行」）：它比生成响应的上限小，响应超过它就只留证据缺口，API 与 Worker 读同一份配置。

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `RECONCILIATION_BATCH_LIMIT` | `16` | 每轮接管、回收孤儿与领取晚到事实的批次上限 |
| `RECONCILIATION_ORPHAN_MAX_AGE_SECONDS` | `900` | 从未写下提交声明的 `admitted` 超过这个年龄才回收（落失败并释放 Hold 与渠道槽位） |
| `RECONCILIATION_LATE_FACT_CLAIM_TTL_SECONDS` | `300` | 晚到事实的领取 TTL；领取超时可被另一 worker 重领，只有消费成功才标记 |
| `RECONCILIATION_QUERY_TIMEOUT_SECONDS` | `30` | 单次只读账务查询的期限 |
| `RECONCILIATION_QUERY_MAX_ATTEMPTS` | `5` | 一个对账案例自动只读查询的次数上限；到上限转人工、不再自动查询 |
| `RECONCILIATION_QUERY_BACKOFF_BASE_SECONDS` | `30` | 查询退避基：第 n 次查询后排「基 × 2^(n−1)」 |
| `RECONCILIATION_QUERY_BACKOFF_MAX_SECONDS` | `3600` | 单次退避的上限 |
| `RECONCILIATION_LEDGER_AUDIT_EVERY_ROUNDS` | `10` | 每多少轮跑一次慢周期账务核对（只建案告警，不改账）。核对口径见[账本核对](../../.agents/notes/implemented/platform/2026-09-23-ledger-balance-audit.md) |
| `RECONCILIATION_LEDGER_AUDIT_LIMIT` | `100` | 慢周期一次最多核对几个账户 |
| `RECONCILIATION_LEDGER_AUDIT_WINDOW_SECONDS` | `3600` | 慢周期按账户更新时刻取的增量窗口 |

## 4. 加速层（Redis 启用时）

| 变量 | 缺省 |
| --- | --- |
| `CACHE_BALANCE_TTL_SECONDS` | `360` |
| `CACHE_RECONCILE_INTERVAL_MS` | `180000` |
| `CACHE_OPERATION_TIMEOUT_MS` | `200` |

缓存**不是事实来源**：余额以数据库为准，不可达时读写都当未命中，不挂住请求。所以 Redis 故障是**降级**，不是故障。取值理由见[加速层](../../.agents/notes/implemented/platform/2026-09-22-redis-acceleration-layer.md)与[余额缓存的版本守卫](../../.agents/notes/implemented/platform/2026-09-30-account-balance-cache-version-guard.md)。

受理**不从缓存取候选**：直接执行每次受理直读数据库的 `supply` 候选查询。缓存里只放提交后的余额快照（写穿）、公开鉴权端点的失败尝试计数（按端点、来源与身份），以及历史 route 条目——后者由对账器按修订标识与网关模型开关清理，并写 `cache.route_invalidated` 审计。

## 5. 渠道凭证

| 变量 | 说明 |
| --- | --- |
| `AIHUBMIX_API_KEY` / `APIMART_API_KEY` | 变量名由发布素材里的 `credential_env` 指定 |

**凭证只从环境变量读**，数据库只存变量名，日志与响应里不出现。缺哪个渠道的密钥，那一次执行会失败——不是在启动时失败。API 在受理之后、发出外部请求之前按该渠道的 `credential_env` 现场读取；对账 Worker 只在需要重查上游状态时读同一份。

**怎么送进去**：生产用 systemd 的 `EnvironmentFile` 或容器的 `env_file`（或由密钥系统在启动前渲染它），文件属服务账号、`chmod 600`；本地可以 `export` 或写开发机 `.env`。手动 `export` 只活在当前 shell，**机器重启后要重新 set**，所以它只适合本地。

## 6. 供给素材导入

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `SUPPLY_MATERIAL_DIR` | `config/bootstrap` | **API 每次启动**都会在迁移之后把该目录里的 `*.json` 幂等写成渠道与供给：同一模型 + 同一渠道就是同一行，素材改了就地更新。默认值就是仓库与镜像里那份工程师素材；设成**空串＝显式不导入**（测试库、开发库要一份干净的供给清单时用它）；目录不存在或里面没有素材时什么都不做 |
| `PUBLIC_DOCS_DIR` | `public-docs` | 对客公开文档与素材 `documentation.narrative_path` 的解析根，相对进程工作目录。`GET /v1/docs/*` 与素材导入都读它 |

这是**工程侧**的开关：渠道与供给由工程师随素材配一次，运营在后台**选**它们、给价。运营那条路本身不建供给，管理接口也只能启停（`PATCH /api/v1/channels/{id}`、`PATCH /api/v1/offerings/{id}`）；要凭空造供给只剩**旧的内联发布形状**（`POST /api/v1/runtime-revisions` 直接带 `offerings`），它不是运营的路。**不设也能上架**：默认目录就是那份素材；换自己的素材就用只读卷把变量指过去。

## 7. 账本核查与告警

账实核对**没有默认周期**：只由管理员按账户触发（`POST /api/v1/accounts/{id}/ledger-audit`），在后台任务里执行，不参与资金写入或余额读取。

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `PROVIDER_ALERT_WEBHOOK` | 空＝没有出口 | 平台侧故障告警的 POST JSON 出口。**没有默认地址、代码里不写死 URL**；地址写错在启动时就失败，不让进程带着一个"永远发不出去"的出口跑 |
| `PROVIDER_ALERT_CONSECUTIVE_FAILURES` | `3` | 某候选连续失败几次才告警。必须 ≥ 1 |
| `PROVIDER_ALERT_TIMEOUT_MS` | `5000` | 告警投递超时 |
| `PROVIDER_ALERT_RETRIES` | `2` | 告警投递重试次数。两者都**有界**——告警是旁路，不能因为对端不响应把发它的那一轮拖住 |

告警请求体只带**定位字段**（哪次执行失败、哪笔账实不符），**不带凭证、提示词与图片**。口径见[平台故障告警出口](../../.agents/notes/implemented/platform/2026-09-23-platform-failure-alert-webhook.md)。

## 8. 会话与口令

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `SESSION_TTL_SECONDS` | `43200`（12 小时） | 管理员与客户会话的有效期 |
| `PASSWORD_RESET_TTL_SECONDS` | `1800`（30 分钟） | 口令重置令牌的有效期。**比会话短得多**：它能改口令，暴露窗口越小越好 |
| `CUSTOMER_HISTORY_CURSOR_KEY` | 无（**必填**） | 客户调用记录与资金流水翻页游标的加密密钥：**32 字节的 base64**（例如 `openssl rand -base64 32`）。**同一部署的所有 API 实例必须配同一份**，否则换一个实例就解不开上一页给的游标；它只在环境变量里，不进仓库、日志或响应。**轮换会让已发出的游标全部失效**——页面提示客户重新查询即可，不需要清库 |

重置令牌与 API Key 都是**只存哈希**的：明文只在签发那一次回给调用方，之后无法再取出。会话与身份口径见[身份、会话与控制台](../../.agents/notes/implemented/platform/2026-09-27-identity-sessions-and-consoles.md)。

## 9. 幂等与请求指纹密钥

直接同步执行在**不保存明文**的前提下识别重复调用：调用方的 `Idempotency-Key` 用**无密钥** SHA-256（固定领域前缀 ‖ 幂等键）摘成稳定查找键，库里只有摘要、没有明文键；摘要只求稳定、不可逆、所有 API 副本一致，所以不依赖也不轮换密钥。请求指纹密钥把规范化后的请求摘要成 HMAC-SHA256 指纹，用来区分同键同请求（按原请求结果重放）与同键异请求（409 `idempotency_conflict`）。

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `REQUEST_FINGERPRINT_KEY_V<n>` | 无（**必填**，`n` 从 1 到当前版本） | 32 字节 base64 HMAC 密钥，按版本号命名。查到旧记录后要用记录里的版本重算指纹才能比较，所以**旧版本密钥保留到相应记录退出保证范围** |
| `REQUEST_FINGERPRINT_KEY_VERSION` | `1` | 当前请求指纹版本；`1..=该值` 每一档都必须给出密钥 |

**最小配置**：没有轮换时只需 `REQUEST_FINGERPRINT_KEY_V1` 一个值，`REQUEST_FINGERPRINT_KEY_VERSION` 缺省为 1。下面的轮换规则只在**真的换密钥**时用到。

请求指纹密钥只从环境变量读，**32 字节 base64**（`openssl rand -base64 32`），不进仓库、日志、响应或测试夹具，也不与渠道凭证混用。缺失、长度不对、或当前版本没有对应密钥时**启动即失败**，不让进程带着不完整的去重能力跑。

**轮换**：把 `REQUEST_FINGERPRINT_KEY_VERSION` 抬到新值并加上新一档密钥，旧的保留；已有记录继续按自己的版本复核。移除旧版本密钥会让那批记录无法安全比对，同键调用返回 409 `idempotency_conflict`，而不是当成新请求。

## 10. 上传端点与上传存储

调用方把本地文件经 `POST /v1/uploads/images` 写入上传存储换公网 URL，再作为参考图或遮罩提交生成请求。上传不计费、不计量、不限配额，也不建执行记录；上传存储只有**阿里云 OSS** 一种。这条路的口径见[参考图上传](../../.agents/notes/implemented/platform/2026-10-04-reference-image-upload.md)。

上传存储的配置**全部从环境变量读**：没有后台管理页，也没有数据库表。

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `UPLOAD_STORAGE_REGION` | 无；整组都不给＝未配置 | 形如 `cn-hangzhou`，取值是小写字母、数字与连字符，首尾必须是字母或数字，长度不超过 63 |
| `UPLOAD_STORAGE_BUCKET` | 无；整组都不给＝未配置 | 3–63 位小写字母、数字与连字符，**不含点号**（含点的桶名在 `{bucket}.{host}` 形态下会撞只覆盖一级标签的通配符证书） |
| `UPLOAD_STORAGE_ENDPOINT` | 按 region 派生 | 可选；省略时用 `https://oss-{region}.aliyuncs.com`，显式给出时是含 scheme 的 origin（`https://主机[:端口]`；把请求指向进程内假对象存储的测试可用 `http://127.0.0.1[:端口]`、`http://[::1][:端口]` 或 `http://localhost[:端口]`），不带凭证、path、query 与 fragment |
| `UPLOAD_STORAGE_ACCESS_KEY_ID` | 无；须与下一条成对 | 访问密钥标识。只从环境变量读，不进日志与响应 |
| `UPLOAD_STORAGE_ACCESS_KEY_SECRET` | 无；须与上一条成对 | 访问密钥 |
| `UPLOAD_MAX_REQUEST_BYTES` | `22020096`（21 MiB） | 上传请求体上限（单文件 20 MiB 加 1 MiB multipart 协议余量），超限回 `413 request_too_large` |
| `UPLOAD_SLOTS` | `4` | 本机同时读上传正文的许可数，取不到回 `429 upload_busy`，不排队 |
| `UPLOAD_MAX_BUFFER_BYTES` | `100663296`（96 MiB） | 本机上传内存预算 |
| `UPLOAD_REQUEST_TIMEOUT_SECONDS` | `30` | 单次写对象存储的请求超时 |
| `UPLOAD_SLOW_READ_TIMEOUT_SECONDS` | `30` | 上传正文从开始接收到读完的上限；超时在受理前回 `408 request_timeout`，此时没有对象被写入 |
| `UPLOAD_RETRY_MAX_ATTEMPTS` | `3` | 单次上传写入的总尝试次数上限 |
| `UPLOAD_RETRY_BACKOFF_BASE_SECONDS` | `1` | 固定退避基准秒数；对象存储未给出有界整数秒 `Retry-After` 时按它等待 |

单文件上限是**领域常量 20 MiB**（严格小于 20971520 字节），不可配。上传存储的**整组变量都不给＝未配置**：进程照常启动，上传端点对该请求返回 `503 upload_storage_unavailable`；**只给一部分**（缺 region、bucket 或任一条密钥）或取值形状不合法＝**启动期拒绝并点名**，进程不启动。启动与运行期都不做活体探测：对象存储可达性、bucket 是否存在与桶是否匿名可读都不在启动判据里。上传侧的容量组合同样在启动期校验：`UPLOAD_SLOTS × UPLOAD_MAX_REQUEST_BYTES ≤ UPLOAD_MAX_BUFFER_BYTES`（单次上传的预留就是 `UPLOAD_MAX_REQUEST_BYTES`，不另立常数），配不出可用容量就拒绝启动，不替运维调小任何上限。

桶的匿名可读是启用上传的运维前置条件：客户拿到的是对象 URL，桶不允许匿名读时客户拿不到图。桶策略由运维在对象存储控制台写：把该桶的读写权限设为**公共读**。平台不发放对象 ACL 头。
