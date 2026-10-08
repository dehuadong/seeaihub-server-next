# 生产环境（容器）

[生产环境](production.md) 的 systemd 方式之外的另一种部署：两个进程各跑一个容器，PostgreSQL 与 Redis 仍然外置。配置项与注入方式相同（见[配置项](configuration.md)），反代与 TLS 复用同一份 nginx 配置（[生产环境 §2.4](production.md#24-反向代理与-tls)）。

## 1. 与 systemd 方式的差异

| | systemd（[生产环境 §2.3](production.md#23-systemd-单元)） | 容器（本文） |
| --- | --- | --- |
| 进程管理 | 两个 systemd 单元 | 两个容器（`restart: unless-stopped`） |
| 二进制与前端产物 | 在 `/opt/seeai` 构建（[生产环境 §2.2](production.md#22-构建)） | 多阶段构建进镜像 |
| 配置注入 | `EnvironmentFile` | `env_file`（密钥系统在启动前渲染进去） |
| 停机 | 默认 SIGTERM + `TimeoutStopSec` | 默认 SIGTERM + `stop_grace_period` |
| 日志 | journald | `docker logs` 或采集器 |

相同的地方（迁移、探活判据、`Host` 分发、凭证来源）沿用[生产环境](production.md) 与[配置项](configuration.md)；容器里没有需要持久化的本地状态。

**宿主路径不进镜像**：systemd 方式把宿主上的编译期路径写进二进制（[生产环境 §1.1](production.md#11-appswebdist-必须在编译期那个路径下就位)），容器方式在镜像内部固定成 `/app`（§2.2）。所以仓库放哪台机器、哪个目录都可以，宿主只需提供两个 env 文件（§3）。

## 2. 准备镜像

`deploy/compose.prod.yaml` 只认一件事：**本机镜像列表里有一个 `seeai:<tag>`**（第 6 行 `image: seeai:${SEEAI_TAG:-local}`）。镜像从哪来由你定，下面三条路选一条。

| 方式 | 何时用 | 宝塔按钮（§5） | 做哪些节 |
| --- | --- | --- | --- |
| 在本机构建 | 服务器上有源码与 Docker | 构建镜像 | §2.2 |
| 从别处导入 | 构建在另一台机器上，服务器不能直连镜像仓库 | 导入镜像 | 构建机 §2.2 → 服务器 §2.3 |
| 从镜像仓库拉取 | 服务器能直连镜像仓库 | 从仓库中拉取 | §2.4 |

三条路的结果一样：`docker images` 里出现 `seeai:<tag>`。之后都进 §3。

### 2.1 装 Docker

```sh
sudo apt-get install -y docker.io docker-compose-v2
sudo systemctl enable --now docker
```

下面服务器侧的命令按 `sudo docker` 写；把账号加进 `docker` 组并重新登录后可省掉 `sudo`。

### 2.2 在本机构建

`Dockerfile` 三个阶段：`web` 用 Node 构建两份前端产物，`build` 用 Rust 构建两个二进制，`runtime` 用 Debian slim 只带运行所需的东西，以非 root 用户跑。

API 按编译期绝对路径找前端产物（[生产环境 §1.1](production.md#11-appswebdist-必须在编译期那个路径下就位)）：构建与运行都在 `/app`，产物放 `/app/apps/web/dist`，并保留 `/app/apps/api` 这个空目录——路径里的 `apps/api/../web/dist` 要先能进 `apps/api`，中间目录不存在时 `..` 解析不到。改镜像布局时这两条要一起看。

迁移已编进二进制，运行镜像里不需要 `migrations/`（[生产环境 §1.3](production.md#13-数据库用户要有-ddl-权限且迁移会自动跑)）。镜像把素材放在 `/app/config/bootstrap`，而 `SUPPLY_MATERIAL_DIR` 不设时默认就是 `config/bootstrap`（相对 `WORKDIR /app`），所以镜像起来就导入（[配置项 §6](configuration.md#6-供给素材导入)）；用自己的素材则挂一个只读卷并把变量指过去。

在仓库根构建（末尾的 `.` 是**构建上下文**，必须是仓库根——`COPY . .` 与 `.dockerignore` 都相对它解析）：

```sh
docker build -t seeai:<tag> .
```

`-f` 只指定 Dockerfile，与上下文是两件事；Dockerfile 在别处时写成：

```sh
docker build -f <仓库根>/Dockerfile -t seeai:<tag> <仓库根>
```

服务器上没有仓库时，用[生产环境 §2.2](production.md#22-构建) 的 `scripts/deploy/pack-deploy-tree.mjs` 打包整棵树传上去：包里已有 `Dockerfile`、`.dockerignore`、`deploy/compose.prod.yaml` 与前端源码，解包后在部署根直接 `docker build`。

镜像架构与构建机一致；服务器架构不同时用 `--platform` 指定，或在服务器上构建。

### 2.3 从别处导入

构建机与服务器不是同一台时走这节；同一台则构建完直接进 §3。

服务器不能直连镜像仓库时，在构建机导出再上传：

```sh
docker save seeai:<tag> | gzip > seeai-<tag>.tar.gz
scp seeai-<tag>.tar.gz <服务器>:/tmp/
```

服务器上导入：

```sh
gzip -dc /tmp/seeai-<tag>.tar.gz | sudo docker load
```

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

`deploy/compose.prod.yaml` 只起两个应用进程，PostgreSQL 与 Redis 外置，连接串放在各自的 `env_file`。两份 env 的必填项与写法见[生产环境 §2.3](production.md#23-systemd-单元)——容器方式只把 `API_BIND` 覆盖成 `0.0.0.0:8081`。

```sh
sudo install -d -m 750 /etc/seeai
sudo install -m 600 /dev/null /etc/seeai/api.env     # 按 configuration.md 填
sudo install -m 600 /dev/null /etc/seeai/worker.env
sudo env SEEAI_TAG=<tag> docker compose -f deploy/compose.prod.yaml up -d
```

`sudo env SEEAI_TAG=<tag> ...` 把 tag 当命令参数交给 `env`，不依赖 sudo 是否保留环境变量。

几个要点：

- API 容器内 `API_BIND=0.0.0.0:8081`（端口映射要求），宿主只发布到 `127.0.0.1:8081`，外部由 nginx 进；
- worker 每个实例的 `WORKER_ID` 必须唯一（租约按它归属）。`--scale` 会让所有副本共用同一份 `worker.env`，要跑多个 worker 就复制成不同的 service，或各给一份 env 文件；
- `stop_grace_period: 720s`：API 对齐 `GENERATION_SYNC_WAIT_SECONDS`、worker 对齐 `PROVIDER_TIMEOUT_SECONDS`（各以部署配置为准）；进程处理 SIGTERM，容器默认也发 SIGTERM（[生产环境 §3](production.md#3-启动与探活)）；
- 多个副本同时冷启动会争迁移锁，只表现为其中一个启动稍慢（[生产环境 §1.3](production.md#13-数据库用户要有-ddl-权限且迁移会自动跑)）。

```sh
sudo docker compose -f deploy/compose.prod.yaml ps
sudo docker compose -f deploy/compose.prod.yaml logs -f seeai-api
```

## 4. 反向代理与 TLS

反代仍由宿主 nginx 终止 TLS：[生产环境 §2.4](production.md#24-反向代理与-tls) 那份 `server` 配置照用（`Host` 透传、请求体上限、读超时的取值与理由都在那里）。容器方式只多一件事：`proxy_pass` 指到宿主回环上发布的端口（`127.0.0.1:8081`）；别让 nginx 去托管静态产物，分发仍由 API 按主机名决定。

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

**导入镜像**：在别的机器构建后把 `.tar.gz` 传上来，面板「导入镜像」等于 §2.3 的 `docker load`。

**env 文件**：`env_file` 写死为 `/etc/seeai/api.env` 与 `/etc/seeai/worker.env`（§3），与仓库目录无关。面板文件管理器切到根目录后可以编辑 `/etc/seeai/`。

**运行**：在终端跑 §3 的 compose 命令，路径用仓库实际位置：

```sh
sudo env SEEAI_TAG=<tag> docker compose -f /www/wwwroot/seework/deploy/compose.prod.yaml up -d
```

**端口**：确认 8081 未被面板或其他站点占用。

**站点与反向代理**：面板建的站点配置在 `/www/server/panel/vhost/nginx/<域名>.conf`，三处要改：

- 删掉面板生成的 `root` 与 `try_files`；前端产物由 API 按 `Host` 分发，反代不托管静态文件（§4）；
- 「发送域名」一栏必须是 `$host`；写成 `127.0.0.1` 或写死域名后，所有请求都回客户控制台，运营后台打不开（§4）；
- 按 §4 把 `client_max_body_size 22m;`、`proxy_read_timeout 720s;`、`proxy_send_timeout 720s;` 落进 `location`；先在面板「配置文件」里核对面板已有的默认值，再覆盖。

证书用面板申请，文件在 `/www/server/panel/vhost/cert/<域名>/`。443 的 `server` 块可以留面板生成的那份，只改 `location`。

**来源头**：`api.env` 里配 `AUTH_SOURCE_HEADER=x-real-ip`，并确认 nginx 用 `$remote_addr` 覆盖写 `X-Real-IP`（[生产环境 §2.4](production.md#24-反向代理与-tls)）。前提是 API 只绑回环（§3）。

## 6. 升级与备份

- 升级：按 §2 准备新 `<tag>` → `sudo env SEEAI_TAG=<tag> docker compose -f deploy/compose.prod.yaml up -d`（迁移与回滚口径见[生产环境 §9](production.md#9-升级与重启)）；
- 回退：镜像还在本地就用上一版 tag 再 `up -d`；镜像已删就从构建机重新导入。数据库迁移只进不退，回退数据库只能从备份恢复；
- 备份：运行镜像里没有 `pg_dump`，在 PostgreSQL 一侧或任一装了同大版本 `pg_dump` 的机器上跑（[生产环境 §6](production.md#6-备份)）。

## 7. 投产前的演练

与[生产环境 §7](production.md#7-投产前的演练)同一张清单，容器方式把「起进程」换成 `sudo docker compose -f deploy/compose.prod.yaml up -d`，判据不变：`/health` 回 200、`admin.<域名>` 出运营后台、`app.<域名>` 出客户控制台、能发布并停用一个平台模型。
