# SeeAI Hub Server Next

独立建设的 SeeAI Hub 新服务端。第一阶段只实现图片生成，不依赖旧服务端代码或数据。

## 当前纵切

- 一个 `CreateImageGeneration` 应用命令和内部持久 Generation Job（对客不可见）；
- 两个渠道族的 Adapter：AIHubMix（`https://api.inferera.com`，同步）与 APIMart（`https://api.apib.ai`，任务式，只在 Adapter 内部）；
- 对客只有两条同步路径：`POST /v1/images/generations`（JSON）与 `POST /v1/images/edits`（multipart）；参考图/遮罩用公网 URL 或 data URL 给出；
- 无图片走上游的文生图接口；有图片、可选遮罩则按渠道自己的参数走图生图/编辑；
- 结果按渠道原形返回：渠道给 `url` 就给 `url`、给 `b64_json` 就给 `b64_json`，平台不落盘静态素材；
- PostgreSQL 固化 Runtime Revision、Job、Attempt、Price Snapshot 和账本；
- 对账案例可由管理员查询（含上游对账标识），并以幂等业务键退款、释放预授权。

上架或调整模型时，向管理接口发布一份新的运行时配置即可，不需要重新编译。

## 文档去哪看

| 想知道什么 | 看哪 |
| --- | --- |
| **代码结构**：哪个 crate / 文件 / 表负责什么 | [`docs/architecture.md`](docs/architecture.md) |
| 分层的**职责与规则**（①–⑤、R1–R4、接入清单） | [`docs/design/0004-layered-architecture.md`](docs/design/0004-layered-architecture.md) |
| 持久决定 | [`docs/adr/`](docs/adr/) |
| 领域词汇表 | [`CONTEXT.md`](CONTEXT.md) |
| 各渠道的事实（端点、参数、计量与成本口径、实测记录） | [`docs/facts/channel-facts.md`](docs/facts/channel-facts.md) |
| 受控验证清单（步骤、停止条件、留档要求） | [`docs/verification/`](docs/verification/) |
| 工程变更与交付记录 | [`.agents/notes/`](.agents/notes/) |
| 上游原始材料（文档、Schema 快照、实测响应） | [`out-reference/`](out-reference/) |
| 后续提案与进度 | [seeaihub-server-next#1](https://github.com/dehuadong/seeaihub-server-next/issues/1)、[seeaihub-server-next#5](https://github.com/dehuadong/seeaihub-server-next/issues/5) |

## 本地启动

```sh
docker compose up -d
copy .env.example .env
cargo run -p seeai-api
cargo run -p seeai-worker
```

启动前把渠道凭证放进环境变量（变量名由发布素材的 `credential_env` 指定，例如 `AIHUBMIX_API_KEY`、`APIMART_API_KEY`）。密钥不会写入数据库；Channel 只保存环境变量名称。

参考图与遮罩是**参数值**：公网 URL 或 `data:image/…;base64,…`（遮罩用 PNG data URL）。平台不落盘、不校验其内容——上游不接受就会报错。

## 验证

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

端到端合同测试默认被 `#[ignore]`，需要一个可连接的空库：

```sh
HTTP_CONTRACT_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/seeai_contract \
  cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1
```

它们**不调用真实上游**：假上游在进程内监听 `127.0.0.1`；只有显式授权的受控实测才会发真实计费调用（见 `docs/verification/`）。
