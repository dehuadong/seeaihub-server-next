# AIHubMix `gpt-image-2` 第一方协议调研

> 调研日期：2026-09-18
> 用途：为历史提案 #674 与本仓库首个图片 Provider 提供事实依据
> 性质：上游协议研究与受控验证证据，不是平台对外接口合同
> 位置：**2026-09-19 从 `out-reference/aihubmix/` 迁到 `docs/research/`**——它是我们自己写的调研，按 `docs/agents/artifacts.md` 的分工不属于「上游或第三方原始材料」；原始证据（响应样本、Schema 快照）仍在 `out-reference/aihubmix/`。

## 0. 更正记录（2026-09-19 整改）

本文件此前有**两处结论是错的**，另有一处与另一渠道的对比混入。以下是更正，正文相应位置已标注：

| # | 原结论（错） | 更正后 | 依据 |
| --- | --- | --- | --- |
| 1 | §9.1 / §10：公开价**只有三项**（文本输入 / 图片输入 / 图片输出），仓库旧资料里的「文本输出 `$10 / 1M`」**不予采用** | **四档都要用**：文本输入 `$5`、**文本输出 `$10`**、图像输入 `$8`、图像输出 `$30`（每 1M tokens）。当初按"只有三项"发布，导致生效配置里**文本输出费率写成 0**，即少收费——该缺陷记录在 `docs/facts/channel-facts.md` §2.11 | 用户 2026-09-19 确认四档；`docs/facts/channel-facts.md` §2.4 |
| 2 | §6.1（据文档）：**即使同步执行，AIHubMix 也会保存任务记录**，创建响应丢失时可通过 `GET /ai/v1/images` 查找 | **对本渠道的 `/v1` 同步分支不成立**：§13.2 实测两次同步调用**未出现在** `/ai/v1/images` 列表里。因此 `/v1` 的创建请求失联后**没有**可查询的上游任务 ⇒ 只能进对账（见 `docs/adr/0005`/`0007`） | 本文件 §13.2 实测 |
| 3 | §7.1b 末：以「与火山方舟的行为相反」作对比 | 已删除。渠道差异不互相推导（`docs/design/0004` R1）；火山方舟的事实只在 `out-reference/doubao/doubao-ark-image-research.md` | `docs/design/0004` R1 |

**另外**：§12 的验证清单已在 §13 完成（§13 取代相冲突的"待确认"），§12 保留为历史过程记录。

## 1. 结论

