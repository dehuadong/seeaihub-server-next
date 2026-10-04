# 配置项

两个进程的配置**只从环境变量读**，没有配置文件。值怎么送进进程（systemd `EnvironmentFile`、密钥系统、开发机 `.env`）见[生产环境](production.md) §2.4 与[开发环境](development.md) §3。

每个变量按"不设会怎样"分三类：

- **必填**：不设进程起不来，报错点名。`DATABASE_URL`（两个进程）、`ADMIN_TOKEN`（API）与 `CUSTOMER_HISTORY_CURSOR_KEY`（API）。
- **不设＝关掉该能力**：进程照常启动，但某个功能静默失效，日志里只有一条容易漏掉的提示。这类最容易踩——供给导入、管理员登录、渠道密钥、加速层、告警出口，以及多 worker 的 `WORKER_ID`；下面各表会点出来。
- **有缺省**：不设就走代码缺省，按需覆盖。

`RUST_LOG` 控制日志级别（缺省 `info`），输出是 **JSON**。各表"缺省"列是代码里的缺省，没设就走它。

## 1. 进程与连接

| 变量 | 缺省 | 生产怎么取 |
| --- | --- | --- |
| `DATABASE_URL` | 无（**必填**） | 两个进程都读。指向生产库，用户需 DDL 权限（[生产环境](production.md) §1.3） |
| `API_BIND` | `127.0.0.1:8081` | 监听地址。反代与 API 同机时保持回环；只有反代在不同机器或网络命名空间里，才需要用 `0.0.0.0:8081` 并配防火墙 |
| `ADMIN_TOKEN` | 无（**必填**） | 强随机串，按密钥管理（[生产环境](production.md) §1.4） |
| `ADMIN_EMAIL` / `ADMIN_PASSWORD` | 无 | 用来建/更新那个管理员账号。**不设＝运营后台登录不可用**（共享令牌仍能调接口）；生产应配上 |
| `WORKER_ID` | `worker-local-1` | **每个 worker 实例必须唯一**（租约按它归属）。多实例不显式给不同值＝共用同一个 ID，租约会互相抢 |
| `REDIS_URL` | 空＝不启用 | 与 API 同一个 Redis。**不设＝没有加速层**（功能不变、性能下降），全部回源数据库 |
| `CONSOLE_DEV_HOST` | 无 | **生产不要设**。它只为"本机没有域名可指"存在 |
| `HEALTH_PROBE_TIMEOUT_MS` | `2000` | 探活超时。读不出或为 0 退回缺省 |

## 2. 生成与成本护栏

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `GENERATION_MAX_COST_MICROUSD` | `20000` | **兜底保底额**（microusd） |
| `GENERATION_MAX_REQUEST_COST_MICROUSD` | `10000000` | **单次请求的上游成本上限**（microusd，默认 10 元）。发布期与受理期各判一次。与上一项**不是同一个量** |
| `GENERATION_MAX_DAILY_SPEND_MICROUSD` | `50000000` | **每账户每日**扣费上限（microusd，默认 50 美元等值）。按 UTC 自然日的已完成实收合计判定，超限对客 `429 daily_spend_limit_exceeded` |
| `GENERATION_MAX_CONCURRENT_JOBS` | `1` | **每账户**同时能有多少个在跑的生成任务（**不是**每个 worker 的并发） |
| `GENERATION_RATE_LIMIT_REQUESTS_PER_WINDOW` | `60` | 受理侧限流（每窗口请求数） |
| `GENERATION_RATE_LIMIT_WINDOW_MS` | `60000` | 限流窗口（即默认每分钟 60 次） |
| `GENERATION_SYNC_WAIT_SECONDS` | `PROVIDER_TIMEOUT_SECONDS + 30` | 对客同步等待窗口。**必须 ≥ `PROVIDER_TIMEOUT_SECONDS`**，否则启动时拒绝并点名 |
| `PROVIDER_TIMEOUT_SECONDS` | 按合同最大输出张数算出 | 上游调用超时上限：`基数 + max(0, n − 含张数) × 每张`。显式设了就以它为准 |
| `PROVIDER_TIMEOUT_BASE_SECONDS` | `180` | 超时链的固定基数 |
| `PROVIDER_TIMEOUT_INCLUDED_IMAGES` | `4` | 基数里已含的产出张数 |
| `PROVIDER_TIMEOUT_PER_IMAGE_SECONDS` | `30` | 超出基数后每张追加的秒数 |
| `GENERATION_RETRY_MAX_ATTEMPTS` | `3` | 一次请求内的安全重投上限。**只在可证明上游没有受理时重投**；状态不确定一律不重投，进对账 |
| `GENERATION_RETRY_BACKOFF_BASE_MS` | `1000` | 重投退避基数（同上） |

