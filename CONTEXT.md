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
向平台实际提供模型调用和账单的服务方，例如 AIHubMix。**Vendor 与 Provider 是角色而非身份类别**：同一主体可以同时是某个模型产品的 Vendor、又是它的 Provider（厂商直连自营时，如火山方舟之于 ByteDance 的 Seedream）。Provider 可以只供应一家 Vendor 的模型（直连型），也可以供应多家（聚合型）——这是供应范围的自然结果，不是两种不同的层。
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
Generation Job 对某个 Offering 和 Channel 发起的一次外部副作用尝试。它同时承载对账标识（Provider 的逐请求标识），该标识只用于对账，不参与计价，也不属于 Metering Evidence。
_Avoid_: 重试、Job

**Reference Image / Mask**（参考图与遮罩）:
调用方给出的图片参数值：公网 URL 或 data URL。它**只是参数值**——平台不落盘、不校验其内容、不给它独立身份，由渠道决定接受什么形态、拒绝什么形态。平台认的调用方图片契约字段只有 `image` / `image_urls` / `mask` 三个名字（`image` 与 `image_urls` 同义）；候选**声明**的参数名另有一套判定：名字以 `image` 开头的是参考图、含 `mask` 的是遮罩（**两者都像时以遮罩为准**），它只用于"把调用方的图落到该候选的哪个参数上"，不用于拦截调用方字段。合同过滤与承载校验见 [Vendor Model Contract 与 Offering Parameter Mapping 落地设计](docs/design/0005-vendor-model-contract-and-offering-mapping.md) §4。
_Avoid_: Asset、资产、素材库、把渠道参数名当作模型参数名、在每个渠道重复一套判定

**Result Envelope**（结果信封）:
上游交付结果时给的 `url` 或 `b64_json`，原样进入对客响应的 `data[]`（每项只保留其中之一）。平台**不下载、不解码、不归档**；链接的有效期与长期保存由调用方自己负责。
_Avoid_: 平台结果资产、归档、本地副本

**Metering Evidence**:
Provider 成功响应或账单中可核验的计量事实，不包含平台价格计算结果。对账标识**不属于**本词条的一部分——它由 Generation Attempt 自己承载，见 `Generation Attempt`。
_Avoid_: 费用、估算值

