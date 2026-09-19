# 第二阶段渠道事实台账（按渠道分节）

- **性质**：**本仓库自己的汇总登记**（渠道事实的单一出处），**不是平台接口合同**；运行中服务不读取本文。只记录**渠道事实与来源**，不记录密钥值、不保存真实图片 URL、task id、签名参数。
- **位置说明（2026-09-19 从 `out-reference/` 迁入 `docs/facts/`）**：本文原先放在 `out-reference/`，那是错的——那里按定义只放「**来自本仓库之外的上游或第三方材料**」（原始证据：官方文档、Schema 快照、调用响应）。而本文是**汇总登记**：它把散在各 `out-reference/<provider>/` 里的原始证据归纳成「哪些事实已结清、依据是什么、本平台据此决定什么」，并含**平台侧的判断**（如「`quality` 位置差异属 ②」）。**原始证据仍在 `out-reference/`，本文只引用不复述全文。**
- **来源**：用户（项目运营方）2026-09-19 直接给出的渠道信息，以及据此的本机只读抓取与经用户授权的实测调用（原始样本见 §2.2、§2.6 所引文件）。
- **纪律（本文的写法）**：**渠道之间分开记录，不做跨渠道合并结论。** 依据 `docs/design/0004-layered-architecture.md` §2 **R1**：上游的同步/异步、Base64/URL、**token/金额**等差异**全部属于 ② Adapter Driver 的内部实现**，一个渠道一族、各自独立；不得把两家并成一张表去推导「平台级」结论。本文此前犯过这个错，已重写。

## 1. 用户给出的渠道（原文口径）

| 渠道 | 文档 | API Base URL |
| --- | --- | --- |
| AIHubMix | `https://api.inferera.com/model/gpt-image-2.5-sunburst/llms.txt`<br>`https://api.inferera.com/model/gpt-image-2.5-flare/llms.txt` | `https://api.inferera.com/v1` |
| APIMart | `https://docs.apib.ai/cn/api-reference/images/gpt-image-2.5/generation.md` | **`https://api.apib.ai/v1`** |

**Base URL 以用户指定为准，不得再当成未决项。** 此前本文把「APIMart 两份文档正文里写 `api.apimart.ai`，而用户给的是 `api.apib.ai`」记成待确认并重复追问——**这是错的**。用户已两次明确指定，平台固化该值即可。

### 1.1 凭证（只记变量名）

| 变量名 | 本机实际情况（2026-09-19） |
| --- | --- |
| `AIHUBMIX_API_KEY` | User 级与 Machine 级**均存在**（51 字符），本会话**已成功取到并用于一次经授权的实测调用** |
| `DOUBAO_API_KEY` | User 级与 Machine 级均存在；属火山方舟，与本阶段无关 |
| `APIMART_API_KEY` | **不存在**（User/Machine/进程均为空） |

**说明**：Agent 进程默认环境里读不到，但可用 `[Environment]::GetEnvironmentVariable(name,'User'|'Machine')` 取到——**「读不到」此前被写成部署障碍，是措辞错误**。

## 2. AIHubMix

> 适用层级：② Adapter Driver（代码）／③ Profile（Schema 数据）／④ Offering（供给）／⑤ Price（价格）。

### 2.1 端点（三个，来自 `llms.txt`）

| 端点 | 形态 | 本仓库现状 |
| --- | --- | --- |
| `POST /v1/images/generations` | **同步**，OpenAI 兼容 | **第一阶段即用它**（bootstrap `adapter_key: aihubmix-image-v1`） |
| `POST /v1/images/edits` | **同步**，OpenAI 兼容，multipart | 同上 |
| `POST /ai/v1/images/generations` | **默认同步**；`async: true` 转任务式 | 已实测（见 2.3） |

- 免鉴权机器 Schema：`https://aihubmix.com/call/schema/models/{model}/endpoints`。
- 输出 URL **约 30 分钟**过期，下载需带同一 `Authorization: Bearer`。

### 2.2 同步 `/v1` 的真实响应（第一阶段的付费实测样本，已入库）

`out-reference/aihubmix/gpt_image_2_generations.json`（HTTP 200，耗时 21.05 秒）：

```json
{ "created": 1785485861, "background": "opaque", "output_format": "png",
  "quality": "low", "size": "1024x1024",
  "data": [ { "b64_json": "<2,096,080 字符 base64 PNG>" } ],
  "usage": { "input_tokens": 13,
             "input_tokens_details":  { "image_tokens": 0,   "text_tokens": 13 },
             "output_tokens": 196,
             "output_tokens_details": { "image_tokens": 196, "text_tokens": 0 },
             "total_tokens": 209 } }
```

