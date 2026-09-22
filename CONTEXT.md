# SeeAI Hub 新服务端

该上下文描述模型供给、图片生成、任务执行和计费之间的业务语言。

## Language

**Vendor**:
定义模型产品与原生能力的厂商，例如 OpenAI。
_Avoid_: Provider、渠道

**Vendor Model**:
由 Vendor 发布、以原生模型 ID 和修订标识确定的模型产品。
_Avoid_: 平台模型、渠道模型

**Vendor Model Contract**:
调用方针对某个 Vendor Model 提交参数时所遵循的合同：字段名、类型、枚举、默认值、组合约束与能力边界。它表达**模型语义**，不表达任何 Provider 的 HTTP 包装。它随不可变 Runtime Revision 发布，调用方按选中的模型使用它，不按渠道使用它。归属见 `docs/adr/0015`。
_Avoid_: Provider Schema、渠道请求格式、跨厂商统一图片参数

**Provider**:
向平台实际提供模型调用和账单的服务方，例如 AIHubMix。**Vendor 与 Provider 是角色而非身份类别**：同一主体可以同时是某个模型产品的 Vendor、又是它的 Provider（厂商直连自营时，如火山方舟之于 ByteDance 的 Seedream）。Provider 可以只供应一家 Vendor 的模型（直连型），也可以供应多家（聚合型）——这是供应范围的自然结果，不是两种不同的层，因此没有单独的「聚合/直连」类型字段。
_Avoid_: Vendor

**Offering**:
Provider 通过特定 Adapter 和 Channel 提供某个 Vendor Model Revision 的可调用供给。
_Avoid_: 模型、渠道

**Offering Parameter Mapping**:
把 Vendor Model Contract 里的参数转换成某个 Offering **实际要求的渠道包装**的那一层（改名、位置、枚举、单位、默认值、字段的拆分与合并、能力子集的声明）。粒度是 **Vendor Model × Offering**，不是全局 Vendor 或全局 Provider。它属于平台内部，不是调用方契约，也不是 Provider Adapter 的职责（Adapter 只做渠道级传输与归一）。归属见 `docs/adr/0015`。
_Avoid_: 全局 Provider 参数表、跨厂商统一转换、Adapter 的字段翻译、调用方需要知道的渠道包装

**Channel**:
Provider 的一个调用入口及凭证身份，包含地址、凭证引用和启用状态。
_Avoid_: Provider、Offering

**Runtime Revision**:
一次经过校验并发布的不可变运行时目录，固定模型、供给、渠道限制与价格关系。
_Avoid_: 配置文件、当前缓存

**Generation Job**:
**内部的**执行与审计记录：平台已受理、可持久恢复的一次图片生成请求。它承载路由判定、计量证据、对账与结算，**对客不可见**——平台对消费者只有同步调用，不提供任务号轮询，也不把这条记录投射成对客协议。
_Avoid_: 对客任务、任务号、Provider Task、HTTP 请求

**Generation Attempt**:
Generation Job 对某个 Offering 和 Channel 发起的一次外部副作用尝试。它同时承载对账标识（Provider 的逐请求标识，例如响应头 `x-request-id`），该标识只用于对账，不参与计价，也不属于 Metering Evidence。
_Avoid_: 重试、Job

**Reference Image / Mask**（参考图与遮罩）:
调用方给出的图片参数值：公网 URL 或 `data:image/…;base64,…`（遮罩为 PNG data URL）。它**只是参数值**——平台不落盘、不校验其内容、不给它独立身份，由渠道决定接受什么形态、拒绝什么形态。

平台认**调用方契约字段**只有两个名字：`image` 与 `image_urls`（同义、二选一，只有两边都给了非空值才算冲突），以及 `mask`。其余参数按**该 Vendor Model 的合同**过滤：合同里没有的字段**直接丢掉**（不报错、也不发上游）；合同里有、但命中的那条供给**承载面**没有的字段，则该供给**不合格**（换供给；全都不合格按平台侧故障返回，不静默丢）。候选**声明**的参数名另有一套判定：名字以 `image` 开头的是参考图、含 `mask` 的是遮罩（**两者都像时以遮罩为准**）、其余一律拒绝——这一套判定只用于"把调用方的图落到该候选的哪个参数上"，不用于拦截调用方字段。
_Avoid_: Asset、资产、素材库、把渠道参数名当作模型参数名、在每个渠道重复一套判定

