---
title: 图片按渠道原形进原形出：不落盘静态资产，也不提供异步形态
status: implemented
created: 2026-09-20
updated: 2026-09-20
approval: 用户 2026-09-20 明确指示：「无论是请求阶段还是返回结果都不要落盘静态资产」「返回的url还是base64由渠道商决定。平台判断后返回两种类型的格式：[{"b64_json":…}]或者[{"url":…}]；有客户端执行判断」「取消平台异步」，并指明参考做法是 `E:\workspace\seeaihub\docs\api\images.md`（GitHub 上没有逐字记录，本条只陈述出处，不额外推定）
verification: 2026-09-20 本地最终工作树：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets --all-features -- -D warnings` 均 exit 0；`cargo test --workspace --all-features` 全绿（0 failed）；空库端到端 `cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1` **18 passed / 0 failed**，全程本地假上游、**零真实计费调用**（由收口复核者亲自重跑）
---

# 图片按渠道原形进原形出：不落盘静态资产，也不提供异步形态

## 决定与出处

决策正文在 [`docs/adr/0019`](../../../../docs/adr/0019-images-pass-through-without-asset-storage.md)，它取代 [`docs/adr/0008`](../../../../docs/adr/0008-own-object-storage-is-the-platform-result.md)（已按约定降级为一句话存根）。范围与验收在工单 [`#12`](https://github.com/dehuadong/seeaihub-server-next/issues/12)。

## 实际交付

- **对客面只剩两条同步路径**：`POST /v1/images/generations`（JSON）与 `POST /v1/images/edits`（multipart，`image`/`mask` 是文件部件）——同一个能力、两个路径。成功都回 `{created, data:[{url|b64_json}]}`。
- **请求侧**：`image` / `image_urls`（同义、二选一）收公网 URL 或 `data:image/…;base64,…`；`mask` 收 PNG data URL。不再有 asset id、上传端点、资产角色/尺寸/摘要校验。
- **响应侧**：不下载、不解码、不归档。渠道给 `url` 就回 `url`、给 `b64_json` 就回 `b64_json`。
- **删掉的设施**：`/v1/assets` 两条路由、`POST /v1/image-generations`（202）与 `GET /v1/image-generations/{job_id}`、`AssetService`/`AssetStore`、整个 `crates/object-storage`、`generation.assets` 表与 `asset_bindings`/`result_asset_ids` 两列、`ASSET_*`/`S3_*` 环境变量与 compose 里的 minio。
- **保留**：内部 Job / Attempt / Worker 流水线（计量证据、对账、退款、对客错误改写、并发上限、路由与发布）。Job 是**内部 Generation Record**，`job_id` 只在内部与管理员面出现，不投射成对客异步协议。
- **迁移**：`migrations/0005_images_pass_through.sql` 增量删列删表、新增 `generation.jobs.result_images jsonb`（存结果信封）。

## 容易被忽略的几处

- **超时按失败处理**：同步门面等不到终态就回超时错误，文案不再暗示"稍后可取回结果"，也不再指路任何查询接口——对客根本没有这样的接口。
- **APIMart 的结果 URL 原样交给调用方**，平台不代取；该链接短期有效，过期后取不到属既定代价（决策见 0019）。
- **图片参数的规则只有一份**，落在 `crates/domain`，分成三面：受理侧只认**调用方契约字段名**（`image`、`image_urls`、`mask`，精确匹配，只有两边都给非空值才算同义冲突）；**候选声明的参数名**那套判定（名字以 `image` 开头＝参考图、含 `mask`＝遮罩，两者都像时以遮罩为准）只用于落位；**归属**由受理时按"候选声明面 + 分支"算出的**显式名单**决定，随请求交给 Driver——Driver 读图只按这份名单，透传只跳过 `model`/`prompt`/名单里的名字。归属**不看取值形状**（那会把调用方自己带的图名长尾参数误认成平台的图）。
- **参数按候选声明面过滤**（用户 2026-09-20 明确）：受理选中候选之后、写 Job 之前，只保留该候选 `capability_schema.properties` 里声明过的**顶层**名字；消费侧发了而候选没声明的，**直接丢掉——不报错、也不发给上游**（决策见 `docs/adr/0018` 的同日修订，取代原来的"一律放行透传"）。已声明的参数原样发给上游、不校验取值；必填项仍在场检查。过滤只比顶层名，候选声明了 `extra` 就整体留下（不递归进去）。
- **multipart 的判定**：带文件名的部件是图片字节，文本部件按同一套语义当参数值读（所以 `image_urls` 当文本给仍与 `image` 同义）。因此"叫 `image` 但不带文件名"的部件从"当字节"变成"当文本"。AIHubMix 的编辑路径（multipart）只能承载标量文本部件：遇到装不下的数组/对象会**明确失败并点名参数**（发生在上游调用之前），不静默丢。

## 验证

| 行为 | 证据 |
| --- | --- |
| 对客没有异步协议：旧路由 404、响应体不出现 `job`/`job_id`/`image-generations` | `public_surface_has_no_async_task_protocol` |
| 声明过的参数上行、没声明的在上游报文里完全不出现 | `declared_parameters_go_upstream_and_undeclared_ones_never_leave_the_platform` |
| 过滤不会把平台装载的参考图弄丢 | `filtering_never_drops_the_images_the_platform_places` |
| multipart 编辑路径同样只留声明的、丢掉没声明的 | `the_multipart_edit_path_keeps_declared_parameters_and_drops_undeclared_ones` |
| multipart 文本部件与 JSON 同义；同义字段都给非空值即 400 | `multipart_text_image_fields_follow_the_same_contract` |
| b64 形态原样返回 | `aihubmix_returns_the_url_shape_verbatim` |
| 公网 URL 逐字透传、data URL 解码后上传换 URL | `apimart_passes_public_urls_through_and_uploads_inline_images` |
| 两条同步路径能力等价 | `aihubmix_sync_entries_accept_images_and_return_the_provider_envelope` |
| 内部记录仍在（同步调用后直接查库到终态） | 同上的查库断言 |
| 已建过的库能升上来、旧资产表/列确实不在 | `images_pass_through_migration_applies_on_an_existing_database` |

## 未做 / 边界

- **`async` 参数不做**（对客没有异步形态）；**`cost` 不返回**（对外价是 [`#5`](https://github.com/dehuadong/seeaihub-server-next/issues/5)）。
- `content_rejected` 目前没有 Adapter 会产生它（改动前即如此），只有 application 单测覆盖。
- APIMart 取图是否需要平台那枚凭据**未实测**；按契约平台不代取，故未加任何代取逻辑。
- 上一版留下的本地样本目录（`.data/*-assets`，已被 gitignore）未删。

## 依据与关联

决策 [`docs/adr/0019`](../../../../docs/adr/0019-images-pass-through-without-asset-storage.md)（取代 0008）、工单 [`#12`](https://github.com/dehuadong/seeaihub-server-next/issues/12)、[`docs/architecture.md`](../../../../docs/architecture.md)、[`docs/design/0002`](../../../../docs/design/0002-image-generation-tech-design.md)、[`CONTEXT.md`](../../../../CONTEXT.md)。上一版契约见 [对客请求体扁平化与 image/mask 角色化](./2026-09-20-flat-request-body-and-asset-roles.md)（其资产与异步部分已作废）。