**四分项 → 领域 `TokenUsage` 的映射（② 的归一职责）**：

| 响应字段 | `TokenUsage` |
| --- | --- |
| `usage.input_tokens_details.text_tokens` | `input_text_tokens` |
| `usage.input_tokens_details.image_tokens` | `input_image_tokens` |
| `usage.output_tokens_details.text_tokens` | `output_text_tokens` |
| `usage.output_tokens_details.image_tokens` | `output_image_tokens` |
| `usage.total_tokens` | 一致性校验 |

**按费率算一次**（microusd，`1_000_000 microusd = $1`）：

```
13×5,000,000 + 0×8,000,000 + 0×10,000,000 + 196×30,000,000 = 5,945,000,000
÷ 1,000,000（向上取整） = 5,945 microusd = $0.005945
```

### 2.3 异步 `/ai/v1` 的真实响应

> ⚠️ **本节原写于 2026-09-19，是一次重复验证——第一阶段（2026-09-18）已完成更完整的同类实测。**
> 权威记录在 `gpt-image-2-inferera-research.md` **§13.1**：三个场景（纯文生图 / 单图输入 / 图像+alpha mask）各一次真实付费调用，**创建与详情的 `usage` 均为「无」**；任务列表项只有 `id/object/model/status/output/error/created_at/completed_at/expires_at`，**无 prompt、无 metadata、无 correlation ID、无 usage**。
> §13.3 第 6 条已作出结论：`/ai/v1` 保留为 Adapter 已验证能力，**在提供可关联 usage/账单证据前，不发布为正式计费 Offering 的执行路径**。
> 本节 2026-09-19 的实测**与该结论一致，不构成新发现**，仅补了 2.5 之前的一个样本。留此以供交叉核对，**结论归因于第一阶段**。

**创建**（`{"model":"gpt-image-2","prompt":"…","n":1,"async":true}`，HTTP 200）：

```json
{"completed_at":null,"created_at":1789804016,"error":null,"expires_at":null,
 "id":"t_<…已脱敏…>","model":"gpt-image-2","object":"image",
 "output":[],"status":"pending"}
```

**轮询**（`GET /ai/v1/images/{id}`，HTTP 200，约 12 秒后终态）：

```json
{"completed_at":1789804028,"created_at":1789804016,"error":null,"expires_at":1789811227,
 "id":"t_<…已脱敏…>","model":"gpt-image-2","object":"image",
 "output":[{"b64_json":null,
            "content_url":"https://aihubmix.com/ai/v1/images/<id>/content/res_…",
            "index":0,"type":"file"}],
 "status":"completed"}
```

**这次实测确认的**：

1. 任务对象**只有 9 个字段**：`id`、`object`、`model`、`status`、`output`、`error`、`created_at`、`completed_at`、`expires_at`；
2. **任务对象里没有 `usage`**——全文检索 `"usage"` **0 次**；对历史任务列表（3 条已完成任务）检索同样为 0；
3. `output[]` 项为 `{index, type, content_url, b64_json}`，本次 `b64_json: null`，只给 `content_url`；
4. 状态：受理即 `pending`，十几秒内 `completed`；
5. **`quality` 不是 `/ai/v1` 的顶层参数**：顶层传它被硬拒 ——
   `HTTP 400 {"error":{"code":"schema_violation","message":"Unknown request parameter: \`quality\`.","type":"invalid_request_error"}}`；
   ⇒ 必须放进 `extra`（与本地 Schema 快照一致）。**未知参数是硬拒绝，不静默接受**（与火山方舟相反）。

**尚未测**：本次用的是 `gpt-image-2`（已退役），**未对 `gpt-image-2.5-flare`/`-sunburst` 做异步调用**；2.5 的异步任务对象形状是否相同**未验证**。

**本渠道对第二阶段的直接含义**：**关键区分不是「哪个端点」，而是「同步还是异步」**。

| 返回形态 | 端点 | 计量 |
| --- | --- | --- |
| **同步**响应体（`data[0].b64_json`） | `/v1/images/generations`、`/v1/images/edits` | **有**四分项 `usage` |
| **任务对象**（`id` + 轮询 `/ai/v1/images/{id}`） | 仅 `/ai/v1/images/generations`（`async: true`；权威 Schema 中**只有它**声明 `async` 且 `supports_async=true`，`/v1/*` 未声明异步） | **无** `usage` |

