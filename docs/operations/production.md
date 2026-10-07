# 生产环境

两个进程 + 两个依赖：

| 组件 | 数量 | 职责 |
| --- | --- | --- |
| `seeai-api` | 可多实例 | 控制面（发布、目录、账务、对账）、图片生成入口，并**托管两份前端产物** |
| `seeai-worker` | 可多实例 | 领取 Job、调上游渠道、落账与结算 |
| PostgreSQL 17 | 1 套（可主从） | 业务事实权威 |
| Redis | 可选 | 加速层（路由候选集与余额缓存）；不配也能跑，功能不变、性能下降 |

图片**不落盘**：请求里的图就是参数值，结果按渠道原形返回。所以除数据库与产物目录外，没有需要持久化的本地状态。

部署有两种方式：传统方式（systemd，本文）与容器（[生产环境（容器）](production-docker.md)）；配置注入、反代与迁移口径一致。

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

所以顺序是：**先 `npm run build` 出产物，再 `cargo build --release`**；运行时必须能在编译时那个**绝对路径**下找到 `apps/web/dist`。打包脚本（§2.2）与就地构建（§2.3）都把仓库放在这个路径上编译；把二进制或整个目录搬到别的绝对路径都不行。

### 1.2 前端分发靠主机名，反代要透传 `Host`

两份产物由同一个进程提供，判据是 `Host`（去掉端口后的整串）：

| 请求的主机名 | 返回 |
| --- | --- |
| 以 `admin` 开头（如 `admin.example.com`） | 运营后台 `console.html` |
| 其余（如 `app.example.com`、`example.com`） | 客户控制台 `portal.html` |

**不要求两个二级域名**：只有 `admin` 前缀是判据，`app` 只是习惯叫法——同一个二级域名下拿 `admin.example.com` 配裸域 `example.com` 也行，甚至可以用另一个域名。边界有三条：

- 是**整串前缀**匹配，不是"第一段等于 `admin`"：`administrator.example.com` 也回运营后台，而 `foo.admin.example.com` 不回；
- 匹配**区分大小写**：`Admin.example.com` 会落到客户入口；经 nginx 时 `$host` 已小写化，直连 API 要自己保证；
- 端口会被去掉，`admin.example.com:8443` 仍判运营后台。

因此反向代理**必须原样透传 `Host`**；改写成固定的 `127.0.0.1:8081` 会让所有请求都回客户控制台，运营后台打不开。另外两个入口有**文件名直达**（`/console.html`、`/portal.html`），精确文件名优先于主机名判据，在任何主机上都有效。

TLS 在反代终止（本服务只监听明文 HTTP）。**用 HTTPS 是必须的**：管理员与客户的会话凭据都走 `Authorization` 头。

### 1.3 数据库用户要有 DDL 权限，且迁移会自动跑

**两个进程各自在启动时跑迁移**（`hub_repository.migrate()`，迁移幂等、由 PG advisory lock 串行化）。所以：

- 迁移由 `sqlx::migrate!` 在编译期嵌进二进制，运行不需要 `migrations/`；
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

### 2.2 打包与部署

主路径是**在构建机出包，生产机只解包**。二进制把编译期路径写死（§1.1），所以构建时仓库要在 `/opt/seeai`、包也只能解到这个路径。

**构建机前置**：Node 24、Rust 1.94+、C 工具链，以及 `cmake`、`perl`、`pkg-config`（aws-lc-rs 要从源码构建）、`bubblewrap`：

```sh
sudo apt-get install -y build-essential cmake perl pkg-config bubblewrap binutils
```

```sh
build/package.sh
```

`--version` 可指定包名里的版本，缺省用 `git describe`。

脚本做四件事：构建两份前端产物；用 bubblewrap 把仓库挂到 `/opt/seeai` 再编译两个二进制，把 `/opt/seeai` 编进路径；组装 `build/dist/seeai-<版本>.tar.gz`；量出二进制要求的最高 GLIBC 版本。脚本会核对二进制里确实留下了 `/opt/seeai/apps/api`，对不上直接失败。`--skip-web`、`--skip-build` 复用已有的前端产物与二进制，`--out` 换输出目录。

