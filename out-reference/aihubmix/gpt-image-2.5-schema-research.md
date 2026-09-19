# AIHubMix gpt-image-2 与 gpt-image-2.5 两款的机器 Schema 实测（2026-09-19）

> 上游协议研究与只读取证记录，**不是平台对外接口合同**。所有证据来自无需鉴权的机器可读端点，**未发起任何计费调用**，未使用任何 API Key。

## 1. 取证方式

| 端点 | 鉴权 | 用途 |
| --- | --- | --- |
| `GET https://aihubmix.com/call/schema/models/{model}/endpoints` | 无需鉴权 | 该模型的端点、运行时语义与**参数 Schema**（权威、机器可读） |
| `GET https://aihubmix.com/model/{model}/llms.txt` | 无需鉴权 | 模型摘要（开发者、模态、发布日、端点、参数表） |
| `GET https://aihubmix.com/models/retirements` | 无需鉴权 | 渠道的**退役清单**（Deprecating / Retired / Replacement） |

抓取时间：2026-09-19。三个模型的三类端点均返回 HTTP 200。

## 2. 事实：2.5 两款确实存在，且 `quality` 档位确有扩展

`gpt-image-2.5-flare` 与 `gpt-image-2.5-sunburst` 在 AIHubMix 上**均可解析、均有完整 Schema**：

- 开发者：OpenAI；输入模态：text, image；发布日：**2026-09-08**（两者相同）；
- 端点集合与 `gpt-image-2` **完全一致**：`/ai/v1/images/generations`、`/v1/images/edits`、`/v1/images/generations`；
- 顶层参数集合也一致：`async`、`extra`、`image`、`images`、`mask`、`model`、`n`、`output_format`、`prompt`、`size`、`webhook_events_filter`、`webhook_url`。

**差异集中在 `extra`（`additionalProperties: false`）**：

| `extra` 字段 | `gpt-image-2` | `gpt-image-2.5-flare` / `-sunburst` |
| --- | --- | --- |
| `quality` | `low` / `medium` / `high` | **`low` / `medium` / `high` / `xhigh` / `max` / `auto`（默认 `auto`）** |
| `moderation` | 不存在 | **`auto` / `low`（默认 `auto`）** |
| `background` | `transparent` / `opaque` / `auto`（默认 `auto`） | `auto` / `opaque` / `transparent`（默认 `auto`） |
| `output_compression` | integer，默认 `100` | integer，默认 `100` |
| `user` | string | string |

`n` 三者的声明相同：`integer|null`，`minimum 1`、`maximum 10`、默认 `1`，并带 `x-capability: supports_n_gt_1` 与 `x-evidence: strong`。

**结论**：用户所述「两者区别就是增加了 `quality` 档位」与实测一致——新增 `xhigh` / `max` / `auto`，另多一个 `moderation` 字段；端点与其余参数不变。

## 3. `lifecycle` 字段的含义（易误读）

Schema 中每个端点带 `lifecycle`，但它描述的是**运行时语义**，不是退役状态：

```json
{"cancel_path":"","mode":"sync","poll_method":"GET","poll_path":"/ai/v1/images/{id}",
 "status_values":["pending","in_progress","completed","failed","cancelled"],"supports_async":true}
```

- `/ai/v1/images/generations`：`supports_async: true`，有 `poll_path`；
- `/v1/images/generations` 与 `/v1/images/edits`：`mode: sync`，`poll_path` 为空。

这与第一阶段的技术设计结论一致（正式计费路径走 `/v1`，`/ai/v1` 为已验证但无 `usage` 的异步能力）。

## 4. **未能证实**：渠道的退役清单里没有 `gpt-image-2`

用户告知「GPT-Image-2 即将下线」。我按只读取证核对，**在当前证据中找不到该声明**：