⇒ **任务式返回不带计量，是「任务对象」这种格式本身的性质，换端点也一样**（只要能异步，返回的就是任务对象）。第一阶段 §13.3 第 4 条据此选定 `/v1`：**其成功响应提供可审计 token usage，能完成 Price Snapshot + Metering Evidence 结算**。
因此仓库现有计费路径（`TokenUsage` 四分项）**只与同步 `/v1` 相容**。

### 2.4 AIHubMix 的费率（四档）

| 计费项 | 单价 |
| --- | --- |
| 文本输入 | `$5 / 1M tokens` |
| 图像输入 | `$8 / 1M tokens` |
| 文本输出 | `$10 / 1M tokens` |
| 图像输出 | `$30 / 1M tokens` |

与 `crates/domain/src/lib.rs:345-348` 的默认 `PriceRates` **逐项一致**。

**措辞提醒**：`llms.txt` 里写 `Pricing: per-generation`，而模型页给的是 token 单价表——**以模型页的四档 token 单价为准**。

### 2.5 `quality` 的位置按**端点族**分辨（权威 Schema 实测，2026-09-19）

来源：`GET https://api.inferera.com/call/schema/models/gpt-image-2.5-flare/endpoints`（HTTP 200，9442 bytes，免鉴权）。注意 `aihubmix.com` 在本机不可达，同一 Schema 在 `api.inferera.com` 上取到。

| 端点 | `additionalProperties` | 顶层参数 | `quality` 位置 |
| --- | --- | --- | --- |
| `/ai/v1/images/generations` | `false` | `async, extra, image, images, mask, model, n, output_format, prompt, size, webhook_events_filter, webhook_url` | **在 `extra` 内**（`extra` 含 `background, moderation, output_compression, quality, user`） |
| `/v1/images/generations` | `false` | `model, n, output_format, prompt, quality, size` | **在顶层** |
| `/v1/images/edits` | `false` | `image, mask, model, n, output_format, prompt, quality, size` | **在顶层** |

`extra.quality` 与顶层 `quality` 的取值集合相同：`low` / `medium` / `high` / `xhigh` / `max` / `auto`（默认 `auto`）。

⇒ **同一个 `quality`，端点族不同则位置不同**：`/ai/v1` 走 `extra`，OpenAI 兼容的 `/v1/*` 走顶层。这与 2.3 的实测 400 一致（在 `/ai/v1` 顶层传 `quality` 被硬拒）。**参数名不变，只是位置不同**（`0004` R1：位置差异属 ② 内部实现）。

### 2.6 2.5 两款在同步 `/v1` 上的实测（2026-09-19，经用户授权）

这是**第二阶段真正的增量**：第一阶段测的是 `gpt-image-2`（已退役），2.5 此前从未做过付费调用。

`POST https://api.inferera.com/v1/images/generations`，两次 body 除 `model` 外完全相同（`prompt`、`n=1`、`size=1024x1024`、`quality=low`、`output_format=png`）：

| 模型 | 结果 | 顶层字段 | `usage` |
| --- | --- | --- | --- |
| `gpt-image-2.5-sunburst` | HTTP 200，245,138 bytes，19.8 秒 | `created, background, data, output_format, quality, size, usage` | 见下 |
| `gpt-image-2.5-flare` | HTTP 200，321,146 bytes，14.7 秒 | **同上（字段集合一致）** | 见下 |

两次 `usage` **逐项相同**：

```json
{ "input_tokens": 14,
  "input_tokens_details":  { "image_tokens": 0,   "text_tokens": 14 },
  "output_tokens": 196,
  "output_tokens_details": { "image_tokens": 196, "text_tokens": 0 },
  "total_tokens": 210 }
```

**结论**：**2.5 两款在同步 `/v1` 上与 `gpt-image-2` 完全同构**——顶层字段集合相同、`usage` 同为四分项 + `total_tokens`、`quality` 顶层传入即被接受（无需 `extra`）。⇒ **② 的同步解码器不需要为 2.5 新建分支**。

**计费**（两次相同）：`14×$5 + 0×$8 + 0×$10 + 196×$30` per 1M → **5950 microusd = $0.005950**。**响应里没有金额字段**，只有 token。

### 2.6b 2.5-sunburst 的完整响应样例

```json
{ "created": 1789804584, "background": "opaque", "output_format": "png",
  "quality": "low", "size": "1024x1024",
  "data": [ { "b64_json": "<244,700 字符 base64 PNG>" } ],
  "usage": { "input_tokens": 14,
             "input_tokens_details":  { "image_tokens": 0,   "text_tokens": 14 },
             "output_tokens": 196,
             "output_tokens_details": { "image_tokens": 196, "text_tokens": 0 },
             "total_tokens": 210 } }
```

