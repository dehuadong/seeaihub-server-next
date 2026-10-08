---
title: 对客请求体扁平化与 image/mask 角色化，AIHubMix 去掉 extra
status: implemented
created: 2026-09-20
updated: 2026-09-23
approval: 用户 2026-09-20 指出"`native_parameters` 是多余的、其下应该都是顶层""`quality` 怎么还是在 `extra`""`asset_bindings` 应兼容 OpenAI 的 `mask`"并要求直接实现（GitHub 上没有对应的授权语句记录，本条只陈述出处，不额外推定）
verification: 2026-09-20 本地：`cargo fmt --check`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`、`cargo test --workspace --all-features` 全部通过；空库端到端 `cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1` **13/13 通过**（含参考图、遮罩、上传失败三条真实链路，两个 OpenAI 兼容入口的**同步**响应，以及并发上限）
---

# Agent Note：对客请求体扁平化与 image/mask 角色化，AIHubMix 去掉 extra

> **失效范围（2026-09-20 同日）**：本条里"图片是平台资产 id""有 202 受理与 job_id 轮询"的部分**已作废**——平台不再托管静态素材，对客只有同步形态。取代记录见 [图片按渠道原形进原形出](./2026-09-20-images-pass-through-without-asset-storage.md)；决策见 `docs/adr/0019`。本条里"按第一方文档把 `background`/`output_compression`/`user`/`moderation` 声明在顶层"以及"参数一律放行透传"两句也已作废：声明面改以**该模型对应端点的 `request.schema`** 为准（那四项属 `/ai/v1` 族、本平台不走），参数改按候选声明面**过滤**（没声明的丢掉）。见 [AIHubMix 声明面对齐端点 schema](./2026-09-20-aihubmix-declared-face-follows-endpoint-schema.md)。其余（扁平请求体、`Idempotency-Key`、服务端定预授权额）仍有效。

## 问题

对客受理请求原先套着 `native_parameters` 外壳，图片参数用渠道的字段名与路径（`position`），AIHubMix 另有一层 `extra`，平台自己的控制字段（`native_model_id`、`idempotency_key`、`max_cost_microusd`）也留在请求体里。调用方因此得知道命中哪个候选才能写对字段名，渠道的长尾参数要逐个由平台声明，多写一个参数还会被整体拒掉。字段命名上同样混着平台型号名与厂商原生名，没有区分"平台型号名 / 厂商原生名 / 发给渠道的模型名"三个角色。要收口的方向是请求体扁平化、图片改用 OpenAI 契约的角色名（`image`/`mask`），装载位置由平台按选中候选声明的参数面决定，而不是看调用方恰好写了哪个渠道字段名。

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
  只做请求解码与资产绑定。**响应形态**：统一入口回 `202 {job_id}`，两个兼容入口
  同日改成**同步**回图片（见下方"三条收口"）。

## 追加（同日，参数放行）

- **请求参数一律放行透传**：受理前**只要求合同的必填项在场**，取值不校验（枚举、区间、类型、
  未知字段都不管）。渠道自己的长尾参数因此不必逐个由平台声明，调用方也不会因为多写一个参数
  被整体拒掉；哪些参数需要把取值管起来，等一份明确的清单后再加在 [`validate_native_request`]
  那一处（用户 2026-09-20 说明后期统一整理）。
- **两个 Adapter 同步改成透传**：出网请求体只重写 `model`/`prompt` 与图片参数（图片由 assets
  回填），其余键原样发给上游。

## 决定

**对客受理请求换形**（`POST /v1/image-generations`）：

- **扁平**：模型参数直接写在顶层，不再有 `native_parameters` 外壳；`native_model_id`、`idempotency_key`、`max_cost_microusd` 是平台自己的控制字段。
- **图片用角色名**：`image`（一个资产 id 或 id 数组）与 `mask`（一个资产 id），对齐 OpenAI 契约的字段名。参数路径与 `position` 从调用方消失。
- **映射层落地了第一块**：平台按**选中候选**声明的参数面决定装到哪儿——AIHubMix 的 `/image`、`/mask`，APIMart 的 `/image_urls/0`、`/mask_url`。候选的参数面里没有能装这类图的参数时，该候选**不合格**，选路因此不再依赖"调用方恰好写了哪个渠道字段名"。
- **落库与 Worker 形状不变**：`generation.jobs.native_parameters` / `asset_bindings` 仍是"已落到某个候选装载面"的形态，Worker 与 Adapter 没动。

**AIHubMix 的 `extra` 去掉了**（素材 + AdapterDescriptor + 发布校验一处不改就发不出去，所以三处一起改）：

- 素材把 `quality` 等声明在**顶层**，`extra` 这一层消失；
- `AdapterDescriptor` 的顶层参数面 = `model/prompt/image/mask/n/size/output_format/quality`，不再声明 `extra`，出网时也不再摊平 `extra.*`；
- **四个参数改按第一方文档声明**（`background`、`output_compression`、`user`、`moderation`），
  依据 [`docs/adr/0018`](../../../../docs/adr/0018-open-parameters-by-first-party-docs.md)：文档写明支持的
  参数就声明，不再以"没实测"为由拦截。本条原先的"一律不声明"因此作废。

## 追加（同日，三条收口：命名 / 并发 / 同步门面）

- **字段命名统一成 `model`**：对外只有 `model`；库里与类型上叫 `gateway_model`（平台型号名，
  即运营发布时用的标识）；厂商原生名留在 `catalog.vendor_models.native_model_id`；发给渠道的是
  `provider_model_id`。迁移 `migrations/0004_gateway_model.sql` 把
  `generation.jobs` 与 `publication.runtime_entries` 的 `native_model_id` 改成 `gateway_model`
  （两者当时同值，改名只写清角色，不改取值）。
- **同一账户在**同一个网关模型**上的在飞任务数上限**：名额由运营按模型设置
  （`publication.gateway_models.max_concurrent_jobs`），模型没设时用部署缺省
  `GENERATION_MAX_CONCURRENT_JOBS`（默认 1）。受理时在同一事务里数一次该账户在该模型上
  `admitted`/`executing` 的 Job，到名额回 `429 too_many_in_flight`
  （`ApplicationError::TooManyInFlight`）。额度按**账户 × 模型**算，不是按端点——用户 2026-09-20
  的裁定是"针对端点先设计成 1 个并发数"；2026-10-08 用户指出"一个图片任务挡住同一账户的视频
  请求"不合理，维度由**账户**改成**账户 × 模型**，见
  [网关模型的并发名额](./2026-10-08-gateway-model-concurrency-quota.md)。
  **同一个幂等键的那个不算占名额**：那种请求会去重成原来那个 Job，重发不该被上限拒掉
  （用户同日："超时当然可以重发"）。
- **两个 OpenAI 兼容入口改成同步返回**：受理后等任务跑到终态（上限
  `GENERATION_SYNC_WAIT_SECONDS`，默认 120s），成功回 `{created, data:[{b64_json}]}`，失败回
  OpenAI 错误信封（平台侧语义，渠道原文不外泄）。内部流水线一字未改，同步只是门面的等待。

## 验证

| 行为 | 证据 |
| --- | --- |
| 扁平请求体受理成功 | `apps/api/tests/http_contract/harness.rs` 的 `route_request`（全部端到端用例都走它） |
| 参考图：调用方给 `image`，平台落到 APIMart 的 `image_urls` | `apimart_driver_uploads_reference_images_before_submitting` |
| 遮罩：`image` + `mask` 各落各的位置、各上传一次 | `apimart_driver_uploads_reference_image_and_mask_together` |
| 候选表达不了参考图时不合格 | `injects_array_bindings_into_the_vendors_own_array_parameter` 的后半段 |
| 只有遮罩没有参考图直接拒 | `mask_without_an_image_is_rejected` |
| `quality` 顶层直传、线上没有 `extra` | `sends_quality_on_the_wire_field`（aihubmix） |
| 素材与 Adapter 声明面一致 | `accepts_bootstrap_capability_contract`（aihubmix，读真实素材） |
| 没见过的参数原样发给上游 | `forwards_parameters_it_does_not_know`（两个 Adapter） |
| 取值放行、必填项仍拦 | `loose_and_unknown_parameters_are_passed_through`、`missing_required_parameters_are_rejected` |
| 两个兼容入口：带图/不带图都合法、分支按内容判定、图片映射到候选参数路径、Job 照样跑完 | `openai_compatible_entries_accept_and_map_assets` |
| 兼容入口**同步**返回图片 | 同上（读 `data[0].b64_json` 解出来是上游那张 PNG） |
| 在飞任务到顶回 429、跑完释放、同键重发不被误拒 | `concurrent_generations_are_capped` |
| 改名后受理与查询都不缺列 | 全部端到端用例（`active_offering` 读 `gateway_model`） |

<!-- agent-note-format: alternatives-not-recorded (pre-format Agent Note) -->

## 后果

- **预授权额的精度**：现在是一个固定数；按 Price Snapshot 算最坏成本、以及低于最小可能成本就受理前拒绝（`docs/adr/0009`），仍未实现。
- **其余字段的取值仍随候选不同**：同一个 `size`，AIHubMix 收 `1024x1024`、APIMart 收 `1:1` 并多一个 `resolution`——调用方仍要看命中哪个候选。
- **哪些参数要把取值管起来**：用户 2026-09-20 说明后期统一整理一份清单，届时加在 [`validate_native_request`] 那一处。
- **兼容入口的等待窗口**：`GENERATION_SYNC_WAIT_SECONDS` 默认 120s；等不到终态时的行为（现在按超时错误回）还没有产品裁定。

## 依据与关联

`docs/adr/0015`（调用方合同与映射层）、`docs/adr/0002`（未证实的参数不开启）、`docs/adr/0001`（统一命令）、`out-reference/openai/openai-images-api.md`（`image`/`mask` 的字段名来源）；差距登记在 [`#6`](https://github.com/dehuadong/seeaihub-server-next/issues/6)；接口与形状见 `docs/architecture.md`、`docs/design/0002` §3。
