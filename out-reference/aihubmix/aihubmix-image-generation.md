> ## Documentation Index
> Fetch the complete documentation index at: https://docs.aihubmix.com/llms.txt
> Use this file to discover all available pages before exploring further.

> 优选baseurl
> 如果当地网络连接失败时，建议切换至优选baseurl。https://api.inferera.com

> 价格详情（实际以结果cost字段值为准）：按 Tokens 计费：文本输入 $5 / 1M tokens ｜ 文本输出 $10 / 1M tokens ｜ 图像输入 $8 / 1M tokens ｜ 图像输出 $30 / 1M tokens

# 图片生成

> 使用 AIHubMix 原生图片协议同步或异步生成图片，并查询任务和下载结果。

AIHubMix 原生图片协议使用 `/ai/v1/images` 系列端点。生成接口默认同步，传入布尔值
`async: true` 后在后台生成，并使用同一套任务状态、Webhook 和错误结构。

<Warning>
  使用异步图片、任务查询或 Webhook 前，需要为当前账户开启异步任务功能。未开启时，
  异步任务创建请求返回 `403 async_not_enabled`。
</Warning>

## 快速开始

下面使用 `qwen-image-2.0` 异步生成一张图片。

<CodeGroup>
  ```bash 创建任务 theme={null}
  curl -X POST https://aihubmix.com/ai/v1/images/generations \
    -H "Authorization: Bearer $AIHUBMIX_API_KEY" \
    -H "Content-Type: application/json" \
    -d '{
      "model": "qwen-image-2.0",
      "prompt": "A flower shop with delicate windows, warm sunlight streaming in",
      "n": 1,
      "size": "1024x1024",
      "async": true
    }'
  ```

  ```json 创建响应 theme={null}
  {
    "id": "task_01K0ABCDEF",
    "object": "image",
    "model": "qwen-image-2.0",
    "status": "in_progress",
    "output": [],
    "error": null,
    "created_at": 1784707200,
    "completed_at": null,
    "expires_at": null
  }
  ```

  ```bash 查询任务 theme={null}
  curl https://aihubmix.com/ai/v1/images/{id} \
    -H "Authorization: Bearer $AIHUBMIX_API_KEY"
  ```

  ```bash 下载图片 theme={null}
  curl "{content_url}" \
    -H "Authorization: Bearer $AIHUBMIX_API_KEY" \
    --output result.png
  ```
</CodeGroup>

任务进入 `completed` 后，逐项读取 `output[].content_url`。该地址对应
`GET /ai/v1/images/{id}/content/{result_id}`，客户端不需要自行拼接 `result_id`。

## 接口概览

| 场景 | 方法 | 路径 | 说明 |
| - | - | - | - |
| 生成图片 | POST | `/ai/v1/images/generations` | 默认同步，`async: true` 时异步 |
| 查询图片详情 | GET | `/ai/v1/images/{id}` | 返回任务最新状态 |
| 查询图片列表 | GET | `/ai/v1/images` | 返回当前账户创建的图片任务快照 |
| 下载指定图片 | GET | `/ai/v1/images/{id}/content/{result_id}` | 下载多图任务中的指定结果 |

Base URL：`https://aihubmix.com`

认证方式：

```text theme={null}
Authorization: Bearer $AIHUBMIX_API_KEY
```

## 查询模型 Schema

模型目录可以筛选已经提供请求 Schema 的文生图模型：

```bash theme={null}
curl "https://aihubmix.com/api/v1/models?type=image_generation&schema_checked=true&sort_by=order"
```

取得 `model_id` 后，查询该模型实际支持的端点：

```bash theme={null}
curl "https://aihubmix.com/call/schema/models/qwen-image-2.0/endpoints"
```

同一模型可能同时返回 AIHubMix 原生与兼容端点。应按 `path` 选择原生图片接口，再读取
对应的 `request.schema`：

```bash theme={null}
curl -s "https://aihubmix.com/call/schema/models/qwen-image-2.0/endpoints" \
  | jq '.endpoints[] | select(.path == "/ai/v1/images/generations") | .request.schema'
```