（flare 那次 `b64_json` 为 320,708 字符，其余字段同形。）
- **`quality` 顶层传入即被接受**（无需 `extra`）——与 2.2b 的 Schema 一致

响应（`b64_json` 截断）：

```json
{ "created": 1789804584, "background": "opaque", "output_format": "png",
  "quality": "low", "size": "1024x1024",
  "data": [ { "b64_json": "<244,700 字符 base64 PNG>" } ],
  "usage": { "input_tokens": 14,
             "input_tokens_details":  { "image_tokens": 0,   "text_tokens": 14 },
             "output_tokens": 196,
             "output_tokens_details": { "image_tokens": 196, "text_tokens": 0 },
             "total_tokens": 210 } }
```

### 2.7 本渠道独立待办

- ~~2.5 两款在同步 `/v1` 上是否同构~~ → **已测（2.6）：两款均同构** ✅
- ~~`gpt-image-2.5-flare` 单独调用~~ → **已测（2.6）** ✅
- 2.5 两款在**异步 `/ai/v1`** 上是否同样无 `usage` —— **未测**，且**不必测**：任务对象格式本身不带计量已由第一阶段 §13.1 三场景证实，且 `/ai/v1` 已判定不作为正式计费路径；
- 带参考图的编辑路径（`/v1/images/edits`，multipart）—— 第一阶段在 `gpt-image-2` 上测过（图片输入 1024 tokens），**2.5 未测**。

### 2.8 发布 2.5 供给所需的渠道侧事实（就绪清单）

供发布 ③ Profile / ④ Offering / ⑤ Price 时取用。**只列渠道侧已确认的事实**；发布动作本身属实现范围。

**Profile（③）与 `gpt-image-2` 的差异**（来源：本地机器 Schema 快照，2026-09-19 抓取）：

| 项 | `gpt-image-2` | 2.5 两款 |
| --- | --- | --- |
| `model.const` 字面值 | `gpt-image-2` | **`gpt-image-2.5-flare` / `gpt-image-2.5-sunburst`**（各与 `native_model_id` 同名） |
| `extra.quality` 取值 | `low`/`medium`/`high` | **新增 `xhigh`、`max`**，共 `low`/`medium`/`high`/`xhigh`/`max`/`auto`（默认 `auto`） |
| `extra.moderation` | **无** | **有**（`auto`/`low`） |
| `extra.background` | 有 | 有 |
| `n` | `min 1, max 10` | `min 1, max 10`（**同**） |

**Offering（④）参数**：

| 项 | 值 |
| --- | --- |
| `provider_kind` | `AIHubMix` |
| `adapter_key` | `aihubmix-image-v1`（**沿用现有 Driver**，2.5 同构故不新增） |
| `base_url` | `https://api.inferera.com`（Channel 字段，不写死） |
| `credential_env` | `AIHUBMIX_API_KEY` |
| `provider_model_id` | `gpt-image-2.5-flare` / `gpt-image-2.5-sunburst` |
| 执行端点 | `/v1/images/generations`（文生图）、`/v1/images/edits`（图生图/编辑）——**同步** |
| 参数位置（② 负责） | `quality` 在 `/v1/*` **顶层** |

**Price（⑤）**：

| 计费项 | 单价 |
| --- | --- |
| 文本输入 | `$5 / 1M tokens` |
| 图像输入 | `$8 / 1M tokens` |
| 文本输出 | `$10 / 1M tokens` |
| 图像输出 | `$30 / 1M tokens` |

与 `crates/domain/src/lib.rs:345-348` 的默认 `PriceRates` 逐项一致。**响应不返回金额**，结算按 `usage` 四分项 × 上述单价计算（已用真实响应验证：14 文本输入 + 196 图像输出 → $0.005950）。

**本渠道就绪判定**：③④⑤ 所需事实**齐备**；② 沿用现有 Driver（2.5 实测同构）——**但存在一处发布阻塞，见 2.9**。

### 2.9 ⚠️ 发布阻塞：Adapter 未声明 `moderation`，2.5 的 Profile 会被发布期拒绝

**事实链**（三处均已核对）：

