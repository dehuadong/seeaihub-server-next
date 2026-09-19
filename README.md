# SeeAI Hub Server Next

独立建设的 SeeAI Hub 新服务端。第一阶段只实现图片生成，不依赖旧服务端代码或数据。

本仓库后续提案与进度入口为 [seeaihub-server-next#1](https://github.com/dehuadong/seeaihub-server-next/issues/1)。历史产品与架构来源保留在 [seeaihub#674](https://github.com/dehuadong/seeaihub/issues/674)，技术设计以仓库内 `docs/design/` 和 `docs/adr/` 为准。

## 当前纵切

- 一个 `CreateImageGeneration` 应用命令和持久 Job；
- AIHubMix Provider，默认 Base URL 为 `https://api.inferera.com`；
- 无图片由 Adapter 调用 `/v1/images/generations`；
- 有图片、可选 mask，由 Adapter 调用 `/v1/images/edits`；
- PostgreSQL 固化 Runtime Revision、Job、Attempt、Price Snapshot 和账本；
- 本地文件或 S3 兼容对象存储承载 Asset。
- 对账案例可由管理员查询，并以幂等业务键退款、释放预授权；没有可核验 Metering Evidence 时不能人工确认扣款。

上架或调整模型时，向管理接口发布一份新的运行时配置即可，不需要重新编译。每个 `native_model_id` 独立切换自己的活动发布项；同一次发布固定 Capability Schema、Adapter、Provider Model ID、Channel 和价格快照，不会误下架其他模型。

## 本地启动

```sh
docker compose up -d
copy .env.example .env
cargo run -p seeai-api
cargo run -p seeai-worker
```

启动前需要把 `AIHUBMIX_API_KEY` 放进环境变量。密钥不会写入数据库；Channel 只保存环境变量名称。
默认使用本地对象目录；把 `ASSET_STORE` 改为 `s3` 后可使用 Compose 自动创建的 MinIO `seeai-assets` bucket。

输入资产角色只接受 `image` 或 `mask`。Mask 必须是带 alpha 通道的 PNG，并且尺寸与输入图片一致。
