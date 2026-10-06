# AIHubMix：`gpt-image-2` 与 `gpt-image-2.5` 两款的机器 Schema 与端点族实测

> 上游协议研究与只读取证记录，**不是平台对外接口合同**。所有证据来自无需鉴权的机器可读端点，**未发起任何计费调用**，未使用任何 API Key。
> 抓取时间：2026-09-19（三个模型的三类端点均返回 HTTP 200）。
> **范围**：只写 AIHubMix。跨渠道的归纳与另一渠道的事实见 [`docs/facts/channel-facts.md`](../facts/channel-facts.md) 与 `out-reference/apimart/`（依据 `docs/architecture.md` R1：渠道差异不得互相推导）。

## 更正记录（2026-09-19，本次整改）

1. **本文件原先混写了 APIMart**（原第 5、8、9、10 节：APIMart 的供应声明与别名、`quality` 边界、token 单价表、`/api/pricing` 探测、落盘清单与来源）。这些内容已**整节移出**——一个渠道的调研不夹带另一个渠道的结论。
2. **被移出的内容里有一条结论是错的**：原第 8.2 节写「APIMart 的任务查询响应**没有** `usage`」。**该结论已被 2026-09-19 的真实调用推翻**——APIMart 的任务完成响应含**四分项** `usage`（`input_tokens_details` 区分 text/image）。正确记录在 [`docs/facts/channel-facts.md`](../facts/channel-facts.md) 的 APIMart 节，原始样本在 `out-reference/apimart/controlled-probe-2026-09-19.json`。
3. **随之作废的推论**：原第 7 节末「APIMart 的渠道版只返回上游声明的扣费金额」、原第 8.3 节「两家真实差别＝一家有分项 token、另一家只有金额」——两家**都有**四分项 token；上游声明的金额与按公开费率算出的金额不一致（见 [`docs/facts/channel-facts.md`](../facts/channel-facts.md) 的 APIMart 节）。
4. 原文的 `gpt-image-2-official` / `gpt-image-2-ext` 别名核对属 APIMart 事实，已移出；需要时看 APIMart 自己的文件（`out-reference/apimart/apimart-image-api-research.md`）。
5. 本次同时把本文件从 `out-reference/aihubmix/` 迁到 `docs/research/`：它是**我们自己写的调研**，按 `docs/agents/artifacts.md` 的分工不属于「上游或第三方原始材料」。原始响应样本（`gpt_image_2_generations.json`）与 Schema 快照仍留在 `out-reference/aihubmix/`。

## 1. 取证方式

| 端点 | 鉴权 | 用途 |
| --- | --- | --- |
| `GET https://aihubmix.com/call/schema/models/{model}/endpoints` | 无需鉴权 | 该模型的端点、运行时语义与**参数 Schema**（权威、机器可读） |
| `GET https://api.inferera.com/model/{model}/llms.txt` | 无需鉴权 | 模型摘要（开发者、模态、发布日、端点、参数表） |
| `GET https://aihubmix.com/models/retirements` | 无需鉴权 | 渠道的**退役清单**（Deprecating / Retired / Replacement） |

`aihubmix.com` 在本机不可达时，同一份 Schema 在 `api.inferera.com` 上取到（首期 Channel 默认域名）。

## 2. 事实：两款 2.5 确实存在，且 `quality` 档位确有扩展

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

## 3. `lifecycle` 字段的含义（易误读）

Schema 中每个端点带 `lifecycle`，但它描述的是**运行时语义**，不是退役状态：

```json
{"cancel_path":"","mode":"sync","poll_method":"GET","poll_path":"/ai/v1/images/{id}",
 "status_values":["pending","in_progress","completed","failed","cancelled"],"supports_async":true}
```

- `/ai/v1/images/generations`：`supports_async: true`，有 `poll_path`；
- `/v1/images/generations` 与 `/v1/images/edits`：`mode: sync`，`poll_path` 为空。

这与第一阶段的技术设计结论一致（正式计费路径走 `/v1`，`/ai/v1` 为已验证但不返回 `usage` 的异步能力，见 `docs/adr/0005`）。

## 4. 退役清单与命名（AIHubMix 侧）

`GET https://aihubmix.com/models/retirements` 的本轮检索结论：正文中 **`gpt-image` 命中 0 次**——即该清单**没有**列出 `gpt-image-2`，也没有列出 2.5 两款。页面本身未入库（只记检索结论）。