1. 2.5 的机器 Schema **含 `extra.moderation`**（`auto`/`low`），`gpt-image-2` **不含**；
2. `crates/adapter-aihubmix/src/lib.rs` 的 `AdapterDescriptor` 声明：
   `supported_extra_parameters: &["quality", "background", "output_compression", "user"]` —— **没有 `moderation`**；
3. `crates/application/src/lib.rs:504-519` 的 `validate_adapter_compatibility` 会逐项检查 `capability_schema.extra.properties` 的键，**不在该列表内即返回 `Validation`，发布失败且不产生 revision 行**。

⇒ **直接拿 2.5 的机器 Schema 当 Profile 发布，会被拒。** 这不是渠道问题，是**平台侧的 Driver 声明面落后于上游能力**。

**两条路（各自动作与代价）**：

| | 做法 | 动作 | 代价 |
| --- | --- | --- | --- |
| **A** | 把 `moderation` 加进 `supported_extra_parameters` | **改代码**（`crates/adapter-aihubmix`），新 Driver 版本 | 本阶段的改动面从"纯数据发布"变为**含代码发版**；但该参数确由上游支持，属如实声明 |
| **B** | 维持 Adapter 不动，2.5 的 Profile **不声明 `moderation`** | 纯数据发布 | Profile 比上游能力**窄**；请求带 `moderation` 会在受理前被拒（比"声明了却不发"更安全）；需按 `0002` §5 记录「上游 Schema 与本地 Profile 的差异」 |

**注意**：`quality` **不是**阻塞项——它已在列表内，2.5 只是**拓宽取值**（新增 `xhigh`/`max`），发布期只看键名不看取值集合。

**决定（2026-09-19）：本阶段走 B**——Adapter 不动，2.5 的 Profile 不声明 `moderation`。理由：`#2` 的核心命题是「同一 Vendor Model 多 Offering 路由」，不是"支持 `moderation`"；B 使 2.5 供给可发布且零代码改动，`moderation` 留作后续能力扩展。

**B 有一个必须记住的后果**：带 `moderation` 的请求会在**受理前**被 Profile 校验拒绝（这是**更安全**的方向——不会出现"声明了却不发"）。差异需按 `0002` §5 记录：上游 Schema 与本地 Profile 不同，属**有证据的本地收窄**。

### 2.10 已生成的发布素材

| 文件 | 内容 |
| --- | --- |
| `config/bootstrap/aihubmix-gpt-image-2.5-flare.json` | AIHubMix → `gpt-image-2.5-flare` 的 Profile + Offering + Price（草案） |
| `config/bootstrap/aihubmix-gpt-image-2.5-sunburst.json` | 同上，`sunburst` |

已在生成时逐项模拟 `validate_adapter_compatibility`：**两者都会通过发布校验**（对照验证：若按 2.5 原始 Schema 带上 `moderation`，会被拒并给出 `adapter AIHubMix does not support native parameter extra.moderation`）。

**两个前置（文件内 `_status` 已写明）**：

1. **未获「执行实现」授权前不得用于生产**；
2. 这两个文件用的是规划 §3.1 的**新形状**（`price_plan`），而**当前实现是扁平的 `rates` + `price_source_url`** ⇒ **代码支持新形状前，它们无法直接发布**。这是 B 的第二个后果：素材可以现在备好，但发布要等形状落地。

### 2.11 ⚠️ 既有配置的一处实质缺陷（未擅自修改）

`config/bootstrap/aihubmix-gpt-image-2.json`（**现在在跑的配置**）里：

```json
"rates": { ..., "text_output_microusd_per_million": 0 }
```

**文本输出的费率是 0**，而 AIHubMix 公开单价是 **$10 / 1M tokens**（用户 2026-09-19 亦确认该四档）。

**后果**：若某次响应出现 `output_tokens_details.text_tokens > 0`，平台**不会对这部分计费**——即**少收费**。第一阶段实测样本里该项恰为 0，所以没暴露。

**我没有改它**：费率属运营定价，改 `gpt-image-2` 的费率不在本阶段范围内，且改生效配置需要你的决定。**仅记录，待你定。**

（新生成的 2.5 两个文件按 **$10** 写，与此处不同。）

## 3. APIMart

> 同样适用 ②③④⑤；与本文件的 AIHubMix 各节**互不推导**。

### 3.1 端点

| 端点 | 形态 |
| --- | --- |
| `POST /v1/images/generations` | **异步**，立即返回 `task_id` |
| `GET /v1/tasks/{task_id}` | 任务查询（可选 `?language=`，仅影响 `error.message`） |
| `POST /v1/uploads/images` | 上传本地图以取得可用 `url` |