- `GET https://aihubmix.com/models/retirements` 返回 HTTP 200，页面自述「13 scheduled for retirement / 34 already retired」，列出的图像类退役条目是 Google 的 Gemini / Imagen 系列；
- 在该页正文中检索 **`gpt-image` 出现 0 次、`image-2` 出现 0 次**；
- `GET /call/schema/models/gpt-image-2/endpoints` 全文检索 `retire`、`deprecat`、`sunset`、`retirement_date`、`replacement`、`lifecycle_status` **均为 0 次命中**；
- `gpt-image-2` 的 `llms.txt` 与模型页均**没有**下线提示。

**因此本条记录为「用户告知、平台侧未证实」**，不得作为合同前提直接使用。可能的原因（未验证，不作结论）：该退役由**上游**（OpenAI 或另一渠道）宣布而 AIHubMix 尚未登记；或退役清单只覆盖 LLM 类别而图像模型另有公告位置。

**需要的确认**：退役的**权威出处**（哪个渠道/厂商的哪个页面）、**生效日期**、以及**是否有明确替代型号**。在拿到出处之前，`gpt-image-2` 是否下线不能被当作第二阶段的前提。

## 5. 对第二阶段的影响（记录，不在本文做决定）

1. **模型选择属于用户决定**：仓库根 `AGENTS.md` 已明确「未经批准不自主选择渠道模型」，因此本文只记录事实，不推荐取舍。
2. **若 `gpt-image-2` 确实即将下线**，第一阶段的正式计费 Offering（AIHubMix → `gpt-image-2`）将面临**供给面替换**，那属于另一次契约变更，需要单独的规划与批准；它同时会影响「第二个 Offering 供应同一 Vendor Model」这一命题是否仍然成立（被替换后双方可能都转向 2.5）。
3. **`quality` 档位差异是可判定的能力收窄素材**：`gpt-image-2` 只有三档、2.5 有六档，这为「同一 Vendor Model 的不同 Offering 各自声明收窄后的 Schema」提供了真实而非虚构的样本。
4. **`x-capability` / `x-evidence` 字段的存在**：AIHubMix 的 Schema 自带能力与证据等级标注（例如 `supports_n_gt_1` 标为 `strong`），可作为平台发布期校验的交叉依据。

## 6. APIMart 侧对同一对模型的供应与别名（只读核对）

抓取时间 2026-09-19，全部为无需鉴权的只读请求。

### 6.1 事实：APIMart 也供应 `gpt-image-2.5-flare` 与 `gpt-image-2.5-sunburst`

文档 `GET https://docs.apimart.ai/cn/api-reference/images/gpt-image-2.5/generation.md`（HTTP 200）在 `model` 字段的取值里列出 **`gpt-image-2.5-flare`** 与 **`gpt-image-2.5-sunburst`** 两个值，并说明两者「**单价和相同参数下的 token 消耗一致**」。

⇒ **两个渠道都供应同一对 2.5 型号**，这是本阶段此前一直缺失的「同一 Vendor Model 由两个不同 Provider 供应」的真实样本（AIHubMix 与 APIMart 的 `native_model_id` 同为 `gpt-image-2.5-flare` / `-sunburst`）。

### 6.2 事实：APIMart 侧的 `quality` 取值与可判定边界

- `quality` 默认 **`auto`**，支持 **`low` / `medium` / `high` / `xhigh` / `max` / `auto`**；文档明确「**`xhigh` 和 `max` 仅 GPT-Image-2.5 支持。将其传给 `gpt-image-2` 会同步返回 400，不会自动降级**」——这是一条可直接用于发布期校验的**可判定边界**。
- 文档另述 2.5 相对上一代「新增 `xhigh` 和 `max` 两个质量档位」，且「`medium` 与 `high` 的输出 token 消耗约为上一代同名档位的**四分之一**」。

### 6.3 事实：`gpt-image-2-official` 是独立文档页，与 `gpt-image-2` 的等价关系未被第一方声明

