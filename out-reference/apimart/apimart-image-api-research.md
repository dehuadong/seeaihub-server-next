# APIMart `gpt-image-2` 第一方现状核实与接口合同调研

> 调研日期：2026-09-19（抓取时间窗口 UTC 2026-09-19 03:50–04:15 / UTC+8 11:50–12:15）
> 用途：为「第二阶段接入第二个 Provider、验证同一 Vendor Model 多 Offering 供应」提供事实依据
> 性质：上游协议研究与现状核实，**不是**本平台对外接口合同；`out-reference/` 不参与构建与 cargo 验证
> 与既有快照的关系：本文**不修改** `generation.md` / `status.md` / `webhook.md`（旧仓库快照，含旧仓库退场标注），只补充现状核实与评估

## 0. 阅读约定

每条关键事实后给出**来源 URL + 证据等级 + 抓取时间**。证据等级：

| 代号 | 含义 |
| --- | --- |
| **A-第一方文档** | APIMart 自营文档站 `docs.apimart.ai` 上的页面（含其官方 `llms.txt` / `sitemap.xml`） |
| **A-第一方站点** | APIMart 自营站点 `apimart.ai` 上的页面或服务端渲染数据 |
| **A-第一方接口** | 对 `api.apimart.ai` 的只读/无凭证探测（未产生任何计费调用） |
| **本地旧快照** | 本仓库既有 `out-reference/apimart/*.md`，来自旧仓库，可能已过期 |
| **我的推断** | 本文作者基于上述证据的推论，不是 APIMart 声明 |
| **待确认** | 第一方资料缺失或自相矛盾，必须实测才能定论 |

**凭证纪律**：本文不含任何真实 API Key、Bearer token。文档中出现的示例 task id 一律脱敏为 `task_…（示例已截断）`。本文未执行任何付费调用。

**重要澄清**：既有快照里的「已退场（#223 / ADR-0008，2026-07-26）：Apimart 渠道已从 SeeAI Hub 完整移除」是**旧仓库的产品决策**，不是 APIMart 服务停运的证据。本次核实的结论恰好相反，见第 2 节。

---

## 1. 调研范围与结论摘要

调研对象：候选 Provider **APIMart**（站点 `apimart.ai`，文档 `docs.apimart.ai`，API `api.apimart.ai`），聚焦其图像生成接口与 `gpt-image-2` 相关模型。

**十项调研问题的一句话结论**：

| # | 问题 | 一句话结论 |
| --- | --- | --- |
| 1 | 是否仍在运营 | **是，且明显活跃**：文档站 1710 个 URL 的 sitemap 最新 lastmod 为 2026-09-18，API 无凭证探测全部返回预期的 401/404；但**没有 changelog / 弃用公告页**，也没有可核验的自有状态页 |
| 2 | 图像生成接口合同 | `POST /v1/images/generations` 合同完整可取；但 `gpt-image-2` 的 `n` **文档内自相矛盾**（字段说明写「取值 1」、同页示例写 `n: 2`），参考图上限已从旧快照的 16 变为 **15**，并新增 `nsfw_check` |
| 3 | 任务查询 | `GET /v1/tasks/{task_id}` 字段与终态清晰，创建响应 `data[0].task_id` 可直接用于恢复——**这一项明显优于第一阶段 AIHubMix 的 `/v1` 同步路径**；但状态机有四种互相冲突的写法 |
| 4 | 计量证据 | **对 `gpt-image-2` 这个 model 名只有金额没有用量**：任务响应给 `cost`(USD) + `credits_cost`(积分)；带分项 `usage` 的第一方示例只出现在 `gpt-image-2-official` 与 `gpt-image-2.5`。另有 `GET /v1/usage` 聚合账（金额/积分/请求数/token），但**不返回 task_id**，无法逐笔关联 |
| 5 | 幂等与安全重提 | **APIMart 设计了完整的 `Idempotency-Key` 语义**（含 409/503 四种冲突码），但只在 `grok-imagine-2.0-ext` 的页面文档化，`gpt-image-2` 页面完全未提 → 是否生效为**待确认** |
| 6 | webhook | 合同清楚（字段、`base + /callback` 拼接、只推终态、最多 3 次重试、按 `id` 去重）；**没有可验证的签名机制**，文档只写「配置并校验签名 / verify the origin」，无签名头、无算法、无密钥 |
| 7 | 错误语义 | 统一 `error{code,message,type}`；幂等场景额外有 `409 idempotency_key_reused / idempotency_in_progress / idempotency_result_indeterminate` 与 `503 idempotency_unavailable`，其中后者第一方明确声明「当前请求未执行」——这是**少见的、可证明未受理未计费**的错误码 |
| 8 | 价格与计价单位 | **Credits 计价，10 credits = $1，页面同时标注约合 USD**；`gpt-image-2` 按张 × 分辨率档位（1K $0.0085 / 2K $0.014 / 4K $0.021），`gpt-image-2-official` 按 token；中文价格页**没有人民币** |
| 9 | 模型身份 | **无法证实** `gpt-image-2` 就是 OpenAI 官方同一个 Vendor Model。第一方文档把 `gpt-image-2` 与 `gpt-image-2-official`（明确写「OpenAI 官方 gpt-image-2 模型」）**并列为两个模型**，且价格差 25 倍 → 标为**待确认，且倾向「非官方渠道」** |
| 10 | 接入手续 | 注册 → `/keys` 创建 Key（可设配额/模型限制/IP 白名单）→ 充值（支付宝/微信/Stripe/U支付/PayPal/Creem）。未见到实名要求；免费额度与最低充值金额**待确认** |

**总体判断**：APIMart 的**协议完备度显著高于第一阶段 AIHubMix**（异步 task id 可查询、有幂等键设计、有用量聚合 API），服务也确实在运营。但它在「同一 Vendor Model 多 Offering 供应」这个具体验证目标上存在一个**核心未决问题：`gpt-image-2` 的模型身份未经证实**。详见第 12 节。

---

## 2. 运营状态（调研问题 1）

### 2.1 事实：APIMart 正在运营，且文档处于活跃维护状态

| 事实 | 证据 | 等级 | 抓取时间 |
| --- | --- | --- | --- |
| 官网 `https://apimart.ai/` 返回 HTTP 200，页面约 480 KB | 直接请求 | **A-第一方站点** | 2026-09-19 |
| 文档站 `https://docs.apimart.ai/llms.txt` 返回 HTTP 200，提供 `_llms/{en,cn,ja,ko,ru,de,fr,pt,id,es}` 共 10 种语言索引 | `https://docs.apimart.ai/llms.txt` | **A-第一方文档** | 2026-09-19 |
| 文档站自我描述为「API Manual (152 pages)」「Chinese (171 pages)」，并列出 30+ 图像模型、20+ 视频模型、音频、审核、上传、任务管理、账户管理章节 | `https://docs.apimart.ai/_llms/en/api-manual.md` | **A-第一方文档** | 2026-09-19 |
| `https://docs.apimart.ai/sitemap.xml` 返回 HTTP 200，共 **1710** 条 URL，且每条带 `lastmod`；最新 lastmod 为 **2026-09-18T09:08:08.951Z** | `https://docs.apimart.ai/sitemap.xml` | **A-第一方文档** | 2026-09-19 |
| API 主机在线：`GET /v1/models`、`GET /v1/usage` 无凭证返回 **401**；`GET /v1/nonexistent-path-xyz` 返回 **404** | 对 `https://api.apimart.ai` 的无凭证探测 | **A-第一方接口** | 2026-09-19 |
| 图像模型目录持续扩张：现同时存在 `gpt-image-2`、`gpt-image-2-official`、`gpt-image-2.5`、`gpt-image-2.5-ext`、`gpt-image-1`（1/1.5）、Seedream 4/4.5/5.0 Lite/5.0 Pro、FLUX 2、FLUX 3（coming soon）、Qwen Image 3.0、Z-Image-Turbo、Grok Imagine、wan2.7、Midjourney 等 | `https://docs.apimart.ai/_llms/en/api-manual.md` | **A-第一方文档** | 2026-09-19 |

**结论（事实）**：APIMart 在 2026-09-19 仍正常运营并提供图像生成 API。既有快照的「已退场」标注不反映 APIMart 服务状态。

### 2.2 事实：没有版本/迁移/弃用公告渠道

- 在 1710 条 sitemap URL 中检索 `changelog`、`release`、`migration`、`deprecat`、`announce` —— **命中数均为 0**。`https://docs.apimart.ai/sitemap.xml`，**A-第一方文档**，2026-09-19。
- `llms.txt` 的 Guides 章节只有 `quickstart` 与 `development`，没有变更日志。`https://docs.apimart.ai/llms.txt`，**A-第一方文档**，2026-09-19。

**影响（我的推断）**：APIMart 的接口合同会**静默演进**。本仓库既有快照与当前文档在多个字段上已经不一致（见第 3 节），这说明「按快照固定合同」的做法在这里特别必要——不能跟随上游即时变化。

### 2.3 事实：未找到可核验的自有状态页（并观察到悬空子域）

| 探测目标 | 结果 |
| --- | --- |
| `https://status.apimart.ai` | TLS 握手失败 |
| `http://status.apimart.ai` | HTTP **502 Bad Gateway** |
| `https://apimart.ai/status` | HTTP **404** |
| DNS `status.apimart.ai` A/AAAA | `128.242.245.93`、`2a03:2880:f12d:83:face:b00c:0:25de` |

来源：本机 DNS 与 HTTP 探测，**A-第一方接口/我的观察**，2026-09-19。

**我的推断（非 APIMart 声明）**：这两个 IP 属于 Meta/Facebook 的地址段（`face:b00c` 是其知名段），说明 `status.apimart.ai` 这个子域**并不由 APIMart 当前提供服务**，看起来是一条指向第三方基础设施的悬空解析，而不是 APIMart 的状态页。因此：**无法用第一方状态页核实可用性**。这一点值得在选型时注意（既是可用性监控缺口，也是一个子域治理观察）。

站点落地文档 `https://docs.apimart.ai/en/index.md` 中的「Real-Time Status」卡片实际链接到的是**任务状态接口** `/api-reference/tasks/status`，不是服务健康页。**A-第一方文档**，2026-09-19。

---

## 3. 接口合同（调研问题 2、3、6）

### 3.1 端点总览