**服务器前置**：与构建机同 CPU 架构，且 glibc 不低于包内 `PACKAGE.txt` 记的 `required-glibc`。任一条不满足就别用这个包，改用 §2.3 在服务器上构建。

把包传到服务器，再建服务账号并解包。服务账号只用来**跑**进程，不参与构建，所以它不需要 Node/Rust，也不需要 home：

```sh
scp build/dist/seeai-<版本>.tar.gz <服务器>:/tmp/

sudo useradd --system --user-group --shell /usr/sbin/nologin seeai
sudo install -d -m 755 /opt/seeai
sudo tar -xzf seeai-<版本>.tar.gz -C /opt/seeai --strip-components=1
```

包由 `build/package.sh` 组装；进程运行时读下面这些路径（目录 755、文件 644 就够；要让 `seeai` 拥有它就 `chown -R seeai:seeai /opt/seeai`）：

| 路径 | 是什么 |
| --- | --- |
| `/opt/seeai/target/release/seeai-api` | API 二进制 |
| `/opt/seeai/target/release/seeai-worker` | Worker 二进制 |
| `/opt/seeai/apps/web/dist/` | 前端产物 |
| `/opt/seeai/apps/api/` | 空目录：路径里的 `..` 要先能进它（§1.1） |
| `/opt/seeai/public-docs/` | 对客公开文档，也是素材 `narrative_path` 的解析根 |
| `/opt/seeai/config/bootstrap/` | 供给素材（`SUPPLY_MATERIAL_DIR` 的缺省值） |

迁移已编进二进制，运行不需要 `migrations/`（§1.3）。包里另有 `deploy/systemd/` 两个单元与 `PACKAGE.txt`（版本、提交、架构与 glibc 记录）。

`/etc/seeai/api.env`、`/etc/seeai/worker.env` 见 §2.4。

### 2.3 就地构建（备选）

没有构建机、或服务器 glibc 比包要求的旧时，在生产机上直接构建。工具链同 §2.2（不需要 `bubblewrap`）。顺序不能反：**先出前端产物，再编译二进制**（§1.1）。

把仓库放在 `/opt/seeai`，这个路径就是编译时的路径根：

```sh
sudo git clone <仓库地址> /opt/seeai      # /opt/seeai 必须为空
cd /opt/seeai
sudo npm --prefix apps/web ci
sudo npm --prefix apps/web run build
sudo cargo build --release -p seeai-api -p seeai-worker
```

产物：

| 路径 | 是什么 |
| --- | --- |
| `target/release/seeai-api` | API 二进制 |
| `target/release/seeai-worker` | Worker 二进制 |
| `apps/web/dist/` | `console.html`、`portal.html` 与它们引用的 `assets/*` |
| `public-docs/`、`config/bootstrap/` | 运行时读的公开文档与供给素材 |

用有 Node/Rust 工具链的账号构建；跑进程的 `seeai` 账号只读这棵树。

### 2.4 systemd 单元

两个进程都用系统用户 `seeai` 跑，工作目录 `/opt/seeai`，配置从 `EnvironmentFile` 读。文件是每行 `KEY=VALUE`（systemd 自己解析，不做 shell 展开）。单元文件随包带，源码在 [`deploy/systemd/`](../../deploy/systemd/)：

```sh
sudo install -m 644 /opt/seeai/deploy/systemd/seeai-api.service /etc/systemd/system/seeai-api.service
sudo install -m 644 /opt/seeai/deploy/systemd/seeai-worker.service /etc/systemd/system/seeai-worker.service
```

单元的停机宽限（`TimeoutStopSec`）与理由写在单元文件里。

先建配置目录与两份 env（属服务账号、`600`——里面有口令）。**必填**项与取值见[配置项](configuration.md)（§1、§8、§9）；其余变量代码都留了缺省，只有要覆盖缺省才写。下面示例把必填项都写上：

