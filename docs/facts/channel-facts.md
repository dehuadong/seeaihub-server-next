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

| 变量名 | 本机实际情况（2026-09-19 复核） |
| --- | --- |
| `AIHUBMIX_API_KEY` | User 级与 Machine 级**均存在**，本会话**已成功取到并用于经授权的实测调用** |
| `DOUBAO_API_KEY` | User 级与 Machine 级均存在；属火山方舟，与本阶段无关 |
| `APIMART_API_KEY` | User 级与 Machine 级**均存在**（此处此前写成"不存在"，是错的——当时只看了进程环境） |

**说明**：三个变量在 **User 与 Machine 级都存在**，但 Agent 进程默认环境里读不到；可用 `[Environment]::GetEnvironmentVariable(name,'User'|'Machine')` 取到——**「读不到」此前被写成部署障碍或"凭证不存在"，都是措辞错误**。

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
> 权威记录在 `docs/research/gpt-image-2-inferera-research.md` **§13.1**：三个场景（纯文生图 / 单图输入 / 图像+alpha mask）各一次真实付费调用，**创建与详情的 `usage` 均为「无」**；任务列表项只有 `id/object/model/status/output/error/created_at/completed_at/expires_at`，**无 prompt、无 metadata、无 correlation ID、无 usage**。
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

按 **Tokens 计费**（上游口径）：

| 计费项 | 单价 |
| --- | --- |
| 文本输入 | `$5 / 1M tokens` |
| 文本输出 | `$10 / 1M tokens` |
| 图像输入 | `$8 / 1M tokens` |
| 图像输出 | `$30 / 1M tokens` |

**这是本渠道的成本价来源**：上游**只返回四分项 token、不返回任何金额字段**（§2.6），所以成本价 = Σ(分项 token × 上表费率)；与 APIMart 不同（那边上游直接声明 `cost`，见 §3.9.1）。

与 `crates/domain/src/lib.rs:345-348` 的默认 `PriceRates` **逐项一致**。

**措辞提醒**：`llms.txt` 里写 `Pricing: per-generation`，而模型页给的是 token 单价表——**以模型页的四档 token 单价为准**。

**缓存**：本阶段按 Tokens 计费，**不考虑缓存档**（§3.9.2）。

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

| 端点 | 形态 | 路由是否存在（2026-09-19 零费用探测） |
| --- | --- | --- |
| `POST /v1/images/generations` | **异步**，立即返回 `task_id` | **存在**（无凭证 401） |
| `GET /v1/tasks/{task_id}` | 任务查询（可选 `?language=`，仅影响 `error.message`） | **存在**（无凭证 401） |
| `POST /v1/uploads/images` | 上传本地图以取得可用 `url` | **存在**（无凭证 401；对照：`/v1/nonexistent-route` 返回 404） |

**探测方法**：对 `https://api.apib.ai` 发**不带任何凭证**的请求，只看 401/404（不触达任何账号、不产生任何费用）。三个真实端点在**我们实际配置的域名**上都存在，不只是文档里写着。

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
4. **计价维度与 AIHubMix 相同**：单价 `$5 / $8 / $10 / $30` per 1M；本次 `cost = 0.00476 USD`，与按公开单价算出的 `0.00595` 差**正好 20%**——2026-09-19 由上游账单面板结清：那是面板自报的 `Group ratio 0.8`（**账号级固定倍率**），不是计量误差，也不作为平台结算依据（详见 §3.9）。

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
- ~~`cost` / `credits_cost` 与 `usage` 的关系~~ → **已结清**：`cost` 是按**折后**账号价的实际扣费，`credits = cost × 10`；折扣是面板自己写明的 `Group ratio 0.8`（§3.9）✅
- ~~`Idempotency-Key` 是否定义~~ → **机器 Schema 明确声明**（§3.4）✅
- **异步状态取值集合** —— 本次实测见到的终态为 `completed`；完整集合仍以两份文档的**并集**处理（未知取值继续轮询，不得当失败）。**未逐一实测**，属 ② 层实现时按并集容错即可，不阻塞；
- **`image_urls` 图生图路径** —— **已受控实测结清**（见 §3.7、§5.6）；
- **`sunburst` 型号** —— 未单独实测（目录中已确认在册，`endpoint_types` 与 flare 相同）；图生图按同渠道族 flare 的实测开放。

### 3.7 参考图与遮罩：必须先上传（**2026-09-19 已受控实测结清**）

原始材料：`out-reference/apimart/uploads-images.cn.md`（上传页，2026-09-19 抓取）；实测记录见 §5.6。