- `gpt-image-2` 生成页有一处「模型名兼容提示」：兼容别名 `gpt-image-2-ext`，与 `gpt-image-2` 可互换使用。**这是唯一被第一方明确写成「等价」的别名。**
- `gpt-image-2-official` 有**独立文档页** `.../images/gpt-image-2/official.md`，模型字段固定为 `gpt-image-2-official`，参数集合与计价维度均与 `gpt-image-2` 不同（前者带分项 token `usage` 与 `mask_url`，后者不带）。

⇒ **可核验的事实是**：`gpt-image-2-ext ≡ gpt-image-2`（渠道别名）；而 **`gpt-image-2-official` 与 `gpt-image-2` 的等价关系在本轮全部可读来源中均未被声明**。

### 6.4 与运营决策的关系

运营方（用户）已判定 `gpt-image-2-official` 与 `gpt-image-2` 属同一模型，差异只在命名与参数能力，并据此把两个接入名归一为同一 Vendor Model——见 `docs/adr/0013-retire-gpt-image-2-use-2-5-models.md`。**本文件只记录平台侧证据：该归一判断不是从第一方文档推导出来的，而是运营决策。** 因此规划不得把它当作上游事实引用，也不需要再索要「权威出处」——运营决策本身就是依据。

## 7. 实测补录：`quality` 的位置随端点族变化，且 2.5 改了取值

抓取时间 2026-09-19，来源 `GET https://aihubmix.com/call/schema/models/{model}/endpoints`。

| 模型 | `/ai/v1/images/generations`（`kind=image`） | `/v1/images/generations`（`kind=openai_compatible`） |
| --- | --- | --- |
| `gpt-image-2` | 参数含 `extra`（`quality` 在 `extra` 内） | 参数含**顶层 `quality`** |
| `gpt-image-2.5-flare` / `-sunburst` | 同上，参数含 `extra` | 同上，参数含**顶层 `quality`** |

两代模型在**同一个端点族内部**的 `quality` 位置一致，但**跨端点族不同**：OpenAI 兼容端点把 `quality` 作为**顶层参数**，而平台原生 `/ai/v1` 端点把它放在 `extra` 对象里。这与第一阶段技术设计（Native 字段保留厂商原生语义、不做跨厂商翻译）的取向一致——两个端点族的原生合同本就不同，Adapter 必须各按各的 Schema 组装。

**取值变化（事实）**：

```json
// gpt-image-2
"quality": {"enum": ["low","medium","high"], "type": "string",
            "description": "Output quality. high/medium/low for GPT image; …"}

// gpt-image-2.5-flare（-sunburst 同）
"quality": {"enum": ["low","medium","high","xhigh","max","auto", null],
            "type": ["string","null"], "default": "auto",
            "description": "GPT-Image-2.5 output quality."}
```

即：**新增 `xhigh` / `max` / `auto`，并把 `auto` 设为默认**；`type` 从 `string` 变为 `["string","null"]`。

**两款 2.5 的端点集合与顶层参数与 `gpt-image-2` 完全一致**（三个端点：`/ai/v1/images/generations`、`/v1/images/edits`、`/v1/images/generations`），因此 Adapter 的端点分流逻辑不需改变。

## 8. 事实：schema 不含响应定义，计量来源须由实测确认

三个端点的 `request` 只含 `envelope` 与 `schema`，**没有 `response` 定义**——机器 Schema 描述请求合同，不描述响应。因此「各渠道在响应里返回什么计量」**无法由 Schema 结清**，只有真实调用的响应样本能回答。

平台侧已有的相关实测（第一阶段，`out-reference/aihubmix/gpt-image-2-inferera-research.md`）：

- AIHubMix 的 OpenAI 兼容端点样本返回**分项 token**：`input_tokens` / `input_tokens_details.{image_tokens,text_tokens}` / `output_tokens` / `output_tokens_details.{image_tokens,text_tokens}` / `total_tokens`；文生图样本为文本输入 24、图片输入 0、图片输出 196；图片编辑样本为 27 / 1024 / 196。
- 其模型页公开价为 **token 计价**（文本输入 `$5`/图片输入 `$8`/图片输出 `$30` per 1M），而 2.5 的 `llms.txt` 摘要写「per-generation」——**措辞不精确，应以模型页与响应 `usage` 为准**。
- AIHubMix 的 `/ai/v1` 异步 Task 对象**没有 `usage`**（第一阶段已确认），故该路径不作为正式计费执行路径（见 `docs/adr/0005`）。