> 超时链是**启动时校验**的：API 会读已发布合同声明的最大输出张数，算一遍整条链，**不一致就拒绝启动并点名**。所以升级 `PROVIDER_TIMEOUT_*` 时要连同发布侧一起想清楚——它不是"调大就更快"。

### 直接同步执行

两条图片入口在 API 进程内直连 Provider：认证与读取准入在消费正文前完成，不建生成 Job、Worker 不领取、结果只在本进程内存里。上面的超时链、上游超时与成本护栏继续生效；下表只列这条路自己的容量与期限。

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `GENERATION_SETTLE_RESERVE_SECONDS` | `10` | 总期限 D 里预留给结算、提交确认与失败收尾的预算 R：上游预算因此是 D 减 R |
| `GENERATION_EXECUTION_LEASE_SECONDS` | `60` | v1 执行所有权的租约时长（秒）。`begin_submission` 按它落 `lease_expires_at`，API Supervisor 按它的三分之一周期独立续约；续约冲突或所有权失效立即取消该次执行 |
| `GENERATION_MAX_CHANNEL_IN_FLIGHT` | `32` | **渠道全局**未决任务上限，多副本经数据库槽位共同遵守（不是单机限制） |
| `GENERATION_EXECUTION_SLOTS` | `64` | 本机同时在执行的生成任务数 |
| `GENERATION_MAX_MEMORY_BYTES` | `2147483648`（2GiB） | 本机在飞执行可预占的内存总量；每次执行的预留按各 Driver 声明的字节上限算出（入口 wire、上游响应与编码膨胀同时计），配得比它小进程起不来 |
| `GENERATION_READ_SLOTS` | `64` | 本机同时在读请求正文的准入名额；取不到直接拒绝，不排队 |
| `GENERATION_SEND_SLOTS` | `64` | 本机同时可持有的响应发送名额；在受理前预留 |
| `GENERATION_SLOW_READ_TIMEOUT_SECONDS` | `30` | 请求正文从开始接收到读完的上限；超时在受理前返回 408 `request_timeout`，不建记录 |
| `GENERATION_SEND_TIMEOUT_SECONDS` | `30` | 客户端发送的独立有界期限，不占用 D |
| `GENERATION_SHUTDOWN_GRACE_SECONDS` | `25` | 停机时给在飞任务有限收尾的宽限期；到点残余交异常对账 |
| `GENERATION_RECONCILIATION_READ_BYTES` | `1048576`（1MiB） | 只读对账查询的响应上限（字节）。它独立于生成响应上限，超限的响应只留下证据缺口、不无界读图；配得比任何 Adapter 声明的生成响应上限还大时进程启动失败 |
| `GENERATION_OBSERVABILITY_INTERVAL_SECONDS` | `30` | 预算观测记录周期（秒）；`0` 表示不做周期记录，逐次容量拒绝仍在拒绝点记录 |

名额取正数、内存预算至少够一次执行：配不成可用的执行容量时进程启动失败并点名。

预算观测每周期记一次已预留字节、实际缓冲字节、活跃读取/执行/发送、连接数与各段拒绝次数（只记数量，不记图片或参数）；容量拒绝当场各记一条。两者都走 `tracing`，不引入独立的指标库。

### 连接与断开监视

API 自己驱动连接（accept loop + Hyper 连接 future），图片响应的发送许可与字节预算由连接注册表持有到连接销毁，客户端关闭由进程唯一的 epoll 线程独立观察。下表是这条路的容量与期限，全部有缺省。

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `API_MAX_CONNECTIONS` | `1024` | 同时在册连接上限；达到上限的新连接直接关闭，不进队列 |
| `API_MAX_CONNECTION_TASKS` | `64` | 每连接由 Hyper 派生的子任务上限；满员时拒绝新 task 并关闭整条连接 |
| `API_MONITOR_CONTROL_QUEUE` | `1024` | 断开监视控制队列容量，其中 64 个槽位固定留给清理；配得不大于 64 进程启动失败 |
| `API_MONITOR_CONFIRM_MILLIS` | `2000` | 等待监视注册、清理确认的上限；注册拿不到确认的连接不启动 |
| `API_MAX_HEADERS` | `128` | HTTP/1 每次请求的 header 条数上限 |
| `API_MAX_BUFFER_BYTES` | `65536` | HTTP/1 解析缓冲上限（也限制已缓存的流水线字节） |
| `API_H2_MAX_CONCURRENT_STREAMS` | `128` | HTTP/2 每连接最大并发流 |
| `API_H2_MAX_SEND_BUFFER_BYTES` | `1048576` | HTTP/2 发送缓冲上限 |
| `API_H2_MAX_HEADER_LIST_BYTES` | `65536` | HTTP/2 header 列表上限 |

