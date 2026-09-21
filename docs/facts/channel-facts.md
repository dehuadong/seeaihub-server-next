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

> **响应结构台账**：各端点实际返回什么形状、哪一次调用有逐字样本，见 [`out-reference/aihubmix/response-shapes.md`](../../out-reference/aihubmix/response-shapes.md)（外部参考资源，只作证据）。

| 端点 | 形态 | 本仓库现状 |
| --- | --- | --- |
| `POST /v1/images/generations` | **同步**，OpenAI 兼容 | **第一阶段即用它**（bootstrap `adapter_key: aihubmix-image-v1`） |
| `POST /v1/images/edits` | **同步**，OpenAI 兼容，multipart | 同上 |
| `POST /ai/v1/images/generations` | **默认同步**；`async: true` 转任务式 | 已实测（见 2.3） |

- 免鉴权机器 Schema：`https://aihubmix.com/call/schema/models/{model}/endpoints`。
- 输出 URL **约 30 分钟**过期，下载需带同一 `Authorization: Bearer`。

### 2.2 同步 `/v1` 的真实响应（**用户早期采集**的样本，随仓库建立入库）

`out-reference/aihubmix/gpt_image_2_generations.json`（HTTP 200，耗时 21.05 秒）：

> **来源说明（2026-09-20 更正）**：这份样本**不是**本仓库的受控实测——它是**用户自己早期采集**的，响应体里的 `created = 1785485861` ⇒ **2026-07-31 16:17:41 +08:00**，随仓库建立提交 `1fe462a`（"establish independent image generation server"）入库。
> 另外：**响应体没有 `model` 字段**，所以"它来自 `gpt-image-2`"是按文件名与 `out-reference/aihubmix/gpt-image-2.md` **推断**的，报文本身证明不了。

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
> 权威记录原是 `docs/research/gpt-image-2-inferera-research.md` **§13.1**（**该文件已于 2026-09-20 清理删除，提交 `ed140c2`**；`docs/research/` 这个位置本身保留；以下转述其结论）：三个场景（纯文生图 / 单图输入 / 图像+alpha mask）各一次真实付费调用，**创建与详情的 `usage` 均为「无」**；任务列表项只有 `id/object/model/status/output/error/created_at/completed_at/expires_at`，**无 prompt、无 metadata、无 correlation ID、无 usage**。
> §13.3 第 6 条原已作出结论：`/ai/v1` 保留为 Adapter 已验证能力，**在提供可关联 usage/账单证据前，不发布为正式计费 Offering 的执行路径**。
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
   ⇒ 在 `/ai/v1` 那族端点上必须放进 `extra`。**本平台不调用那族端点**：执行路径是同步 `/v1/*`，那里 `quality` 就在顶层（本仓库的发布素材已按顶层声明，不再有 `extra`）。**未知参数是硬拒绝，不静默接受**（与火山方舟相反）。

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

**这是本渠道的成本价来源**：上游**只返回四分项 token、不返回任何金额字段**（§2.6），所以成本价 = Σ(分项 token × 上表费率)；与 APIMart 不同（那边上游直接声明 `cost`，见 §5.1）。

与 `crates/domain/src/lib.rs:345-348` 的默认 `PriceRates` **逐项一致**。

**措辞提醒**：`llms.txt` 里写 `Pricing: per-generation`，而模型页给的是 token 单价表——**以模型页的四档 token 单价为准**。

**缓存**：本阶段按 Tokens 计费，**不考虑缓存档**（§5.2）。

### 2.5 `quality` 的位置按**端点族**分辨（权威 Schema 实测，2026-09-19）

来源：`GET https://api.inferera.com/call/schema/models/gpt-image-2.5-flare/endpoints`（HTTP 200，9442 bytes，免鉴权）。注意 `aihubmix.com` 在本机不可达，同一 Schema 在 `api.inferera.com` 上取到。

| 端点 | `additionalProperties` | 顶层参数 | `quality` 位置 |
| --- | --- | --- | --- |
| `/ai/v1/images/generations` | `false` | `async, extra, image, images, mask, model, n, output_format, prompt, size, webhook_events_filter, webhook_url` | **在 `extra` 内**（`extra` 含 `background, moderation, output_compression, quality, user`） |
| `/v1/images/generations` | `false` | `model, n, output_format, prompt, quality, size` | **在顶层** |
| `/v1/images/edits` | `false` | `image, mask, model, n, output_format, prompt, quality, size` | **在顶层** |