**Token Usage**:
`Metering Evidence` 中"分项 token 计数"这一形态：上游返回的文本输入/图像输入/文本输出/图像输出四项计数。计量形态跟着渠道的计费方式走（按 token、按张数、按次数、或上游直接声明的金额），不是必须统一成 token。领域里由 `TokenUsage` 承载，计量事实本身仍归 `Metering Evidence` 词条。**它不是独立于证据之外的第二个概念**——渠道报出来的金额另有归宿：它是**成本事实**（见 [Provider Cost](#provider-cost渠道成本事实)），不是计量证据。
_Avoid_: Metered Usage、费用

**Provider Cost**（渠道成本事实）:
一次执行留下的**成本平面**事实：**成本从哪来**，取值只有三个（`computed` = 渠道不给金额字段，平台按该条 Offering 的[计价形态](#pricing-formula计价形态)自算；`declared` = 渠道终态**直接给了金额**，比自算权威，直接取它；`unavailable` = 本该有金额却拿不到、或按登记的形态算不出来——**不得猜测**），以及金额与**该渠道声明的币种**（不假定 USD）。它与 [Pricing Formula](#pricing-formula计价形态) 是两个层级：**计价形态说这个渠道按什么单位算钱**（渠道事实，随发布冻结），**来源三态说这一笔的钱实际从哪来**（执行事实）。它与 `Metering Evidence` 并列但**不是计量证据**：金额不替代分项 token，也**不参与对客金额**（对客只有一个币种 CNY），只进 [Gross Margin](#gross-margin毛利) 口径。只有请求根本没交到渠道的执行才没有成本事实，那时的空值是"根本没采"，不是"成本是 0"；`unavailable` 那笔**不进对账态**（对客结算照常完成），缺口由运营核上游账单后补录。成本事实的采集、折算与缺口处置见 [定价、保底与结算](docs/design/0007-pricing-floor-and-settlement.md) §7。
_Avoid_: 把渠道报的金额当成计量证据或对客售价、用"金额对不对"代替来源判定、拿不到金额时用自算或 0 顶替、把成本缺口推进对账态、把计价形态与来源三态混成一个量

**Routing Priority**:
同一 Vendor Model 的候选 Offering 之间的**档位**，随 Runtime Revision 发布；数字小者优先，受理时在合格候选里取档位最小的那一档。它是**发布决定**，不由请求参数或 Adapter 决定，也不由价格自动推导。发布时也可以给多条候选同一个档位，让它们**落在同一档**（同档再按 [Routing Weight](#routing-weight档内权重) 分摊）。档位怎么表达、怎么取见 [路由策略与缓存](docs/design/0008-routing-strategy-and-caching.md) §1–§2。
_Avoid_: 价格优先、负载均衡、把档位顺序说成"可配置的策略"、把"档内分流"说成改变档位顺序

**Routing Weight**（档内权重）:
某个候选 Offering 在**同一档位内**的分流比：正整数，随 Runtime Revision 发布。它**不改变档位顺序**、不看价格、不看健康度或延迟——它是发布者给出的分流比，与 [Routing Priority](#routing-priority) 同为发布数据，不是核心服务内置的择优规则。分摊是**确定性的**：同一请求重放必然落同一条候选，不引入随机数发生器。**不合格的候选不进分摊**，因此权重再大也换不来一次选中；落点是判定记录的一部分，事后可重建"为什么是它"。分摊的输入与定序见 [路由策略与缓存](docs/design/0008-routing-strategy-and-caching.md) §2。
_Avoid_: 加权轮询 / 负载均衡、按权重改变档位顺序、把权重当健康度或价格择优、用随机数发生器分流

**Price Plan**:
一个 Offering 的**渠道成本价目**：**这个渠道按 token 计量量计价时**的那份四档 token 单价（文本输入 / 图像输入 / 文本输出 / 图像输出，每 1M tokens）、**成本侧的渠道币种**与价目出处。它**只是一种计价形态的参数**——[计价形态](#pricing-formula计价形态)是渠道事实（按 token 计量量 / 按产出张数 / 按调用次数 / 上游直接给金额），所以渠道按张、按次计价或直接由上游给金额时，这条 Offering **没有** Price Plan；**按 token 计量量计价时它仍是必填**。它的费率是**渠道成本费率**（定价时的参考口径与毛利核算用），**不是对客结算基数**——对客售价走 [Consumer Rate Vector](#consumer-rate-vector对客费率向量)。价格变化只通过发布新的 Price Plan 生效；汇率不在这里固定，见 [FX Rate](#fx-rate折算率)。落点与发布期校验见 [定价、保底与结算](docs/design/0007-pricing-floor-and-settlement.md) §1。
_Avoid_: 当前价格、把 Price Plan 的费率当对客售价、把"上游直接给金额"当成一种 Price Plan、把计价形态当成运营的定价选项

**Pricing Formula**（计价形态）:
一条 Offering 的**渠道事实**：这个渠道的这个模型**按什么计价**——`token_rates`（按四分项 token 计量量）、`per_image`（按产出张数）、`per_call`（按调用次数）、`upstream_declared`（上游终态直接给实扣金额）。它由渠道决定、平台如实登记（出处是渠道文档或实测），**不是运营的选项**：管理员最多登记或更正这条事实。它**决定成本怎么算**（平台与渠道怎么结算），**不决定对客卖多少钱**（对客走 [Consumer Rate Vector](#consumer-rate-vector对客费率向量)）；**上游给了金额就先取它**（见 [Provider Cost](#provider-cost渠道成本事实) 的 `declared`），只有拿不到金额时才按登记的形态自算。它随 Runtime Revision 发布、随 Job 的 [Price Snapshot](#price-snapshot) 冻结；每条 Offering 另声明它的**成本币种**。形态与参数的配套、发布期校验见 [定价、保底与结算](docs/design/0007-pricing-floor-and-settlement.md) §1。
_Avoid_: Cost Formula、把计价形态当运营的定价策略、把"按张 / 按次"当成对客计费单位、用计价形态决定对客售价

**Consumer Rate Vector**（对客费率向量）:
某个 Offering 的**对客四档 CNY token 费率**（文本输入 / 图像输入 / 文本输出 / 图像输出，每 1M tokens），**随 Runtime Revision 按候选发布**、**受理时随 Price Snapshot 冻结**。它是**按 token 计量量计价的候选的对客价**：实收由这份向量与本次实际用量算出，结算只读冻结的那一份；其余三种计价形态**没有这个载体**，它们的对客价由成本单价乘 [Markup](#markup加价系数) 算出来。**可被路由的供给必须能给出对客价**（或旧口径那份 Price Plan 费率）——给不出就发布期拒，**不按 0 收**。管理员按该候选的**成本单价**乘倍率推导，也可以直接录入。**同一个 Gateway Model 的不同候选价格不同**——候选的成本不同，售价就不同。它不是渠道成本费率：两者是两个量，混用会让成本跟着售价漂移。推导口径与旧口径见 [定价、保底与结算](docs/design/0007-pricing-floor-and-settlement.md) §2–§3。
_Avoid_: 把渠道成本费率当对客售价、用一个单值推出四档向量、按网关模型或全平台一个价、把它当成每种计价形态都要有的对客价载体

**Markup**（加价系数）:
**每个 Gateway Model 一个**的倍率（倍率 = 1 + `markup_bps` / 10000），随 Runtime Revision 发布、随 Job 的 Price Snapshot 冻结。按 token 计量量的候选由管理员按它推导 [Consumer Rate Vector](#consumer-rate-vector对客费率向量)；其余三种计价形态的对客价由结算按**冻结的那份倍率**算出来。数值由后台录入，不是设计决策。
_Avoid_: 全局加价、改价不用发布、在代码里写一个默认倍率

**FX Rate**（折算率）:
**按币种维护**的"渠道币种 → CNY"折算率，带**生效时间**，由管理员在后台维护；**同币种也按币种录一行**（例如 CNY → CNY），那一行的率恒为 1、折出来逐位不变，所以同币种渠道的供给发布得出去、且不产生折算。受理时按该候选的成本币种取受理时刻生效的那一份并随 Price Snapshot 冻结，受理之后不再换算；同一时刻同一币种全平台只有一个数（否则对账对不起来）。它**不进不可变修订**：放进每份发布里，改一次汇率就要重发所有型号。**没有折算率的币种在发布期被拒**。它只用于把成本折算成 CNY 做毛利核算，**不参与对客金额**。取值规则与录入入口见 [定价、保底与结算](docs/design/0007-pricing-floor-and-settlement.md) §2/§8。
_Avoid_: 把汇率固定在每份发布里、受理后再换算、用浮点算钱、拿对不上币种的汇率去折、把某个币种写死成"不用折算"

**Floor Amount**（保底额）:
受理时按**供给（vendor + offering）维度**从该供给的保底表查得的预授权额，**CNY**；保底表随 Runtime Revision 发布、随 Price Snapshot 冻结、**不编进代码**。供给内按 `(size, quality)` 两维给额，`quality` 留空即按 `size` 档；**像素型的 `size` 先归到档位**，`size = auto`（或没给 `size`）取默认档。它的**两个身份**：① **准入闸门**——受理时 `余额 ≥ 保底额` 才放行，不足即 402 `insufficient_balance`；② **结算的参考下限**——**不是上限**：实收按实际算，估小了由结算**透支**吸收（余额可为负），估大了结算释放差额。它**不由售价派生**。归位规则与回落链见 [定价、保底与结算](docs/design/0007-pricing-floor-and-settlement.md) §6。
_Avoid_: 把保底额当售价或结算上限、按张数乘单价、按网关模型或全平台一个数、写进代码、把像素尺寸一律当成"归不出档位"

**Gross Margin**（毛利）:
`售价（CNY）− 成本折算后 CNY`，按 Job 可查。**两条线分开留痕**：售价 / 保底 / 扣费记 CNY（账本是权威，Price Snapshot 是冻结的那一份），成本记**原币种原值 + 币种 + 当时汇率 + 折算后 CNY**。成本来源可辨（`computed` / `declared` / `unavailable`）；来源是 `unavailable` 时标**"成本未知"**——金额与折算值留空，**不猜**（不写 0、不用费率顶替）。**上游声明的实际金额只影响毛利，不改对客金额**。落点见 [定价、保底与结算](docs/design/0007-pricing-floor-and-settlement.md) §5 与 [代码结构图](docs/architecture.md) §5。
_Avoid_: 用售价顶替成本、拿不到成本就写 0、把毛利算成"售价 − 参考成本"（参考成本只是定价参考）

**Price Snapshot**:
Job 受理时固定的计价单位、单价和公式版本；受理之后不再换算。**对客平面**（一律 CNY）：命中候选的 [Consumer Rate Vector](#consumer-rate-vector对客费率向量)、档位价目表（只作参考与展示）、该供给的保底表与算定的 [Floor Amount](#floor-amount保底额)、命中的候选。**成本平面**（按该供给声明的成本币种）：[计价形态](#pricing-formula计价形态)与它的参数、参考成本与成本币种、成本来源口径，以及受理时取的那一份 [FX Rate](#fx-rate折算率)。**对客实收 = 成本单价 × [Markup](#markup加价系数) 倍率 × [FX Rate](#fx-rate折算率) × 本次实际量**（按 token 计量量时读冻结的那份向量）；**算不出对客价 = 这条供给没有对客计费基准**——发布期就拒，运行时真遇到（只有历史修订才可能）按**平台侧故障**处置，**不按 0 结算**。字段与各量的落点见 [定价、保底与结算](docs/design/0007-pricing-floor-and-settlement.md) §3。
_Avoid_: 当前价格、受理后再换算汇率、按发布物现价结算已受理的 Job、把"没有对客计费基准"当成 0 元计价

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
运营发布时给某个 Vendor Model Revision 的那个**平台型号名**；对客接口里它就是对外的 `model`。它与 **Vendor Model**（厂商的模型产品）、以及真正发给渠道的 **Provider Model** 是三个分开的角色：同一次调用里，调用方只认 `model`，落到哪个厂商模型、换成什么渠道模型名，由平台内部决定。
_Avoid_: 把 `model` 当成厂商原生模型名、让调用方按渠道改写模型名、把三个角色合并成一个字段

**Acceleration Cache**（加速层）:
路由候选集与账户余额的缓存，**只做加速、不是事实源**：金额判定与选路结果的正确性不依赖它，扣减与余额事实只在 PostgreSQL 事务里发生。缓存写入一律发生在**数据库提交之后**、写的是**提交后的值**（不是增量命令），与数据库不一致时以数据库为准。这一层没配置时不构造，行为与没有它时相同；配了但连不上、超时或命令报错，一律当"这次没命中"，回源数据库。键与值见 [路由策略与缓存](docs/design/0008-routing-strategy-and-caching.md) §7.2。
_Avoid_: 事实源、余额权威、用缓存里的数扣费、增量写入、把缓存不可用当成服务不可用

**Balance Cache Entry**（余额缓存条目）:
余额缓存条目的值：余额（CNY，**可为负**——透支发生在结算）、**数据库盖章的写入时间**，以及来源标记（`db_commit` = 数据库提交后的写穿；`reconciler` = 定时对账写回的副本）。只有来源是 `db_commit` **且**写入时间落在新鲜窗口内的条目，才允许用来提前拒绝；其余一律交给数据库的条件更新判。
_Avoid_: 拿对账写回的值拒绝客户、用进程时钟当写入时间、把缺时间戳的旧格式条目当新鲜

**Route Cache**（候选集缓存）:
该网关模型当前生效候选集的缓存值，外加**写它那次发布的修订标识**。受理时与当前生效修订比对，不一致（或值里没有这个标识）就当未命中、回源数据库——因此陈旧是**可检的**，不依赖发布后的失效一定成功。网关模型的启用开关不进这个判定。
_Avoid_: 靠"发布后失效成功"保证正确性、把启用开关交给缓存判、把缓存里的候选集当成发布物

**Cache Reconciliation**（缓存对账）:
以数据库为准把加速层覆盖回去的定时任务：把余额按增量写回（来源标记 `reconciler`）、把不是当前生效修订的候选集拿掉，发现不一致时覆盖并写审计。它**不是** [Reconciliation Case](#reconciliation-case)：后者是上游受理状态不明、要人工处置的业务事实。
_Avoid_: 把缓存对账当成业务对账、用缓存对账替代账实核对
**Ledger Audit**（账实核对）:
按账户比对**账本汇总**（`ledger.entries` 的金额之和）与**账户余额**（`ledger.accounts.balance_microusd`）的定时任务：两者本该一体（每笔分录都同时改两边），不等即说明有漏写、手工改库或半提交。不一致时**建一条账户级对账案例**（一直对不上只留一条）并**只在新建那次**发一条平台侧告警。它**只发现、不改账**——自动改回去会把"为什么对不上"一起抹掉。它**不是** [Cache Reconciliation](#cache-reconciliation缓存对账)：那条比的是库与缓存两份副本，这条比的是库里两个事实。
_Avoid_: 把账实核对当成缓存对账、让核对任务自动改账、每轮不符都刷告警


**Route Policy**（路由策略）:
在一批合格候选（[Offering](#offering)）里"挑哪一条"的**运行期配置**：全局一条、可按 [Gateway Model](#gateway-model) 覆盖，未配置时默认 `priority_failover`。四种策略：`priority_failover`（按档位顺序、档内按权重）、`weighted_random`（不看档位、在全部合格候选里按权重）、`least_cost`（按**折后成本估算**最小）、`user_tag`（按 [Account Tag](#account-tag账户标签) 经映射指定）。它**不进不可变修订**（不是"卖什么"，而是"在已发布的合格候选里怎么选"），改它即刻影响之后的受理、已受理的 [Generation Job](#generation-job) 不受影响。取值空间**只有合格候选**：承载面表达不了这次请求的候选先被排除，策略与标签映射都指定不了它们；`least_cost` 与 `user_tag` 给不出答案时退回默认顺序，不判失败。
_Avoid_: 把策略当成发布内容、用策略或标签映射绕过承载校验、把"档位顺序"说成策略、把折扣率当成本口径、把退回默认顺序说成"策略生效了"

**Account Tag**（账户标签）:
账户上的一个由运营设的字符串，**只有生效的 `user_tag` 策略消费它**：策略里的映射把它对应到某条候选。没有那条策略时，改标签不改变任何选路结果；映射指向的候选这次承载不了时也不选它。
_Avoid_: 把标签当权限或计费档、用标签绕过承载校验、以为设了标签就会改变选路（要有生效的策略）
