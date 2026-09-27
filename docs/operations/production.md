# 生产环境

两个进程 + 两个依赖：

| 组件 | 数量 | 职责 |
| --- | --- | --- |
| `seeai-api` | 可多实例 | 控制面（发布、目录、账务、对账）、图片生成入口，并**托管两份前端产物** |
| `seeai-worker` | 可多实例 | 领取 Job、调上游渠道、落账与结算 |
| PostgreSQL | 1 套（可主从） | 业务事实权威 |
| Redis | 可选 | 加速层（路由候选集与余额缓存）；不配也能跑，功能不变、性能下降 |

图片**不落盘**：请求里的图就是参数值，结果按渠道原形返回。所以除数据库与产物目录外，没有需要持久化的本地状态。

## 1. 投产前的四条硬约束

这四条如果没满足，服务要么起不来、要么跑起来是错的。它们不是"建议"。

### 1.1 `apps/web/dist` 必须在构建 API 之前就位

API 按**编译期路径**找前端产物：

```rust
Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("web").join("dist")
```

后果分两种：

- 构建时目录不存在 → 编译照过；运行时只会记一条 `no web build found; the API serves no front end` 警告，**两个界面都打不开**（API 与 `/v1/*` 仍然正常，所以很容易查错方向）；
- 构建后把二进制搬到别处 → 它仍然去找 `apps/api/../web/dist`，**跟着二进制走的是编译时那个路径**，不是运行目录。

所以顺序是：**先 `npm run build` 出产物，再 `cargo build --release`**，且部署时保持 `apps/api` 与 `apps/web/dist` 的相对位置（或整套目录一起搬）。

### 1.2 前端分发靠主机名，反代要透传 `Host`

两份产物由同一个进程提供，判据是 `Host` 头的第一段：

| 请求的主机名 | 返回 |
| --- | --- |
| 以 `admin` 开头（如 `admin.example.com`） | 运营后台 `console.html` |
| 其余（如 `app.example.com`、`example.com`） | 客户控制台 `portal.html` |

因此反向代理**必须原样透传 `Host`**；改写成固定的 `127.0.0.1:8081` 会让所有请求都回客户控制台，运营后台打不开。另外两个入口有**文件名直达**（`/console.html`、`/portal.html`），任何主机上都有效。

TLS 在反代终止（本服务只监听明文 HTTP）。**用 HTTPS 是必须的**：管理员与客户的会话凭据都走 `Authorization` 头。

### 1.3 数据库用户要有 DDL 权限，且迁移会自动跑

**两个进程各自在启动时跑迁移**（`hub_repository.migrate()`，迁移幂等、由 PG advisory lock 串行化）。所以：

- 连接用的数据库用户需要建表/改表权限，不能是只读账号；
- 升级时**先部署、后观察**即可，不需要单独的迁移步骤；但**多个实例同时冷启动**时它们会争那把锁，表现为其中一个稍慢启动，属正常；
- 回滚要谨慎：迁移是**只进不退**的（没有 down 脚本）。真要回退，得从备份恢复。

### 1.4 `ADMIN_TOKEN` 是必填的共享凭据，它不指向具体的人

API 起不来的第一原因就是它为空。它的作用：`Authorization: Bearer <ADMIN_TOKEN>` 直接当管理员用，**不需要登录**。

| | 登录会话（`ADMIN_EMAIL` / `ADMIN_PASSWORD`） | 共享令牌（`ADMIN_TOKEN`） |
| --- | --- | --- |
| 谁用 | 人在运营后台上登录 | 机器（运维脚本、CI、受控验证） |
| 审计里记的是 | 那个管理员的 id | `None`——**它不指向具体的人** |
| 生产取值 | 强口令 | **强随机串**，按密钥管理，能轮换 |

泄漏共享令牌等于交出全部管理接口，而审计里只看得到"某个用共享令牌的人"。所以它要跟其他密钥一样对待，别写进脚本、别进命令行历史。

## 2. 构建