| 事实 | 值 |
| --- | --- |
| 上传端点 | `POST /v1/uploads/images`，`multipart/form-data`，字段名 `file` |
| 接受格式 / 上限 | JPEG、PNG、WebP、GIF；单张 ≤ **20MB** |
| 返回（**实测**） | `{url, filename, content_type, bytes, created_at}`；字段与文档一致；`content_type` 由上游探测（我们传 PNG 得到 `image/png`） |
| 返回的 URL 主机（**实测**） | `getapib.org`（**不是**文档示例里的 `upload.apimart.ai`）；`https`，路径较长且随机；有效期文档写 72 小时 |
| 生成请求里的参考图（**实测**） | `image_urls`：**字符串数组**（≤16，单张 ≤20MB、总计 ≤256MB），只接受公网可访问 URL |
| 遮罩（**实测**） | `mask_url`（字符串）与 `image_urls` **同用可行**；本次用 512×512 带 alpha 的 PNG，尺寸与参考图一致，上游未报任何尺寸/通道错误 |

**两处文档冲突，实测后的结论**：

1. 上传页的 Python 示例把 `image_urls` 写成 `[{"url": …}]`（对象数组）。**实测用字符串数组提交成功并完成出图** ⇒ 平台发**字符串数组**是对的（与机器 Schema、生成页字段说明及三个示例一致）。
2. 上传页声明生成接口**不再接受 base64**，生成页仍写「支持 `base64 data URI`、可与 URL 混填」⇒ 平台取**更严的一侧**：一律先上传换 URL。**本次未实测 base64 是否仍被接受**（不需要，取严的一侧不受影响）。

**带参考图时的计量（实测）**：`usage` 仍是四分项，且 `input_tokens_details.image_tokens` **真的会涨**（512×512 参考图 ⇒ **1024**，与文档口径一致），`text_tokens` 为提示词长度；平台按 `TokenUsage` 归一后结算，与上游自报金额只差固定折扣（见 §3.3、§5.6）。

**平台侧决定（全在 ② 层，不外泄）**：参考图/遮罩在提交生成任务**之前**先上传换 URL；上传失败＝生成任务**可证明未受理**（`SafeBeforeAcceptance`，`docs/adr/0011`）⇒ Job `failed` + 释放预授权，**不进对账**（与"提交后失联"是两条路径）。

**发布状态：三条分支已开放。** 两个 `config/bootstrap/apimart-gpt-image-2.5-*.json` 的 `allowed_branches` 已加上 `image_conditioned` / `masked`（`max_images: 16`）。依据 `docs/adr/0002`「未证实的参数不开启，经真实 wire 验证后再发布新修订」——验证已完成：上传返回、`image_urls` 形态、`mask_url` 同用、以及 `usage.input_image_tokens` 四件事都在**一次真实调用**里结清；另外我们**自己的服务**（API + Worker，真实凭证）也对着真实上游跑通了同一条路径（§5.6）。

**参数名不改写**：生成请求用上游原生名 `image_urls` / `mask_url`，`AssetBinding.native_parameter_path` 就是这些原生参数路径（`/image_urls/0`、`/mask_url`）；平台**不**把它改名成 `images`。依据 `docs/adr/0002`（"若某厂商不使用 `image` 这个字段名，由该厂商自己的 Schema 声明原生字段路径"）与其补充决定（统一参数转换属后期对外消费侧）。平台只在**一处**判定"这个参数装的是参考图还是遮罩"：名字以 `image` 开头＝参考图、含 `mask`＝遮罩、其余一律拒绝（发布期与运行期共用同一个函数）。

**未做**：`sunburst` 的图生图未单独实测（与 flare 同渠道族、同端点、同参数面）；`base64` 路径未测；20MB / 16 张 / 256MB 这些**边界**未逐个压测（只说单张 20MB 上限来自文档，代码里已按此拒绝并另有总量上限）。

### 3.8 错误信封（2026-09-19 零费用探测，无凭证）

无凭证请求真实端点，取到的 401 响应体：

```json
{"error":{"code":"","message":"invalid API key (request id: 20260919182056471923385yBRUUrTx)","param":"","type":"apimart_error"}}
```

两条对实现有影响的事实：

1. **`error.code` 是空字符串**，可用的只有 `type: "apimart_error"` 与 `message`。因此"只依据 `error.code` 分类"在**凭据类失败**上会退化成"受理状态不确定"，把一个明确没进到生成的请求送进人工对账。平台的处置：**凭据/权限类 HTTP 状态（401/402/403）作为兜底信号**判为确定性拒绝；**5xx 仍不看状态码**（`build_request_failed` 会以 500 承载参数错误，那正是这条规则要防的情况）。
2. **每请求标识在失败时也有**：响应头 `X-Oneapi-Request-Id`，同时被写进 `message` 里的 `(request id: …)`。失败路径的 `provider_error_message` 会原样落库，排查时不需要额外取头（**只在失败路径**；成功路径的对账标识是任务式上游的 `task_id`）。

