# SeeAI Hub Server Next

独立建设的 SeeAI Hub 新服务端。第一阶段只实现图片生成，不依赖旧服务端代码或数据。

## 当前纵切

- 一个 `CreateImageGenerationRequest` 接收入口与一条直接执行路径；内部只持久化最小执行与账务记录（Job/Attempt，对客不可见）；
- 两个渠道族的 Adapter：AIHubMix（`https://api.inferera.com`，同步）与 APIMart（`https://api.apib.ai`，任务式，只在 Adapter 内部）；
- 对客只有两条同步路径：`POST /v1/images/generations` 与 `POST /v1/images/edits`，两者接受同一个 JSON 请求；参考图/遮罩只收 `http(s)` 公网 URL，本地文件先经上传端点 `POST /v1/uploads/images` 换成公网 URL；
- 无图片走上游的文生图接口；有图片、可选遮罩则按渠道自己的参数走图生图/编辑；
- 结果按渠道原形返回：渠道给 `url` 就给 `url`、给 `b64_json` 就给 `b64_json`，平台不落盘静态素材；
- PostgreSQL 固化 Runtime Revision、Job、Attempt、Price Snapshot 和账本；
- 对账案例可由管理员查询（含上游对账标识），并以幂等业务键退款、释放预授权。

上架或调整模型时，向管理接口发布一份新的运行时配置即可，不需要重新编译。

## 文档去哪看

| 想知道什么 | 看哪 |
| --- | --- |
| **API 使用文档**（模型查询、各模型说明、素材上传与错误处理） | 对客读 `GET /v1/docs/README.md`（入口，索引下面三份）、`GET /v1/docs/authentication.md`、`GET /v1/docs/uploads/images.md`、`GET /v1/docs/http-errors.md`；源码在 [`public-docs/`](public-docs/README.md) |
| **怎么跑起来**（开发环境：依赖、配置、构建、两个起点、常见坑） | [`docs/operations/development.md`](docs/operations/development.md) |
| **怎么上生产**（打包、部署、反代与主机分发、systemd、备份、投产演练） | [`docs/operations/production.md`](docs/operations/production.md) |
| **全部配置项**（每个变量的缺省、含义、生产取值） | [`docs/operations/configuration.md`](docs/operations/configuration.md) |
| **容器部署**（Dockerfile、compose、镜像与升级） | [`docs/operations/production-docker.md`](docs/operations/production-docker.md) |
| **素材与模型升级教程**（改素材后怎么生效、合同变更为什么要升修订、本地库怎么处理） | [`docs/tutorials/supply-materials.md`](docs/tutorials/supply-materials.md) |
| **架构治理**：分层职责、依赖方向、边界规则（R1–R5）与扩展纪律 | [`docs/architecture.md`](docs/architecture.md) |
| 分层规则的**依据、边界细则与例外理由** | [`docs/design/0004-layered-architecture.md`](docs/design/0004-layered-architecture.md) |
| 持久决定 | [`docs/adr/`](docs/adr/) |
| 领域词汇表 | [`CONTEXT.md`](CONTEXT.md) |
| 各渠道的事实（端点、参数、计量与成本口径、实测记录） | [`docs/facts/channel-facts.md`](docs/facts/channel-facts.md) |
| 受控验证与人工验收清单（步骤、停止条件、留档要求） | [`docs/verification/`](docs/verification/) |
| 工程变更与交付记录 | [`.agents/notes/`](.agents/notes/) |
| 上游原始材料（文档、Schema 快照、实测响应） | [`out-reference/`](out-reference/) |
| 后续提案与进度 | [seeaihub-server-next#1](https://github.com/dehuadong/seeaihub-server-next/issues/1)、[seeaihub-server-next#5](https://github.com/dehuadong/seeaihub-server-next/issues/5) |

## 本地启动

```sh
# 依赖：本机 PostgreSQL 17（5432）与 Redis 7（6379，可选）；首次要先建角色与库，见 docs/operations/development.md §2
cp .env.example .env
npm --prefix apps/web ci && npm --prefix apps/web run build   # 前端产物，API 要托管它
cargo run -p seeai-api
cargo run -p seeai-worker            # 另开一个终端
```

进程启动时会自己载入 `.env`（`dotenvy`，从当前工作目录往上找，且**不覆盖**已有的环境变量），所以在仓库根 `cp .env.example .env` 之后直接 `cargo run` 就行，不用手动 `set -a`。两个进程都要有 `DATABASE_URL`；API 还要 `ADMIN_TOKEN`、`SEE_BASEURL`、`CUSTOMER_HISTORY_CURSOR_KEY`、`REQUEST_FINGERPRINT_KEY_V1`（都是**必填，缺了起不来**）与 `ADMIN_EMAIL` / `ADMIN_PASSWORD`（用来建/更新那个管理员账号）。`SEE_BASEURL` 是平台对客基址，模型说明与公共文档的链接按它写成绝对地址。

前端产物那一步**不能省**：API 只在 `apps/web/dist` 存在时才托管界面（它按编译期路径找），否则 API 与 `/v1/*` 都正常、但浏览器打不开界面。

启动前把渠道凭证放进环境变量（变量名由发布素材的 `credential_env` 指定，例如 `AIHUBMIX_API_KEY`、`APIMART_API_KEY`）。密钥不会写入数据库；Channel 只保存环境变量名称。

| 入口 | 地址 |
| --- | --- |
| 运营后台 | `http://admin.localhost:8081/` |
| 客户控制台 | `http://app.localhost:8081/` |

本机 `*.localhost` 由浏览器解析到回环，**不用改 hosts**；但 **Node 的解析器不认 `.localhost`**，脚本里要直连 `127.0.0.1:8081`。可选的供给清单来自 `SUPPLY_MATERIAL_DIR`（开发时可设 `config/bootstrap`），不设就是空的。

参考图与遮罩是**参数值**：只收 `http(s)` 公网 URL；本地文件先经上传端点换成公网 URL。平台不落盘、不校验其内容——上游不接受就会报错。

**逐项说明与常见坑见 [`docs/operations/development.md`](docs/operations/development.md)；生产部署见 [`docs/operations/production.md`](docs/operations/production.md)。**

## 验证

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

端到端合同测试默认被 `#[ignore]`，需要一个可连接的空库（它会自己派生独立库，**别指向开发库**）：

```sh
HTTP_CONTRACT_DATABASE_URL=postgres://seeai:seeai@127.0.0.1:5432/seeai_contract \
  cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1
```

浏览器行为在 `apps/web` 下跑，命令自己拉起空库、产物与 API：

```sh
npm --prefix apps/web run e2e
```

它们**都不调用真实上游**：假上游在进程内监听 `127.0.0.1`；只有显式授权的受控实测才会发真实计费调用（见 [`docs/verification/`](docs/verification/)）。

投产前该做什么演练、每条判据是什么，见 [`docs/operations/production.md`](docs/operations/production.md) §7。