⇒ **平台侧结论（待真实调用确认）**：两家都以 token 计价；差别在于**是否直接返回分项计数**——AIHubMix 返回，APIMart 的渠道版只返回上游声明的扣费金额。因此「金额」的正确用途是**交叉校验**，不是第三种计量维度。

## 9. APIMart 2.5 的计费事实与两家的真实差别（决定性）

来源：`out-reference/apimart/gpt-image-2.5-generation.cn.md`（首方文档原文快照）与 `out-reference/apimart/tasks-status.cn.md`。

### 9.1 事实：APIMart 2.5 公布了官方 token 单价表

文档「计费说明」段原文：「GPT-Image-2.5 按实际 **token** 用量计费，Flare 与 Sunburst 单价相同。最终费用请以价格页面或 `/api/pricing` 返回的实时值为准。」

| 项目 | 每 100 万 token 单价 |
| --- | --- |
| 图片输出 | `$30.00` |
| 图片输入 | `$8.00` |
| 图片输入（缓存命中） | `$2.00` |
| 文本输入 | `$5.00` |
| 文本输入（缓存命中） | `$1.25` |

「实际扣费还会受到**账号分组倍率和折扣**影响。」

文档另给 `1024×1024` 输出的 token 参考：`low` 196 / `medium` 439 / `high` 1756 / `xhigh` 3122 token。

⇒ **本文只采用文档给出的 token 单价与 token 参考值**（它们是「按 token 计费」这一口径的直接证据，可用于设计阶段的量级判断）。**价格页的按张数字一概不予采用**——见 `billing-basis.md`。

### 9.2 事实：APIMart 的任务查询响应**没有** `usage`

`out-reference/apimart/tasks-status.cn.md` 全文检索：`usage` **0 次**、`input_tokens` **0 次**、`output_tokens` **0 次**。成功响应示例（图像任务）字段为：

```json
{"code":200,"data":{"id":"task_…","status":"completed","cost":0.15,"credits_cost":1.5,
 "progress":100,"result":{"images":[{"url":["https://…png"],"expires_at":1763174708}]},
 "created":1763088289,"completed":1763088308,"estimated_time":60,"actual_time":19}}
```

即：**金额（`cost`/`credits_cost`）有，token 分项没有**；`usage` 的分项示例只出现在其 `gpt-image-2-official` / `gpt-image-2.5` 的另一处文档口径里。

**待确认（只能由真实调用回答）**：APIMart 2.5 的任务响应**实际**是否包含 `usage`。文档未承诺，而文档又声明按 token 计费——两者需以真实样本结清。**本轮不做付费调用。**

### 9.3 由此得到的「两家真实差别」

| | 分项 token 计数 | 上游声明金额 |
| --- | --- | --- |
| **AIHubMix** | ✅（第一阶段实测样本：文本输入 24 / 图片输入 0 / 图片输出 196；编辑 27 / 1024 / 196） | ❌ |
| **APIMart** | ❓（文档未承诺，须实测） | ✅（`cost`，可按 `task_id` 逐笔关联） |

**这决定了结算设计的形状**：AIHubMix 走「计量量 × 已发布单价」，APIMart 可能只能走「上游声明的扣费金额」。两者都需要，构成**两条并列的证据形态**，对应 `docs/adr/0010`（token 主路径）与 `docs/adr/0012`（金额替代路径，待批准）。

### 9.4 事实：文档提到的 `/api/pricing` 不可用