`extra.quality` 与顶层 `quality` 的取值集合相同：`low` / `medium` / `high` / `xhigh` / `max` / `auto`（默认 `auto`）。

**位置差异属于渠道各端点族自己的形态。** 本平台对 AIHubMix 采用的执行路径是**同步**的 `/v1/images/generations` 与 `/v1/images/edits`（§2.6 实测结清），上表第 1 行那条异步面**不使用**——它的包装形态与本平台无关。

**`background` / `output_compression` / `user` / `moderation` 的位置（2026-09-20 定）**：它们在第一方**文档**里是 OpenAI 兼容面的顶层参数（AIHubMix 2.5 的机器 Schema 则把它们放在 `/ai/v1` 那族的 `extra` 内，`gpt-image-2` 没有 `moderation`）。按 [`ADR-0018`](../adr/0018-open-parameters-by-first-party-docs.md)，**文档写明支持的参数直接声明**，不再以"没实测"为由拦截；平台按声明在受理前校验取值。本仓库的 AIHubMix 素材据此把四项声明在**顶层**。

**仍存的一处不一致（如实登记，不在声明上回避）**：上表第 2、3 行（我们实际走的 `/v1/*`）的机器 Schema 只列 `model, n, output_format, prompt, quality, size`（edits 另有 `image, mask`），且 `additionalProperties: false`。也就是说文档的 OpenAI 兼容面比机器 Schema 宽——**某个参数真正需要用时再验证它在 `/v1/*` 上的行为**，那时才值得一次计费调用（零费用的做法：用一个明显非法的取值，若被拒则是"不认识该参数"或"取值非法"，两者都在受理前、不计费；若被接受则说明端点认这个参数）。

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

**同步响应体里没有 `id`**（顶层 7 个字段、`data[]` 只有 `b64_json`），且同步调用**不出现在** `/ai/v1/images` 任务列表里 ⇒ 创建请求失联后**没有技术手段把结果取回**，只能进对账人工核对（`docs/adr/0007`）。**但响应头里有逐请求标识**：`X-Request-ID`，② 采的就是它。

**转录已落盘**：`out-reference/aihubmix/transcript-sync-and-async-2026-09.json`（本次 2.5 两次 + 2026-09-18 的同步 generations/edits 与 `/ai/v1` 异步；**转录**，非逐字——逐字报文当时未落盘）。

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

### 2.7 本渠道独立待办

- ~~2.5 两款在同步 `/v1` 上是否同构~~ → **已测（2.6）：两款均同构** ✅
- ~~`gpt-image-2.5-flare` 单独调用~~ → **已测（2.6）** ✅
- ~~AIHubMix 的响应头里到底有没有逐请求标识~~ → **已测：有，`X-Request-ID`（§2.6）** ✅
- 2.5 两款在**异步 `/ai/v1`** 上是否同样无 `usage` —— **未测**，且**不必测**：任务对象格式本身不带计量已由第一阶段 §13.1 三场景证实，且 `/ai/v1` 已判定不作为正式计费路径（2026-09-20 复核：该端点不带 `async` 时同样是任务对象，同样无 `usage`）；
- 带参考图的编辑路径（`/v1/images/edits`，multipart）—— 第一阶段在 `gpt-image-2` 上测过（图片输入 1024 tokens），**2.5 未测**。

### 2.8 发布 2.5 供给所需的渠道侧事实（就绪清单）

供发布 ③ Profile / ④ Offering / ⑤ Price 时取用。**只列渠道侧已确认的事实**；发布动作本身属实现范围。

**Profile（③）与 `gpt-image-2` 的差异**（来源：本地机器 Schema 快照，2026-09-19 抓取）：

| 项 | `gpt-image-2` | 2.5 两款 |
| --- | --- | --- |
| `model.const` 字面值 | `gpt-image-2` | **`gpt-image-2.5-flare` / `gpt-image-2.5-sunburst`**（各与 `native_model_id` 同名） |
| `quality` 取值 | `low`/`medium`/`high` | **新增 `xhigh`、`max`**，共 `low`/`medium`/`high`/`xhigh`/`max`/`auto`（默认 `auto`）；在本平台采用的同步 `/v1` 端点上是**顶层**参数 |
| `moderation` | **无** | 上游 `/ai/v1` 机器 Schema 有（`auto`/`low`），但**本平台素材不声明**它（Adapter 未验证该参数，见 §2.9） |
| `background` | 上游有 | 上游有；**本平台素材不声明**（未经验证，按 `docs/adr/0002` 不开放） |
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