```sh
sudo install -d -m 750 -o seeai -g seeai /etc/seeai
sudo install -m 600 -o seeai -g seeai /dev/null /etc/seeai/api.env
sudo install -m 600 -o seeai -g seeai /dev/null /etc/seeai/worker.env

sudo tee /etc/seeai/api.env >/dev/null <<'EOF'
DATABASE_URL=postgres://seeai:<强口令>@127.0.0.1:5432/seeai_next
ADMIN_TOKEN=<openssl rand -hex 32>
ADMIN_EMAIL=ops@example.com
ADMIN_PASSWORD=<强口令>
SEE_BASEURL=https://app.<域名>
CUSTOMER_HISTORY_CURSOR_KEY=<openssl rand -base64 32>
REQUEST_FINGERPRINT_KEY_V1=<openssl rand -base64 32>
API_BIND=127.0.0.1:8081
RUST_LOG=info
SUPPLY_MATERIAL_DIR=config/bootstrap
EOF

sudo tee /etc/seeai/worker.env >/dev/null <<'EOF'
DATABASE_URL=postgres://seeai:<强口令>@127.0.0.1:5432/seeai_next
WORKER_ID=worker-1
AIHUBMIX_API_KEY=<渠道密钥>
APIMART_API_KEY=<渠道密钥>
RUST_LOG=info
EOF

sudo systemctl daemon-reload
sudo systemctl enable --now seeai-api seeai-worker
```

