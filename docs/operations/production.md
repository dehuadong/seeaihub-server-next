# 生产环境

两个进程 + 两个依赖：

| 组件 | 数量 | 职责 |
| --- | --- | --- |
| `seeai-api` | 可多实例 | 控制面（发布、目录、账务、对账）、图片生成入口，并**托管两份前端产物** |
| `seeai-worker` | 可多实例 | 领取 Job、调上游渠道、落账与结算 |
| PostgreSQL 17 | 1 套（可主从） | 业务事实权威 |
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
- 构建后把二进制搬到别处 → 它仍然去找**编译时那个绝对路径**下的 `apps/api/../web/dist`，**跟着二进制走的是编译时路径**，不是运行目录。

所以顺序是：**先 `npm run build` 出产物，再 `cargo build --release`**；运行时必须能在编译时那个**绝对路径**下找到 `apps/web/dist`。在目标机同一路径构建最省事（§2.3），把二进制或整个目录搬到别的绝对路径都不行。

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

生成用 `openssl rand -hex 32`（256 位、64 个十六进制字符）之类，别手敲。

**轮换**：API 只在启动时读一次并常驻内存（与请求里的令牌做 `constant_time_eq` 比较），所以改环境变量后**重启 API** 即可——不动数据库，也不会让已登录的管理员或客户会话失效。多实例滚动重启期间新旧令牌会短时并存；要严格一次切换就挑停服窗口。轮换后审计里仍然只记 `None`，"这次是谁做的"依然答不出来——这正是它只该给机器用的原因。

## 2. 依赖、构建与部署

### 2.1 依赖准备

**PostgreSQL 17**（业务事实权威，§1.3 要它）。Ubuntu 24.04 源里是 16，先加 PGDG：

```sh
sudo install -d /usr/share/postgresql-common/pgdg
sudo curl -fsSL -o /usr/share/postgresql-common/pgdg/apt.postgresql.org.asc https://www.postgresql.org/media/keys/ACCC4CF8.asc
. /etc/os-release
echo "deb [signed-by=/usr/share/postgresql-common/pgdg/apt.postgresql.org.asc] https://apt.postgresql.org/pub/repos/apt $VERSION_CODENAME-pgdg main" | sudo tee /etc/apt/sources.list.d/pgdg.list
sudo apt-get update && sudo apt-get install -y postgresql-17
```

建角色与库。生产角色**不需要** `CREATEDB`（那是开发机给契约测试与 e2e 派生库用的）；库的 owner 已经够跑迁移所需的建 schema、建表：

```sh
sudo -u postgres psql -p 5432 -c "CREATE ROLE seeai LOGIN PASSWORD '<强口令>';"
sudo -u postgres psql -p 5432 -c "CREATE DATABASE seeai_next OWNER seeai;"
```

`DATABASE_URL=postgres://seeai:<强口令>@<主机>:5432/seeai_next`。跨机访问按你们的网络策略配 `listen_addresses` 与 `pg_hba.conf`，不要对公网开放。

**Redis 7（可选）**：装上并让 `REDIS_URL` 指过去，例如 `sudo apt-get install -y redis-server` 加 `REDIS_URL=redis://127.0.0.1:6379`。它是加速层，故障只算降级（缓存读不到按 miss 处理），不必为它做高可用来保可用性。

### 2.2 构建顺序

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

### 2.3 部署布局与编译期路径

API 找前端的路径在**编译时**就写死成 `<构建目录>/apps/web/dist`（§1.1），**不是运行目录**。最省事的做法是：在运行它的那台机器、同一个绝对路径里构建。

```sh
sudo useradd --system --create-home --home-dir /opt/seeai --shell /usr/sbin/nologin seeai
sudo -u seeai git clone <仓库地址> /opt/seeai
sudo -u seeai sh -c 'cd /opt/seeai && npm --prefix apps/web ci && npm --prefix apps/web run build && cargo build --release -p seeai-api -p seeai-worker'
```

部署后的布局：

| 路径 | 是什么 |
| --- | --- |
| `/opt/seeai/apps/web/dist/` | 两份前端产物，与二进制的编译路径同源 |
| `/opt/seeai/target/release/seeai-api` | API 二进制 |
| `/opt/seeai/target/release/seeai-worker` | Worker 二进制 |
| `/etc/seeai/api.env`、`/etc/seeai/worker.env` | 环境变量（§2.4），属 `seeai`、`chmod 600` |