**提交响应**：`{"code":200,"data":[{"status":"submitted","task_id":"task_…"}]}` —— **`data` 是数组，读 `data[0].task_id`**。

### 3.2 请求参数（`gpt-image-2.5`）

`model`（`gpt-image-2.5-flare` / `gpt-image-2.5-sunburst`）、`prompt`、`size`（`auto` + 15 比例 + 精确像素）、`resolution`（`1k/2k/4k`）、`quality`（`low/medium/high/xhigh/max/auto`，默认 `auto`）、`n`（`1~4`）、`output_format`、`output_compression`、`background`、`moderation`（默认 `low`）、`image_urls`（≤16，**仅公网可访问 URL**）。

### 3.3 任务成功响应（**2026-09-19 实测结清**）

**实测原文**（`gpt-image-2.5-flare`、`n=1`、`size=1:1`、`resolution=1k`、`quality=low`；原始记录见 `out-reference/apimart/controlled-probe-2026-09-19.json`）：

```json
{ "code": 200, "data": {
    "id": "task_…", "status": "completed", "progress": 100,
    "cost": 0.00476, "credits_cost": 0.0476,
    "created": 1789806894, "completed": 1789806901,
    "actual_time": 7, "estimated_time": 60,
    "result": { "images": [ { "url": ["<已脱敏>"], "expires_at": 1789893301 } ] },
    "usage": {
      "input_tokens": 14,
      "input_tokens_details": { "cached_tokens": 0, "image_tokens": 0, "text_tokens": 14 },
      "output_tokens": 196,
      "output_tokens_details": { "image_tokens": 196, "text_tokens": 0 },
      "total_tokens": 210
    }
} }
```

**V1 的答案（Gate 前提，已结清）**：

1. **任务成功响应含四分项 `usage`**，且 `input_tokens_details` **区分 `text_tokens` 与 `image_tokens`**（另有 `cached_tokens`）；
2. ⇒ **无需扩展领域类型**：本渠道与 AIHubMix 的响应都能归一成既有 `TokenUsage`（`input_text` / `input_image` / `output_text` / `output_image`）；
3. ⇒ `docs/adr/0012`（金额型证据）**本阶段不需要**；
4. **计价维度与 AIHubMix 相同**：单价 `$5 / $8 / $10 / $30` per 1M；本次 `cost = 0.00476 USD`，与按公开单价算出的 `0.00595` 存在差额，属**账号折扣**（见 `out-reference/apimart/billing-basis.md` §2），不作为结算依据。

**此前记录的文档不一致**（生成页样例有 `usage`、`tasks/status.md` 样例无 `usage`）**已由实测结清**：实际响应**有**四分项 `usage`，粒度比两份文档样例都更细（文档只给聚合三项）。

### 3.4 `Idempotency-Key`（机器 Schema 结清，V4 有答案）

`GET /v1/models/gpt-image-2.5-flare/schema` 返回（原始快照 `out-reference/apimart/schema-gpt-image-2.5-flare.input.json`）：

```json
"idempotency": { "header": "Idempotency-Key", "replay_header": "Idempotency-Replayed",
                 "required": false, "retention_seconds": 86400, "scope": "api_key_endpoint" }
```

⇒ **该端点的幂等键被平台明确定义**（保留 24 小时、作用域为 api_key+endpoint、可选的 `required:false`）。**结论**：创建请求失联后**具备安全重试的机制基础**；但本阶段仍按 `0002` §7 的「创建请求绝不重发」执行——是否启用幂等重试属**后续工作项**，不作为本阶段的处置路径。

**同一 Schema 的另外两条事实**：

- `model.const = "gpt-image-2.5-flare"`（与 `native_model_id` 同名，发布期校验可过）；
- `additionalProperties: true`，且 `quality` / `size` 只声明为 `type: string`**无取值约束**（`required` 仅 `model`，另有 `anyOf: prompt | messages`）。⇒ **未知字段不被此 Schema 拒绝**，与 AIHubMix 的 `additionalProperties: false` 相反。

### 3.5 机器可读证据（已入库）

| 文件 | 内容 |
| --- | --- |
| `out-reference/apimart/catalog-models.json` | `GET /v1/models` 目录（8 条图像模型，含 `owned_by`、`category`、`supported_endpoint_types`） |
| `out-reference/apimart/schema-gpt-image-2.5-flare.input.json` | `GET /v1/models/{model}/schema` 的输入合同 + 幂等声明 + 响应契约版本 |
| `out-reference/apimart/controlled-probe-2026-09-19.json` | 本次受控实测的请求与终态响应（已脱敏） |

