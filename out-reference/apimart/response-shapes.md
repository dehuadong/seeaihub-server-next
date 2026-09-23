# APIMart 各端点的**实测响应结构**（收集台账）

> **性质**：外部参考资源（上游返回的实测样本），**不是平台接口合同**，服务不读取。
> **用途**：给 ② Adapter 的解码器与测试 fixture 提供依据；回答"这个端点实际返回什么形状"。
> **纪律**：不保存真实 Key、结果 URL、task id（只记存在性与长度级信息）。

## 0. 一句话现状（2026-09-19）

**来源分级**：**本仓库受控实测** = 经用户授权、由本仓库 Agent 发起并留档（`docs/verification/paid-provider-calls.md` 有留档）；本目录里没有"用户早期采集"的 APIMart 样本。

| 端点 | 实测过？ | 有独立样本文件？ | 哪几次 | 结构记在哪 |
| --- | --- | --- | --- | --- |
| `POST /v1/images/generations`（提交） | ✅ 2026-09-19 | ✅ `controlled-probe-2026-09-19.json`（文生图那次，逐字）+ ✅ `transcript-image-edit-2026-09-19.json`（图生图那次） | 受控探针 1 次 + 参考图/遮罩轮 2 次 | §2 |
| `GET /v1/tasks/{id}`（终态） | ✅ 2026-09-19（3 次） | ✅ 同上两个文件（文生图逐字；图生图与平台那次为转录） | 同上 | §3 |
| `POST /v1/uploads/images`（上传） | ✅ 2026-09-19（2 次） | ✅（转录）`transcript-image-edit-2026-09-19.json` | 参考图/遮罩那一轮 | §1 |
| 错误信封（401 凭据类） | ✅ 2026-09-19 零费用探测 | ✅（转录，报文逐字）同上文件 | 无凭证路由探测 | §4 |

> `controlled-probe-2026-09-19.json` 随提交 `86b84af`（"docs: 第二阶段 Planning 收口"）入库，是本仓库的受控探针留档。
> `transcript-image-edit-2026-09-19.json` 是**转录**：字段名与取值照当时的工具输出记录整理，逐字报文未落盘（临时目录已删）。

**缺口（如实记录）**：参考图/遮罩那一轮（2 次上传 + 2 次生成）与平台端到端那次的**逐字报文没有保存**；字段与取值已转录进 `transcript-image-edit-2026-09-19.json`。**结论不受影响**（它们依赖的是字段与数值，且已被后续真实调用交叉印证），受影响的是"逐字可复核"这一层。规则已写进文末，避免再发生。

## 1. `POST /v1/uploads/images`（multipart，字段名 `file`）

**按运行记录整理**（2026-09-19，2 次：512×512 参考图、512×512 带 alpha 遮罩）：

```json
{ "url": "https://<主机>/f/image/…-photo.png",
  "filename": "reference.png",
  "content_type": "image/png",
  "bytes": 4499,
  "created_at": 1789813766 }
```

| 项 | 实测 |
| --- | --- |
| 顶层字段 | **正好 5 个**：`url`、`filename`、`content_type`、`bytes`、`created_at`（与上传页文档一致） |
| `url` 形态 | `https`，主机是 **`getapib.org`**（**不是**文档示例里的 `upload.apimart.ai`），路径较长且随机；文档称有效期 72 小时 |
| `content_type` | 由上游探测，我们传 PNG 得到 `image/png` |
| `bytes` | 与上传字节数一致（4499 / 2442） |
| 失败形状 | 未实测（上传页文档给 400/413/500，只有 429 带 `error.code`） |

## 2. `POST /v1/images/generations`（提交）

**逐字样本**（`controlled-probe-2026-09-19.json`，HTTP 200）：

```json
{ "code": 200, "data": [ { "status": "submitted", "task_id": "task_<已脱敏，长度 31>" } ] }
```

**按运行记录整理**（参考图 + 遮罩那次同形）：`data` 是**数组**，读 `data[0].task_id`；`status` 受理时为 `submitted`。

## 3. `GET /v1/tasks/{id}`（终态）

**逐字样本**（文生图那次，`controlled-probe-2026-09-19.json`）：

```json
{ "code": 200, "data": {
    "id": "task_<已脱敏，长度 31>", "status": "completed", "progress": 100,
    "actual_time": 7, "estimated_time": 60,
    "created": 1789806894, "completed": 1789806901,
    "cost": 0.00476, "credits_cost": 0.0476,
    "result": { "images": [ { "url": ["<已脱敏>"], "expires_at": 1789893301 } ] },
    "usage": { "input_tokens": 14,
               "input_tokens_details":  { "cached_tokens": 0, "image_tokens": 0,   "text_tokens": 14 },
               "output_tokens": 196,
               "output_tokens_details": { "image_tokens": 196, "text_tokens": 0 },
               "total_tokens": 210 } } }
```