### 2.9 素材的参数面：文档支持的都声明在顶层

**执行路径是同步 `/v1/*`，没有 `extra` 这一层**（`extra` 属 `/ai/v1` 那族端点，见 §2.5）。本仓库的 AIHubMix 素材把参数**全部声明在顶层**：

`model` / `prompt` / `image` / `mask` / `n` / `size` / `output_format` / `quality` / `background` / `output_compression` / `user`（2.5 两款另有 `moderation`）。

**依据是文档，不是实测**（[`ADR-0018`](../adr/0018-open-parameters-by-first-party-docs.md)）：渠道第一方文档写明支持的参数就声明；"没实测过"不再作为拦截理由，某个参数真正需要用时再验它在实际端点上的行为。
**当前阶段平台不校验取值**（枚举、区间、类型都不管），但**按选中候选声明的参数面过滤**：候选声明过的参数原样发给上游，没声明的直接丢掉（不报错、不发上游）。哪些参数需要把取值管起来，等一份明确的清单（用户 2026-09-20 说明后期统一整理）。

**取值约束取自文档**：`background` 为 `auto`/`opaque`/`transparent`；`moderation` 为 `auto`/`low`；`output_compression` 为 0–100 的整数；`user` 为字符串。素材里还带一条文档写明的条件：`background = transparent` 时 `output_format` 必须是 `png`。

**2026-09-20 的变化**：此前素材把 `quality` 等放在 `extra` 内、并因此与 Adapter 的声明面互相牵制（当时记为"发布阻塞"）。现在两边都去掉了 `extra`，阻塞不存在了；`quality` 顶层直传（上游在同步 `/v1` 上就是这样）。

### 2.10 已生成的发布素材

| 文件 | 内容 |
| --- | --- |
| `config/bootstrap/aihubmix-gpt-image-2.5-flare.json` | AIHubMix → `gpt-image-2.5-flare` 的 Profile + Offering + Price（草案） |
| `config/bootstrap/aihubmix-gpt-image-2.5-sunburst.json` | 同上，`sunburst` |

两个文件都通过发布校验（顶层参数面与 Adapter 的 `AdapterDescriptor` 一致；见 `crates/application` 的 `validate_adapter_compatibility`）。

**前置（文件内 `_status` 已写明）**：未获「执行实现」授权前不得用于生产；素材是"草案 · 未发布"。

### 2.11 费率

四档单价见 §2.4（文本输入 $5 / 图像输入 $8 / 文本输出 $10 / 图像输出 $30，每 1M tokens）；`config/bootstrap/aihubmix-*.json` 的 `price_plan` 按同一四档填写。

**（2026-09-20 更正）** 本节此前记"在跑的配置把文本输出费率写成 0、平台少收费"。核对当前素材：`text_output_microusd_per_million` 已是 **10000000**（$10），该缺陷不存在于现文件。

### 2.12 平台侧额度失败：`403`（用户 2026-09-20 告知）

AIHubMix 的 **`403` 是平台侧欠费/额度不足**——说的是**我们在这个渠道的账户**没额度了，与消费者余额无关。因此：

- **不得原样返回给消费客户端**：对客应表现为平台侧故障，不能让消费者以为自己欠费；
- 内部必须能发现它属**运营事件**（该渠道需要充值/换额度），而不是普通的用户请求失败；
- 领域区分见 `CONTEXT.md` 的 `Platform Funding Failure` / `Consumer Insufficient Balance`。

该码与 APIMart 的 `402` 是同一类（见 §3.8 第 3 条）；两者都被 Adapter 按"凭据/权限类 → 确定性拒绝"处理（重试同一配置无意义）。**对客怎么呈现已由 `docs/adr/0017` 定下**：一律说成平台侧故障（`platform_unavailable`），渠道码与原文只留内部；内部靠 `failure_kind = platform_funding` 让它成为可发现的运营事件。

### 2.13 错误码（第一方文档快照，页面更新于 2026-06-01）