HTTP/1 的图片响应带 `Connection: close`：写完这条连接就结束，发送许可随连接销毁释放；非图片路由保留 keep-alive。HTTP/2 的发送期限按**连接**生效——同一连接内多个图片响应取最早期限，到期关闭整条连接（含其他流）。客户端只关写半边按完整连接取消处理。

直接执行只在 Linux 上启动：关闭事件检测用的是 epoll，其他平台即使显式配置也拒绝启动，不会退回保存业务载荷的生成链路。反向代理若不在客户端断开时取消上游请求，API 观察不到终端离开，只由总期限收口。

## 3. Worker 轮询

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `WORKER_POLL_INTERVAL_MS` | `1000` | 没有活可干时的轮询间隔 |

### 异常对账查询调度

Worker 每轮跑异常对账：接管租约过期的 v1 执行、按已知句柄只读查询、按证据幂等结算或建案。查询排期落在 `operations.reconciliation_cases` 的 `next_query_at`/`attempts` 上：同一案例未到下次查询时刻的记录本轮跳过；自动查询到次数上限后转人工并告警，不再自动查询。配置这些值不会改变收费或占用释放语义。

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
| `RECONCILIATION_LEDGER_AUDIT_EVERY_ROUNDS` | `10` | 每多少轮跑一次慢周期账务核对（只建案告警，不改账） |
| `RECONCILIATION_LEDGER_AUDIT_LIMIT` | `100` | 慢周期一次最多核对几个账户 |
| `RECONCILIATION_LEDGER_AUDIT_WINDOW_SECONDS` | `3600` | 慢周期按账户更新时刻取的增量窗口 |

## 4. 加速层（Redis 启用时）

| 变量 | 缺省 |
| --- | --- |
| `CACHE_ROUTE_TTL_SECONDS` | `60` |
| `CACHE_BALANCE_TTL_SECONDS` | `360` |
| `CACHE_FRESHNESS_WINDOW_MS` | `5000` |
| `CACHE_RECONCILE_INTERVAL_MS` | `180000` |
| `CACHE_OPERATION_TIMEOUT_MS` | `200` |

缓存**不是事实来源**：它对不上就回源数据库，不可达时报 miss 而不是挂住。所以 Redis 故障是**降级**，不是故障。

## 5. 渠道凭证

| 变量 | 说明 |
| --- | --- |
| `AIHUBMIX_API_KEY` / `APIMART_API_KEY` | 变量名由发布素材里的 `credential_env` 指定 |

**凭证只从环境变量读**，数据库只存变量名，日志与响应里不出现。缺哪个渠道的密钥，那一次执行会失败——不是在启动时失败。API 在受理之后、发出外部请求之前按该渠道的 `credential_env` 现场读取；对账 Worker 只在需要重查上游状态时读同一份。

**怎么送进去**：生产用 systemd 的 `EnvironmentFile`（或由密钥系统在启动前渲染它），文件属服务账号、`chmod 600`；本地可以 `export` 或写开发机 `.env`。手动 `export` 只活在当前 shell，**机器重启后要重新 set**，所以它只适合本地，不是生产手段（见[生产环境](production.md) §2.4）。

## 6. 供给素材导入

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `SUPPLY_MATERIAL_DIR` | `config/bootstrap` | **API 每次启动**都会在迁移之后把该目录里的 `*.json` 幂等写成渠道与 Offering（匹配键见 `docs/design/0012-platform-model-publishing.md` §3）。默认值就是仓库与镜像里那份工程师素材；设成**空串＝显式不导入**（测试库、开发库要一份干净的供给清单时用它）；目录不存在或里面没有素材时什么都不做 |

