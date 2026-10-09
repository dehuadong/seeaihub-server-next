# 部署总览与基础准备

两个进程 + 两个依赖：

| 组件 | 数量 | 职责 |
| --- | --- | --- |
| `seeai-api` | 可多实例 | 控制面（发布、目录、账务、对账）、图片生成入口，并**托管两份前端产物** |
| `seeai-worker` | 可多实例 | 领取 Job、调上游渠道、落账与结算 |
| PostgreSQL 17 | 1 套（可主从） | 业务事实权威 |
| Redis | 可选 | 加速层（路由候选集与余额缓存）；不配也能跑，功能不变、性能下降 |

图片**不落盘**：请求里的图就是参数值，结果按渠道原形返回。所以除数据库与产物目录外，没有需要持久化的本地状态。

## 1. 选哪种方式

部署有两种方式，**只有「进程怎么起」与「产物怎么来」不同**：配置项、反向代理、迁移、备份与演练判据都一样。

| | systemd（传统） | 容器 |
| --- | --- | --- |
| 进程管理 | 两个 systemd 单元 | 两个容器（`restart: unless-stopped`） |
| 产物 | 宿主上编译出的二进制与前端产物 | 多阶段构建进镜像 |
| 编译期路径 | 宿主上的仓库绝对路径（§3.1） | 镜像内部固定 `/app` |
| 配置注入 | `EnvironmentFile` | `env_file` |
| 停机 | 默认 SIGTERM + `TimeoutStopSec` | 默认 SIGTERM + `stop_grace_period` |
| 日志 | journald | `docker logs` 或采集器 |
| 文档 | [生产环境（systemd）](production.md) | [生产环境（容器）](production-docker.md) |

本文是两条路共用的部分：服务器基础准备、硬约束、配置内容、反向代理、探活、备份与演练。**先按本文把服务器准备好**，再进所选那条路线。

## 2. 服务器基础准备

### 2.1 系统与网络

示例系统是 Ubuntu 24.04。对外只开 SSH、`80` 与 `443`：

- `80` 只用来跳 `443`，TLS 在反向代理终止（§5）；
- API 绑在回环 `127.0.0.1:8081`（§4），不对外；
- PostgreSQL 与 Redis **不要对公网开放**（§2.2、§2.3）。

域名方面，两个入口都解析到这台机器（§3.2）：一个 `admin` 前缀的域名（如 `admin.example.com`）与一个对客域名（如 `app.example.com` 或裸域）。

### 2.2 PostgreSQL 17

业务事实权威，§3.3 要它。Ubuntu 24.04 源里是 16，先加 PGDG：

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

### 2.3 Redis 7（可选）

装上并让 `REDIS_URL` 指过去，例如 `sudo apt-get install -y redis-server` 加 `REDIS_URL=redis://127.0.0.1:6379`。它是加速层，故障只算降级（缓存读不到按 miss 处理），不必为它做高可用来保可用性。

### 2.4 nginx 与 TLS 证书

```sh
sudo apt-get install -y nginx certbot
```

证书只签不装（配置由 §5 手写，`certonly` 不动 nginx 的 `server` 块）：

```sh
sudo certbot certonly --nginx -d admin.example.com -d app.example.com
```

证书落在 `/etc/letsencrypt/live/<名字>/`，`<名字>` 取第一个 `-d` 的域名，也可以用 `--cert-name` 指定。§5 的配置里那两条 `ssl_certificate` 路径要与它一致。续期由 certbot 的 timer 自动做，续期后 `sudo systemctl reload nginx` 让新证书生效。

### 2.5 代码到服务器上

