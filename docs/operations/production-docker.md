# 生产环境（容器）

两个进程各跑一个容器，PostgreSQL 与 Redis 仍然外置；容器里没有需要持久化的本地状态。

**先按[部署总览与基础准备](deployment.md) 把服务器准备好**——系统与网络、PostgreSQL、Redis、nginx 与证书、代码怎么到服务器上、四条硬约束、配置内容、反向代理、探活、备份与演练判据都在那里。本文只写容器这一路特有的部分。

systemd 方式见[生产环境（systemd）](production.md)。

## 1. 与 systemd 方式的差异

| | systemd（[生产环境（systemd）](production.md)） | 容器（本文） |
| --- | --- | --- |
| 进程管理 | 两个 systemd 单元 | 两个容器（`restart: unless-stopped`） |
| 二进制与前端产物 | 在服务器构建或安装开发构建机的发布包（[生产环境（systemd）§1](production.md#1-构建)） | 在开发构建机或服务器多阶段构建进镜像 |
| 配置注入 | `EnvironmentFile` | `env_file`（密钥系统在启动前渲染进去） |
| 停机 | 默认 SIGTERM + `TimeoutStopSec` | 默认 SIGTERM + `stop_grace_period` |
| 日志 | journald | `docker logs` 或采集器 |

**宿主路径不进镜像**：systemd 方式把宿主上的编译期路径写进二进制（[部署总览 §3.1](deployment.md#31-appswebdist-必须在编译期那个路径下就位)），容器方式在镜像内部固定成 `/app`（§2.2）。所以仓库放哪台机器、哪个目录都可以，宿主只需提供两个 env 文件（§3）。

## 2. 准备镜像

`deploy/compose.prod.yaml` 只认一件事：**本机镜像列表里有一个 `seeai:<tag>`**（第 6 行 `image: seeai:${SEEAI_TAG:-local}`）。镜像从哪来由你定，下面三条路选一条。

| 方式 | 何时用 | 宝塔按钮（§5） | 做哪些节 |
| --- | --- | --- | --- |
| 在本机构建 | 服务器上有源码与 Docker | 构建镜像 | §2.2 |
| 从别处导入 | 构建在另一台机器上，服务器不能直连镜像仓库 | 导入镜像 | 构建机 §2.2 → 服务器 §2.3 |
| 从镜像仓库拉取 | 服务器能直连镜像仓库 | 从仓库中拉取 | §2.4 |

三条路的结果一样：`docker images` 里出现 `seeai:<tag>`。之后都进 §3。

### 2.1 装 Docker

生产服务器使用 Linux，安装 Docker Engine 与 Compose V2。镜像架构必须与服务器 CPU 一致。运行镜像内使用 Debian Bookworm，不要求宿主也使用 Debian；容器共享宿主内核，不能消除架构与内核能力要求，见 [Docker 多平台说明](https://docs.docker.com/build/building/multi-platform/)。

下面的安装命令适用于提供这些包、使用 systemd 的 Debian/Ubuntu 环境；其他发行版使用对应的 Docker 安装步骤。

```sh
sudo apt-get install -y docker.io docker-compose-v2
sudo systemctl enable --now docker
```

安装后用 `sudo docker version` 与 `sudo docker compose version` 确认服务可连接、Compose 可用。下面服务器侧的命令按 `sudo docker` 写；把账号加进 `docker` 组并重新登录后可省掉 `sudo`。

服务器只运行预构建镜像时，无需安装 Rust、Node 或编译工具。PostgreSQL、可选 Redis、环境变量与 HTTPS 反代仍需准备（§3、§4）。本文不规定最低 CPU、内存或磁盘容量；按[生成预算](configuration.md#2-生成护栏)、[上传预算](configuration.md#10-上传端点与上传存储)、其他进程占用和实际并发配置资源，生成内存预算不等于整机内存需求。

### 2.2 在本机构建

构建可以在 Windows Docker Desktop、Linux/WSL 的 Docker 环境或生产服务器执行。下面的 `docker` 命令均在仓库根执行；先检出要发布的提交或标签，记录 `git rev-parse HEAD`。

Windows 构建机使用 Docker Desktop 的 Linux 容器模式，可使用 WSL 2 后端；安装要求见 [Docker Desktop Windows 文档](https://docs.docker.com/desktop/setup/install/windows-install/)。从 PowerShell 构建 Windows 目录中的仓库即可，不需要在 Windows 另装 Rust、Node；从 WSL 执行时需要 Docker Desktop 启用该发行版的 WSL 集成，或使用该 Linux 环境自己的 Docker Engine。

先检查 Docker 服务与构建器：

```sh
docker version
docker info --format '{{.OSType}}'
docker buildx version
docker buildx inspect --bootstrap
```

`OSType` 必须为 `linux`。跨架构构建时，构建器的 `Platforms` 应包含目标架构，并具备模拟或对应架构的构建节点。Docker Desktop 默认提供模拟支持；独立 Docker Engine 的构建器需按 [Docker 多平台构建文档](https://docs.docker.com/build/building/multi-platform/) 准备。模拟编译通常比同架构构建慢。

`Dockerfile` 三个阶段：`web` 用 Node 构建两份前端产物，`build` 用 Rust 构建两个二进制，`runtime` 用 Debian slim 只带运行所需的东西，以非 root 用户跑。

API 按编译期绝对路径找前端产物（[部署总览 §3.1](deployment.md#31-appswebdist-必须在编译期那个路径下就位)）：构建与运行都在 `/app`，产物放 `/app/apps/web/dist`，并保留 `/app/apps/api` 这个空目录——路径里的 `apps/api/../web/dist` 要先能进 `apps/api`，中间目录不存在时 `..` 解析不到。改镜像布局时这两条要一起看。

迁移已编进二进制，运行镜像里不需要 `migrations/`（[部署总览 §3.3](deployment.md#33-数据库用户要有-ddl-权限且迁移会自动跑)）。镜像把素材放在 `/app/config/bootstrap`，而 `SUPPLY_MATERIAL_DIR` 不设时默认就是 `config/bootstrap`（相对 `WORKDIR /app`），所以镜像起来就导入（[配置项 §6](configuration.md#6-供给素材导入)）；用自己的素材则挂一个只读卷并把变量指过去。

在仓库根构建（末尾的 `.` 是**构建上下文**，必须是仓库根——`COPY . .` 与 `.dockerignore` 都相对它解析）：

```sh
docker build -t seeai:<tag> .
```

`-f` 只指定 Dockerfile，与上下文是两件事；Dockerfile 在别处时写成：

```sh
docker build -f <仓库根>/Dockerfile -t seeai:<tag> <仓库根>
```

服务器上没有仓库时，用[部署总览 §2.5](deployment.md#25-代码到服务器上) 的 `scripts/deploy/pack-deploy-tree.mjs` 打包整棵树传上去：包里已有 `Dockerfile`、`.dockerignore`、`deploy/compose.prod.yaml` 与前端源码，解包后在部署根直接 `docker build`。

**在服务器上构建镜像要用完整包，不加 `--no-web-src`。** 镜像的 `web` 阶段在容器里跑 `npm ci && npm run build`，要的是 `apps/web/` 那组构建输入（清单见[部署总览 §2.5](deployment.md#25-代码到服务器上)）；少了它们的包 `docker build` 会在 `COPY` 那步失败。只有走 §2.3 那条路时服务器才不需要这些输入。

按生产服务器架构选择一条命令。下面示例生成单架构镜像，并用 `--load` 载入本地 Docker，供 §2.3 导出；命令可直接在 PowerShell 或 Bash 执行。

```sh
# x86_64 服务器
docker buildx build --platform linux/amd64 -t seeai:release-amd64 --load .

# ARM64 服务器
docker buildx build --platform linux/arm64 -t seeai:release-arm64 --load .
```

当前 Dockerfile 在目标平台执行 Node 与 Rust 构建；x86_64 上构建 ARM64 时通常模拟 ARM64 环境，未配置在 x86_64 上直接调用 Rust 交叉编译器。构建机的仓库路径不会成为 API 的前端查找路径，镜像中的构建与运行目录均为 `/app`。

构建需要下载基础镜像、系统包、npm 与 Cargo 依赖，不需要生产数据库或渠道凭证。完成后检查镜像平台，例如 ARM64：

```sh
docker image inspect seeai:release-arm64 --format '{{.Os}}/{{.Architecture}}'
```

结果应为 `linux/arm64`；x86_64 镜像应为 `linux/amd64`。平台检查与构建成功不替代[部署总览 §8](deployment.md#8-投产前的演练)的目标服务器验证。发布时使用能识别版本的 tag（§2.5），下面以 `release-arm64` 为例。

### 2.3 从别处导入

构建机与服务器不是同一台时走这节；同一台则构建完直接进 §3。这节服务器不构建镜像，所以[部署总览 §2.5](deployment.md#25-代码到服务器上) 里加了 `--no-web-src` 的包也可以用——它带 `deploy/compose.prod.yaml`，够起容器。

先将镜像导出成文件。以下命令在 Windows PowerShell、Linux 或 WSL 均可执行；使用 `docker save -o`，避免通过 PowerShell 的文本管道传输二进制归档。将 `<服务器>` 替换为 SSH 目标：

```sh
docker save -o seeai-release-arm64.tar seeai:release-arm64
scp seeai-release-arm64.tar <服务器>:/tmp/
scp deploy/compose.prod.yaml <服务器>:/tmp/seeai-compose.prod.yaml
```

记录并核对归档的 SHA-256。Windows 构建机执行 `Get-FileHash ./seeai-release-arm64.tar -Algorithm SHA256`；Linux/WSL 构建机执行 `sha256sum seeai-release-arm64.tar`。不要把开发 `.env` 随镜像归档或 Compose 文件上传，生产配置按 §3 单独准备。

以下命令在生产 Linux 服务器执行。先核对归档哈希与构建机记录一致，再导入镜像，并把 Compose 文件放到固定位置：

```sh
uname -m
sha256sum /tmp/seeai-release-arm64.tar
sudo docker load -i /tmp/seeai-release-arm64.tar
sudo docker image inspect seeai:release-arm64 --format '{{.Os}}/{{.Architecture}}'
sudo install -d -m 755 /opt/seeai/deploy
sudo install -m 644 /tmp/seeai-compose.prod.yaml /opt/seeai/deploy/compose.prod.yaml
cd /opt/seeai
```

`uname -m` 的 `aarch64` 对应 `linux/arm64`，`x86_64` 对应 `linux/amd64`。生产应运行匹配架构的镜像。服务器无需检出完整仓库；镜像、Compose 文件与 §3 的配置文件足够启动两个应用进程。


### 2.4 从镜像仓库拉取

服务器能直连镜像仓库时走这节，省掉传文件。镜像名要带仓库前缀：

```sh
# 构建机
docker tag seeai:<tag> <仓库>/seeai:<tag>
docker push <仓库>/seeai:<tag>

# 服务器
sudo docker pull <仓库>/seeai:<tag>
```

`deploy/compose.prod.yaml` 第 6 行的 `seeai:${SEEAI_TAG:-local}` 随之改成 `<仓库>/seeai:${SEEAI_TAG:-local}`。私有仓库先在两台机器上 `docker login`。

### 2.5 镜像 tag

文档里的 `<tag>` 是占位符，要换成实际值。走 §2.2、§2.3 时 tag 只存在本地，除格式外没有别的约束：

- 格式：`[A-Za-z0-9_]` 开头，后接 `[A-Za-z0-9_.-]`，最长 128 字符；不能有冒号、斜杠、空格；
- 与 `SEEAI_TAG` 一致：构建成 `seeai:20261009-1` 就配 `SEEAI_TAG=20261009-1`，这个变量不带 `seeai:` 前缀；
- 不复用：回退靠 tag（§6），同一个 tag 被新构建覆盖后就回不去了。用日期或版本，例如 `20261009-1`。

走 §2.4 时镜像名还要带仓库前缀，见那节。

## 3. 运行

`deploy/compose.prod.yaml` 只起两个应用进程，PostgreSQL 与 Redis 外置，连接串放在各自的 `env_file`。两份 env 的**内容**见[部署总览 §4](deployment.md#4-配置)——容器方式由 compose 把 `API_BIND` 覆盖成 `0.0.0.0:8081`。

连接串必须从容器内可达。容器里的 `127.0.0.1` 指向本容器，不能直接复制 systemd 示例中的回环数据库地址来访问宿主数据库；填写容器可访问的数据库主机地址，并配置数据库监听、访问规则与防火墙。Redis 启用时同样处理。

以下命令在服务器部署根执行，§2.3 的示例为 `/opt/seeai`。首次创建 env 文件后按配置文档填写；升级时保留现有文件，不重复执行写空文件的 `install /dev/null`。`<tag>` 替换为已导入的标签，例如 `release-arm64`。

```sh
sudo install -d -m 750 /etc/seeai
sudo install -m 600 /dev/null /etc/seeai/api.env     # 按部署总览 §4 填
sudo install -m 600 /dev/null /etc/seeai/worker.env
sudo env SEEAI_TAG=<tag> docker compose -f deploy/compose.prod.yaml config --quiet
sudo env SEEAI_TAG=<tag> docker compose -f deploy/compose.prod.yaml up -d
```

`sudo env SEEAI_TAG=<tag> ...` 把 tag 当命令参数交给 `env`，不依赖 sudo 是否保留环境变量。

几个要点：

- API 容器内 `API_BIND=0.0.0.0:8081`（端口映射要求），宿主只发布到 `127.0.0.1:8081`，外部由 nginx 进；
- worker 每个实例的 `WORKER_ID` 必须唯一（租约按它归属）。`--scale` 会让所有副本共用同一份 `worker.env`，要跑多个 worker 就复制成不同的 service，或各给一份 env 文件；
- `stop_grace_period: 720s`：API 对齐 `GENERATION_SYNC_WAIT_SECONDS`、worker 对齐 `PROVIDER_TIMEOUT_SECONDS`（各以部署配置为准）；进程处理 SIGTERM，容器默认也发 SIGTERM（[部署总览 §6](deployment.md#6-探活与优雅停机)）；
- 多个副本同时冷启动会争迁移锁，只表现为其中一个启动稍慢（[部署总览 §3.3](deployment.md#33-数据库用户要有-ddl-权限且迁移会自动跑)）。**例外**：迁移 `0049` 会把没有名额的模型回填成 1 并当场对所有副本生效，要保留更大并发就先在升级前逐个模型钉值（同 §3.3）。

```sh
sudo docker compose -f deploy/compose.prod.yaml ps
sudo docker compose -f deploy/compose.prod.yaml logs -f seeai-api
```

## 4. 反向代理与 TLS

宿主 nginx 那份 `server` 配置照用（[部署总览 §5](deployment.md#5-反向代理与-tls)）：`Host` 透传、请求体上限、读超时的取值与理由都在那里。容器方式只多一件事——`proxy_pass` 指到宿主回环上发布的端口 `127.0.0.1:8081`，与 systemd 方式一致。

## 5. 宝塔面板

面板只管宿主：装 Docker、建站点、申请证书。镜像内部的 `/app` 与面板无关（§2.2）。

**Docker 与 compose**：面板「软件商店」装 Docker 后，在终端确认 compose v2 可用：

```sh
docker compose version
```

没有输出就补装：

```sh
sudo apt-get install -y docker-compose-v2
```

**构建镜像**：面板「构建镜像」等于 §2.2。仓库放面板的站点目录即可，例如 `/www/wwwroot/seework`，它只是 `docker build` 的构建上下文。Dockerfile 选仓库根的那份，标签栏填完整名 `seeai:<tag>`。

面板对话框不让指定构建上下文，所以提交后要核对日志首行 `Sending build context to Docker daemon`，并确认 `COPY . .` 那一步不报错——上下文不是仓库根时这一步会失败。Rust release 构建耗时较长，面板对话框未必等得到结束；等不到就改用终端跑 §2.2 的命令。

**导入镜像**：在别的机器构建后把镜像归档传上来，面板「导入镜像」对应 §2.3 的 `docker load`；本节命令使用 `.tar`。

**env 文件**：`env_file` 写死为 `/etc/seeai/api.env` 与 `/etc/seeai/worker.env`（§3），与仓库目录无关。面板文件管理器切到根目录后可以编辑 `/etc/seeai/`。

**运行**：在终端跑 §3 的 compose 命令，路径用仓库实际位置：

```sh
sudo env SEEAI_TAG=<tag> docker compose -f /www/wwwroot/seework/deploy/compose.prod.yaml up -d
```

**端口**：确认 8081 未被面板或其他站点占用。

**站点与反向代理**：面板建的站点配置在 `/www/server/panel/vhost/nginx/<域名>.conf`，三处要改：

- 删掉面板生成的 `root` 与 `try_files`；前端产物由 API 按 `Host` 分发，反代不托管静态文件（[部署总览 §5](deployment.md#5-反向代理与-tls)）；
- 「发送域名」一栏必须是 `$host`；写成 `127.0.0.1` 或写死域名后，所有请求都回客户控制台，运营后台打不开（[部署总览 §3.2](deployment.md#32-前端分发靠主机名反代要透传-host)）；
- 按[部署总览 §5](deployment.md#5-反向代理与-tls) 把 `client_max_body_size 22m;`、`proxy_read_timeout 720s;`、`proxy_send_timeout 720s;` 落进 `location`；先在面板「配置文件」里核对面板已有的默认值，再覆盖。

证书用面板申请，文件在 `/www/server/panel/vhost/cert/<域名>/`。443 的 `server` 块可以留面板生成的那份，只改 `location`。

**来源头**：`api.env` 里配 `AUTH_SOURCE_HEADER=x-real-ip`，并确认 nginx 用 `$remote_addr` 覆盖写 `X-Real-IP`（[部署总览 §5](deployment.md#5-反向代理与-tls)）。前提是 API 只绑回环（§3）。

## 6. 升级与回退

- 升级：按 §2 准备新 `<tag>` → `sudo env SEEAI_TAG=<tag> docker compose -f deploy/compose.prod.yaml up -d`（迁移与回滚口径见[部署总览 §3.3](deployment.md#33-数据库用户要有-ddl-权限且迁移会自动跑)）；
- 回退：镜像还在本地就用上一版 tag 再 `up -d`；镜像已删就从构建机重新导入（§2.3）。数据库迁移只进不退，回退数据库只能从备份恢复；
- 备份：运行镜像里没有 `pg_dump`，在 PostgreSQL 一侧或任一装了同大版本 `pg_dump` 的机器上跑（[部署总览 §7](deployment.md#7-备份)）。
