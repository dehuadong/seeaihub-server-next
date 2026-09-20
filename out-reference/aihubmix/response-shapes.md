# AIHubMix 各端点的**实测响应结构**（收集台账）

> **性质**：外部参考资源（上游返回的实测样本），**不是平台接口合同**，服务不读取。
> **用途**：给 ② Adapter 的解码器与测试 fixture 提供依据；回答"这个端点实际返回什么形状"。
> **纪律**：不保存真实 Key、结果 URL、task id；`b64_json` 只记长度不落全文。

## 0. 一句话现状（2026-09-19）

**来源分级**（这份台账里每行都标）：

- **用户早期采集**：仓库建立（提交 `1fe462a`）时随库进来的样本，采集时间见响应里的 `created`——**不是**本仓库的受控实测；
- **本仓库受控实测**：经用户授权、由本仓库 Agent 发起并留档的调用（`docs/facts/channel-facts.md` §5 有留档）。

| 端点 | 实测过？ | 有独立样本文件？ | 谁采集的 | 结构记在哪 |
| --- | --- | --- | --- | --- |
| `POST /v1/images/generations`（同步） | ✅ | ✅ **`gpt_image_2_generations.json`**（逐字，含 2 MB `b64_json`） | **用户早期采集**：响应 `created = 1785485861` ⇒ **2026-07-31 16:17:41 +08:00**；随仓库建立提交 `1fe462a` 入库 | §1 |
| `POST /v1/images/generations`（同步，2.5 两款） | ✅ 2026-09-19 | ✅（转录）**`transcript-sync-and-async-2026-09.json`** | 本仓库受控实测（`channel-facts` §5.2） | §1 |
| `POST /v1/images/generations`（同步，2.5 flare，逐字） | ✅ 2026-09-20 | ✅ **`probe-2026-09-20-generations.json`** | 本仓库受控实测（`channel-facts` §5.7） | §1、§7 |
| `POST /ai/v1/images/generations`（**默认同步**，2.5 flare） | ✅ 2026-09-20 | ✅ **`probe-2026-09-20-native_sync.json`** | 本仓库受控实测（`channel-facts` §5.7） | §3、§7 |
| `POST /v1/images/edits`（同步，multipart，图片+mask） | ✅ 2026-09-18 | ✅（转录）`transcript-sync-and-async-2026-09.json` | 本仓库受控实测（同文件 §13.2 的转述，见 `channel-facts` §2.3 的说明） | §2 |
| `POST /ai/v1/images/generations`（异步任务对象） | ✅ 2026-09-18 + 2026-09-19 | ✅（转录）同上文件 | 本仓库受控实测（同上 / `channel-facts` §2.3） | §3 |
| 错误信封 | ✅ 部分 | ✅（转录）同上文件 | 本仓库受控实测 + 上游文档 | §4 |
| **响应头** | ✅ 2026-09-20 | ✅ 两个 `probe-2026-09-20-*.json` | 本仓库受控实测 | §7 |

**证据等级**：`gpt_image_2_generations.json` 是**逐字**报文；`transcript-sync-and-async-2026-09.json` 是**转录**（字段名与取值照当时的实测记录整理，`b64_json` 只记长度）——这批调用的逐字报文当时没有落盘。`probe-2026-09-20-*.json` 是**逐字**（`b64_json`、URL 与 task id 已脱敏）。

**口径提醒**：`gpt_image_2_generations.json` 的响应体**没有 `model` 字段**（顶层只有 `created/background/data/output_format/quality/size/usage`），因此"它来自 `gpt-image-2`"是**按文件名与 `gpt-image-2.md` 推断**的，报文本身证明不了。同理，同步 `/v1` 的响应都**不回显模型名**。

**缺口**：**逐字报文**这一层现在有用户早期那份（§1）与 2026-09-20 的两份 `probe-2026-09-20-*.json`；2026-09-18/19 那几批仍只有**转录**。**响应头**这一层 2026-09-20 之前完全空白，现已补上（§7）。

## 1. `POST /v1/images/generations`（同步，OpenAI 兼容）

**逐字样本**：`gpt_image_2_generations.json`（**用户早期采集，响应时间 2026-07-31**；随仓库建立提交 `1fe462a` 入库）= `{http_status:200, elapsed_seconds:21.05, body:{…}}`，其中 `body`：

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

**2.5 两次（2026-09-19，脱敏转录，未落独立文件）** —— 顶层字段集合与上面**完全一致**：

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