**Result Envelope**（结果信封）:
上游交付结果时给的 `url` 或 `b64_json`，原样进入对客响应的 `data[]`（每项只保留其中之一）。平台**不下载、不解码、不归档**；链接的有效期与长期保存由调用方自己负责。
_Avoid_: 平台结果资产、归档、本地副本

**Metering Evidence**:
Provider 成功响应或账单中可核验的计量事实，不包含平台价格计算结果。对账标识**不属于**本词条的一部分——它由 Generation Attempt 自己承载，见 `Generation Attempt`。
_Avoid_: 费用、估算值

**Token Usage**:
`Metering Evidence` 中**当前启用**的计量形态：上游返回的分项 token 计数（文本输入/图像输入/文本输出/图像输出）。计量形态跟着渠道的计费方式走（按 token、按张数、按次数、或上游直接声明的金额），不是必须统一成 token；本阶段启用的是 token。领域里由 `TokenUsage` 承载，计量事实本身仍归 `Metering Evidence` 词条。**它不是独立于证据之外的第二个概念**——「上游声明的扣费金额」能否替代或补充它，已由实测结清：当前两个渠道在采用的路径上都返回分项 token，本阶段**不需要**金额型证据（该候选决策已被否决并退役）。渠道报出来的金额另有归宿：它是**成本事实**（见 `Provider Cost`），不是计量证据。
_Avoid_: Metered Usage（未曾有代码或文档使用该名）、费用

