# 配置项

两个进程的配置**只从环境变量读**，没有配置文件。值怎么送进进程（systemd `EnvironmentFile`、密钥系统、开发机 `.env`）见[生产环境](production.md) §2.4 与[开发环境](development.md) §3。

`RUST_LOG` 控制日志级别（缺省 `info`），输出是 **JSON**。下表"缺省"列是代码里的缺省，没设就走它。

## 1. 进程与连接

| 变量 | 缺省 | 生产怎么取 |
| --- | --- | --- |
| `DATABASE_URL` | 无（**必填**） | 两个进程都读。指向生产库，用户需 DDL 权限（[生产环境](production.md) §1.3） |
| `API_BIND` | `127.0.0.1:8081` | 监听地址。反代与 API 同机时保持回环；只有反代在不同机器或网络命名空间里，才需要用 `0.0.0.0:8081` 并配防火墙 |
| `ADMIN_TOKEN` | 无（**必填**） | 强随机串，按密钥管理（[生产环境](production.md) §1.4） |
| `ADMIN_EMAIL` / `ADMIN_PASSWORD` | 无 | 用来建/更新那个管理员账号。**只在不给 `ADMIN_TOKEN` 的机器上才必须**；给了令牌也必须能登录运营后台，所以生产应配上 |
| `WORKER_ID` | `worker-local-1` | **每个 worker 实例必须唯一**（租约按它归属）。多实例时要显式给不同值 |
| `REDIS_URL` | 空＝不启用 | 与 API 同一个 Redis。不配则全部回源数据库 |
| `CONSOLE_DEV_HOST` | 无 | **生产不要设**。它只为"本机没有域名可指"存在 |
| `HEALTH_PROBE_TIMEOUT_MS` | `2000` | 探活超时。读不出或为 0 退回缺省 |

## 2. 生成与成本护栏

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `GENERATION_MAX_COST_MICROUSD` | `20000` | **兜底保底额**（microusd） |
| `GENERATION_MAX_REQUEST_COST_MICROUSD` | `10000000` | **单次请求的上游成本上限**（microusd，默认 10 元）。发布期与受理期各判一次。与上一项**不是同一个量** |
| `GENERATION_MAX_DAILY_SPEND_MICROUSD` | `50000000` | **每账户每日**扣费上限（microusd，默认 50 美元等值）。判定每次回账本聚合，超限对客 `429 daily_spend_limit_exceeded` |
| `GENERATION_MAX_CONCURRENT_JOBS` | `1` | **每账户**同时能有多少个在跑的生成任务（**不是**每个 worker 的并发） |
| `GENERATION_RATE_LIMIT_REQUESTS_PER_WINDOW` | `60` | 受理侧限流（每窗口请求数） |
| `GENERATION_RATE_LIMIT_WINDOW_MS` | `60000` | 限流窗口（即默认每分钟 60 次） |
| `GENERATION_SYNC_WAIT_SECONDS` | `PROVIDER_TIMEOUT_SECONDS + 30` | 对客同步等待窗口。**必须 ≥ `PROVIDER_TIMEOUT_SECONDS`**，否则启动时拒绝并点名 |
| `PROVIDER_TIMEOUT_SECONDS` | 按合同最大输出张数算出 | 上游调用超时上限：`基数 + max(0, n − 含张数) × 每张`。显式设了就以它为准 |
| `PROVIDER_TIMEOUT_BASE_SECONDS` | `180` | 超时链的固定基数 |
| `PROVIDER_TIMEOUT_INCLUDED_IMAGES` | `4` | 基数里已含的产出张数 |
| `PROVIDER_TIMEOUT_PER_IMAGE_SECONDS` | `30` | 超出基数后每张追加的秒数 |
| `GENERATION_RETRY_MAX_ATTEMPTS` | `3` | 安全重投上限。**只在可证明上游没有受理时重投**；状态不确定一律不重投，进对账 |
| `GENERATION_RETRY_BACKOFF_BASE_MS` | `1000` | 重投退避基数 |

> 超时链是**启动时校验**的：API 会读已发布合同声明的最大输出张数，算一遍整条链，**不一致就拒绝启动并点名**。所以升级 `PROVIDER_TIMEOUT_*` 时要连同发布侧一起想清楚——它不是"调大就更快"。

## 3. Worker 租约与轮询

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `WORKER_POLL_INTERVAL_MS` | `1000` | 轮询间隔 |
| `WORKER_LEASE_SECONDS` | `PROVIDER_TIMEOUT_SECONDS × 1.2` | 领取租约时长。**必须 ≥ `PROVIDER_TIMEOUT_SECONDS`**；**过短**会让长任务被另一个 worker 抢走，**过长**让崩溃后的 Job 迟迟不恢复 |

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

**凭证只从环境变量读**，数据库只存变量名，日志与响应里不出现。缺哪个渠道的密钥，那个渠道的 Job 会失败——不是在启动时失败。把密钥放进 worker 的环境（同一个 `EnvironmentFile`，或密钥系统注入）；worker 在每个 Job 执行时按该渠道的 `credential_env` 现场读取，API 不调上游、不需要它们。

## 6. 供给素材导入

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `SUPPLY_MATERIAL_DIR` | 空＝不导入 | 设了之后，**API 每次启动**都会在迁移之后把目录里的 `*.json` 幂等写成渠道与 Offering（匹配键见 `docs/design/0012-platform-model-publishing.md` §3） |

这是**工程侧**的开关：渠道与供给由工程师随素材配一次，运营在后台**选**它们、给价。生产上给不给它，取决于你们的渠道接入流程是"随发布包走"还是"用管理接口配"——**目录不存在或没有素材时静默跳过**，所以不设它不会让服务起不来，只是可选供给清单会是空的。

## 7. 账本巡检与告警

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `LEDGER_AUDIT_ENABLED` | 开 | 关掉时这一层**根本不挂起来**（不是挂起来空转） |
| `LEDGER_AUDIT_INTERVAL_MS` | `900000`（15 分钟） | 巡检周期 |
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

重置令牌与 API Key 都是**只存哈希**的：明文只在签发那一次回给调用方，之后无法再取出。