两次调用的差异只有 `b64_json` 长度与耗时：`sunburst` 245,138 bytes / 19.8 秒；`flare` 321,146 bytes / 14.7 秒（`flare` 的 `b64_json` 为 320,708 字符）。

**结构要点**：

| 字段 | 说明 |
| --- | --- |
| **响应体里没有 `id`** | 顶层只有 `created/background/data/output_format/quality/size/usage`——**既没有任务 id，也没有请求 id**；`data[]` 项里也只有 `b64_json`。两条同步路径（generations / edits）都是这样。**逐请求标识在响应头里**（`X-Request-ID`，见 §7） |
| `created` / `background` / `output_format` / `quality` / `size` | 顶层回显类字段，**不在** `data[]` 里 |
| `data[]` | 数组；每项**只有** `b64_json`（本渠道这两条路径**不返回 URL**） |
| `usage` | **四分项**（`input_tokens_details` / `output_tokens_details` 各含 `text_tokens`/`image_tokens`）+ `total_tokens`；**没有** `cached_tokens`、**没有**金额字段 |
| 金额 | **响应里没有** ⇒ 成本价只能按四档 token 费率自算（见 `docs/facts/channel-facts.md` §2.4） |

**⇒ 后果（同步路径）**：同步调用**没有可查的上游任务**——响应体里没有 id，且实测两次同步调用**未出现在** `/ai/v1/images` 任务列表里（原调研记录的 §13.2；该文档已于 2026-09-20 清理删除，结论转述在 `docs/facts/channel-facts.md` §2.3）。所以创建请求一旦失联，**没有技术手段能把结果取回**，只能进对账、人工按账号与时间窗核对（`docs/adr/0005`/`0007`）——**但有一枚可归因的请求标识**（响应头 `X-Request-ID`，§7），对账不再是"连查哪个请求都不知道"。`/ai/v1` 返回的才是任务对象（有 `id`），但那条路径不返回 `usage`（§3、§7），不作为计费执行路径。

## 2. `POST /v1/images/edits`（同步，`multipart/form-data`）

**实测**（第一阶段，2026-09-18，脱敏转录）：HTTP 200，`image` + `mask` 各一张 ⇒

- 顶层字段与 §1 相同（`created/background/data/output_format/quality/size/usage`），输出同为 `data[0].b64_json`；
- 耗时 23.3 秒；
- `usage`：`input_tokens 1051`（`text_tokens 27`、**`image_tokens 1024`**）、`output_tokens 196`（`image_tokens 196`）、`total_tokens 1247`——**图片输入会真实计入 `input_tokens_details.image_tokens`**（1024×1024 的输入图 ⇒ 1024）；
- 请求侧为 multipart：字段名 `image`、`mask`（见 `gpt-image-2.md` 的示例）。

> 注：**2.5 的 `/v1/images/edits` 未测**（只测了 `gpt-image-2`）。结构按同渠道族推断为同形，但**未经实测**。

## 3. `POST /ai/v1/images/generations`（异步任务对象）

**创建**（HTTP 200，立即返回）：

```json
{"completed_at":null,"created_at":1789804016,"error":null,"expires_at":null,
 "id":"t_<已脱敏>","model":"gpt-image-2","object":"image",
 "output":[],"status":"pending"}
```

**轮询 `GET /ai/v1/images/{id}` 至终态**（HTTP 200，约 12 秒）：

```json
{"completed_at":1789804028,"created_at":1789804016,"error":null,"expires_at":1789811227,
 "id":"t_<已脱敏>","model":"gpt-image-2","object":"image",
 "output":[{"b64_json":null,
            "content_url":"https://aihubmix.com/ai/v1/images/<id>/content/res_<已脱敏>",
            "index":0,"type":"file"}],
 "status":"completed"}
```

**结构要点**：任务对象只有 9 个字段 `id/object/model/status/output/error/created_at/completed_at/expires_at`；**没有 `usage`**、没有 prompt、没有 metadata/correlation ID。`output[]` 项为 `{index, type, content_url, b64_json}`，本渠道这两次 `b64_json` 为 `null`、只给 `content_url`（下载仍需创建时的同一 Bearer 凭据）。任务列表项为同样 9 个字段。

**状态取值**：`pending` → `in_progress` → `completed`（文档另列 `failed`/`cancelled`）。