```sh
# 1) 前端产物（必须在 cargo build 之前，见 §1.1）
npm --prefix apps/web ci
npm --prefix apps/web run build

# 2) 两个二进制
cargo build --release -p seeai-api -p seeai-worker
```

产物：

| 路径 | 是什么 |
| --- | --- |
| `target/release/seeai-api` | API 二进制 |
| `target/release/seeai-worker` | Worker 二进制 |
| `apps/web/dist/` | `console.html`、`portal.html` 与它们引用的 `assets/*` |

**部署包要带上 `apps/web/dist`**，并保持 `target/release/` 与 `apps/web/dist` 的相对关系（即把仓库布局一起带走，或按 §1.1 的路径规则摆好）。

## 3. 启动与探活

两个进程都从环境变量取配置（**不读 `.env` 文件**）。

```sh
DATABASE_URL=... ADMIN_TOKEN=... ADMIN_EMAIL=... ADMIN_PASSWORD=... API_BIND=0.0.0.0:8081 seeai-api
DATABASE_URL=... WORKER_ID=worker-1 seeai-worker
```

| 探活 | 说明 |
| --- | --- |
| `GET /health` | 探数据库（`SELECT 1` 带超时）。可达回 `200 {"status":"ok"}`；不可达回 **503** `{"status":"unhealthy","database":"unreachable"}`。**不探上游渠道** |
| Worker | 没有 HTTP 端口。它的存活看进程与日志：`worker started`，以及轮询期间的错误 |
| 优雅停机 | 两个进程都响应 `Ctrl+C` / `SIGINT`，等待在飞的工作收尾（API 等在处理的请求，Worker 等当前那一轮） |

`/health` 只回答"我这个进程能不能读库"，所以**别用它判上游是否可用**——上游健康是渠道侧的事实，见 `docs/facts/channel-facts.md`。

## 4. 配置项

`RUST_LOG` 控制日志级别（缺省 `info`），输出是 **JSON**。其余按下表。

### 4.1 进程与连接

| 变量 | 缺省 | 生产怎么取 |
| --- | --- | --- |
| `DATABASE_URL` | 无（**必填**） | 两个进程都读。指向生产库，用户需 DDL 权限（§1.3） |
| `API_BIND` | `127.0.0.1:8081` | 反代之后的监听地址；容器里一般是 `0.0.0.0:8081` |
| `ADMIN_TOKEN` | 无（**必填**） | 强随机串，按密钥管理（§1.4） |
| `ADMIN_EMAIL` / `ADMIN_PASSWORD` | 无 | 用来建/更新那个管理员账号。**只在不给 `ADMIN_TOKEN` 的机器上才必须**；给了令牌也必须能登录运营后台，所以生产应配上 |
| `WORKER_ID` | `worker-local-1` | **每个 worker 实例必须唯一**（租约按它归属）。多实例时要显式给不同值 |
| `REDIS_URL` | 空＝不启用 | 与 API 同一个 Redis。不配则全部回源数据库 |
| `CONSOLE_DEV_HOST` | 无 | **生产不要设**。它只为"本机没有域名可指"存在 |
| `HEALTH_PROBE_TIMEOUT_MS` | `2000` | 探活超时。读不出或为 0 退回缺省 |