| 端点 | 用途 | 证据 | 等级 |
| --- | --- | --- | --- |
| `POST https://api.apimart.ai/v1/images/generations` | 文生图 / 图生图（统一入口，异步） | [gpt-image-2 生成文档](https://docs.apimart.ai/en/api-reference/images/gpt-image-2/generation.md) | **A-第一方文档** |
| `GET https://api.apimart.ai/v1/tasks/{task_id}` | 任务状态与结果 | [任务状态文档](https://docs.apimart.ai/en/api-reference/tasks/status.md) | **A-第一方文档** |
| `POST https://api.apimart.ai/v1/tasks/batch` | 批量查询任务 | 仅被 `gpt-image-2.5` 页提及；无文档页；探测返回 401 而非 404 | **A-第一方文档 + A-第一方接口**，合同**待确认** |
| `POST https://api.apimart.ai/v1/uploads/images` | 上传图片换 URL（multipart，72h 有效，≤20MB） | [上传图片文档](https://docs.apimart.ai/en/api-reference/uploads/images.md) | **A-第一方文档** |
| `GET https://api.apimart.ai/v1/usage` | 消费/用量聚合查询 | [查询消费用量](https://docs.apimart.ai/en/api-reference/account/usage.md) | **A-第一方文档** |
| `POST https://api.apimart.ai/v1/logs/export` | 异步导出调用明细（CSV/XLSX） | 仅在同一页的对比表出现；**无独立文档页**；探测 401 非 404 | **A-第一方文档（弱）+ A-第一方接口**，合同**待确认** |
| `GET https://api.apimart.ai/v1/dashboard/billing/usage` | 累计消费（无模型/时间过滤） | 同上 | **待确认** |
| `POST /v1/images/edits` | 存在（webhook 文档明确说它不支持 `webhook`/`language`） | [Webhook 文档](https://docs.apimart.ai/en/api-reference/tasks/webhook.md) | **A-第一方文档**，合同未展开 |

**文档站缺陷（事实）**：`llms.txt` 把 `https://docs.apimart.ai/api-reference/openapi.json` 列为 OpenAPI Spec，但该文件实际内容是**一份名为 "OpenAPI Plant Store" 的示例规范**（`paths` 只有 `/plants`，`servers.url` 为 `https://api.apimart.ai`）。抓取时间 2026-09-19，**A-第一方文档**。

**我的推断**：不存在可机读的权威 OpenAPI 合同。Native Schema 只能用**页面文本 + `GET /v1/models?expand=parameters`** 获取，而后者需要 API Key。见第 9 节。

### 3.2 `POST /v1/images/generations` —— `gpt-image-2`（渠道版）请求字段

来源：[中文页面](https://docs.apimart.ai/cn/api-reference/images/gpt-image-2/generation.md) / [英文页面](https://docs.apimart.ai/en/api-reference/images/gpt-image-2/generation.md)，页面 `lastmod` = **2026-09-01T06:31:28Z**（取自 sitemap.xml）。抓取时间 2026-09-19。

| 字段 | 类型 | 必填 | 默认 | 取值范围 / 约束 | 等级 |
| --- | --- | --- | --- | --- | --- |
| `model` | string | 是 | `gpt-image-2` | 固定 `gpt-image-2`；**兼容别名 `gpt-image-2-ext`，文档称两者等价、结果相同** | **A-第一方文档** |
| `prompt` | string | 是 | — | 中英文均可；提交前经过平台敏感词/安全审核，命中违规直接返回错误 | **A-第一方文档** |
| `n` | integer | 否 | `1` | 字段说明写「**取值：1**」；必须传纯数字，不能加引号 | **A-第一方文档（但见下方冲突）** |
| `size` | string | 否 | `1:1` | `auto` + 15 个比例（`1:1 3:2 2:3 4:3 3:4 5:4 4:5 16:9 9:16 2:1 1:2 3:1 1:3 21:9 9:21`）；也可直接传像素尺寸如 `1881x836`。`size=auto` 时默认比例回落 `1:1` | **A-第一方文档** |
| `resolution` | string | 否 | `1k` | `1k` / `2k` / `4k`；与 `size` 相乘决定实际像素（4K 支持全部 15 个比例） | **A-第一方文档** |
| `image_urls` | array | 否 | — | **最多 15 张**，超过返回 `image_urls exceeds max 15`；单张 ≤20MB、总计 ≤256MB；支持公网 URL 与 `data:image/...;base64,...`，**同一数组可混填**；不传 `size` 时输出分辨率 = 输入图分辨率 | **A-第一方文档** |
| `official_fallback` | boolean | 否 | `false` | `false` 不使用，`true` 使用「官方渠道兜底」 | **A-第一方文档** |
| `nsfw_check` | boolean | 否 | `false` | `true` 时用 `omni-moderation-latest` 审核提示词与输入图，增加审核成本与延迟 | **A-第一方文档** |
| `response_format` / `style` | — | — | — | **不支持，会被忽略** | **A-第一方文档** |

注意：`gpt-image-2`（渠道版）页面**没有** `quality`、`background`、`moderation`、`output_format`、`output_compression`、`mask` 字段（已用文本检索确认：`quality=False background=False mask=False`）。这些字段只出现在 `gpt-image-2-official` 页面。

#### 3.2.1 与既有本地快照的差异（事实）

| 项目 | 本地旧快照 `generation.md`（2026-08-04） | 当前第一方文档（2026-09-01） |
| --- | --- | --- |
| `n` 取值 | `1 - 10` | **字段说明写「取值：1」** |
| 参考图上限 | 最多 **16** 张，超限报 `image_urls exceeds max 16` | 最多 **15** 张，超限报 `image_urls exceeds max 15` |
| `nsfw_check` | 无 | 新增，默认 `false` |
| 模型别名 | 无 | 新增兼容别名 `gpt-image-2-ext` |
| 英文示例 | — | 与中文文档一致 |

**影响（我的推断）**：任何按旧快照固化的 `n` 上限（10）与参考图上限（16）都会与当前合同不符。进入实现前必须以带日期的页面快照 + 付费冒烟为准。

#### 3.2.2 事实：`gpt-image-2` 页面存在文档内部矛盾

同一页（`lastmod` 2026-09-01）同时出现：

1. `n` 字段说明：`取值：1`（中文）/ `Value: 1`（英文）；
2. 「文生图（多张）」示例：`{"n": 2}`（中文页约 582 行、英文页约 584 行）。

**这是同一份第一方文档的自相矛盾**，不是跨来源冲突。`n` 的真实可接受范围只能实测。**A-第一方文档**，2026-09-19。

#### 3.2.3 事实：上传页声明 base64 已不再支持，与生成页矛盾

- 生成页（`gpt-image-2`、`gpt-image-2-official`）仍明确列出「支持 `base64 data URI`（形如 `data:image/png;base64,...`）」。
- 上传页给出 `Warning`：**"Important Change: For better performance and cost control, we no longer support passing base64 image data directly in generation APIs. Please use this API to upload images, get the URL, and then call the generation API."**

来源：[生成页](https://docs.apimart.ai/en/api-reference/images/gpt-image-2/generation.md) · [上传页](https://docs.apimart.ai/en/api-reference/uploads/images.md)，**A-第一方文档**，2026-09-19。

**我的推断**：参考图入参方式应以**上传换 URL** 为稳妥路径；base64 是否仍被接受为**待确认**。对 Adapter 而言，「把调用方图片先变成 URL」的封装是必要的。

#### 3.2.4 事实：上传页的端到端示例已过期

上传页末尾的 Python「Full Example」使用 `image_urls: [{"url": ...}]`、`response.json()['id']`、`result['status']`、`fail_reason`、**`GET /v1/images/generations/{task_id}`** 轮询。而当前合同是：`image_urls` 为**字符串数组**、task id 在 `data[0].task_id`、轮询路径是 **`GET /v1/tasks/{task_id}`**。

补充佐证（**A-第一方接口**，2026-09-19）：`GET https://api.apimart.ai/v1/images/generations/task_abc` 返回 **404**，而 `GET /v1/tasks/task_abc` 返回 **401**（存在但需鉴权）。即上传页示例的轮询路径**不存在**。

### 3.3 创建响应

```json
{
  "code": 200,
  "data": [
    { "status": "submitted", "task_id": "task_…（示例，已脱敏）" }
  ]
}
```

- 第一方说明：「成功提交后返回 `task_id`，通过 `GET /v1/tasks/{task_id}` 轮询」；`data` 为数组，读 **`data[0].task_id`**。
- 来源：[生成页](https://docs.apimart.ai/en/api-reference/images/gpt-image-2/generation.md) · [gpt-image-2.5 页](https://docs.apimart.ai/en/api-reference/images/gpt-image-2.5/generation.md)，**A-第一方文档**，2026-09-19。
- **事实**：创建响应**不含** `usage`、`cost`、`credits_cost` —— 计费信息只在任务查询/终态出现。

### 3.4 任务查询 `GET /v1/tasks/{task_id}`

来源：[任务状态文档](https://docs.apimart.ai/en/api-reference/tasks/status.md) / [中文](https://docs.apimart.ai/cn/api-reference/tasks/status.md)，页面 `lastmod` = **2026-08-25T04:02:31Z**。抓取时间 2026-09-19。

**Query 参数**：`language`，支持 `en zh ja ko ru fr de id pt es`（10 种，大小写不敏感、去首尾空格；**不接受 `zh-CN`/`en-US` 这类区域标签**；只影响 `error.message`）。

**响应字段**（`data` 对象内）：

| 字段 | 类型 | 语义 | 出现条件 |
| --- | --- | --- | --- |
| `id` | string | 任务唯一标识 | 始终 |
| `status` | string | 见下方状态机 | 始终 |
| `cost` | number | **本次任务扣费金额（USD）** | 始终（文档未标注条件） |
| `credits_cost` | number | **本次任务扣费积分** | 始终 |
| `progress` | integer | 进度 0–100 | 始终 |
| `result` | object | `result.images[]`（图像）或 `result.videos[]`（视频），每项含 `url: string[]` 与 `expires_at` | 仅 `completed` |
| `created` | integer | 创建时间戳（秒） | 始终 |
| `completed` | integer | 完成时间戳 | 仅完成时 |
| `estimated_time` | integer | 预计耗时（秒） | 始终 |
| `actual_time` | integer | 实际耗时（秒） | 仅完成时 |
| `error` | object | `{code, message, type}` | 仅 `failed` |

**结果取值**：`data.result.images[0].url[0]`（第一方明确给出）。

#### 3.4.1 事实：状态机存在四种互相冲突的写法

| 来源 | 状态取值 / 流转 |
| --- | --- |
| 任务状态页「Response → status」枚举（`lastmod` 2026-08-25） | `pending`（排队）/ `processing`（处理中）/ `completed` / `failed` / `cancelled`（用户取消） |
| `gpt-image-2` 生成页「任务状态说明」（`lastmod` 2026-09-01） | `submitted`（已提交）/ `processing`（上游处理中）/ `completed` / `failed` |
| `gpt-image-2-official` 生成页（`lastmod` 2026-08-21） | 流转写作 `submitted` → **`in_progress`** → `completed` / `failed` |
| `gpt-image-2.5` 生成页（`lastmod` 2026-09-09） | `submitted` / `processing` / `completed` / `failed` |
| 创建响应（所有页面一致） | `status: "submitted"` |

**后果（我的推断）**：Adapter **不能**把状态枚举写死为某一页的清单。稳妥做法是把 `submitted|pending|processing|in_progress` 全部视为「非终态」，把 `completed|failed|cancelled` 视为终态，并对**未知状态值**默认继续轮询而非判定成功/失败。这是本次调研中**最需要实测确认**的合同点之一。

#### 3.4.2 事实：`cancelled` 只在任务状态页的枚举里出现

- 创建、webhook、各图像生成页都没有 `cancelled` 的流转或取消 API。
- webhook 页明确：「只推送**终态**（`completed` / `failed`）」——**不含 `cancelled`**。
- 1710 条 sitemap URL 中未检索到 cancel 相关的任务接口页。

**我的推断**：`cancelled` 很可能是历史/内部状态，当前**没有公开的取消能力**；且即便出现，也不能把「本地取消」解释为上游未执行或不计费（第一方对浏览器取消有明确声明，见 3.6 节）。

### 3.5 `gpt-image-2-official`（独立 model 名）的请求合同

来源：[官方渠道生成文档](https://docs.apimart.ai/en/api-reference/images/gpt-image-2/official.md)，页面 `lastmod` = **2026-08-21T08:30:09Z**，抓取时间 2026-09-19。

第一方摘要：「**OpenAI official `gpt-image-2` model**, based on `/v1/images/generations` compatible protocol …… 单请求最多 4 张图、最多 16 张参考图；与 `gpt-image-1.5-official` 参数对齐度 95%」。

| 字段 | 类型 | 默认 | 取值范围 / 约束 |
| --- | --- | --- | --- |
| `model` | string | `gpt-image-2-official` | 固定值（第一方称其为 OpenAI 官方 gpt-image-2 模型） |
| `nsfw_check` | boolean | `false` | 同渠道版 |
| `prompt` | string | — | 必填 |
| `size` | string | `1:1` | `auto` + 15 比例 + 像素尺寸 |
| `resolution` | string | `1k` | `1k` / `2k` / `4k`（第一方标注为「**new field**」，1k=1024 基线、2k=2048、4k=3840） |
| `quality` | string | `auto` | `auto`（默认，通常等价 `low`）/ `low` / `medium` / `high`（4K + high 可能 >120s） |
| `background` | string | `auto` | `auto` / `opaque` / `transparent`（透明要求 PNG/WebP） |
| `moderation` | string | `auto` | `auto` / `low` |
| `output_format` | string | `png` | `png` / `jpeg` / `webp` |
| `output_compression` | integer | — | `0–100`，仅 jpeg/webp 有效 |
| `n` | integer | `1` | **`1 ~ 4`** |
| `image_urls` | array | — | 最多 **16** 张；单张 ≤20MB、总 ≤256MB；**要求公网可访问的稳定 URL** |
| `mask_url` | string | — | 必须与 `image_urls` 同用；mask 需带 Alpha 通道，且尺寸必须与**第一张参考图**一致 |

### 3.6 webhook 合同（调研问题 6）

来源：[任务完成回调文档](https://docs.apimart.ai/en/api-reference/tasks/webhook.md) / [中文](https://docs.apimart.ai/cn/api-reference/tasks/webhook.md)，`lastmod` = **2026-08-25T04:02:31Z**，抓取时间 2026-09-19。

| 维度 | 合同 | 等级 |
| --- | --- | --- |
| 字段名 | 提交体**顶层** `webhook`（基础地址）+ `language`（可选，仅影响 `error.message`） | **A-第一方文档** |
| 地址拼接 | 你的地址 + **`/callback`**；`https://x.com`→`https://x.com/callback`；`https://x.com/api`→`https://x.com/api/callback`；尾部斜杠被归一 | **A-第一方文档** |
| 推送时机 | **只推终态**（`completed` / `failed`），处理中不推 | **A-第一方文档** |
| 载荷 | **与 `GET /v1/tasks/{task_id}` 完全一致**（可用同一套解析） | **A-第一方文档** |
| 失败结构 | `error: { message, type: "task_failed", param: "", code: "task_failed" }` | **A-第一方文档** |
| 重试 | 约 10s 内未返回 2xx，或返回 `5xx` → 最多重试 **3 次**，间隔约 **10s / 30s / 60s**；全部失败即放弃（约 2 分钟内结束） | **A-第一方文档** |
| 不重试 | 返回 `4xx` 视为地址/请求有问题，立即放弃 | **A-第一方文档** |
| 去重 | 正常只推一次；极端情况（发出后确认前重启）**可能重复推送**，第一方要求**按 `id`（task_id）幂等去重** | **A-第一方文档** |
| 签名 | **无任何可验证签名机制**。文档只在建议步骤里写「配置并校验签名 / Configure and verify the signature」，以及「verify the origin of callback requests」；**未给出签名头名、算法、密钥或 IP 段** | **A-第一方文档（缺失即证据）** |
| 不支持方 | `POST /mj/submit/*` 与 **`POST /v1/images/edits`** 不支持 `webhook`/`language`；官方 xAI 图像模型不支持 `language`（会返回 `400 parameter "language" is not supported`） | **A-第一方文档** |
| 地址要求 | 必须公网可访问（`127.0.0.1`/`10.x`/`192.168.x` 被拒）、`http`/`https`、标准端口 `80`/`443`、不能指向 APIMart 自身域名；不满足直接丢弃（不推送、不重试） | **A-第一方文档** |

**我的推断**：webhook 只能当**加速唤醒信号**；收到后仍应以 `GET /v1/tasks/{id}` 复核，并按 `id` 去重。由于没有签名，公开回调端点只能靠**平台侧一次性随机回调路径 + 来源 IP 白名单 + 主动查询复核**来防御伪造。这与第一阶段 AIHubMix 的结论一致（任务级 webhook 无独立签名密钥）。

---

## 4. 计量证据评估（调研问题 4）

这是本次调研与第二阶段目标**最相关**的一节。

### 4.1 逐项列出的用量事实

| 证据位置 | 字段 | 单位与语义 | 是否足以支撑结算 | 等级 |
| --- | --- | --- | --- | --- |
| `gpt-image-2` 任务查询响应 | `cost` | 金额，**美元** | 是**金额**，非用量 | **A-第一方文档** |
| `gpt-image-2` 任务查询响应 | `credits_cost` | 平台积分，**满足 `credits_cost = cost × 10`**（0.05279 ↔ 0.5279；0.15 ↔ 1.5；0.006 ↔ 0.06 均可验算） | 同上，是金额的另一种单位 | **A-第一方文档 + 我的验算** |
| `gpt-image-2` 任务查询响应 | `usage` | **第一方示例中不存在** | **否** | **A-第一方文档（缺失即证据）** |
| `gpt-image-2-official` 任务查询响应 | `usage.input_tokens`、`usage.input_tokens_details.{cached_tokens,image_tokens,text_tokens}`、`usage.output_tokens`、`usage.output_tokens_details.{image_tokens,text_tokens}`、`usage.total_tokens` | token 数；第一方称「the billable token usage for this request」；示例 `22 + 196 = 218` | **是（分项可核验用量）** | **A-第一方文档** |
| `gpt-image-2.5` 任务查询响应 | `usage.{input_tokens, output_tokens, total_tokens}` | token 数（无 details 子对象）；示例 `16 + 439 = 455` | **是（较少分项）** | **A-第一方文档** |
| `GET /v1/usage` 聚合账 | `data.total` / `data.items[].{amount_usd, credits, requests, prompt_tokens, completion_tokens}` | `amount_usd`（USD，6 位小数）、`credits`（= `amount_usd × 10`，与网站展示一致）、`requests`（**成功计费的调用数**）、`prompt_tokens` / `completion_tokens`（**图像/视频模型通常为 0**，因为按次/按张计费） | **是（账号+模型+时间窗级别的金额与调用次数）**，但**无 task_id** | **A-第一方文档** |
| 控制台 UI | 「消费日志」「任务日志」「导出记录」「充值&账单」 | 站点 i18n 字符串中出现这些页面名 | 可能可逐笔核对，但**非文档化 API** | **A-第一方站点（间接）** |

来源：[gpt-image-2 生成页](https://docs.apimart.ai/en/api-reference/images/gpt-image-2/generation.md) · [gpt-image-2-official 生成页](https://docs.apimart.ai/en/api-reference/images/gpt-image-2/official.md) · [gpt-image-2.5 生成页](https://docs.apimart.ai/en/api-reference/images/gpt-image-2.5/generation.md) · [任务状态](https://docs.apimart.ai/en/api-reference/tasks/status.md) · [查询消费用量](https://docs.apimart.ai/en/api-reference/account/usage.md)（`lastmod` **2026-09-18T09:08:08Z**，是本次抓取到的最新页面之一）。抓取时间均为 2026-09-19。

### 4.2 `GET /v1/usage` 的关键约束（事实）

- 端点：`GET /v1/usage`（等价 `GET /usage`），支持 CORS。
- 参数：`start`（含）、`end`（不含），接受 Unix 秒或 RFC3339；**区间 ≤ 31 天**；`model`（逗号分隔，最多 50 个，精确且大小写不敏感，**不支持通配符**）；`group_by` = `none|model|date|model,date`；`tz` 默认 `Asia/Shanghai`；`scope` = `key`（默认，仅当前 Key）或 `account`（账号下全部 Key）。
- 速率与缓存：每 Key 每分钟 60 次；相同参数结果**缓存 60 秒**（响应头 `X-Usage-Cache: hit|miss`）；用量记录通常 1 秒内可用，但**最后一分钟可能不完整**；第一方明确要求「**不要把这个端点当作实时计费通知**」。
- 计费口径（原文要点）：
  - 「使用**成功计费**的调用记录，与网站看板同一份数据」；
  - 「**失败调用、失败后退款的任务不计入**，无需人工冲正。**部分成功的图片批次按实际交付张数计费**」；
  - 「消费归属**计费入账时间**；异步图像/视频任务在**完成时**入账，而非提交时；跨零点任务计入完成日」；
  - 「手动余额调整不属于调用消费，不计入」；
  - 「**2026-04-27 之后的数据可用**」。
- 错误处理：`400 invalid_start/invalid_end/invalid_range/range_too_large/invalid_tz/invalid_group_by/invalid_scope/too_many_models`；`401/403` 认证层；`429` 限流；`503 usage_unavailable`。第一方**明确警告**：「`503 usage_unavailable` **不返回任何金额**，它表示查询不可用，**不表示消费为零**；不要把失败响应当成 0 或覆盖上一次成功结果」。

### 4.3 结论：对 `gpt-image-2` 而言，只有金额，没有用量；"可核验"程度取决于能否接受聚合口径

**事实（三条并列，必须一起看）**：

1. `gpt-image-2` 的任务响应第一方示例**只有 `cost` + `credits_cost`**，没有 token 用量。
2. 带分项 `usage` 的第一方示例只挂在 **`gpt-image-2-official`** 与 **`gpt-image-2.5`** 这两个**不同 model 名**下。
3. `GET /v1/usage` 提供账号+模型+时间窗的金额与成功计费调用数，**但不返回 task_id**，无法把某一次提交与某一笔扣费逐笔关联。

**我的推断（对结算设计的影响）**：

- 本平台**可以**用 `cost` 作为「Provider 声明的单次扣费金额」证据，做**金额级**对账；这已经比「平台自算价格」强，因为它是上游声明的实际扣费。
- 但**不能**声称拿到了「可核验的用量事实（token 数 / 张数）」——对 `gpt-image-2` 而言，图片张数只能由 `result.images[].url` 的长度推断，token 用量在第一方资料中不存在。
- 「按张计费」这一口径（价格页按 1K/2K/4K 档位 × 张）与 `cost` 是自洽的：可以用 `cost` 反查是否符合价格页档位，作为**交叉校验**。
- 若要更细的逐笔记录，只剩两条未文档化路径：`POST /v1/logs/export`（第一方在对比表里说明「异步导出调用明细 CSV/XLSX，需自行聚合」，但**没有文档页**）与控制台「任务日志 / 消费日志」。
- `gpt-image-2` 是否**实际**会返回 `usage`（文档未写但实现可能存在）属于**待确认**，只有付费实测能回答。

---

## 5. 幂等与恢复能力（调研问题 5）

### 5.1 事实：APIMart 定义了一套完整的幂等键语义——但只在 grok 模型页文档化

来源：[Grok Imagine Image 2.0 官方图像生成与编辑（中文）](https://docs.apimart.ai/cn/api-reference/images/grok-imagine-2.0-ext/official.md)（约 17 KB）。抓取时间 2026-09-19，**A-第一方文档**。

**推荐请求头**：

| Header | 要求 | 说明 |
| --- | --- | --- |
| `Authorization` | 必须 | `Bearer <APIMart API Key>` |
| `Content-Type` | 必须 | `application/json` |
| `Accept` | 推荐 | `application/json` |
| **`Idempotency-Key`** | **强烈推荐** | 每次逻辑生成使用唯一 UUID；网络重试复用原值 |
| **`X-APIMart-Response-Version`** | **强烈推荐** | 固定为 `2026-07-27`，确保响应结构稳定 |

**幂等规则（原文要点）**：图片生成会产生费用；每次用户确认的**一次逻辑生成**创建一个 `Idempotency-Key`；同一次请求的网络重试**复用原 UUID 和完全相同的请求 Body**；修改模型/提示词/图片/其它参数后创建**新** UUID；**不要**在每次自动重试时创建新 UUID；同一 Key 重试时 `X-APIMart-Response-Version` 也必须保持一致。

**幂等结果矩阵（原文表格）**：

| 场景 | 响应 | 处理方式 |
| --- | --- | --- |
| 原请求已完成 | 重放原响应 | 使用原任务 ID 查询结果 |
| 原请求仍在处理 | `409 idempotency_in_progress` | 按 `Retry-After` 等待，用原 Key 与原 Body 重试 |
| 同一 Key 对应不同参数 | `409 idempotency_key_reused` | 客户端逻辑错误，修复 Key 生命周期 |
| **请求结果无法确认** | **`409 idempotency_result_indeterminate`** | **停止自动重试，不要换 Key，记录 `request_id` 排查** |
| 幂等服务暂不可用 | `503 idempotency_unavailable` | **当前请求未执行**，稍后重试 |

同一页还有一条与结算直接相关的第一方声明（`Note`）：**「浏览器取消请求只表示客户端不再等待响应，不代表服务端生成已经取消，也不代表一定不会计费。」**

### 5.2 事实：`gpt-image-2` 相关页面完全没有提到幂等

对已下载的第一方页面做全文检索：

| 页面 | 含 `Idempotency-Key` | 含 `X-APIMart-Response-Version` |
| --- | --- | --- |
| `gpt-image-2` 生成（中/英） | 否 | 否 |
| `gpt-image-2-official` 生成 | 否 | 否 |
| `gpt-image-2.5` 生成 | 否 | 否 |
| `gpt-image-1` 生成 | 否 | 否 |
| `grok-imagine-2.0-ext` 官方（中文，全量） | **是** | **是** |
| `grok-imagine-2.0-ext` 官方（英文，仅 3.9 KB stub） | 否 | 是（仅摘要行） |

**我的推断**：APIMart **平台层面具备**幂等键能力（错误码命名以 `idempotency_*` 为前缀、`X-APIMart-Response-Version` 也是平台级命名），但**对 `gpt-image-2` 是否生效没有第一方依据**。这是「APIMart 是否能显著优于 AIHubMix 的失联恢复能力」的**决定性问题**，必须实测。**如果生效**，那么「响应丢失后安全重提」在这一 Offering 上是可实现的；**如果只对部分模型生效**，则 `gpt-image-2` 的失联处理与 AIHubMix 同等，仍须 `reconciliation_required`。

### 5.3 事实 + 推断：恢复能力

- **事实**：创建响应直接给出 `data[0].task_id`，且 `GET /v1/tasks/{task_id}` 可查询状态、结果、`cost`、`credits_cost`。**A-第一方文档**，2026-09-19。
- **我的推断**：只要**拿到过** task_id，本次提交就是**完全可恢复**的（状态、结果、金额都能确认）。这是相对 AIHubMix `/v1` 同步路径（无 task id 可查）的**实质性改进**，直接缓解第一阶段「请求发出后响应丢失只能人工对账」的已知缺陷。
- **事实**：文档化的任务管理**只有** `status` 与 `webhook` 两个页面；1710 条 sitemap URL 中没有任务**列表**接口。**A-第一方文档**，2026-09-19。
- **事实**：`POST /v1/tasks/batch` 被 `gpt-image-2.5` 页提及（「Use `POST /v1/tasks/batch` to query multiple tasks」），探测返回 401（存在），但**无文档页**。**A-第一方文档（弱）+ A-第一方接口**，2026-09-19。
- **推论**：如果**创建响应本身丢失**（连 task_id 都没拿到），当前文档**没有**公开的「按时间窗列出任务」API 来反查这次提交是否被受理。控制台「任务日志」页面很可能提供了这个能力（i18n 字符串存在「任务日志」「消费日志」「导出记录」），但它**不是文档化的 API**。因此：**「提交后完全失联」场景下，自动恢复仍不可保证**——这一点与 AIHubMix 相同。

---

## 6. 错误语义（调研问题 7）

### 6.1 事实：错误结构与状态码

**标准形态**（图像生成页 / 任务查询页一致）：

```json
{ "error": { "code": 400, "message": "……", "type": "invalid_request_error" } }
```

**更完整形态**（grok 页的「统一错误结构」，JSON 与 webhook 失败载荷一致）：

```json
{ "request_id": "req_…（示例，已脱敏）", "error": { "message": "……", "type": "…", "param": "", "code": "…" } }
```

来源：[gpt-image-2 生成页](https://docs.apimart.ai/en/api-reference/images/gpt-image-2/generation.md) · [任务状态页](https://docs.apimart.ai/en/api-reference/tasks/status.md) · [grok 官方页](https://docs.apimart.ai/cn/api-reference/images/grok-imagine-2.0-ext/official.md)，**A-第一方文档**，2026-09-19。

| HTTP | 创建接口 | 任务查询接口 | 幂等场景（grok 页） | 能否证明「上游未受理、未计费」 |
| --- | --- | --- | --- | --- |
| `400` | 参数错误（size 不合法 / resolution 不支持 / 像素违规） | 无效的任务 ID | 参数错误，**不要自动重试** | **能**（未受理） |
| `401` | 认证失败 | 凭据无效 | API Key 无效 | **能** |
| `402` | 账户余额不足，请充值 | 余额不足 | 余额或额度不足 | **能**（未受理） |
| `403` | 权限不足（官方渠道页有） | 访问被禁止 | 权限不足 | **能** |
| `409` | — | — | `idempotency_in_progress` / `idempotency_key_reused` / `idempotency_result_indeterminate` | **前两者能**（请求未执行或为客户端逻辑错误）；**`result_indeterminate` 不能**——第一方要求停止自动重试、不要换 Key |
| `429` | 请求过于频繁 | 限流 | 按 `Retry-After` 退避，**复用原 Key** | **能**（未受理） |
| `500` | 服务器错误 | 服务器内部错误 | 服务异常，保留原 Key 与原 Body 按策略重试 | **不能**——结果不明 |
| `502` | 网关错误 | 网关错误 | — | **不能** |
| `503` | 上游暂时不可用 | — | **`503 idempotency_unavailable`：当前请求未执行** | **能（仅幂等不可用这一子类）**；普通 503 不能 |
| 网络超时 / 连接中断 | — | — | 第一方明示：浏览器取消**不代表**服务端未生成、**不代表**不计费 | **不能** |

### 6.2 事实：部分参数校验错误以 500 返回

`gpt-image-2` 页的 `500` 示例 `message` 为：

> `build_request_failed: invalid size: 3:5, allowed: 1:1 / 16:9 / 9:16 / 4:3 / 3:4 / 3:2 / 2:3 / 5:4 / 4:5 / 2:1 / 1:2 / 3:1 / 1:3 / 21:9 / 9:21`

来源：[gpt-image-2 生成页](https://docs.apimart.ai/en/api-reference/images/gpt-image-2/generation.md)，**A-第一方文档**，2026-09-19。

**我的推断**：这是一个必须处理的实际陷阱——**上游会用 `500` 表达「参数不合法」**。Adapter 若按「500 = 结果不明 → 进入 reconciliation_required」处理，会把纯粹的可修正请求错误升级成人工对账。稳妥做法是：对 `5xx` 额外检查 `message` 中是否含 `build_request_failed` / `invalid size` 等可识别的前缀，命中则映射为**参数错误**（不重试、不计费风险低），否则按结果不明处理。这是从第一方示例里读出的、需要实测确认的经验规则（**我的推断 + 待确认**）。

### 6.3 事实：失败任务会退款

- `gpt-image-2.5` 页状态表：`failed` = 「Generation failed; check `error.message`; **reserved funds are refunded**」。
- `GET /v1/usage` 计费口径：「**Failed calls and tasks refunded after failure are excluded**」，且「**部分成功的图片批次按实际交付张数计费**」。

来源同上，**A-第一方文档**，2026-09-19。

**我的推断**：这为「失败不计费」提供了第一方文本依据，且「部分成功按张计费」正好与「按张计价」自洽，可用于结算逻辑的失败分支设计。但**退款到账的时延与对账口径仍需实测**。

---

## 7. 价格与计价单位（调研问题 8）

### 7.1 事实：币种与单位

- 价格页地址：**`https://apimart.ai/zh/pricing`**（中文）、`https://apimart.ai/pricing`（英文，见 FAQ 链接）、以及 `/fr/pricing`、`/es/pricing` 等本地化路径。
- 计价单位：**Credits（积分）**，页面同时给出「~$X」的美元约合值。换算关系 **10 credits = $1**（`0.085 Credits ~$0.0085`、`0.14 ~$0.014`、`0.21 ~$0.021` 均可验算）。
- **中文价格页全文检索：`人民币`、`CNY`、`¥`、`&yen;`、`美元`、`USD` 命中数均为 0** —— 即页面用「Credits + ~$」表达，**没有人民币计价展示**。
- 价格页 meta 描述：「500+ 顶级 AI 模型透明计费，覆盖对话、图像、视频、音频。按量付费，无套餐，无隐藏费用。」（注：模型广场页写的是「100+ AI 模型」，两处数字不一致）

来源：`https://apimart.ai/zh/pricing`，**A-第一方站点**，抓取时间 2026-09-19。

### 7.2 事实：`gpt-image-2` 的官方价格表（价格页原文数值）

价格页把该模型显示为 **`gpt-image-2-ext`**，副标题为 **`(gpt-image-2)`**，标注「**4 个价格档位**」，并给出「我们的价格 / 官方价格 / 节省」三列：

| 档位 | 我们的价格 | 官方价格（页面对照列） | 节省 |
| --- | --- | --- | --- |
| 默认 | 0.085 Credits /张 ~$0.0085/张 | 0.10625 Credits /张 ~$0.010625/张 | 20 % |
| 1K | **0.085 Credits /张 ~$0.0085/张** | **2.109 Credits /张 ~$0.2109/张** | **96 %** |
| 2K | **0.14 Credits /张 ~$0.014/张** | 4.283 Credits /张 ~$0.4283/张 | 97 % |
| 4K | **0.21 Credits /张 ~$0.021/张** | 7.117 Credits /张 ~$0.7117/张 | 97 % |

**计价单位 = 按张 × 分辨率档位（1K/2K/4K）**，第一方摘要行亦写「Billed by resolution tier (1K / 2K / 4K)」。

### 7.3 事实：`gpt-image-2-official` 的价格表（按 token）

价格页把该模型显示为 **`gpt-image-2-official`**，副标题 **`OpenAI GPT Image 2 · 2026-07-18`**，标注「**1 个价格档位**」，并附注「**下方为 Token 单价，最终费用按实际 Token 用量结算**」：

| 计费项 | 我们的价格 | 官方价格（页面对照列） | 节省 |
| --- | --- | --- | --- |
| 文本输入 | 40 Credits /M ~$4 /M | 50 Credits /M ~$5 /M | 20 % |
| 缓存文本输入 | 10 Credits /M ~$1 /M | 12.5 Credits /M ~$1.25 /M | 20 % |
| 图片输入 | 64 Credits /M ~$6.4 /M | 80 Credits /M ~$8 /M | 20 % |
| 缓存图片输入 | 16 Credits /M ~$1.6 /M | 20 Credits /M ~$2 /M | 20 % |
| 图片输出 | 240 Credits /M ~$24 /M | 300 Credits /M ~$30 /M | 20 % |

**对照价值（我的推断）**：这张表实质上是 **OpenAI 官方 gpt-image-2 的价目**（$5 / $1.25 / $8 / $2 / $30 per M tokens，与第一阶段 AIHubMix 公开的 $5 / $8 / $30 per M 一致），而 `gpt-image-2`（渠道版）的「官方价格」列写的是 1K ≈ $0.2109、2K ≈ $0.4283、4K ≈ $0.7117 per 张。**两个模型的计价维度完全不同**（按张 vs 按 token），这本身就是「它们是不同供给/不同东西」的一条强证据。

### 7.4 事实：任务响应里的示例金额与价格页不一致

- `gpt-image-2` 任务查询示例：`cost: 0.05279`、`credits_cost: 0.5279`（≈ $0.0528/任务）。
- `gpt-image-2-official` 任务查询示例：`cost: 0.004792`、`credits_cost: 0.047920000000000004`。
- `gpt-image-2.5` 任务查询示例：`cost: 0.01325`、`credits_cost: 0.1325`。
- 任务状态页示例：`cost: 0.15`、`credits_cost: 1.5`。

**我的推断**：这些是**跨版本拼接的示意数值**，与价格页档位（1K $0.0085）不对应，**不能当作当前价格**使用。价格必须以价格页（带抓取时间）为准，最终以 `cost` 与 `GET /v1/usage` 实测为准。

---

## 8. 模型身份（调研问题 9）

这是**结论最负面**的一节，也是最需要如实记录的一节。

### 8.1 事实：第一方把 `gpt-image-2` 与 `gpt-image-2-official` 并列为两个不同模型

| 证据 | 原文（关键部分） | 来源 |
| --- | --- | --- |
| 文档模型索引 | `[GPT-Image-2 Image Generation]` 与 `[GPT-Image-2 Official Channel Image Generation]` 是**两条独立条目** | [api-manual](https://docs.apimart.ai/_llms/en/api-manual.md) |
| 渠道版页面摘要 | 「OpenAI Images compatible protocol」；**未声明供应商** | [gpt-image-2 生成页](https://docs.apimart.ai/en/api-reference/images/gpt-image-2/generation.md) |
| 渠道版模型字段 | 「Fixed to `gpt-image-2`（compatible alias `gpt-image-2-ext`）」 | 同上 |
| 官方版页面摘要 | 「**OpenAI official `gpt-image-2` model**, based on `/v1/images/generations` compatible protocol」 | [official 页](https://docs.apimart.ai/en/api-reference/images/gpt-image-2/official.md) |
| 官方版模型字段 | 「Fixed to `gpt-image-2-official`（**OpenAI official gpt-image-2 model**）」 | 同上 |
| 价格页 | 两条独立价格条目：`gpt-image-2-ext`（副标题 `(gpt-image-2)`，按张 4 档）与 `gpt-image-2-official`（副标题 **`OpenAI GPT Image 2 · 2026-07-18`**，按 token） | `https://apimart.ai/zh/pricing` |
| 模型详情页 | `https://apimart.ai/model/gpt-image-2` 页面提供 **`gpt-image-2-ext` 与 `gpt-image-2-official` 两个页签** | `https://apimart.ai/model/gpt-image-2` |

抓取时间均为 2026-09-19。

### 8.2 事实：营销页的措辞是模糊的，且不能替代模型身份声明

- `https://apimart.ai/model/gpt-image-2` 的 HTML `<title>` 为 **"Free GPT Image 2 API - OpenAI GPT Image API on APIMart"**，meta description 也写 "OpenAI GPT Image API"。
- 同一页面正文写「APIMart provides the most affordable and reliable access to GPT Image 2」「up to 70% cheaper than competitors with 99.9% uptime guarantee」。
- 该页**没有任何一句**说明 `gpt-image-2-ext`（即 `gpt-image-2`）的供给来源、是否 OpenAI 官方直连、是否为第三方托管/微调/近似模型。

来源：`https://apimart.ai/model/gpt-image-2`，**A-第一方站点**，2026-09-19。

### 8.3 结论：**待确认，且第一方证据倾向「不是同一官方供给」**

- **待确认**：不存在任何第一方声明表明 `gpt-image-2` 就是 OpenAI 官方同一个 Vendor Model。SEO 标题里的 "OpenAI GPT Image API" 是营销措辞，而**第一方文档在需要精确的地方把「official」单独命名**，这本身就是反证。
- **我的推断（有证据支撑的倾向，但不等于事实）**：三点一致指向「`gpt-image-2` 是**非官方渠道供给**」——
  1. 第一方为「官方供给」单独开了 `gpt-image-2-official` 这个 model 名并明确写 "OpenAI official"；
  2. 两者计价维度不同（按张 vs 按 token），渠道版 1K 价格只有官方对照价的约 **4%**（0.085 vs 2.109 credits）；
  3. 渠道版页面支持 `official_fallback`（是否使用官方渠道兜底）这个字段——**「兜底」这个语义只有在默认路径不是官方渠道时才成立**。
- **可验证路径（待确认 → 可核实）**：文档提供了 `GET /v1/models?expand=category` 与 `GET /v1/models/{model}/schema`、`GET /v1/model-schema?model=...`（均需 API Key；探测返回 401 说明存在），响应项含 **`owned_by`** 字段（示例里有 `"owned_by": "openai"`、`"owned_by": "alibaba"`）。**用真实 Key 查 `gpt-image-2` 的 `owned_by` 是可行性最高、成本最低的模型身份核实手段。** 来源：[Models List Metadata API](https://docs.apimart.ai/en/api-reference/texts/models/list.md)，**A-第一方文档**，2026-09-19。

**对第二阶段目标的直接影响**：如果第二阶段的验证命题是「**同一个 Vendor Model 由多个 Offering 供应**」，那么 `gpt-image-2` 这个 Offering 的**模型身份正是该命题的核心前提**，而第一方证据**不支持**它。这一点不能靠推断补上。

---

## 9. 接入手续与受控付费验证可行性（调研问题 10）

### 9.1 事实：注册与 API Key

- 快速开始：访问 **`https://apimart.ai/keys`** → 登录/注册 → 点 **Create API Key** → 填 **Name**，可选配置**配额（Unlimited Quota）**、**模型限制（Enable Model Limits）**、**IP 白名单** → **Create Key** → 复制保存。
- 来源：[Quick Start](https://docs.apimart.ai/en/quickstart.md)，**A-第一方文档**，2026-09-19。
- 站点 i18n 字符串显示支持 **GitHub / Google OAuth** 登录（`"Continue with GitHub"`、`"Continue with Google"`），并有绑定第三方账号的流程。来源：`https://apimart.ai/zh/keys`，**A-第一方站点**，2026-09-19。
- 支持渠道：Discord 社区（`https://discord.gg/V8zqssyZ5c`）、X/Twitter `@APIMart_`、Dashboard 内企业微信客服扫码、客服邮箱、工单系统。来源：[Account Management FAQ](https://docs.apimart.ai/en/faqs/account-management.md) 与站点 i18n，2026-09-19。

### 9.2 事实：充值

- 页面：`https://apimart.ai/zh/billing`（「充值&账单」）。
- 支付方式（站点 i18n 字符串）：**支付宝、微信支付、Stripe、U支付（nowpayments）、PayPal、Creem**。
- 形态：**充值套餐 + 自定义充值金额**；有「赠送 {percent}%」与「兑换码」机制；有「最低」限制标签（`"minimum":"最低"`）与错误文案 `minimumAmount`「充值金额低于最低限制，请提高金额后重试」。
- 来源：`https://apimart.ai/zh/billing` / `https://apimart.ai/zh/pricing`，**A-第一方站点**，2026-09-19。

**待确认（明确缺失）**：

- **最低充值具体金额未在静态页面中给出**（需要登录或在充值弹窗中读取）。
- 站点另有「免费试用 / 次每天」与「免费」计费类型标签（i18n 字符串 `freeTrial`、`timesPerDay`、`remaining`、`free`），但**未见针对 `gpt-image-2` 的免费额度说明**。
- **未见任何实名认证要求的说明**（文档与 FAQ 均未提及）。这只能记为「未见到要求」，不能断言「不需要」。

### 9.3 事实：站点规模自述（营销口径，非审计数据）

- 模型页自称「50K+ Active Users」「99.9% Uptime」「2x Faster」「70% Cost Savings」「99.9% SLA」「Official Discounts」「Pay-as-you-go」。
- 来源：`https://apimart.ai/model/gpt-image-2`，**A-第一方站点（自述，无法核验）**，2026-09-19。**这不是可核验的运营数据。**

### 9.4 受控付费验证的可行性评估

**结论：可行，且成本极低；但有三项必须先做、无 Key 无法完成的核实。**

- 单张 1K 成本约 **$0.0085**（≈ 0.085 credits），一次完整验证集（含失败用例）预计总成本 **< $0.10**。
- **低成本前置核实（不需要生成图片，只需 API Key）**：
  1. `GET /v1/models?expand=category` → 查 `gpt-image-2` 与 `gpt-image-2-official` 的 **`owned_by`**，回答第 8 节的模型身份问题；
  2. `GET /v1/models/gpt-image-2/schema`（或 `/v1/model-schema?model=gpt-image-2`）→ 拿到**权威参数 schema**（`required`/`enum`/`minimum`/`default`），一次性解决 `n` 范围、`image_urls` 上限、是否接受 base64 等全部文档冲突；
  3. `GET /v1/usage`（空转，零余额也可用）→ 确认返回结构与 `amount_usd`/`credits` 口径。

**建议的最小付费验证集**（每项都记录 wire 请求/响应，但只留协议字段与计量值）：

| # | 场景 | 要回答的问题 |
| --- | --- | --- |
| 1 | `gpt-image-2` 文生图，`resolution=1k`，`size=1:1` | 创建响应形状、task_id 可查询性、终态取值（`submitted`/`pending`/`processing`/`in_progress` 到底哪个出现）、**任务响应是否真的没有 `usage`**、`cost` 是否等于价格页 1K 档（$0.0085） |
| 2 | `gpt-image-2` 图生图，`image_urls` 传**上传后的 URL** | 图生图路径是否可用、`cost` 是否变化 |
| 3 | `gpt-image-2` 图生图，`image_urls` 传 **base64 data URI** | 上传页的「不再支持 base64」是否与生成页矛盾（这是文档冲突的裁决点） |
| 4 | **同一 `Idempotency-Key` + 完全相同 Body 连发两次** | **幂等键对 `gpt-image-2` 是否生效**（返回同一 `task_id`？还是新建任务并二次计费？）——本次调研最重要的待确认项 |
| 5 | 故意传 `size=3:5` | 是否真的返回 **500** 且 message 含 `build_request_failed`；`GET /v1/usage` 是否计入 |
| 6 | `GET /v1/usage?group_by=model`（范围覆盖上述调用） | `amount_usd` / `requests` / `prompt_tokens` 在图像模型上是否为 0 / 是否正确累计；`requests` 是否等于成功张数 |
| 7 | 触发一次失败（如不可达的 `image_urls`） | 是否退款、`GET /v1/usage` 是否排除该笔、任务 `error` 结构 |
| 8 | `POST /v1/logs/export`（若可用） | 导出明细**是否含 task_id**，能否实现逐笔对账（当前最大的计量缺口） |

---

## 10. 与第一阶段 AIHubMix 的能力差异对照表

对照基线：本仓库 `out-reference/aihubmix/gpt-image-2-inferera-research.md`（含 2026-09-18 受控实测结论）。

| 维度 | AIHubMix（第一阶段） | APIMart（本次核实） | 对第二阶段的意义 |
| --- | --- | --- | --- |
| 调用模型 | 同步 `/v1` POST 直接返回结果 | 创建即**异步**，返回 `task_id` | APIMart 天然契合「先落持久 Job 再调 Provider」 |
| **上游 task id 可查询** | **`/v1` 路径无**（已知缺陷 → 只能人工对账） | **有**：`data[0].task_id` + `GET /v1/tasks/{id}` | **APIMart 在这一点上实质优于 AIHubMix**，直接缓解第一阶段缺陷 |
| 响应丢失后恢复 | 无法确认上游是否生成/计费 | 拿到 task_id 即可完全恢复；**未拿到则仍无公开的任务列表 API** | 部分改善；「完全失联」场景仍需人工 |
| **计量证据** | `/v1` 返回分项 token `usage`（`input_tokens_details`/`output_tokens_details`），可支撑结算 | `gpt-image-2` **只有 `cost`/`credits_cost` 金额**，无 token；分项 `usage` 只在 `gpt-image-2-official` / `gpt-image-2.5` 名下 | **APIMart 在这一 Offering 上计量粒度反而更弱**（但有上游声明的实际扣费金额） |
| 账单级证据 | 待确认（未找到账单查询 API） | `GET /v1/usage`（金额/积分/请求数/token，按模型/日分组）+ 控制台日志/导出；**无 task_id 维度** | APIMart 提供可用但对账粒度有限 |
| **幂等键** | **无**（文档、Schema、模型说明均未声明） | **有完整设计**（`Idempotency-Key` + `X-APIMart-Response-Version`，含 409/503 语义），但**仅 grok 页文档化，`gpt-image-2` 未提** | 若实测生效 → 显著优于 AIHubMix；否则持平 |
| Webhook 签名 | 任务级无签名，第一方建议用账户级订阅 | 任务级无签名，文档只写「校验来源」 | **持平（都不可签名验证）** |
| Webhook 重试 | 最多 6 次；5xx/网络/超时重试，3xx/4xx 不重试 | 最多 **3 次**（约 10s/30s/60s）；5xx 重试，4xx 不重试 | 略弱（次数少） |
| 错误结构 | `error{message,type,code,tid}`（5xx 带 `tid`） | `error{code,message,type}`；grok 页有 `request_id`/`param` | 持平；APIMart 的 `request_id` 是否普遍返回**待确认** |
| 明确的「未受理未计费」错误码 | 有分类，无幂等专用码 | `409 idempotency_key_reused`、`503 idempotency_unavailable`（原文「当前请求未执行」） | **APIMart 更明确** |
| 图片输入上限 | `images` 最多 **16** | `gpt-image-2` **15** / `gpt-image-2-official` **16** | 需按模型分别配置 |
| `n` 范围 | `1..10` | `gpt-image-2` 文档写 **1**（示例写 2，矛盾）；official `1..4` | 需实测 |
| 参考图入参 | URL / Data URI | URL / base64 data URI 文档说支持，**上传页说不支持 base64** → 有 `POST /v1/uploads/images` 作为替代路径 | APIMart 多一条「先上传换 URL」的正规路径 |
| Mask | 支持 `mask`（类型有 Schema 冲突） | `gpt-image-2` 无 mask；`gpt-image-2-official` 有 `mask_url`（须与 `image_urls` 同用、需 Alpha、尺寸须匹配首图） | APIMart 把 mask 能力放在另一个 model 名下 |
| 质量/背景等扩展 | `extra.quality`(low/medium/high)、`extra.background`… | `gpt-image-2` **无** `quality`/`background`；official 有 `quality`/`background`/`moderation`/`output_format`/`output_compression` | 能力分布不同，不能跨 Offering 假定同构 |
| 计价单位 | token：文本输入 $5/M、图片输入 $8/M、图片输出 $30/M | `gpt-image-2` **按张 × 分辨率档**（1K $0.0085 / 2K $0.014 / 4K $0.021）；official 按 token（$4/$6.4/$24 per M 渠道价） | 需要两套不同形态的 Price Plan |
| **模型身份** | 第一方模型页明确 `gpt-image-2`，开发者 OpenAI | `gpt-image-2` 与 `gpt-image-2-official` **分离**，前者身份未证实，有 `official_fallback` 字段暗示默认非官方 | **最大风险点**，见第 12 节 |
| 服务状态可核验性 | 有第一方状态码文档；第三方 | 无自有状态页（`status` 子域悬空指向 Meta IP）；靠 sitemap lastmod 与 API 探测判断 | 弱于 AIHubMix |
| 变更公告渠道 | 无（同样是文档静默演进） | 无 changelog/弃用公告页 | 持平；都要求「按快照固定合同」 |
| 文档质量 | 有 Schema 自相矛盾（mask 类型、quality=auto） | **同样有**：`n` 自相矛盾、状态机 4 种写法、base64 声明冲突、上传页示例用已不存在的轮询路径、`openapi.json` 是 plant store 占位、英文 grok 页是 3.9KB stub、落地页 `index` 严重滞后于目录 | 持平或略差；**两边都必须以实测为准** |

---

## 11. 事实 / 推论 / 待确认 三分清单

### 11.1 硬事实（第一方来源直接支持）

1. APIMart 在 2026-09-19 仍在运营：官网、文档站、API 主机均在线；文档站 sitemap 共 1710 条 URL，最新 lastmod **2026-09-18T09:08:08Z**；`api.apimart.ai` 无凭证探测返回预期的 401/404。
2. 既有快照中的「Apimart 已退场」是旧仓库产品决策，**不反映** APIMart 服务状态。
3. `POST https://api.apimart.ai/v1/images/generations` 的请求字段、类型、默认值与约束如第 3.2 节；响应为 `{code, data:[{status:"submitted", task_id}]}`，task id 在 **`data[0].task_id`**。
4. `gpt-image-2` 的参考图上限当前为 **15**（旧快照写的 16 已过期）；新增 `nsfw_check`；新增兼容别名 `gpt-image-2-ext`。
5. `gpt-image-2` 文档存在**内部矛盾**：`n` 字段说明写「取值 1」，同页示例写 `n: 2`。
6. 上传页第一方声明「**no longer support passing base64 image data directly in generation APIs**」，与生成页仍列 base64 支持**互相矛盾**。
7. 上传页的端到端示例使用**不存在的**轮询路径 `GET /v1/images/generations/{task_id}`（探测 404）；当前路径是 `GET /v1/tasks/{task_id}`（探测 401）。
8. `GET /v1/tasks/{task_id}` 的响应字段如第 3.4 节，含 `cost`（USD）与 `credits_cost`（积分），且满足 `credits_cost = cost × 10`。
9. 状态机有**四种互相冲突的写法**（`pending`/`processing`、`submitted`、`in_progress`、`submitted→processing`）；webhook 只推 `completed`/`failed`；`cancelled` 仅在任务状态页枚举中出现，无公开取消 API。
10. `gpt-image-2` 的任务响应第一方示例**只有 `cost` + `credits_cost`，无 `usage`**；分项 `usage` 只出现在 `gpt-image-2-official` 与 `gpt-image-2.5`。
11. `GET /v1/usage` 返回 `amount_usd`（6 位小数）、`credits`（= amount_usd×10）、`requests`（成功计费调用数）、`prompt_tokens`、`completion_tokens`；图像/视频模型 token 字段通常为 0；区间 ≤31 天；缓存 60s；**数据自 2026-04-27 起**；失败/已退款任务排除；异步任务按**完成时间**入账；**不返回 task_id**；`503 usage_unavailable` 不得当成 0。
12. APIMart 定义了完整 `Idempotency-Key` + `X-APIMart-Response-Version` 语义，含 `409 idempotency_in_progress` / `idempotency_key_reused` / `idempotency_result_indeterminate` 与 `503 idempotency_unavailable`（原文：「当前请求未执行」）；**仅在 `grok-imagine-2.0-ext` 中文页文档化**。
13. webhook 合同：顶层 `webhook`（base）+ 自动拼 `/callback`；只推终态；载荷与任务查询接口完全一致；最多重试 3 次（约 10/30/60s）；4xx 不重试；可能重复推送需按 `id` 去重；**无任何签名机制**；`POST /v1/images/edits` 与 `/mj/submit/*` 不支持 `webhook`/`language`。
14. 错误结构为 `error{code,message,type}`（部分页面另有 `request_id`/`param`）；**部分参数校验错误以 `500` 返回**，示例 message 为 `build_request_failed: invalid size: 3:5, allowed: …`。
15. `failed` 任务**会退款**；**部分成功的图片批次按实际交付张数计费**。
16. 价格页：`https://apimart.ai/zh/pricing`；单位 Credits，**10 credits = $1**，页面同时标注约合 USD；**中文页无人民币字样**。`gpt-image-2`（显示为 `gpt-image-2-ext`）按张 4 档：默认/1K 0.085 credits（~$0.0085）、2K 0.14（~$0.014）、4K 0.21（~$0.021），「官方价格」对照列 1K 2.109（~$0.2109）、2K 4.283、4K 7.117，节省 96–97%。`gpt-image-2-official` 按 token：文本输入 40 credits/M、缓存文本 10、图片输入 64、缓存图片 16、图片输出 240（页面对照价 $5/$1.25/$8/$2/$30 per M）。
17. 第一方把 `gpt-image-2` 与 `gpt-image-2-official` 作为**两个模型**；后者被明确写为「**OpenAI official `gpt-image-2` model**」；`gpt-image-2` 页面**未声明供应商**，只有 `official_fallback` 字段。
18. 文档**没有** changelog / release / migration / deprecation 页面（1710 条 URL 命中 0）。
19. `https://docs.apimart.ai/api-reference/openapi.json` 是**"OpenAPI Plant Store" 示例**，不是 APIMart 的真实 API 规范。
20. 未找到 APIMart 自有状态页：`status.apimart.ai` 解析到 Meta/Facebook 地址段，`http` 502、`https` TLS 失败；`apimart.ai/status` 404。
21. `POST /v1/tasks/batch`、`POST /v1/logs/export`、`GET /v1/dashboard/billing/usage`、`GET /v1/models/gpt-image-2/schema`、`GET /v1/model-schema` **端点存在**（探测 401 而非 404），其中前三者**无独立文档页**。
22. 接入流程：注册 → `https://apimart.ai/keys` 创建 Key（可设配额/模型限制/IP 白名单）→ 充值；支付方式含支付宝、微信、Stripe、U支付、PayPal、Creem。

### 11.2 我的推断（有证据支撑，但不是 APIMart 声明）

1. APIMart 的接口合同会**静默演进**（无公告渠道，且旧快照已与现文档多处不一致），必须以带日期的快照固定 Native Schema。
2. `gpt-image-2` 很可能是**非 OpenAI 官方渠道**的供给（依据：官方版被单独命名、计价维度不同、渠道价仅为官方对照价的约 4%、存在 `official_fallback`「官方渠道兜底」字段）。
3. `cancelled` 状态当前**没有对应的公开取消能力**。
4. 平台侧应把 `Idempotency-Key` 视为**平台级能力但模型级生效范围未知**；在实测前不能依赖它做安全重提。
5. 上游用 `500` 表达部分参数错误，Adapter 需要**基于 message 前缀**区分「参数错误」与「结果不明」，否则会把可修正请求升级成人工对账。
6. 任务响应里的 `cost` 示例数值是**跨版本拼接的示意值**，不能当价格使用。
7. 只要拿到过 task_id，本次提交就完全可恢复；**未拿到 task_id 的「完全失联」场景，公开 API 无法反查**（控制台「任务日志」可能可以，但非文档化）。
8. `gpt-image-2-official` 价格表里的「官方价格」列（$5/$1.25/$8/$2/$30 per M）实质就是 OpenAI 官方 gpt-image-2 价目，与第一阶段 AIHubMix 公开价一致。
9. 由于 webhook 无签名，「base + /callback」这个固定路径反而是个弱点——建议用**不可猜测的回调路径**并把 webhook 只当唤醒信号。

### 11.3 待确认（必须先实测，不能推断）

1. **`gpt-image-2` 的 `owned_by` / 供给身份**（`GET /v1/models?expand=category` 可答）——**最高优先级**。
2. **`Idempotency-Key` 是否对 `gpt-image-2` 生效**；重复提交同一 Key + 同 Body 是否重放原响应还是二次计费。
3. **`gpt-image-2` 的任务响应实际是否返回 `usage`**（文档示例没有，但实现可能有）。
4. `n` 的真实可接受范围（文档写 1，示例写 2）与参考图真实上限（15 还是 16）。
5. `gpt-image-2` 是否真的接受 **base64 data URI**（生成页 vs 上传页矛盾），以及 `POST /v1/uploads/images` 返回 URL 在生成接口中的可用性。
6. 状态机的真实取值集合（`submitted`/`pending`/`processing`/`in_progress` 到底哪些出现，终态是否含 `cancelled`）。
7. `409 idempotency_result_indeterminate` 与 `409`/`503` 在**创建**路径上是否真的会返回；`request_id` 是否在图像接口普遍返回。
8. `POST /v1/tasks/batch`（批量查询）与 `POST /v1/logs/export`（明细导出）的**请求/响应合同**，以及导出**是否含 task_id**——这决定能否实现逐笔对账。
9. `GET /v1/dashboard/billing/usage` 的合同。
10. **最低充值金额**、`gpt-image-2` 是否享有免费试用额度（站点有 `freeTrial` 机制）、是否要求实名。
11. 是否需要 `X-APIMart-Response-Version` 才能锁定响应结构，以及该头对 `gpt-image-2` 是否有效。
12. 失败退款的**到账时延**与「部分成功按张计费」在 `gpt-image-2` 上的实际表现。
13. 结果 URL 的真实有效期（文档只给 `expires_at` 字段，无固定时长描述）。
14. 5xx/超时之后上游的真实计费行为（是否部分计费）。
15. APIMart 的企业主体、注册地、发票/合规信息（本次未在文档中找到）。

---

## 12. 适配性评估：APIMart 是否适合作为第二阶段的第二 Provider

### 12.1 支持接入的理由

1. **服务确实在运营**，且文档活跃度极高（sitemap 最新 lastmod 为抓取前一天，账户用量页 lastmod 为抓取当天）。
2. **协议能力在关键一点上强于第一阶段**：创建即返回可查询的 `task_id`，`GET /v1/tasks/{id}` 能给出状态、结果与**上游声明的扣费金额**。这直接缓解了第一阶段「`/v1` 无 task id、响应丢失只能人工对账」的已知缺陷。
3. **设计了完整的幂等键语义**，包括一个专门用于「结果无法确认」的 `409 idempotency_result_indeterminate` 和明确声明「当前请求未执行」的 `503 idempotency_unavailable`——这种把「未受理」与「结果不明」分开表达的合同，恰好落在本仓库 `reconciliation_required` 的语义需求上。
4. **有账单/用量 API**（`GET /v1/usage`），口径与网站看板一致，且明确排除了失败与已退款任务。
5. **验证成本极低**（1K 单张约 $0.0085），受控付费验证完全可行。
6. 单一 endpoint 同时覆盖文生图与图生图（由 `image_urls` 决定），与「一个 `CreateImageGeneration` Command」的方向一致。

### 12.2 最大风险（按严重度排序）

**风险 1（致命级，针对本阶段的验证目标）：模型身份未证实，且第一方证据倾向「不是同一官方供给」。**
第二阶段的命题是「同一 Vendor Model 由多个 Offering 供应」。`gpt-image-2` 是第一方文档里**没有声明供应商**的名字，而 APIMart 另有一个名为 `gpt-image-2-official` 的模型才被写作 "OpenAI official `gpt-image-2` model"。加上计价维度不同（按张 vs 按 token）、渠道价仅为官方对照价约 4%、以及 `official_fallback`（「官方渠道兜底」）这个只有在默认非官方时才成立的字段——**用 `gpt-image-2` 来证明「同一 Vendor Model 多 Offering 供应」的前提本身就不成立**。这不是可以通过适配器绕开的问题，是选型前提问题。
*缓解*：先用真实 Key 查 `owned_by` 与 schema（几美分成本）再决定；若 `owned_by` 不是 `openai`，则应改用 `gpt-image-2-official` 作为该 Vendor Model 的第二个 Offering，或者换 Provider。

**风险 2（高）：这一 Offering 的计量证据只有金额、没有用量。**
`gpt-image-2` 的任务响应第一方示例只有 `cost` / `credits_cost`；分项 token `usage` 只挂在别的 model 名下；`GET /v1/usage` 是聚合口径、**不含 task_id**。这意味着结算可以做到「金额级对账」，但做不到「单次提交 ↔ 单笔扣费」的逐笔核验。若本平台把「可核验的计量事实」定义为 token/张数级别的强证据，那么 `gpt-image-2` **当前不满足**；如果接受「上游声明的扣费金额」作为证据，则满足但粒度较粗。
*缓解*：实测任务响应是否真无 `usage`；摸清 `POST /v1/logs/export` 是否含 task_id。

**风险 3（高）：`Idempotency-Key` 对 `gpt-image-2` 的生效范围未文档化。**
第 5 节显示这套语义只在一个 grok 模型页出现。若对该模型不生效，「响应丢失后安全重提」就退化回 AIHubMix 的处境（必须 `reconciliation_required`、人工对账）。这决定了 APIMart 相对第一阶段的**主要增益能否兑现**。

**风险 4（中）：模型级能力分布不同构，容易把 official 的能力误配到渠道版。**
`quality`、`background`、`moderation`、`output_format`、`output_compression`、`mask_url` 全部只在 `gpt-image-2-official`；`gpt-image-2` 只有 `size`/`resolution`/`n`/`image_urls`/`official_fallback`/`nsfw_check`。`n` 上限、参考图上限、mask 支持在两者间**全部不同**。这要求 Offering/Native Schema 必须按 model 名严格分开，不能按 Provider 合并。

**风险 5（中）：文档质量与合同稳定性差。**
本次核实发现 6 处第一方文档缺陷：`n` 自相矛盾、状态机 4 种写法、base64 支持声明冲突、上传页示例指向不存在的轮询路径、`openapi.json` 是 plant store 占位、英文 grok 页仅 3.9KB stub、落地页 `index` 严重滞后于目录（仍写 GPT-4o Image / Gemini 2.0）。加上**没有任何变更公告渠道**，任何按文档直接固化合同的做法都会随上游静默变化而失效。

**风险 6（中）：错误语义有陷阱。**
上游用 `500` + `build_request_failed: invalid size` 表达参数错误。若 Adapter 严格按状态码分类，会把「请求不合法、上游未受理」误判为「结果不明」，制造不必要的人工对账。

**风险 7（低–中）：可观测性与主体信息缺口。**
没有自有状态页（且 `status` 子域解析到第三方地址段）；没有找到企业主体、注册地、发票/合规、SLA 条款的可核验文本（「99.9% SLA」「50K+ 用户」都只是营销页自述）。作为**生产结算链路上的一环**，这些是运营层面的未知数。

### 12.3 结论

**APIMart 值得作为第二阶段的第二 Provider 候选，但不应在付费实测前就宣称它满足「同一 Vendor Model 多 Offering 供应」的验证目标。**

具体建议：

1. **先做 3 项零/极低成本核实（必须用真实 Key，不需要生成图片）**：
   - `GET /v1/models?expand=category` → `gpt-image-2` 的 `owned_by`；
   - `GET /v1/models/gpt-image-2/schema` → 权威参数 schema（一次性裁决 `n`、参考图上限、base64）；
   - `GET /v1/usage` 空转 → 确认返回结构。
2. **如果 `owned_by` 不是 OpenAI，或 schema 与官方模型不一致**：把这一 Offering 的定位从「同一 Vendor Model 的第二供给」改为「另一个 Vendor Model / 另一类供给」，或改用 `gpt-image-2-official` 承担该验证目标。**不要为了凑齐「多 Offering」而把一个身份不明的模型声明为同一 Vendor Model。**
3. **付费实测时把幂等作为第一优先级的验证项**（同一 Key + 同 Body 连发两次，观察是否重放同一 task_id）。这是 APIMart 相对 AIHubMix 最可能的关键增益，也是唯一必须付费才能确认的核心能力。
4. **实现层面**：Native Schema 按 **model 名**分版（`gpt-image-2` 与 `gpt-image-2-official` 不能共用一套字段）；状态机按「非终态集合 / 终态集合 + 未知状态继续轮询」实现，不写死某一页的枚举；错误映射对 `5xx` 增加 `build_request_failed` 前缀识别；参考图统一走**先上传换 URL**；所有 Provider 创建请求在无幂等保证前仍进 `reconciliation_required`，不得自动重提。

---

## 13. 来源列表

所有来源均为**第一方**（APIMart 自营站点/文档/接口）。抓取时间统一为 **2026-09-19（UTC 03:50–04:15 / UTC+8 11:50–12:15）**，除非另有说明。

### 13.1 第一方文档（`docs.apimart.ai`）

| 页面 | URL | 页面 lastmod | 本文用途 |
| --- | --- | --- | --- |
| 文档索引 | https://docs.apimart.ai/llms.txt | — | 站点结构、语言、页面总量 |
| 英文 API 手册索引 | https://docs.apimart.ai/_llms/en/api-manual.md | — | 端点与模型目录全集（152 页） |
| 中文文档索引 | https://docs.apimart.ai/_llms/cn.md | — | 145+ 页中文目录 |
| Sitemap | https://docs.apimart.ai/sitemap.xml | 最新 2026-09-18T09:08:08Z | 运营活跃度、页面 lastmod、确认无 changelog/状态页 |
| GPT-Image-2 图像生成（英） | https://docs.apimart.ai/en/api-reference/images/gpt-image-2/generation.md | 2026-09-01T06:31:28Z | 渠道版请求合同、错误、n/参考图冲突 |
| GPT-Image-2 图像生成（中） | https://docs.apimart.ai/cn/api-reference/images/gpt-image-2/generation.md | 2026-09-01 | 同上（中文原文，用于与旧快照对照） |
| GPT-Image-2 官方渠道生成（英） | https://docs.apimart.ai/en/api-reference/images/gpt-image-2/official.md | 2026-08-21T08:30:09Z | official 请求合同、**task 响应 `usage` 分项**、模型身份措辞 |
| GPT-Image-2.5 生成 | https://docs.apimart.ai/en/api-reference/images/gpt-image-2.5/generation.md | 2026-09-09T06:35:41Z | 新一代合同、`usage` 简版、失败退款、`/v1/tasks/batch` |
| GPT-Image-1 生成 | https://docs.apimart.ai/en/api-reference/images/gpt-image-1/generation.md | — | 对照（无 usage） |
| 获取任务状态（英） | https://docs.apimart.ai/en/api-reference/tasks/status.md | 2026-08-25T04:02:31Z | 任务字段、状态枚举、`cost`/`credits_cost`、`language` |
| 获取任务状态（中） | https://docs.apimart.ai/cn/api-reference/tasks/status.md | 2026-08-25 | 同上（中文） |
| 任务完成回调（英） | https://docs.apimart.ai/en/api-reference/tasks/webhook.md | 2026-08-25T04:02:31Z | webhook 拼接、重试、去重、**无签名** |
| 任务完成回调（中） | https://docs.apimart.ai/cn/api-reference/tasks/webhook.md | 2026-08-25 | 同上（中文） |
| 查询消费用量 | https://docs.apimart.ai/en/api-reference/account/usage.md | **2026-09-18T09:08:08Z** | `GET /v1/usage` 合同与计费口径 |
| 查询令牌余额 | https://docs.apimart.ai/en/api-reference/account/token-balance.md | 2026-06-15T03:24:44Z | 余额查询入口 |
| 查询用户余额 | https://docs.apimart.ai/en/api-reference/account/user-balance.md | 2026-06-15T03:24:44Z | 余额查询入口 |
| 上传图片 | https://docs.apimart.ai/en/api-reference/uploads/images.md | — | 上传换 URL、**base64 弃用声明**、过期示例 |
| Models List Metadata API | https://docs.apimart.ai/en/api-reference/texts/models/list.md | — | `expand` 参数、**`owned_by` 字段**、单模型 schema 端点 |
| Grok Imagine 2.0 官方（中，全量） | https://docs.apimart.ai/cn/api-reference/images/grok-imagine-2.0-ext/official.md | — | **`Idempotency-Key` / `X-APIMart-Response-Version` 完整语义**、统一错误结构 |
| Grok Imagine 2.0 官方（英，stub） | https://docs.apimart.ai/en/api-reference/images/grok-imagine-2.0-ext/official.md | — | 英文页不完整（仅 3.9KB）的佐证 |
| Quick Start | https://docs.apimart.ai/en/quickstart.md | — | 注册与 API Key 创建流程 |
| Development Guide | https://docs.apimart.ai/en/development.md | — | 集成指引 |
| 落地页 | https://docs.apimart.ai/en/index.md | — | 平台自述（注意其内容滞后于目录） |
| FAQs | https://docs.apimart.ai/en/faqs.md | 2026-05-29T09:55:21Z | FAQ 结构 |
| FAQ · Account Management | https://docs.apimart.ai/en/faqs/account-management.md | 2026-01-22 | 用量/费用查看入口、支持渠道 |
| FAQ · Connection & Usage | https://docs.apimart.ai/en/faqs/connection-usage.md | 2026-01-22 | 端点地址确认 |
| FAQ · Security & Configuration | https://docs.apimart.ai/en/faqs/security-configuration.md | 2026-01-22 | API Key 管理建议 |
| OpenAPI Spec（实为占位） | https://docs.apimart.ai/api-reference/openapi.json | — | **证明不存在真实 OpenAPI 合同** |

### 13.2 第一方站点（`apimart.ai`）

| 页面 | URL | 用途 |
| --- | --- | --- |
| 定价中心（中） | https://apimart.ai/zh/pricing | `gpt-image-2` / `gpt-image-2-official` 价格与计价单位、Credits↔USD |
| 定价（英） | https://apimart.ai/pricing | 语言无关的价格页地址（FAQ 中的规范链接） |
| GPT Image 2 模型页 | https://apimart.ai/model/gpt-image-2 | 营销措辞、两个变体页签、无供给来源声明 |
| 模型市场（图像） | https://apimart.ai/zh/model?type=image | 模型目录与 `/model/...` 详情页 URL |
| API 密钥页 | https://apimart.ai/keys | Key 创建入口（配额/模型限制/IP 白名单） |
| 充值&账单 | https://apimart.ai/zh/billing | 支付方式、套餐、最低金额标签 |
| 站点首页 | https://apimart.ai/ | 站点在线核实 |

### 13.3 第一方接口探测（无凭证，**未产生任何计费调用**）

对 `https://api.apimart.ai` 的无凭证探测结果（2026-09-19）：

| 路径 | 结果 | 用途 |
| --- | --- | --- |
| `GET /v1/models` | 401 | 服务在线、鉴权生效 |
| `GET /v1/usage` | 401 | 同上 |
| `GET /v1/nonexistent-path-xyz` | 404 | 对照：不存在的路径返回 404，可区分「存在但需鉴权」与「不存在」 |
| `GET /v1/tasks/task_abc` | 401 | 任务查询端点存在 |
| `GET /v1/tasks/batch`（POST） | 401 | 批量查询端点存在（无文档） |
| `GET /v1/logs/export`（POST） | 401 | 日志导出端点存在（无文档） |
| `GET /v1/dashboard/billing/usage` | 401 | 累计消费端点存在（无文档） |
| `GET /v1/models/gpt-image-2/schema` | 401 | 单模型 schema 端点存在 |
| `GET /v1/model-schema?model=gpt-image-2` | 401 | 同上（查询参数形式） |
| `GET /v1/images/generations/task_abc` | **404** | **证明上传页示例的轮询路径不存在** |

### 13.4 本地旧快照（仅供对照，不构成当前合同）

- `out-reference/apimart/generation.md`（旧仓库快照，2026-08-04）：`n` 1–10、参考图 16、无 `nsfw_check`——**均已与当前第一方文档不一致**。
- `out-reference/apimart/status.md`（旧仓库快照，2026-08-04）：任务查询字段与 `cost`/`credits_cost` 与当前文档基本一致。
- `out-reference/apimart/webhook.md`（旧仓库快照，2026-08-04）：webhook 拼接、重试次数、去重语义与当前文档一致；当前文档新增了顶层 `language` 字段与更完整的失败 `error` 结构（`param`/`code`）。
- 三份快照顶部的「已退场」标注为**旧仓库产品决策**，不反映 APIMart 服务状态。

### 13.5 说明

- 本文**未引用第三方博客或转述**。第 2 节起的所有结论均可回溯到上表的第一方 URL。
- 本文**未执行任何付费调用**，因此第 11.3 节列出的 15 项「待确认」在本文范围内**无法闭环**；它们需要真实 API Key 的受控验证（第 9.4 节给出了最小验证集建议）。
- 本文与 `generation.md` / `status.md` / `webhook.md` 的关系是**补充现状核实与评估**，不替代、不修改这三份既有快照。