**Provider Cost**（渠道成本事实）:
一次执行留下的**成本平面**事实：**成本从哪来**，取值就是这三个（`computed` = 渠道不给金额字段，平台按**实际分项 token** × 该渠道的**成本费率**自算；`declared` = 渠道终态**直接给了金额**，含渠道侧折扣，比自算权威，直接取它；`unavailable` = 本该有金额却拿不到——**不得猜测**），以及金额、**该渠道声明的币种**（不假定 USD）与**按冻结汇率折算后 CNY**。它与 `Metering Evidence` 并列但**不是计量证据**：金额不替代分项 token，也**不参与对客金额**（对客只有一个币种 CNY），只进 [Gross Margin](#gross-margin毛利) 口径。落点是执行尝试上的成本四列；**成功与"结果交付失败进对账"两条路径都落**。`unavailable` 那笔**不进对账态**（对客结算照常完成），由**成本缺口清单**（`GET /api/v1/provider-cost-gaps`）交给运营去核上游账单，补录归账实核对那条线。
_Avoid_: 把渠道报的金额当成计量证据或对客售价、用"金额对不对"代替来源判定、拿不到金额时用自算或 0 顶替、把成本缺口推进对账态

**Routing Priority**:
同一 Vendor Model 的候选 Offering 之间的选择顺序，随 Runtime Revision 发布；数字小者优先。它是**发布决定**，不由请求参数或 Adapter 决定，也不由价格自动推导。
_Avoid_: 价格优先、负载均衡、把优先级顺序说成"可配置的策略"

**Price Plan**:
某个 Offering 的计价合同：声明**计价形态**（本阶段只有"按 token 计量量"一种；"按上游声明的扣费金额"这一形态经实测已不需要）、该形态所需的单价或来源，以及**成本侧的渠道币种**。它的费率**收窄为渠道成本费率**（定价时的参考口径与毛利核算用），**不再是对客结算基数**——对客售价走 [Consumer Rate Vector](#consumer-rate-vector对客费率向量) 。**汇率不在这里固定**——它是**按币种维护的折算率表**（[FX Rate](#fx-rate折算率)，渠道币种 → CNY，带生效时间），**受理时取"受理时刻生效的那一行"并快照**进 Price Snapshot；**对客只有一个币种（CNY）**（余额、充值、售价、保底额、扣费一律 CNY），**汇率只用于把成本折算成 CNY 做毛利核算，不参与对客金额**。价格变化只通过发布新的 Price Plan 生效。
_Avoid_: 当前价格、费率表、把 Price Plan 的费率当对客售价

**Consumer Rate Vector**（对客费率向量）:
某个 Offering 的**对客四档 CNY token 费率**（文本输入 / 图像输入 / 文本输出 / 图像输出，每 1M tokens），**随 Runtime Revision 按候选发布**、**受理时随 Price Snapshot 冻结**。它是**实收依据**：实收 = 这份向量 × 本次实际用量的分项 token。管理员按"该候选的**成本费率** ×(1 + [Markup](#markup加价系数))× 该币种 → CNY 的 [FX Rate](#fx-rate折算率)"**设定/推导**，也可以直接录入。**同一个 Gateway Model 的不同候选价格不同**——候选的成本不同，售价就不同。它不是渠道成本费率：两者是两个量，混用会让成本跟着售价漂移。
_Avoid_: 把渠道成本费率当对客售价、用一个单值推出四档向量、按网关模型或全平台一个价

**Markup**（加价系数）:
**每个 Gateway Model 一个**的加价系数（整数基点，避免浮点），随 Runtime Revision 发布、随 Job 的 Price Snapshot 冻结。它**参与设定**该模型各候选的 [Consumer Rate Vector](#consumer-rate-vector对客费率向量)（管理员按"成本费率 ×(1 + 加价系数)× 汇率"推导），但**不参与结算**——结算只读受理时冻结的那份向量。数值由后台录入，不是设计决策。
_Avoid_: 全局加价、改价不用发布、把加价系数当结算参数

**FX Rate**（折算率）:
**按币种维护**的"渠道币种 → CNY"折算率（定点整数，分母 1e6，不用浮点），带**生效时间**，由管理员在后台维护（`PUT /api/v1/fx-rates`，写审计）。**受理时按该候选的成本币种取"受理时刻生效的那一行"并原值快照**进 Price Snapshot，受理之后不再换算；同一时刻同一币种全平台只有一个数（否则对账对不起来）。它**不进不可变修订**：放进每份发布里，改一次汇率就要重发所有型号。**没有折算率的币种在发布期被拒**。它只用于把成本折算成 CNY 做毛利核算，**不参与对客金额**。
_Avoid_: 把汇率固定在每份发布里、受理后再换算、用浮点算钱、拿对不上币种的汇率去折

**Floor Amount**（保底额）:
受理时按**供给（vendor + offering）维度**从该供给的保底表（随 Runtime Revision 发布、随 Price Snapshot 冻结、**不编进代码**）查得的预授权额，**CNY**。供给内按 `(size, quality)` 两维给额，`quality` 留空即按 `size` 档；**像素型的 `size` 先归到档位**——优先按该供给发布的**档位像素表**（它的尺寸档案：档位 → 比例 → 像素）反向查，缺失或查不到那一格时按**最长边**阈值兜底（≤1024 → 1K、≤2048 → 2K、>2048 → 4K）；`size = auto`（或没给 `size`）取**默认档 2K**；归不出档位、或该档位在表里没有，回落到该供给的**封顶保底值**；连封顶值也没有才回落到平台兜底数。它的**两个身份**：① **准入闸门**——受理时 `余额 ≥ 保底额` 才放行，不足即 402 `insufficient_balance`；② **结算的参考下限**——**不是上限**：实收按实际算，估小了由结算**透支**吸收（余额可为负），估大了结算释放差额。它**不由售价派生**。
_Avoid_: 把保底额当售价或结算上限、按张数乘单价、按网关模型或全平台一个数、写进代码、把像素尺寸一律当成"归不出档位"

**Gross Margin**（毛利）:
`售价（CNY）− 成本折算后 CNY`，按 Job 可查。**两条线分开留痕**：售价 / 保底 / 扣费记 CNY（`ledger.entries` 与 `ledger.holds` 是权威，Price Snapshot 是冻结的那一份），成本记**原币种原值 + 币种 + 当时汇率 + 折算后 CNY**。成本来源可辨（`computed` / `declared` / `unavailable`）；来源是 `unavailable` 时标**"成本未知"**——金额与折算值留空，**不猜**（不写 0、不用费率顶替），该笔成本缺口进对账口径（见 [Provider Cost](#provider-cost渠道成本事实) 与成本缺口清单）。**上游声明的实际金额只影响毛利，不改对客金额**。
_Avoid_: 用售价顶替成本、拿不到成本就写 0、把毛利算成"售价 − 参考成本"（参考成本只是定价参考）

**Price Snapshot**:
Job 受理时固定的计价单位、单价和公式版本。**对客平面**（一律 CNY）：命中候选的 [Consumer Rate Vector](#consumer-rate-vector对客费率向量)（售价依据）、档位价目表（只作参考与展示）、该供给的保底表与算定的 [Floor Amount](#floor-amount保底额) 及其来源、命中的候选。**成本平面**（按渠道声明的币种）：渠道成本费率、参考成本与成本币种、成本来源口径，以及受理时取的那一行 [FX Rate](#fx-rate折算率)。**缺对客费率向量与保底额 ⇒ 旧口径**（对客扣费按已发布费率、预授权回落平台兜底数），历史 Job 与"迁移后仍生效但没有定价的旧修订"都走这条路。
_Avoid_: 当前价格、受理后再换算汇率、按发布物现价结算已受理的 Job

**Reconciliation Case**:
Provider 是否受理、是否生成或是否计费无法自动确认时，需要独立处置的业务事实。
_Avoid_: 普通失败、自动重试

**Platform Funding Failure**:
平台在某个 Provider 侧的账户余额、额度或权限不足，导致该渠道拒绝**平台的**调用。它不是消费者的问题，属**运营事件**；对客表现**不得**是"余额不足"，且必须能被运营侧发现与处置。
_Avoid_: 用户欠费、`insufficient_balance`（那是消费者余额的语义）、把渠道的信封原样当成对客语义

**Consumer Insufficient Balance**:
消费者在本平台的账户余额不足。它在**受理之前**就被拒绝，是对客可见、有意义的状态。
_Avoid_: 与 Platform Funding Failure 混用；把平台在渠道侧的额度问题说成"用户余额不足"

**Consumer-Facing Error Code**（对客错误码）:
消费者在 Job 上唯一看得到的一类错误标识，只有三个取值：`platform_unavailable`（平台侧故障）、`outcome_unknown`（受理状态不明，已进对账）、`content_rejected`（消费者内容被渠道拒绝）。渠道的 HTTP 状态码、错误码与原文只留内部。内部另记失败类别：欠费、凭证/权限、平台自身、渠道拒绝了平台的请求、渠道不可用、渠道限流、消费者内容、不确定——其中**渠道不可用、渠道限流、消费者内容被拒不是平台侧事件**（可观测，但不是平台要去修的），其余四类与"不确定"算平台侧事件。语义与理由见 `docs/adr/0017`。
_Avoid_: 把渠道的状态码或错误码当对客码、用 `insufficient_user_quota`/`payment_required` 这类渠道标识符对客、把平台欠费说成消费者余额不足

**Gateway Model**（对外的模型字段 `model`）:
运营发布时给某个 Vendor Model Revision 的那个**平台型号名**；对客接口里它就是对外的 `model`。它与 **Vendor Model**（厂商的模型产品）、以及真正发给渠道的 **Provider Model**（Offering 上的 `provider_model_id`）是三个分开的角色：同一次调用里，调用方只认 `model`，落到哪个厂商模型、换成什么渠道模型名，由平台内部决定。
_Avoid_: 把 `model` 当成厂商原生模型名、让调用方按渠道改写模型名、把三个角色合并成一个字段