来源：`out-reference/aihubmix/error-code.md`（第一方「HTTP 状态码」页）。**只有部分状态码带机器可读的「错误标识符」**，其余只能靠状态码 + 消息文本识别；该页自述「**大部分 400 错误是上游透传的报错**」——连消息文本也可能是第三方（Vendor）原文。

| 状态码 | 错误标识符 | 消息（原文摘要） | 常见原因（第一方口径） |
| --- | --- | --- | --- |
| 503 | — | Incorrect model ID… / you do not have permission to use this model | 没有可用的渠道处理请求 |
| 503 | — | Rate limited by provider – contact support… | 模型遇到官方限速 |
| 429 | — | The xx model Too many requests; please try again later. | 请求频率超过限制 |
| **403** | **`insufficient_user_quota`** | Your account balance is insufficient. Please recharge your account… | **用户余额不足，需要充值**——**这里的"用户"是我们**，即平台侧欠费 |
| 403 | — | Account suspended. | 用户状态被禁用或在黑名单 |
| 403 | — | Forbidden – insufficient permissions. | 用户角色权限不够 |
| 403 | — | Forbidden – key(后六位) allowed only from approved IP ranges. | IP 不在令牌允许的网段内 |
| 403 | — | Forbidden – key(后六位) not authorized to access the requested model. | 令牌不支持请求的模型 |
| 403 | — | Key error；(后六位) | 非管理员用户尝试指定渠道 |
| 403 | — | Forbidden – channel has been disabled. | 渠道状态为禁用 |
| 401 | — | Unauthorized – no access token supplied | 未提供 Authorization 头 |
| 401 | — | Unauthorized – access token is invalid or expired | access token 验证失败 |
| 400 | — / `prompt_missing` / `prompt_too_long` / `text_too_long` / `size_not_supported` / `n_not_within_range` | Bad Request – invalid channel ID / prompt is required / … | 渠道 ID 错误、缺提示词、提示词或输入过长、尺寸不被支持、n 超范围 |

**对平台的三条要点**：

1. **"余额不足"说的是我们**：`insufficient_user_quota` 是**平台在渠道侧的账户欠费**（`CONTEXT.md` 的 `Platform Funding Failure`），**不得原样返回给消费客户端**；403 的其余分支（账号禁用、IP 白名单、令牌不支持该模型、渠道被禁用）也全是**我们与渠道之间的配置/资质问题**。
2. **没有"服务器错误"这一档**：该页的 503 只有"没有可用渠道"与"被官方限速"两种含义，属渠道侧/上游侧问题，不是"我们调它时它崩了"。
3. **分类不能只靠字符串匹配**：多数分支没有错误标识符，文本还可能是上游透传——所以要以**状态码兜底**，并保留原始文本供人工核对。

## 3. APIMart

> 同样适用 ②③④⑤；与本文件的 AIHubMix 各节**互不推导**。

### 3.1 端点

> **响应结构台账**：各端点实际返回什么形状、哪一次调用有逐字样本，见 [`out-reference/apimart/response-shapes.md`](../../out-reference/apimart/response-shapes.md)（外部参考资源，只作证据）。

| 端点 | 形态 | 路由是否存在（2026-09-19 零费用探测） |
| --- | --- | --- |
| `POST /v1/images/generations` | **异步**，立即返回 `task_id` | **存在**（无凭证 401） |
| `GET /v1/tasks/{task_id}` | 任务查询（可选 `?language=`，仅影响 `error.message`） | **存在**（无凭证 401） |
| `POST /v1/uploads/images` | 上传本地图以取得可用 `url` | **存在**（无凭证 401；对照：`/v1/nonexistent-route` 返回 404） |

**探测方法**：对 `https://api.apib.ai` 发**不带任何凭证**的请求，只看 401/404（不触达任何账号、不产生任何费用）。三个真实端点在**我们实际配置的域名**上都存在，不只是文档里写着。