### 4.2 生成与成本护栏

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `GENERATION_MAX_COST_MICROUSD` | `20000` | **兜底保底额**（microusd） |
| `GENERATION_MAX_REQUEST_COST_MICROUSD` | `10000000` | **单次请求的上游成本上限**（microusd，默认 10 元）。发布期与受理期各判一次。与上一项**不是同一个量** |
| `GENERATION_MAX_DAILY_SPEND_MICROUSD` | `50000000` | **每账户每日**扣费上限（microusd，默认 50 美元等值）。判定每次回账本聚合，超限对客 `429 daily_spend_limit_exceeded` |
| `GENERATION_MAX_CONCURRENT_JOBS` | `1` | **每账户**同时能有多少个在跑的生成任务（**不是**每个 worker 的并发） |
| `GENERATION_RATE_LIMIT_REQUESTS_PER_WINDOW` | `60` | 受理侧限流（每窗口请求数） |
| `GENERATION_RATE_LIMIT_WINDOW_MS` | `60000` | 限流窗口（即默认每分钟 60 次） |
| `GENERATION_SYNC_WAIT_SECONDS` | `120` | 对客同步等待窗口 |
| `PROVIDER_TIMEOUT_SECONDS` | `660` | 上游调用超时 |
| `PROVIDER_TIMEOUT_BASE_SECONDS` | `180` | 超时链的固定基数 |
| `PROVIDER_TIMEOUT_INCLUDED_IMAGES` | `4` | 基数里已含的产出张数 |
| `PROVIDER_TIMEOUT_PER_IMAGE_SECONDS` | `30` | 超出基数后每张追加的秒数 |
| `GENERATION_RETRY_MAX_ATTEMPTS` | `3` | 安全重投上限。**只在可证明上游没有受理时重投**；状态不确定一律不重投，进对账 |
| `GENERATION_RETRY_BACKOFF_BASE_MS` | `1000` | 重投退避基数 |

> 超时链是**启动时校验**的：API 会读已发布合同声明的最大输出张数，算一遍整条链，**不一致就拒绝启动并点名**。所以升级 `PROVIDER_TIMEOUT_*` 时要连同发布侧一起想清楚——它不是"调大就更快"。

### 4.3 Worker 租约与轮询

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `WORKER_POLL_INTERVAL_MS` | `1000` | 轮询间隔 |
| `WORKER_LEASE_SECONDS` | `900` | 领取租约时长。**过短**会让长任务被另一个 worker 抢走；**过长**让崩溃后的 Job 迟迟不恢复 |

### 4.4 加速层（Redis 启用时）

| 变量 | 缺省 |
| --- | --- |
| `CACHE_ROUTE_TTL_SECONDS` | `60` |
| `CACHE_BALANCE_TTL_SECONDS` | `360` |
| `CACHE_FRESHNESS_WINDOW_MS` | `5000` |
| `CACHE_RECONCILE_INTERVAL_MS` | `180000` |
| `CACHE_OPERATION_TIMEOUT_MS` | `200` |

缓存**不是事实来源**：它对不上就回源数据库，不可达时报 miss 而不是挂住。所以 Redis 故障是**降级**，不是故障。

### 4.5 渠道凭证

| 变量 | 说明 |
| --- | --- |
| `AIHUBMIX_API_KEY` / `APIMART_API_KEY` | 变量名由发布素材里的 `credential_env` 指定 |

**凭证只从环境变量读**，数据库只存变量名，日志与响应里不出现。缺哪个渠道的密钥，那个渠道的 Job 会失败——不是在启动时失败。

### 4.6 供给素材导入

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `SUPPLY_MATERIAL_DIR` | 空＝不导入 | 设了之后，**API 每次启动**都会在迁移之后把目录里的 `*.json` 幂等写成渠道与 Offering（匹配键见 `docs/design/0012-platform-model-publishing.md` §3） |

这是**工程侧**的开关：渠道与供给由工程师随素材配一次，运营在后台**选**它们、给价。生产上给不给它，取决于你们的渠道接入流程是"随发布包走"还是"用管理接口配"——**目录不存在或没有素材时静默跳过**，所以不设它不会让服务起不来，只是可选供给清单会是空的。

### 4.7 账本巡检与告警

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `LEDGER_AUDIT_ENABLED` | 开 | 关掉时这一层**根本不挂起来**（不是挂起来空转） |
| `LEDGER_AUDIT_INTERVAL_MS` | `900000`（15 分钟） | 巡检周期 |
| `PROVIDER_ALERT_WEBHOOK` | 空＝没有出口 | 平台侧故障告警的 POST JSON 出口。**没有默认地址、代码里不写死 URL**；地址写错在启动时就失败，不让进程带着一个"永远发不出去"的出口跑 |
| `PROVIDER_ALERT_CONSECUTIVE_FAILURES` | `3` | 某候选连续失败几次才告警。必须 ≥ 1 |
| `PROVIDER_ALERT_TIMEOUT_MS` | `5000` | 告警投递超时 |
| `PROVIDER_ALERT_RETRIES` | `2` | 告警投递重试次数。两者都**有界**——告警是旁路，不能因为对端不响应把发它的那一轮拖住 |