**未做（本节的探测范围）**：这次探测没有带凭证、也没有调用上传接口——那一步后来在用户批准下单独做过，见 §5.6。本节只记**路由与错误信封**的事实。

### 3.9 平台成本价：各渠道怎么得到（2026-09-19，含上游账单面板核对）

> **本阶段只做成本侧**：拿到上游的价格或计算方式，得到**平台成本价**。平台**对外价**（加价、让利）属后期产品决定，本阶段不做——见 §3.9.3 与工作项 [#5](https://github.com/dehuadong/seeaihub-server-next/issues/5)。

#### 3.9.1 两个渠道的成本来源不同（这正是 ② 层各归一的事）

| 渠道 | 成本价从哪来 | 依据 |
| --- | --- | --- |
| **APIMart** | **上游直接声明金额**：任务响应里的 `cost`（USD）。面板写明它 = `Base × Group ratio × Channel ratio × Discount ratio`，`credits_cost = cost × 10` | `out-reference/apimart/controlled-probe-2026-09-19.json`、§5.3/§5.6、上游账单面板 |
| **AIHubMix** | **上游只给 token，金额要自己按费率算**：按 Tokens 计费，文本输入 **$5** / 文本输出 **$10** / 图像输入 **$8** / 图像输出 **$30**，每 1M tokens ⇒ 成本 = Σ(分项 token × 费率) | `docs/facts/channel-facts.md` §2.4/§2.6（同步 `/v1` 响应只有四分项 token，**没有金额字段**） |

⇒ **不需要用 list 再算一遍 APIMart 的成本**：它自己给了数。`list × 倍率` 只是解释"为什么声明的金额低于公开费率"（本账号 Group ratio 0.8），**不是取数路径**；倍率也可能随账号变化，重算反而引入失真。

#### 3.9.2 实测成本（三笔，都是上游口径）

| 渠道 / 调用 | token 分项（文本in / 图片in / 图片out） | **成本价** | 来源 |
| --- | --- | --- | --- |
| APIMart 走我们自己的服务 | 29 / 1024 / 196 | **$0.011374** | 上游 `cost`（= 面板 Actual cost） |
| APIMart curl 直连 | 33 / 1024 / 196 | **$0.011390** | 上游 `cost`（= 面板 Actual cost） |
| APIMart curl 直连（纯文生图） | 14 / 0 / 196 | **$0.004760** | 上游 `cost`（见 §5.3） |
| AIHubMix 同步 `/v1`（2.5 两款） | 14 / 0 / 196 | **$0.005950** | 自算：14×$5 + 196×$30 per 1M（响应无金额字段） |

**两个渠道的共同点**：都有**四分项 token**（`input_text` / `input_image` / `output_text` / `output_image`），所以平台侧的 `TokenUsage` 归一不变；差别只是"上游给不给金额"，留在各自 ② Driver 里（`0004` R1）。

**`cost` 与公开费率的差额**（面板自报口径）：`Base cost` = Σ(分项 token × 费率)；`Actual cost` = `Base × Group ratio(0.8) × Channel ratio(1) × Discount ratio(1)`；`Credits = Actual × 10`。**平台结算基数**用的是 `price_plan` 里的费率 × 真实分项 token（两个 APIMart 素材现在填的是上游公开费率），**平台对外价未定**。逐笔差价见 §3.9.4。

**缓存不参与**：本阶段按 Tokens 计费，**不区分缓存**——不建模缓存档、不为它加字段、也不把它当待办。

#### 3.9.3 平台侧现在怎么用这些数（以及没有做什么）

- 平台结算用的是**已发布 `price_plan` 的费率 × 真实分项 token**。两个 APIMart 素材的 `price_plan` 现在填的是上游公开费率——它现在的角色是**结算基数**，不是"平台对外定价决定"。
- **平台对外价尚未决定**：要不要在基数之上加价、要不要把账号折扣让给消费侧，都是**后期产品决定**（跟踪工作项 [#5](https://github.com/dehuadong/seeaihub-server-next/issues/5)）。本阶段**只固化成本价**。
- 上游声明的 `cost`（APIMart）是**折后账号价**，随账号分组变化；它作为**成本价**是对的，但**不能**反过来当作"可复现的计量事实"去替代分项 token（`docs/adr/0012` 在本阶段作废的原因之一）。

#### 3.9.4 差价（面板账单 vs 我们的记录）

用户提供面板要核对的就是**差价**。逐笔如下（三笔都是同一倍率，**没有其它费用**）：

| 调用 | token 分项（文本in / 图片in / 图片out） | 面板 `Base cost`（= 平台侧 capture / 结算基数） | 上游实收（= **成本价**） | **差价** | 差价率 |
| --- | --- | --- | --- | --- | --- |
| 走我们自己的服务 | 29 / 1024 / 196 | $0.014217（capture 14217 microusd） | $0.011374 | **$0.002843** | 20% |
| curl 直连 | 33 / 1024 / 196 | $0.014237 | $0.011390 | **$0.002847** | 20% |
| curl 直连（纯文生图，§5.3） | 14 / 0 / 196 | $0.005950 | $0.004760 | **$0.001190** | 20% |

**差价的来源只有一个**：面板自报的 `Group ratio 0.8 × Channel ratio 1 × Discount ratio 1`，即**整笔 −20%**；分项逐项算也对得上（`Base = Σ(token × 费率)`，两次面板的每一行都与按公开费率手算的结果一致）。`credits = USD × 10`。

**平台侧的位置**：capture `14217 microusd` 等于面板 `Base cost`（不是实收）——平台侧结算基数用的是公开费率，与上游实收之间就是这 20%。

**AIHubMix 侧目前无法核对差价**：上游不返回任何金额字段，也无从知道它是否给账号折扣（§2.4）。要结清得看它的控制台/账单，不在这几次调用的实测范围内。

**`docs/adr/0012` 因此在本阶段作废**：平台的**计量事实**是四分项 token；金额随账号倍率变化、不可复现，所以结算必须由分项 token 推出，上游声明的金额只用来核成本（本阶段要的正是它）。§5.6 那次真实端到端是这条的实证。

**来源**：用户 2026-09-19 在会话中提供的两张上游控制台"详情"面板截图（含 `task_id` 与 API 密钥标签，故**截图本身不入库**；本表只转录与结算有关的数字与倍率）。

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
| 计费 / 成本 | 上游自报 **`cost = 0.00476 USD`**（这就是本笔成本价）、`credits_cost = 0.0476`；公开费率算得 `0.00595`，差额是上游面板自报的 `Group ratio 0.8`（§3.9） |
| 未做 | 未下载结果图（URL 已脱敏）；未测 `sunburst`；未测 `image_urls` 图生图 |
| 原始记录 | `out-reference/apimart/controlled-probe-2026-09-19.json` |

### 5.4 累计

本日经用户授权的计费提交共 **7 次**（AIHubMix 4 次 + APIMart 3 次）：

| # | 渠道 / 端点 | 结果 | 是否必要 |
| --- | --- | --- | --- |
| 1 | AIHubMix `/ai/v1`（异步） | HTTP 400 **未受理**（顶层 `quality` 非法） | 顺带确认了参数位置 |
| 2 | AIHubMix `/ai/v1`（异步） | 完成 | **重复验证**——第一阶段 §13.1 已有三场景更完整的同类结论 |
| 3 | AIHubMix `/v1`（同步，sunburst） | 完成 | **必要**（2.5 首次付费验证） |
| 4 | AIHubMix `/v1`（同步，flare） | 完成 | **必要**（补上 flare 缺口） |
| 5 | APIMart `/v1`（异步，flare） | 完成 | **必要**（结清 V1：`usage` 是否存在与粒度） |
| 6 | APIMart `/v1`（异步，flare，**带参考图 + 遮罩**） | 完成 | **必要**（结清图生图合同：`image_urls` 形态、`mask_url`、`input_image_tokens`） |
| 7 | 同上，但**走我们自己的 API + Worker** | **`succeeded`** | **必要**（第一次让真实凭证跑通自家全链路：上传 → 生成 → 取图 → 归档 → 结算） |

第 **1–4 次（AIHubMix）**：三次完成（其中 1 次是重复验证）+ 1 次未受理（$0）。两次同步 2.5 各按已发布单价算得 **$0.005950**（上游不返回金额，只能自算）；异步那次上游同样不返回金额，**金额未知**。

第 **5–7 次（APIMart）**：上游三次都有折后实际扣费可对——**$0.004760**（纯文生图，§5.3）+ **$0.011390**（直连图生图）+ **$0.011374**（走自家服务），合计 **$0.027524**。

可核对总额 ≈ **$0.0394**（AIHubMix 2 次 + APIMart 3 次），另加 1 次金额未知的 AIHubMix 异步调用。全部使用 `n=1`、`quality=low` 的最小配置。**未发起任何火山方舟调用。** 另完成 3 次 APIMart **只读**探测（§3.5）与 4 次**无凭证**路由探测（§5.5），零费用。

**成本价来源（两渠道不同）**：**AIHubMix** 上游只给四分项 token，成本价按四档费率自算（§2.4）；**APIMart** 上游在响应里直接声明 `cost`，那就是成本价（§3.9.1）。**平台对外价未定**（后期，[#5](https://github.com/dehuadong/seeaihub-server-next/issues/5)）。逐笔见 §3.9。

### 5.5 零费用路由探测（2026-09-19，**无凭证**）

| 项 | 值 |
| --- | --- |
| 目的 | 确认我们**实际配置的域名**上三个端点真的存在（此前只有文档写着） |
| 方法 | 对 `https://api.apib.ai` 发**不带任何凭证**的请求，只看状态码；不触达任何账号 |
| 次数 | **4 次**（`POST /v1/uploads/images`、`POST /v1/images/generations`、`GET /v1/tasks/nonexistent`、`GET /v1/nonexistent-route` 对照） |
| 结果 | 三个真实端点均 **401**（存在但需鉴权）；对照的不存在路由 **404** ⇒ 路由判定有效 |
| 计费 | **零**（无凭证、未生成、未上传任何文件） |
| 附带事实 | 401 错误信封与失败路径的请求标识（见 §3.8） |
| 未做 | **没有带凭证调用上传接口**，也没有任何生成调用 |

### 5.6 图生图 + 遮罩受控实测（2026-09-19，经用户批准）

| 项 | 值 |
| --- | --- |
| 授权 | 用户 2026-09-19 在本会话明确选择「批准，按这个方案跑」；方案当时写明：上传 1~2 张测试图 + **最多 2 次生成**、`n=1`、`quality=low`、预算上限 $1 |
| 实际用量 | **上传 2 次**（512×512 参考图、512×512 带 alpha 遮罩）+ **生成 2 次**（1 次 curl 直连探合同、1 次走我们自己的 API+Worker） |
| 生成参数 | `model=gpt-image-2.5-flare`、`n=1`、`size=1:1`、`resolution=1k`、`quality=low`、`image_urls=[<上传后的 URL>]`、`mask_url=<上传后的 URL>` |
| 直接调用结果 | 上传 HTTP 200（字段与文档一致）；提交 HTTP 200（`status: submitted`）；11 秒后 `completed`，`cost = 0.01139 USD` |
| 直接调用的计量 | `input_tokens=1057`（`image_tokens=1024`、`text_tokens=33`）、`output_tokens=196`（`image_tokens=196`）、`total=1253` |
| **走我们自己服务的端到端** | 发布真实素材 → 平台接口上传两张图 → 受理 Job（`/image_urls/0` + `/mask_url`）→ 真实 Worker 执行 → **`succeeded`** |
| 端到端计量 | Evidence 记 `input_text=29 / input_image=1024 / output_image=196`（与上游 `usage` 逐项一致） |
| **成本价** | 上游自报 **`cost`**：直连那次 `$0.011390`、走自家服务那次 `$0.011374`（面板的 `Actual cost`，逐笔见 §3.9） |
| 平台侧结算与成本的关系 | capture `14217 microusd` = 面板 `Base cost`（= 公开费率 × 分项 token）；成本价 = 它 × 账号倍率 0.8。差额来自账号倍率，**不是**计量误差（§3.9） |
| 端到端结果 | 结果图归档到自有对象存储：`image/png`、**1,486,934 bytes、1024×1024**；`provider_trace_id` 已落库（真实 task id） |
| 面板核对（**已结清**） | 面板写明 `Base cost = Σ(token × 费率)`、`Actual = Base × Group ratio 0.8`，两次与 `cost` 逐位一致（§3.9） |
| 敏感信息 | **未保存**真实图片 URL 与 task id；上表只记存在性、主机名与长度级信息。原始响应只留在本机临时目录，不入仓库。用户后来提供的上游账单面板含 task id 与密钥标签，**截图同样不入库**，只把结算数字转录进 §3.9 |
| 未做 | 未下载上游结果图（结果图是我们自己服务完成取图后归档的）；未测 `sunburst`；未测 base64；未压测 20MB/16 张/256MB 边界 |

**这一轮调用同时结清了 `docs/verification/phase2-controlled-verification.md` 里列的四条**（上传返回、`image_urls` 形态、图生图可用 + `usage` 变化、`mask_url` 可用），因此两个发布素材据此放开两条分支。
