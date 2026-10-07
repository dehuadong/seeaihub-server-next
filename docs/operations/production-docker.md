# 生产环境（容器）

[生产环境](production.md) 的 systemd 方式之外的另一种部署：两个进程各跑一个容器，PostgreSQL 与 Redis 仍然外置。配置项与注入方式相同（见[配置项](configuration.md)），反代与 TLS 复用同一份 nginx 配置（[生产环境 §2.5](production.md#25-反向代理与-tls)）。

## 1. 与 systemd 方式的差异

| | systemd（[生产环境 §2.4](production.md#24-systemd-单元)） | 容器（本文） |
| --- | --- | --- |
| 进程管理 | 两个 systemd 单元 | 两个容器（`restart: unless-stopped`） |
| 二进制与前端产物 | 构建机出包，解到 `/opt/seeai`（[生产环境 §2.2](production.md#22-打包与部署)） | 多阶段构建进镜像 |
| 配置注入 | `EnvironmentFile` | `env_file`（密钥系统在启动前渲染进去） |
| 停机 | 默认 SIGTERM + `TimeoutStopSec` | 默认 SIGTERM + `stop_grace_period` |
| 日志 | journald | `docker logs` 或采集器 |

相同的地方（迁移、探活判据、`Host` 分发、凭证来源）沿用[生产环境](production.md) 与[配置项](configuration.md)；容器里没有需要持久化的本地状态。

## 2. 镜像

### 2.1 装 Docker

```sh
sudo apt-get install -y docker.io docker-compose-v2
sudo systemctl enable --now docker
```

下面服务器侧的命令按 `sudo docker` 写；把账号加进 `docker` 组并重新登录后可省掉 `sudo`。

### 2.2 构建

`Dockerfile` 三个阶段：`web` 用 Node 构建两份前端产物，`build` 用 Rust 构建两个二进制，`runtime` 用 Debian slim 只带运行所需的东西，以非 root 用户跑。

API 按编译期绝对路径找前端产物（[生产环境 §1.1](production.md#11-appswebdist-必须在构建-api-之前就位)）：构建与运行都在 `/app`，产物放 `/app/apps/web/dist`，并保留 `/app/apps/api` 这个空目录——路径里的 `apps/api/../web/dist` 要先能进 `apps/api`，中间目录不存在时 `..` 解析不到。改镜像布局时这两条要一起看。

迁移已编进二进制，运行镜像里不需要 `migrations/`（[生产环境 §1.3](production.md#13-数据库用户要有-ddl-权限且迁移会自动跑)）。镜像把素材放在 `/app/config/bootstrap`，而 `SUPPLY_MATERIAL_DIR` 不设时默认就是 `config/bootstrap`（相对 `WORKDIR /app`），所以镜像起来就导入（[配置项 §6](configuration.md#6-供给素材导入)）；用自己的素材则挂一个只读卷并把变量指过去。

在仓库根构建：

```sh
docker build -t seeai:<tag> .
```

镜像架构与构建机一致；服务器架构不同时用 `--platform` 指定，或在服务器上构建。

### 2.3 把镜像带到服务器

服务器不能直连镜像仓库时，在构建机导出再上传：

```sh
docker save seeai:<tag> | gzip > seeai-<tag>.tar.gz
scp seeai-<tag>.tar.gz <服务器>:/tmp/
```

服务器上导入：

```sh
gzip -dc /tmp/seeai-<tag>.tar.gz | sudo docker load
```

## 3. 运行

`deploy/compose.prod.yaml` 只起两个应用进程，PostgreSQL 与 Redis 外置，连接串放在各自的 `env_file`。两份 env 的必填项与写法见[生产环境 §2.4](production.md#24-systemd-单元)——容器方式只把 `API_BIND` 覆盖成 `0.0.0.0:8081`。

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

反代仍由宿主 nginx 终止 TLS：[生产环境 §2.5](production.md#25-反向代理与-tls) 那份 `server` 配置照用（`Host` 透传、请求体上限、读超时的取值与理由都在那里）。容器方式只多一件事：`proxy_pass` 指到宿主回环上发布的端口（`127.0.0.1:8081`）；别让 nginx 去托管静态产物，分发仍由 API 按主机名决定。

## 5. 升级与备份

- 升级：构建并导入新 `<tag>` → `sudo env SEEAI_TAG=<tag> docker compose -f deploy/compose.prod.yaml up -d`（迁移与回滚口径见[生产环境 §9](production.md#9-升级与重启)）；
- 回退：镜像还在本地就用上一版 tag 再 `up -d`；镜像已删就从构建机重新导入。数据库迁移只进不退，回退数据库只能从备份恢复；
- 备份：运行镜像里没有 `pg_dump`，在 PostgreSQL 一侧或任一装了同大版本 `pg_dump` 的机器上跑（[生产环境 §6](production.md#6-备份)）。

## 6. 投产前的演练

与[生产环境 §7](production.md#7-投产前的演练)同一张清单，容器方式把「起进程」换成 `sudo docker compose -f deploy/compose.prod.yaml up -d`，判据不变：`/health` 回 200、`admin.<域名>` 出运营后台、`app.<域名>` 出客户控制台、能发布并停用一个平台模型。