这是**工程侧**的开关：渠道与供给由工程师随素材配一次，运营在后台**选**它们、给价。运营那条路本身不建供给，管理接口也只能启停（`PATCH /api/v1/channels/{id}`、`PATCH /api/v1/offerings/{id}`）；要凭空造供给只剩**旧的内联发布形状**（`POST /api/v1/runtime-revisions` 直接带 `offerings`，`docs/design/0012-platform-model-publishing.md` §7 的过渡路径，不是运营的路）。**不设也能上架**：默认目录就是那份素材；换自己的素材就用只读卷把变量指过去。

## 7. 账本核查与告警

账实核对**没有默认周期**：只由管理员按账户触发（`POST /api/v1/accounts/{id}/ledger-audit`），在后台任务里执行，不参与资金写入或余额读取。

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `PROVIDER_ALERT_WEBHOOK` | 空＝没有出口 | 平台侧故障告警的 POST JSON 出口。**没有默认地址、代码里不写死 URL**；地址写错在启动时就失败，不让进程带着一个"永远发不出去"的出口跑 |
| `PROVIDER_ALERT_CONSECUTIVE_FAILURES` | `3` | 某候选连续失败几次才告警。必须 ≥ 1 |
| `PROVIDER_ALERT_TIMEOUT_MS` | `5000` | 告警投递超时 |
| `PROVIDER_ALERT_RETRIES` | `2` | 告警投递重试次数。两者都**有界**——告警是旁路，不能因为对端不响应把发它的那一轮拖住 |

告警请求体只带**定位字段**（哪次执行失败、哪笔账实不符），**不带凭证、提示词与图片**。

## 8. 会话与口令

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `SESSION_TTL_SECONDS` | `43200`（12 小时） | 管理员与客户会话的有效期 |
| `PASSWORD_RESET_TTL_SECONDS` | `1800`（30 分钟） | 口令重置令牌的有效期。**比会话短得多**：它能改口令，暴露窗口越小越好 |
| `CUSTOMER_HISTORY_CURSOR_KEY` | 无（**必填**） | 客户调用记录与资金流水翻页游标的加密密钥：**32 字节的 base64**（例如 `openssl rand -base64 32`）。**同一部署的所有 API 实例必须配同一份**，否则换一个实例就解不开上一页给的游标；它只在环境变量里，不进仓库、日志或响应。**轮换会让已发出的游标全部失效**——页面提示客户重新查询即可，不需要清库 |

重置令牌与 API Key 都是**只存哈希**的：明文只在签发那一次回给调用方，之后无法再取出。
## 9. 幂等与请求指纹密钥

直接同步执行在**不保存明文**的前提下识别重复调用：调用方的 `Idempotency-Key` 用**无密钥** SHA-256（固定领域前缀 || 幂等键）摘成稳定查找键（库里只有摘要，没有明文键），摘要只求稳定、不可逆、所有 API 副本一致，不依赖也不轮换密钥；请求指纹密钥把规范化后的请求摘要成 HMAC-SHA256 指纹，用来区分同键同请求（按 [Spec 0005](../specs/0005-synchronous-image-gateway.md) §4 给重放投影）与同键异请求（409 `idempotency_conflict`）。

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `REQUEST_FINGERPRINT_KEY_V<n>` | 无（**必填**，`n` 从 1 到当前版本） | 32 字节 base64 HMAC 密钥，按版本号命名。查到旧记录后要用记录里的版本重算指纹才能比较（[RFC 0017](../design/0017-synchronous-image-gateway.md) §2），所以**旧版本密钥保留到相应记录退出保证范围** |
| `REQUEST_FINGERPRINT_KEY_VERSION` | `1` | 当前请求指纹版本；`1..=该值` 每一档都必须给出密钥 |

**最小配置**：没有轮换时只需 `REQUEST_FINGERPRINT_KEY_V1` 一个值，`REQUEST_FINGERPRINT_KEY_VERSION` 缺省为 1。下面的轮换规则只在**真的换密钥**时用到。

请求指纹密钥只从环境变量读，**32 字节 base64**（`openssl rand -base64 32`），不进仓库、日志、响应或测试夹具，也不与渠道凭证混用。缺失、长度不对、或当前版本没有对应密钥时**启动即失败**，不让进程带着不完整的去重能力跑。

**轮换**：把 `REQUEST_FINGERPRINT_KEY_VERSION` 抬到新值并加上新一档密钥，旧的保留；已有记录继续按自己的版本复核。移除旧版本密钥会让那批记录无法安全比对，同键调用返回 409 `idempotency_conflict`，而不是当成新请求。