告警请求体只带**定位字段**（哪次执行失败、哪笔账实不符），**不带凭证、提示词与图片**。

## 5. 会话与口令

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `SESSION_TTL_SECONDS` | `43200`（12 小时） | 管理员与客户会话的有效期 |
| `PASSWORD_RESET_TTL_SECONDS` | `1800`（30 分钟） | 口令重置令牌的有效期。**比会话短得多**：它能改口令，暴露窗口越小越好 |

重置令牌与 API Key 都是**只存哈希**的：明文只在签发那一次回给调用方，之后无法再取出。

## 6. 备份

```sh
DATABASE_URL=... pwsh scripts/backup/pg-backup.ps1 [-TargetDir <目录>] [-RetentionDays <天数>] [-Database <库名>]
```

- 连接串**只从 `DATABASE_URL` 读**，不从参数读、不打印：转储里含业务数据，凭据不进命令行历史；
- 导出方式自动选：`pg_dump` 在 PATH 就用它，否则回落到 `docker compose exec postgres`；两者都失败时**明确报错**，不静默产出空文件；
- 按保留天数清掉更旧的转储。

**要恢复时注意**：迁移只进不退（§1.3），所以恢复的目标版本必须与备份时的 schema 一致，或者先把代码退到那个版本再恢复。

## 7. 投产前的演练

按顺序做完，每一条都有明确判据：

| # | 做什么 | 判据 |
| --- | --- | --- |
| 1 | 按 §2 构建并部署，起 API | `GET /health` 回 `200 {"status":"ok"}` |
| 2 | 浏览器打开 `https://admin.<域名>/` | 出现**运营后台**登录页（不是客户控制台——那说明 `Host` 没透传，§1.2） |
| 3 | 同一浏览器打开 `https://app.<域名>/` | 出现**客户控制台** |
| 4 | 起 worker，看日志 | `worker started`，且没有反复刷新的错误 |
| 5 | 在运营后台发一个平台模型（选厂商 → 勾供给 → 给价） | 模型目录里出现它；`GET /v1/models` 也列出它 |
| 6 | 用**假上游**或在受控额度下跑一次真实生成 | Job 完成、账本有 `hold`/`release`/`capture` 三条、余额变化与用量对得上 |
| 7 | 把一个平台模型停用 | 它从 `GET /v1/models` 消失；用它受理得到"模型不存在" |
| 8 | 从备份恢复到另一个库，指向它起一次 API | 能起来、能登录、目录与账务与备份时一致 |

**第 6 条会真的调用上游并计费**，按 `AGENTS.md` 的约定：真实 Provider 调用必须显式批准并限制次数。想只验管道就指向本地假上游——那验的是"链路通不通"，不是"上游通不通"。

## 8. 与开发环境的差异

| | 开发（见[开发环境](development.md)） | 生产 |
| --- | --- | --- |
| 端口 | Postgres `54329`、Redis `63799`、API `8081` | 自定；Redis 可选 |
| 凭证 | 可以留空（不调上游就没用） | 必须是真密钥，按密钥管理 |
| `ADMIN_TOKEN` | 任意非空 | 强随机、可轮换（§1.4） |
| 前端产物 | 本机构建后即可 | **必须在构建 API 之前就位**（§1.1） |
| `CONSOLE_DEV_HOST` | 可用 | **不设**（§4.1） |
| TLS | 不需要 | 反代终止，必须 HTTPS（§1.2） |
| 迁移 | 进程启动自动跑 | 同上；多实例冷启动会争锁（§1.3） |
