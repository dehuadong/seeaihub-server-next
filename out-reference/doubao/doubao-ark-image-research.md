# 火山方舟 Doubao / Volcengine Ark 图片生成（Seedream）第一方协议调研

> 调研日期：2026-09-19（UTC+8）
> 用途：为第二阶段「接入第二个 Provider」提供第一方事实依据
> 性质：上游协议研究，不是本平台对外接口合同；不参与构建，也不进入 cargo 验证
> 纪律：本文不记录真实 API Key、AK/SK、Bearer token、Endpoint ID、任务 ID、短期结果 URL。所有请求示例均使用文档中的公开发布值。

---

## 1. 调研范围与实际取证方式

本次调研对象是火山方舟（Volcengine Ark）**图片生成**能力，重点模型为 Doubao Seedream 5.0 系列。

取证方式说明（重要，影响可信度判断）：

- 方舟官方文档站 `https://docs.volcengine.com/docs/82379/<id>` 是服务端渲染的单页应用。文档正文以「Quill Delta JSON」形式内嵌在 HTML 的 `curDoc.Content` 字段里，纯 `web_fetch` 只能拿到页面骨架。
- 因此本次调研先用 HTTP 直接下载官方文档 HTML（HTTP 200），再用自建脚本从内嵌 Delta 中还原正文文本，逐条比对。所有「第一方正文」结论都来自还原后的官方页面文本，不是第三方转述。
- 官方文档导航树（`https://docs.volcengine.com/docs/ark`）也被完整还原，用于回答「有哪些 API 存在、哪些不存在」这类**结构性**问题——这一点对第 3 节的异步接口判断是决定性的。
- 只读探测（无凭证）直接打到 `ark.cn-beijing.volces.com`，用于确认端点存在性与错误形状；未执行任何付费调用。

抓取时间：除特别标注外，均为 **2026-09-19（UTC+8）**。

### 1.1 已核实的第一方页面清单

| 页面 | URL | 还原后正文字数 | 2026-09-19 抓取 |
| --- | --- | --- | --- |
| 图片生成 API | https://docs.volcengine.com/docs/82379/1541523 | 13,942 | ✅ 成功 |
| 图片生成流式响应事件 | https://docs.volcengine.com/docs/82379/1824137 | 4,523 | ✅ 成功 |
| 图片生成教程（Seedream 4.0–5.0） | https://docs.volcengine.com/docs/82379/1824121 | 73,375 | ✅ 成功 |
| Doubao Seedream 5.0 pro 教程 | https://docs.volcengine.com/docs/ark/seedream-5-0-pro | 30,003 | ✅ 成功 |
| 模型价格（含图片生成） | https://docs.volcengine.com/docs/82379/1544106 | 18,811 | ✅ 成功 |
| 模型服务计费说明 | https://docs.volcengine.com/docs/ark/model-service-pricing | 4,764 | ✅ 成功 |
| 创建视频生成任务 | https://docs.volcengine.com/docs/82379/1520757 | 16,971 | ✅ 成功（用于对照） |
| Base URL 及鉴权 | https://docs.volcengine.com/docs/82379/1298459 | 2,344 | ✅ 成功 |
| 获取 API Key 并配置 | https://docs.volcengine.com/docs/82379/1541594 | 859 | ✅ 成功 |
| 免费推理额度 | https://docs.volcengine.com/docs/82379/1399514 | 1,245 | ✅ 成功 |
| 官方文档导航树 | https://docs.volcengine.com/docs/ark | — | ✅ 成功 |

抓取失败的页面，如实记录：

| 页面 | 现象 | 处理 |
| --- | --- | --- |
| `https://docs.volcengine.com/docs/82379/1299023`（错误码，搜索命中的 ID） | 只是 11 KB 的 SPA 骨架，无正文 | 改用本地旧快照 `./error-code.md` 与实测 401 错误体交叉验证，见 §9 |
| `https://docs.volcengine.com/docs/ark/free-inference-quota` 等 slug 形式 | 部分 slug 返回骨架 | 改用 ID `1399514` 成功取得正文 |
| `https://www.volcengine.com/docs/82379/1541523` | 跨域重定向到 `docs.volcengine.com` | 已改用目标域名重新抓取 |
| `https://docs.volcengine.com/docs/ark/api-list` | 返回骨架，无正文 | 改用导航树还原 API 清单，见 §3 |

---

## 2. 本地旧快照与现网第一方文档的差异

本地 `out-reference/doubao/` 下的快照是**旧版**方舟文档，与 2026-09-19 的现网文档存在实质差异。以下逐项列出。

| 主题 | 本地旧快照 | 2026-09-19 现网第一方 | 差异性质 |
| --- | --- | --- | --- |
| 请求字段 `prompt` | 标为**必选** | 图层拆分场景下 `prompt` 为**可选**；`image` 在图层拆分场景为必选 | **合同变化**；「必选」不再是全局性质 |
| `background` 字段 | 无 | 新增 `background`（`transparent`/`opaque`，默认 `opaque`），仅 Seedream 5.0 pro、仅图生图且输入单张带透明通道图片 | **新增字段** |
| `layer_decomposition` 字段 | 无 | 新增 `layer_decomposition`（boolean，默认 `false`），仅 Seedream 5.0 pro；开启后 `image` 必选且仅 1 张 | **新增字段**，且是重要的能力分支 |
| 图层拆分相关响应字段 | 无 | 新增 `data[].z_index`、`data[].name`、`data[].description`、`data[].bounding_box.{absolute,normalized}` | **新增字段** |
| Seedream 5.0 pro 的 `size` | 仅 `1K`/`2K` | 图片生成场景 `1K`/`1.5K`/`2K`（默认 `2K`）；图层拆分场景 `1K`/`1.5K`/`2K`/`auto`（默认 `auto`）；1.5K 与 1K **同价** | **能力与计价变化** |
| 各模型 `max_images` 支持 | lite/4.5/4.0 支持 | 现网 API 页只在 lite 名下标注 `sequential_image_generation_options`；4.5/4.0 仍支持 `sequential_image_generation` | **需要逐一核实**，见 §11 |
| `output_format` 支持模型 | Seedream 5.0 pro、5.0 lite | 现网 API 页仍写 pro + lite；但教程能力矩阵里 Seedream 4.5 也标 `png, jpeg` | **文档内部不一致**，见 §11 |
| 图片输入总像素下限 | `> 14 px` 宽高，「总像素 ≤ 3600 万」 | 图片生成场景总像素 `[196, 6000×6000（3600万）]`；图层拆分场景总像素 `[512×512, 6000×6000]`；格式要求也不同 | **约束细化** |
| 单价档位（pro 输出） | `≤ 236万像素：0.30` / `> 236万像素：0.60` | `≤ 261万像素（分辨率 1.5K 及以下）：0.30` / `> 261万像素（分辨率 1.5K 以上）：0.60`；**并区分「单图生成场景」与「图层拆分场景」两套价** | **计价合同变化**，见 §6 |
| 图层拆分层级计价规则 | 无 | 「图层拆分场景，同一次请求输出的图层可能分别落在不同像素档位，按每个图层实际像素档位单独计费」 | **新增计价规则** |
| 流式限额预扣规则 | 无 | 「图层拆分场景下，每次请求预扣减 17 IPM（按最多输出 1 张底图和 16 张图层预留配额）；全部图片生成后，按实际生成数量返还多扣减的额度」 | **新增限流规则** |
| IPM 数值 | 「RPM 限流」 | 教程能力矩阵给出 **IPM 500（张/分钟）** | **术语与数值明确化**，见 §11 |
| `usage.generated_images` 语义 | 「成功生成图片数（计费依据）」 | 「模型成功生成的图片张数，不包含生成失败的图片。仅对成功生成图片按张数进行计费」 | 语义一致，**现网表述更明确** |
| 错误码页 | `error-code.md` 快照 | 现网 ID `1299023` 返回骨架，无法直接核对 | **无法逐字核实**，见 §9 与 §11 |

**小结**：本地旧快照**不能**作为当前合同使用。特别是 `size` 档位、像素档位阈值（236万 → 261万）、以及图层拆分引入的**第二套计价规则与第二套响应结构**，都是本地快照完全没有的。

