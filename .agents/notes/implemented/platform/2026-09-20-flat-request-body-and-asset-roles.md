---
title: 对客请求体扁平化与 image/mask 角色化，AIHubMix 去掉 extra
status: implemented
created: 2026-09-20
updated: 2026-09-20
approval: 用户 2026-09-20 指出"`native_parameters` 是多余的、其下应该都是顶层""`quality` 怎么还是在 `extra`""`asset_bindings` 应兼容 OpenAI 的 `mask`"并要求直接实现（GitHub 上没有对应的授权语句记录，本条只陈述出处，不额外推定）
verification: 2026-09-20 本地：`cargo fmt --check`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`、`cargo test --workspace --all-features` 全部通过；空库端到端 `cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1` **12/12 通过**（含参考图、遮罩、上传失败三条真实链路，以及两个 OpenAI 兼容入口）
---

# 对客请求体扁平化与 image/mask 角色化，AIHubMix 去掉 extra

## 追加（同日，按用户的四条决定）

- **对外的模型字段就是 `model`**：请求里不再有 `native_model_id`（那是内部/发布侧的名字），
  平台型号名与厂商原生名、发给渠道的模型名是三个角色（词汇表新增 Gateway Model）。
  Job 查询返回的字段也从 `native_model_id` 改成 `model`。**库里的列名暂未改**——三方命名的
  收口（表与类型上分开）另做，避免把一次接口改动扩成迁移。
- **幂等键改走 `Idempotency-Key` 请求头**（OpenAI 的写法，可选）：给了就用它去重，没给就
  服务端生成一个（只失去跨重试的自动去重）。
- **预授权额由服务端定**：请求体里不再有 `max_cost_microusd`；服务端按固定数给
  （`GENERATION_MAX_COST_MICROUSD`，默认 $0.02）。按 Price Snapshot 算该请求的最坏成本、
  以及 `docs/adr/0009` 要求的"低于最小可能成本就受理前拒绝"，都留作后续优化。
- **两个 OpenAI 兼容入口落地**：`POST /v1/images/generations`（JSON）与
  `POST /v1/images/edits`（`multipart/form-data`，`image`/`mask` 是文件部件，自动存成平台
  资产）。它们与统一入口**是同一个能力**：分支只看请求里有没有 `image`/`mask`，**不按端点
  断言**——带图的 generations、不带图的 edits 都合法。三个入口共用同一条受理路径；兼容入口
  只做请求解码与资产绑定。**响应仍是异步受理（202 返回 Job）**——要不要再给"等结果"的
  同步响应形态，属尚未作出的产品选择。

## 实际交付

**对客受理请求换形**（`POST /v1/image-generations`）：

- **扁平**：模型参数直接写在顶层，不再有 `native_parameters` 外壳；`native_model_id`、`idempotency_key`、`max_cost_microusd` 是平台自己的控制字段。
- **图片用角色名**：`image`（一个资产 id 或 id 数组）与 `mask`（一个资产 id），对齐 OpenAI 契约的字段名。参数路径与 `position` 从调用方消失。
- **映射层落地了第一块**：平台按**选中候选**声明的参数面决定装到哪儿——AIHubMix 的 `/image`、`/mask`，APIMart 的 `/image_urls/0`、`/mask_url`。候选的参数面里没有能装这类图的参数时，该候选**不合格**，选路因此不再依赖"调用方恰好写了哪个渠道字段名"。
- **落库与 Worker 形状不变**：`generation.jobs.native_parameters` / `asset_bindings` 仍是"已落到某个候选装载面"的形态，Worker 与 Adapter 没动。

**AIHubMix 的 `extra` 去掉了**（素材 + AdapterDescriptor + 发布校验一处不改就发不出去，所以三处一起改）：

- 素材把 `quality` 等声明在**顶层**，`extra` 这一层消失；
- `AdapterDescriptor` 的顶层参数面 = `model/prompt/image/mask/n/size/output_format/quality`，不再声明 `extra`，出网时也不再摊平 `extra.*`；
- **四个未经验证的参数一律不声明**（`background`、`output_compression`、`user`、`moderation`），依据 `docs/adr/0002`「未证实的参数不开启」——这是**有意的收窄**，请求带它们会在受理前被拒。

## 验证结果

| 行为 | 证据 |
| --- | --- |
| 扁平请求体受理成功 | `apps/api/tests/http_contract.rs` 的 `route_request`（全部端到端用例都走它） |
| 参考图：调用方给 `image`，平台落到 APIMart 的 `image_urls` | `apimart_driver_uploads_reference_images_before_submitting` |
| 遮罩：`image` + `mask` 各落各的位置、各上传一次 | `apimart_driver_uploads_reference_image_and_mask_together` |
| 候选表达不了参考图时不合格 | `injects_array_bindings_into_the_vendors_own_array_parameter` 的后半段 |
| 只有遮罩没有参考图直接拒 | `mask_without_an_image_is_rejected` |
| `quality` 顶层直传、线上没有 `extra` | `sends_quality_on_the_wire_field`（aihubmix） |
| 素材与 Adapter 声明面一致 | `accepts_bootstrap_capability_contract`（aihubmix，读真实素材） |
| 两个兼容入口：带图/不带图都合法、分支按内容判定、图片映射到候选参数路径、Job 照样跑完 | `openai_compatible_entries_accept_and_map_assets` |

## 未做（需要你的决定）

- **字段命名**：请求体里仍同时有 `native_model_id`（查目录用）与合同参数里的 `model`（由服务端强制成同一个值），冗余。要不要统一成一个"平台型号名"（例如只用 `model`）仍未定。
- **编辑请求的编码**：现在是统一 JSON + 先上传换资产 id；OpenAI 的编辑端点用 `multipart/form-data`。要不要提供兼容入口（`docs/design/0002` 里设计过 generations/edits 兼容入口，一直没实现）仍未定。
- **预授权额的精度**：现在是一个固定数；按 Price Snapshot 算最坏成本、以及低于最小可能成本就受理前拒绝（`docs/adr/0009`），仍未实现。
- **库/类型里的三方命名**：`generation.jobs.native_model_id` 装的其实是平台型号名，收口（表与类型上把平台型号名 / 厂商原生名 / 渠道模型名分开）另做。
- **其余字段的取值仍随候选不同**：同一个 `size`，AIHubMix 收 `1024x1024`、APIMart 收 `1:1` 并多一个 `resolution`——调用方仍要看命中哪个候选。

## 依据与关联

`docs/adr/0015`（调用方合同与映射层）、`docs/adr/0002`（未证实的参数不开启）、`docs/adr/0001`（统一命令）、`out-reference/openai/openai-images-api.md`（`image`/`mask` 的字段名来源）；差距登记在 [`#6`](https://github.com/dehuadong/seeaihub-server-next/issues/6)；接口与形状见 `docs/architecture.md`、`docs/design/0002` §3。