本节用于在服务器构建：上传的是源码与构建输入，`pack-deploy-tree.mjs` 不包含 Rust 编译产物。开发构建机直接交付产物时，按[systemd 发布包](production.md#13-在开发构建机生成发布包)或[容器镜像](production-docker.md#22-在本机构建)的构建与传输步骤操作，服务器无需检出完整仓库。

有仓库访问权限时直接 clone 到部署根（部署根必须为空）：

```sh
sudo git clone <仓库地址> /opt/seeai
```

没有仓库访问权限时不用 `git clone`，把下面这些传上去即可。**一个包，systemd 与容器两条路线都用它**，部署方式在服务器上再定：

| 传什么 | 为什么 |
| --- | --- |
| `Cargo.toml`、`Cargo.lock` | workspace 清单与锁定版本 |
| `crates/` | 9 个 crate；[workspace](../../Cargo.toml) 清单里列出的都要在，缺一个 cargo 报错 |
| `apps/api/`、`apps/worker/` | 两个二进制的源码 |
| `migrations/` | 编译期嵌入二进制 |
| `public-docs/`、`config/bootstrap/` | 运行期读 |
| `deploy/systemd/` | systemd 路线：装到 `/etc/systemd/system/` 的两个单元 |
| `Dockerfile`、`.dockerignore`、`deploy/compose.prod.yaml` | 容器路线：在部署根 `docker build`，再 compose 起容器 |
| `apps/web/dist/` | 本地打好的前端产物，服务器不装 Node 时直接用它 |
| `apps/web/` 的 `src/`、`console.html`、`portal.html`、`package.json`、`package-lock.json`、`tsconfig.json`、`vite.config.ts`、`e2e/check-bundle-isolation.mjs` | 在服务器上打前端、或镜像内构建时的输入（最后一个是 `npm run build` 的一环，漏了会失败） |

在有仓库的那台机器上打包，清单由 `scripts/deploy/pack-deploy-tree.mjs` 逐项核对：路径缺失、[workspace](../../Cargo.toml) 成员没被覆盖、或 `Dockerfile` 的 `COPY` 要读的路径没进包，它都报错退出，不出一份缺东西的包；`apps/web/dist` 比前端源码旧就警告。

```sh
node scripts/deploy/pack-deploy-tree.mjs
```

`-o <文件>` 改输出路径（默认 `./seeai-deploy-<YYYYMMDD>.tar.gz`），`--help` 看说明。脚本在本地跑，不编译，只收集与核对。

`--no-web-src` 去掉前端源码，只带 `apps/web/dist`（它必须已存在）：包小一点（21 项 → 13 项，2.9 MB → 2.8 MB）。代价两条：

- **要在服务器上构建镜像，就必须用完整包**（不加这个开关）。镜像的 `web` 阶段要前端源码，加了开关的包缺 `apps/web/package.json` 等输入，`docker build` 会在 `COPY` 那步失败；那种包只能配「镜像在别的机器上构建再导入」（[生产环境（容器）§2.3](production-docker.md#23-从别处导入)）。
- 服务器上也不再能打前端，`npm ci`、`npm run build` 必须已经在本地做完。

包传到服务器。`<服务器>` 是 `[用户@]主机`——`root@203.0.113.10`、`root@api.example.com`，或 `~/.ssh/config` 里的别名；SSH 不是默认端口时用大写 `-P`（小写 `-p` 是保留时间戳）：

```sh
scp seeai-deploy-<YYYYMMDD>.tar.gz root@203.0.113.10:/tmp/
scp -P 2222 seeai-deploy-<YYYYMMDD>.tar.gz root@203.0.113.10:/tmp/
```

服务器上解包到部署根，`Cargo.toml`、`crates/` 等直接落在部署根下，没有多一层目录：

```sh
sudo install -d -m 755 /opt/seeai
sudo tar -xzf /tmp/seeai-deploy-<YYYYMMDD>.tar.gz -C /opt/seeai
```

整包约 12 MB，打包后约 2.9 MB。`target/` 与 `apps/web/node_modules/` 不传，都在服务器上重新生成——`node_modules` 带平台相关二进制，传过去也不能用。`out-reference/`、`docs/`、`.agents/`、`.github/`、`scripts/` 与 e2e 的其余文件构建与运行都不读；`.env` 不传，生产配置见 §4。构建要从 crates.io 下载（在服务器上打前端时还要 npm registry），服务器连不上就先配镜像源。

部署根里这些路径是运行时要读的（目录 755、普通文件 644、二进制 755；容器方式在镜像里是同一套相对布局，根是 `/app`）：

| 路径 | 是什么 |
| --- | --- |
| `<部署根>/target/release/seeai-api` | API 二进制 |
| `<部署根>/target/release/seeai-worker` | Worker 二进制 |
| `<部署根>/apps/web/dist/` | `console.html`、`portal.html` 与它们引用的 `assets/*` |
| `<部署根>/public-docs/` | 对客公开文档，也是素材 `narrative_path` 的解析根 |
| `<部署根>/config/bootstrap/` | 供给素材（`SUPPLY_MATERIAL_DIR` 的缺省值） |

## 3. 投产前的四条硬约束

这四条如果没满足，服务要么起不来、要么跑起来是错的。它们不是"建议"。两条路都要满足。

### 3.1 `apps/web/dist` 必须在编译期那个路径下就位

API 按**编译期路径**找前端产物：

```rust
Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("web").join("dist")
```

后果分两种：

- 构建时目录不存在 → 编译照过；运行时只会记一条 `no web build found; the API serves no front end` 警告，**两个界面都打不开**（API 与 `/v1/*` 仍然正常，所以很容易查错方向）；
- 构建后把二进制搬到别处 → 它仍然去找**编译时那个绝对路径**下的 `apps/api/../web/dist`，**跟着二进制走的是编译时路径**，不是运行目录。

**服务启动时，编译时那个绝对路径下必须有 `apps/web/dist`，并保留中间目录 `apps/api`。** 前端不参与 Rust 编译，可以在另一台机器构建。二进制可以移动，但移动它不会改变前端查找路径；只把整套文件搬到另一个绝对路径，界面仍会打不开。

systemd 方式的编译期路径就是宿主上的部署根，所以**编译与运行必须在同一个绝对路径**下（[生产环境（systemd）](production.md)）。容器方式在镜像里构建，路径固定成 `/app`，跟着镜像走，宿主目录放哪都不影响（[生产环境（容器）](production-docker.md)）。

### 3.2 前端分发靠主机名，反代要透传 `Host`

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

### 3.3 数据库用户要有 DDL 权限，且迁移会自动跑

**两个进程各自在启动时跑迁移**（`hub_repository.migrate()`，迁移幂等、由 PG advisory lock 串行化）。所以：

- 迁移由 `sqlx::migrate!` 在编译期嵌进二进制，运行不需要 `migrations/`；
- 连接用的数据库用户需要建表/改表权限，不能是只读账号；
- 升级时**先部署、后观察**即可，不需要单独的迁移步骤；但**多个实例同时冷启动**时它们会争那把锁，表现为其中一个稍慢启动，属正常；
- **例外：把并发名额改成必填的那次升级（迁移 `0049`）有一个无窗口的顺序**——迁移一跑就把没有名额的模型回填成 1，对所有副本当场生效。要保留更大并发的部署，先在**升级之前**用 `PATCH /api/v1/gateway-models/{model}`（模型页的"并发名额"也能做）把现值逐个钉到模型上，用 `GET /api/v1/gateway-models` 核对，再升级；升级后不需要再设回去。
- 回滚要谨慎：迁移是**只进不退**的（没有 down 脚本）。真要回退，得从备份恢复。

### 3.4 `ADMIN_TOKEN` 是必填的共享凭据，它不指向具体的人

API 起不来的第一原因就是它为空。它的作用：`Authorization: Bearer <ADMIN_TOKEN>` 直接当管理员用，**不需要登录**。

| | 登录会话（`ADMIN_EMAIL` / `ADMIN_PASSWORD`） | 共享令牌（`ADMIN_TOKEN`） |
| --- | --- | --- |
| 谁用 | 人在运营后台上登录 | 机器（运维脚本、CI、受控验证） |
| 审计里记的是 | 那个管理员的 id | `None`——**它不指向具体的人** |
| 生产取值 | 强口令 | **强随机串**，按密钥管理，能轮换 |

泄漏共享令牌等于交出全部管理接口，而审计里只看得到"某个用共享令牌的人"。所以它要跟其他密钥一样对待，别写进脚本、别进命令行历史。

生成用 `openssl rand -hex 32`（256 位、64 个十六进制字符）之类，别手敲。

**轮换**：API 只在启动时读一次并常驻内存（与请求里的令牌做 `constant_time_eq` 比较），所以改环境变量后**重启 API** 即可——不动数据库，也不会让已登录的管理员或客户会话失效。多实例滚动重启期间新旧令牌会短时并存；要严格一次切换就挑停服窗口。轮换后审计里仍然只记 `None`，"这次是谁做的"依然答不出来——这正是它只该给机器用的原因。

## 4. 配置

全部配置项（每个变量的缺省、含义、生产取值、失败方式）见[配置项](configuration.md)。**值怎么送进进程**两条路不同：systemd 用 `EnvironmentFile`（[生产环境（systemd）](production.md)），容器用 `env_file`（[生产环境（容器）](production-docker.md)）。

两份 env 的内容一样，**必填**项与取值见[配置项](configuration.md)（§1、§8、§9）；其余变量代码都留了缺省，只有要覆盖缺省才写。下面示例把必填项都写上：

```sh
# api.env
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
```

```sh
# worker.env
DATABASE_URL=postgres://seeai:<强口令>@127.0.0.1:5432/seeai_next
WORKER_ID=worker-1
AIHUBMIX_API_KEY=<渠道密钥>
APIMART_API_KEY=<渠道密钥>
RUST_LOG=info
```

两处按路线取值：

- `API_BIND`：systemd 保持回环 `127.0.0.1:8081`；容器必须 `0.0.0.0:8081`（端口映射要求），`deploy/compose.prod.yaml` 已覆盖成它；
- `SUPPLY_MATERIAL_DIR`（相对进程的工作目录）让 API 每次启动都把供给素材幂等导成渠道与 Offering；**不设时默认就是 `config/bootstrap`**，上面显式写出来只是为了部署文件可读——要换成自己的素材，把它指到挂载卷（[配置项 §6](configuration.md#6-供给素材导入)）。它只被 API 读，worker 不需要。

`worker.env` 比 `api.env` 多两样：每个实例唯一的 `WORKER_ID`，以及渠道密钥（[渠道凭证](configuration.md#5-渠道凭证)）。

上线还会踩的两条：

- **超时链是启动时一起校验的**：API 与 Worker 都读已发布合同声明的最大输出张数，算一遍 `PROVIDER_TIMEOUT_*` 与 `GENERATION_SYNC_WAIT_SECONDS` 整条链，不一致就拒绝启动并点名——它不是"调大就更快"（见[生成护栏](configuration.md#2-生成护栏)）；
- **渠道密钥两条进程都要有**：API 直接执行时读它，对账 Worker 重查上游状态时读同一份；凭证只从环境变量读，数据库只存变量名（见[渠道凭证](configuration.md#5-渠道凭证)）。

会话与口令重置的有效期也是配置项（`SESSION_TTL_SECONDS`、`PASSWORD_RESET_TTL_SECONDS`），见[会话与口令](configuration.md#8-会话与口令)。

改过 env 文件后要让新值生效，得重启进程——两个进程都只在启动时读一次环境变量。

## 5. 反向代理与 TLS

反代只做两件事：**终止 TLS** 与**保留 `Host`**。两份前端产物由 API 自己按主机名分发，反代里**不要**配 `root` 或 `try_files`（§3.2）。两条路的 `proxy_pass` 都指向宿主回环上发布的端口 `127.0.0.1:8081`。

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
    # docs/contracts/0007-image-upload-and-object-storage.md §2.2。
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
- 上限不足时上传请求会被反向代理先回 `413`；上传路由的上限合同与失败后果见[图片上传与对象存储 Spec](../contracts/0007-image-upload-and-object-storage.md) §2.2；
- API 绑在回环（`API_BIND=127.0.0.1:8081`），只让 nginx 够得到；证书按 §2.4 签发，路径要与上面的 `ssl_certificate` 一致；
- 公开鉴权端点的来源维默认用**连接对端地址**；部署在反代之后时对端是 nginx 本身，要在 API 进程配
  `AUTH_SOURCE_HEADER=x-real-ip`（nginx 用 `$remote_addr` 覆盖写它）才能区分真实客户端。**采信这个头
  的前提是 API 不能被绕过代理直连**（示例已把 API 绑在回环）；能直连的调用方可以自带同名头伪造来源。

## 6. 探活与优雅停机

| 探活 | 说明 |
| --- | --- |
| `GET /health` | 探数据库（`SELECT 1` 带超时）。可达回 `200 {"status":"ok"}`；不可达回 **503** `{"status":"unhealthy","database":"unreachable"}`。**不探上游渠道** |
| Worker | 没有 HTTP 端口。它的存活看进程与日志：`worker started`，以及轮询期间的错误 |
| 优雅停机 | 两个进程都处理 `SIGINT`（Ctrl+C）与 `SIGTERM`（systemd、容器默认），等飞完手头的工作再退（API 等在处理的请求，Worker 等当前那一轮） |

`/health` 只回答"我这个进程能不能读库"，所以**别用它判上游是否可用**——上游健康是渠道侧的事实，见 `docs/facts/channel-facts.md`。

## 7. 备份

在装了 `pg_dump` 的机器上导（systemd 方式的服务器随 PostgreSQL 一起有；容器镜像里没有）：

```sh
pg_dump --format=custom --file <文件> "$DATABASE_URL"
```

- 用与服务器 PostgreSQL 同大版本的 `pg_dump`；
- `DATABASE_URL` 带口令，`pg_dump` 会把整串放进命令行（`ps` 可见）；要避免就用 `.pgpass`，或用 `PGPASSWORD` 配合 `-h`/`-U`/`-d` 分开给；
- 导出失败时**明确报错**，不静默产出空文件。

仓库里另有 `scripts/backup/pg-backup.ps1`（PowerShell，按保留天数清旧转储）：它随检出走；要用得先有 `pwsh`。

**要恢复时注意**：迁移只进不退（§3.3），所以恢复的目标版本必须与备份时的 schema 一致，或者先把代码退到那个版本再恢复。

## 8. 投产前的演练

按顺序做完，每一条都有明确判据：

| # | 做什么 | 判据 |
| --- | --- | --- |
| 1 | 按所选方式部署并起 API（[systemd](production.md) / [容器](production-docker.md)） | `GET /health` 回 `200 {"status":"ok"}` |
| 2 | 浏览器打开 `https://admin.<域名>/` | 出现**运营后台**登录页（不是客户控制台——那说明 `Host` 没透传，§3.2） |
| 3 | 同一浏览器打开 `https://app.<域名>/` | 出现**客户控制台** |
| 4 | 起 worker，看日志 | `worker started`，且没有反复刷新的错误 |
| 5 | 在运营后台发一个平台模型（选厂商 → 勾供给 → 给价） | 模型目录里出现它；`GET /v1/models` 也列出它 |
| 6 | 用**假上游**或在受控额度下跑一次真实生成 | Job 完成、账本有 `hold`/`release`/`capture` 三条、余额变化与用量对得上 |
| 7 | 把一个平台模型停用 | 它从 `GET /v1/models` 消失；用它受理得到"模型不存在" |
| 8 | 从备份恢复到另一个库，指向它起一次 API | 能起来、能登录、目录与账务与备份时一致 |

**第 6 条会真的调用上游并计费**，按 `AGENTS.md` 的约定：真实 Provider 调用必须显式批准并限制次数。想只验管道就指向本地假上游——那验的是"链路通不通"，不是"上游通不通"。

## 9. 与开发环境的差异

| | 开发（见[开发环境](development.md)） | 生产 |
| --- | --- | --- |
| 端口 | Postgres `5432`、Redis `6379`、API `8081` | 自定；Redis 可选 |
| 凭证 | 可以留空（不调上游就没用） | 必须是真密钥，按密钥管理 |
| `ADMIN_TOKEN` | 任意非空 | 强随机、可轮换（§3.4） |
| `SEE_BASEURL` | 本机 `http://app.localhost:8081` | 对客域名 `https://app.<域名>`，只写源 |
| 前端产物 | 本机构建后即可 | **服务启动时**要在编译期那个路径下（§3.1）；本地打好再传也行 |
| `CONSOLE_DEV_HOST` | 可用 | **不设**（[进程与连接](configuration.md#1-进程与连接)） |
| TLS | 不需要 | 反代终止，必须 HTTPS（§3.2） |
| 迁移 | 进程启动自动跑 | 同上；多实例冷启动会争锁（§3.3） |
| 依赖 | 本机系统包安装的 PG17 与 Redis（§2.2、§2.3） | 同样要 PG17，Redis 可选；按 §2.2 准备 |
| 进程管理 | 两个终端 `cargo run` | 两个 systemd 单元或两个容器（§1） |
| 配置注入 | 仓库根 `.env`（进程自己读） | `EnvironmentFile` 或 `env_file`（§4） |
| 日志 | 终端 stdout | journald 或 `docker logs`（§1） |
