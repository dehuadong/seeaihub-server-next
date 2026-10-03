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
| `GENERATION_RETRY_MAX_ATTEMPTS` | `3` | 安全重投上限（旧 Worker 与直接同步执行共用）。**只在可证明上游没有受理时重投**；状态不确定一律不重投，进对账 |
| `GENERATION_RETRY_BACKOFF_BASE_MS` | `1000` | 重投退避基数（同上） |

> 超时链是**启动时校验**的：API 会读已发布合同声明的最大输出张数，算一遍整条链，**不一致就拒绝启动并点名**。所以升级 `PROVIDER_TIMEOUT_*` 时要连同发布侧一起想清楚——它不是"调大就更快"。

### 直接同步执行

`GENERATION_DIRECT_EXECUTION=true` 时，两条图片入口在 API 进程内直连 Provider：认证与读取准入在消费正文前完成，不建生成 Job、Worker 不领取、结果只在本进程内存里。上面的超时链、上游超时与成本护栏继续生效；下表只列这条路自己的开关、容量与期限。

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `GENERATION_DIRECT_EXECUTION` | `false` | 直接同步执行开关。关着时图片入口逐字走旧路径（建 Job、Worker 领取、轮询结果），下面的变量一个都不读 |
| `GENERATION_SETTLE_RESERVE_SECONDS` | `10` | 总期限 D 里预留给结算、提交确认与失败收尾的预算 R：上游预算因此是 D 减 R |
| `GENERATION_EXECUTION_LEASE_SECONDS` | `60` | v1 执行所有权的租约时长（秒）。`begin_submission` 按它落 `lease_expires_at`，API Supervisor 按它的三分之一周期独立续约；续约冲突或所有权失效立即取消该次执行 |
| `GENERATION_MAX_CHANNEL_IN_FLIGHT` | `32` | **渠道全局**未决任务上限，多副本经数据库槽位共同遵守（不是单机限制） |
| `GENERATION_EXECUTION_SLOTS` | `64` | 本机同时在执行的生成任务数 |
| `GENERATION_MAX_MEMORY_BYTES` | `2147483648`（2GiB） | 本机在飞执行可预占的内存总量；每次执行预留 32MiB，配得比它小进程起不来 |
| `GENERATION_READ_SLOTS` | `64` | 本机同时在读请求正文的准入名额；取不到直接拒绝，不排队 |
| `GENERATION_SEND_SLOTS` | `64` | 本机同时可持有的响应发送名额；在受理前预留 |
| `GENERATION_SLOW_READ_TIMEOUT_SECONDS` | `30` | 请求正文从开始接收到读完的上限；超时在受理前返回 408 `request_timeout`，不建记录 |
| `GENERATION_SEND_TIMEOUT_SECONDS` | `30` | 客户端发送的独立有界期限，不占用 D |
| `GENERATION_SHUTDOWN_GRACE_SECONDS` | `25` | 停机时给在飞任务有限收尾的宽限期；到点残余交异常对账 |

名额取正数、内存预算至少够一次执行：配不成可用的执行容量时进程启动失败并点名。

## 3. Worker 租约与轮询

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `WORKER_POLL_INTERVAL_MS` | `1000` | 轮询间隔 |
| `WORKER_LEASE_SECONDS` | `PROVIDER_TIMEOUT_SECONDS × 1.2` | 领取租约时长。**必须 ≥ `PROVIDER_TIMEOUT_SECONDS`**；**过短**会让长任务被另一个 worker 抢走，**过长**让崩溃后的 Job 迟迟不恢复 |

### 异常对账查询调度

旧 Worker 的生成领取之外，Worker 每轮还跑异常对账：接管租约过期的 v1 执行、按已知句柄只读查询、按证据幂等结算或建案。查询排期落在 `operations.reconciliation_cases` 的 `next_query_at`/`attempts` 上：同一案例未到下次查询时刻的记录本轮跳过；自动查询到次数上限后转人工并告警，不再自动查询。配置这些值不会改变收费或占用释放语义。

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

**凭证只从环境变量读**，数据库只存变量名，日志与响应里不出现。缺哪个渠道的密钥，那个渠道的 Job 会失败——不是在启动时失败。worker 在每个 Job 执行时按该渠道的 `credential_env` 现场读取，API 不调上游、不需要它们。

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