不要依赖 `endpoints` 数组位置。完整响应字段和失败情况参阅
[模型 Schema 接口](/cn/api/async-tasks#model-schema)。

## 创建图片

### 同步生成

省略 `async` 或设置为 `false` 时，请求等待生成完成后返回任务对象：

```bash theme={null}
curl -X POST https://aihubmix.com/ai/v1/images/generations \
  -H "Authorization: Bearer $AIHUBMIX_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "qwen-image-2.0",
    "prompt": "A quiet reading room in the afternoon",
    "n": 1,
    "size": "1024x1024",
    "response_format": "url"
  }'
```

同步请求也会保存任务记录。创建响应丢失时，可以通过图片列表找回任务。

### 异步生成

`async` 必须是布尔值 `true`，不能写成字符串。异步请求立即返回任务对象，生成在后台继续。
`webhook_url` 和 `webhook_events_filter` 只能与 `async: true` 一起使用。

### 标准字段

| 字段 | 类型 | 必填 | 说明 |
| - | - | - | - |
| `model` | string | 是 | 模型 ID |
| `prompt` | string | 是 | 图片描述，不能为空 |
| `n` | integer/null | 否 | 图片数量，最小值 `1`，默认 `1` |
| `size` | string/null | 否 | `{width}x{height}`，例如 `1024x1024` |
| `aspect_ratio` | string/null | 否 | 宽高比，与 `size` 二选一 |
| `seed` | integer/null | 否 | 随机种子 |
| `negative_prompt` | string/null | 否 | 负向提示词 |
| `image` | string/object | 否 | 单张输入图片 |
| `images` | array/null | 否 | 多张输入图片 |
| `mask` | string/object | 否 | 图片编辑蒙版 |
| `output_format` | string/null | 否 | `png`、`jpeg` 或 `webp` |
| `response_format` | string/null | 否 | `url` 或 `b64_json` |
| `async` | boolean | 否 | `true` 时异步执行 |
| `webhook_url` | string | 否 | HTTPS 回调地址，最长 512 字符 |
| `webhook_events_filter` | string\[] | 否 | `completed`、`failed`、`cancelled` 的非空子集 |
| `extra` | object/null | 否 | 模型专属扩展参数 |

<Warning>
  标准字段集合不表示所有模型支持全部字段。字段、枚举和取值范围以该模型
  `/ai/v1/images/generations` 端点的 `request.schema` 为准。
</Warning>

## 图片任务对象

同步生成完成后，或异步任务进入结束态后，接口返回以下结构：

```json theme={null}
{
  "id": "task_01K0ABCDEF",
  "object": "image",
  "model": "qwen-image-2.0",
  "status": "completed",
  "output": [
    {
      "index": 0,
      "type": "file",
      "b64_json": null,
      "content_url": "https://aihubmix.com/ai/v1/images/task_01K0ABCDEF/content/result_01K0XYZ"
    }
  ],
  "error": null,
  "created_at": 1784707200,
  "completed_at": 1784707218,
  "expires_at": 1784714418
}
```

| 字段 | 类型 | 说明 |
| - | - | - |
| `id` | string | 平台任务 ID |
| `object` | string | 图片任务固定为 `image` |
| `model` | string | 实际使用的模型 ID |
| `status` | string | 当前任务状态 |
| `output` | array | 已生成的图片；尚无结果时为空数组 |
| `error` | object/null | 失败信息，可含 `code`、`message` 和 `upstream_detail` |
| `created_at` | integer | 创建时间，Unix 秒 |
| `completed_at` | integer/null | 进入结束态的时间，Unix 秒 |
| `expires_at` | integer/null | 结果过期时间，Unix 秒；任务完成前可能为空 |

`output` 项包含顺序 `index`、固定值 `type: "file"`、可选的 `b64_json` 和
`content_url`。多图请求会按 `index` 返回多个结果。

### 状态说明

| 状态 | 是否结束 | 说明 |
| - | - | - |
| `pending` | 否 | 已接收，等待执行 |
| `in_progress` | 否 | 正在生成 |
| `completed` | 是 | 已完成，可读取 `output` |
| `failed` | 是 | 已失败，原因见 `error` |
| `cancelled` | 是 | 已取消 |

客户端可以每 15 秒查询一次，直到状态变为 `completed`、`failed` 或 `cancelled`。
15 秒是客户端轮询建议，不是服务端协议限制。

## 查询图片任务

### 查询详情

```bash theme={null}
curl https://aihubmix.com/ai/v1/images/{task_id} \
  -H "Authorization: Bearer $AIHUBMIX_API_KEY"
```

图片详情接口会返回任务最新状态。异步图片轮询必须使用该接口，不要使用统一任务详情代替。

### 查询列表

创建响应丢失时，可以通过图片列表找回任务 ID：

```bash theme={null}
curl "https://aihubmix.com/ai/v1/images?limit=20&order=desc" \
  -H "Authorization: Bearer $AIHUBMIX_API_KEY"
```

| 参数 | 类型 | 默认值 | 说明 |
| - | - | - | - |
| `after` | string | - | 分页游标，使用上一页的 `next_after` |
| `limit` | integer | `20` | 每页数量，最大 `100` |
| `order` | string | `desc` | `asc` 为升序，其他值按 `desc` 处理 |

```json theme={null}
{
  "object": "list",
  "data": [
    {
      "id": "task_01K0ABCDEF",
      "object": "image",
      "model": "qwen-image-2.0",
      "status": "in_progress",
      "output": [],
      "error": null,
      "created_at": 1784707200,
      "completed_at": null,
      "expires_at": null
    }
  ],
  "has_more": true,
  "next_after": "task_01K0ABCDEF"
}
```

列表返回查询时的任务快照，不会主动刷新活动任务状态。

## 统一任务接口

`/ai/v1/tasks` 提供图片、视频和 LLM 任务的统一只读视图。可以只查询图片任务：

```bash theme={null}
curl "https://aihubmix.com/ai/v1/tasks?object=image&status=completed&limit=20&order=desc" \
  -H "Authorization: Bearer $AIHUBMIX_API_KEY"
```

统一任务列表支持 `object`、`status`、`model`、`after`、`limit` 和 `order`。统一任务详情：

```bash theme={null}
curl https://aihubmix.com/ai/v1/tasks/{task_id} \
  -H "Authorization: Bearer $AIHUBMIX_API_KEY"
```

统一任务中的图片结果会额外提供 `result_id` 和 `content_type`：

```json theme={null}
{
  "index": 0,
  "result_id": "result_01K0XYZ",
  "type": "file",
  "content_type": "image/png",
  "content_url": "https://aihubmix.com/ai/v1/tasks/task_01K0ABCDEF/content/result_01K0XYZ"
}
```

多结果统一任务通过 `/ai/v1/tasks/{id}/content/{result_id}` 下载。未指定结果 ID 时返回
`400 result_id_required`。

<Note>
  媒体详情接口可能在查询时更新活动任务状态，统一任务接口只返回当前快照。因此轮询使用
  `/ai/v1/images/{id}`；统一筛选和读取结果元数据时使用 `/ai/v1/tasks`。
  任务及内容按创建任务时的 Bearer Token 隔离。
</Note>

## 下载图片结果

任务进入 `completed` 后，优先直接使用媒体任务对象返回的结果字段：

* `output[].b64_json` 非空：直接进行 Base64 解码。
* `output[].content_url` 非空：携带创建任务时的 Bearer Token 请求该地址。

```bash theme={null}
curl "{content_url}" \
  -H "Authorization: Bearer $AIHUBMIX_API_KEY" \
  --output result.png
```

图片媒体下载路径是 `/ai/v1/images/{id}/content/{result_id}`。媒体任务对象不单独公开
`result_id`，客户端不需要自行解析或拼接，逐项使用 `output[].content_url` 即可。

<Warning>
  结果可能过期，也可能存在下载次数限制。过期返回 `410 artifact_expired`；超过下载
  次数限制返回 `429 too_many_downloads`。客户端应在任务完成后及时保存结果。
</Warning>

## Webhook

Webhook 仅用于 `async: true` 的图片请求：

```bash theme={null}
curl -X POST https://aihubmix.com/ai/v1/images/generations \
  -H "Authorization: Bearer $AIHUBMIX_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "qwen-image-2.0",
    "prompt": "A quiet reading room in the afternoon",
    "n": 1,
    "size": "1024x1024",
    "async": true,
    "webhook_url": "https://example.com/webhooks/aihubmix",
    "webhook_events_filter": ["completed", "failed"]
  }'
```

`webhook_url` 最长 512 字符，不能指向本机、私网或其他受限地址。省略事件过滤器时，
平台推送 `completed`、`failed` 和 `cancelled`；显式传入时，数组不能为空、不能重复，
并且必须与 `webhook_url` 一起使用。

### 回调请求

```json theme={null}
{
  "event_id": "evt_01K0ABCDEF",
  "event_type": "completed",
  "created_at": "2026-08-12T12:00:00Z",
  "data": {
    "task_id": "task_01K0ABCDEF",
    "status": "completed",
    "model": "qwen-image-2.0",
    "results": [
      {
        "url": "https://aihubmix.com/ai/v1/tasks/task_01K0ABCDEF/content/result_01K0XYZ"
      }
    ]
  }
}
```

`event_id` 用于去重；`data.error` 在失败时可能出现；`data.results` 只在结果已存档时出现，
下载仍需 Bearer Token。

### 重试与去重

平台采用至少一次投递，同一事件可能重复送达：

* HTTP `2xx` 表示接收成功。
* HTTP `5xx`、网络错误或超时会触发重试。
* HTTP `3xx` 和 `4xx` 不会重试。
* 最多投递 6 次，重试间隔依次为 1、4、16、64、256 秒。

接收端应保存 `event_id`，重复收到相同事件时直接返回 `2xx`。任务级 Webhook 本身不
携带独立签名密钥；需要签名验证时，请配置账户级 Webhook，并保留详情查询作为结果确认方式。

<h2 id="error-codes">
  错误响应与错误码
</h2>

本节适用于 `/ai/v1/images/*` 和图片任务。视频错误见 [视频接口](/cn/api/aihubmix-video-generation#error-codes)。

* **当前请求失败**：HTTP 非 2xx，表示本次创建、查询或下载请求失败，见 [HTTP 请求失败](#http-errors)。
* **任务执行失败**：查询返回 HTTP 200，但任务的 `status=failed`，原因记录在任务内的 `error`，见 [任务执行失败](#task-errors)。
* **列表单条结果读取失败**：列表返回 HTTP 200，但某条任务带有 `output_error`，见 [列表单条结果读取失败](#output-read-errors)。

下表 `message` 列列出英文返回文案，说明列解释含义及处理方式。参数校验错误列出通用文案，实际响应可能进一步指出具体字段和约束。客户端应使用 `code` 判断错误类型，不应依赖完整 `message` 匹配。

```json theme={null}
{
  "error": {
    "message": "The task was not found.",
    "type": "invalid_request_error",
    "code": "task_not_found",
    "tid": "req_01K0ABCDEF"
  }
}
```

<Card title="提交 HTTP 5xx 错误反馈" icon="bug" href="https://console.aihubmix.com/support" horizontal>
  请求返回 HTTP `5xx` 时，请提交反馈并附上 `error.tid`。
</Card>

<h3 id="http-errors">
  HTTP 请求失败
</h3>

下表中的 HTTP 状态用于当前请求直接失败的情况。已创建任务的执行失败见下方“任务执行失败”表。

<h4 id="input-errors">
  请求参数与媒体输入
</h4>

<div className="media-error-table media-error-http">
  | HTTP | `code` | `message` | 说明 |
  | - | - | - | - |
  | 400 | `invalid_request` | `Invalid request. Check the request body and parameters.` | 请求体、查询参数或参数类型不正确。按错误信息检查请求格式、参数类型和取值。 |
  | 404 | `model_not_found` | `The requested model was not found. Check the model name.` | 请求的模型不存在。检查模型名称。 |
  | 400 | `schema_violation` | `One or more model parameters are invalid. Check the model schema and request values.` | 参数不符合所选模型的输入要求。查阅该模型的 schema，修正错误信息指出的字段。 |
  | 400 | `unsupported_input_combination` | `This model does not support this operation or media input combination. Change the inputs or choose another model.` | 当前模型不支持该操作与媒体输入组合。更换输入组合或选择支持该组合的模型。 |
  | 400 | `capability_not_supported` | `One or more requested parameters are not supported by this model. Change the parameters or choose another model.` | 请求参数超出模型能力范围。按错误信息调整参数，或选择其他模型。 |
  | 400 | `unsupported_media_format` | `The {media_kind} format is not supported for this model.` | 该模型不支持当前媒体格式。使用错误信息或 `error.details.allowed_mime_types` 中列出的格式；未提供列表时，查阅该模型的输入要求。 |
  | 400 | `invalid_media_data` | `The media data is invalid. Provide valid media data.` | 已确认媒体数据无效，例如 base64 损坏或 data URI 结构错误。检查数据完整性和编码，重新提供有效媒体。 |
  | 400 | `media_url_unreachable` | `The media URL could not be accessed. Make sure it is publicly accessible and returns valid media data.` | 媒体链接无法访问或读取。检查链接是否有效、是否允许公网访问，以及是否返回有效媒体内容。 |
  | 400 | `invalid_media` | `The media input could not be processed. Check that its data or URL is valid and uses a supported format.` | 媒体输入无法处理，当前无法确认更具体原因。检查媒体数据或链接是否有效，以及格式是否受模型支持。 |
  | 413 | `image_too_large` | `The image is too large. Reduce the image size to {max_bytes} bytes or less and try again.` | 图片任务的媒体内容超过当前处理上限。按 `error.details.max_bytes` 或错误信息中的上限缩小媒体后再试。 |
  | 413 | `request_too_large` | `The request body exceeds the 32 MiB limit. Reduce the request size and try again.` | 整个 HTTP 请求体超过 32 MiB。缩小实际发送的请求体，包括文本、参数和内联媒体编码。 |
</div>

**大小限制**

* **媒体大小**：图片任务超限返回 `image_too_large`，具体上限见错误信息或 `error.details.max_bytes`。
* **整个请求大小**：`request_too_large` 表示 HTTP 请求体超过 **32 MiB**，包括文本、参数和内联媒体编码。只传 URL 时，链接本身计入请求体，链接指向的文件仍需满足模型的媒体限制。
* **实际大小**：`error.details.actual_bytes` 仅在完整大小已确认时提供。通过 URL 读取媒体时，若达到读取上限后停止，可能不返回该字段。

**支持格式**

以所选模型为准。先查看 `error.details.allowed_mime_types` 或错误信息中的格式列表；未提供列表时，查阅该模型的 Schema。

**文案中的占位符**

* `{media_kind}`：实际媒体类型。类型已确认时，`invalid_media_data` 和 `media_url_unreachable` 的文案也会使用 `image` 或 `video`。
* `{max_bytes}`：字节上限。图片上限未知时，返回 `The image is too large. Reduce the image size and try again.`
* `{allowed_formats}`：允许的格式列表。格式错误文案可能追加 `Use one of: {allowed_formats}.`

<h4 id="generation-errors">
  生成请求与返回结果
</h4>

<div className="media-error-table media-error-http">
  | HTTP | `code` | `message` | 说明 |
  | - | - | - | - |
  | 502 | `image_not_generated` | `The model provider completed the request but did not return an image. Adjust the request or choose another model, then try again. If the problem persists, contact support with the request ID.` | 模型推理厂商已完成请求，但没有返回可用图片。调整请求或选择其他模型后再试；持续失败时，提供请求 ID 联系支持。不要仅因 HTTP 502 就无条件自动重复生成。 |
  | 422 | `output_blocked` | `The model provider blocked the generated output. Change the prompt or input content and try again.` | 模型推理厂商拦截了生成内容，且未返回可用图片。请修改提示词或输入内容后再试。本次不收取生成费用。 |
  | 400 | `upstream_rejected` | `The model provider rejected the request. Check the request parameters and content.` | 模型推理厂商拒绝了请求，但仅凭此码不能确定是内容违规。检查请求参数和输入内容，修改后再试。 |
  | 502 | `upstream_bad_response` | `The model provider response could not be processed. Contact support with the request ID.` | 模型推理厂商已返回响应，但平台无法处理；不表示已确认没有生成结果。提供请求 ID 联系支持，不要把它当成参数错误或空结果自行判断。 |
  | 400 | `doubao_real_person_required` | `This request uses a real-person image. Complete real-person verification and use an active asset reference. See https://docs.aihubmix.com/cn/api/doubao-real-person-assets.` | 豆包返回了特定的真人图片隐私拒绝码。先完成真人认证，再使用已激活的 `asset://` 素材；详见 [真人素材文档](https://docs.aihubmix.com/cn/api/doubao-real-person-assets)。 |
  | 504 | `sync_timeout` | `Generation did not complete within the waiting period. Contact support with the request ID.` | 生成未能在等待时间内完成；超时不证明请求没有执行。提供请求 ID 联系支持；不要查询未向你返回的任务 ID。 |
  | 502 | `task_status_unavailable` | `The model provider task state could not be confirmed. Do not submit it again. Contact support with the task ID or request ID.` | 任务已提交给模型推理厂商，后续状态无法确认。请勿重复提交；提供请求 ID 联系支持，有任务 ID 时一并提供。 |
</div>

<h4 id="account-errors">
  账户与权限
</h4>

<div className="media-error-table media-error-http">
  | HTTP | `code` | `message` | 说明 |
  | - | - | - | - |
  | 401 | `authentication_failed` | `Authentication failed: invalid or missing API key.` | API Key 缺失或无效。检查 `Authorization` 请求头和 API Key。 |
  | 402 | `insufficient_quota` | `Your account has insufficient quota. Add credits and try again.` | 账户额度不足。补充账户额度后再试。 |
  | 403 | `permission_denied` | `You do not have permission to access this resource. Check the account, API key permissions, and IP restrictions.` | 账户或 API Key 无权访问。检查账户权限、API Key 权限和 IP 限制。 |
  | 403 | `async_not_enabled` | `Asynchronous tasks are not enabled. Please go to https://console.aihubmix.com/async-tasks to activate.` | 账户未开启异步任务功能。按响应中的控制台链接开通后再试。 |
</div>

<h4 id="service-errors">
  服务可用性与限流
</h4>

<div className="media-error-table media-error-http">
  | HTTP | `code` | `message` | 说明 |
  | - | - | - | - |
  | 429 | `rate_limited` | `Too many requests. Reduce the request rate and try again later.` | 请求过于频繁。降低请求频率后再试。 |
  | 429 | `upstream_rate_limited` | `Upstream service is rate limiting. Please retry later.` | 模型推理厂商限流。稍后再试，避免立即反复提交。 |
  | 500 | `internal_error` | `An internal error occurred. Contact support with the request ID.` | 平台发生内部错误。提供请求 ID 联系支持，不需要反复修改正常输入。 |
  | 502 | `upstream_unreachable` | `Upstream service is temporarily unavailable. Please retry.` | 模型推理厂商暂时不可用，可能是连接失败、厂商服务错误或响应读取失败。仅凭此码无法确认是否已产生结果；持续失败时，提供请求 ID 联系支持。 |
  | 502 | `provider_unavailable` | `The model provider is unavailable. Contact support with the request or task ID.` | 模型推理厂商不可用。表中为默认文案，实际响应可能提供更具体的提示；错误类型以 `code` 为准。提供请求 ID 联系支持。 |
  | 503 | `model_unavailable` | `The requested model is temporarily unavailable. Try again later or choose another model. If the problem persists, contact support with the request ID.` | 当前模型暂时不可用。稍后再试或选择其他模型；持续失败时，提供请求 ID 联系支持。 |
  | 503 | `service_unavailable` | `The service is temporarily unavailable. Try again later. If the problem persists, contact support with the request ID.` | 当前服务暂时不可用。稍后重试当前操作；下载失败时重试下载，不必重新生成。 |
</div>

`provider_unavailable` 表示已明确识别的模型推理厂商故障。仅凭普通 429 或 4xx 无法确认账户额度、内容审核或参数问题。

<h4 id="query-errors">
  任务查询与结果下载
</h4>

<div className="media-error-table media-error-http">
  | HTTP | `code` | `message` | 说明 |
  | - | - | - | - |
  | 404 | `task_not_found` | `The task was not found.` | 任务不存在，或不属于当前账户。请检查任务 ID，并确认 API Key 属于创建任务的账户。 |
  | 404 | `result_not_found` | `The result was not found.` | 结果不存在或无权读取。检查任务 ID、结果 ID 和 API Key；已知无效结果 ID 不会因为任务仍在执行而变成 409。 |
  | 400 | `invalid_cursor` | `The cursor is invalid or expired. Start a new list request.` | 分页游标无效或已失效。去掉 `after`，从第一页重新查询，再使用该列表返回的新游标。 |
  | 400 | `result_id_required` | `This task has multiple results. Specify result_id.` | 任务有多个结果，但未指定 `result_id`。从任务输出中选择结果，并指定对应的 `result_id`。 |
  | 409 | `result_not_ready` | `The result is not available yet. Check the task status before downloading.` | 任务仍在排队或执行，结果尚未准备好。继续查询任务状态，结果就绪后再下载；不需要重新生成。 |
  | 410 | `artifact_expired` | `The result has expired and is no longer available. Submit a new generation request.` | 结果已过保留期。已过期结果无法继续下载，需要时提交新的生成请求。 |
  | 429 | `too_many_downloads` | `Download limit exceeded for this result.` | 下载次数达到限制。检查已保存的结果；单纯等待不保证恢复下载次数。 |
</div>

<h3 id="task-errors">
  任务执行失败
</h3>

任务创建成功后，生成失败通过 `status=failed` 和任务内的 `error` 表达。查询成功仍返回 HTTP 200。

```json theme={null}
{
  "id": "task_01K0ABCDEF",
  "object": "image",
  "status": "failed",
  "output": [],
  "error": {
    "code": "image_not_generated",
    "message": "The model provider completed the request but did not return an image. Adjust the request or choose another model, then submit a new task."
  }
}
```

<div className="media-error-table media-error-task">
  | Task `code` | `message` | 说明 |
  | - | - | - |
  | `image_not_generated` | `The model provider completed the request but did not return an image. Adjust the request or choose another model, then submit a new task.` | 模型推理厂商已完成请求，但没有返回可用图片。调整请求或选择其他模型后提交新任务；不要无条件自动重复生成。 |
  | `output_blocked` | `The model provider blocked the generated output. Change the prompt or input content and try again.` | 模型推理厂商拦截了生成内容，且未返回可用图片。请修改提示词或输入内容后提交新任务。本次不收取生成费用。 |
  | `output_policy_violation` | `Generated content was rejected by the content policy.` | 生成内容未通过审核。修改提示词或输入内容后提交新任务；按现有内容审核收费规则处理，历史费用以计费记录为准。 |
  | `upstream_rejected` | `The model provider rejected the request. Check the request parameters and content.` | 模型推理厂商拒绝了请求，但仅凭此码不能确定是内容违规。检查请求参数和输入内容，修改后提交新任务。 |
  | `upstream_bad_response` | `The model provider response could not be processed. Contact support with the task ID.` | 平台无法处理模型推理厂商返回的响应；不表示已确认没有生成结果。提供任务 ID 联系支持。 |
  | `task_status_unavailable` | `The model provider task state could not be confirmed. Do not submit it again. Contact support with the task ID.` | 任务已提交给模型推理厂商，后续状态无法确认。请勿重复提交；提供任务 ID 联系支持。 |
  | `result_delivery_failed` | `The generated result could not be delivered. Do not resubmit automatically. Contact support with the task ID.` | 已有生成结果，但平台未能完成保存或交付。请勿自动重新生成；提供任务 ID 联系支持，先排查已有结果。 |
  | `upstream_rate_limited` | `Upstream service is rate limiting. Please retry later.` | 模型推理厂商限流。稍后再试，避免立即反复提交。 |
  | `upstream_unreachable` | `Upstream service is temporarily unavailable. Please retry.` | 任务执行期间遇到模型推理厂商连接失败、厂商服务错误或响应读取失败。仅凭此码无法确认是否已产生结果；持续失败时，提供任务 ID 联系支持。 |
  | `provider_unavailable` | `The model provider is unavailable. Contact support with the request or task ID.` | 模型推理厂商不可用。表中为默认文案，实际响应可能提供更具体的提示；错误类型以 `code` 为准。提供任务 ID 联系支持。 |
  | `doubao_real_person_required` | `This request uses a real-person image. Complete real-person verification and use an active asset reference. See https://docs.aihubmix.com/cn/api/doubao-real-person-assets.` | 豆包返回了特定的真人图片隐私拒绝码。先完成真人认证，再使用已激活的 `asset://` 素材；详见 [真人素材文档](https://docs.aihubmix.com/cn/api/doubao-real-person-assets)。 |
  | `sync_timeout` | `Generation timed out. Contact support with the task ID.` | 生成任务超时。提供任务 ID 联系支持。 |
  | `internal_error` | `An internal error occurred. Contact support with the task ID.` | 平台发生内部错误。提供任务 ID 联系支持，不需要反复修改正常输入。 |
</div>

媒体输入错误也可能出现在失败 Task 中，错误码含义与上方媒体输入表一致；此时查询成功的 HTTP 状态仍为 200。 媒体大小错误的 `message` 结尾使用 `submit a new task.`，提示缩小媒体后提交新任务。

`output_blocked` 表示明确拦截且未返回可用图片，本次不收取生成费用。`output_policy_violation` 按现有内容审核收费规则处理。历史费用请以计费记录为准。

<h3 id="output-read-errors">
  列表单条结果读取失败
</h3>

图片、视频任务列表中的个别行可能附带 `output_error`：

```json theme={null}
{
  "output_error": {
    "code": "internal_error",
    "type": "server_error",
    "message": "The task output could not be read. Contact support with the request ID.",
    "tid": "req_01K0ABCDEF"
  }
}
```

该字段表示本次无法读取该行的结果。列表仍返回 HTTP 200，该行 `output=[]`，原 `id`、`status`、`error` 和分页保持不变，其他可正常读取的任务不受影响。即使 `status=completed`，也应检查 `output_error`，再判断结果是否可读取。

请提供 `output_error.tid` 联系支持；该字段未带 `tid` 时，可提供本次响应头中的请求 ID。此错误不改变任务状态或费用，也不触发 Webhook。

图片、视频列表保留已取得的 `expires_at`。统一 `/ai/v1/tasks` 列表的读取失败行可能返回 `expires_at=null`，结果仍受原保留期限制。

上述处理仅适用于单行结果无法读取且没有可用回退的情况。统一 tasks 接口整批查询失败仍返回 HTTP 错误，详情读取同类故障仍返回 HTTP 500。

相关文档：[图片接口](/cn/api/aihubmix-image-generation#error-codes) · [视频接口](/cn/api/aihubmix-video-generation#error-codes) · [异步任务](/cn/api/async-tasks#error-codes)

## 完整示例

以下示例完成异步创建、轮询和多图片保存。

<CodeGroup>
  ```python Python theme={null}
  import base64
  import os
  import time

  import requests

  base_url = "https://aihubmix.com"
  headers = {
      "Authorization": f"Bearer {os.environ['AIHUBMIX_API_KEY']}",
      "Content-Type": "application/json",
  }

  response = requests.post(
      f"{base_url}/ai/v1/images/generations",
      headers=headers,
      json={
          "model": "qwen-image-2.0",
          "prompt": "A flower shop with delicate windows, warm sunlight streaming in",
          "n": 1,
          "size": "1024x1024",
          "async": True,
      },
      timeout=60,
  )
  response.raise_for_status()
  task = response.json()

  while task["status"] not in {"completed", "failed", "cancelled"}:
      time.sleep(15)
      response = requests.get(
          f"{base_url}/ai/v1/images/{task['id']}",
          headers=headers,
          timeout=30,
      )
      response.raise_for_status()
      task = response.json()

  if task["status"] != "completed":
      raise RuntimeError(task.get("error") or task["status"])

  for output in task["output"]:
      filename = f"result-{output['index']}.png"
      if output.get("b64_json"):
          content = base64.b64decode(output["b64_json"])
      else:
          result = requests.get(output["content_url"], headers=headers, timeout=120)
          result.raise_for_status()
          content = result.content
      with open(filename, "wb") as file:
          file.write(content)
  ```

  ```typescript TypeScript theme={null}
  import { writeFile } from "node:fs/promises";

  const baseUrl = "https://aihubmix.com";
  const headers = {
    Authorization: `Bearer ${process.env.AIHUBMIX_API_KEY}`,
    "Content-Type": "application/json",
  };

  const created = await fetch(`${baseUrl}/ai/v1/images/generations`, {
    method: "POST",
    headers,
    body: JSON.stringify({
      model: "qwen-image-2.0",
      prompt: "A flower shop with delicate windows, warm sunlight streaming in",
      n: 1,
      size: "1024x1024",
      async: true,
    }),
  });
  if (!created.ok) throw new Error(await created.text());
  let task = await created.json();

  const finished = new Set(["completed", "failed", "cancelled"]);
  while (!finished.has(task.status)) {
    await new Promise((resolve) => setTimeout(resolve, 15_000));
    const polled = await fetch(`${baseUrl}/ai/v1/images/${task.id}`, { headers });
    if (!polled.ok) throw new Error(await polled.text());
    task = await polled.json();
  }

  if (task.status !== "completed") {
    throw new Error(JSON.stringify(task.error ?? task.status));
  }

  for (const output of task.output) {
    let content;
    if (output.b64_json) {
      content = Buffer.from(output.b64_json, "base64");
    } else {
      const result = await fetch(output.content_url, { headers });
      if (!result.ok) throw new Error(await result.text());
      content = Buffer.from(await result.arrayBuffer());
    }
    await writeFile(`result-${output.index}.png`, content);
  }
  ```
</CodeGroup>

## 常见问题

**图片任务应该查询 `/ai/v1/tasks/{id}` 还是图片详情接口？**

轮询使用 `/ai/v1/images/{id}`；统一筛选任务或读取 `result_id`、`content_type` 时使用
`/ai/v1/tasks`。

**创建响应丢失后如何找回任务？**

请求 `GET /ai/v1/images?limit=20&order=desc`，再用返回的任务 ID 查询图片详情。

**Webhook 没收到怎么办？**

确认回调地址可以公开访问并及时返回 `2xx`，然后使用图片详情接口确认最终状态。

更多跨媒体背景参阅 [异步任务](/cn/api/async-tasks)、
[Webhook 说明](/cn/api/async-tasks#webhooks) 和
[完整错误码](/cn/api/async-tasks#error-codes)。


This documentation is built and hosted on [Mintlify](https://mintlify.com), a developer documentation platform.