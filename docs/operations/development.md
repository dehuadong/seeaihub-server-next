# 开发环境

一台机器上把两个进程与依赖跑起来：`seeai-api`（控制面 + 图片生成入口 + 托管两份前端产物）与 `seeai-worker`（领取 Job、调上游、结算）。开发机是 WSL Ubuntu，PostgreSQL 与 Redis 由系统包安装并常驻，不用 Docker。

## 1. 前置

| 需要什么 | 版本 | 说明 |
| --- | --- | --- |
| Rust | `Cargo.toml` 声明 `rust-version = 1.94`（edition 2024） | 两个进程都是 Rust 二进制 |
| Node.js + npm | 24（CI 用的版本） | 只用来构建前端产物 |
| PostgreSQL | 17 | 业务事实权威，监听 `127.0.0.1:5432` |
| Redis | 7（可选） | 只是加速层；`REDIS_URL` 留空即不启用，受理与结算全部回源数据库 |

`psql` / `pg_dump` / `redis-cli` 随包安装，建库、备份与探活用得到。

## 2. 起依赖

PostgreSQL 17 与 Redis 7 已装好并监听 `127.0.0.1`：

| 服务 | 宿主端口 | 凭据 / 说明 |
| --- | --- | --- |
| `postgresql`（集群 `17/main`） | **5432** | 超级用户 `postgres`，开发库 `seeai_next` |
| `redis-server` | **6379** | 无密码；不启用加速层也能跑 |

两个服务随 WSL 的系统服务启动，手动起停用 `sudo systemctl start postgresql redis-server`（停用就把 `start` 换成 `stop`）。

首次给本项目建角色与库。角色带 `CREATEDB`：契约测试与 e2e 都要自己派生一次性库：

```sh
sudo -u postgres psql -p 5432 -c "CREATE ROLE seeai LOGIN CREATEDB PASSWORD 'seeai';"
sudo -u postgres psql -p 5432 -c "CREATE DATABASE seeai_next OWNER seeai;"
sudo -u postgres psql -p 5432 -c "CREATE DATABASE seeai_contract OWNER seeai;"
```

`seeai_next` 是开发库；`seeai_contract` 只当**可连接的空库**给契约用例派生用（见 §7），不放开发数据。

## 3. 配置

```sh
cp .env.example .env
```

两个进程启动时都会调 `dotenvy::dotenv()`：它从**当前工作目录**起往上找 `.env`，找到就载入，但**不覆盖**已经存在的环境变量。所以在仓库根 `cargo run` 会自动读到这份 `.env`，不用手动 `export`；想临时换一个值，直接在命令前设环境变量即可（它优先于 `.env`）。`.env` 已在 `.gitignore` 里，不进版本库。

必填与常用项（全表见 [`.env.example`](../../.env.example)，逐项说明见[配置项](configuration.md)）：

| 变量 | 开发取值 | 说明 |
| --- | --- | --- |
| `DATABASE_URL` | `postgres://seeai:seeai@127.0.0.1:5432/seeai_next` | 两个进程都读 |
| `API_BIND` | `127.0.0.1:8081` | 只 `seeai-api` 读 |
| `SEE_BASEURL` | `http://app.localhost:8081` | **必填**，平台对客基址；模型说明与公共文档的链接按它写成绝对地址，只写源 |
| `ADMIN_TOKEN` | 任意非空 | **必填**，空值会让进程起不来 |
| `CUSTOMER_HISTORY_CURSOR_KEY` | `openssl rand -base64 32` | **必填**，客户历史翻页游标的加密密钥；缺了或不是 32 字节的 base64 时 API 起不来 |
| `REQUEST_FINGERPRINT_KEY_V1` | `openssl rand -base64 32` | **必填**，请求指纹的 HMAC 密钥；缺了或不是 32 字节的 base64 时 API 起不来 |
| `ADMIN_EMAIL` / `ADMIN_PASSWORD` | 自定 | 用来建/更新那个管理员账号，运营后台的登录页用它 |
| `REDIS_URL` | `redis://127.0.0.1:6379` | 留空则不启用加速层，功能不变 |
| `SUPPLY_MATERIAL_DIR` | `config/bootstrap` | 设了才会导入供给素材；不设则可选供给清单是空的 |
| `AIHUBMIX_API_KEY` / `APIMART_API_KEY` | 见下 | 变量名由素材的 `credential_env` 指定 |