**在 CI 构建再拷二进制**也能用，但必须保证编译时那个绝对路径在服务器上仍然存在（把 `apps/web/dist` 放到编译时的同一位置），否则界面打不开——就是 §1.1 的两种后果之一。

### 2.4 systemd 单元

两个进程都用系统用户 `seeai` 跑，工作目录设成构建目录，配置从 `EnvironmentFile` 读。文件是每行 `KEY=VALUE`（systemd 自己解析，不做 shell 展开）：

```sh
# /etc/seeai/api.env
DATABASE_URL=postgres://seeai:<强口令>@127.0.0.1:5432/seeai_next
ADMIN_TOKEN=<openssl rand -hex 32>
ADMIN_EMAIL=ops@example.com
ADMIN_PASSWORD=<强口令>
API_BIND=127.0.0.1:8081
RUST_LOG=info
```

```ini
# /etc/systemd/system/seeai-api.service
[Unit]
Description=SeeAI Hub API
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=seeai
Group=seeai
WorkingDirectory=/opt/seeai
EnvironmentFile=/etc/seeai/api.env
ExecStart=/opt/seeai/target/release/seeai-api
Restart=on-failure
RestartSec=3
# 给在飞请求收尾；应不小于 GENERATION_SYNC_WAIT_SECONDS。进程处理 SIGINT 与 SIGTERM。
TimeoutStopSec=720

[Install]
WantedBy=multi-user.target
```

```ini
# /etc/systemd/system/seeai-worker.service
[Unit]
Description=SeeAI Hub Worker
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=seeai
Group=seeai
WorkingDirectory=/opt/seeai
EnvironmentFile=/etc/seeai/worker.env
ExecStart=/opt/seeai/target/release/seeai-worker
Restart=on-failure
RestartSec=3
# 给手上那一轮跑完；至少给到一次上游调用的最坏耗时 PROVIDER_TIMEOUT_SECONDS。
# 给不够也不丢事实：强杀后由过期租约回收（§1.3）。
TimeoutStopSec=720

[Install]
WantedBy=multi-user.target
```

`worker.env` 比 `api.env` 多两样：唯一的 `WORKER_ID`，以及渠道密钥（[配置项](configuration.md) §5）。启用：

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now seeai-api seeai-worker
```

日志走 stdout、由 journald 收（JSON）：`journalctl -u seeai-api -f`。

### 2.5 反向代理与 TLS

反代只做两件事：**终止 TLS** 与**保留 `Host`**。两份前端产物由 API 自己按主机名分发，反代里**不要**配 `root` 或 `try_files`（§1.2）。

```nginx
# /etc/nginx/sites-available/seeai
server {
    listen 80;
    listen [::]:80;
    server_name admin.example.com app.example.com;
    return 301 https://$host$request_uri;
}

