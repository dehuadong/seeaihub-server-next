# 计量证据承载单位，计价必须声明维度

> **状态：未生效。** 本文随工作项 [seeaihub-server-next#2](https://github.com/dehuadong/seeaihub-server-next/issues/2) 的第二阶段规划起草；**在该规划通过 Plan Review 之前，本文不是实现依据**，也不得被下游工件当作已接受的决策引用。规划正文见 #2 的规划评论（以该 issue 上标注为「当前唯一可执行版本」的那条为准，不在此写死版本号）。

结算不假设「计量必然是 token」，而是要求 **Metering Evidence 是带单位的计量量**、**Price Plan 显式声明它按哪个维度计价**。已应用的维度是 **token**：分项 token 数（`input_text_tokens` / `input_image_tokens` / `output_text_tokens` / `output_image_tokens` 等）。

**逐项必须能唯一落到已声明的单位或档位**，否则不得猜测金额、进入对账。若某 Provider 只有一个恒定单价，应发布**单档覆盖全域**的档位表，而不是省略档位——分档这件事始终显式可审。

## 金额与计量量的边界（本条的约束范围）

**不得由金额反推计量量**——即不能拿上游声明的扣费金额除出一个「用量」再当计量事实用（[0006](./0006-no-settlement-without-metering-evidence.md) 的禁止项）。金额是结果，不是计量。

**「金额本身能否作为结算事实」不由本条决定**。那是一个**独立的持久决定**，属主是 [0012](./0012-provider-declared-charge-as-evidence.md)（Provider 声明的扣费金额作为计量证据）。本条只规定：

- 若某个 Offering 走「计量量 × 已发布单价」这条主路径，则它的 Evidence 必须是**带单位的计量量**，不得写成金额；
- 两个形态**不得在同一 Offering 上并存或隐含切换**，由 Price Plan 在受理时固定；
- 无论走哪条路径，**逐项都要能唯一落到已声明的单位**，落不到就进对账。

**这条边界是必需的**：否则「金额不能替代证据」与「接受金额作为证据」会同时成立，实现者无从判断。**先前的表述（「金额型返回值一律按交叉校验处理、不新增证据类型」）已被本条取代**，因为它把一个尚未结清的独立决定写成了本条结论。

**币种**：账本与 Price Snapshot 以 microUSD 计价。Price Plan 保留**原生币种与原生单价**以及发布时固定的汇率，使「厂商原价 → 平台美元价」可追溯，且已受理 Job 不受后续汇率漂移影响。对本身就以 USD 计价的计划，原生价即 microUSD 本身、汇率取恒等值，**不得为此伪造一个换算**。`native_pricing` 的来源 URL 与抓取时间与 `fx_source_url`/`fx_captured_at` 同为发布必填，汇率不得凭空填入。

**其它计量维度是已声明的能力，但本阶段不启用**：机制上不排除将来按图片张数与像素档位计价。**当前不引入该形态**——已核实的两个 Provider 中，AIHubMix 直接返回分项 token；APIMart 的响应形状**尚未由真实调用结清**（见 [0012](./0012-provider-declared-charge-as-evidence.md)）。届时确有需要，再以新的修订启用，并基于真实需求定义档位边界，而不是预先发明。

**本阶段不预判「要不要扩展证据形态」**：APIMart 的响应到底给分项 token 还是只给金额，属实施期受控验证要结清的事实。在结清之前，本条只声明原则（计量承单位、计价声明维度、不得由金额反推计量量），**不为推测的形态先造机制**。真实形状确定后，若需要扩展，按最小改动另行修订。

**来源**：第二阶段 Planning。实测依据：AIHubMix 的 OpenAI 兼容端点在样本中返回分项 token `usage`（见 `docs/research/gpt-image-2-inferera-research.md`）；APIMart 任务响应（文档示例）返回 `cost`/`credits_cost` 金额，其分项 `usage` 是否在场尚未实测（见 `out-reference/apimart/billing-basis.md` 与 `tasks-status.cn.md`）。