因此：**`gpt-image-2` 的退役是运营/产品决定，不是上游公告**（见 `docs/adr/0013`）。平台不需要「上游已退役」这一证据；平台要做的是让「退役旧供给、发布新供给」可执行、可审计、可回滚。

## 5. 端点族决定 `quality` 的位置

同一个模型在**不同端点族**下的原生合同不同（同一族的两个模型则一致）：

| 模型 | `/ai/v1/images/generations`（`kind=image`） | `/v1/images/generations`（`kind=openai_compatible`） |
| --- | --- | --- |
| `gpt-image-2` | 参数含 `extra`（`quality` 在 `extra` 内） | 参数含**顶层 `quality`** |
| `gpt-image-2.5-flare` / `-sunburst` | 同上 | 同上，参数含**顶层 `quality`** |

这与平台取向一致（Native 字段保留厂商原生语义、不做跨厂商翻译）：两个端点族的原生合同本就不同，② Adapter 必须各按各的 Schema 组装——本仓库选定的执行路径见 `docs/adr/0005`。

## 6. `quality` 的取值变化（Schema 片段）

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

**两款 2.5 的端点集合与顶层参数与 `gpt-image-2` 完全一致**，因此 Adapter 的端点分流逻辑不需要因为 2.5 而改变。

## 7. Schema 不含响应定义：计量来源必须实测

三个端点的 `request` 只含 `envelope` 与 `schema`，**没有 `response` 定义**——机器 Schema 描述请求合同，不描述响应。因此「本渠道在响应里返回什么计量」**无法由 Schema 结清**，只有真实调用的样本能回答。

本渠道已有的实测（第一阶段的付费样本，记录在 [`gpt-image-2-inferera-research.md`](./gpt-image-2-inferera-research.md) §13）：OpenAI 兼容端点返回**分项 token**（`input_tokens_details.{text_tokens,image_tokens}` 等），`/ai/v1` 异步任务对象**没有 `usage`**。归纳后的渠道事实见 [`docs/facts/channel-facts.md`](../facts/channel-facts.md) 的 AIHubMix 节。

**成本价怎么来**（**本渠道自己的口径**）：本渠道**只返回四分项 token、不返回任何金额字段**，因此成本价 = Σ(分项 token × 官方四档费率)。四档费率与实测见 [`docs/facts/channel-facts.md`](../facts/channel-facts.md) 的 AIHubMix 节——渠道之间不互相推导（`docs/architecture.md` R1），另一渠道怎么取成本价看那份文档的 APIMart 节。

## 8. 本轮落盘的资料（只留会被用到的）

| 文件 | 来源 | 会被用在哪里 |
| --- | --- | --- |
| `out-reference/aihubmix/schema-gpt-image-2.5-flare.endpoints.json` | `aihubmix.com/call/schema/models/gpt-image-2.5-flare/endpoints` | 写 `capability_schema`（发布物必需输入：闭合对象、`model.const`、参数枚举） |
| `out-reference/aihubmix/schema-gpt-image-2.5-sunburst.endpoints.json` | 同上，`-sunburst` | 同上（第二个模型） |
| `out-reference/aihubmix/schema-gpt-image-2.endpoints.json` | 同上，`gpt-image-2` | 对照上一代，证明 `quality` 位置的继承关系（§5） |

**有意未入库**：三份 `model-*.llms.txt` 营销摘要页（内容已被机器 Schema 覆盖，且其中 `Pricing: per-generation` 的措辞与实测不符）；`aihubmix.com/models/retirements` 页面正文（只记检索结论，见 §4）。

## 9. 来源

- `https://aihubmix.com/call/schema/models/gpt-image-2.5-flare/endpoints`（2026-09-19，HTTP 200）
- `https://aihubmix.com/call/schema/models/gpt-image-2.5-sunburst/endpoints`（2026-09-19，HTTP 200）
- `https://aihubmix.com/call/schema/models/gpt-image-2/endpoints`（2026-09-19，HTTP 200）
- `https://api.inferera.com/model/{gpt-image-2,gpt-image-2.5-flare,gpt-image-2.5-sunburst}/llms.txt`（2026-09-19，HTTP 200）
- `https://aihubmix.com/models/retirements`（2026-09-19，HTTP 200，`gpt-image` 命中 0 次）