文档称「最终费用请以价格页面或 `/api/pricing` 返回的实时值为准」。本机只读探测 `apimart.ai/api/pricing`、`api.apimart.ai/api/pricing`、`api.apimart.ai/v1/pricing`、`apimart.ai/v1/pricing`、`api.apimart.ai/pricing`、`www.apimart.ai/api/pricing` **全部 404**。因此价格只能从价格页（HTML）或真实调用取得，**不存在可用的机器可读价格端点**。

## 10. 本轮落盘的资料：只留「v4 规划与实施会用到」的

入库标准是**会不会被用到**，不是「读过就存」。因此只保留三类：机器可读的请求契约、异步任务的查询合同、以及结算口径规则。

| 文件 | 来源 | 会被用在哪里 |
| --- | --- | --- |
| `aihubmix/schema-gpt-image-2.5-flare.endpoints.json` | `aihubmix.com/call/schema/models/gpt-image-2.5-flare/endpoints` | 写 `capability_schema`（发布物必需输入：闭合对象、`model.const`、参数枚举） |
| `aihubmix/schema-gpt-image-2.5-sunburst.endpoints.json` | 同上，`-sunburst` | 同上（第二个模型） |
| `aihubmix/schema-gpt-image-2.endpoints.json` | 同上，`gpt-image-2` | 对照上一代，证明 `quality` 位置的继承关系（§7） |
| `apimart/gpt-image-2.5-generation.cn.md` | `docs.apimart.ai/cn/api-reference/images/gpt-image-2.5/generation.md` | APIMart 的线上请求合同（`quality` 六档、`xhigh`/`max` 的 400 边界） |
| `apimart/tasks-status.cn.md` | `docs.apimart.ai/cn/api-reference/tasks/status.md` | **实施必需**：APIMart 是异步的，Adapter 必须按 `task_id` 轮询取结果 |
| `apimart/billing-basis.md` | 由价格页与 2.5 文档提炼 | 结算口径两条规则 |

**有意未入库**（读过即够，入库只增噪音）：

- 三份 `model-*.llms.txt` 营销摘要页——内容已被机器 schema 覆盖，且其中 `Pricing: per-generation` 的措辞与实测不符；
- `docs.apimart.ai/llms.txt` 站点导航索引——与接口合同无关；
- 价格页原始 HTML（1.21 MB）及其价格数字——见 `billing-basis.md` 的说明；
- 上一代合同（`gpt-image-2` 生成页、`gpt-image-2-official` 页）与 AIHubMix 的 `/ai/v1` 异步任务文档——**与第二阶段的 Provider 集（2.5 + AIHubMix/APIMart）不直接相关**。它们描述的是上一代模型合同与一条本阶段不采用的执行路径；需要时按来源 URL 重新取。
- `aihubmix.com/models/retirements` 页面——本轮只记录检索结论（正文中 `gpt-image` 命中 0 次），未保存页面。

**两处来源说明**：`docs.apib.ai` 与 `docs.apimart.ai` 返回**完全相同**的页面内容（同一套文档的两个域名），只保存一份，来源记为 `docs.apimart.ai`；`docs.apib.ai/.../gpt-image-2.5/status.md` 返回 404（任务查询的真实路径是 `tasks/status`）。

## 11. 引用的来源（追加）

- `https://docs.apimart.ai/cn/api-reference/images/gpt-image-2.5/generation.md`（2026-09-19，HTTP 200，已读计费与质量档位段落）
- `https://apimart.ai/zh/pricing`（2026-09-19，HTTP 200，6 档 `flare@`/`sunburst@` 价格与三个独立模型条目）
- `https://docs.apimart.ai/cn/api-reference/images/gpt-image-2/generation.md`（2026-09-19，HTTP 200，`gpt-image-2-ext` 别名声明）
- `https://docs.apimart.ai/cn/api-reference/images/gpt-image-2/official.md`（2026-09-19，HTTP 200，独立模型页）
- `https://www.volcengine.com/docs/82379/…` 与火山方舟相关实测见 `out-reference/doubao/doubao-ark-image-research.md`

