# 生产环境（systemd）

传统部署：两个进程各跑一个 systemd 单元，二进制与前端产物可以在服务器构建，也可以由开发构建机生成发布包后安装。

**先按[部署总览与基础准备](deployment.md) 把服务器准备好**——系统与网络、PostgreSQL、Redis、nginx 与证书、代码怎么到服务器上、四条硬约束、配置内容、反向代理、探活、备份与演练判据都在那里。本文只写 systemd 这一路特有的部分。

容器方式见[生产环境（容器）](production-docker.md)。

## 1. 构建

可以在生产服务器构建，也可以在开发构建机生成发布包后上传。本文与 [`deploy/systemd/`](../../deploy/systemd/) 的单元都按示例根 `/opt/seeai` 写；换根时同时调整构建目录、前端部署目录、单元的 `WorkingDirectory` 与 `ExecStart`（§1.1）。

### 1.1 构建机与服务器的要求

构建机使用 Linux 或 WSL 2 的 Linux 工具链；Windows 原生编译的 `.exe` 不适用于本文的 Linux systemd 部署。构建时需要 Rust 1.94+、Node 24/npm、C 工具链、`cmake`、`perl` 与 `pkg-config`。构建不需要生产数据库、渠道凭证，也不会启动服务。

直接传二进制时还要满足：