- **事实**：AIHubMix 将模型标识公开为 `gpt-image-2`，输入模态为文本和图片，使用 `Authorization: Bearer $AIHUBMIX_API_KEY`。官方文档示例使用 `https://aihubmix.com`；本项目按用户确认选择 `https://api.inferera.com` 作为 Channel 默认 Base URL，并已用只读 Schema 与任务列表验证连通性。[模型说明](https://api.inferera.com/model/gpt-image-2/llms.txt) · [异步任务文档](https://docs.aihubmix.com/en/api/async-tasks.md#api-overview)
- **事实**：`POST /ai/v1/images/generations` 是一个统一 JSON 接口：没有 `image/images` 时为文生图，有 `image/images` 时为图生图或编辑，`mask` 必须与 `image/images` 同时出现。它默认同步，传布尔值 `async: true` 后异步执行。[实时模型 Schema](https://aihubmix.com/call/schema/models/gpt-image-2/endpoints) · [异步任务文档](https://docs.aihubmix.com/en/api/async-tasks.md#create-async-task)
- **推论**：这与 #674 的“文生图和图生图共用一个应用 Command，由输入图片决定操作”方向一致。第一期 Adapter 可以优先使用 `/ai/v1/images/generations`，不必因上游另有 `/v1/images/generations`、`/v1/images/edits` 就拆成两套领域流程。
- **事实**：同一模型还提供 OpenAI 兼容接口：`POST /v1/images/generations` 使用 JSON，`POST /v1/images/edits` 使用 `multipart/form-data`；这两个接口在实时 Schema 中都标成同步。[实时模型 Schema](https://aihubmix.com/call/schema/models/gpt-image-2/endpoints)
- **事实**：模型说明称生成可能超过 5 分钟，并建议客户端超时至少 10 分钟；异步任务需要账户预先开通，否则返回 `403 async_not_enabled`。[模型说明](https://api.inferera.com/model/gpt-image-2/llms.txt) · [异步任务文档](https://docs.aihubmix.com/en/api/async-tasks.md)
- **推论**：新服务端仍应先落自己的持久 Job，再调用 Provider。若账户已开通异步能力，首选 `async: true`；不应让面向调用方的 HTTP 连接等待十分钟。
- **事实**：公开模型页当前展示的价格为文本输入 `$5 / 1M tokens`、图片输入 `$8 / 1M tokens`、图片输出 `$30 / 1M tokens`。[模型页](https://aihubmix.com/model/gpt-image-2)
- **待确认**：`/ai/v1` 的任务对象文档没有 `usage` 字段，而仓库中的 OpenAI 兼容接口实测样本有分项 token 用量。生产计费前必须确认异步任务的权威用量/账单来源，不能仅靠本地估算。[异步任务对象](https://docs.aihubmix.com/en/api/async-tasks.md#task-object) · [本地实测样本](../../out-reference/aihubmix/gpt_image_2_generations.json)
- **待确认**：实时 Schema 与模型介绍/旧资料在 `input_fidelity`、`quality=auto`、`moderation`、`response_format`、`webp` 等字段上不一致。实现时应以实时 Schema 快照为默认合同，并通过付费冒烟测试确认差异，不能把旧文档字段直接固化进 Native Schema。[实时模型 Schema](https://aihubmix.com/call/schema/models/gpt-image-2/endpoints) · [模型说明](https://api.inferera.com/model/gpt-image-2/llms.txt) · [仓库旧资料](../../out-reference/aihubmix/gpt-image-2.md)

**建议**：可以把 AIHubMix 作为 #674 的首个 Provider 候选，并把文生图、图生图、多图参考、遮罩放在同一阶段；但“异步用量证据”和“冲突参数的真实可用性”是进入生产计费前的阻塞验证项。

## 2. 来源与证据等级

| 等级 | 来源 | 本文用途 |
| --- | --- | --- |
| A | [AIHubMix 实时模型 Schema](https://aihubmix.com/call/schema/models/gpt-image-2/endpoints) | 当前端点、Content-Type、请求字段、约束、生命周期；模型说明明确称该 Schema 始终最新且权威 |
| A | [AIHubMix 异步任务文档](https://docs.aihubmix.com/en/api/async-tasks.md) | Task 状态、轮询、下载、Webhook、错误和恢复语义 |
| A | [AIHubMix 模型页](https://aihubmix.com/model/gpt-image-2) | 当前公开价格、模型身份和模态 |
| B | [用户给出的 Inferera 模型说明](https://api.inferera.com/model/gpt-image-2/llms.txt) | 模型摘要、端点索引、鉴权、超时、结果 URL 和错误概览；正文给出的 canonical 地址是 `aihubmix.com` 下的同名页面 |
| B | [AIHubMix HTTP 状态码说明](https://docs.aihubmix.com/en/FAQs/HTTP-Codes.md) | OpenAI 兼容/通用接口的错误概览 |
| 仓库观察 | [现有说明](../../out-reference/aihubmix/gpt-image-2.md)、[实测响应](../../out-reference/aihubmix/gpt_image_2_generations.json) | 与最新第一方资料做差异对照；不能反向覆盖当前第一方合同 |

说明：本文没有引用第三方博客。第 1–12 节保留付费验证前的调研过程，第 13 节记录随后完成的受控真实付费验证；后者取代前文相冲突的“待确认”结论。

## 3. 服务地址、鉴权与模型身份

| 项目 | 结论 | 性质与来源 |
| --- | --- | --- |
| Provider | AIHubMix | **事实**：[模型页](https://aihubmix.com/model/gpt-image-2) |
| 模型 ID | `gpt-image-2` | **事实**：[模型说明](https://api.inferera.com/model/gpt-image-2/llms.txt) |
| 开发者 | OpenAI | **事实**：[模型说明](https://api.inferera.com/model/gpt-image-2/llms.txt) |
| 输入模态 | `text,image` | **事实**：[模型说明](https://api.inferera.com/model/gpt-image-2/llms.txt) |
| 官方文档 Base URL | `https://aihubmix.com` | **事实**：[异步任务文档](https://docs.aihubmix.com/en/api/async-tasks.md#api-overview) |
| 本项目默认 Channel Base URL | `https://api.inferera.com` | **已确认并只读验证**：用户指定；2026-09-18 实测任务列表和模型 Schema 均返回 HTTP 200 |
| 鉴权 | `Authorization: Bearer $AIHUBMIX_API_KEY` | **事实**：[异步任务文档](https://docs.aihubmix.com/en/api/async-tasks.md#api-overview) |
| Schema 查询 | `GET /call/schema/models/gpt-image-2/endpoints`，无需 Bearer Token | **事实**：[模型说明](https://api.inferera.com/model/gpt-image-2/llms.txt) · [异步任务文档](https://docs.aihubmix.com/en/api/async-tasks.md#model-schema) |
| `api.inferera.com` 是否可作为 Base URL | 可以；`GET /ai/v1/images` 与 `GET /call/schema/models/gpt-image-2/endpoints` 已验证 | **已确认**：用户指定作为默认值；只读接口实测 HTTP 200，付费 POST 不重复执行 |

**决定**：Provider 配置继续把 Base URL 作为 Channel 运行时字段，不写死在 Adapter；首版默认值采用 `https://api.inferera.com`。保存时规范化去除尾部 `/`，Adapter 再拼接 `/v1/...` 或 `/ai/v1/...`，避免双斜杠。

## 4. 三个端点的职责

| 路径 | 请求格式 | 同步性 | 能力 | 性质与来源 |
| --- | --- | --- | --- | --- |
| `POST /ai/v1/images/generations` | `application/json` | 默认同步；支持 `async: true` | 文生图、单图/多图编辑、遮罩、Webhook | **事实**：[实时模型 Schema](https://aihubmix.com/call/schema/models/gpt-image-2/endpoints) |
| `POST /v1/images/generations` | `application/json` | 同步 | OpenAI 兼容文生图 | **事实**：[实时模型 Schema](https://aihubmix.com/call/schema/models/gpt-image-2/endpoints) |
| `POST /v1/images/edits` | `multipart/form-data` | 同步 | OpenAI 兼容图片编辑，`image` 必填，`mask` 可选 | **事实**：[实时模型 Schema](https://aihubmix.com/call/schema/models/gpt-image-2/endpoints) |

**推论**：

1. 新服务端内部只需要一个 `CreateImageGeneration`；外部若将来兼容 OpenAI 的 generations/edits 两个入口，只做入站解析差异，最后汇合到同一个 Command。
2. AIHubMix Adapter 首选 `/ai/v1/images/generations`，通过 `image/images/mask` 组合表达原生操作；OpenAI 兼容端点可保留为故障诊断或协议兼容选项，不应成为领域模型的拆分依据。
3. `endpoint_family` 和实际路径要记录在 Attempt 上，因为不同端点的请求格式、响应证据和错误可能不同。

## 5. `/ai/v1/images/generations` Native Schema

下表来自 2026-09-18 获取的 `schema_version: 1.2`。[实时模型 Schema](https://aihubmix.com/call/schema/models/gpt-image-2/endpoints)

| 字段 | 当前约束 | 性质 |
| --- | --- | --- |
| `model` | string，必填 | **事实** |
| `prompt` | string，非空，必填；没有图片输入时最大 32,000 字符 | **事实** |
| `async` | boolean，默认 `false` | **事实** |
| `image` | string 或 `{ "url": string }`；媒体引用 | **事实** |
| `images` | array/null，最多 16 项；每项为 string 或 `{ "url": string }` | **事实** |
| `mask` | 非空媒体引用；出现时必须同时有 `image` 或非空 `images` | **事实**；Schema 自身的类型声明有冲突，见下文 |
| `n` | integer/null，`1..10`，默认 `1` | **事实** |
| `output_format` | `png`、`jpeg` 或 null，默认 `png` | **事实** |
| `size` | `auto`、`{width}x{height}` 或 null；有图片输入时进一步限制为 `auto`、`1024x1024`、`1536x1024`、`1024x1536` 或 null | **事实** |
| `webhook_url` | URI，最多 512 字符 | **事实** |
| `webhook_events_filter` | 非空且去重的 `completed/failed/cancelled` 子集 | **事实** |
| `extra` | object/null；只接受 `background`、`output_compression`、`quality`、`user` | **事实** |
| 未声明字段 | 顶层与 `extra` 都设置 `additionalProperties: false` | **事实** |

`extra` 的模型原生扩展：[实时模型 Schema](https://aihubmix.com/call/schema/models/gpt-image-2/endpoints)

| 字段 | 当前约束 | 性质 |
| --- | --- | --- |
| `extra.background` | `transparent/opaque/auto`，默认 `auto`；`transparent` 要求 PNG | **事实** |
| `extra.output_compression` | integer，`0..100`，默认 `100`；仅 JPEG 有效 | **事实** |
| `extra.quality` | `low/medium/high` | **事实** |
| `extra.user` | string，传给上游的最终用户标识 | **事实** |

### 5.1 操作判定

- **事实**：没有 `image` 且 `images` 为空/缺失时，Schema 走纯文生图分支。[实时模型 Schema](https://aihubmix.com/call/schema/models/gpt-image-2/endpoints)
- **事实**：`image` 是单图输入，`images` 是多图输入；Schema 标注两者可双向归并，`image == images[0]`。[实时模型 Schema](https://aihubmix.com/call/schema/models/gpt-image-2/endpoints)
- **事实**：`mask` 不能脱离 `image/images` 单独提交。[实时模型 Schema](https://aihubmix.com/call/schema/models/gpt-image-2/endpoints)
- **推论**：应用层可把 `image` 规范化成 `images[0]`，但必须保存调用方原始请求和规范化请求，方便审计与复现。
- **推论**：Native Operation 可以由输入组合派生为 `generate`、`edit`、`masked_edit`，不需要调用方再提交一个可能冲突的 `operation` 字段。

### 5.2 当前文档冲突

| 冲突 | 第一方现状 | 处理建议 |
| --- | --- | --- |
| `input_fidelity` | 模型说明称模型支持 high input fidelity，但实时 Schema 没有该字段，且拒绝未知字段 | **待确认**；首版 Native Schema 不开放，付费联调确认后再按版本加入。[模型说明](https://api.inferera.com/model/gpt-image-2/llms.txt) · [实时模型 Schema](https://aihubmix.com/call/schema/models/gpt-image-2/endpoints) |
| `quality=auto` | 模型页/仓库旧示例使用 `auto`，实时 Schema 只允许 `low/medium/high` | **待确认**；自动导入时遵守实时 Schema，不把示例值越权加入枚举。[模型页](https://aihubmix.com/model/gpt-image-2) · [实时模型 Schema](https://aihubmix.com/call/schema/models/gpt-image-2/endpoints) |
| `moderation` | 仓库旧资料列出 `auto/low`，实时 Schema 没有该字段 | **待确认**；首版不开放。[仓库旧资料](../../out-reference/aihubmix/gpt-image-2.md) · [实时模型 Schema](https://aihubmix.com/call/schema/models/gpt-image-2/endpoints) |
| `response_format` | 通用异步文档列出 `url/b64_json`，模型实时 Schema 没有该字段 | **待确认**；Adapter 响应解析同时兼容 URL 和 Base64，但请求侧不承诺可选择。[异步任务文档](https://docs.aihubmix.com/en/api/async-tasks.md#image-parameters) · [实时模型 Schema](https://aihubmix.com/call/schema/models/gpt-image-2/endpoints) |
| `webp` | 通用异步文档列出 `webp`，模型实时 Schema 仅允许 PNG/JPEG | **事实 + 待确认**；模型级 Schema 应覆盖通用字段全集。[异步任务文档](https://docs.aihubmix.com/en/api/async-tasks.md#image-parameters) · [实时模型 Schema](https://aihubmix.com/call/schema/models/gpt-image-2/endpoints) |
| `mask` 类型 | 同一实时 Schema 同时含 `type: string` 和允许 `{url}` 的 `oneOf` | **待确认**；这是自相矛盾的 JSON Schema，首版先只接受 string，待联调验证 object。[实时模型 Schema](https://aihubmix.com/call/schema/models/gpt-image-2/endpoints) |
| 文生图尺寸 | 文生图分支只校验 `数字x数字`，图生图分支才列出三个尺寸和 `auto` | **待确认**；任意数字尺寸可能仍被上游拒绝，应保留 `size_not_supported` 映射并做真实能力测试。[实时模型 Schema](https://aihubmix.com/call/schema/models/gpt-image-2/endpoints) · [HTTP 状态码](https://docs.aihubmix.com/en/FAQs/HTTP-Codes.md) |

## 6. 同步、异步与 Job 生命周期

### 6.1 Provider 事实

- **事实**：图片接口默认同步；`async: true` 时立即返回任务对象，并在后台继续生成。文档称即使同步执行也会保存任务记录、创建响应丢失时可通过 `GET /ai/v1/images` 查找——**⚠️ 实测更正（见文首第 2 条）**：这句话对本渠道的 `/v1` 同步分支**不成立**，两次同步调用未出现在任务列表里；只有 `/ai/v1` 的异步任务才在列表里。[异步任务文档](https://docs.aihubmix.com/en/api/async-tasks.md#create-async-task)
- **事实**：异步能力需在账户后台开通；未开通返回 `403 async_not_enabled`。[异步任务文档](https://docs.aihubmix.com/en/api/async-tasks.md)
- **事实**：详情轮询为 `GET /ai/v1/images/{id}`；状态为 `pending`、`in_progress`、`completed`、`failed`、`cancelled`，建议每 15 秒轮询一次。[异步任务文档](https://docs.aihubmix.com/en/api/async-tasks.md#task-status)
- **事实**：实时 Schema 的 `cancel_path` 为空；当前文档没有给出图片取消接口。[实时模型 Schema](https://aihubmix.com/call/schema/models/gpt-image-2/endpoints)
- **事实**：查询媒体详情可能刷新状态；任务列表只是快照，不主动刷新状态。[异步任务文档](https://docs.aihubmix.com/en/api/async-tasks.md#query-tasks)

### 6.2 对新服务端的影响

- **推论**：本地 Job 必须在第一次上游调用前持久化；Worker 认领后只创建一个 Provider Attempt，避免客户端重试直接产生第二次付费生成。
- **推论**：Attempt 至少保存 `provider`、`model=gpt-image-2`、`endpoint_family`、Native Schema 版本/哈希、规范化请求、上游 task ID、状态、`tid`、开始/结束时间和凭据版本引用。
- **推论**：本地取消只能停止继续轮询或阻止尚未提交的 Attempt；上游一旦提交，没有公开取消能力，不能把本地 `cancelled` 解释成上游未执行或不计费。
- **推论**：同步/异步是 Adapter 的执行策略，不应变成平台两个公开协议。Provider 同步或异步结果都映射到同一 Job 状态机。

## 7. 响应、结果保存与 Webhook

### 7.1 Task 响应

**事实**：媒体任务对象包含 `id`、`object`、`model`、`status`、`output`、`error`、`created_at`、`completed_at`、`expires_at`。输出项包含 `index`、`type`、`content_url`、`b64_json`；失败任务通过 `status=failed` 和 `error` 表达，即使查询 HTTP 状态是 200。[任务对象](https://docs.aihubmix.com/en/api/async-tasks.md#task-object) · [任务错误](https://docs.aihubmix.com/en/api/async-tasks.md#task-errors)

**事实**：图片结果可能通过 URL 或 Base64 返回。URL 下载仍要求创建任务时对应的 Bearer 凭据；模型说明称结果 URL 约 30 分钟失效，而任务对象的 `expires_at` 当前可能仍为 null。[结果下载](https://docs.aihubmix.com/en/api/async-tasks.md#get-task-results) · [模型说明](https://api.inferera.com/model/gpt-image-2/llms.txt)

**推论**：Provider 完成后必须立即把全部结果复制进新服务端自己的对象存储，记录内容哈希、MIME、字节数、宽高和上游 result/index；不能把短期上游 URL 直接当作平台长期结果。

**事实**：仓库已有的 OpenAI 兼容接口响应样本不是 Task 对象，而是 `created/background/data[].b64_json/output_format/quality/size/usage`；样本 HTTP 200、耗时 21.05 秒，`usage` 含文本/图片输入输出 token 分项。[本地实测样本](../../out-reference/aihubmix/gpt_image_2_generations.json)

**推论**：Adapter 必须按 `endpoint_family` 使用不同响应解码器，不能假定 `/ai/v1` 和 `/v1` 返回相同形状。

### 7.1b `/ai/v1` 异步任务的**真实**响应（2026-09-19 实测，用户授权）

此前 7.1 的 Task 对象结构只来自文档。本次以 `POST https://api.inferera.com/ai/v1/images/generations`（`async: true`）**实际发起并轮询**，取得真实样本。

**创建成功（HTTP 200）后立即返回任务对象，`output` 为空、`status: pending`**（task id 与结果 URL 已脱敏；原始样本未入库）：

```json
{"completed_at":null,"created_at":1789804016,"error":null,"expires_at":null,
 "id":"t_<已脱敏>","model":"gpt-image-2","object":"image",
 "output":[],"status":"pending"}
```

**轮询 `GET /ai/v1/images/{id}` 至终态（HTTP 200，约 12 秒完成）**：

```json
{"completed_at":1789804028,"created_at":1789804016,"error":null,"expires_at":1789811227,
 "id":"t_<已脱敏>","model":"gpt-image-2","object":"image",
 "output":[{"b64_json":null,
            "content_url":"https://aihubmix.com/ai/v1/images/<id>/content/res_<已脱敏>",
            "index":0,"type":"file"}],
 "status":"completed"}
```

**由此确认的事实**：

1. **任务对象只有 9 个字段**：`id`、`object`、`model`、`status`、`output`、`error`、`created_at`、`completed_at`、`expires_at`。**其中没有 `usage`**——全文检索 `"usage"` **0 次**（对历史任务列表 `GET /ai/v1/images` 的 3 条已完成任务检索同样为 0 次）。
2. **`output[]` 项**为 `{index, type, content_url, b64_json}`；本次 `b64_json` 为 `null`，结果只给 `content_url`。
3. **状态流转**：受理即 `pending` → 本样本 10 秒内 `completed`。
4. **`quality` 不是 `/ai/v1` 的顶层参数**：顶层传 `quality` 被拒，HTTP 400，`{"error":{"code":"schema_violation","message":"Unknown request parameter: `quality`.","type":"invalid_request_error"}}`；去掉后即受理。⇒ 必须放进 `extra`（与本地 Schema 一致）。
5. **未知参数是硬拒绝**（`schema_violation`），不是静默接受。

**尚未确认（本次未测）**：

- 本次用的是 `gpt-image-2`（已退役），**未对 `gpt-image-2.5-flare`/`-sunburst` 做异步调用**；两者的异步任务对象形状**是否相同未验证**；
- 任务列表/详情里只出现 `gpt-image-2`，说明该账户此前的异步历史也都在旧型号上；
- 异步任务的**权威用量来源仍未知**：任务对象不含 `usage`，`/ai/v1` 也未给出按次金额。

**对第二阶段的影响（仅供 ② 层参考）**：若走 `/ai/v1` 异步，则**拿不到分项 token**，只能得到 `content_url` + 状态；而第一阶段已确认**同步 `/v1` 返回四分项 token 且首期即用它**。因此本仓库现有计费路径（`TokenUsage` 四分项）**只与同步 `/v1` 相容**。同步 `/v1` 亦会保存任务记录（见 §6），`GET /ai/v1/images` 可查到。

### 7.2 Webhook

- **事实**：Webhook 只适用于异步任务，采用至少一次投递；同一 `event_id` 可能重复。`5xx`、网络错误和超时会重试，最多 6 次；`3xx/4xx` 不重试。[Webhook 重试](https://docs.aihubmix.com/en/api/async-tasks.md#webhook-retry)
- **事实**：任务级 Webhook 没有独立签名密钥；第一方建议需要签名时使用账户级订阅，并保留任务详情查询作为结果确认方式。[Webhook 重试](https://docs.aihubmix.com/en/api/async-tasks.md#webhook-retry)
- **推论**：第一期可不依赖任务级 Webhook，使用轮询收敛；若启用，Webhook 只能作为加速信号，收到后仍用 Bearer 凭据查询任务详情确认，按 `event_id` 去重。

## 8. 错误、幂等与重试

### 8.1 错误合同

**事实**：异步媒体 API 的 HTTP 错误形状为：

```json
{
  "error": {
    "message": "...",
    "type": "invalid_request_error",
    "code": "...",
    "tid": "req_..."
  }
}
```

客户端应按 `code` 分类，不应匹配完整 `message`；向支持方报告 5xx 时带上 `tid`。[异步任务错误](https://docs.aihubmix.com/en/api/async-tasks.md#error-codes)

| 类别 | 代表错误 | 处理结论 | 性质与来源 |
| --- | --- | --- | --- |
| 请求/Schema | `invalid_request`、`schema_violation`、`unsupported_input_combination`、`capability_not_supported` | 修正请求，不自动原样重试 | **事实/推论**：[异步任务错误](https://docs.aihubmix.com/en/api/async-tasks.md#error-codes) |
| 媒体 | `unsupported_media_format`、`invalid_media_data`、`media_url_unreachable`、`image_too_large`、`request_too_large` | 记录允许 MIME/大小细节，要求调用方修正 | **事实/推论**：[异步任务错误](https://docs.aihubmix.com/en/api/async-tasks.md#error-codes) |
| 账号 | `authentication_failed`、`insufficient_quota`、`permission_denied`、`async_not_enabled` | Provider/凭据不可用，不重试同一配置 | **事实/推论**：[异步任务错误](https://docs.aihubmix.com/en/api/async-tasks.md#error-codes) |
| 限流 | `rate_limited`、`upstream_rate_limited` | 有界退避；避免立即重复提交 | **事实**：[异步任务错误](https://docs.aihubmix.com/en/api/async-tasks.md#error-codes) |
| 状态不明 | `sync_timeout`、`upstream_bad_response`、`task_status_unavailable`、`result_delivery_failed` | 可能已经执行或已有结果，禁止盲目新建生成 | **事实**：[异步任务错误](https://docs.aihubmix.com/en/api/async-tasks.md#error-codes) |
| 结果被拦截 | `output_blocked`、`output_policy_violation` | 需要修改内容；两者计费语义不同 | **事实**：[异步任务错误](https://docs.aihubmix.com/en/api/async-tasks.md#task-errors) |
| 下载 | `result_not_ready`、`artifact_expired`、`too_many_downloads` | 前者继续轮询；后两者不能通过重新下载保证恢复 | **事实**：[异步任务错误](https://docs.aihubmix.com/en/api/async-tasks.md#error-codes) |

**事实**：通用 HTTP 状态码文档仍使用 `403 insufficient_user_quota`，而新版异步任务文档使用 `402 insufficient_quota`。不同端点族的状态码/错误码不能假定完全一致。[HTTP 状态码](https://docs.aihubmix.com/en/FAQs/HTTP-Codes.md) · [异步任务错误](https://docs.aihubmix.com/en/api/async-tasks.md#error-codes)

### 8.2 幂等和安全重试

- **事实**：已检查的模型 Schema、模型说明和异步任务文档没有声明请求幂等键、幂等 Header 或客户端 correlation ID。[实时模型 Schema](https://aihubmix.com/call/schema/models/gpt-image-2/endpoints) · [模型说明](https://api.inferera.com/model/gpt-image-2/llms.txt) · [异步任务文档](https://docs.aihubmix.com/en/api/async-tasks.md)
- **事实**：创建响应丢失后可以列出图片任务尝试找回 task ID，但文档没有给出把列表项与某个本地 Job 唯一关联的字段。[异步任务文档](https://docs.aihubmix.com/en/api/async-tasks.md#query-task-list)
- **推论**：平台幂等必须由自己的 `Idempotency-Key + 请求哈希 + Job 唯一约束` 保证；这只能防止平台重复派发，不能让一次“发送后失联”的 Provider 创建天然可重放。
- **推论**：Provider 创建调用一旦进入“是否已提交未知”状态，默认标记 Attempt 为 `reconciliation_required`，不要自动再发。只有在能证明请求未到达，或 Provider 后续提供真正的幂等键时，才允许自动重建。
- **推论**：查询和结果下载是可安全重试的读操作；创建生成不是天然安全重试操作。

## 9. 用量与价格证据

### 9.1 当前公开价格

**四档 token 单价**（本渠道按 Tokens 计费；上游**只返回 token、不返回金额**）：

| 计费项 | 公开单价 | 性质与来源 |
| --- | --- | --- |
| 文本输入 | `$5 / 1M tokens` | **事实**：[模型页](https://aihubmix.com/model/gpt-image-2) |
| **文本输出** | **`$10 / 1M tokens`** | **事实**：用户 2026-09-19 确认；`docs/facts/channel-facts.md` §2.4 |
| 图像输入 | `$8 / 1M tokens` | **事实**：[模型页](https://aihubmix.com/model/gpt-image-2) |
| 图像输出 | `$30 / 1M tokens` | **事实**：[模型页](https://aihubmix.com/model/gpt-image-2) |

> **⚠️ 更正（见文首第 1 条）**：本节此前只列三项，并把仓库旧资料里的「文本输出 `$10 / 1M`」判为不予采用。**那是错的**——按"只有三项"发布的生效配置把**文本输出费率写成 0**，会在 `output_text_tokens > 0` 时少收费（缺陷记录：`docs/facts/channel-facts.md` §2.11）。四档都要用。

**事实**：模型说明摘要写成“per-generation，价格见模型页”，但实际模型页展示的是 token-based pricing；应以模型页具体价格表为准。[模型说明](https://api.inferera.com/model/gpt-image-2/llms.txt) · [模型页](https://aihubmix.com/model/gpt-image-2)

### 9.2 本地响应样本

**事实**：仓库样本记录：

```json
{
  "input_tokens": 13,
  "input_tokens_details": {
    "image_tokens": 0,
    "text_tokens": 13
  },
  "output_tokens": 196,
  "output_tokens_details": {
    "image_tokens": 196,
    "text_tokens": 0
  },
  "total_tokens": 209
}
```

来源：[本地实测样本](../../out-reference/aihubmix/gpt_image_2_generations.json)。按公开**四档**单价做**示意计算**：`13×5 + 0×8 + 0×10 + 196×30`（每 1M）`= $0.005945`——本样本没有文本输出，第四档不参与。这只是按公开价格和响应 usage 推导的名义金额，不是账单核对结果。

### 9.3 对计费设计的影响

- **事实**：公开的 `/ai/v1` Task 对象没有 `usage` 字段；OpenAI 兼容本地样本有 `usage`。[任务对象](https://docs.aihubmix.com/en/api/async-tasks.md#task-object) · [本地实测样本](../../out-reference/aihubmix/gpt_image_2_generations.json)
- **待确认**：AIHubMix 是否通过任务详情、响应 Header、账单查询 API 或其他记录提供异步任务的文本/图片 token 分项和最终扣费。
- **待确认**：缓存输入、舍入、最低扣费、失败计费、促销/折扣的权威规则（**缓存不由我们建模**：按 Tokens 计费不区分缓存，见 `docs/facts/channel-facts.md` §3.9.2）。`output_blocked` 明确称不收生成费，但 `output_policy_violation` 可能仍按既有审核计费规则处理。[任务错误](https://docs.aihubmix.com/en/api/async-tasks.md#task-errors)
- **推论**：价格配置必须版本化并保留四种可能的 money rate；结算应优先使用 Provider 权威用量或账单记录，不应用图片尺寸自行反推 token。
- **推论**：若 `/ai/v1` 最终无法提供可审计用量，需在以下方案中做明确选择后才能生产计费：使用可返回 usage 的 `/v1` 端点、接入 AIHubMix 账单记录、或采用经过验证的固定/预估计价并向用户明确其性质。

## 10. 与仓库现有资料的对照

| 主题 | 仓库现有资料 | 最新第一方资料 | 结论 |
| --- | --- | --- | --- |
| Base URL | `https://api.inferera.com/v1` | 当前文档示例写 `https://aihubmix.com`，但 Inferera 域名的任务列表和 Schema 已实测可用 | Channel 默认值使用 `https://api.inferera.com`（不含 `/v1`），具体路径由 Adapter 拼接 |
| 统一接口 | 记录了 `/v1` generations/edits 和一个旧的 predictions 示例 | 当前提供 `/ai/v1/images/generations` 统一接口 | 首版优先 `/ai/v1`，旧 predictions 路径不能视为当前合同 |
| 文生图/图生图 | `/v1` 两个接口 | `/ai/v1` 以 `image/images/mask` 判定，仍保留两个 `/v1` 兼容接口 | 支持一个领域 Command、多个传输协议 |
| 多图 | 示例只上传一张 | `images` 最多 16 项 | Native Schema 应保留数组能力 |
| `quality` | 写有 `auto/low/medium/high` | 实时 Schema 只有 `low/medium/high` | `auto` 待付费测试，不先承诺 |
| `input_fidelity` | 写有 `high/low` | 模型介绍提及，但实时 Schema 不接受 | 参数可用性待确认 |
| `moderation` | 写有 `auto/low` | 实时 Schema 不接受 | 首版不开放 |
| 输出格式 | 旧资料写 PNG/JPEG/WebP | 模型实时 Schema 只有 PNG/JPEG | 模型级约束覆盖通用说明 |
| 结果 | OpenAI 兼容实测是 Base64 + usage | `/ai/v1` 文档是 Task + URL/Base64，未见 usage | Adapter 分开解码；计费证据待确认 |
| 价格 | 多写一项文本输出 `$10/M` | 当前模型页的公开价格句只展示三项 | **⚠️ 已更正：四项都要用**——模型页那句话没列文本输出，不等于该档不存在；用户已确认四档（见 §9.1 与文首更正 1） |

仓库来源：[现有说明](../../out-reference/aihubmix/gpt-image-2.md) · [实测响应](../../out-reference/aihubmix/gpt_image_2_generations.json)。第一方来源：[实时模型 Schema](https://aihubmix.com/call/schema/models/gpt-image-2/endpoints) · [模型页](https://aihubmix.com/model/gpt-image-2) · [异步任务文档](https://docs.aihubmix.com/en/api/async-tasks.md)。

## 11. 对 #674 技术设计的具体影响

### 11.1 Adapter

1. **推论**：实现 `AihubmixImageAdapter`，默认调用 `/ai/v1/images/generations`；把 Provider 的同步/异步差异封装在 Attempt 驱动器内。
2. **推论**：至少提供 `submit`、`poll`、`fetch_artifact` 三个动作；暂不声明 `cancel` 能力。
3. **推论**：响应解析按 `/ai/v1` Task 与 `/v1` OpenAI 兼容两种 codec 分开，不用一个宽松 JSON 结构同时猜测。
4. **推论**：把 `tid`、上游 task ID、结果 URL和原始错误详情作为 Evidence 保存；对外错误再映射为平台稳定错误码。
5. **推论**：任务级 Webhook 默认不开启；以后启用时仅作唤醒信号，必须查询上游详情确认。

### 11.2 Native Capability Schema

1. **推论**：模型身份保持原生 `gpt-image-2`，字段命名保持 `image/images/mask/n/size/output_format/extra`，不映射成跨厂商统一质量/尺寸枚举。
2. **推论**：保存获取时间、上游 `schema_version`、内容哈希和原始 Schema；发布模型版本时固定一个已评审快照，运行中不要无审查地跟随上游即时变化。
3. **推论**：在 Schema 快照之上只允许有证据的本地修订层，用于标记上游 Schema 自相矛盾或付费测试结果；每个修订都要记录来源和日期。
4. **推论**：未知字段默认拒绝。`extra` 是 Provider 明确给出的长尾参数命名空间，不等于允许任意 JSON。
5. **推论**：首版明确支持无图、单图、多图和遮罩四组合同；`mask` 的 object 形式、`quality=auto`、`input_fidelity`、`moderation` 暂不进入已承诺能力。

### 11.3 Job、Attempt 与资产

1. **推论**：平台 Job 状态与 Provider Task 状态分层保存；平台可有 `accepted/dispatching/running/succeeded/failed/cancel_requested/cancelled/reconciliation_required`，Adapter 再把上游五种状态映射进来。
2. **推论**：在 Provider 没有幂等键时，一个 Attempt 只能由一个 Worker 提交一次；模糊失败不能由通用重试器重新创建。
3. **推论**：输入图片先进入平台 Asset，再由 Adapter 转成 URL/Data URI/Base64；输出在任务完成后立即归档到平台 Asset。
4. **推论**：在飞任务必须固定凭据版本引用。统一 `/ai/v1/tasks` 甚至要求使用创建任务的同一 API Key 读取，因此轮询/下载不能随意切换新密钥。[统一任务隔离](https://docs.aihubmix.com/en/api/async-tasks.md#unified-tasks)

### 11.4 计费

1. **推论**：Price Version 记录公开单价与来源时间，Usage Evidence 和 Money Charge 分开保存。
2. **推论**：预授权可按保守上限冻结，但最终结算不能在缺少权威 usage 的情况下假装精确。
3. **推论**：Provider 返回的分项 usage、Provider 账单记录和平台最终扣款都要可追溯到同一 Attempt。
4. **阻塞项**：未确认 `/ai/v1` 异步用量来源前，AIHubMix 可用于 Adapter/Job 开发和受控测试，但不应宣布生产计费闭环完成。

## 12. 首个 Provider 验证清单（历史过程记录）

> 本节是**验证前的清单**。其中绝大部分已由 §13 的受控实测完成；§13 取代与本节相冲突的"待确认"。保留本节只为记录当时的判断依据。

以下项目需要使用测试账户和真实付费请求完成；本文没有代替它们：

1. **待确认**：账户已开通异步任务；`gpt-image-2 + async:true` 能创建、轮询并下载结果。
2. **待确认**：纯文生图、单图编辑、多图参考、mask 编辑各成功一次，并记录实际 wire 请求/响应。
3. **待确认**：`quality=auto`、`input_fidelity`、`moderation`、`response_format`、WebP 和 mask object 的真实行为。
4. **待确认**：输入图片支持的 MIME、单文件大小、像素、总请求大小和多图总量；当前只有多图最多 16 和总 HTTP body 32 MiB 是已公开约束。[实时模型 Schema](https://aihubmix.com/call/schema/models/gpt-image-2/endpoints) · [异步任务错误](https://docs.aihubmix.com/en/api/async-tasks.md#error-codes)
5. **待确认**：`/ai/v1` 在同步和异步模式下是否返回 usage，或如何从账单记录关联到 task ID。
6. **待确认**：同一请求在客户端断连、创建超时和响应丢失时的可恢复性；是否有未公开的幂等 Header 或 correlation 字段。
7. **待确认**：结果真实保留时间、下载次数限制、Content-Type 与 C2PA/元数据保留行为。
8. **已确认配置选择**：默认使用 `https://api.inferera.com`；任务列表和 Schema 已只读验证。付费 POST 未在该域名重复测试，作为实现期首个受控集成测试验证。
9. **待确认**：公开价格与实际账单逐项对账，包括失败、审核拦截、缓存 token、舍入和折扣。

通过上述清单后，再把验证证据固化进 AIHubMix Adapter fixture、Native Schema 版本和计费合同。

## 13. 受控实测结果（2026-09-18）

本节使用已开通异步任务能力的测试账户完成真实付费调用。测试没有记录 API Key、上游 task ID 或短期结果 URL；只保留协议字段、状态、计量值和内容摘要。

### 13.1 `/ai/v1` 统一异步接口

| 场景 | 创建与终态 | 输出 | `usage` |
| --- | --- | --- | --- |
| 纯文生图，`quality=low`、`1024x1024` | HTTP 200；`pending → completed` | 1 个受保护 PNG URL；下载 216,063 bytes | 创建和详情均无 |
| 单图输入，Data URI、`quality=low`、`size=auto` | HTTP 200；`pending → in_progress → completed` | 1 个受保护 PNG URL；下载 2,717,520 bytes | 创建和详情均无 |
| 图片 + PNG alpha mask，`quality=low`、`1024x1024` | HTTP 200；`pending → in_progress → completed` | 1 个受保护 PNG URL；下载 1,021,847 bytes | 创建和详情均无 |

结论：统一接口已经真实证明同一路径可以覆盖文生图、图生图和 mask，不需要把平台领域协议拆开；string/Data URI 形式的 `mask` 可用。它适合上游异步执行和任务恢复，但当前响应不能提供 token 结算证据。

任务列表 `GET /ai/v1/images?limit=20&order=desc` 返回 3 条上述异步任务。列表项只有 `id/object/model/status/output/error/created_at/completed_at/expires_at`，没有 prompt、调用方 metadata、correlation ID 或 usage。因而创建响应丢失时只能按账号、模型和时间窗口缩小范围，不能在并发请求下可靠地自动关联到某个本地 Job。

### 13.2 OpenAI 兼容同步接口

| 场景 | 结果 | usage |
| --- | --- | --- |
| `POST /v1/images/generations` 文生图 | HTTP 200，13.3 秒；返回 `data[0].b64_json` | 文本输入 24、图片输入 0、图片输出 196、总计 220 tokens |
| `POST /v1/images/edits` 图片 + mask | HTTP 200，23.3 秒；返回 `data[0].b64_json` | 文本输入 27、图片输入 1024、图片输出 196、总计 1247 tokens |

两条响应的顶层字段均包含 `background/created/data/output_format/quality/size/usage`；`usage` 明确区分文本输入、图片输入和图片输出，可以直接形成强类型 `MeteringEvidence`。两次调用均未出现在 `/ai/v1/images` 任务列表中。

按模型页当前公开单价计算，文生图样本的名义成本为 `$0.006000`，图片编辑样本为 `$0.014207`。这是“响应 usage × 公开单价”的计算验证，不代表已经与 AIHubMix 最终账单完成逐笔核对。

### 13.3 对首期 Adapter 决策的修正

实测后，首期生产路径建议从“统一 `/ai/v1` 异步接口优先”修正为：

1. 平台应用层仍只有一个 `CreateImageGeneration` 和持久 Job；平台对调用方始终异步。
2. AIHubMix Adapter 根据已验证分支，在内部选择 `/v1/images/generations` 或 `/v1/images/edits`；这正是 Adapter 的 wire 职责，不回流成两个领域协议。
3. Worker 可以等待 Provider 同步响应，同时维护数据库 lease/heartbeat；Provider 同步不等于客户端同步。
4. 选择 `/v1` 的理由是其成功响应提供了可审计 token usage，能够完成 Price Snapshot + Metering Evidence 结算。
5. 代价是 `/v1` 响应丢失后没有可查询的 Provider task，也没有公开幂等键。Attempt 必须进入 `reconciliation_required`，不得自动重提或切换 Offering；后续通过人工账单/支持渠道处理已有成本和用户退款。
6. `/ai/v1` 保留为 Adapter 的已验证能力，但在 AIHubMix 提供可关联 usage/账单证据前，不发布为正式计费 Offering 的执行路径。

这项修正不改变“文生图和图生图同阶段、同 Command/Job”的设计，反而用真实证据确认了 endpoint 差异只应存在于 Adapter 内部。

### 13.4 Base URL 只读验证

用户确认 `https://api.inferera.com/` 可以替换文档示例域名。2026-09-18 使用同一测试账户完成无费用验证：

- `GET https://api.inferera.com/ai/v1/images?limit=1&order=desc` 返回 HTTP 200，可读取已有 `gpt-image-2` 任务；
- `GET https://api.inferera.com/call/schema/models/gpt-image-2/endpoints` 返回 HTTP 200，包含 `/ai/v1/images/generations`、`/v1/images/generations`、`/v1/images/edits` 三条路径。

因此首期 Channel 默认 Base URL 改为 `https://api.inferera.com`。该值仍是运行时配置，不进入 Adapter 常量；正式实现第一次集成测试需从该域名执行 Provider POST，确认写路径与已验证的只读路径一致。
