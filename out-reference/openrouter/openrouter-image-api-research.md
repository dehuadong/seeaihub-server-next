# OpenRouter 图像 API 第一方协议调研

> 调研日期：2026-09-19（北京时间 11:50–11:55，UTC 03:50–03:55）
> 用途：为第二阶段候选 Provider OpenRouter 提供事实依据，重点回答「可核验计量证据」与「幂等/恢复能力」
> 性质：上游协议研究与只读探测证据，不是平台对外接口合同
> 纪律：本文不记录任何真实 API Key、Bearer token、task id、短期结果 URL。所有鉴权字段一律脱敏为 `<OPENROUTER_API_KEY>` / `sk-or-v1-<REDACTED>`。

---

## 0. 调研范围与证据等级

| 等级 | 来源 | 本文用途 |
| --- | --- | --- |
| A | [Image Generation 指南](https://openrouter.ai/docs/guides/overview/multimodal/image-generation.md)（2026-09-19 抓取） | 专用图像 API 的端点、请求字段、响应形状、计费与取消语义 |
| A | [Generate an image API Reference](https://openrouter.ai/docs/api/api-reference/images/generate-an-image.md)（2026-09-19 抓取） | `/api/v1/images` 的完整 OpenAPI 片段：请求 Schema、响应 Schema、状态码、`usage` 结构 |
| A | [List endpoints for an image model](https://openrouter.ai/docs/api/api-reference/images/list-endpoints-for-an-image-model.md)（2026-09-19 抓取） | 每模型按端点的能力与价格定义；`pricing[].billable/unit/variant` 语义 |
| A | 实时端点 API `GET https://openrouter.ai/api/v1/images/models/openai/gpt-image-2/endpoints`（2026-09-19 11:50 +08:00 实测 HTTP 200） | `openai/gpt-image-2` 的当前供应方、字段集合、单价行 |
| A | 实时模型列表 API `GET https://openrouter.ai/api/v1/images/models`（2026-09-19 11:50 +08:00 实测 HTTP 200，共 52 条） | 模型级能力并集、是否存在多个模型条目 |
| A | [API 概览 / usage 与 /generation](https://openrouter.ai/docs/api-reference/overview.md)（2026-09-19 抓取） | `usage` 字段语义、`cost` 单位、`/api/v1/generation` 查询能力 |
| A | [Get request & usage metadata for a generation](https://openrouter.ai/docs/api/api-reference/generations/get-request-&-usage-metadata-for-a-generation.md)（2026-09-19 抓取） | `/api/v1/generation?id=` 的字段与用途 |
| A | [Errors and Debugging](https://openrouter.ai/docs/guides/overview/errors-and-debugging.md) / 本地快照 `errors-and-debugging.md` | 错误信封、typed error codes、`error_type` 稳定性 |
| A | [Provider Routing](https://openrouter.ai/docs/guides/routing/provider-selection.md)（2026-09-19 抓取） | 默认按价负载均衡、`order`/`only`/`ignore`/`allow_fallbacks`、账号级偏好 |
| A | [Rate Limits 与 In-flight spending budget](https://openrouter.ai/docs/api_reference/limits.md)（2026-09-19 抓取） | 402/429 分类、按 token 预估的预授权、图像价格不参与预估 |
| A | [FAQ](https://openrouter.ai/docs/faq.md)（2026-09-19 抓取） | 充值费率、免费额度、活动页导出的对账能力 |
| A | [Authentication](https://openrouter.ai/docs/api_reference/authentication.md)、[Management API Keys](https://openrouter.ai/docs/guides/overview/auth/management-api-keys.md)、[Get remaining credits](https://openrouter.ai/docs/api/api-reference/credits/get-remaining-credits.md)、[Activity Export](https://openrouter.ai/docs/cookbook/administration/activity-export.md)（均 2026-09-19 抓取） | 鉴权方式、credits/key 查询、账单导出 |
| A | [OpenAPI 规范 https://openrouter.ai/openapi.json](https://openrouter.ai/openapi.json)（2026-09-19 抓取，2.17 MB） | 全量路径与 Schema 的机器可查版本；`/images` 只存在 POST，且无 `Idempotency-Key` 参数 |
| B | 本地旧快照 `openrouter-image.md`、`图像生成.md`、`images-models.md`、`errors-and-debugging.md` | 与第一方现状做差异对照；不作为当前合同 |
| 我的推断 | 只读探测（无付费调用） | 见文中标注「**我的推断**」或「**探测事实**」的条目 |

**探测方法与边界**：本次调研**未做任何付费图像生成调用**。所有「探测事实」均来自 (a) 公开只读 API（`/api/v1/images/models`、`/api/v1/images/models/{author}/{slug}/endpoints`、`/api/v1/models`）与 (b) 携带**无效** API Key 的请求，用于观察参数校验与错误信封；无效 Key 请求不会产生推理费用（返回 400 校验错误或 401）。

本文引用的第一方文档中，`https://openrouter.ai/docs/...md` 形式由文档站直接提供（每个页面由 `llms.txt` 索引，见 [文档索引](https://openrouter.ai/docs/llms.txt)）。

---

## 1. 问题 1：是否提供图像生成 API、文档地址、状态与门槛

- **事实**：OpenRouter 提供**专用图像 API**（不是 chat completions 的附带能力）。文档章节为 [Image Generation](https://openrouter.ai/docs/guides/overview/multimodal/image-generation.md)，API Reference 页面为 [Generate an image](https://openrouter.ai/docs/api/api-reference/images/generate-an-image.md)。抓取时间 2026-09-19。
- **事实**：模型浏览入口为 `https://openrouter.ai/models?output_modalities=image`；模型发现走 `GET /api/v1/images/models`，端点明细走 `GET /api/v1/images/models/{author}/{slug}/endpoints`。[Image Generation 指南](https://openrouter.ai/docs/guides/overview/multimodal/image-generation.md)
- **事实**：文档没有任何「beta」「preview」「waitlist」「需要申请开通」字样；OpenAPI `info.version` 为 `1.0.0`，`/images` 是正式路径。图像 API 与 chat API 共用同一套 API Key 鉴权。[OpenAPI 规范](https://openrouter.ai/openapi.json) · [Authentication](https://openrouter.ai/docs/api_reference/authentication.md)
- **事实**：门槛是**账户余额**，不是申请制。余额不足时请求直接返回 `402`（[Rate Limits](https://openrouter.ai/docs/api_reference/limits.md)）。FAQ 称新用户会获得「a small free allowance」，但未给出该额度的具体数值与是否可用于付费模型。[FAQ](https://openrouter.ai/docs/faq.md)
- **我的推断**：本仓库只需 Channel 级配置（Base URL + Bearer Key）即可接入，不需要 OpenRouter 侧开通动作；与 AIHubMix「异步能力需后台开通」的形态不同。

---

## 2. 问题 2：接口完整合同

### 2.1 端点、方法与鉴权

| 项目 | 结论 | 性质与来源 |
| --- | --- | --- |
| 生成端点 | `POST https://openrouter.ai/api/v1/images` | **事实**：[指南](https://openrouter.ai/docs/guides/overview/multimodal/image-generation.md) · [OpenAPI](https://openrouter.ai/openapi.json) `paths./images.post` |
| 鉴权 | `Authorization: Bearer <OPENROUTER_API_KEY>`；OpenAPI `securitySchemes.apiKey` 为 `http`/`bearer` | **事实**：[Authentication](https://openrouter.ai/docs/api_reference/authentication.md) · [OpenAPI](https://openrouter.ai/openapi.json) |
| Content-Type | `application/json` | **事实**：[指南](https://openrouter.ai/docs/guides/overview/multimodal/image-generation.md) |
| 同步性 | **同步**。单次 POST 返回 `{created, data[], usage?}`，图像以 base64 内联返回；没有 task id、没有轮询接口 | **事实**：[指南](https://openrouter.ai/docs/guides/overview/multimodal/image-generation.md) · [OpenAPI](https://openrouter.ai/openapi.json) |
| 可选流式 | `stream: true` 时走 SSE，事件类型 `image_generation.partial_image` / `image_generation.text_chunk` / `image_generation.completed` / 错误事件，终止于 `data: [DONE]` | **事实**：[指南](https://openrouter.ai/docs/guides/overview/multimodal/image-generation.md) · [OpenAPI `ImageStreamingResponse`](https://openrouter.ai/openapi.json) |
| 图片返回形式 | **base64（`b64_json`）**，可选 `media_type`。**没有** `url` 返回形式，因此不存在「结果 URL 有效期」问题 | **事实**：[OpenAPI `ImageGenerationResponse`](https://openrouter.ai/openapi.json) · [指南](https://openrouter.ai/docs/guides/overview/multimodal/image-generation.md) |
| 视频 API 对照 | 视频生成**是**异步任务（提交返回 polling URL），图像 API **不是** | **事实**：[Submit a video generation request](https://openrouter.ai/docs/api/api-reference/video-generation/submit-a-video-generation-request.md) · [指南](https://openrouter.ai/docs/guides/overview/multimodal/image-generation.md) |

**我的推断**：正因为没有 URL 结果，「归档短期上游 URL」这类第一期问题在 OpenRouter 上不存在；代价是响应体携带完整 base64 图像，`n=10` 且高质量时响应体可能达到数十 MB，需要关注网关与 Worker 的响应体上限。

### 2.2 请求字段（全局 Schema）

来自 [OpenAPI `ImageGenerationRequest`](https://openrouter.ai/openapi.json) 与 [指南的参数表](https://openrouter.ai/docs/guides/overview/multimodal/image-generation.md)：

| 字段 | 类型 | 约束 | 性质 |
| --- | --- | --- | --- |
| `model` | string | **必填** | **事实** |
| `prompt` | string | **必填**，`minLength: 1` | **事实** |
| `n` | integer | 1–10（`n > 1` 时单图 Provider 会拒绝） | **事实** |
| `input_references` | array | 最多 16 项；每项为 `{type:"image_url", image_url:{url, detail?}}`；url 可为 HTTP(S) 或 base64 data URL | **事实**（OpenAPI `maxItems: 16`） |
| `resolution` | string enum | `512` / `1K` / `2K` / `4K` | **事实**（实测 Zod 报错回显同一枚举） |
| `aspect_ratio` | string enum | 24 个值：`1:1,1:2,1:4,1:8,2:1,2:3,2.35:1,3:2,3:4,4:1,4:3,4:5,5:2,5:4,8:1,9:16,16:9,9:19.5,19.5:9,9:20,20:9,9:21,21:9,auto` | **事实**（[OpenAPI](https://openrouter.ai/openapi.json) + 2026-09-19 实测 Zod 回显完全一致） |
| `size` | string | 便捷简写：层级（`"2K"`）或显式像素（`"2048x2048"`）。像素形式是权威值，与不匹配的 `resolution`/`aspect_ratio` 同时出现会 **400** | **事实** |
| `quality` | string enum | 全局接受 `auto/low/medium/high/xhigh/max`（**不是所有端点都支持**） | **事实**（[OpenAPI](https://openrouter.ai/openapi.json) + 实测 Zod 回显） |
| `output_format` | string enum | `png/jpeg/webp/svg`（svg 仅矢量化模型） | **事实** |
| `background` | string enum | `auto/transparent/opaque`（全局；端点级能力会收窄） | **事实** |
| `output_compression` | integer | 0–100，仅 webp/jpeg | **事实** |
| `seed` | integer | 确定性采样，文档明示「Determinism is not guaranteed for all providers」 | **事实** |
| `stream` | boolean | SSE 部分图像 | **事实** |
| `session_id` | string | ≤256 字符，用于观测分组，**不发给 Provider** | **事实** |
| `user` | string | ≤256 字符，稳定终端用户标识；不会原样下发，会折叠为哈希后的上游 user | **事实** |
| `trace` | object | 观测/追踪元数据（`trace_id` 等） | **事实** |
| `provider` | object | 路由偏好，见 2.4 | **事实** |

**探测事实**：`POST /api/v1/images` 的**参数校验发生在鉴权之前**。带无效 Key 提交非法 `quality`/`aspect_ratio`/`resolution`/`background`/`output_format`/`n`/空 `prompt`，返回的是 `400` 与 Zod 校验信封，而不是 `401`；见 5.2。这类 400 因此可以被认定为「未到达 Provider、未受理」。

### 2.3 `openai/gpt-image-2` 的端点级能力（重点）

**事实**：`openai/gpt-image-2` 目前**只有 1 个端点**，来自 Provider `OpenAI`（`provider_name: "OpenAI"`、`provider_slug: "openai"`、`provider_tag: "openai"`）。来源：实时 `GET https://openrouter.ai/api/v1/images/models/openai/gpt-image-2/endpoints`，2026-09-19 11:50 +08:00。

端点级 `supported_parameters`（2026-09-19 实测原文）：

| 参数 | 描述符 | 值 |
| --- | --- | --- |
| `aspect_ratio` | enum | `1:1, 3:2, 2:3, 4:3, 3:4, 16:9, 9:16, 21:9, auto` |
| `quality` | enum | `auto, low, medium, high` |
| `background` | enum | `auto, opaque` |
| `n` | range | 1–10 |
| `input_references` | range | 0–16 |
| `output_compression` | range | 0–100 |

其余端点级字段：`allowed_passthrough_parameters: ["moderation"]`、`supports_streaming: true`。
**关键否定事实**：`resolution`、`size`、`output_format`、`seed`、`stream` **没有**出现在模型级/端点级 `supported_parameters` 中。按文档定义「缺少的键表示该端点不支持该参数」（[指南的能力描述符](https://openrouter.ai/docs/guides/overview/multimodal/image-generation.md)）。

模型级能力（`GET /api/v1/images/models`，2026-09-19）与端点级**完全一致**，没有额外并集；`architecture.input_modalities = ["text","image"]`，`output_modalities = ["image"]`，`supports_streaming: true`，`description` 明说「Supports high-fidelity image generation and editing via the dedicated Images API」。

**我的推断（重要）**：
1. 第一期 AIHubMix 的 `size` 走显式像素；OpenRouter 的 gpt-image-2 端点未声明 `size`/`resolution`，但 `aspect_ratio` 声明了 `auto`。**「像素级尺寸能否被接受」必须由一次付费冒烟测试确认**，不能把 `size: "1024x1024"` 直接固化进 Native Schema。
2. 本仓库 `openrouter-image.md` 旧资料写「Gateway 根据产品尺寸档案唯一查表并向 OpenRouter 发送内部像素 `size`」，这与端点级能力声明存在张力：`size` 属于全局请求字段（会被网关接受并按 `resolution`+`aspect_ratio` 归一或 400 拒绝），但它对**该端点**是否生效文档未声明。这是一个必须用实测收敛的点。

### 2.4 路由字段

`provider`（`ImageGenerationProviderPreferences`）支持：[`only` / `order` / `ignore` / `sort` / `allow_fallbacks` / `options`](https://openrouter.ai/openapi.json)，语义见 [Provider Routing](https://openrouter.ai/docs/guides/routing/provider-selection.md) 与 [指南](https://openrouter.ai/docs/guides/overview/multimodal/image-generation.md)：

- `only`：只允许列出的 provider slug（与账号级 allowed 列表**取交集**；若无交集则 404）。
- `order`：按序尝试；设置后**关闭负载均衡**。
- `ignore`：排除（与账号级 ignored 列表**合并**）。
- `sort`：按 `price` / `throughput` / `latency` 排序；设置后不做负载均衡。
- `allow_fallbacks`：默认 `true`；`false` 时主 Provider 失败即停止并返回上游错误。
- `options`：按 provider slug 传 Provider 私有参数；该端点只允许 `moderation`。

**事实**：OpenRouter 默认策略是「按价格负载均衡」，并在 Provider 返回 5xx 或被限流时自动 fallback。[API 概览](https://openrouter.ai/docs/api-reference/overview.md) · [Provider Routing](https://openrouter.ai/docs/guides/routing/provider-selection.md)

---

## 3. 问题 3：计量证据（本节是第二阶段选型的关键）

### 3.1 响应中的 `usage`

**事实**：`POST /api/v1/images` 的 200 响应顶层可选带 `usage`，Schema 名为 `ImageGenerationUsage`，必填字段为 `prompt_tokens` / `completion_tokens` / `total_tokens`；描述原文为「Token and cost usage for the image generation request, **when available**」。[OpenAPI](https://openrouter.ai/openapi.json) · [指南](https://openrouter.ai/docs/guides/overview/multimodal/image-generation.md)

`ImageGenerationUsage` 可用字段（OpenAPI）：

| 字段 | 单位/语义 | 性质 |
| --- | --- | --- |
| `prompt_tokens` | 整数；含图像、输入音频、工具 | **事实** |
| `completion_tokens` | 整数；生成的 token | **事实** |
| `total_tokens` | 整数；前两者之和 | **事实** |
| `prompt_tokens_details.cached_tokens` / `cache_write_tokens` / `audio_tokens` / `file_tokens` / `video_tokens` | 分项（可空） | **事实** |
| `completion_tokens_details.image_tokens` / `reasoning_tokens` / `audio_tokens` | **图像输出 token 分项** | **事实** |
| `cost` | number（USD credits）；描述「Cost of the completion」 | **事实** |
| `cost_details.upstream_inference_cost` / `upstream_inference_prompt_cost` / `upstream_inference_completions_cost` | 上游成本拆分（USD） | **事实** |
| `is_byok` | 是否使用自带 Key | **事实** |
| `service_tier` / `speed` / `iterations` / `server_tool_use` / `cache_creation` | 其他修饰信息 | **事实** |

官方响应样例（文档原样，图像场景）：

```json
{
  "created": 1748372400,
  "data": [{ "b64_json": "<base64-encoded-image>", "media_type": "image/png" }],
  "usage": { "prompt_tokens": 0, "completion_tokens": 4175, "total_tokens": 4175, "cost": 0.04 }
}
```

流式场景的 `image_generation.completed` 事件同样带 `usage`（含 `cost`）。[指南](https://openrouter.ai/docs/guides/overview/multimodal/image-generation.md)

### 3.2 计费单位：token 还是张数？

**事实**：`openai/gpt-image-2` 端点返回的 `pricing` 数组**只有 3 条 per-token 费率，且没有 `variant` 分档**（2026-09-19 实测原文）：

| `billable` | `unit` | `cost_usd` | 折算 |
| --- | --- | --- | --- |
| `input_image` | `token` | 0.000008 | $8 / 1M tokens |
| `input_text` | `token` | 0.000005 | $5 / 1M tokens |
| `output_image` | `token` | 0.00003 | $30 / 1M tokens |

来源：`GET https://openrouter.ai/api/v1/images/models/openai/gpt-image-2/endpoints`，2026-09-19 11:50 +08:00。

**事实**：图像模型的 `pricing[].unit` 有三种可能值 `image` / `megapixel` / `token`，并可能带 `variant`（如 `2k`、`4k` 的分辨率分档）。也就是说**同一 API 内确实存在「按 token」与「按张/按分辨率」混用**——只是 `openai/gpt-image-2` 这条端点当前是纯 token。[List endpoints 文档](https://openrouter.ai/docs/api/api-reference/images/list-endpoints-for-an-image-model.md) · [指南的 pricing 字段说明](https://openrouter.ai/docs/guides/overview/multimodal/image-generation.md)

**事实**：`GET /api/v1/models` 的通用 `pricing` 对象对同一模型给出 `{prompt: "0.000008", completion: "0.000008", image_output: "0.00003", input_cache_read: "0.000002", web_search: "0.01"}`（2026-09-19 实测）。它与按端点记录**不一致**：`prompt` 报成 $8/M 而不是 `input_text` 的 $5/M。

**我的推断**：通用 models API 的 `pricing` 是按「文本 token / 图像输出 token / 缓存读取 / 联网」这套**聊天向字段**压平的结果，图像侧字段名不匹配，**不能**作为图像结算的权威单价来源；权威来源是 `/api/v1/images/models/{author}/{slug}/endpoints` 的 `pricing[]`。这条差异应记录为「同一平台两处价格视图不一致」的实测事实。

**事实**：`input_cache_read` 为 $2/M tokens（缓存读取），该行**没有**出现在图像端点的 `pricing[]` 中。若命中缓存，实际计费与 `usage` 分项的表现需实测。[Rate Limits](https://openrouter.ai/docs/api_reference/limits.md) 说明「Only token prices are estimated; per-request fees, plugin charges, and image pricing are not part of the estimate」。

### 3.3 是否构成「可核验的计量事实」

**我的评估：构成，且强于第一期 AIHubMix 的 `/ai/v1` 路径。** 理由：

1. 成功响应同时给出**分项 token 数**（`prompt_tokens_details` / `completion_tokens_details.image_tokens`）与**金额**（`usage.cost`，USD）。
2. 文档把 `/api/v1/generation?id=<generation id>` 定义为**事后可查询的权威用量与成本记录**，字段包括 `id`、`model`、`provider_name`、`usage`（金额）、`total_cost`、`upstream_inference_cost`、`native_tokens_prompt/completion/cached`、`latency`、`generation_time`、`cancelled`、`finish_reason`、`created_at`、`api_type`、`data_region`、`workspace_id` 等。[Generation 文档](https://openrouter.ai/docs/api/api-reference/generations/get-request-&-usage-metadata-for-a-generation.md)
3. `usage.cost` 是**平台自己的计费金额**（不是平台自算），且 FAQ 明确「我们收到 Provider 处理的 token 总数，据此计算并从余额扣除」，无推理加价（加价只发生在充值环节）。[FAQ](https://openrouter.ai/docs/faq.md)

**待确认（阻塞级）**：**图像生成的响应体里没有 `id` 字段**（OpenAPI `ImageGenerationResponse` 只 `required: [created, data]`），文档也没有明确写「图像响应带 `X-Generation-Id`」。而 `/api/v1/generation` 必须传 `id`。因此**「图像生成能否事后按 generation id 拉取权威用量记录」在文档层面未被证实**。
- 间接证据（探测事实）：`POST /api/v1/images` 的响应头里存在 `Access-Control-Expose-Headers: X-Generation-Id, X-Provider-Name, request-id, cf-ray`（2026-09-19 03:51 UTC，无效 Key 实测）。这说明该端点**具备** `X-Generation-Id` 响应头。
- **我的推断**：图像生成成功后大概率会带 `X-Generation-Id`，且该 id 可用于 `/api/v1/generation` 对账；但 `id` 不在响应 body 中、也不在 OpenAPI 里，属于「头里有、文档没写」的隐含合同，**必须实测确认后才可写入结算设计**。

---

## 4. 问题 4：幂等与恢复能力

### 4.1 幂等

- **事实**：`/api/v1/images` 的完整 OpenAPI 操作**没有任何 `Idempotency-Key` 参数**（`parameters` 为空）。全量 `openapi.json` 中 `Idempotency-Key` 只出现在 `POST /interns`（一个与本仓库无关的托管沙箱功能），并伴随 `idempotency_key_reused` 错误码。抓取时间 2026-09-19。[OpenAPI](https://openrouter.ai/openapi.json)
- **事实**：文档索引 `llms.txt` 中除 intern 条目外无 idempotency 相关页面。[文档索引](https://openrouter.ai/docs/llms.txt)
- **事实**：请求体里唯一可用来做「分组」的字段是 `session_id`（≤256 字符）与 `trace`，文档明说 `session_id` 只用于观测分组、**不发往 Provider**；`trace` 是观测/广播元数据。[OpenAPI](https://openrouter.ai/openapi.json)

**结论（我的推断）**：OpenRouter **不提供**创建图像生成的幂等键；平台侧幂等必须继续由本仓库自己的 `Idempotency-Key + 请求哈希 + Job 唯一约束` 承担。

### 4.2 异步任务 / 轮询 / Webhook

- **事实**：图像 API **同步**，没有任务对象、没有轮询端点、没有 webhook 回调参数。对比：视频 API 才有「提交返回 polling URL → 轮询状态 → 下载内容」；Webhook 文档（[Broadcast Webhook](https://openrouter.ai/docs/guides/features/broadcast/webhook.md)）是把**观测 trace** 推送到 HTTP 端点，不是任务完成回调。
- **事实**：`stream: true` 是「同步请求内的 SSE 分块」，不是异步任务恢复机制——连接断掉就没有后续。
- **事实**：失败/取消的图像生成明确**不计费**；未完成的生成返回 `502 Bad Gateway` 而非部分结果。同时文档承认：「若客户端在生成中断开，上游仍可能完成渲染并向 OpenRouter 收费；但客户端只会观察到两种结果之一：完整计费的结果，或错误。OpenRouter 不会对用户未收到的工作计费。」[指南 · Billing and Cancellation](https://openrouter.ai/docs/guides/overview/multimodal/image-generation.md)

### 4.3 响应丢失后的对账能力

| 手段 | 能力 | 性质与来源 |
| --- | --- | --- |
| `/api/v1/generation?id=<id>` | **单请求级**权威用量与成本（含 provider_name、total_cost、latency、cancelled）。是唯一真正能定位「这一次提交是否被受理、是否已计费」的接口 | **事实**（接口存在）· **待确认**（图像请求是否可获得 id，见 3.3） · [文档](https://openrouter.ai/docs/api/api-reference/generations/get-request-&-usage-metadata-for-a-generation.md) |
| `X-Generation-Id` 响应头 | 探测事实：该端点的 CORS 暴露头包含它 | **探测事实**，2026-09-19 |
| `X-Provider-Name` 响应头 | 探测事实：可证明本次实际由哪个上游 Provider 服务 | **探测事实**，2026-09-19 |
| `GET /api/v1/key` | 账户/Key 级 `usage`/`usage_daily`/`usage_weekly`/`usage_monthly`/`limit_remaining`；**聚合值，无逐请求明细** | **事实** · [Rate Limits](https://openrouter.ai/docs/api_reference/limits.md) |
| `GET /api/v1/credits` | `total_credits` / `total_usage`（需要 **management key**） | **事实** · [Get remaining credits](https://openrouter.ai/docs/api/api-reference/credits/get-remaining-credits.md) |
| Activity 页 CSV/PDF 导出 | 可按时间窗 + Model/API Key/创建者分组导出；文档描述为**聚合**指标（Spend/Tokens/Requests），**不是逐请求明细** | **事实** · [Activity Export](https://openrouter.ai/docs/cookbook/administration/activity-export.md) |
| `X-OpenRouter-Metadata: enabled` | 可拿到 `openrouter_metadata`（含 `attempts[]`、`endpoints.available[].selected`、`strategy`、`attempt`），用于证明路由与是否发生 fallback | **事实**：该 opt-in 只声明覆盖 `/chat/completions`、`/messages`、`/responses`、`/completions` **四条**路由，**未列出 `/images`** · [Router Metadata](https://openrouter.ai/docs/guides/features/router-metadata.md) |

**我的推断**：整体恢复能力属于「**部分可用，且依赖未文档化的头**」。若 `X-Generation-Id` + `/api/v1/generation` 实测成立，OpenRouter 会成为本仓库目前**唯一**能对「已受理但响应丢失」做逐请求事后核验的 Provider；若不成立，则与 AIHubMix 的 `/v1` 同步路径同级（只能进入 `reconciliation_required`），差别只是 OpenRouter 的响应头至少能提供 `request-id` 与 `X-Provider-Name`。

---

## 5. 问题 5：错误语义

### 5.1 文档化错误信封

**事实**：标准错误信封为 `{ "error": { "code": number, "message": string, "metadata"?: object } }`，HTTP 状态码与 `error.code` 一致（当请求本身非法或余额不足时）；若请求已进入生成阶段，HTTP 200 + 响应体内错误。[Errors and Debugging](https://openrouter.ai/docs/guides/overview/errors-and-debugging.md)

**事实**：`/api/v1/images` 的 OpenAPI 声明了这些错误响应：`400 / 401 / 402 / 403 / 404 / 413 / 429 / 500 / 502 / 524 / 529`。[OpenAPI](https://openrouter.ai/openapi.json)

**事实**：Provider 错误会被归一化为稳定的 `error.metadata.error_type` 词表；图像相关类型包括 `invalid_image`（400）、`image_too_large`（400）、`image_too_small`（400）、`unsupported_image_format`（400）、`image_not_found`（404）、`image_download_failed`（400）。其他常用类型：`invalid_request`(400)、`content_policy_violation`(400)、`refusal`(400)、`payload_too_large`(413)、`authentication`(401)、`permission_denied`(403)、`payment_required`(402)、`rate_limit_exceeded`(429)、`provider_overloaded`(503)、`provider_unavailable`(502)、`timeout`(504)、`server`(500)、`unmapped`(500)。[Errors and Debugging · Typed error codes](https://openrouter.ai/docs/guides/overview/errors-and-debugging.md)

**事实**：`429` 与 `503` 可能带标准 `Retry-After` 头。`500` 的错误消息会被替换为通用串并省略 `provider_code`；非 500 会在 `error.metadata.provider_code` 暴露上游原始错误码。[同上](https://openrouter.ai/docs/guides/overview/errors-and-debugging.md)

**事实**：402 有细分语义，`error.metadata.limit_source` 可取 `openrouter_in_flight_budget` / `openrouter_key_limit` / `openrouter_credits`，并附 `remedy_hint`；其中 `openrouter_in_flight_budget` 是**瞬时**的（带 `Retry-After`），不应被当成凭据失效。[Rate Limits](https://openrouter.ai/docs/api_reference/limits.md)

### 5.2 实测错误信封（探测事实，2026-09-19 03:51–03:52 UTC）

| 场景 | HTTP | 响应体（脱敏） |
| --- | --- | --- |
| 无效 Key + 合法请求体 | 401 | `{"error":{"message":"User not found.","code":401}}` |
| 无效 Key + 非法 `quality` | **400** | `{"success":false,"error":{"name":"ZodError","message":"[... invalid_value ... \"quality\" ...]"}}` |
| 无效 Key + 缺 `model` | **400** | `{"success":false,"error":{"name":"ZodError","message":"[... invalid_type ... \"model\" ...]"}}` |

**重要观察（我的推断）**：

1. `/api/v1/images` 存在**第二套错误信封**：边缘参数校验走 `{"success":false,"error":{"name":"ZodError","message":"<JSON 字符串>"}}`，与文档描述的 `{"error":{"code","message","metadata"}}` **不同构**。Adapter 必须同时解析两种形状，**不能**假设 `error.code` 一定存在。
2. **参数校验先于鉴权**，且 Zod 报错会原样回显允许枚举（本次正是借此确认了 `quality`/`aspect_ratio`/`resolution`/`background`/`output_format`/`n`/`prompt` 的精确约束）。
3. 顶层未知字段（如 `moderation`）**不会**触发 Zod 拒绝，请求会继续走到鉴权（返回 401）。也就是说 Zod 层不是 `additionalProperties: false` 严格校验。**这构成一个真实风险：拼错的字段名会被静默忽略**，需要平台侧的强 Schema 校验兜底。

### 5.3 哪些错误能证明「未受理、未计费」

| 类别 | 能否作为「未受理」证据 | 理由 |
| --- | --- | --- |
| 400（Zod / `invalid_request` / 图像格式类） | **能**。校验在鉴权与 Provider 调用之前 | **我的推断**（基于探测事实 5.2）+ 文档「合成请求非法 → 4xx」 |
| 401 / 403 | **能**（未通过鉴权） | 文档 |
| 402（余额/预算不足） | **能**；注意 `openrouter_in_flight_budget` 是瞬时而非永久 | 文档 |
| 404（无可用 Provider，通常因 `provider.only` 过滤掉全部候选） | **能**；`openrouter_metadata.attempt` 为 0 时表示未到达任何 Provider | 文档（Router Metadata 的错误示例） |
| 413 | **能** | 文档 |
| 429（限流） | **不能**完全证明：可能来自 OpenRouter 平台，也可能来自已开始处理的上游 Provider。文档对图像场景只承诺「未完成 → 不计费」 | 文档 + **我的推断** |
| 502 / 503 / 504 / 524 / 529（上游失败/超时/过载） | **不能**证明未受理。文档只说「失败的生成不计费」，但无法排除「上游已完成渲染而 OpenRouter 承担成本」的分支 | 文档（Billing and Cancellation） |
| 客户端超时 / 连接断开 | **不能**证明。这是最危险的一类 | 文档 |

**事实**：图像计费是**全有或全无**（all-or-nothing）：完成则按端点定价全额计费，失败或取消不计费；不存在部分/分数计费。这与 chat completions「取消的流仍按已产生 token 计费」不同。[指南 · Billing and Cancellation](https://openrouter.ai/docs/guides/overview/multimodal/image-generation.md)

---

## 6. 问题 6：价格与计价单位

### 6.1 第一方价格页

- 人类可读价格页：[https://openrouter.ai/openai/gpt-image-2](https://openrouter.ai/openai/gpt-image-2)（页面为交互式渲染，**服务端 HTML 不含价格文本**；2026-09-19 抓取 504 KB HTML，未检出可机读的价格明细）。
- 机器可读价格来源（**推荐用于 Price Snapshot**）：
  - 端点级：`GET https://openrouter.ai/api/v1/images/models/openai/gpt-image-2/endpoints` → `endpoints[].pricing[]`
  - 模型级：`GET https://openrouter.ai/api/v1/images/models` → `supported_parameters`（不含价格）
  - 通用：`GET https://openrouter.ai/api/v1/models` → `pricing`（字段名图像侧不匹配，见 3.2）
- 充值费率与免费额度：[https://openrouter.ai/pricing](https://openrouter.ai/pricing) 与 [FAQ](https://openrouter.ai/docs/faq.md)。

### 6.2 单价快照（2026-09-19 11:50 +08:00）

| `billable` | `unit` | `cost_usd` | 折算 |
| --- | --- | --- | --- |
| `input_image` | token | 0.000008 | $8.00 / 1M |
| `input_text` | token | 0.000005 | $5.00 / 1M |
| `output_image` | token | 0.00003 | $30.00 / 1M |

这三条与本地旧快照 `images-models.md`、`openrouter-image.md` **完全一致**，因此旧快照在这一项上已回到第一方核实：**事实**，非过时数据。

### 6.3 是否随上游变动

- **事实**：`pricing` 是按端点发布的，且支持 `variant` 分辨率分档；文档明确「`pricing`：此端点的计费定价行」。[List endpoints](https://openrouter.ai/docs/api/api-reference/images/list-endpoints-for-an-image-model.md)
- **事实**：FAQ 声明「OpenRouter 不标记上游推理价格，你始终支付与 Provider 标价相同的价格」；加价只发生在充值环节（Standard 5.5%、Business 8%）。[FAQ](https://openrouter.ai/docs/faq.md) · [Pricing](https://openrouter.ai/pricing)
- **我的推断**：价格**会随上游变动**（FAQ「我们传递上游定价」+ 端点级发布机制）。本仓库必须把该 `pricing[]` 快照连同**抓取时间**固化进 Price Version，并定期重新拉取。

### 6.4 与文档示例的自洽性检查

[指南](https://openrouter.ai/docs/guides/overview/multimodal/image-generation.md) 首屏示例：`openai/gpt-image-2`、`quality: high`、`aspect_ratio: 16:9`、`n: 1`、输出 1536×864 PNG、耗时 94s、**成本 $0.13**。
若 `usage.cost=0.13` 且 `completion_tokens` 全部按 `output_image` $30/M 计，则隐含约 4333 个输出 token；这类量级与文档 200 样例的 4175 tokens 同阶，**数量级自洽**。
但文档的 200 样例 `{prompt_tokens: 0, completion_tokens: 4175, cost: 0.04}` **无法**用 $30/M 精确解释（4175 × 30/1M = $0.12525 ≠ $0.04），因此该样例应是通用占位示例，而非 gpt-image-2 真实样本。**我的推断**：以真实付费调用回传的 `usage` 为准，不采信文档示例数值。采样详情、`variant` 分档与缓存读取的实际计费行为列为待确认项。

---

## 7. 问题 7：模型身份与上游路由（对多 Offering 设计最关键）

### 7.1 `openai/gpt-image-2` 由谁供应

- **事实**：`GET /api/v1/images/models/openai/gpt-image-2/endpoints` 返回**恰好 1 个端点**：`provider_name: "OpenAI"`、`provider_slug: "openai"`、`provider_tag: "openai"`、`quantization: "unknown"`、`status: 0`、`uptime_last_30m: 100`、`uptime_last_1d: 99.62`。抓取时间 2026-09-19 11:50 +08:00。
- **事实**：该模型在 `GET /api/v1/images/models` 中只有**一个条目**；快照中的其他 OpenAI 图像模型（`openai/gpt-image-1`、`openai/gpt-image-1-mini`、`openai/gpt-5-image`、`openai/gpt-5-image-mini`、`openai/gpt-image-2.5-sunburst`、`openai/gpt-image-2.5-flare`、`openai/gpt-5.4-image-2`）是**不同 model id**，不是同一 model 的多供应。总模型数 52（图像 API）/ 54（通用 models API 带 image 过滤）。
- **推论**：**此刻** `openai/gpt-image-2` 在 OpenRouter 上是**单供应**，不存在「同一请求被路由到不同后端供应方」的情形。

### 7.2 「多供应」语义确实存在，但当前不适用于该模型

- **事实**：文档把「每个模型可能由多个 Provider 提供」作为一等概念，并提供 `provider.only/order/ignore/sort/allow_fallbacks` 来控制；默认行为是**按价格在头部 Provider 之间负载均衡以最大化可用性**，并在 5xx/限流时自动 fallback。[指南](https://openrouter.ai/docs/guides/overview/multimodal/image-generation.md) · [Provider Routing](https://openrouter.ai/docs/guides/routing/provider-selection.md)
- **事实（重要风险）**：路由偏好有三个来源会**叠加**：
  1. **请求级** `provider.only` / `ignore`；
  2. **账号级** allowed / ignored providers 设置（请求级是「天花板内收窄」/「合并」）；
  3. **路由变体**如 `:nitro`（按吞吐排序并放开 priority service tier 端点）、`:floor`（按价格排序）；`sort`/`order` 一旦设置即关闭负载均衡。
  [Provider Routing](https://openrouter.ai/docs/guides/routing/provider-selection.md) · [FAQ · 模型变体](https://openrouter.ai/docs/faq.md)
- **事实**：模型级 slug 可加 `:nitro` 等后缀，**不改变模型身份**（`model` 元数据仍是原模型），只改变路由。[FAQ](https://openrouter.ai/docs/faq.md)

**对多 Offering 路由设计的影响（我的评估）**：
1. **可核验计量的最大隐性风险不是「多 Provider」，而是「账号级与请求级路由偏好叠加」**。若账号设置过 ignored/allowed provider 或隐私/数据策略开关，实际被服务的端点可能与「按端点 API 读到的候选」不同；FAQ 明确：若请求的 provider 偏好与账号隐私设置无交集，请求会**直接失败**。[FAQ](https://openrouter.ai/docs/faq.md) · [Provider Routing](https://openrouter.ai/docs/guides/routing/provider-selection.md)
2. 由于当前只有 `openai` 一个端点，**用 `provider: {only: ["openai"], allow_fallbacks: false}` 可以把路由固定到单一上游**，从而让「结果可复现」与「计量可比」成立；这应作为接入 OpenRouter 时的默认请求策略，而不是依赖默认负载均衡。
3. **但 `allow_fallbacks: false` 有代价**：默认 fallback 是可用性来源；关掉后单端点故障即报错。是否采用应由平台的「结果可复现优先 vs 可用性优先」策略决定，并**记录在 Attempt 上**。
4. **`X-Provider-Name` 响应头（探测事实）可以事后证明实际服务方**；而 `openrouter_metadata`（`attempts[]`/`selected`）**未声明覆盖 `/images`**，因此不能依赖它做路由审计。[Router Metadata](https://openrouter.ai/docs/guides/features/router-metadata.md)
5. `openai/gpt-image-2` 的「同一 Vendor Model 多 Offering」在当前 OpenRouter 上**不是**一个现成可用的例证——若第二阶段的目标是验证多供给路由，OpenRouter 的 gpt-image-2 **提供不了多 Offering 样本**（更适合的图像模型例子是 `google/gemini-3-pro-image`、`google/gemini-3.1-flash-image` 等有 `google-vertex/global` 与 `google-ai-studio` 双供应渠道的模型，见本地快照 `images-models.md` 第 315–324 行，该双供应声明需回第一方核实）。

---

## 8. 问题 8：受控验证可行性

| 项目 | 结论 | 性质与来源 |
| --- | --- | --- |
| 注册与 Key | 注册账号后可在 `https://openrouter.ai/settings/keys` 自建 API Key；key 以 `Authorization: Bearer` 使用。另有 management key 用于 key/credits 管理类接口 | **事实** · [Authentication](https://openrouter.ai/docs/api_reference/authentication.md) · [Management API Keys](https://openrouter.ai/docs/guides/overview/auth/management-api-keys.md) |
| 是否需要充值 | 付费模型**需要余额**；余额为 0 或不足时返回 `402` | **事实** · [Rate Limits](https://openrouter.ai/docs/api_reference/limits.md) |
| 免费额度 | FAQ 称新用户获得「a small free allowance」，并宣传免费模型（约 $0/1M）；但**未公布额度数值**，且 `openai/gpt-image-2` 无 `:free` 变体，**不能**用于本次图像验证 | **事实（未公布数额）** · [FAQ](https://openrouter.ai/docs/faq.md) |
| 最小充值额 | 文档**未**给出最低充值金额。可核验的金额门槛只有：购买 credits 时 Standard 计划费率 **5.5%（最低 $0.80）**，Business **8%**；支付方式为信用卡 / AliPay / USDC | **事实** · [FAQ](https://openrouter.ai/docs/faq.md) · [Pricing](https://openrouter.ai/pricing) |
| 可建议的验证预算 | 单张 `quality: high`、1536×864 的官方示例成本约 **$0.13**（文档示例）；`low` 质量应显著更低。因此 **$5 级别充值足以覆盖十几次受控调用** | **我的推断**（基于文档示例值，非实测） |
| 额度有效期 | 条款保留「购买一年后未使用 credits 可被过期」的权利 | **事实** · [FAQ](https://openrouter.ai/docs/faq.md) |
| 退款 | 未使用 credits 可在交易后 **24 小时内**申请退款；平台费用不退；加密支付不可退 | **事实** · [FAQ](https://openrouter.ai/docs/faq.md) |
| 只读验证（已做） | `/api/v1/images/models`、`/api/v1/images/models/openai/gpt-image-2/endpoints`、`/api/v1/models?output_modalities=image`、`/api/v1/images`（无效 Key 探测）均可在**无余额**下完成 | **探测事实**，2026-09-19 |
| 付费验证（未做） | 本次调研**未**执行任何付费调用；`usage` 形状、`X-Generation-Id` 可用性、`/generation` 对账、`size` 像素接受度、`output_format`/`resolution` 实际生效与否均待付费冒烟 | **待确认** |

**建议的停止条件（我的推断）**：受控验证应设定「最多 N 次付费调用 + 每次记录 `usage.cost` 与 `/api/v1/key` 差值」的双重预算上限；一旦连续两次出现「响应丢失或 5xx 但余额被扣减」，立即停止并升级为 `reconciliation_required` 语义审查。

---

## 9. 与第一阶段 AIHubMix 的能力差异对照

| 维度 | AIHubMix（第一期，已实测） | OpenRouter（本期调研） | 性质 |
| --- | --- | --- | --- |
| 生成端点 | `POST /ai/v1/images/generations`（统一，可 `async:true`）与 `/v1/images/generations`、`/v1/images/edits` | `POST /api/v1/images`（单一同步端点） | **事实** |
| 同步/异步 | 默认同步，可异步 + 任务列表/详情/Webhook | **只有同步**；`stream:true` 是同步内 SSE，无任务对象 | **事实** |
| 请求格式 | JSON；`image`/`images`/`mask` | JSON；`prompt` + `input_references[]`（≤16），**无 mask 字段** | **事实** |
| 结果返回 | URL（约 30 分钟）或 base64；URL 下载需同一 Bearer | **仅 base64**（+`media_type`）；无短期 URL 问题 | **事实** |
| 响应 `usage` | `/ai/v1` **无** usage；OpenAI 兼容 `/v1` **有**分项 token usage（已实测） | 200 响应**有** `usage`（分项 token + `cost` USD + `cost_details`），文档标注「when available」 | **事实** |
| 逐请求事后对账 | 任务列表无 prompt/metadata/correlation id，**无法可靠关联**本地 Job；`/v1` 无幂等键，丢响应只能人工处理 | **可能**有 `X-Generation-Id` → `/api/v1/generation`（**待确认**）；另有 `request-id`、`X-Provider-Name` 头 | **事实 + 待确认** |
| 幂等键 | 无 | 无（`/interns` 才有，与本用途无关） | **事实** |
| 取消 | 无公开取消接口 | 无取消接口；失败/取消不计费（全有或全无） | **事实** |
| 错误信封 | 统一 `{error:{message,type,code,tid}}`，按 `code` 分类 | **两套**信封：文档式 `{error:{code,message,metadata}}` + 边缘 **Zod** `{success:false,error:{name,...}}`；`error_type` 词表稳定 | **事实（含实测）** |
| 请求 ID 对账 | 错误体带 `tid` | 响应头 `request-id`；成功侧可能有 `X-Generation-Id`（待确认） | **事实 + 待确认** |
| 多供应 | 单 Channel | 概念完备（默认按价负载均衡 + fallback），但 gpt-image-2 **当前单供应** | **事实** |
| 价格 | 文本输入 $5/M、图片输入 $8/M、图片输出 $30/M | 完全相同：input_text $5/M、input_image $8/M、output_image $30/M | **事实**（两者独立抓取，数值巧合一致） |
| 账号级策略叠加 | 无同类机制 | 有（allowed/ignored providers、隐私/ZDR、`:nitro`/`:floor` 变体），且会与请求级偏好叠加 | **事实** |
| 异步开通门槛 | 需后台开通，否则 `403 async_not_enabled` | 无开通动作，门槛是余额 | **事实** |

**结论对比（我的评估）**：OpenRouter 在「计量证据」与「错误分类」上**强于** AIHubMix 的 `/ai/v1` 异步路径；在「异步任务与恢复」上**弱于** AIHubMix 的异步路径（因为根本没有异步）；在「幂等」上与 AIHubMix 同样缺失。

---

## 10. 事实 / 推论 / 待确认三分清单

### 10.1 事实（第一方来源或实测，可直接引用）

1. OpenRouter 提供专用同步图像 API：`POST /api/v1/images`，Bearer 鉴权，非 beta 申请制。[指南](https://openrouter.ai/docs/guides/overview/multimodal/image-generation.md) · [OpenAPI](https://openrouter.ai/openapi.json)
2. 图像以 base64（`b64_json`）返回，无 URL 形式，因此无「短期 URL 有效期」问题。
3. 全局请求字段为 `model`/`prompt`/`n`/`input_references`/`resolution`/`aspect_ratio`/`size`/`quality`/`output_format`/`background`/`output_compression`/`seed`/`stream`/`session_id`/`user`/`trace`/`provider`。
4. `openai/gpt-image-2` 端点级只声明 `aspect_ratio`（9 值含 auto）、`quality`（auto/low/medium/high）、`background`（auto/opaque）、`n`（1–10）、`input_references`（0–16）、`output_compression`（0–100）；`allowed_passthrough_parameters=["moderation"]`；`supports_streaming=true`。实测于 2026-09-19 11:50 +08:00。
5. `openai/gpt-image-2` 当前只有 1 个端点，来自 `provider_slug/tag = "openai"`（Provider 名 "OpenAI"）。
6. 端点定价为 3 条 per-token 行：`input_image` $8/M、`input_text` $5/M、`output_image` $30/M；**无** `variant` 分档。
7. 图像端点的 `unit` 枚举包含 `image` / `megapixel` / `token`，并可能带 `variant`，即同一 API 内确实混用 per-token 与 per-image/per-megapixel。
8. 200 响应可带 `usage`（`prompt_tokens`/`completion_tokens`/`total_tokens` 必填，另有分项详情、`cost` USD、`cost_details`、`is_byok`），文档标注「when available」。
9. `/api/v1/generation?id=` 提供逐请求的权威用量与成本（含 `provider_name`、`total_cost`、`upstream_inference_cost`、`native_tokens_*`、`cancelled`、`latency`）。
10. 图像 API **无** `Idempotency-Key`，**无**任务 id、轮询或 webhook；`/images` 的 OpenAPI `parameters` 为空。
11. 图像计费全有或全无；失败/取消不计费；未完成返回 502 而非部分结果。
12. `POST /api/v1/images` 响应头暴露 `X-Generation-Id`、`X-Provider-Name`、`request-id`、`cf-ray`（实测，无效 Key）。
13. 参数校验先于鉴权；非法参数返回 **Zod 形态** 的 400，与文档错误信封不同构；未知顶层字段不触发 Zod 拒绝。
14. 标准错误信封为 `{error:{code,message,metadata}}`，`error_type` 为稳定词表，含 6 个图像专属类型；`429`/`503` 可能带 `Retry-After`。
15. 402 有 `limit_source` 细分；`openrouter_in_flight_budget` 为瞬时（带 `Retry-After`）。
16. 图像价格**不参与**在飞预授权预估（只预估 token 价格）。
17. 默认路由是按价格负载均衡并自动 fallback；`order`/`sort` 一旦设置即关闭负载均衡；账号级 allowed/ignored 与请求级偏好叠加。
18. `X-OpenRouter-Metadata` 只声明覆盖 4 条 completion 路由，**未包含 `/images`**。
19. 充值时 Standard 费率 5.5%（最低 $0.80）、Business 8%；`/api/v1/credits` 需 management key；`/api/v1/key` 提供 Key 级聚合用量。
20. Activity 导出是**聚合**（Spend/Tokens/Requests，可按 Model/Key/创建者分组），文档未描述逐请求明细。
21. 本地旧快照 `images-models.md` 与 `openrouter-image.md` 中的 3 条 token 单价与第一方实时数据**完全一致**。

### 10.2 推论（基于事实的合理判断，需在设计中标注为判断）

1. OpenRouter 的计量证据**强于** AIHubMix 的异步路径：一次成功响应即可拿到分项 token + 平台计费金额。
2. 通用 `GET /api/v1/models` 的 `pricing` 字段名与图像侧不匹配（`prompt` 报成 $8/M 而非 $5/M），**不应**作为图像价格权威来源。
3. 文档 200 样例 `{completion_tokens:4175, cost:0.04}` 与 $30/M 不自洽，应为占位示例，不可用于核算。
4. 缺少幂等键意味着平台侧幂等设计**不能**放宽；「发送后失联」在 OpenRouter 上仍不可自动重放。
5. 若 `X-Generation-Id` 实测可用，OpenRouter 可把 `reconciliation_required` 从「人工兜底」提升为「自动逐请求对账」；这将是本仓库接入的最大增益点。
6. 若把路由固定为 `provider:{only:["openai"], allow_fallbacks:false}`，可获得「单上游 + 可复现」的语义；代价是失去默认 fallback 的可用性。
7. 由于 gpt-image-2 当前单供应，OpenRouter **不能**为「同一 Vendor Model 由多个 Offering 供应」这一第二阶段目标提供现成例证。
8. 两套错误信封意味着 Adapter 的错误解析必须容错，否则 Zod 400 会被误判为「未知错误」而进入错误的恢复分支。
9. 未知字段被静默忽略（探测事实）意味着平台必须做**强 Schema 校验**，否则字段拼写错误会变成静默降级。

### 10.3 待确认（必须由付费受控验证收敛，阻塞正式计费）

| # | 待确认项 | 为什么阻塞 | 建议验证方式 |
| --- | --- | --- | --- |
| 1 | 成功响应是否真的带 `X-Generation-Id` | 决定能否逐请求事后对账 | 1 次最低成本付费调用，记录响应头 |
| 2 | 该 id 能否用于 `GET /api/v1/generation?id=` 并返回该次图像生成的 `total_cost`/`usage` | 决定「可核验计量」是否闭环 | 紧接 #1 调用该接口并比对金额 |
| 3 | `usage` 在图像场景是否**总是**返回（「when available」到底何时不可用） | 决定是否必须有兜底计量路径 | 多次不同参数（含 n>1、input_references）调用 |
| 4 | `/generation` 记录出现在「请求完成」后多久（是否存在结算窗口延迟） | 决定对账的重试/等待策略 | 完成后立即查、+5s、+30s、+5min 各查一次 |
| 5 | `size` 显式像素（如 `1024x1024`）是否被该端点接受/生效 | 决定 Native Schema 能否沿用第一期的像素尺寸档案 | 文生图与图生图各 1 次 |
| 6 | `resolution` / `output_format` / `seed` / `stream` 在该端点是否真的生效或仅被忽略 | 端点能力声明为「不支持」，但全局接受，行为未定义 | 逐项单变量调用 |
| 7 | `quality` 传 `xhigh`/`max`（全局合法、端点未声明）的真实行为 | 影响能力收窄实现 | 1 次调用观察是否报错或降级 |
| 8 | `input_references` 的实际数量上限与单图 MIME/像素/体积约束 | 影响输入资产校验 | 递增参考图数量直至失败 |
| 9 | `input_cache_read`（$2/M）是否会在图像请求中命中，命中时 `usage` 如何体现 | 影响单价版本与结算 | 重复同一提示词调用并比对 `prompt_tokens_details` |
| 10 | 客户端超时/断开后是否真的不计费（含上游已完成渲染的分支） | 决定 `reconciliation_required` 的计费假设 | 故意在超时后核对 `/api/v1/key` 与 `/generation` |
| 11 | 是否存在未文档化的幂等头或 `request-id` 关联能力 | 决定能否自动重试 | 同 `request-id` 重放请求观察行为 |
| 12 | 账号级 allowed/ignored providers 与隐私设置对 `/images` 的实际影响 | 决定是否需要平台侧固定 `provider.only` | 在测试账号调整设置后比对 `X-Provider-Name` |
| 13 | 双供应图像的对照模型（如 `google/gemini-3-pro-image` 的 `google-vertex/global` + `google-ai-studio`）是否真有两个端点 | 若要在 OpenRouter 验证多 Offering，需要这样的样本 | 只读调用其 endpoints API（无需付费） |
| 14 | `usage` 中图像输入 token 的计量方式（是否在 `prompt_tokens_details` 中体现 image 输入） | 影响分项结算 | 单图参考 vs 无参考对照 |

---

## 11. 来源列表

第一方文档（均 2026-09-19 抓取）：

- [Image Generation 指南](https://openrouter.ai/docs/guides/overview/multimodal/image-generation.md)
- [Generate an image（API Reference）](https://openrouter.ai/docs/api/api-reference/images/generate-an-image.md)
- [List image generation models](https://openrouter.ai/docs/api/api-reference/images/list-image-generation-models.md)
- [List endpoints for an image model](https://openrouter.ai/docs/api/api-reference/images/list-endpoints-for-an-image-model.md)
- [API 概览（usage 语义、/generation）](https://openrouter.ai/docs/api-reference/overview.md)
- [Get request & usage metadata for a generation](https://openrouter.ai/docs/api/api-reference/generations/get-request-&-usage-metadata-for-a-generation.md)
- [Errors and Debugging](https://openrouter.ai/docs/guides/overview/errors-and-debugging.md)
- [Router Metadata](https://openrouter.ai/docs/guides/features/router-metadata.md)
- [Provider Routing](https://openrouter.ai/docs/guides/routing/provider-selection.md)
- [Rate Limits（含 In-flight spending budget）](https://openrouter.ai/docs/api_reference/limits.md)
- [Authentication](https://openrouter.ai/docs/api_reference/authentication.md)
- [Management API Keys](https://openrouter.ai/docs/guides/overview/auth/management-api-keys.md)
- [Get remaining credits](https://openrouter.ai/docs/api/api-reference/credits/get-remaining-credits.md)
- [Activity Export](https://openrouter.ai/docs/cookbook/administration/activity-export.md)
- [FAQ](https://openrouter.ai/docs/faq.md)
- [Pricing](https://openrouter.ai/pricing)
- [文档索引 llms.txt](https://openrouter.ai/docs/llms.txt)
- [OpenAPI 规范（JSON）](https://openrouter.ai/openapi.json)

第一方只读 API（2026-09-19 11:50–11:55 +08:00 实测）：

- [图像模型列表](https://openrouter.ai/api/v1/images/models)（52 条）
- [openai/gpt-image-2 端点记录](https://openrouter.ai/api/v1/images/models/openai/gpt-image-2/endpoints)
- [通用模型端点记录](https://openrouter.ai/api/v1/models/openai/gpt-image-2/endpoints)
- 通用模型列表 `https://openrouter.ai/api/v1/models?output_modalities=image`（54 条）
- `POST https://openrouter.ai/api/v1/images` 无效 Key 探测（仅观察校验与信封；无付费调用）

本地旧快照（仅作对照，不作为当前合同）：

- `out-reference/openrouter/openrouter-image.md`
- `out-reference/openrouter/图像生成.md`
- `out-reference/openrouter/images-models.md`
- `out-reference/openrouter/errors-and-debugging.md`