**该渠道对同一模型声明两种端点类型，但只有任务面给得出计量与计费事实（2026-09-20 补记）**：目录里 `gpt-image-2.5-flare` / `-sunburst` 的 `supported_endpoint_types` 是 `["image-generation", "openai"]`（原始材料 `out-reference/apimart/catalog-models.json`）。**任务面**（`image-generation`）的终态同时返回四分项 `usage` 与 `cost`（§3.3 有逐字样本）；**非任务面拿不到 token，也没有 `cost` 字段**（用户 2026-09-20 实测）。⇒ 本平台**只用任务面**，该 Offering 的计量与成本事实的唯一来源是任务终态；成本口径的决策见 `docs/adr/0006`。

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
3. ⇒ **金额型计量证据本阶段不需要**（该候选决策经实测被否决并退役，理由见 `.agents/notes/rejected/domain/2026-09-19-provider-declared-charge-as-metering-evidence.md`）；
4. **计价维度与 AIHubMix 相同**：单价 `$5 / $8 / $10 / $30` per 1M；本次 `cost = 0.00476 USD`，与按公开单价算出的 `0.00595` 差**正好 20%**——2026-09-19 由上游账单面板结清：那是面板自报的 `Group ratio 0.8`（**账号级固定倍率**），不是计量误差，也不作为平台结算依据（详见 §5）。

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
- ~~`cost` / `credits_cost` 与 `usage` 的关系~~ → **已结清**：`cost` 是按**折后**账号价的实际扣费，`credits = cost × 10`；折扣是面板自己写明的 `Group ratio 0.8`（§5）✅
- ~~`Idempotency-Key` 是否定义~~ → **机器 Schema 明确声明**（§3.4）✅
- **异步状态取值集合** —— 本次实测见到的终态为 `completed`；完整集合仍以两份文档的**并集**处理（未知取值继续轮询，不得当失败）。**未逐一实测**，属 ② 层实现时按并集容错即可，不阻塞；
- **`image_urls` 图生图路径** —— **已受控实测结清**（见 §3.7、留档 §6）；
- **`sunburst` 型号** —— 未单独实测（目录中已确认在册，`endpoint_types` 与 flare 相同）；图生图按同渠道族 flare 的实测开放。

### 3.7 参考图与遮罩：必须先上传（**2026-09-19 已受控实测结清**）

原始材料：`out-reference/apimart/uploads-images.cn.md`（上传页，2026-09-19 抓取）；实测记录见留档 §6。

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

**带参考图时的计量（实测）**：`usage` 仍是四分项，且 `input_tokens_details.image_tokens` **真的会涨**（512×512 参考图 ⇒ **1024**，与文档口径一致），`text_tokens` 为提示词长度；平台按 `TokenUsage` 归一后结算，与上游自报金额只差固定折扣（见 §3.3、留档 §6）。

**平台侧决定（全在 ② 层，不外泄）**：参考图/遮罩在提交生成任务**之前**先上传换 URL；上传失败＝生成任务**可证明未受理**（`SafeBeforeAcceptance`，`docs/adr/0011`）⇒ Job `failed` + 释放预授权，**不进对账**（与"提交后失联"是两条路径）。**2026-09-20 补**：平台不再托管素材，所以这条只对**调用方给 data URL** 的情况成立（那时才需要解码后上传换 URL）；调用方给公网 URL 时逐字透传，不上传。

**发布素材状态：三条分支已开放。** 两个 `config/bootstrap/apimart-gpt-image-2.5-*.json` 的 `allowed_branches` 已加上 `image_conditioned` / `masked`（`max_images: 16`）。**注意这是发布素材里的能力声明，不是产品上线**——素材 `_status` 仍是"草案 · 未发布"；用户 2026-09-20 明确本阶段是阶段性任务、不存在上线批准。依据 `docs/adr/0002`「未证实的参数不开启，经真实 wire 验证后再发布新修订」——验证已完成：上传返回、`image_urls` 形态、`mask_url` 同用、以及 `usage.input_image_tokens` 四件事都在**一次真实调用**里结清；另外我们**自己的服务**（API + Worker，真实凭证）也对着真实上游跑通了同一条路径（留档 §6）。

