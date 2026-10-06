# 渠道事实台账（按渠道分节）

本仓库自己的**渠道事实汇总登记**——把散在各 `out-reference/<provider>/` 的上游材料归纳成结论，逐条给出处。不是平台接口合同，运行中的服务不读取本文。

- **只放结论**，引用不复述：原始形状、逐字样本与错误码表在 `out-reference/<provider>/`，见 §1 的总账。
- **不合并渠道**：一个渠道一族，各自独立；渠道差异属该渠道 ② Driver 的内部实现，不互相推导（依据 [`docs/design/0004`](../design/0004-layered-architecture.md) R1）。
- **不记凭证值**，只记变量名：`AIHUBMIX_API_KEY`（AIHubMix）、`APIMART_API_KEY`（APIMart）、`DOUBAO_API_KEY`（火山方舟）。三个变量在 **User 与 Machine 级都存在**，Agent 进程默认环境里读不到——用 `[Environment]::GetEnvironmentVariable(name,'User'|'Machine')` 取（2026-09-23 复核）。
- **本文不记计费调用流水**：授权依据、次数、花费与样本位置统一留档在 [`docs/verification/paid-provider-calls.md`](../verification/paid-provider-calls.md)。
- **不记平台口径**：平台怎么取成本、怎么结算、怎么对客呈现归 [`docs/design/0007`](../design/0007-pricing-floor-and-settlement.md)、[`docs/adr/0006`](../adr/0006-no-settlement-without-metering-evidence.md) 与 [`docs/adr/0017`](../adr/0017-provider-errors-are-rewritten-for-consumers.md)；实施清单归相应工单。

## 1. 渠道与原始材料总账