**默认（不带 `async`）也是同一个任务对象（2026-09-20 实测）**：`{"model":"gpt-image-2.5-flare","prompt":"…","n":1,"size":"1024x1024","extra":{"quality":"low"}}` ⇒ HTTP 200、13.86 秒后返回 `status: completed` 的同形任务对象（`id`、`output[0].b64_json` 与 `content_url` 都在，**仍无 `usage`**）。也就是说这条端点的"同步"只是**等它跑完再返回**，返回形状与 `async: true` 一致 ⇒ **它无论同步异步都不带计量**。样本：`probe-2026-09-20-native_sync.json`；响应头见 §7。

## 4. 错误信封

文档给出的形状（**异步文档**，`gpt-image-2`）：

```json
{"error":{"message":"…","type":"invalid_request_error","code":"…","tid":"req_…"}}
```

- **实测的一条**（`/ai/v1` 顶层传 `quality`）：HTTP 400，`{"error":{"code":"schema_violation","message":"Unknown request parameter: `quality`.","type":"invalid_request_error"}}`——**这一条里没有 `tid` 字段**（文档说错误体带 `tid`，实测这个 400 没带）；
- **`tid` / 响应头的现状（2026-09-20 实测结清，见 §7）**：文档说错误体带 `tid`；**响应头确实有逐请求标识**——`/v1/*` 带 `X-Request-ID` 与 `apim-request-id`，两条路径都带 `x-aihubmix-request-id`。② Adapter 读的 `x-request-id` 因此能采到值（成功路径也会落库）。此前"从未实测过响应头"的缺口已关闭；
- **未知参数是硬拒绝**（`schema_violation`），不会静默降级。

## 5. 相关记录

- 渠道事实（归纳后）：`docs/facts/channel-facts.md` §2.2/§2.3/§2.6/§2.6b/§2.6c；
- 调研过程与受控实测：2026-09-20 之前那份调研文档（`docs/research/gpt-image-2-inferera-research.md` §13）**已于 2026-09-20 清理删除**，其结论转述在 `docs/facts/channel-facts.md` §2.3；本目录保留当时的转录与快照；
- 请求侧合同（机器 Schema 快照）：`schema-gpt-image-2*.endpoints.json`；
- 另一个渠道的对应台账：`out-reference/apimart/response-shapes.md`。

## 7. 响应头（2026-09-20 首次实测）

| 头 | 出现在 | 样例（脱敏后原样） |
| --- | --- | --- |
| `X-Request-ID` | 仅 `/v1/images/generations` | `911efcbf-2868-490c-b87e-729d1751ca38` |
| `x-aihubmix-request-id` | 两条路径都有 | `2026092006194196914429082113579` |
| `apim-request-id` | 仅 `/v1/images/generations` | `cf78feef-2e42-4596-9376-37996551bd3b` |
| `x-ratelimit-limit-requests` / `-remaining-requests` | 仅 `/v1/images/generations` | `12` / `8`（本次调用前） |
| `x-aihubmix-model` / `x-ms-deployment-name` / `x-ms-region` | 仅 `/v1/images/generations` | `gpt-image-2.5-flare` / `gpt-image-25-flare` / `Sweden Central` |

**要点**：① 同步路径的对账标识**拿得到**（`X-Request-ID`），② Adapter 的取法（读 `x-request-id`）是对的；② **账号级限流天花板**：响应头自报每分钟 12 次；③ `/ai/v1` 那条路径的响应头里**只有** `x-aihubmix-request-id`。

样本：`probe-2026-09-20-generations.json`（`/v1`）、`probe-2026-09-20-native_sync.json`（`/ai/v1` 默认同步）。

## 8. 怎么补齐剩下的缺口

跑 `scripts/probe/response-shapes.ps1`（本仓库**唯一**会发真实计费调用的入口，默认只演练，必须显式加 `-ConfirmPaidCalls`）：

```powershell
# 演练：看要发哪些请求、大概花多少
pwsh -File scripts/probe/response-shapes.ps1 -Provider aihubmix
# 真跑：同步文生图 + 图片编辑（各 1 次付费调用）
pwsh -File scripts/probe/response-shapes.ps1 -Provider aihubmix -Probe generations,edits -ConfirmPaidCalls
# `/ai/v1` 默认同步 / 异步任务对象（各 1 次付费调用）
pwsh -File scripts/probe/response-shapes.ps1 -Provider aihubmix -Probe native_sync,async -ConfirmPaidCalls
```

脚本会写好 `out-reference/aihubmix/probe-<日期>-<probe>.json`（自动脱敏 URL / task id / `b64_json`），并在结束时提醒补两处登记：`docs/facts/channel-facts.md` §5 与本文档 §0 的表格。