**参数名不改写**：生成请求用上游原生名 `image_urls` / `mask_url`（调用方给的是 `image`/`mask`，Adapter 落到这两个字段上）；平台**不**把它改名成 `images`。依据 `docs/adr/0002`（"若某厂商不使用 `image` 这个字段名，由该厂商自己的 Schema 声明原生字段路径"）。平台只在**一处**判定"这个**候选声明**的参数装的是参考图还是遮罩"：名字以 `image` 开头＝参考图、含 `mask`＝遮罩（两者都像时以遮罩为准）、其余一律拒绝（发布期与运行期共用同一个函数）；这套判定只管落位，不用来拦截调用方字段——调用方那些该候选**没声明**的参数在受理时就被丢掉了（不会到这一层，也不会发给上游）。**合同归属的更正（2026-09-20）**：原文此处还引用了 `0002` 的补充决定（"统一参数转换属后期对外消费侧"）。该补充决定已被 [`docs/adr/0015`](../adr/0015-vendor-model-contract-and-offering-parameter-mapping.md) 取代——调用方所见参数名归 **Vendor Model Contract**，"原生名"的落位由 **Offering Parameter Mapping** 承担；"平台内部不改渠道名"这一**当前实现事实**仍然成立，但它是映射层尚未落位的现状，不是既定归属（差距见工作项 [`#6`](https://github.com/dehuadong/seeaihub-server-next/issues/6)）。

**未做**：`sunburst` 的图生图未单独实测（与 flare 同渠道族、同端点、同参数面）；`base64` 路径未测；20MB / 16 张 / 256MB 这些**边界**未逐个压测（只说单张 20MB 上限来自文档，代码里已按此拒绝并另有总量上限）。

### 3.8 错误信封（2026-09-19 零费用探测，无凭证）

无凭证请求真实端点，取到的 401 响应体：

```json
{"error":{"code":"","message":"invalid API key (request id: 20260919182056471923385yBRUUrTx)","param":"","type":"apimart_error"}}
```

三条对实现有影响的事实：

1. **`error.code` 是空字符串**，可用的只有 `type: "apimart_error"` 与 `message`。因此"只依据 `error.code` 分类"在**凭据类失败**上会退化成"受理状态不确定"，把一个明确没进到生成的请求送进人工对账。平台的处置：分类**以 `error.code` 与消息前缀为主**，状态码只在它们给不出信息时兜底——凭据/权限类 HTTP 状态（401/402/403）判为确定性拒绝；**5xx 不按状态码定性**（`build_request_failed` 会以 500 承载参数错误，那正是这条规则要防的情况）。创建阶段另有按状态码收窄的三类，见 §3.10 的 `429` 与两个幂等子类。
2. **每请求标识在失败时也有**：响应头 `X-Oneapi-Request-Id`，同时被写进 `message` 里的 `(request id: …)`。失败路径的 `provider_error_message` 会原样落库，排查时不需要额外取头（**只在失败路径**；成功路径的对账标识是任务式上游的 `task_id`）。
3. **`402` 是平台侧欠费/额度不足**（用户 2026-09-20 告知）：它说的是**我们在这个渠道的账户**没额度了，与消费者余额无关。因此**不得原样返回给消费客户端**——对客表现为平台侧故障（`docs/adr/0017`：`platform_unavailable`），内部靠 `failure_kind = platform_funding` 让它成为可发现的运营事件。领域区分见 `CONTEXT.md` 的 `Platform Funding Failure` / `Consumer Insufficient Balance`。

**未做（本节的探测范围）**：这次探测没有带凭证、也没有调用上传接口——那一步后来在用户批准下单独做过，见留档 §6。本节只记**路由与错误信封**的事实。

### 3.9 成本口径

成本口径（各渠道怎么取数、面板差价核对）**已独立成节**，见 §5。

### 3.10 错误码与错误信封（第一方文档，2026-09-19）

信封统一为 `{"error":{"code","message","type"}}`（部分页面另有 `param`、`request_id`）。**`code` 的类型与是否在场随端点而异**：真实 401 探测里 `error.code` 是**空字符串**（§3.8），而文档示例里 `code` 是**数字**（401/402/…）。因此分类必须以 HTTP 状态码兜底。

| HTTP | 创建/查询接口（第一方文档） | 能否证明"未受理、未计费"（第一方口径） |
| --- | --- | --- |
| `400` | `invalid_request_error`：size 不合法 / resolution 不支持 / 像素违规；查询侧＝"无效的任务 ID" | **能** |
| `401` | `authentication_error`：身份验证失败 | **能** |
| `402` | `payment_required`：**账户余额不足，请充值后再试** | **能**（未受理）——**这里的"账户"是我们**，即平台侧欠费 |
| `403` | 权限不足（官方渠道页） | **能** |
| `409` | 幂等子类：`idempotency_in_progress` / `idempotency_key_reused` / `idempotency_result_indeterminate` | 前两者**能**；`result_indeterminate` **不能**（第一方要求停止自动重试、不要换 Key） |
| `429` | `rate_limit_error`：请求过于频繁 | **能**（未受理） |
| `500` | `server_error`：服务器错误 | **不能**——结果不明 |
| `502` | 网关错误 | **不能** |
| `503` | `service_unavailable`：上游暂时不可用 | 普通 503 **不能**；`503 idempotency_unavailable`（原文"当前请求未执行"）**能** |
| 超时 / 连接中断 | — | **不能**（第一方明示：客户端取消不代表服务端未生成、不代表不计费） |

**两个必须处理的陷阱**：

1. **`500` 会被用来承载参数错误**：示例 message 为 `build_request_failed: invalid size: 3:5, allowed: …`。若按"500 ⇒ 结果不明 ⇒ 进对账"处理，会把一个纯粹可修正的请求错误升级成人工对账。
2. **失败任务会退款**：`failed` 状态写明"reserved funds are refunded"，`/v1/usage` 也写明失败与失败后退款的调用不计入、部分成功的批次按实际交付张数计费。这影响"失败是否产生成本"的判断，但不改变平台的证据门槛。

## 4. 不跨渠道合并（原写法的更正）

本文此前写过「两家响应形状对比表」「一个公式套不了两家」「证据形状不同所以要改领域」等跨渠道结论——**那些都是把渠道差异往上抬，违反 `0004` R1，已删除**。

正确做法：**每个渠道各自的 ② Driver 负责把它自己的响应归一成领域形状**（`TokenUsage`），差异留在各自的 Driver 与其测试里，不进入 ①③④⑤，也不互相推导。

## 5. 平台成本价：各渠道怎么得到（2026-09-19，含上游账单面板核对）

> **本阶段只做成本侧**：拿到上游的价格或计算方式，得到**平台成本价**。平台**对外价**（加价、让利）属后期产品决定，本阶段不做——见 §5.3 与工作项 [#5](https://github.com/dehuadong/seeaihub-server-next/issues/5)。

### 5.1 两个渠道的成本来源不同（这正是 ② 层各归一的事）

| 渠道 | 成本价从哪来 | 依据 |
| --- | --- | --- |
| **APIMart** | **上游直接声明金额**：任务响应里的 `cost`（USD）。面板写明它 = `Base × Group ratio × Channel ratio × Discount ratio`，`credits_cost = cost × 10` | `out-reference/apimart/controlled-probe-2026-09-19.json`、留档 §3/§6、上游账单面板 |
| **AIHubMix** | **上游只给 token，金额要自己按费率算**：按 Tokens 计费，文本输入 **$5** / 文本输出 **$10** / 图像输入 **$8** / 图像输出 **$30**，每 1M tokens ⇒ 成本 = Σ(分项 token × 费率) | `docs/facts/channel-facts.md` §2.4/§2.6（同步 `/v1` 响应只有四分项 token，**没有金额字段**） |

⇒ **不需要用 list 再算一遍 APIMart 的成本**：它自己给了数。`list × 倍率` 只是解释"为什么声明的金额低于公开费率"（本账号 Group ratio 0.8），**不是取数路径**；倍率也可能随账号变化，重算反而引入失真。

### 5.2 实测成本（三笔，都是上游口径）

| 渠道 / 调用 | token 分项（文本in / 图片in / 图片out） | **成本价** | 来源 |
| --- | --- | --- | --- |
| APIMart 走我们自己的服务 | 29 / 1024 / 196 | **$0.011374** | 上游 `cost`（= 面板 Actual cost） |
| APIMart curl 直连 | 33 / 1024 / 196 | **$0.011390** | 上游 `cost`（= 面板 Actual cost） |
| APIMart curl 直连（纯文生图） | 14 / 0 / 196 | **$0.004760** | 上游 `cost`（见留档 §3） |
| AIHubMix 同步 `/v1`（2.5 两款） | 14 / 0 / 196 | **$0.005950** | 自算：14×$5 + 196×$30 per 1M（响应无金额字段） |

**两个渠道的共同点**：都有**四分项 token**（`input_text` / `input_image` / `output_text` / `output_image`），所以平台侧的 `TokenUsage` 归一不变；差别只是"上游给不给金额"，留在各自 ② Driver 里（`0004` R1）。

**`cost` 与公开费率的差额**（面板自报口径）：`Base cost` = Σ(分项 token × 费率)；`Actual cost` = `Base × Group ratio(0.8) × Channel ratio(1) × Discount ratio(1)`；`Credits = Actual × 10`。**平台结算基数**用的是 `price_plan` 里的费率 × 真实分项 token（两个 APIMart 素材现在填的是上游公开费率），**平台对外价未定**。逐笔差价见 §5.4。

**缓存不参与**：本阶段按 Tokens 计费，**不区分缓存**——不建模缓存档、不为它加字段、也不把它当待办。

### 5.3 平台侧现在怎么用这些数（以及没有做什么）

- 平台结算用的是**已发布 `price_plan` 的费率 × 真实分项 token**。两个 APIMart 素材的 `price_plan` 现在填的是上游公开费率——它现在的角色是**结算基数**，不是"平台对外定价决定"。
- **`owned_by` 不携带厂商信息（2026-09-20 登记）**：APIMart 的目录接口响应对**所有**模型都返回 `"owned_by": "custom"`（含 `gemini-*` 等明确非 OpenAI 的模型），因此它**既不能证明也不能否证**某个 `gpt-image-*` 的 Vendor 归属。原始材料见 `out-reference/apimart/catalog-models.json`。⇒ 本仓库 `vendor_id: OpenAI` 是**运营方的显式配置决定**（发布命令里的 `vendor_id` + `native_model_id`，见 `config/bootstrap/*.json` 与工作项 `#2` 的规划范围），不是由渠道字段推导出来的事实。
- **平台对外价尚未决定**：要不要在基数之上加价、要不要把账号折扣让给消费侧，都是**后期产品决定**（跟踪工作项 [#5](https://github.com/dehuadong/seeaihub-server-next/issues/5)）。本阶段**只固化成本价**。
- 上游声明的 `cost`（APIMart）是**折后账号价**，随账号分组变化；它作为**成本价**是对的，但**不能**反过来当作"可复现的计量事实"去替代分项 token（这也是金额型证据被否决的原因之一，见 `.agents/notes/rejected/domain/2026-09-19-provider-declared-charge-as-metering-evidence.md`）。**成本口径按渠道取数的决策见 [`docs/adr/0006`](../adr/0006-no-settlement-without-metering-evidence.md)**（原 `0016` 已合并进该条）：APIMart 取 `cost`、AIHubMix 按费率自算。

### 5.4 差价（面板账单 vs 我们的记录）

用户提供面板要核对的就是**差价**。逐笔如下（三笔都是同一倍率，**没有其它费用**）：

| 调用 | token 分项（文本in / 图片in / 图片out） | 面板 `Base cost`（= 平台侧 capture / 结算基数） | 上游实收（= **成本价**） | **差价** | 差价率 |
| --- | --- | --- | --- | --- | --- |
| 走我们自己的服务 | 29 / 1024 / 196 | $0.014217（capture 14217 microusd） | $0.011374 | **$0.002843** | 20% |
| curl 直连 | 33 / 1024 / 196 | $0.014237 | $0.011390 | **$0.002847** | 20% |
| curl 直连（纯文生图，留档 §3） | 14 / 0 / 196 | $0.005950 | $0.004760 | **$0.001190** | 20% |

**差价的来源只有一个**：面板自报的 `Group ratio 0.8 × Channel ratio 1 × Discount ratio 1`，即**整笔 −20%**；分项逐项算也对得上（`Base = Σ(token × 费率)`，两次面板的每一行都与按公开费率手算的结果一致）。`credits = USD × 10`。

**平台侧的位置**：capture `14217 microusd` 等于面板 `Base cost`（不是实收）——平台侧结算基数用的是公开费率，与上游实收之间就是这 20%。

**AIHubMix 侧目前无法核对差价**：上游不返回任何金额字段，也无从知道它是否给账号折扣（§2.4）。要结清得看它的控制台/账单，不在这几次调用的实测范围内。

**金额型计量证据因此被否决**：平台的**计量事实**是四分项 token；金额随账号倍率变化、不可复现，所以结算必须由分项 token 推出，上游声明的金额只用来核成本（本阶段要的正是它）。留档 §6 那次真实端到端是这条的实证。

**来源**：用户 2026-09-19 在会话中提供的两张上游控制台"详情"面板截图（含 `task_id` 与 API 密钥标签，故**截图本身不入库**；本表只转录与结算有关的数字与倍率）。

---

真实计费调用（授权依据、次数、花费、样本位置）留档在 `docs/verification/paid-provider-calls.md`。