| 渠道 | 文档（第一方） | API Base URL | 原始材料 |
| --- | --- | --- | --- |
| AIHubMix | `https://api.inferera.com/model/gpt-image-2.5-sunburst/llms.txt`、`…/gpt-image-2.5-flare/llms.txt`、`out-reference/aihubmix/error-code.md` | `https://api.inferera.com/v1`（用户指定） | `out-reference/aihubmix/`：`response-shapes.md`、`transcript-sync-and-async-2026-09.json`、`gpt_image_2_generations.json`、`schema-gpt-image-2*.endpoints.json`、`error-code.md`、`gpt-image-2.md` |
| APIMart | `https://docs.apib.ai/cn/api-reference/images/gpt-image-2.5/generation.md`（另有 `/tasks/status.md`、`/uploads/images.md`） | `https://api.apib.ai/v1`（用户指定；文档正文写 `api.apimart.ai`，指向同一套服务） | `out-reference/apimart/`：`response-shapes.md`、`controlled-probe-2026-09-19.json`、`transcript-image-edit-2026-09-19.json`、`catalog-models.json`、`schema-gpt-image-2.5-flare.input.json`、`gpt-image-2.5-generation.cn.md`、`tasks-status.cn.md`、`uploads-images.cn.md`、`apimart-image-api-research.md` |
| Doubao（火山方舟） | `https://docs.volcengine.com/docs/82379/1541523`（图片生成 API；另有 [模型价格](https://docs.volcengine.com/docs/82379/1544106)、[Base URL 及鉴权](https://docs.volcengine.com/docs/82379/1298459)） | `https://ark.cn-beijing.volces.com/api/v3` | `out-reference/doubao/`：`doubao-ark-image-research.md`、`图片生成模型API调用指南.md`、`图片生成示例.md`、`doubao-price.md`、`Doubao-Seedream-5.0-pro-教程.md`、`error-code.md` |

厂商侧材料：`out-reference/openai/openai-images-generate-2026-09-20.md`（`size` 像素型与 `quality` 六档的厂商口径）。

## 2. AIHubMix

### 2.1 端点与鉴权

| 端点 | 形态 | 平台使用 |
| --- | --- | --- |
| `POST /v1/images/generations` | 同步，OpenAI 兼容 JSON | `prompt_only` 分支走它 |
| `POST /v1/images/edits` | 同步，OpenAI 兼容 multipart | `image_conditioned` / `masked` 分支走它 |
| `POST /ai/v1/images/generations` | 默认同步，`async: true` 转任务式（`GET /ai/v1/images/{id}` 轮询） | 不使用 |

- 机器 Schema（免鉴权）：`https://api.inferera.com/call/schema/models/{model}/endpoints`；`aihubmix.com` 在本机不可达，同一 Schema 在 `api.inferera.com` 上取到（出处：`out-reference/aihubmix/schema-gpt-image-2*.endpoints.json`）。
- 鉴权：`Authorization: Bearer` + `AIHUBMIX_API_KEY`；输出 URL 约 30 分钟过期，下载需带同一凭据（出处：`out-reference/aihubmix/gpt-image-2.md`）。
- 同步两个端点的存在性已由真实调用结清（[`paid-provider-calls.md`](../verification/paid-provider-calls.md) §2、§8.1）。

### 2.2 参数与取值

- 本平台走 `/v1` 族，**没有 `extra` 这一层**：参数落顶层（出处：`out-reference/aihubmix/schema-gpt-image-2*.endpoints.json` 的 `request.schema`）。
- 字段面：`model` / `prompt` / `image` / `mask` / `n` / `size` / `output_format` / `quality`（`image`、`mask` 只在 edits 端；generations 端没有这两个字段）。平台按**公网 URL 文本部件**把参考图与遮罩逐字透传，不下载、不上传。机器 Schema 把这两个字段声明成 `format: binary`，与渠道文档通用参数表把 `image` 写成 `string`（"参考图片路径"）冲突；真实上游对 URL 文本部件的接受度待一次计费调用确认。
- `prompt` 必填、`minLength 1`；2.5 另有 `maxLength 32000`。
- `n`：`1`–`10`，默认 `1`。
- `quality`：2.5 两款 `low` / `medium` / `high` / `xhigh` / `max` / `auto`（默认 `auto`）；`gpt-image-2` 只有 `low` / `medium` / `high`（机器 Schema 里没有 `auto`）。
- `size`：`auto` 或 `宽x高`；Schema 用 `anyOf` 的 `const` + `pattern` 表达，不是枚举。`gpt-image-2` 的 edits 面另有 `auto` / `1024x1024` / `1536x1024` / `1024x1536` 的限制。
- `output_format`：2.5 是 `png` / `jpeg` / `webp`（默认 `png`）；`gpt-image-2` 无 `webp`。
- `quality` 的位置按**端点族**分辨：`/v1/*` 在顶层；`/ai/v1` 在 `extra` 内（`extra` 含 `background` / `moderation` / `output_compression` / `quality` / `user`），取值集合两处相同。
- `background` / `output_compression` / `moderation` / `user` 只存在于 `/ai/v1` 族的 `extra` 内；`background` 的 `transparent` 要求 `output_format` 为 `png` 或 `webp`（`/ai/v1` 族的机器 Schema 约束）。
- 本平台对这些字段的字段面与取值一律按**厂商契约**声明，不以渠道机器 Schema 的宽窄为准（依据 [`docs/adr/0015`](../adr/0015-vendor-model-contract-and-offering-parameter-mapping.md)；`gpt-image-2` 的 `quality` 差异属型号面）。
- 平台当前不校验取值，但按候选声明过的参数面过滤：没声明的直接丢弃，不报错、不发上游。
- 参考图与遮罩按名字落位：以 `image` 开头＝参考图，含 `mask`＝遮罩（都像时以遮罩为准），其余拒绝；一张发 `image`、多张发重复的 `image[]` 部件——**重复单值 `image` 会 400**。
- 上游 `403` 的 `insufficient_user_quota` 说的是**平台在该渠道的账户欠费**（`Platform Funding Failure`），与消费者余额无关。

### 2.3 响应与失败

- 同步响应要点：顶层 `created` / `background` / `output_format` / `quality` / `size` / `usage`，图像在 `data[0].b64_json`，**没有任务 id、没有金额字段**（出处：[`out-reference/aihubmix/response-shapes.md`](../../out-reference/aihubmix/response-shapes.md) §1）。
- `usage` 是四分项：`input_tokens_details{text_tokens,image_tokens}` / `output_tokens_details{…}` + `total_tokens`；**没有 `cached_tokens`**。
- 2.5 两款在同步 `/v1` 上与 `gpt-image-2` 同构（顶层字段集合相同，编辑端点同字段面），② 的同步解码器不需要按型号分支。
- 2.5 的 `/v1/*` 实测：`quality` 顶层传入即被接受（无需 `extra`）；接受并落实 `background=transparent`（响应回显 + 产物是带 alpha 的 RGBA PNG）；两张参考图经重复 `image[]` 部件一次提交成功。
- 绑定失败与恢复：创建请求失联后没有取回手段（同步响应无 id，且同步调用不出现在 `/ai/v1/images` 任务列表里），只能人工对账（[`docs/adr/0007`](../adr/0007-reconciliation-instead-of-automatic-retry.md)）。
- 异步 `/ai/v1` 任务对象给出任务 id 与轮询状态，**不返回 `usage`、不返回金额**，因此不作为平台的计量与计费执行路径（出处：`out-reference/aihubmix/response-shapes.md` §3；[`paid-provider-calls.md`](../verification/paid-provider-calls.md) §1）。
- 错误信封：`{"error":{"code","message","type"}}`（异步文档另带 `tid`）；实测顶层 `quality` 非法时 HTTP 400 + `code: schema_violation`，**未知参数是硬拒绝、不静默降级**（出处：`out-reference/aihubmix/response-shapes.md` §4）。
- 错误码表：见 `out-reference/aihubmix/error-code.md`（第一方页面，更新于 2026-06-01）。可用信息的边界：**只有部分状态码带机器可读的错误标识符**（如 `insufficient_user_quota`、`prompt_missing`、`prompt_too_long`、`text_too_long`、`size_not_supported`、`n_not_within_range`），其余只能靠状态码 + 消息文本；该页自述大部分 400 是上游透传报错。`403` 的其余分支（账号禁用、IP 白名单、令牌不支持该模型、渠道被禁用）都是我们与渠道之间的配置/资质问题；该页没有「服务器错误」这一档，`503` 只有「没有可用渠道」与「被官方限速」两种含义。⇒ 分类以状态码兜底，并保留原始文本供人工核对。

### 2.4 计量与费率

- 按 Tokens 计费，四档单价（每 1M tokens）：文本输入 `$5` / 图像输入 `$8` / 文本输出 `$10` / 图像输出 `$30`（出处：`out-reference/aihubmix/gpt-image-2.md` 与模型页）。
- 上游**只返回四分项 token、不返回任何金额字段**；`llms.txt` 写 `Pricing: per-generation`，与模型页的 token 单价表冲突——**以模型页的四档 token 单价为准**。
- 四档与 `config/bootstrap/` 里各 AIHubMix 素材的 `price_plan` 逐项一致（含 `text_output_microusd_per_million = 10000000`）——素材里这条供给登记的**计价形态**是 `token_rates`（按四分项 token 计量量），那份价目表就是它的参数。
- 本阶段按 Tokens 计费，**不考虑缓存档**；逐笔成本价与金额留档在 [`paid-provider-calls.md`](../verification/paid-provider-calls.md)。

## 3. APIMart

### 3.1 端点与鉴权

| 端点 | 形态 | 平台使用 |
| --- | --- | --- |
| `POST /v1/images/generations` | 异步，立即返回 `task_id` | 提交生成任务 |
| `GET /v1/tasks/{task_id}` | 任务查询（可选 `?language=`，只影响 `error.message`） | 轮询到终态并取计量与成本 |
| `POST /v1/uploads/images` | 上传本地图换公网 `url`（`multipart/form-data`，字段名 `file`） | APIMart Driver 用它把内联参考图与遮罩换成公网 URL，再组装生成请求；生成入口收敛为只收公网 URL 的合同见 [Spec 0005](../specs/0005-synchronous-image-gateway.md) §1、§3，随实现落地 |

- 鉴权：`Authorization: Bearer` + `APIMART_API_KEY`。
- 三个端点在**我们实际配置的域名**上均存在：无凭证请求返回 401，对照的 `/v1/nonexistent-route` 返回 404（零费用探测，[`paid-provider-calls.md`](../verification/paid-provider-calls.md) §5）。
- 目录里 `gpt-image-2.5-flare` / `-sunburst` 的 `supported_endpoint_types` 是 `["image-generation","openai"]`（出处：`out-reference/apimart/catalog-models.json`）。
- 任务面是唯一的计量与成本来源：终态同时返回四分项 `usage` 与 `cost`；**非任务面拿不到 token、也没有 `cost` 字段**，因此平台只用任务面。

### 3.2 请求参数与尺寸

- 字段：`model`（`gpt-image-2.5-flare` / `-sunburst`）、`prompt`、`size`、`resolution`、`quality`、`n`（`1`–`4`）、`output_format`、`output_compression`、`background`、`moderation`、`image_urls`、`mask_url`。
- `size`：默认 `auto`；另有 15 个比例（`1:1` `3:2` `2:3` `4:3` `3:4` `5:4` `4:5` `16:9` `9:16` `2:1` `1:2` `21:9` `9:21` `3:1` `1:3`）与精确像素（如 `1600x1200`）。文档明写图生图时**建议不传** `size`，由系统按输入图比例与 `resolution` 算。
- `resolution`：默认 `1k`，取 `1k` / `2k` / `4k`；只与比例形式的 `size` 配合决定输出像素，`size` 用精确像素时**该字段被忽略**。
- 精确像素的合法性：宽高均为 `16` 的倍数、任意单边 ≤ `3840`、长短边之比 ≤ `3:1`、总像素 `655,360`–`8,294,400`；高于 `2560x1440` 属实验性范围。
- 比例 × 档位 → 像素的映射表存在（15 比例 × 3 档，整表见上游文档）；两个特征值：`4k` 的 `1:1` 是 `2880×2880`（不是 3840）；`4k` 下只有 `16:9` / `9:16` / `2:1` / `1:2` / `21:9` / `9:21` / `3:1` / `1:3` 八个比例能到 3840。
- `quality`：`low` / `medium` / `high` / `xhigh` / `max` / `auto`（默认 `auto`，五档+auto；`xhigh`、`max` 仅 2.5 支持，传给 `gpt-image-2` 会同步返回 400、不自动降级）。
- `moderation`：渠道默认 `low`（厂商契约默认是 `auto`）。
- 来源：`out-reference/apimart/gpt-image-2.5-generation.cn.md`（2026-09-19 抓取）与上游文档 `https://docs.apib.ai/cn/api-reference/images/gpt-image-2.5/generation.md`，2026-09-20 逐句复核一致。
- **`gpt-image-2` 的尺寸档案不适用于 2.5**：`4k` = 3840、「`auto` 回落 `1:1`」、「不传 `size` ⇒ 输出分辨率 = 输入图分辨率」都是 2.5 之前的旧口径；2.5 是「`auto` 由模型按提示词或参考图决定」、不传 `size` 时按输入图比例 + `resolution` 算。
- 厂商侧对照（`out-reference/openai/openai-images-generate-2026-09-20.md`）：厂商原生 `size` 是**像素型**（任意 `宽x高`、宽高被 16 整除、比例 1:3–3:1、上限 `3840x2160`、`>2560x1440` 属实验性），**没有 `resolution` 字段**——那是 APIMart 的渠道包装；`gpt-image-2.5-flare` / `-sunburst` 是厂商侧真实模型（含 `2026-09-08` 快照枚举）；`quality` 的 `xhigh` / `max` 与厂商契约一致。
- 提交请求参数、遮罩与幂等绑定见 §3.3、§3.5；平台的承载面只声明合同可达的字段（`auto` | `宽x高`），比例名与 `resolution` 无法从厂商合同到达，因此不声明。

### 3.3 参考图、遮罩与上传

- 上传返回 `{url, filename, content_type, bytes, created_at}`；实测返回的 URL 主机是 `getapib.org`（不是文档示例里的 `upload.apimart.ai`），`https`、路径较长且随机；文档写 URL 有效期 72 小时。
- 接受格式 JPEG / PNG / WebP / GIF，单张 ≤ `20MB`；上游文档示例的报错文案（`unsupported image type…`、`file size … exceeds maximum 20971520 bytes`）与上传页一致（出处：`out-reference/apimart/uploads-images.cn.md`）。
- 生成请求里的参考图是 `image_urls`：**字符串数组**（≤16，单张 ≤20MB、总计 ≤256MB），只接受**公网可访问 URL**。上传页的 Python 示例把它写成 `[{"url": …}]`（对象数组）——实测用字符串数组提交成功并完成出图，平台取字符串数组。
- 遮罩是 `mask_url`（字符串），与 `image_urls` 同用可行；实测用 512×512 带 alpha 的 PNG、尺寸与参考图一致，上游未报尺寸/通道错误。
- 上传页声明生成接口**不再接受 base64**，生成页仍写支持 `base64 data URI` 可与 URL 混填 ⇒ 平台给上游的生成请求只放公网 URL：本地文件先经平台的上传端点换成公网 URL。
- 流程：平台把公网 URL 逐字透传给上游，不下载、不上传。生成入口只收公网 URL、平台自建上传端点的合同见 [Spec 0005](../specs/0005-synchronous-image-gateway.md) §1、§3 与 [Spec 0007](../specs/0007-image-upload-and-object-storage.md)。
- 参数名不改写：生成请求用上游原生名 `image_urls` / `mask_url`，由 Offering Parameter Mapping 从合同字段 `image` / `mask` 落位（依据 [`docs/adr/0015`](../adr/0015-vendor-model-contract-and-offering-parameter-mapping.md)）。
- `mask_url` 不在 2.5 生成文档的字段表里，但**实测被接受**（提交 200 → `completed`），因此按厂商契约声明遮罩；渠道将来若拒绝它，表现会是渠道报错，不是平台静默丢字段。
- 遮罩**不额外计费**：带/不带 `mask_url` 的两次调用 `usage` 与 `cost` 完全相同。
- 遮罩是否真的生效只有**弱信号**：每 8 像素采样对比两张产物，遮罩椭圆内差异像素 21%、全图 79.9%——方向一致，但生成随机且请求无 `seed`，不能据此断言遮罩被严格遵从。
- 未做：20MB / 16 张 / 256MB 边界未逐个压测。

### 3.4 任务流转与响应

- 提交响应：`{"code":200,"data":[{"status":"submitted","task_id":"task_…"}]}`——**`data` 是数组，读 `data[0].task_id`**。
- 任务终态：`status` 取 `pending` / `processing` / `completed` / `failed` / `cancelled`；成功时 `result.images[]` 每项为 `{url: [字符串], expires_at}`（**`url` 是数组**，`expires_at` 说明结果 URL 会过期、必须立刻下载转存）。
- 终态同时返回四分项 `usage`（`input_tokens_details` 区分 `text_tokens` / `image_tokens`，另有 `cached_tokens`）与 `cost` / `credits_cost`（`credits_cost = cost × 10`）。
- 实测结清：任务成功响应的 `usage` 粒度比生成页与 `/tasks/status` 两处文档样例都更细（文档只给聚合三项，`tasks-status.cn.md` 样例连 `usage` 都没有）；`cost` 是上游声明的实际扣费。参考图会真实计入 `image_tokens`（1024×1024 ⇒ 1024）；带参考图时四分项形状不变，② 的解码器不需要新分支。
- **计价形态是"上游直接给实扣金额"**（`cost`，上面那条实测样例 `0.011354`）：素材里这条供给登记为 `upstream_declared` 并声明成本币种 USD，**没有** Price Plan——平台不按 token 单价反算它的成本。
- **创建请求的失联处理**：响应丢失后无法按时间窗反查这次提交是否被受理（文档化的任务管理只有状态查询与 webhook，没有任务列表接口）⇒ 进人工对账，不自动重提。

### 3.5 幂等

- `POST /v1/images/generations` 的机器 Schema 明确定义幂等键：头 `Idempotency-Key`、重放标识头 `Idempotency-Replayed`、`required: false`、`retention_seconds: 86400`、`scope: api_key_endpoint`（出处：`out-reference/apimart/schema-gpt-image-2.5-flare.input.json`）。
- 结论：创建请求失联后**具备安全重试的机制基础**；平台的处置路径仍是「创建请求绝不重发」（[`docs/design/0002`](../design/0002-image-generation-tech-design.md) §7），是否启用幂等重试属后续工作项。
- 同一 Schema 的另外两条事实：`model.const = "gpt-image-2.5-flare"`（与 `native_model_id` 同名，发布期校验可过）；`additionalProperties: true`，`quality` / `size` 只声明为 `type: string` 无取值约束，`required` 仅 `model`（另有 `anyOf: prompt | messages`）⇒ **未知字段不被此 Schema 拒绝**，与 AIHubMix 的 `additionalProperties: false` 相反。

### 3.6 错误信封与错误码

- 信封统一为 `{"error":{"code","message","type"}}`（部分页面另有 `param`、`request_id`）。实测（无凭证 401）的 `error.code` 是**空字符串**、`type` 是 `apimart_error`，而文档示例里 `code` 是数字（401/402/…）⇒ 分类必须以 HTTP 状态码兜底，不能只依据 `error.code`。
- 每请求标识在失败时也有：响应头 `X-Oneapi-Request-Id`，同时写进 `message` 的 `(request id: …)`；失败路径的 `provider_error_message` 原样落库，排查不必额外取头（成功路径的对账标识是 `task_id`）。
- `402` 是**平台在该渠道的账户欠费/额度不足**（`Platform Funding Failure`），与消费者余额无关；对客呈现与内部归类见 [`docs/adr/0017`](../adr/0017-provider-errors-are-rewritten-for-consumers.md)。

| HTTP | 含义（第一方口径） | 能否证明「未受理、未计费」 |
| --- | --- | --- |
| `400` | `invalid_request_error`：size 不合法 / resolution 不支持 / 像素违规；查询侧＝无效的任务 ID | 能 |
| `401` | `authentication_error`：身份验证失败 | 能 |
| `402` | `payment_required`：账户余额不足（这里的「账户」是我们） | 能 |
| `403` | 权限不足 | 能 |
| `409` | 幂等子类：`idempotency_in_progress` / `idempotency_key_reused` / `idempotency_result_indeterminate` | 前两者能；`result_indeterminate` **不能**（第一方要求停止自动重试、不要换 Key） |
| `429` | `rate_limit_error`：请求过于频繁 | 能 |
| `500` | `server_error`：服务器错误 | **不能**——结果不明 |
| `502` | 网关错误 | **不能** |
| `503` | `service_unavailable`：上游暂时不可用 | 普通 503 不能；`503 idempotency_unavailable`（原文「当前请求未执行」）**能** |
| 超时 / 连接中断 | — | **不能**（第一方明示：客户端取消不代表服务端未生成、不代表不计费） |

- 陷阱一：`500` 会被用来承载参数错误（示例 message 为 `build_request_failed: invalid size: 3:5, allowed: …`）；按「500 ⇒ 结果不明」处理会把可修正的请求错误升级成人工对账。
- 陷阱二：失败任务会退款——`failed` 状态写明 reserved funds are refunded，`/v1/usage` 写明失败与失败后退款的调用不计入、部分成功的批次按实际交付张数计费。
- 创建阶段另有按状态码收窄的三类（`429` 与两个幂等子类），凭据/权限类 HTTP 状态（401/402/403）判为确定性拒绝，`5xx` 不按状态码定性。

## 4. Doubao（火山方舟）

- **计价单位是元/张，币种 CNY**（不是按 token、也不是美元）：pro 输出单图生成 **≤261 万像素 0.30 元/张**、**>261 万像素 0.60**；pro 图层拆分场景 0.15 / 0.30；pro 输入图首张免费、第 2 张起 0.02；lite 输出 **0.22**、输入免费；4.5 输出 0.25、4.0 输出 0.20。**因审核等原因未成功输出的图片不计费**（出处：[模型价格](https://docs.volcengine.com/docs/82379/1544106)，2026-09-19 抓取；归纳见 `out-reference/doubao/doubao-ark-image-research.md` §6 的价格表 L290–L296 与 §12.1 事实 8 L525）。
- **计费依据是成功张数**：终态 `usage.generated_images`；`output_tokens` / `total_tokens` 是 `sum(宽×高)/256` 的面积换算，**不是真实 token 消耗**，不能当作按 token 结算的计量证据（出处：同上一节 §12.1 事实 7、§12.2 推论 1）。
- 端点与鉴权：`POST https://ark.cn-beijing.volces.com/api/v3/images/generations`，`Authorization: Bearer` + `DOUBAO_API_KEY`；图片生成**没有**异步任务 API、**没有**幂等键、**没有**任务 ID，创建请求失联后只能人工对账（出处：同上一节 §12.1 事实 1、2、11）。
- 平台怎么取成本、怎么定价、怎么对客呈现不在这里：见 [`docs/design/0007`](../design/0007-pricing-floor-and-settlement.md) 与 [`docs/adr/0006`](../adr/0006-no-settlement-without-metering-evidence.md)。

## 5. 本台账未结清的部分

- APIMart 400 / 429 / 5xx 的**真实**报文未实测（400 与 413 形状来自上传页文档，`build_request_failed` 前缀来自生成页文档）。
- AIHubMix 缓存输入、失败计费、促销/折扣的权威规则；`output_blocked` 明确不收生成费，`output_policy_violation` 可能仍按审核计费规则处理。
- 逐字报文只有 AIHubMix 用户早期那一份，2.5 的实测只有转录（`out-reference/aihubmix/response-shapes.md` §0）。
