# 生产环境（systemd）

传统部署：两个进程各跑一个 systemd 单元，二进制与前端产物在宿主上构建。

**先按[部署总览与基础准备](deployment.md) 把服务器准备好**——系统与网络、PostgreSQL、Redis、nginx 与证书、代码怎么到服务器上、四条硬约束、配置内容、反向代理、探活、备份与演练判据都在那里。本文只写 systemd 这一路特有的部分。

容器方式见[生产环境（容器）](production-docker.md)。

## 1. 构建

在服务器上构建。二进制把编译期仓库路径写死（[部署总览 §3.1](deployment.md#31-appswebdist-必须在编译期那个路径下就位)），所以**编译与运行必须在同一个绝对路径**下；仓库放哪由你定。本文与 [`deploy/systemd/`](../../deploy/systemd/) 的单元都按示例根 `/opt/seeai` 写，换根时把 `/opt/seeai` 整体换成你的目录，并改单元里的 `WorkingDirectory` 与 `ExecStart`。

前置：Rust 1.94+、C 工具链，以及 `cmake`、`perl`、`pkg-config`（aws-lc-rs 要从源码构建）；前端在服务器上打才需要 Node 24：

```sh
sudo apt-get install -y build-essential cmake perl pkg-config
```

前端产物不参与编译，只在运行时按编译期路径找（[部署总览 §3.1](deployment.md#31-appswebdist-必须在编译期那个路径下就位)）；先出产物、再编二进制是最省事的顺序：

```sh
cd /opt/seeai
sudo npm --prefix apps/web ci
sudo npm --prefix apps/web run build
sudo cargo build --release -p seeai-api -p seeai-worker
```

不想在服务器上装 Node 时，前端在本地打好即可：`npm --prefix apps/web ci && npm --prefix apps/web run build`，把生成的 `apps/web/dist/` 整个传到 `/opt/seeai/apps/web/dist/`。产物是静态文件，引用 `/assets/...` 这类站内绝对路径，没有构建期配置，换机器与换目录都不影响；服务器上那两步 `npm ci`、`npm run build` 随之省掉，但 `dist/` 仍要在服务启动前就位（[部署总览 §3.1](deployment.md#31-appswebdist-必须在编译期那个路径下就位)）。用[部署总览 §2.5](deployment.md#25-代码到服务器上) 的打包脚本时，`dist/` 已经在包里。

编译期路径因此固定成 `/opt/seeai`，编译后不能再搬这棵树（[部署总览 §3.1](deployment.md#31-appswebdist-必须在编译期那个路径下就位)）。

用有工具链的账号构建；跑进程的账号见 §2。

## 2. systemd 单元

两个进程都用系统用户 `seeai` 跑，工作目录 `/opt/seeai`，配置从 `EnvironmentFile` 读。文件是每行 `KEY=VALUE`（systemd 自己解析，不做 shell 展开）。服务账号只用来**跑**进程，不参与构建，所以它不需要 Node/Rust，也不需要 home；单元文件在仓库 [`deploy/systemd/`](../../deploy/systemd/)：

```sh
sudo useradd --system --user-group --shell /usr/sbin/nologin seeai
sudo install -m 644 /opt/seeai/deploy/systemd/seeai-api.service /etc/systemd/system/seeai-api.service
sudo install -m 644 /opt/seeai/deploy/systemd/seeai-worker.service /etc/systemd/system/seeai-worker.service
```

单元的停机宽限（`TimeoutStopSec`）与理由写在单元文件里。

跑进程的 `seeai` 账号只需要**读**这棵树：保持目录 755、文件 644 就够（[部署总览 §2.5](deployment.md#25-代码到服务器上)）；不想放开其他账号的读权限就把它交给 `seeai`：

```sh
sudo chown -R seeai:seeai /opt/seeai
```

先建配置目录与两份 env（属服务账号、`600`——里面有口令），**内容见[部署总览 §4](deployment.md#4-配置)**：

```sh
sudo install -d -m 750 -o seeai -g seeai /etc/seeai
sudo install -m 600 -o seeai -g seeai /dev/null /etc/seeai/api.env
sudo install -m 600 -o seeai -g seeai /dev/null /etc/seeai/worker.env
# 两份文件按部署总览 §4 写

sudo systemctl daemon-reload
sudo systemctl enable --now seeai-api seeai-worker
```

改过 env 文件后要让新值生效，得 `sudo systemctl restart seeai-api seeai-worker`——进程只在启动时读一次环境变量。

日志走 stdout、由 journald 收（JSON）：`journalctl -u seeai-api -f`。

### 2.1 前台直接跑（排障用）

进程启动时的 `dotenvy::dotenv()` 只是兜底：它从**当前工作目录**往上找 `.env` 并载入，但**不覆盖**已有环境变量——生产不要放 `.env` 进部署目录。绕过 systemd 直接跑：

```sh
DATABASE_URL=... ADMIN_TOKEN=... SEE_BASEURL=... CUSTOMER_HISTORY_CURSOR_KEY=... REQUEST_FINGERPRINT_KEY_V1=... ADMIN_EMAIL=... ADMIN_PASSWORD=... API_BIND=127.0.0.1:8081 seeai-api
DATABASE_URL=... WORKER_ID=worker-1 seeai-worker
```

## 3. 升级与重启

- 迁移在两个进程启动时自动跑（[部署总览 §3.3](deployment.md#33-数据库用户要有-ddl-权限且迁移会自动跑)），升级就是更新 `/opt/seeai` 里的代码（`git pull` 或按[部署总览 §2.5](deployment.md#25-代码到服务器上) 重新上传）、重新构建（§1）再重启；
- `systemctl restart seeai-api` 发 `SIGINT`、走排空（[部署总览 §6](deployment.md#6-探活与优雅停机)），不需要单独的停机脚本；多实例逐台重启，避免全部同时冷启动去争迁移锁（争锁只表现为启动稍慢）；
- 应用回退用上一版代码重新构建（`git checkout <上一个 tag>` 或传回上一版）；数据库迁移只进不退，回退数据库只能从备份恢复（[部署总览 §3.3](deployment.md#33-数据库用户要有-ddl-权限且迁移会自动跑)）。升级前先做一次备份（[部署总览 §7](deployment.md#7-备份)）；
- 单元文件有改动时重新装到 `/etc/systemd/system/` 再 `systemctl daemon-reload`（§2）——更新代码只覆盖 `/opt/seeai`，不动 `/etc`。