⚠️ 注：`api.apib.ai` 与文档正文示例里的 `api.apimart.ai` 指向同一套服务；**平台固化使用用户指定的 `https://api.apib.ai/v1`**。

### 3.6 本渠道独立待办

- ~~`usage` 实际是否存在~~ → **实测：存在**（§3.3）✅
- ~~分项粒度~~ → **实测：四分项，含 `cached_tokens`**（§3.3）✅
- ~~`cost` / `credits_cost` 与 `usage` 的关系~~ → 响应同时给出，`cost` 为实际扣费；差额属账号折扣（§3.3）✅
- ~~`Idempotency-Key` 是否定义~~ → **机器 Schema 明确声明**（§3.4）✅
- **异步状态取值集合** —— 本次实测见到的终态为 `completed`；完整集合仍以两份文档的**并集**处理（未知取值继续轮询，不得当失败）。**未逐一实测**，属 ② 层实现时按并集容错即可，不阻塞；
- **`image_urls` 图生图路径** —— **已实现**（见 §3.7），但**没有任何真实计费实测**；
- **`sunburst` 型号** —— 未测（目录中已确认在册，`endpoint_types` 与 flare 相同）。

### 3.7 参考图与遮罩：必须先上传（**文档已结清，未做真实计费实测**）

原始材料：`out-reference/apimart/uploads-images.cn.md`（上传页，2026-09-19 抓取）。

| 事实 | 值 |
| --- | --- |
| 上传端点 | `POST /v1/uploads/images`，`multipart/form-data`，字段名 `file` |
| 接受格式 / 上限 | JPEG、PNG、WebP、GIF；单张 ≤ **20MB** |
| 返回 | `{url, filename, content_type, bytes, created_at}`；`url` 有效期 **72 小时** |
| 生成请求里的参考图 | `image_urls`：**字符串数组**（≤16，单张 ≤20MB、总计 ≤256MB），**只接受公网可访问 URL** |
| 遮罩 | `mask_url`（字符串），必须与 `image_urls` 同用，尺寸须与第一张参考图一致 |

**两处文档冲突，平台的取法**：

1. 上传页的 Python 示例把 `image_urls` 写成 `[{"url": …}]`（对象数组）。机器 Schema、生成页字段说明与三个示例、`gpt-image-2.5` 中文页**都指向字符串数组** ⇒ 平台发**字符串数组**（`out-reference/apimart/uploads-images.cn.md` 末尾备注）。
2. 上传页声明生成接口**不再接受 base64**，生成页仍写「支持 `base64 data URI`、可与 URL 混填」⇒ 平台取**更严的一侧**：一律先上传换 URL，不在生成请求里塞 base64。

**平台侧决定（全在 ② 层，不外泄）**：参考图/遮罩在提交生成任务**之前**先上传换 URL；上传失败＝生成任务**可证明未受理**（`SafeBeforeAcceptance`，`docs/adr/0011`）⇒ Job `failed` + 释放预授权，**不进对账**（与"提交后失联"是两条路径）。

**发布状态：这两条分支尚未开放。** 两个 `config/bootstrap/apimart-gpt-image-2.5-*.json` 的 `allowed_branches` 目前只有 `prompt_only`。按 `docs/adr/0002`「未证实的参数不开启，经真实 wire 验证后再以新 Schema 修订发布」，要放开需要一次经用户授权的受控调用，先把上面两处冲突与上传接口的真实行为验掉。链路本身（上传 → 回填 URL → 提交）已实现，并有假上游端到端用例。

**参数名不改写**：生成请求用上游原生名 `image_urls` / `mask_url`，`AssetBinding.native_parameter_path` 就是这些原生参数路径（`/image_urls/0`、`/mask_url`）；平台**不**把它改名成 `images`。依据 `docs/adr/0002`（"若某厂商不使用 `image` 这个字段名，由该厂商自己的 Schema 声明原生字段路径"）。平台只在**一处**判定"这个参数装的是参考图还是遮罩"：名字以 `image` 开头＝参考图、含 `mask`＝遮罩、其余一律拒绝（发布期与运行期共用同一个函数）。

**未做**：本渠道的图生图路径**没有任何真实计费实测**（未获授权）；实现只由**进程内假上游**端到端验证。

## 4. 不跨渠道合并（原写法的更正）

本文此前写过「两家响应形状对比表」「一个公式套不了两家」「证据形状不同所以要改领域」等跨渠道结论——**那些都是把渠道差异往上抬，违反 `0004` R1，已删除**。

