> 归属状态（2026-10-09）：归属切换未完成；本文件适用且已接受的范围暂保留旧属主，原待评审／待接受内容不因此获批准。依赖它的新工作先核对有效内容及评审缺口，不得默认沿用。 全部映射与未决影响见[归属切换登记](../agents/document-ownership-transition.md)。以下原文与状态头保留其历史身份，不扩大本文权威。

---
status: accepted
---

# 结算的证据门槛、计量维度与成本口径

**没有可核验的计量证据，就不做正式资金结算。** 结算只读取强类型证据与 Job 固化的 Price Snapshot；公开单价只能作为 Price Plan 候选，不能单独证明最终结算依据；不得用请求参数反推用量并当作真实账单；证据缺字段、负数、总量不一致或解析失败时不得猜测费用，进对账；未取得可核验计量证据前，该 Offering 不能以正式资金结算状态发布。Reconciliation Case 不是例外：人工处置当前只能退款并释放全部预授权，不能仅凭金额、备注、业务键或运营判断确认扣款。

**证据必须带单位，计价必须声明维度。** 计量形态跟着渠道的计费方式走——按 token 计费就取分项 token，直接声明金额就用那句金额，按次数或张数计费就取次数/张数；平台不假设"计量必然是 token"。逐项要能唯一落到该 Offering 已声明的单位或档位，落不到就进对账；某渠道只有一个恒定单价时，应发布单档覆盖全域的档位表，而不是省略档位。一个 Offering 只声明一种计费形态，不得并存或隐含切换。**不得由金额反推计量量**——金额是结果，不是计量。

**成本与结算基数是两个量，成本按渠道各自的口径取数。** 上游给了金额就以金额为准；上游没给，才按该 Offering 声明的计费形态算成本（分项 token × 已发布费率、张数 × 单价、次数 × 单价）。`cost` 只用于核成本，不替代计量事实。

**币种：对客只有 CNY 单币种，成本按渠道声明的币种记原值。** 余额、充值、售价、保底额与扣费一律 CNY；成本记原币种原值，可能是 USD、CNY 或别的。**对客价怎么落成 CNY 见 [`0007`](../design/0007-pricing-floor-and-settlement.md) §2**：按 token 四档时是对客 CNY 费率向量 × 实际用量；按上游声明金额 × 倍率时是声明的成本（渠道币种）× 冻结倍率 × 冻结折算率。折算率同时用于把实际成本折算成 CNY 做毛利核算。

**编号存根**：本条合并了 ADR-0010 与 ADR-0016，两个编号保留以便旧链接可解析。

机制（计费形态落在哪、汇率表与快照位、各形态的算式）见 [`../design/0007-pricing-floor-and-settlement.md`](../design/0007-pricing-floor-and-settlement.md)。**声明金额是它自己的一种计量形态**：某条上游面拿不到分项 token、只给实扣金额时，那一句金额就是当次的计量依据，"计量形态跟着渠道的计费方式走"正是它的落点，处置见 [`.agents/notes/implemented/platform/2026-10-06-aihubmix-ai-v1-execution-path.md`](../../.agents/notes/implemented/platform/2026-10-06-aihubmix-ai-v1-execution-path.md)；"把金额当作第二类 **token** 证据"这一候选问题仍是否决的，边界见 [`.agents/notes/rejected/domain/2026-09-19-provider-declared-charge-as-metering-evidence.md`](../../.agents/notes/rejected/domain/2026-09-19-provider-declared-charge-as-metering-evidence.md)。

**反悔成本**：改它要放开结算路径上的证据校验与库层约束，并把已发布的 Offering、已产生的 `ledger.entries` 与已进对账的案例按新口径重新处置——对账的处置口径（只能退款、不得仅凭运营判断确认扣款）也要一起改。