来源：[图片生成 API](https://docs.volcengine.com/docs/82379/1541523) · [模型价格](https://docs.volcengine.com/docs/82379/1544106) · [Seedream 5.0 pro 教程](https://docs.volcengine.com/docs/ark/seedream-5-0-pro) · [本地旧快照](./图片生成模型API调用指南.md) · 抓取时间 2026-09-19

---

## 3. 图片生成接口合同

### 3.1 端点与鉴权

| 项目 | 结论 | 性质与来源 |
| --- | --- | --- |
| 端点 | `POST https://ark.cn-beijing.volces.com/api/v3/images/generations` | **事实**：[图片生成 API](https://docs.volcengine.com/docs/82379/1541523) 首行；2026-09-19 无凭证探测该路径返回结构化 401（而非 404），说明路由存在 |
| 数据面 Base URL | `https://ark.cn-beijing.volces.com/api/v3` | **事实**：[Base URL 及鉴权](https://docs.volcengine.com/docs/82379/1298459) |
| 管控面 Base URL | `https://ark.cn-beijing.volcengineapi.com/` | **事实**：同上（本轮不使用） |
| 鉴权方式 A | `Authorization: Bearer $ARK_API_KEY` | **事实**：[图片生成 API](https://docs.volcengine.com/docs/82379/1541523) · [Base URL 及鉴权](https://docs.volcengine.com/docs/82379/1298459) |
| 鉴权方式 B | Access Key 签名鉴权（HMAC-SHA256，Service=`ark`，Region=`cn-beijing`）；**此方式下 `model` 必须填 Endpoint ID** | **事实**：[Base URL 及鉴权](https://docs.volcengine.com/docs/82379/1298459) |
| API Key 数量上限 | 一个主账号 50 个；可按项目隔离，可限制可鉴权的 Model ID 与来源 IP | **事实**：[获取 API Key 并配置](https://docs.volcengine.com/docs/82379/1541594) |
| API Key 明文格式 | **2026-09-17 12:00（UTC+8）之后创建的 ModelArk API Key 使用新明文格式**；旧 Key 继续有效 | **事实**：同上。这是很新的变化，凭证校验逻辑不应硬编码旧格式前缀 |

无凭证探测实测（2026-09-19，只读、无费用）：

```
GET  https://ark.cn-beijing.volces.com/api/v3/models                       => HTTP 401
GET  https://ark.cn-beijing.volces.com/api/v3/contents/generations/tasks   => HTTP 401
POST https://ark.cn-beijing.volces.com/api/v3/images/generations          => HTTP 401
响应头 x-request-id: 02…（本机实测值，已脱敏；形如 02 + 十六进制串）
响应体: {"error":{"code":"AuthenticationError","message":"the API key or AK/SK in the request is missing or invalid. request id: 02…（同上，已脱敏）","param":"","type":"Unauthorized"}}
```

**事实**：错误体形状为 `{"error":{"code","message","param","type"}}`，请求 ID 同时出现在 `x-request-id` 响应头和 `message` 文本末尾（`request id: <id>`）。**与我方推测**：该 Request ID 可用于向方舟对账/提单，见 §9。

**遗漏说明**：`GET /api/v3/models` 在 2026-09-19 返回 401（不是 404），只能证明该路径受鉴权保护；**待确认**它是否为文档化的「模型列表」接口。

### 3.2 请求字段（现网第一方）

| 字段 | 类型 / 取值 | 默认值 | 必选性 | 约束与备注 | 性质与来源 |
| --- | --- | --- | --- | --- | --- |
| `model` | string | — | **必选** | 「模型 ID（模型名称-版本）」；也可用 Endpoint ID 以获得限流、计费类型、运行状态查询等能力 | **事实**：[图片生成 API](https://docs.volcengine.com/docs/82379/1541523) |
| `prompt` | string | — | 图片生成场景必选；**图层拆分场景可选** | 中英文；建议中文 ≤300 字、英文 ≤600 词。Seedream 5.0 pro 额外支持 14 种语言 | **事实**：同上 |
| `image` | string / string[] | — | 图层拆分场景必选（且仅 1 张） | 支持 URL 或 Base64（`data:image/<小写格式>;base64,<...>`） | **事实**：同上 |
| `size` | string | 见 §3.3 | 否 | 分辨率档位或 `宽x高`，**不可混用** | **事实**：同上 |
| `optimize_prompt_options.mode` | string：`standard` / `fast` | `standard` | 否 | `fast` 耗时更短；**Seedream 5.0 lite / 4.5 当前不支持 `fast`** | **事实**：同上 |
| `output_format` | string：`png` / `jpeg` | **`jpeg`** | 否 | 仅 Seedream 5.0 pro、5.0 lite；图层拆分场景下只控制底图，图层恒为 PNG | **事实**：同上 |
| `response_format` | string：`url` / `b64_json` | **`url`** | 否 | `url` 链接生成后 24 小时内有效 | **事实**：同上 |
| `sequential_image_generation` | string：`auto` / `disabled` | **`disabled`** | 否 | 仅 Seedream 5.0 lite / 4.5 / 4.0；**Seedream 5.0 pro 不支持** | **事实**：同上 |
| `sequential_image_generation_options.max_images` | integer，`[1,15]` | `15` | 否 | 「输入参考图数量 + 最终生成图片数量 ≤ 15」 | **事实**：同上 |
| `stream` | boolean | `false` | 否 | 仅 Seedream 5.0 lite / 4.5 / 4.0 | **事实**：同上 |
| `tools[].type` | string，当前仅 `web_search` | — | 否 | 仅 Seedream 5.0 lite；实际次数见 `usage.tool_usage.web_search` | **事实**：同上 |
| `watermark` | boolean | **`true`** | 否 | `true` 时在右下角加「AI 生成」水印 | **事实**：同上 |
| `background` | string：`transparent` / `opaque` | `opaque` | 否 | **仅 Seedream 5.0 pro**；仅图生图，且只支持输入 1 张带透明通道的图片；透明模式下输出默认 png，若同配 `output_format=jpeg` 会**报错** | **事实**：同上 |
| `layer_decomposition` | boolean | `false` | 否 | **仅 Seedream 5.0 pro**；`true` 时把单图拆成 1 张底图 + 最多 16 个图层 | **事实**：同上 |
| `n`（OpenAI 风格张数） | — | — | — | **第一方文档中不存在该字段**（只在 `sequential_image_generation_options.max_images` 上出现「最大生成数量」）。**待确认**是否被兼容接受 | **事实（不存在）** + **待确认（兼容性）**：同上，全文检索未见顶层 `n` |

### 3.3 `size` 与像素/宽高比约束（现网第一方逐模型）

**输入参考图的公共约束**（**事实**，[图片生成 API](https://docs.volcengine.com/docs/82379/1541523)）：

- 数量：Seedream 5.0 pro 最多 **10 张**；Seedream 5.0 lite / 4.5 / 4.0 最多 **14 张**。
- 格式（图片生成场景）：`jpeg`、`png`、`webp`、`bmp`、`tiff`、`gif`、`heic`、`heif`。
- 宽高比：`[1/16, 16]`；宽高长度（px）：`> 14`；单张大小：`≤ 30 MB`。
- 总像素（图片生成场景）：`[196, 6000×6000（3600万）]`。
- 图层拆分场景（仅 pro）单图要求更窄：格式仅 `png`/`jpeg`，总像素 `[512×512（262144）, 6000×6000]`。

| 模型 | 方式1（分辨率档位） | 方式2（`宽x高`）默认值 | 方式2 总像素范围 | 宽高比范围 |
| --- | --- | --- | --- | --- |
| Seedream 5.0 pro（**图片生成**） | `1K`/`1.5K`/`2K`，**默认 `2K`** | — | `[1280x720（921600）, 2048x2048×1.1025（4624220）]` | `[1/16, 16]` |
| Seedream 5.0 pro（**图层拆分**） | `1K`/`1.5K`/`2K`/`auto`，**默认 `auto`** | — | 同上（`auto` 按输入尺寸自适应，下限 1K、上限 2K） | 同上 |
| Seedream 5.0 lite | `2K`/`3K`/`4K` | `2048x2048` | `[2560x1440（3686400）, 4096x4096（16777216）]` | `[1/16, 16]` |
| Seedream 4.5 | `2K`/`4K` | `2048x2048` | `[2560x1440（3686400）, 4096x4096（16777216）]` | `[1/16, 16]` |
| Seedream 4.0 | `1K`/`2K`/`4K` | `2048x2048` | `[1280x720（921600）, 4096x4096（16777216）]` | `[1/16, 16]` |

**关键约束（事实）**：采用方式 2 时**必须同时满足**总像素范围与宽高比范围两个区间；文档明确「总像素是对单张图宽度和高度的像素乘积限制，而不是对宽度或高度的单独值进行限制」。官方有效/无效示例：

- 有效：`2048x1024`（2,097,152 ∈ [921600, 4624220]，宽高比 2 ∈ [1/16,16]）
- 无效：`512x512`（262,144 < 921600）
- lite 有效：`3750x1250`；lite 无效：`1500x1500`（2,250,000 < 3,686,400）

**★ 对平台最关键的推论**：`size` 的有效性不是「枚举白名单」，而是**两个连续区间的合取**。平台若把 `size` 建模成跨厂商枚举，会同时丢掉 (a) 任意合法像素组合、(b) 分辨率档位与像素值不可混用这条规则、(c) 各模型区间差异。它更适合作为一个**带模型级区间校验的原生字符串字段**。

来源：[图片生成 API](https://docs.volcengine.com/docs/82379/1541523) · [图片生成教程](https://docs.volcengine.com/docs/82379/1824121) · [Seedream 5.0 pro 教程](https://docs.volcengine.com/docs/ark/seedream-5-0-pro) · 抓取时间 2026-09-19

### 3.4 当前可用的 Seedream 模型 ID

第一方文档中出现的**准确 model id 字符串**（**事实**，[图片生成教程](https://docs.volcengine.com/docs/82379/1824121)、[Seedream 5.0 pro 教程](https://docs.volcengine.com/docs/ark/seedream-5-0-pro)、[图片生成 API](https://docs.volcengine.com/docs/82379/1541523)）：

| 模型 | 文档中的 model id 字符串 | 备注 |
| --- | --- | --- |
| Doubao Seedream 5.0 pro | `doubao-seedream-5-0-pro-260628` | 教程明确「Model ID: doubao-seedream-5-0-pro-260628」 |
| Doubao Seedream 5.0 lite | `doubao-seedream-5-0-lite-260128` | 教程示例与能力矩阵均使用此字符串 |
| Doubao Seedream 5.0（别名） | `doubao-seedream-5-0-260128` | 能力矩阵写「doubao-seedream-5-0-260128（同时支持：doubao-seedream-5-0-lite-260128）」 |
| Doubao Seedream 4.5 | `doubao-seedream-4-5-251128` | |
| Doubao Seedream 4.0 | `doubao-seedream-4-0-250828` | |

- **事实**：`model` 的格式约定为 `<模型名称>-<版本>`；流式事件文档也重复了这一格式描述。
- **事实**：`usage.model` / 响应 `model` 回显的是**本次请求实际使用的模型 ID**，含版本后缀。
- **推论**：平台若保留厂商原生语义，`model` 字段应固定为上述**带版本后缀的完整字符串**（如 `doubao-seedream-5-0-pro-260628`），而不是裸名 `doubao-seedream-5-0-pro`。理由：(a) 限流是「同模型（区分模型版本）」维度；(b) 计价随版本/场景变化；(c) 响应回显带版本，便于对账。
- **待确认**：裸名（不带版本后缀）是否被 `/api/v3/images/generations` 接受。文档要求「查询 Model ID」，但未说裸名会被拒。

来源：同上 · 抓取时间 2026-09-19

---

## 4. 异步任务能力：**不存在图片异步任务接口**（本节为最关键结论之一）

### 4.1 结论

**事实（结构性证据）**：2026-09-19 还原的方舟官方文档导航树（`https://docs.volcengine.com/docs/ark`）中，各内容生成模态的 API 文档集合如下：

| 模态 | 官方文档中的 API 页面（文档 ID / slug） | 是否具备「创建任务 + 查询任务」 |
| --- | --- | --- |
| **图片生成** | `1666945 / image-generation-api-reference`（图片生成 API）、`1541523 / image-generation-api`（图片生成 API）、`1824137 / image-generation-streaming-responses`（图片生成流式响应事件） | **否**。只有一个同步 POST + 一个 SSE 事件模型 |
| 视频生成 | `1520757 / create-video-generation-task-api`（创建视频生成任务）、`1521309 / get-video-generation-task-api`（查询视频生成任务）、`1521675 / list-video-generation-tasks-api`（查询视频生成任务列表）、`1521720 / cancel-or-delete-video-generation-tasks-api`（取消或删除视频生成任务） | **是**，四件套齐全 |
| 3D 生成 | `1856293`（创建）、`1860231`（查询）、`1860235`（列表） | **是** |

**结论**：方舟对视频与 3D 生成提供完整的异步任务 API（`POST /api/v3/contents/generations/tasks` + `GET /api/v3/contents/generations/tasks/{id}`），但**图片生成没有任何异步任务 API**。图片生成的官方契约只有两条通道：

1. `POST /api/v3/images/generations` —— **同步**返回最终结果（或整体错误）；
2. `POST /api/v3/images/generations` + `stream: true` —— 以 **SSE** 推送 `image_generation.partial_succeeded` / `image_generation.partial_failed` / `image_generation.completed` / `error` 事件。

**旁证（事实）**：官方视频文档明确写「创建视频生成任务为异步接口，获取 ID 后需要通过 查询视频生成任务 API 来查询任务状态」；而图片生成 API 页没有任何「任务」「task」「异步」字段或状态机描述，也没有任务 ID 字段。二者对比强烈。

**旁证（事实）**：无凭证探测 `GET /api/v3/contents/generations/tasks` 在 2026-09-19 返回 401（端点存在，即视频任务列表端点），而**没有任何文档化的图片任务端点可供探测**——这不是遗漏，是因为它不存在。

来源：[官方文档导航树](https://docs.volcengine.com/docs/ark) · [图片生成 API](https://docs.volcengine.com/docs/82379/1541523) · [图片生成流式响应事件](https://docs.volcengine.com/docs/82379/1824137) · [创建视频生成任务](https://docs.volcengine.com/docs/82379/1520757) · [本地旧快照：查询视频生成任务](./查询视频生成任务.md) · 抓取时间 2026-09-19

### 4.2 这对平台意味着什么

- **推论（高置信）**：**火山方舟图片生成无法提供「可恢复的 Provider 侧任务 id」**。因此它**不能**用来满足「确认某次提交是否真的被受理」这一需求——这正是阶段二要验证的能力之一，而方舟在这一维度上是**结构性缺失**，不是配置问题。
- **推论**：SSE 通道也不能充当恢复手段。SSE 是一条**与请求同生命周期**的长连接；连接中断后官方没有给出「按请求 ID 重连并回放」的接口。若连接在 `partial_succeeded` 之后、`image_generation.completed` 之前断开，本地将处于「已可能产生费用但拿不到汇总 usage」的状态。
- **推论**：因为缺少任务查询，方舟的「受理不确定」只能靠**本地 Job 状态 + 幂等约束**兜住，不能靠 Provider 对账兜住。这与阶段一 AIHubMix 的 `/v1` 分支处境相同（AIHubMix 的 `/ai/v1` 至少有任务列表可查），但方舟连「列表找回」都没有。

### 4.3 流式事件契约（作为唯一可用的「渐进观测」手段）

**事实**（[图片生成流式响应事件](https://docs.volcengine.com/docs/82379/1824137)）：`stream: true` 时以 SSE 推送四类事件：

| 事件 | 语义 | 关键字段 |
| --- | --- | --- |
| `image_generation.partial_succeeded` | 单张图成功 | `type`、`model`、`created`、`image_index`（从 0 起）、`url`（仅 `response_format=url`）、`b64_json`（仅 `b64_json`）、`size` |
| `image_generation.partial_failed` | 单张图失败 | `type`、`model`、`created`、`image_index`、`error.{code,message}` |
| `image_generation.completed` | 请求汇总（结束事件） | `model`、`created`、`tools`（实际调用时返回）、`usage.{generated_images,output_tokens,total_tokens,tool_usage}` |
| `error` | 顶层整体失败 | `error.{code,message}` |

- **事实**：`image_generation.completed.usage.generated_images` =「模型成功生成的图片张数，**不包含生成失败的图片**。仅对成功生成图片按张数进行计费」。
- **事实**：`partial_failed` 的示例错误码为 `OutputImageSensitiveContentDetected`（输出图命中敏感内容）。
- **事实**：`stream` 仅 Seedream 5.0 lite / 4.5 / 4.0 支持；**Seedream 5.0 pro 不支持流式**。

**推论**：对流式可用模型，SSE 至少能在**部分图片已成功**时就拿到 `partial_succeeded` 的 `url`/`size`，比同步接口在「整体失败"时丢掉全部信息要好。但 5.0 pro 无此通道。

---

## 5. 计量证据评估：`usage` 逐项判定

### 5.1 同步响应完整字段

**事实**（[图片生成 API](https://docs.volcengine.com/docs/82379/1541523)）：

| 字段 | 类型 | 现网第一方语义 | 性质判定 |
| --- | --- | --- | --- |
| `created` | integer | 请求创建时间 Unix 秒 | 非计量 |
| `model` | string | 本次请求使用的模型 ID | 用于对账归属 |
| `data[].url` | string | 图片 URL（`response_format=url`），24h 有效 | 交付，非计量 |
| `data[].b64_json` | string | 图片 Base64（`response_format=b64_json`） | 交付，非计量 |
| `data[].size` | string | 图像宽高像素值 `<宽>x<高>`，如 `2048x2048` | **可用于推导像素档位** |
| `data[].output_format` | string | `png` 或 `jpeg`（仅 Seedream 5.0 pro） | 非计量 |
| `data[].error` | object | 单张图失败信息（组图场景） | 失败证据 |
| `data[].z_index` / `.name` / `.description` / `.bounding_box.{absolute,normalized}` | — | 图层拆分场景返回 | 交付元数据 |
| `tools` | object[] | 本次实际被调用的工具列表 | 辅助 |
| `error` | object | 顶层错误，整个请求未生成任何图片时返回 | 失败证据 |
| `usage.generated_images` | integer | 「模型成功生成的图片张数，不包含生成失败的图片。**仅对成功生成图片按张数进行计费**」 | **★ 可核验的计量事实** |
| `usage.input_images` | integer | 输入模型的图片张数；**仅 Seedream 5.0 pro 返回** | **★ 与计价直接相关（pro 输入图有价）** |
| `usage.output_tokens` | integer | 「模型生成的图片花费的 token 数量。计算逻辑为：`sum(图片长 * 图片宽) / 256` 后取整」 | **★ 语义明确：是像素面积的确定性换算，不是模型实际消耗** |
| `usage.total_tokens` | integer | 「本次请求消耗的总 token 数量。**当前不计算输入 token，故与 `output_tokens` 值一致**」 | **冗余字段** |
| `usage.tool_usage.web_search` | integer | 联网搜索实际调用次数；**仅开启联网搜索时返回（即仅 Seedream 5.0 lite）** | 计量事实（但见 §6 计价） |

- **事实**：`image_generation.completed.usage` 与同步 `usage` 字段集合一致（`generated_images`/`output_tokens`/`total_tokens`/`tool_usage`/`input_images`）。
- **待确认**：第一方文档没有说明 `usage` 是否在所有模型上都返回同一字段集合，只逐项标注了「模型支持」。因此**不能假定字段存在性恒定**——例如非 pro 模型不返回 `input_images`、非 lite 模型不返回 `tool_usage`。

### 5.2 逐项判定：它是否构成可核验的计量事实

| 字段 | 可核验？ | 说明 |
| --- | --- | --- |
| `generated_images` | **是** | 有明确计费语义（只算成功张数），是**按张计费的直接计价量**。可在平台侧用「实际归档成功的结果数」交叉核对。 |
| `input_images` | **部分是** | 是输入张数的权威陈述，但平台**自己就知道**提交了几张参考图，因此它是**可交叉验证**而非独立证据。它的价值在于确认 pro 的输入图计费档位。 |
| `output_tokens` | **否（作为成本证据）** | 文档自认它是 `sum(宽×高)/256` 的**确定性换算**。既然平台能从 `data[].size` 自行算出同一个数，它就不构成独立计量事实。它**不是模型实际消耗的 token**。 |
| `total_tokens` | **否** | 文档明说「当前不计算输入 token，故与 `output_tokens` 一致」——即信息量为零的冗余别名。**平台绝不能把它当作 token 计费的用量事实。** |
| `tool_usage.web_search` | **是（计数）**，但**计价归属待确认** | 是实际搜索次数的权威计数。但图片生成计价页（§6）没有给出「联网搜索」的图片侧单价条款，所以它当前更像**成本不透明项**而非可结算项。 |

### 5.3 失败 / 审核拦截是否返回用量、是否计费

| 场景 | 第一方表述 | 是否计费 | 性质 |
| --- | --- | --- | --- |
| 审核拦截导致**单张图**失败（组图场景） | 组图内某张图失败时，`data[]` 对应元素返回 `error`；若因审核不通过，**继续生成同请求内其他图片**；若因内部服务异常（500），**停止后续生成** | **不计费**（「因审核等原因未成功输出的图片不计费」） | **事实**：[图片生成 API](https://docs.volcengine.com/docs/82379/1541523) · [模型价格](https://docs.volcengine.com/docs/82379/1544106) |
| 整个请求失败（顶层 `error`） | 整个请求未能生成任何图片时返回顶层 `error` | **不计费** | **事实**：同上 |
| 流式场景单张失败 | `image_generation.partial_failed` 事件，`error.code` 如 `OutputImageSensitiveContentDetected` | **不计费** | **事实**：[图片生成流式响应事件](https://docs.volcengine.com/docs/82379/1824137) |
| 图层拆分场景部分图层失败 | 「任一图层生成失败，整体请求报错，**不支持部分成功**」 | **待确认** | **事实（不支持部分成功）** + **待确认（计费）**：[图片生成 API](https://docs.volcengine.com/docs/82379/1541523) |

- **事实**：计价页总则两条：「因审核等原因未成功输出的图片不计费」「仅对成功生成的视频计费。因审核等原因导致生成失败的，不收取费用」（后者针对视频，但同一页同一条款体系）。
- **推论**：`usage.generated_images` 与「只对成功图片计费」两条互相印证，构成一个**自洽的按张计价合同**：`应付金额 = f(generated_images, 每张所属像素档位, 场景, 输入图数量)`。
- **待确认（重要）**：文档**没有**承诺「请求失败时 `usage` 一定缺席」。若某次整体失败仍返回了 `usage.generated_images > 0`（例如部分成功但被判定整体失败），平台必须决定以哪个为准。**平台不应把「响应里有 usage」等价于「产生了费用」。**

来源：同上 · 抓取时间 2026-09-19

---

## 6. 计价合同

### 6.1 第一方价格页

| 项目 | 值 | 来源 |
| --- | --- | --- |
| 价格页 URL | https://docs.volcengine.com/docs/82379/1544106 （「模型价格」） | [模型价格](https://docs.volcengine.com/docs/82379/1544106) |
| 抓取时间 | 2026-09-19（UTC+8） | — |
| 计费说明页 | https://docs.volcengine.com/docs/ark/model-service-pricing | [模型服务计费说明](https://docs.volcengine.com/docs/ark/model-service-pricing) |

**事实**：计费说明页把「在线推理」的计费方式描述为「按 **token** 后付费（`prompt_token`/`completion_token`/缓存）」；而**图片生成**的定价在价格页是**按张**的独立表格。二者不是同一套计费项。计费说明页自身也写「不同模型服务的计费项不同，具体请参考 模型服务价格」。

### 6.2 图片生成计价表（现网第一方）

**事实**（[模型价格](https://docs.volcengine.com/docs/82379/1544106)，抓取时间 2026-09-19）。表格列头为「模型名称 / 输入图单价（元/张）/ 输出图单价（元/张）」：

| 模型 | 输入图单价（元/张） | 输出图单价（元/张） | 备注 |
| --- | --- | --- | --- |
| `doubao-seedream-5-0-pro` | 首张免费；**第 2 张起：0.02** | **单图生成场景**：≤ 261 万像素（分辨率 1.5K 及以下）：**0.30**；> 261 万像素（分辨率 1.5K 以上）：**0.60** | 「按生图场景区分定价」 |
| `doubao-seedream-5-0-pro`（**图层拆分场景**） | 同上 | ≤ 261 万像素（分辨率 1.5K 及以下）：**0.15**；> 261 万像素（分辨率 1.5K 以上）：**0.30** | 图层拆分单价是单图生成的一半 |
| `doubao-seedream-5-0-lite` | **免费** | **0.22** | |
| `doubao-seedream-4-5` | 免费 | **0.25** | |
| `doubao-seedream-4-0` | 免费 | **0.20** | |

**事实**：价格页三条图片生成计价说明（原文）：

1. 「因审核等原因未成功输出的图片不计费。」
2. 「Seedream 5.0 pro 图层拆分场景，同一次请求输出的图层可能分别落在不同像素档位，**按每个图层实际像素档位单独计费**。」
3. 「Seedream 5.0 lite / 4.5 / 4.0 组图场景，按实际生成的图片数量计费。」

**事实**：`size` 参数说明中另有两条价格提示——「1.5K 与 1K 价格相同（详情参见 模型价格）」；「Seedream 5.0 pro 的 1.5K 与 1K 价格相同，且图片生成效果更优」。

**事实**：图层拆分场景每次请求**预扣减 17 IPM**（按最多 1 张底图 + 16 张图层预留），全部生成后按实际数量返还多扣部分。**这是配额预扣，不是计费预扣**，两者不要混淆。

### 6.3 区分「单价」与「计费公式」

**单价（第一方明示）**：上表的元/张。

**计费公式（第一方只给了分场景的说明，未给出单一公式）**。据现网说明可整理为：

```
图片生成费用（元）
  = Σ_{每张成功输出的图片} 单价(模型, 该张的实际像素档位, 场景)
  + 输入图费用(模型, 输入图张数)

其中：
  输入图费用(doubao-seedream-5-0-pro) = max(0, input_images - 1) × 0.02   // 首张免费
  输入图费用(lite / 4.5 / 4.0)        = 0
  像素档位 = ≤261万像素 记低档；>261万像素 记高档
  场景     = 单图生成 | 图层拆分（pro 才有两套价）
  组图（lite/4.5/4.0）：按实际生成张数逐张按 0.22/0.25/0.20 计
```

- **这是「我的推断（整理的公式）」**，不是第一方给出的公式。第一方给的是分场景条款，没有形式化公式。
- **事实**：「261 万像素」与「分辨率 1.5K 及以下 / 以上」被第一方**并列**为同一档位判据。二者的分界线在数值上是否处处一致（例如 2K 的 `2496x1664` = 4,153,344 像素，显然 > 261 万），**待确认**。特别是 1.5K 名称对应 `1536x1536 = 2,359,296` 像素，恰好**小于** 261 万；而 1K 的 `1024x1024 = 1,048,576` 也小于 261 万。因此「≤261万 ↔ 1.5K 及以下」在给出的参考值上自洽。

### 6.4 结算到底需要哪些用量事实

**要给出一笔应付金额，平台至少需要：**

| 需要的输入 | 从哪来 | 是否可靠 |
| --- | --- | --- |
| 成功输出的**图片张数** | `usage.generated_images`，或 `data[]` 中无 `error` 的元素个数，或平台实际归档的产物数 | **可三路交叉验证** |
| 每张成功输出的图片的**实际像素值** | `data[].size`（`<宽>x<高>`） | **权威**，且决定档位 |
| 该请求的**场景**（单图生成 / 图层拆分 / 组图） | 只有平台知道自己发了什么（`layer_decomposition`、`sequential_image_generation`） | 平台自持 |
| 使用的**模型**（含版本） | 请求 `model` + 响应回显 `model` | 权威 |
| 输入**参考图张数** | `usage.input_images`（仅 pro 返回），或平台自持 | 可交叉验证 |
| 联网搜索**次数** | `usage.tool_usage.web_search`（仅 lite 且被调用时返回） | 权威计数，但**图片侧是否有单价未在第一方价格页给出** |

**★ 合同落差结论**：

- 方舟图片生成提供的是「**成功张数 + 每张像素**」这组事实，**足以精确算出元金额**；
- 但它**不提供**「输入 token 数 × 输出 token 数」这组事实。`usage.output_tokens` 是 `sum(宽×高)/256` 的**确定性换算**，`total_tokens` 只是它的别名。**平台完全可以用 `data[].size` 自行算出同一个数，因此它不构成任何独立计量信息。**
- 因此**阶段一「强类型 token 用量 × 按 token 单价」的结算模型，无法原样套用到方舟图片生成**。这不是「能凑合用」的程度问题，而是**计量维度不同**：一方是面积折算的伪 token，一方是真实计费单价是「元/张」。

来源：[模型价格](https://docs.volcengine.com/docs/82379/1544106) · [模型服务计费说明](https://docs.volcengine.com/docs/ark/model-service-pricing) · [图片生成 API](https://docs.volcengine.com/docs/82379/1541523) · 抓取时间 2026-09-19

---

## 7. 模型身份与修订

| 问题 | 结论 | 性质与来源 |
| --- | --- | --- |
| 供应方（Vendor） | ByteDance / 字节跳动（Doubao Seedream 系列；上游研究团队对外为 ByteDance Seed） | **事实**：方舟将模型命名为 Doubao Seedream；第三方聚合平台将同一模型登记为 `bytedance-seed/seedream-5-0-pro`（见 §10.1） |
| 原生模型 ID | `doubao-seedream-5-0-pro-260628`、`doubao-seedream-5-0-lite-260128`、`doubao-seedream-5-0-260128`、`doubao-seedream-4-5-251128`、`doubao-seedream-4-0-250828` | **事实**：[图片生成教程](https://docs.volcengine.com/docs/82379/1824121) · [Seedream 5.0 pro 教程](https://docs.volcengine.com/docs/ark/seedream-5-0-pro) |
| 是否有「修订」概念 | **有**。ID 后缀即版本/修订号；`model` 字段格式被明确定义为 `<模型名称>-<版本>`；限流维度是「同模型（**区分模型版本**）」 | **事实**：[图片生成 API](https://docs.volcengine.com/docs/82379/1541523) · [图片生成教程](https://docs.volcengine.com/docs/82379/1824121) |
| 后缀编码含义 | **待确认**。`260628` 形如 `YYMMDD`，但与 `doubao-seed-2-1-pro-260628`（同一后缀）同时出现，无法仅凭日期唯一解释。**不要**把后缀解析成日期作为业务规则 | **我的推断 + 待确认** |
| 平台应固定的 `model` 字符串 | 建议固定为**带版本后缀的完整字符串** | **推论**（理由见 §3.4） |
| 是否可能有第三方 Provider 供应同一 Seedream 模型 | **已确认存在**：OpenRouter 提供 `bytedance-seed/seedream-5-0-pro` 与 `bytedance-seed/seedream-5-0-lite`，均路由到名为 Seed 的 provider | **事实（第三方，已实测 API）**，见 §10.1 |
| 第一方是否提供统一 OpenAI 兼容入口 | 图片生成接口本身就是 OpenAI 风格路径（`/v1`→`/api/v3` 的 `images/generations`），但请求/响应字段是方舟自有的（`sequential_image_generation`、`layer_decomposition`、`usage.generated_images`…） | **事实**：[图片生成 API](https://docs.volcengine.com/docs/82379/1541523) |

---

## 8. 幂等与重试语义

| 问题 | 结论 | 性质与来源 |
| --- | --- | --- |
| 是否有幂等键 / `Idempotency-Key` | **第一方文档中未发现**。图片生成 API 没有幂等键字段、幂等 Header 或客户端 correlation 字段 | **事实（不存在）**：[图片生成 API](https://docs.volcengine.com/docs/82379/1541523) · [图片生成流式响应事件](https://docs.volcengine.com/docs/82379/1824137) · [Base URL 及鉴权](https://docs.volcengine.com/docs/82379/1298459) 全文检索 |
| 是否有 Request ID | **有**。无凭证实测响应头带 `x-request-id`，且错误 `message` 末尾回显 `request id: <id>`。这是**唯一**可用于向方舟对账/提单的标识 | **事实（本机实测）**，2026-09-19 |
| 是否有服务端任务 ID | **图片生成没有**（无任务接口，见 §4） | **事实** |
| 是否有 `user` / `safety_identifier` 透传字段 | 图片生成 API **没有**（视频任务有 `safety_identifier`，有独立的任务查询可关联） | **事实**：[图片生成 API](https://docs.volcengine.com/docs/82379/1541523) · [本地旧快照：查询视频生成任务](./查询视频生成任务.md) |

### 8.1 错误可否证明「未受理、未计费」

下表把方舟错误码分为三类。错误码来源：本地旧快照 `./error-code.md`（「推理错误码」表）——注意现网错误码页 ID `1299023` 在 2026-09-19 返回骨架，**无法逐字核对**，因此本表的「第一方」程度为「第一方页面（可能非最新）」，请按此折价。

**A 类：能证明未受理 / 未生成 → 不计费，可安全让调用方修正后重提**

| HTTP | Code | 含义 |
| --- | --- | --- |
| 400 | `MissingParameter` / `InvalidParameter` / `InvalidParameter.{{Parameter}}` | 缺参 / 非法参数 |
| 400 | `SensitiveContentDetected*`、`InputTextSensitiveContentDetected`、`InputImageSensitiveContentDetected`、`OutputImageSensitiveContentDetected`、`*RiskDetection`、`ContentSecurityDetectionError` | 输入或输出内容安全拦截 |
| 401 | `AuthenticationError` | API Key / AK-SK 缺失或非法（**本机实测确认此形状**） |
| 403 | `AccountOverdueError`、`OperationDenied.ServiceOverdue` | 欠费 / 账单逾期 |
| 403 | `OperationDenied.ServiceNotOpen`、`AccessDenied` | 模型服务未开通 / 无权限 |
| 404 | `InvalidEndpointOrModel.NotFound`、`ModelNotOpen` | 模型不存在或未开通 |
| 429 | `QuotaExceeded`（免费额度耗尽、排队任务超限） | 额度类，未受理 |
| 400 | `RequestBurstTooFast`、`InvalidParameter.UnsupportedParameter` | 参数不支持 / 请求激增保护 |

**B 类：不能证明未受理 → 不可自动重提**

| HTTP | Code | 为什么不能 |
| --- | --- | --- |
| 500 | `InternalServiceError` | 服务内部异常，**可能已经部分生成**；组图场景下 500 会停止后续生成，前面已成功的图可能已产生费用 |
| 429 | `RateLimitExceeded.*`、`ServerOverloaded`、`ModelAccountRpmRateLimitExceeded`、`ModelAccountIpmRateLimitExceeded` | 限流通常在**受理前**拒绝，但文档未承诺「限流返回 ⇒ 未受理」；图片的 IPM 校验发生在生成动作语义上，不能据 HTTP 码推断 |
| — | 连接中断 / 读超时 / 客户端断开 | **完全不可判定**。没有任务 ID 可查（§4），没有幂等键（§8） |
| 499 | `RequestCanceled` | 明示「服务端在返回响应前请求已被客户端取消」，即服务端可能已开始工作 |

**C 类：结论**

- **推论**：**只有 A 类错误码可以作为「未计费」的证据**，且这也只是「未成功生成 → 不计费」的间接推理（第一方只承诺「未成功输出的图片不计费」，没有承诺「A 类错误一定是未受理」）。
- **推论**：**B 类必须进入 `reconciliation_required`，不得自动重提**。这与仓库 `AGENTS.md` 中「Provider 创建请求状态不确定时进入 `reconciliation_required`，不得自动重提」的规则一致。
- **推论**：由于没有任务 ID 与幂等键，方舟的 `reconciliation_required` **无法通过技术手段收敛**，只能靠人工账单核对 + 以 `x-request-id` 向方舟提单。这是方舟相对 AIHubMix `/ai/v1`（有任务列表可查）的**能力倒退**。

来源：[本地旧快照：错误码](./error-code.md) · 本机 2026-09-19 无凭证探测的 401 响应体 · [图片生成 API](https://docs.volcengine.com/docs/82379/1541523)

---

## 9. 结果交付

| 项目 | 结论 | 性质与来源 |
| --- | --- | --- |
| `response_format: url`（默认） | 返回 `data[].url`；**链接在图片生成后 24 小时内有效**，超时自动清除 | **事实**：[图片生成 API](https://docs.volcengine.com/docs/82379/1541523) |
| 下载是否需鉴权 | 第一方只说「请确保图片 URL 可被访问」并给 24h 有效期；**没有**说明下载需带 Bearer。**待确认**（这是短期签名 URL 的常见形态，但未获第一方确认） | **事实（未说明）** + **待确认** |
| 是否支持 `b64_json` | **支持**。`response_format: b64_json` 返回 `data[].b64_json` | **事实**：同上 |
| 官方转存方案 | **有**。第一方明确「推荐配置火山引擎 TOS 提供的数据订阅功能，将您的模型推理产物自动转存到自己的 TOS 桶中」 | **事实**：同上；[TOS 数据订阅](https://www.volcengine.com/docs/6349/1366744) |
| 图层拆分产物格式 | 图层恒为带透明通道 PNG；`output_format` 只控制底图 | **事实**：[图片生成 API](https://docs.volcengine.com/docs/82379/1541523) |
| 交付字段是否够平台自建归档 | 够：`url`/`b64_json` + `size` +（pro）`output_format` + 图层元数据 | **推论** |

**推论**：24 小时有效期 + 无下载鉴权说明，意味着平台**必须**在结果返回后立即把产物复制进自己的对象存储（阶段一 AIHubMix 已经是这个策略）。方舟的 24h 窗口比 AIHubMix 的「约 30 分钟」宽松，但契约性质相同：**短期上游 URL 不能当作平台长期结果**。

**推论**：TOS 数据订阅是官方给的**自建转存替代方案**，但它把产物落到**客户自己的 TOS 桶**，不是方舟托管的长期存储。平台若采用，会引入 TOS 凭证与桶管理，这超出「一个 Provider Adapter」的范围，不建议作为第二阶段范围。

---

## 10. 受控验证可行性

| 项目 | 结论 | 性质与来源 |
| --- | --- | --- |
| 注册 | 注册火山引擎账号即可获得免费推理额度 | **事实**：[免费推理额度](https://docs.volcengine.com/docs/82379/1399514) |
| 是否需实名认证 | **使用免费额度不需要**；但「免费额度耗尽……需要继续使用，需要进行**实名认证**并手动开通对应的模型推理服务」 | **事实**：同上 |
| 免费额度能否覆盖图片生成 | **很可能不能**。免费额度条款明确「仅适用于抵扣**按 token 后付费**产生的**在线推理**费用」，不能抵扣插件/知识库/批量推理；而图片生成是**按张**计价项 | **事实（条款文义）** + **待确认（图片生成实际是否被排除）**：同上 |
| 安心体验模式 | 官方提供「安心体验模式」：仅消耗赠送免费额度，接近额度即停服，避免产生费用。可开启对象限「未开通过模型服务」的账号（含实名认证的个人与企业账号） | **事实**：[免费推理额度](https://docs.volcengine.com/docs/82379/1399514) |
| API Key 获取 | 控制台 →（可选切换项目空间）→ API Key 管理 → 创建。主账号上限 50 个；可按项目隔离并限制 Model ID / 来源 IP | **事实**：[获取 API Key 并配置](https://docs.volcengine.com/docs/82379/1541594) |
| AK/SK 获取 | 需另行创建 Access Key；文档建议创建 IAM 用户并授权，而非用主账号 Access Key | **事实**：[Base URL 及鉴权](https://docs.volcengine.com/docs/82379/1298459) |
| 模型服务开通 | 需在方舟控制台「开通管理」开通对应模型 | **事实**：[图片生成 API](https://docs.volcengine.com/docs/82379/1541523)（`ModelNotOpen` 错误码）· [免费推理额度](https://docs.volcengine.com/docs/82379/1399514) |
| 是否按量后付费 | **是**。「在线推理按 token 后付费」是默认形态；计费说明页写「出具账单后实时结算扣款」 | **事实**：[模型服务计费说明](https://docs.volcengine.com/docs/ark/model-service-pricing) |
| 最小充值金额 | **待确认**。本轮未获取到第一方「最小充值金额」条款 | **待确认** |
| 欠费停服规则 | 2025-07-16 起「欠费 **1 分钟**即关停」（按小时后付费的模型单元和 TPM 保障包除外） | **事实**：[模型服务计费说明](https://docs.volcengine.com/docs/ark/model-service-pricing) |
| 免费额度耗尽后的调用行为 | 「免费额度耗尽，调用将会失败」 | **事实**：[免费推理额度](https://docs.volcengine.com/docs/82379/1399514) |

### 10.1 一笔受控付费验证的成本与停止条件（我的建议）

**事实**：Seedream 5.0 lite 的输出图单价为 **0.22 元/张**，输入图免费。**推论**：这是成本最低的可验证组合。

**建议的最小受控验证（4 次调用，名义成本约 0.88 元人民币 + 可能的高档位差）：**

1. 文生图 `doubao-seedream-5-0-lite-260128`，`size: "2K"`，`sequential_image_generation: disabled`（测最短链路 + `usage` 字段集合 + `data[].url` 24h 行为）
2. 同模型 `size: "2048x2048"`（测像素值与档位、`data[].size` 回显）
3. 单图生图（`image` 传 1 张平台自有小图），测 `usage.input_images` 是否为 lite 缺席
4. 明确越界请求：`size: "1500x1500"`（lite 总像素不足）——**预期被拒**，用来验证「A 类错误码 ⇒ 未计费」这条推理，并确认错误体形状

**停止条件**：任何一次返回 5xx / 连接中断 / 未预期的 `usage` 缺失即停止后续调用，把该次记为 `reconciliation_required` 样本；不得自动重试。

**不要**在受控验证阶段使用 `doubao-seedream-5-0-pro` 的图层拆分（一次请求可能输出 17 张、且图层分档单独计费，成本与不确定性都高），也不要用 `stream: true`（SSE 断开后无法恢复）。

**凭证纪律**：测试 Key 只从环境变量读取；本文档与 fixture 中不出现 Key 明文、Endpoint ID、请求 ID 原值、结果 URL。

---

## 11. 与阶段一 AIHubMix 的能力对照

| 维度 | AIHubMix（`gpt-image-2`，阶段一） | 火山方舟（Seedream 5.0 pro / lite） | 对第二阶段的影响 |
| --- | --- | --- | --- |
| 同一 Vendor Model 由多 Offering 供应 | 阶段一只有 AIHubMix 一个 Offering | **未确认**。AIHubMix 实测 `/call/schema/models/doubao-seedream-5-0-pro/endpoints` 返回 404（模型不存在）；但 **OpenRouter 已提供** `bytedance-seed/seedream-5-0-pro` 与 `...-lite`（实测其 `/api/v1/models` 与 endpoints 接口） | **多 Offering 路由具备现实性**，但第二个 Offering 的候选是 OpenRouter/AIHubMix 之外，不是 AIHubMix。见 §11.1 |
| 计量事实类型 | **token 分项**：`input_tokens`(text/image) + `output_tokens`(image) + `total_tokens`；单价 $/1M tokens | **按张 + 像素档位**：`generated_images`（成功张数）+ `data[].size`（每张像素）+ `input_images`（仅 pro）；`output_tokens` 是 `sum(宽×高)/256` 的换算，`total_tokens` 只是其别名 | **结算模型必须扩展**：token×单价 之外需要「元/张 × 档位」。见 §11.2 |
| 异步任务与可恢复 id | **有**：`/ai/v1/images` 任务列表 + `GET /ai/v1/images/{id}` 详情（`pending`/`in_progress`/`completed`/`failed`/`cancelled`）。OpenAI 兼容 `/v1` 分支**无**任务（实测也确认两次 `/v1` 调用未出现在任务列表） | **完全没有**（§4）。文档导航树证明图片生成只有同步 POST + SSE；视频/3D 才有 tasks 四件套 | 方舟**不能**验证「受理可确认」这一目标；它是该维度的反例 |
| 幂等键 | 无（阶段一已确认无幂等 Header） | 无 | 两者都要靠平台自身 `Idempotency-Key + 请求哈希 + Job 唯一约束` |
| 可对账标识 | 错误体 `tid: req_...`；任务 ID | `x-request-id` 响应头 + 错误 message 回显 `request id: <id>`；**无任务 ID** | 方舟只能人工对账 |
| mask 支持 | **支持**：`mask` 必须与 `image/images` 同时出现；已实测 PNG alpha mask 可用 | **不支持独立 mask 字段**。等价能力是 Seedream 5.0 pro 的**交互编辑**（prompt 内 `<bbox>`/`<point>` 坐标标签 + 手绘标记） | 语义不同，不能映射成同一个 Native 字段 |
| 参考图数量 | `images` 最多 16 | Seedream 5.0 pro 最多 **10**；lite / 4.5 / 4.0 最多 **14**；且组图场景「参考图数 + 生成图数 ≤ 15」 | 方舟约束更紧且是**两个数的联合约束** |
| 输出尺寸能力 | `size`：`auto` / `宽x高`；图生图分支限制为 `auto`/`1024x1024`/`1536x1024`/`1024x1536` | `size`：分辨率档位（pro 1K/1.5K/2K；lite 2K/3K/4K；4.0 1K/2K/4K）**或** `宽x高`，**不可混用**；并有「总像素区间 ∧ 宽高比区间」的合取约束（§3.3） | 方舟的尺寸合同复杂度高一个量级 |
| 多图输出 / 组图 | `n`：1..10 | **无 `n` 字段**。等价物是 lite/4.5/4.0 的 `sequential_image_generation: auto` + `max_images`（默认 15） | 张数控制语义不同 |
| 流式 | 无（阶段一未观察流式） | **有** SSE，但仅 lite/4.5/4.0；**pro 不支持** | 增加一条独立的执行通道 |
| 计价单位 | $/1M tokens（文本输入、图片输入、图片输出三档） | **元/张**（输出按像素档位；pro 输入图第 2 张起 0.02；lite/4.5/4.0 输入免费） | 币种与维度都不同 |
| 失败/审核拦截计费 | `output_blocked` 明确不收生成费；`output_policy_violation` 计费语义不同（阶段一未收敛） | **不计费**（「因审核等原因未成功输出的图片不计费」，表述更明确） | 方舟在这一点上**更清晰** |
| 结果 URL 有效期 | 约 30 分钟（模型说明）；任务对象 `expires_at` 可能为 null | **24 小时** | 方舟窗口更宽 |
| 官方转存 | 无官方方案 | **有** TOS 数据订阅 | 方舟多一个运维选项（但引入 TOS 凭证） |

### 11.1 多 Offering 路由的现实性（已实测）

**事实（第三方，实测 API）**：2026-09-19 查询 `https://openrouter.ai/api/v1/models` 与 `/api/v1/models/bytedance-seed/seedream-5-0-pro/endpoints`：

- `bytedance-seed/seedream-5-0-pro`，名称 `ByteDance Seed: Seedream 5.0 Pro`，`modality: text+image->image`，输出模态 `image`
- 该模型的 endpoint 名为 `Seed`，`provider_name: Seed`，模型版本标识为 `bytedance-seed/seedream-5-0-pro-20260812`
- 定价：`{"image":"0.003","image_token":"0.0000107784431137725","image_output":"0.0000107784431137725"}` → **$0.003/张**，且带 token 形态的字段
- `bytedance-seed/seedream-5-0-lite` 定价 `{"image":"0","image_token":"0.00000838323353293413","image_output":"0.00000838323353293413"}`

**推论**：同一 Vendor Model（Seedream 5.0 pro）**可以由多个 Offering 供应**，且不同 Offering 的 `model` 字符串、计价维度、`image` 单价都不同。这正是第二阶段要验证的「多供给路由」——**在方舟场景下它是真实存在的**，而且恰好是「一方按张、一方按 token 形态」的混合，会**同时**压到结算模型的扩展需求。

**待确认**：OpenRouter 的 `-20260812` 版本标识与方舟的 `-260628` 是否指同一底层修订。**不要**假定它们等价。

**事实（实测）**：AIHubMix（阶段一 Provider）当前**不供应** `doubao-seedream-5-0-pro`——`GET https://api.inferera.com/call/schema/models/doubao-seedream-5-0-pro/endpoints` 返回 404 `model_not_found`。

### 11.2 结算模型的落差的量化表述

阶段一结算输入是一组**强类型 token 计数**，单价是**每百万 token 的金额**，二者相乘即金额。方舟提供的是：

- 一个**按张的金额单价**（元/张，且按像素档位与场景分档）；
- 一组**足以选出正确档位的结构化事实**（`generated_images`、每张 `size`、是否图层拆分、输入图张数）；
- 以及一个**看起来像 token 但实为面积换算**的 `output_tokens`。

因此：
- 若平台坚持「先得到 token 用量再乘 token 单价」，就必须**自己发明**一个方舟侧 token 单价（`0.30 元/张 ÷ (2048×2048/256 tokens)` 之类）。这会把一个**线性、可核验的按张合同**人为转成**近似的 token 合同**，并且一旦上游调整档位阈值（本轮就已经从 236 万改成 261 万）就会静默算错。**不应这样做。**
- 正确方向是把结算**计量量（Metering Quantity）**泛化，而不是把所有 Provider 硬塞进 token。

---

## 12. 事实 / 推论 / 待确认 三分清单

### 12.1 事实（有第一方或实测来源）

1. 图片生成端点为 `POST https://ark.cn-beijing.volces.com/api/v3/images/generations`，鉴权 `Authorization: Bearer $ARK_API_KEY`；另有 Access Key 签名鉴权（`model` 须为 Endpoint ID）。来源：[图片生成 API](https://docs.volcengine.com/docs/82379/1541523) · [Base URL 及鉴权](https://docs.volcengine.com/docs/82379/1298459)，2026-09-19。
2. **图片生成没有异步任务 API**；官方文档导航树中图片生成只有「图片生成 API」与「图片生成流式响应事件」两个页面，而视频与 3D 生成各有创建/查询/列表（视频还有取消）四件套。来源：[官方文档导航树](https://docs.volcengine.com/docs/ark) 还原，2026-09-19。
3. 请求字段集合：`model`(必选)、`prompt`、`image`、`size`、`optimize_prompt_options.mode`(默认 `standard`)、`output_format`(默认 `jpeg`)、`response_format`(默认 `url`)、`sequential_image_generation`(默认 `disabled`)、`sequential_image_generation_options.max_images`(默认 15, [1,15])、`stream`(默认 `false`)、`tools[].type`(`web_search`)、`watermark`(默认 `true`)、`background`(默认 `opaque`)、`layer_decomposition`(默认 `false`)。**无 `n` 字段。** 来源：[图片生成 API](https://docs.volcengine.com/docs/82379/1541523)，2026-09-19。
4. `size` 的合法性是「总像素区间 ∧ 宽高比区间 `[1/16,16]`」的合取；分辨率档位与 `宽x高` 不可混用。pro 图片生成场景总像素 `[921600, 4624220]`，lite/4.5 `[3686400, 16777216]`，4.0 `[921600, 16777216]`；pro 另有图层拆分场景（`size` 默认 `auto`）。来源：同上，2026-09-19。
5. 参考图上限：pro 10 张，lite/4.5/4.0 14 张；组图场景「参考图数 + 生成图数 ≤ 15」。来源：同上，2026-09-19。
6. 可用 model id：`doubao-seedream-5-0-pro-260628`、`doubao-seedream-5-0-lite-260128`、`doubao-seedream-5-0-260128`、`doubao-seedream-4-5-251128`、`doubao-seedream-4-0-250828`；格式为 `<模型名称>-<版本>`。来源：[图片生成教程](https://docs.volcengine.com/docs/82379/1824121) · [Seedream 5.0 pro 教程](https://docs.volcengine.com/docs/ark/seedream-5-0-pro)，2026-09-19。
7. `usage` 字段：`generated_images`（成功张数，**唯一计费依据**）、`input_images`（仅 pro）、`output_tokens`（`sum(宽×高)/256` 取整）、`total_tokens`（「当前不计算输入 token，故与 `output_tokens` 一致」）、`tool_usage.web_search`（仅 lite 且开启时）。来源：[图片生成 API](https://docs.volcengine.com/docs/82379/1541523) · [图片生成流式响应事件](https://docs.volcengine.com/docs/82379/1824137)，2026-09-19。
8. 计价单位是**元/张**：pro 输出单图生成 ≤261万像素 0.30 / >261万像素 0.60，图层拆分场景 0.15 / 0.30；pro 输入图首张免费、第 2 张起 0.02；lite 输出 0.22、输入免费；4.5 输出 0.25；4.0 输出 0.20。**因审核等原因未成功输出的图片不计费**；pro 图层拆分按每个图层实际像素档位单独计费；lite/4.5/4.0 组图按实际生成张数计费。来源：[模型价格](https://docs.volcengine.com/docs/82379/1544106)，2026-09-19。
9. 结果 URL 有效期 **24 小时**；支持 `b64_json`；官方推荐 TOS 数据订阅自动转存。来源：[图片生成 API](https://docs.volcengine.com/docs/82379/1541523)，2026-09-19。
10. 无凭证探测：三条路径均返回 401，错误体 `{"error":{"code","message","param","type"}}`，响应头含 `x-request-id`，message 回显 `request id: <id>`。本机实测，2026-09-19。
11. 无幂等键、无任务 ID、图片生成 API 无 `user`/`safety_identifier` 字段。来源：[图片生成 API](https://docs.volcengine.com/docs/82379/1541523) 全文检索，2026-09-19。
12. API Key 明文格式在 **2026-09-17 12:00（UTC+8）之后**变更（仅格式，不影响鉴权方式）。来源：[获取 API Key 并配置](https://docs.volcengine.com/docs/82379/1541594)，2026-09-19。
13. 免费推理额度仅抵扣「按 token 后付费的在线推理」，不抵扣插件/知识库/批量推理；免费额度耗尽后需**实名认证 + 手动开通模型服务**才能继续。来源：[免费推理额度](https://docs.volcengine.com/docs/82379/1399514)，2026-09-19。
14. 欠费 1 分钟即关停（2025-07-16 起，模型单元与 TPM 保障包除外）。来源：[模型服务计费说明](https://docs.volcengine.com/docs/ark/model-service-pricing)，2026-09-19。
15. 教程能力矩阵给出图片生成 **IPM 限流 500 张/分钟**；图层拆分每次请求预扣减 17 IPM，事后返还。来源：[图片生成教程](https://docs.volcengine.com/docs/82379/1824121) · [Seedream 5.0 pro 教程](https://docs.volcengine.com/docs/ark/seedream-5-0-pro)，2026-09-19。
16. **第三方实测**：OpenRouter 供应 `bytedance-seed/seedream-5-0-pro`（provider `Seed`，版本标识 `...-20260812`，`image` 单价 $0.003/张）与 `bytedance-seed/seedream-5-0-lite`。本机实测 OpenRouter API，2026-09-19。
17. **第三方实测**：AIHubMix（阶段一 Provider）不供应 `doubao-seedream-5-0-pro`（404 `model_not_found`）。本机实测，2026-09-19。

### 12.2 推论（有依据但非第一方直述）

1. 方舟的 `usage` 足以**精确计算按张金额**，但**不提供**任何真实的 token 消耗事实；`output_tokens`/`total_tokens` 是可由 `data[].size` 自行复算的冗余量，**不能**作为 token 结算的计量证据。（依据：事实 7）
2. 平台现有「token 单价 × token 用量」结算模型**必须扩展**，否则要么算错、要么必须发明伪 token 单价来近似一个线性按张合同。（依据：事实 8 + 11.2 分析）
3. 方舟**无法**满足「确认某次提交是否真的被受理」这一阶段二目标，因为缺少任务查询与幂等键。（依据：事实 2 + 事实 11）
4. 方舟的「受理不确定」只能靠平台侧 `Idempotency-Key + Job 唯一约束 + reconciliation_required` 兜住，且**无法技术收敛**，只能以 `x-request-id` 人工提单。（依据：事实 10 + 11 + 本地旧快照错误码表）
5. `size` 不应建模成跨厂商枚举，而应是带「模型级区间 + 合法性规则」的原生字符串。（依据：事实 4）
6. 为保留厂商原生语义，`model` 字段应固定为**带版本后缀**的完整字符串。（依据：事实 6 + 限流按版本区分 + 计价随版本/场景变化）
7. SSE 通道不能替代异步任务的可恢复性；且 Seedream 5.0 pro 不支持流式，因此 pro 完全没有任何渐进观测手段。（依据：事实 2 + 3 + 流式事件文档）
8. 受控验证应优先用 `doubao-seedream-5-0-lite`（0.22 元/张、输入免费），最小成本约 <1 元人民币。（依据：事实 8 + 13）

### 12.3 待确认（必须实测或再取证）

1. `size` 档位名与像素阈值的**精确映射**：261 万像素 ↔ 「1.5K 及以下」在全部参考值上是否处处一致；`auto` 模式的实际输出像素如何计入档位。
2. 非 pro 模型是否**完全不返回** `usage.input_images`；非 lite 模型是否**完全不返回** `usage.tool_usage`。文档只做「模型支持」标注，未承诺字段缺席。
3. 整体失败（顶层 `error`）时 `usage` 是否存在；部分成功后整体失败的计费判定。
4. 图层拆分场景「任一图层失败 ⇒ 整体请求报错，不支持部分成功」时，**已成功生成的图层是否计费**。
5. 裸 model 名（`doubao-seedream-5-0-pro`，不带版本后缀）是否被接受。
6. 是否兼容接受 OpenAI 风格的顶层 `n` 参数。
7. `output_format` 的模型支持范围：API 页写 pro + lite，教程能力矩阵把 4.5 也标为 `png, jpeg`，存在文档内部不一致。
8. 是否提供图片侧「联网搜索」的独立单价条款（价格页未给）。
9. 结果 URL 下载是否**需要**鉴权；是否有下载次数限制；Content-Type 与元数据（水印/C2PA）保留行为。
10. 现网错误码页（`1299023`）正文，用于逐字核对本次使用的本地旧快照错误码表。
11. 图片生成请求体总大小上限（单图 ≤30MB 已知，总请求体上限未在图片 API 页给出；视频侧为 64MB，不可直接套用）。
12. 单次请求超时时间与 `execution_expires_after` 类参数（图片生成未见此类字段）。
13. 最小充值金额与发票/账单查询入口，用于受控付费验证的停止条件设计。
14. 「安心体验模式」是否覆盖图片生成计费项（条款只提推理额度）。
15. OpenRouter 的 `-20260812` 与方舟 `-260628` 是否指同一底层修订；是否存在其他第三方 Offering（如 BytePlus ModelArk 国际站）。
16. 方舟是否有**批量推理**形态的图片生成（价格页未列，但计费说明页有批量推理章节）。

---

## 13. 来源列表

第一方（火山引擎官方文档，均于 2026-09-19 UTC+8 抓取，HTTP 200）：

- 图片生成 API：https://docs.volcengine.com/docs/82379/1541523
- 图片生成流式响应事件：https://docs.volcengine.com/docs/82379/1824137
- 图片生成教程（Seedream 4.0–5.0）：https://docs.volcengine.com/docs/82379/1824121
- Doubao Seedream 5.0 pro 教程：https://docs.volcengine.com/docs/ark/seedream-5-0-pro
- 图片生成模型（体验中心说明）：https://docs.volcengine.com/docs/ark/image-generation-models
- 模型价格：https://docs.volcengine.com/docs/82379/1544106
- 模型服务计费说明：https://docs.volcengine.com/docs/ark/model-service-pricing
- 创建视频生成任务（对照用）：https://docs.volcengine.com/docs/82379/1520757
- Base URL 及鉴权：https://docs.volcengine.com/docs/82379/1298459
- 获取 API Key 并配置：https://docs.volcengine.com/docs/82379/1541594
- 免费推理额度：https://docs.volcengine.com/docs/82379/1399514
- 官方文档导航树（结构性证据）：https://docs.volcengine.com/docs/ark
- 错误码（页面正文未取到，仅确认入口存在）：https://console.volcengine.com/ark/region:cn-beijing/docs/82379/1299023
- TOS 数据订阅：https://www.volcengine.com/docs/6349/1366744

本机只读实测（2026-09-19，无凭证、无费用）：

- `GET/POST https://ark.cn-beijing.volces.com/api/v3/{models, contents/generations/tasks, images/generations}` → 均 HTTP 401，含 `x-request-id`
- `GET https://openrouter.ai/api/v1/models?output_modalities=image` 与 `GET https://openrouter.ai/api/v1/models/bytedance-seed/seedream-5-0-{pro,lite}/endpoints`
- `GET https://api.inferera.com/call/schema/models/doubao-seedream-5-0-pro/endpoints` → 404 `model_not_found`

本地旧快照（仅作线索，已与现网比对，不能作为当前合同）：

- [图片生成模型API调用指南](./图片生成模型API调用指南.md)
- [doubao-price.md](./doubao-price.md)
- [error-code.md](./error-code.md)
- [图片生成示例](./图片生成示例.md)
- [Doubao-Seedream-5.0-pro-教程](./Doubao-Seedream-5.0-pro-教程.md)
- [创建视频生成任务](./创建视频生成任务.md) · [查询视频生成任务](./查询视频生成任务.md)

本文未引用第三方博客或教程作为事实来源。第 11.1 节的 OpenRouter 与 AIHubMix 数据来自其公开 API 实测，属于**第三方事实**，已单独标注。

---

## 14. 对第二阶段的直接判断（简版）

1. **方舟是一个「结算维度不同 + 无恢复能力」的 Provider**。它在两件事上很差：没有异步任务/可恢复 id、没有真实 token 计量。它在三件事上更好：失败不计费的表述更明确、结果 URL 24h 更长、有官方 TOS 转存。
2. **阶段一的结算模型必须扩展**，且这不是可选项。需要引入与 token 并列的第二种计量量（「成功输出的图片张数 × 按像素档位/场景的元单价」+ pro 的输入图阶梯），或者更一般地把结算拆成「计量量（Quantity，带单位）× 单价（Rate，带单位）」。
3. **方舟不能承担「验证受理可确认」这个第二阶段的验证目标**。若第二阶段的目标之一是验证「受理不确定时的安全降级」，方舟只能验证**降级本身**（进入 `reconciliation_required` 且不自动重提），不能验证**恢复**（因为没有可查的任务）。
4. **多 Offering 路由在方舟场景下是真实可验的**（方舟第一方 + OpenRouter 第三方），而且正好跨越两种计价维度，是比「同维度多供给」更有价值的验证样本。
5. **建议**：若第二阶段只打算接一个 Provider 并同时验证「多供给 + 结算扩展 + 安全降级」，方舟是**信息量很高但风险也高**的选择——它的接口简单（一个同步 POST），但合同落差最大，且失败路径不可技术收敛。若希望第二阶段的验证目标集中在「受理可确认 + 多供给路由」，方舟不是合适样本；若目标是「把结算模型从 token-only 泛化」并接受无恢复能力，方舟是合适的**压力样本**。

---

## 15. 本机实测补录（2026-09-19，`dehuadong/seeaihub-server-next#2` Planning）

本节是为第二轮 Planning 补做的实测记录，来源为使用 `DOUBAO_API_KEY`（Machine 级环境变量）对 `https://ark.cn-beijing.volces.com` 的实际调用。**其中 10 次为计费出图**，已在 `.agents/notes/proposed/workflow/2026-09-19-unauthorized-paid-provider-probe.md` 登记。

### 15.1 只读探测（无费用）

| 端点 | 结果 | 结论 |
| --- | --- | --- |
| `GET /api/v3/models` | HTTP 200，133 个模型，含 `status`（`Shutdown` 69 / 空 42 / `Retiring` 22）与 `domain` 字段 | **这是判断模型可用性的唯一只读手段**；`domain=ImageGeneration` 可筛出全部图像模型 |
| `GET /api/v3/billing/usage` | **HTTP 404** | 该路径不存在 |
| `GET /api/v3/billing/balance` | **HTTP 404** | 该路径不存在 |
| `GET /api/v3/usage` | **HTTP 404** | 该路径不存在 |
| `GET /api/v3/foundation_models` | **HTTP 404** | 该路径不存在 |

**结论（对应本文 §12.3 第 13 项）**：方舟**不提供** API 形式的账单/用量查询，费用只能人工在火山控制台核对。这一点已由上面的四次 404 实测确认，不再是待确认项。

### 15.2 模型清单实测（与文档的差异）

`GET /api/v3/models` 中 `domain=ImageGeneration` 的条目与状态：

```text
doubao-seedream-3-0-t2i-250415     Shutdown
doubao-seededit-3-0-i2i-250628     Shutdown
doubao-seedream-4-0-250828         (空)
doubao-seedream-4-5-251128         (空)
doubao-seedream-4-0-20260415       (空)
doubao-seedream-5-0-260128         (空)
doubao-seedream-5-0-pro-260628     (空)
```

**关键差异**：模型清单里**没有** `doubao-seedream-5-0-lite-260128` 这一条目。实测结论：

- `doubao-seedream-5-0-lite-260128` 可调用（返回 `InvalidParameter` 而非 `NotFound`），且其最小像素约束与 `doubao-seedream-5-0-260128` 完全一致（3,686,400）；
- 裸名 `doubao-seedream-5-0-lite` 返回 **404 `InvalidEndpointOrModel.NotFound`**。

因此 `-lite-260128` 是**别名**，不是独立模型条目；它是否与基础模型同一 Vendor Model Revision **仍未证实**。

### 15.3 非法参数探测——**这次探测产生了 10 次计费出图**（教训记录）

原意图是用非法参数区分「模型未开通（404 `ModelNotOpen`）」与「参数非法（400 `InvalidParameter`）」。实测结果**推翻了该探测方法的无副作用的假设**：

| 请求 | 预期 | **实际** |
| --- | --- | --- |
| `size='1x1'` | 400 | 400 `InvalidParameter` + 最小像素消息（无副作用，符合预期） |
| `output_format='webp'` | 400 | 400 `InvalidParameter`（符合预期） |
| `n=2` | 400 | **HTTP 200 并出图** |
| `bogus_field='x'` | 400 | **HTTP 200 并出图** |
| `quality='high'` | 400 | **HTTP 200 并出图** |
| `sequential_image_generation='disabled'` | 400 | **HTTP 200 并出图** |
| `image='https://example.invalid/x.png'` | 400 | 400，但 code 是**生成期失败**（下载输入图失败），与 `InvalidParameter` 不同 |

**三条合同级结论**：

1. **`n` 不受支持且被静默忽略**：`n=2` 返回 200，`data` 只有 1 张。⇒ 平台**不能**用 `n` 反推或约束计费张数。
2. **未知顶层字段被静默接受**：`bogus_field` 返回 200。⇒ **上游不提供「未声明字段失败关闭」这道防线**，平台必须自己校验（本文 §12.1 关于上游 schema 的表述据此修正）。
3. **不受支持的参数不报错**：`quality`、`sequential_image_generation` 均 200，被静默忽略。⇒ 参数能力必须由平台按已发布的 Native Capability Schema 判定，不能依赖上游报错发现。

**方法学教训**：在存在「静默放行」语义的上游上，任何「发一个畸形请求看看它报什么错」的探测都会产生真实副作用。判断模型开通状态应当只依赖 `GET /api/v3/models`。

### 15.4 成功响应与原 `usage` 的实测形状

`doubao-seedream-5-0-260128`，`size='2048x2048'`：

```json
{
  "model": "doubao-seedream-5-0-260128",
  "created": 1789790948,
  "data": [{ "url": "<TOS 预签名 URL，已脱敏>", "size": "2048x2048" }],
  "usage": { "generated_images": 1, "output_tokens": 16384, "total_tokens": 16384 }
}
```

`doubao-seedream-5-0-pro-260628`，同样的 `size`：

```json
{
  "model": "doubao-seedream-5-0-pro-260628",
  "created": 1789791060,
  "data": [{ "url": "<已脱敏>", "size": "2048x2048", "output_format": "jpeg" }],
  "usage": { "input_images": 0, "generated_images": 1, "output_tokens": 16384, "total_tokens": 16384 }
}
```

**逐项结论**：

- `generated_images` 是**唯一计费依据**（实测两次均为 1）；
- `output_tokens = 2048×2048/256 = 16384`，与本文 §4 引用的第一方文档公式一致 ⇒ **平台可从 `data[].size` 复算同一数值**（推论），故它不构成独立计量事实；`total_tokens` 是它的别名；
- `input_images` 在**基础模型**的响应中**缺席**，只在 pro 出现（值 0）。本文 §12.3 第 2 项「非 pro 是否完全不返回该字段」在本轮仍未获第一方承诺，但**对首期发布的基础模型，该字段确定缺席**；
- `data[].size` 在两次响应中都存在且为 `宽x高` 字符串（**实测**，不再是文档推断）；
- `data[].output_format` 在基础模型响应中**缺席**、在 pro 中出现；
- 响应体**不含任何请求 id**；`x-request-id` 只在响应头（实测取到形如 `021789791014897846a816464a9af9c0791e0f68a2ce03c25cf6d` 的值）。

**仍未实测**（保持待确认，不得写成实测）：

- 结果 URL **下载是否需额外鉴权**（本文只探测了 URL 形状与 `X-Tos-Expires=86400` 参数，**未实际下载**）；
- 图生图（`image` 为单个字符串）分支的响应形状与 `usage`；
- 内容审核拒绝的真实 `error.code`；
- 基础模型小尺寸（如 1024×1024）的单价档位。