`SUPPLY_MATERIAL_DIR`（相对 `WorkingDirectory=/opt/seeai`）让 API 每次启动都把仓库里的供给素材幂等导成渠道与 Offering；**不设时默认就是 `config/bootstrap`**，上面显式写出来只是为了部署文件可读——要换成自己的素材，把它指到挂载卷（[配置项 §6](configuration.md#6-供给素材导入)）。它只被 API 读，worker 不需要。

`worker.env` 比 `api.env` 多两样：每个实例唯一的 `WORKER_ID`，以及渠道密钥（[渠道凭证](configuration.md#5-渠道凭证)）。

改过 env 文件后要让新值生效，得 `sudo systemctl restart seeai-api seeai-worker`——进程只在启动时读一次环境变量。

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

    # 请求体上限要同时盖住两条入口：生成入口 16 MiB（参考图与遮罩只以公网 URL 文本随正文提交），
    # 上传路由的 UPLOAD_MAX_REQUEST_BYTES（默认 21 MiB，单文件上限 20 MiB 加 multipart 协议余量）。
    # nginx 默认 1m 会先在它这里 413。示例 22m 盖住两者；上传上限的合同见
    # docs/specs/0007-image-upload-and-object-storage.md §2.2。
    client_max_body_size 22m;

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
- `client_max_body_size` 要同时盖住生成入口的 16 MiB 正文上限与上传路由的 `UPLOAD_MAX_REQUEST_BYTES`（默认 21 MiB；示例 `22m`）；`proxy_read_timeout` 不小于 `GENERATION_SYNC_WAIT_SECONDS`（默认是 `PROVIDER_TIMEOUT_SECONDS + 30`）；
- 上限不足时上传请求会被反向代理先回 `413`；上传路由的上限合同与失败后果见[图片上传与对象存储 Spec](../specs/0007-image-upload-and-object-storage.md) §2.2；
- API 绑在回环（`API_BIND=127.0.0.1:8081`），只让 nginx 够得到；证书用 certbot 之类签发即可。
- 公开鉴权端点的来源维默认用**连接对端地址**；部署在反代之后时对端是 nginx 本身，要在 API 进程配
  `AUTH_SOURCE_HEADER=x-real-ip`（nginx 用 `$remote_addr` 覆盖写它）才能区分真实客户端。**采信这个头
  的前提是 API 不能被绕过代理直连**（示例已把 API 绑在回环）；能直连的调用方可以自带同名头伪造来源。

## 3. 启动与探活

配置全部从环境变量读，文件与 systemd 单元见 §2.4。进程启动时的 `dotenvy::dotenv()` 只是兜底：它从**当前工作目录**往上找 `.env` 并载入，但**不覆盖**已有环境变量——生产不要放 `.env` 进部署目录。前台直接跑也可以（值由 shell 或密钥系统给）：

```sh
DATABASE_URL=... ADMIN_TOKEN=... SEE_BASEURL=... CUSTOMER_HISTORY_CURSOR_KEY=... REQUEST_FINGERPRINT_KEY_V1=... ADMIN_EMAIL=... ADMIN_PASSWORD=... API_BIND=0.0.0.0:8081 seeai-api
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

- **超时链是启动时一起校验的**：API 与 Worker 都读已发布合同声明的最大输出张数，算一遍 `PROVIDER_TIMEOUT_*` 与 `GENERATION_SYNC_WAIT_SECONDS` 整条链，不一致就拒绝启动并点名——它不是"调大就更快"（见[生成与成本护栏](configuration.md#2-生成与成本护栏)）。
- **渠道密钥两条进程都要有**：API 直接执行时读它，对账 Worker 重查上游状态时读同一份；凭证只从环境变量读，数据库只存变量名（见[渠道凭证](configuration.md#5-渠道凭证)）。

## 5. 会话与口令

会话与口令重置的有效期是配置项（`SESSION_TTL_SECONDS`、`PASSWORD_RESET_TTL_SECONDS`），见[会话与口令](configuration.md#8-会话与口令)。

## 6. 备份

在装了 `pg_dump` 的机器上导（服务器随 PostgreSQL 一起有）：

```sh
pg_dump --format=custom --file <文件> "$DATABASE_URL"
```

- 用与服务器 PostgreSQL 同大版本的 `pg_dump`；
- `DATABASE_URL` 带口令，`pg_dump` 会把整串放进命令行（`ps` 可见）；要避免就用 `.pgpass`，或用 `PGPASSWORD` 配合 `-h`/`-U`/`-d` 分开给；
- 导出失败时**明确报错**，不静默产出空文件。

仓库里另有 `scripts/backup/pg-backup.ps1`（PowerShell，按保留天数清旧转储）：它随检出走，不在部署包里；要用得先有 `pwsh`。

**要恢复时注意**：迁移只进不退（§1.3），所以恢复的目标版本必须与备份时的 schema 一致，或者先把代码退到那个版本再恢复。

## 7. 投产前的演练

按顺序做完，每一条都有明确判据：

| # | 做什么 | 判据 |
| --- | --- | --- |
| 1 | 按 §2.2 部署（构建机出包，或 §2.3 就地构建），起 API | `GET /health` 回 `200 {"status":"ok"}` |
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
| `SEE_BASEURL` | 本机 `http://app.localhost:8081` | 对客域名 `https://app.<域名>`，只写源 |
| 前端产物 | 本机构建后即可 | **必须在构建 API 之前就位**（§1.1） |
| `CONSOLE_DEV_HOST` | 可用 | **不设**（[进程与连接](configuration.md#1-进程与连接)） |
| TLS | 不需要 | 反代终止，必须 HTTPS（§1.2） |
| 迁移 | 进程启动自动跑 | 同上；多实例冷启动会争锁（§1.3） |
| 依赖 | 本机系统包安装的 PG17 与 Redis（§2.1） | 同样要 PG17，Redis 可选；按 §2.1 准备 |
| 进程管理 | 两个终端 `cargo run` | systemd 两个单元（§2.4） |
| 配置注入 | 仓库根 `.env`（进程自己读） | `EnvironmentFile` + 密钥系统（§2.4） |
| 日志 | 终端 stdout | journald，`journalctl -u seeai-api` |

## 9. 升级与重启

- 迁移在两个进程启动时自动跑（§1.3），升级就是解新包覆盖 `/opt/seeai` 再重启（§2.2）；
- `systemctl restart seeai-api` 发 `SIGINT`、走排空（§2.4、§3），不需要单独的停机脚本；多实例逐台重启，避免全部同时冷启动去争迁移锁（争锁只表现为启动稍慢）；
- 应用回退用上一版包覆盖；数据库迁移只进不退，回退数据库只能从备份恢复（§1.3）。升级前先做一次备份（§6）；
- 单元文件有改动时重新装到 `/etc/systemd/system/` 再 `systemctl daemon-reload`（§2.4）——解包只覆盖 `/opt/seeai`，不动 `/etc`。