- 编译目标与服务器的操作系统、CPU 架构一致：x86_64 Linux 通常用 `x86_64-unknown-linux-gnu`，ARM64 Linux 用 `aarch64-unknown-linux-gnu`，目标定义见 [Rust 平台说明](https://doc.rust-lang.org/rustc/platform-support.html)。
- 服务器具备二进制所需的动态库与符号版本。使用与服务器一致的发行版及版本构建最便于控制依赖；WSL 较新、服务器较旧时，不能根据本地编译通过判断兼容。上线前在服务器用 `ldd` 检查依赖，并按[部署总览 §8](deployment.md#8-投产前的演练)实际启动验证。
- 构建目录按生产目录布置。下例在构建机 `/opt/seeai` 编译，服务器也在 `/opt/seeai` 放前端；在 `/home/<用户>/...` 编译后只传到 `/opt/seeai` 不满足 [部署总览 §3.1](deployment.md#31-appswebdist-必须在编译期那个路径下就位)。

x86_64 构建机生成 ARM64 二进制还需要目标 Rust 标准库、ARM64 C 工具链、链接器与目标系统库；本项目的 `aws-lc-rs` 包含原生编译，单独运行 `rustup target add` 不够。本文下面给出同架构构建步骤；跨架构可使用 ARM64 Linux 构建机，或采用[容器部署的目标架构构建](production-docker.md#22-在本机构建)。容器镜像不能直接当成 systemd 发布包使用。

### 1.2 在生产服务器构建

前置：Rust 1.94+、C 工具链，以及 `cmake`、`perl`、`pkg-config`（aws-lc-rs 要从源码构建）；前端在服务器上打才需要 Node 24：

```sh
sudo apt-get install -y build-essential cmake perl pkg-config
```

前端产物不参与编译，只在运行时按编译期路径找（[部署总览 §3.1](deployment.md#31-appswebdist-必须在编译期那个路径下就位)）；先出产物、再编二进制是最省事的顺序：

```sh
cd /opt/seeai
sudo npm --prefix apps/web ci
sudo npm --prefix apps/web run build
sudo cargo build --locked --release -p seeai-api -p seeai-worker
```

不想在服务器上装 Node 时，前端在本地打好即可：`npm --prefix apps/web ci && npm --prefix apps/web run build`，把生成的 `apps/web/dist/` 整个传到 `/opt/seeai/apps/web/dist/`。产物是静态文件，引用 `/assets/...` 这类站内绝对路径，没有构建期配置，换机器与换目录都不影响；服务器上那两步 `npm ci`、`npm run build` 随之省掉，但 `dist/` 仍要在服务启动前就位（[部署总览 §3.1](deployment.md#31-appswebdist-必须在编译期那个路径下就位)）。用[部署总览 §2.5](deployment.md#25-代码到服务器上) 的打包脚本时，`dist/` 已经在包里。

用有工具链的账号构建；跑进程的账号见 §2。

### 1.3 在开发构建机生成发布包

以下命令在 Linux/WSL 的 Bash 中执行。构建机与服务器架构一致，并满足 §1.1 的系统库要求。`<仓库地址>`、`<发布提交或标签>` 与 `<服务器>` 按实际值替换；已有 `/opt/seeai` 检出时使用该目录，不重复克隆。

```sh
sudo install -d -m 755 -o "$(id -un)" -g "$(id -gn)" /opt/seeai
git clone <仓库地址> /opt/seeai
cd /opt/seeai
git checkout <发布提交或标签>
git rev-parse HEAD
rustc -vV
npm --prefix apps/web ci
npm --prefix apps/web run build
cargo build --locked --release -p seeai-api -p seeai-worker
tar -czf /tmp/seeai-release.tar.gz \
  target/release/seeai-api target/release/seeai-worker \
  apps/web/dist public-docs config/bootstrap deploy/systemd
sha256sum /tmp/seeai-release.tar.gz
scp /tmp/seeai-release.tar.gz <服务器>:/tmp/
```

记录发布提交、`rustc -vV` 的编译目标及发布包的 SHA-256。发布包只含运行文件与 systemd 单元，不携带 `.env`、生产凭证、源码、`node_modules` 或整个 `target/`。迁移已嵌入二进制，无需传 `migrations/`。这与[部署总览 §2.5](deployment.md#25-代码到服务器上)的源码包不同：`pack-deploy-tree.mjs` 不包含 Rust 编译产物，不能代替本节的发布包。

### 1.4 在服务器安装发布包

以下命令在生产 Linux 服务器执行。核对 SHA-256 与构建机记录一致，再解包；服务器无需安装 Rust、Cargo、Node 或 C 编译工具，需要运行依赖与 CA 证书。Debian/Ubuntu 可用 `sudo apt-get install -y ca-certificates` 安装证书。

升级现有服务时，先按 [部署总览 §7](deployment.md#7-备份) 备份数据库，再执行 `sudo systemctl stop seeai-api seeai-worker`，确认服务停止后执行下面的解包命令，避免覆盖正在运行的二进制。首次安装无需停止服务。

```sh
sha256sum /tmp/seeai-release.tar.gz
sudo install -d -m 755 /opt/seeai
sudo tar -xzf /tmp/seeai-release.tar.gz -C /opt/seeai
sudo install -d -m 755 /opt/seeai/apps/api
sudo chmod 755 /opt/seeai/target/release/seeai-api /opt/seeai/target/release/seeai-worker
ldd /opt/seeai/target/release/seeai-api
ldd /opt/seeai/target/release/seeai-worker
```

`ldd` 出现 `not found` 或符号版本错误时不要启动，先补齐运行库或在兼容的构建环境重新生成。检查通过不替代 [部署总览 §8](deployment.md#8-投产前的演练) 的启动与功能验证。首次安装继续 §2 配置 systemd、[部署总览 §5](deployment.md#5-反向代理与-tls) 配置反代。

升级安装完成后按 §3 启动并验证。发布包解压不会删除旧素材或旧静态文件，发布中有文件删除时，应按对应发布内容清理旧文件。

## 2. systemd 单元

两个进程都用系统用户 `seeai` 跑，工作目录 `/opt/seeai`，配置从 `EnvironmentFile` 读。文件是每行 `KEY=VALUE`（systemd 自己解析，不做 shell 展开）。服务账号只用来**跑**进程，不参与构建，所以它不需要 Node/Rust，也不需要 home；单元文件在仓库 [`deploy/systemd/`](../../deploy/systemd/)：

```sh
sudo useradd --system --user-group --shell /usr/sbin/nologin seeai
sudo install -m 644 /opt/seeai/deploy/systemd/seeai-api.service /etc/systemd/system/seeai-api.service
sudo install -m 644 /opt/seeai/deploy/systemd/seeai-worker.service /etc/systemd/system/seeai-worker.service
```

单元的停机宽限（`TimeoutStopSec`）与理由写在单元文件里。

跑进程的 `seeai` 账号需要读取运行文件并执行两个二进制：目录 755、普通文件 644、二进制 755（[部署总览 §2.5](deployment.md#25-代码到服务器上)）；不想放开其他账号的读权限就把它交给 `seeai`：

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

- 迁移在两个进程启动时自动跑（[部署总览 §3.3](deployment.md#33-数据库用户要有-ddl-权限且迁移会自动跑)）。**例外**：把并发名额改成必填的迁移 `0049` 会把没有名额的模型回填成 1，**对所有副本当场生效**——要保留更大并发的部署，先在升级**之前**用模型页（或 `PATCH /api/v1/gateway-models/{model}`）把现值逐个钉到模型上，再升级；见 §3.3 的同一段。服务器构建时更新 `/opt/seeai` 里的代码并重新构建；开发构建机出包时按 §1.4 停止服务并安装新包，再执行 `sudo systemctl start seeai-api seeai-worker`，按[部署总览 §8](deployment.md#8-投产前的演练)验证；
- `systemctl restart seeai-api` 发 `SIGINT`、走排空（[部署总览 §6](deployment.md#6-探活与优雅停机)），不需要单独的停机脚本；多实例逐台重启，避免全部同时冷启动去争迁移锁（争锁只表现为启动稍慢）；
- 应用回退用上一版代码重新构建，或按 §1.4 重新安装保留的上一版发布包；启动前确认旧程序与当前数据库结构兼容。数据库迁移只进不退，回退数据库只能从备份恢复（[部署总览 §3.3](deployment.md#33-数据库用户要有-ddl-权限且迁移会自动跑)）。升级前先做一次备份（[部署总览 §7](deployment.md#7-备份)）；
- 单元文件有改动时重新装到 `/etc/systemd/system/` 再 `systemctl daemon-reload`（§2）——更新代码只覆盖 `/opt/seeai`，不动 `/etc`。