**三次实测的值对照**（结构完全相同）：

| 调用 | `input_tokens`（text / image） | `output_tokens`（image） | `total` | `cost`（USD） | `credits_cost` |
| --- | --- | --- | --- | --- | --- |
| 文生图（curl，逐字留存） | 14（14 / 0） | 196（196） | 210 | 0.00476 | 0.0476 |
| 图生图（curl 直连，参考图 + 遮罩） | 1057（33 / **1024**） | 196（196） | 1253 | 0.01139 | 0.1139 |
| 图生图（**走我们自己的 API+Worker**） | 1053（29 / **1024**） | 196（196） | 1249 | 0.011374 | 0.11374 |

**结构要点**：

| 字段 | 说明 |
| --- | --- |
| `data.result.images[]` | 每项为 `{ "url": [<字符串>], "expires_at": <unix秒> }`——**`url` 是数组**（与生成页示例一致）；`expires_at` 说明结果 URL 会过期，必须立刻下载转存 |
| `data.usage` | **四分项**：`input_tokens_details{cached_tokens,image_tokens,text_tokens}`、`output_tokens_details{image_tokens,text_tokens}` + `total_tokens`；**参考图会真实计入 `image_tokens`**（1024×1024 ⇒ 1024） |
| `data.cost` / `data.credits_cost` | 上游**声明的**实际扣费（折后账号价）与 Credits（`= cost × 10`）。平台**不把它当计量事实**，只作**成本价**与对账核对（[`docs/facts/channel-facts.md`](../../docs/facts/channel-facts.md) 的 APIMart 计量节） |
| 其它 | `actual_time`（实际耗时秒）、`estimated_time`、`progress`、`created`/`completed` |

## 4. 错误信封

**逐字实测**（2026-09-19，**不带凭证**的零费用探测）：

```json
{"error":{"code":"","message":"invalid API key (request id: 20260919182056471923385yBRUUrTx)","param":"","type":"apimart_error"}}
```

| 项 | 实测 |
| --- | --- |
| 顶层 | `error.code`（**空字符串**）、`error.message`、`error.param`、`error.type`（`apimart_error`） |
| 头 | `X-Oneapi-Request-Id`（逐请求标识，同时写在 `message` 的 `(request id: …)` 里） |
| 影响 | 凭据类失败**没有可用的 `error.code`** ⇒ ② 的错误分类对 401/402/403 用状态码兜底（[`docs/facts/channel-facts.md`](../../docs/facts/channel-facts.md) 的 APIMart 错误节） |
| 未实测 | 400/429/5xx 的**真实**报文（400 与 413 的形状来自上传页文档；`build_request_failed` 前缀来自生成页文档） |

## 5. 相关记录

- 渠道事实（归纳后）：[`docs/facts/channel-facts.md`](../../docs/facts/channel-facts.md) 的 APIMart 各节（响应形状、上传与错误、计量与成本）；
- 原始逐字样本：`controlled-probe-2026-09-19.json`；
- 请求侧合同：`schema-gpt-image-2.5-flare.input.json`、`generation.md`、`gpt-image-2.5-generation.cn.md`、`tasks-status.cn.md`、`uploads-images.cn.md`；
- 另一个渠道的对应台账：`out-reference/aihubmix/response-shapes.md`。

## 6. 流程要求（本文件缺口引起的规则）

**每次经批准的计费实测，收尾时必须把脱敏后的逐字响应落成文件**放进本目录（命名建议 `<端点或主题>-<日期>.json`，与 `controlled-probe-2026-09-19.json` 一致），并在对应渠道的 `response-shapes.md` 里登记一行"哪次调用 → 哪个文件"。只写进文档叙述**不算收集**。

**怎么补**：跑 `scripts/probe/response-shapes.ps1`（本仓库**唯一**会发真实计费调用的入口，默认只演练，必须显式加 `-ConfirmPaidCalls`）：

```powershell
# 先演练：看要发哪些请求、大概花多少
pwsh -File scripts/probe/response-shapes.ps1 -Provider apimart -Probe image_edit
# 确认后再真跑（会写 out-reference/apimart/probe-<日期>-image_edit.json）
pwsh -File scripts/probe/response-shapes.ps1 -Provider apimart -Probe image_edit -ConfirmPaidCalls
```

脚本会自动脱敏 URL 与 task id，并在结束时提醒补两处登记（`docs/verification/paid-provider-calls.md` 与本文档）。