**渠道凭证只在环境变量里**：数据库只存变量名，不存值。开发时如果不打算真的调上游，可以不设——只有真正发起生成请求才会用到，缺了会在执行期失败。

## 4. 构建前端产物

```sh
npm --prefix apps/web ci
npm --prefix apps/web run build
```

产出 `apps/web/dist/`（两个入口 `console.html` 与 `portal.html`）。

**这一步不能省**：API 在启动时按**编译期路径**找 `apps/web/dist`（`CARGO_MANIFEST_DIR/../web/dist`），找不到就只记一条警告并**不托管任何前端**——你会看到 API 与 `/v1/*` 都正常，但浏览器打开主页拿不到界面。产物是构建前端时生成的，与 `cargo build` 无关：先构建产物，再启动 API。

## 5. 跑起来

开两个终端：

```sh
cargo run -p seeai-api
cargo run -p seeai-worker
```

两个进程**各自在启动时跑迁移**（都调 `migrate`，迁移是幂等的）。API 还会顺带做这三件事，任一失败它就**退出**而不是带着断链跑：

1. 幂等导入 `SUPPLY_MATERIAL_DIR` 里的素材（没设/没目录/没素材则静默跳过）；
2. 校验整条超时链（它要读已发布合同里声明的输出张数上限）；
3. 建或更新 `ADMIN_EMAIL` / `ADMIN_PASSWORD` 那个管理员账号。

日志是 **JSON**，级别由 `RUST_LOG` 给，缺省 `info`。

## 6. 打开界面

| 入口 | 地址 | 判据 |
| --- | --- | --- |
| 运营后台 | `http://admin.localhost:8081/` | 主机名第一段是 `admin` |
| 客户控制台 | `http://app.localhost:8081/` | 其余主机都回这一份 |

> 本机的 `*.localhost` 由浏览器解析到回环，**不用改 hosts**。但 **Node 的解析器不认 `.localhost`**：脚本或测试里用 `fetch` / `request` 时要直连 `127.0.0.1:8081`。

没有域名可指时（例如容器里），可以用 `CONSOLE_DEV_HOST=<某主机名>` 让那个主机也回运营后台那一份；两个入口另有**文件名直达**（`/console.html`、`/portal.html`），在任何主机上都有效。

## 7. 验证

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

契约用例默认被 `#[ignore]`，需要一个**可连接的空库**（它会自己派生独立库，**不要指向开发库**）：

```sh
HTTP_CONTRACT_DATABASE_URL=postgres://seeai:seeai@127.0.0.1:5432/seeai_contract \
  cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1
```

浏览器行为在 `apps/web` 下跑，命令自己拉起空库、产物与 API：

```sh
npm --prefix apps/web run e2e
```

**它们都不调真实上游**：假上游在进程内监听回环。真实计费调用只在显式授权的受控实测里发生，见 [`docs/verification/`](../verification/)。

## 8. 开发时容易踩的几件事

| 现象 | 原因 |
| --- | --- |
| 浏览器打开主页没有界面、API 却正常 | `apps/web/dist` 不在——见 §4 |
| 可选供给清单是空的 | 没设 `SUPPLY_MATERIAL_DIR`，或目录里没有 `.json` 素材 |
| 发布平台模型时说"没有生效的折算率" | 先录该渠道币种的折算率（后台「折算率」页，或 `PUT /api/v1/fx-rates`） |
| 请求受理了却一直不完成 | `seeai-worker` 没在跑（它才是领取 Job 的那个进程） |
| 脚本里 `*.localhost` 连不上 | Node 不解析 `.localhost`，改直连 `127.0.0.1` |

## 9. 这一节之外

- 生产怎么部署、面向哪些风险、投产前做什么演练：见[生产环境](production.md)。
- 各配置项在生产下的取值与理由：见[配置项](configuration.md)。
- 备份与恢复：见[生产环境](production.md) §6。