正确做法：**每个渠道各自的 ② Driver 负责把它自己的响应归一成领域形状**（`TokenUsage`），差异留在各自的 Driver 与其测试里，不进入 ①③④⑤，也不互相推导。

## 5. 本次实测的调用记录（留档）

### 5.1 异步实测（2026-09-19，经用户授权）

| 项 | 值 |
| --- | --- |
| 渠道 / 端点 | AIHubMix `POST https://api.inferera.com/ai/v1/images/generations`（异步） |
| 授权 | 用户 2026-09-19 明确指示「AIHubMix 异步实测一个试试」 |
| 计费调用次数 | **2 次提交**：第 1 次因 `quality` 顶层不合法被拒（HTTP 400，**未受理**）；第 2 次受理并完成 |
| 请求参数 | `model=gpt-image-2`、`n=1`、`size` 未传、`async=true`、`quality` 未传 |
| 结果 | `status: completed`，约 12 秒 |
| 未做 | 未下载结果图（仅读响应结构）；未测带图编辑 |

### 5.2 同步实测（2026-09-19，经用户授权）

| 项 | 值 |
| --- | --- |
| 渠道 / 端点 | AIHubMix `POST https://api.inferera.com/v1/images/generations`（同步） |
| 授权 | 用户 2026-09-19 明确指示「可以同步实测下」 |
| 调用次数 | **2 次**（`gpt-image-2.5-sunburst`、`gpt-image-2.5-flare`），除 `model` 外参数相同 |
| 请求参数 | `n=1`、`size=1024x1024`、`quality=low`、`output_format=png` |
| 结果 | 两次均 **HTTP 200**；sunburst 245,138 bytes / 19.8 秒，flare 321,146 bytes / 14.7 秒；四分项 `usage` 齐全且逐项相同（见 2.6） |
| 计费 | 每次按四档费率算得 **5950 microusd = $0.005950**（上游未返回金额，需自行计算） |
| 未做 | 未把结果图写入仓库（`b64_json` 仅看长度与前缀）；未测带图编辑 |

### 5.3 APIMart 受控实测（2026-09-19，经用户授权）

| 项 | 值 |
| --- | --- |
| 渠道 / 端点 | APIMart `POST https://api.apib.ai/v1/images/generations`（异步）+ `GET /v1/tasks/{id}` |
| 授权 | 用户 2026-09-19 提供 `APIMART_API_KEY` 并授权决定后续 |
| 计费调用次数 | **提交 1 次**（另 1 次轮询为只读） |
| 请求参数 | `model=gpt-image-2.5-flare`、`n=1`、`size=1:1`、`resolution=1k`、`quality=low` |
| 结果 | 提交 HTTP 200（`status: submitted`）；10 秒后终态 `completed`，`actual_time=7` |
| 计量 | **四分项 `usage`**：输入文本 14 / 输入图片 0 / 输出图片 196 / total 210（另含 `cached_tokens`） |
| 计费 | 自报 `cost = 0.00476 USD`、`credits_cost = 0.0476`；按公开单价算为 `0.00595`，差额属账号折扣 |
| 未做 | 未下载结果图（URL 已脱敏）；未测 `sunburst`；未测 `image_urls` 图生图 |
| 原始记录 | `out-reference/apimart/controlled-probe-2026-09-19.json` |

### 5.4 累计

本日经用户授权的计费提交共 **5 次**（AIHubMix 4 次 + APIMart 1 次）：

| # | 渠道 / 端点 | 结果 | 是否必要 |
| --- | --- | --- | --- |
| 1 | AIHubMix `/ai/v1`（异步） | HTTP 400 **未受理**（顶层 `quality` 非法） | 顺带确认了参数位置 |
| 2 | AIHubMix `/ai/v1`（异步） | 完成 | **重复验证**——第一阶段 §13.1 已有三场景更完整的同类结论 |
| 3 | AIHubMix `/v1`（同步，sunburst） | 完成 | **必要**（2.5 首次付费验证） |
| 4 | AIHubMix `/v1`（同步，flare） | 完成 | **必要**（补上 flare 缺口） |
| 5 | APIMart `/v1`（异步，flare） | 完成 | **必要**（结清 V1：`usage` 是否存在与粒度） |

全部使用 `n=1`、`quality=low` 的最小配置。**未发起任何火山方舟调用。** 另完成 3 次 APIMart **只读**探测（`/v1/models`、`/v1/models/{model}/schema`、`/v1/usage`），零费用。