server {
    listen 443 ssl;
    listen [::]:443 ssl;
    server_name admin.example.com app.example.com;

    ssl_certificate     /etc/letsencrypt/live/example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/example.com/privkey.pem;

    # API 的请求体上限是 16MB（参考图/遮罩走 multipart 或 data URL）；nginx 默认 1m 会先在它这里 413。
    client_max_body_size 16m;

    # 一次生成对客是同步的，最长等到 GENERATION_SYNC_WAIT_SECONDS 才回；nginx 默认 60s 会提前切断。
    proxy_read_timeout 720s;
    proxy_send_timeout 720s;

    location / {
        proxy_pass http://127.0.0.1:8081;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }
}
```

```sh
sudo ln -s /etc/nginx/sites-available/seeai /etc/nginx/sites-enabled/seeai
sudo nginx -t && sudo systemctl reload nginx
```

要点：

- 两个域名共用一份配置：分发由 API 按 `Host` 决定，反代不区分；**别把 `Host` 改写成 `127.0.0.1`**，否则运营后台打不开；
- `client_max_body_size` 不小于 API 的 16MB；`proxy_read_timeout` 不小于 `GENERATION_SYNC_WAIT_SECONDS`（默认是 `PROVIDER_TIMEOUT_SECONDS + 30`）；
- API 绑在回环（`API_BIND=127.0.0.1:8081`），只让 nginx 够得到；证书用 certbot 之类签发即可。

## 3. 启动与探活

配置全部从环境变量读，文件与 systemd 单元见 §2.4。进程启动时的 `dotenvy::dotenv()` 只是兜底：它从**当前工作目录**往上找 `.env` 并载入，但**不覆盖**已有环境变量——生产不要放 `.env` 进部署目录。前台直接跑也可以（值由 shell 或密钥系统给）：

```sh
DATABASE_URL=... ADMIN_TOKEN=... ADMIN_EMAIL=... ADMIN_PASSWORD=... API_BIND=0.0.0.0:8081 seeai-api
DATABASE_URL=... WORKER_ID=worker-1 seeai-worker
```

| 探活 | 说明 |
| --- | --- |
| `GET /health` | 探数据库（`SELECT 1` 带超时）。可达回 `200 {"status":"ok"}`；不可达回 **503** `{"status":"unhealthy","database":"unreachable"}`。**不探上游渠道** |
| Worker | 没有 HTTP 端口。它的存活看进程与日志：`worker started`，以及轮询期间的错误 |
| 优雅停机 | 两个进程都处理 `SIGINT`（Ctrl+C）与 `SIGTERM`（systemd、容器默认），等飞完手头的工作再退（API 等在处理的请求，Worker 等当前那一轮） |

`/health` 只回答"我这个进程能不能读库"，所以**别用它判上游是否可用**——上游健康是渠道侧的事实，见 `docs/facts/channel-facts.md`。

## 4. 配置项

全部配置项（每个变量的缺省、含义、生产取值、失败方式）见[配置项](configuration.md)。这里只留两条上线会踩的：

- **超时链是启动时一起校验的**：API 读已发布合同声明的最大输出张数，算一遍 `PROVIDER_TIMEOUT_*`、`WORKER_LEASE_SECONDS`、`GENERATION_SYNC_WAIT_SECONDS` 整条链，不一致就拒绝启动并点名——它不是"调大就更快"（见[配置项](configuration.md) §2）。
- **渠道密钥只给 worker**：凭证只从环境变量读，数据库只存变量名；API 不调上游、不需要它们（见[配置项](configuration.md) §5）。

## 5. 会话与口令

| 变量 | 缺省 | 说明 |
| --- | --- | --- |
| `SESSION_TTL_SECONDS` | `43200`（12 小时） | 管理员与客户会话的有效期 |
| `PASSWORD_RESET_TTL_SECONDS` | `1800`（30 分钟） | 口令重置令牌的有效期。**比会话短得多**：它能改口令，暴露窗口越小越好 |

重置令牌与 API Key 都是**只存哈希**的：明文只在签发那一次回给调用方，之后无法再取出。

## 6. 备份

```sh
DATABASE_URL=... pwsh scripts/backup/pg-backup.ps1 [-TargetDir <目录>] [-RetentionDays <天数>]
```

- 连接串**只从 `DATABASE_URL` 读**，不从参数读、不打印：转储里含业务数据，凭据不进命令行历史；
- 导出方式用 PATH 上的 `pg_dump`；找不到或导出失败时**明确报错**，不静默产出空文件；
- 按保留天数清掉更旧的转储。
- 脚本是 PowerShell：服务器上要有 `pwsh`；不装也能直接导一份，命令是 `pg_dump --format=custom --file <文件> "$DATABASE_URL"`。

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
| 端口 | Postgres `5432`、Redis `6379`、API `8081` | 自定；Redis 可选 |
| 凭证 | 可以留空（不调上游就没用） | 必须是真密钥，按密钥管理 |
| `ADMIN_TOKEN` | 任意非空 | 强随机、可轮换（§1.4） |
| 前端产物 | 本机构建后即可 | **必须在构建 API 之前就位**（§1.1） |
| `CONSOLE_DEV_HOST` | 可用 | **不设**（[配置项](configuration.md) §1） |
| TLS | 不需要 | 反代终止，必须 HTTPS（§1.2） |
| 迁移 | 进程启动自动跑 | 同上；多实例冷启动会争锁（§1.3） |
| 依赖 | 本机系统包安装的 PG17 与 Redis（§2.1） | 同样要 PG17，Redis 可选；按 §2.1 准备 |
| 进程管理 | 两个终端 `cargo run` | systemd 两个单元（§2.4） |
| 配置注入 | 仓库根 `.env`（进程自己读） | `EnvironmentFile` + 密钥系统（§2.4） |
| 日志 | 终端 stdout | journald，`journalctl -u seeai-api` |

## 9. 升级与重启

- 迁移在两个进程启动时自动跑（§1.3），升级就是换二进制再重启；
- `systemctl restart seeai-api` 发 `SIGINT`、走排空（§2.4、§3），不需要单独的停机脚本；多实例逐台重启，避免全部同时冷启动去争迁移锁（争锁只表现为启动稍慢）；
- 迁移只进不退：回滚只能从备份恢复（§1.3），所以升级前先做一次备份（§6）。
