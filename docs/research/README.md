# 调查与探索存档（`docs/research/`）

**放什么**：本仓库 Agent 做过、且值得留档的**调查与探索记录**——为了回答某个问题去读一手来源（上游文档、机器 Schema、只读 API、真实响应样本）之后写下的结论。

**不放什么**（各有归属，别重复登记）：

| 内容 | 去哪 |
| --- | --- |
| 上游或第三方的**原始材料**：文档快照、Schema 快照、实测响应 JSON、错误码表 | `out-reference/<provider>/`（**只作证据，服务不读取**） |
| 已经归纳定稿、要长期查证的**渠道事实** | `docs/facts/channel-facts.md`（渠道事实的单一出处） |
| **决定**（选了哪个方案、为什么） | `docs/adr/` |
| **执行清单**（步骤、停止条件、留档要求） | `docs/verification/` |
| 工程变更与交付理由 | `.agents/notes/` |
| 代码结构 / 落点 | `docs/architecture.md` |

## 写法要求

1. **区分"事实 / 推论 / 待确认"**，每条尽量带来源（URL + 抓取时间）。
2. **结论被推翻时要留更正记录**：不要静默改成新结论，加一段"更正记录（日期）"说明原来写了什么、为什么错了、现在以什么为准。本目录里的文件保留这种痕迹，是有意为之。
3. **不跨渠道混写**：一个渠道的调研不夹带另一个渠道的结论（依据 `docs/design/0004` R1——渠道差异不得互相推导）。跨渠道的归纳只放 `docs/facts/channel-facts.md`，且按渠道分节。
4. **不入库敏感信息**：真实 Key、Bearer token、上游 task id、短期结果 URL、用户提供的含上述内容的截图，一律不落仓库；只记存在性、长度级信息与主机名。
5. 文件名沿用被引用时的原名（便于引用稳定）；**移动或改名时在同一变更里更新全部引用**。

## 现有文件

| 文件 | 内容 | 状态 |
| --- | --- | --- |
| [`gpt-image-2-inferera-research.md`](./gpt-image-2-inferera-research.md) | AIHubMix `gpt-image-2`：三条端点、Native Schema、错误与幂等、`/ai/v1` 与 `/v1` 的真实响应、受控实测（§13） | 已更正两处结论（见文首"更正记录"） |
| [`gpt-image-2.5-schema-research.md`](./gpt-image-2.5-schema-research.md) | AIHubMix `gpt-image-2` 与 `gpt-image-2.5` 两款的机器 Schema、端点族差异、`quality` 位置 | 已从"混写 APIMart"改为**只写 AIHubMix**，APIMart 内容移出（见文首"更正记录"） |

**同类但仍在 `out-reference/` 的历史调研**（本仓库自己写的分析，按上面的分工应当迁到本目录；**尚未迁移，等你确认**）：

| 文件 | 说明 |
| --- | --- |
| `out-reference/apimart/apimart-image-api-research.md` | APIMart 的供应、参数、价格页与计价口径张力 |
| `out-reference/doubao/doubao-ark-image-research.md` | 火山方舟图片生成合同（该 Provider 暂不做） |
| `out-reference/openrouter/openrouter-image-api-research.md` | OpenRouter 图像 API 与计量合同（未接入） |

迁移会同时改动 `docs/adr/0014`、`docs/design/0003` 等处的引用；确认后再动，避免半途状态。
