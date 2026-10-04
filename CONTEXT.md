# SeeAI Hub 新服务端

该上下文描述模型供给、图片生成、任务执行和计费之间的业务语言。

## Language

**Vendor**:
定义模型产品与原生能力的厂商，例如 OpenAI。
_Avoid_: Provider、渠道

**Vendor Model**:
由 Vendor 发布、以原生模型 ID 和修订标识确定的模型产品。
_Avoid_: 平台模型、渠道模型

**模型类型**（Model Type）:
Vendor Model 的产品种类，取值为 `image`、`video`、`chat`。它是模型自身的事实，随发布素材声明，不随渠道、候选或定价变化；用量记录与账单汇总按类型决定用量的单位——图片是产出张数，视频是产出秒数，对话是输入与输出 token。它不是计费口径，也不决定一个模型能不能调。
_Avoid_: 计费类型、计价形态、按类型的路由或配额

**Vendor Model Contract**:
调用方针对某个 Vendor Model 提交参数时所遵循的合同：字段名、类型、枚举、默认值、组合约束与能力边界。它表达**模型语义**，不表达任何 Provider 的 HTTP 包装。它随不可变 Runtime Revision 发布，调用方按选中的模型使用它，不按渠道使用它。归属见 `docs/adr/0015`。
_Avoid_: Provider Schema、渠道请求格式、跨厂商统一图片参数

**Provider**:
向平台实际提供模型调用和账单的服务方，例如 AIHubMix。**Vendor 与 Provider 是角色而非身份类别**：同一主体可以同时是某个模型产品的 Vendor、又是它的 Provider（厂商直连自营时，如火山方舟之于 ByteDance 的 Seedream）。Provider 可以只供应一家 Vendor 的模型（直连型），也可以供应多家（聚合型）——这是供应范围的自然结果，不是两种不同的层。
_Avoid_: Vendor

**Offering**:
Provider 通过特定 Adapter 和 Channel 提供某个 Vendor Model Revision 的可调用供给。它是**工程师配好的资产**，不随每次发布重写；一个 Gateway Model 由一组有序的 Offering 供应，运营选的是这些 Offering。
_Avoid_: 模型、渠道、把 Offering 说成"渠道模型"

**供应商模型名**（Provider Model）:
平台向**某个 Provider 的某个 Channel** 请求时用的那个模型标识（库里是 `supply.offerings.provider_model_id`）。它由 Provider 的命名习惯决定，可能等于厂商原生名，也可能是别名——它与 Vendor 原生名、与 Gateway Model 名都是分开的角色。
_Avoid_: 渠道模型、把供应商模型名当成厂商原生名、把它当成对外的 `model`

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
**内部的**执行与审计记录：一次图片生成请求。执行事实与账务可持久恢复，**业务载荷（请求正文、图片与结果信封）不持久化、不可恢复**。它承载路由判定、计量证据、对账与结算，**对客不可见**——平台对消费者只有同步调用，不提供任务号轮询，也不把这条记录投射成对客协议。
_Avoid_: 对客任务、任务号、Provider Task、HTTP 请求

**Generation Attempt**:
Generation Job 对某个 Offering 和 Channel 发起的一次外部副作用尝试。它同时承载对账标识（Provider 的逐请求标识），该标识只用于对账，不参与计价，也不属于 Metering Evidence。
_Avoid_: 重试、Job

**Input Reference**（输入参考资源）:
请求里作为**输入**交给模型的外部资源。它是**与资源种类无关**的分类：今天接入的种类**只有图片**（见 [Reference Image / Mask](#reference-imagemask参考图与遮罩)），视频、音频以及图片与它们的混合输入属于同一类。它的**数量上限**是这条渠道/这条供给能接收的参考资源个数上限，随 Runtime Revision 按供给声明；今天只有图片这一种，上限的名字是 `max_reference_images`（发布数据里是 `restrictions.max_reference_images`）——**名字点明它管的是图片**。它与 `n`（输出张数）是两件事：`n` 说这次要生成几张，`max_reference_images` 说这次能收几张参考资源，两者互不顶替。
_Avoid_: 把输入参考资源与输出张数混为一谈、用 `max_images` 这种看不出是输入还是输出的名字、把"今天只有图片"说成"这类限制只可能有图片这一种"

**Reference Image / Mask**（参考图与遮罩）:
调用方给出的图片参数值：公网 URL 或 data URL，今天**唯一**一种 [Input Reference](#input-reference输入参考资源)。它**只是参数值**——平台不落盘、不校验其内容、不给它独立身份，由渠道决定接受什么形态、拒绝什么形态。平台认的调用方图片契约字段只有 `image` / `image_urls` / `mask` 三个名字（`image` 与 `image_urls` 同义）；候选**声明**的参数名另有一套判定：名字以 `image` 开头的是参考图、含 `mask` 的是遮罩（**两者都像时以遮罩为准**），它只用于"把调用方的图落到该候选的哪个参数上"，不用于拦截调用方字段。合同过滤与承载校验见 [Vendor Model Contract 与 Offering Parameter Mapping 落地设计](docs/design/0005-vendor-model-contract-and-offering-mapping.md) §4。
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
一次执行留下的**成本平面**事实。它由两个**分开的量**说清：**成本怎么算**（[Cost Basis](#cost-basis成本口径)，两态）与**这一笔的金额实际从哪来**（[Provider Cost Source](#provider-cost-source成本来源)，三态），加上金额与**该渠道声明的币种**（不假定 USD）。两个量不能互相顶替：只问"怎么算"会把"本该有金额却拿不到"和"根本不用算"混成一件事，只问"从哪来"则说不出自算时的口径。它与 [Pricing Formula](#pricing-formula计价形态) 也分层：计价形态说这个渠道按什么单位算钱（渠道事实，随发布冻结），成本口径说这条 Offering 的成本由谁定，成本来源说这一笔实际发生了什么。它与 `Metering Evidence` 并列但**不是计量证据**：金额不替代分项 token，也**不参与对客金额**（对客只有一个币种 CNY），只进 [Gross Margin](#gross-margin毛利) 口径。只有请求根本没交到渠道的执行才没有成本事实，那时的空值是"根本没采"，不是"成本是 0"；`unavailable` 那笔**不进对账态**（对客结算照常完成），缺口由运营核上游账单后补录。成本事实的采集、折算与缺口处置见 [定价、保底与结算](docs/design/0007-pricing-floor-and-settlement.md) §7。
_Avoid_: 把渠道报的金额当成计量证据或对客售价、用"金额对不对"代替来源判定、拿不到金额时用自算或 0 顶替、把成本缺口推进对账态、把成本口径与成本来源混成一个量

**Cost Basis**（成本口径）:
这条 Offering 的**成本由谁定**，随发布冻结：`computed` = 渠道不给金额字段，平台按该条 Offering 的计价形态与实际用量自算；`declared` = 渠道终态直接给金额，直接取它、不自己算。
_Avoid_: 把它与成本来源混成一个量、把计价形态当成成本口径

**Provider Cost Source**（成本来源）:
**某一笔执行**的金额实际从哪来，三态：`computed`（自算得到）、`declared`（渠道直接给了）、`unavailable`（本该有金额却拿不到，或按登记的形态算不出来——**不得猜测**）。它是执行事实，不随发布冻结。
_Avoid_: 拿它当发布物、缺金额时用自算或 0 顶替

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
一条 Offering 的**渠道事实**：这个渠道的这个模型**按什么计价**——`token_rates`（按四分项 token 计量量）、`per_image`（按产出张数）、`per_call`（按调用次数）、`upstream_declared`（上游终态直接给实扣金额）。它由渠道决定、平台如实登记（出处是渠道文档或实测），**不是运营的选项**：管理员最多登记或更正这条事实。它**决定成本怎么算**（平台与渠道怎么结算），**不决定对客卖多少钱**（对客由 [Consumer Pricing Form](#consumer-pricing-form对客计价形态) 选：按 token 四档读 [Consumer Rate Vector](#consumer-rate-vector对客费率向量)，或按上游声明金额 × 倍率）；**上游给了金额就先取它**（见 [Provider Cost](#provider-cost渠道成本事实) 的 `declared`），只有拿不到金额时才按登记的形态自算。它随 Runtime Revision 发布、随 Job 的 [Price Snapshot](#price-snapshot) 冻结；每条 Offering 另声明它的**成本币种**。形态与参数的配套、发布期校验见 [定价、保底与结算](docs/design/0007-pricing-floor-and-settlement.md) §1。
_Avoid_: Cost Formula、把成本计价形态当运营的定价选项、把"按张 / 按次"当成对客计费单位、用成本计价形态决定对客售价

**Consumer Pricing Form**（对客计价形态）:
平台对客户**按什么方式收钱**，由**运营按候选选择**，与 Offering 的 [Pricing Formula](#pricing-formula计价形态)（成本计价形态）相互独立：`token_rates`（四档 token，读 [Consumer Rate Vector](#consumer-rate-vector对客费率向量)）、`upstream_declared`（上游声明金额 × [Markup](#markup加价系数)）。对客 token 价由运营维护（初始值取该 vendor/模型已知的渠道价目，经 Markup 与 [FX Rate](#fx-rate折算率) 折 CNY），**不依赖该候选的成本单价**，所以成本由上游直接给金额的渠道也能按 token 四档卖；前提是该渠道的响应能提供对应证据（四档 `usage` / 声明 `cost`）。它随 Runtime Revision 按候选发布、随 Job 的 [Price Snapshot](#price-snapshot) 冻结。见 [定价、保底与结算](docs/design/0007-pricing-floor-and-settlement.md) §1–§2。
_Avoid_: 把成本计价形态当对客形态、在渠道不提供证据时仍选该对客形态、把对客形态写进 Offering（它按候选发布）

**Consumer Rate Vector**（对客费率向量）:
某个 Offering 的**对客四档 CNY token 费率**（文本输入 / 图像输入 / 文本输出 / 图像输出，每 1M tokens），**随 Runtime Revision 按候选发布**、**受理时随 Price Snapshot 冻结**。它是**对客选 `token_rates` 的候选的对客价**：实收由这份向量与本次实际用量算出，结算只读冻结的那一份；对客选 `upstream_declared` 的按上游声明金额 × 冻结倍率 × 冻结折算率。**可被路由的供给必须能给出对客价**（或旧口径那份 Price Plan 费率）——给不出就发布期拒，**不按 0 收**。初始值取该 vendor/模型已知的渠道价目（经 [Markup](#markup加价系数) 与 [FX Rate](#fx-rate折算率) 折 CNY），运营可改、也可直接录入。**同一个 Gateway Model 的不同候选价格不同**，因为对客形态与价目按候选发布。它不是渠道成本费率：两者是两个量，混用会让成本跟着售价漂移。推导口径与旧口径见 [定价、保底与结算](docs/design/0007-pricing-floor-and-settlement.md) §2–§3。
_Avoid_: 把渠道成本费率当对客售价、用一个单值推出四档向量、按网关模型或全平台一个价、把它当成每种对客计价形态都要有的载体

**Markup**（加价系数）:
**每个 Gateway Model 一个**的倍率（倍率 = 1 + `markup_bps` / 10000），随 Runtime Revision 发布、随 Job 的 Price Snapshot 冻结。它是推导对客 token 价目**初始值**的乘数；对客选 `upstream_declared` 时，结算按**冻结的那份倍率**乘上游声明金额。数值由后台录入，不是设计决策。
_Avoid_: 全局加价、改价不用发布、在代码里写一个默认倍率

**FX Rate**（折算率）:
**按币种维护**的"渠道币种 → CNY"折算率，带**生效时间**，由管理员在后台维护；**同币种也按币种录一行**（例如 CNY → CNY），那一行的率恒为 1、折出来逐位不变，所以同币种渠道的供给发布得出去、且不产生折算。受理时按该候选的成本币种取受理时刻生效的那一份并随 Price Snapshot 冻结，受理之后不再换算；同一时刻同一币种全平台只有一个数（否则对账对不起来）。它**不进不可变修订**：放进每份发布里，改一次汇率就要重发所有型号。**没有折算率的币种在发布期被拒**。它只用于把成本折算成 CNY 做毛利核算，**不参与对客金额**。取值规则与录入入口见 [定价、保底与结算](docs/design/0007-pricing-floor-and-settlement.md) §2/§8。
_Avoid_: 把汇率固定在每份发布里、受理后再换算、用浮点算钱、拿对不上币种的汇率去折、把某个币种写死成"不用折算"

**Settled Balance**（已结算余额）:
消费者账户已确认的资金当前值。充值与正式调整增加或减少它，实际扣费减少它；预授权的建立与释放不改变它。它是管理员面与内部受理的量，**不是客户页面上那个数**（客户侧见 **Customer Balance**）。见[账户资金 Spec](docs/specs/0002-account-funds-and-reservations.md) §1、§4。
_Avoid_: 把客户页面上那个数说成已结算余额、把它称为可用额、用预授权改写该值

**Account Name**（账户名称）:
账户的资料，用于识别这个账户服务的对象（个人、工作室、公司或项目）；**每个账户始终有一个，且在全部账户里唯一**（区分大小写：`Star` 与 `star` 是两个名称）：创建时由调用方给出，或由服务端按规则生成（有登录邮箱时取 `<邮箱本地部分>_<账户 id 前 4 位>`，撞名就把 id 片段加长；否则取 `账户_<账户 id 前 8 位>`）。它可被运营与该账户的客户本人改成另一个未被占用的名称，不参与认证、授权、路由或金额计算；账户 id 仍是唯一定位标识。规则见[账户名称 Spec](docs/specs/0003-account-names-and-login-identities.md) §2。
_Avoid_: 把路由标签当名称用、按名称登录或授权、把名称当成经验证的法律主体、让两个账户用完全相同的名称

**Held Amount**（持有中）:
已受理且未结清的请求占用金额合计，随 Hold 状态变化；它不是实际扣费，不写入资金流水，也不作为客户页面上的一个数出现。`可用额 = 已结算余额 − 持有中`。见 **Customer Balance**。
_Avoid_: 把占用当作扣款或退款、在客户页面单列持有中或预授权金额

**Customer Balance**（余额）:
客户控制台上**唯一**的余额数字，取值是客户**现在能用的钱**（`可用额 = 已结算余额 − 持有中`）。请求受理时按占住的额度减少，结算后按实际扣费多退少补；客户页面不分别展示已结算余额、持有中与可用额。它允许为负（实收超过已结算余额时由结算造成），页面照实显示、不截断成 0。见[账户资金 Spec](docs/specs/0002-account-funds-and-reservations.md) §4 与[控制台 Spec](docs/specs/0001-admin-and-customer-consoles.md) C7。
_Avoid_: 在客户侧再起第二个余额标题、按内部三分解分列金额、把这个数说成"已结算余额"

**Floor Amount**（保底额）:
受理时按**供给（vendor + offering）维度**从该供给的保底表查得的预授权额，**CNY**；保底表随 Runtime Revision 发布、随 Price Snapshot 冻结、**不编进代码**。供给内按 `(size, quality)` 两维给**每张**额，受理时 `hold = n × 每张额`（`n` = 请求张数，缺省 1）；`quality` 留空即按 `size` 档；**像素型的 `size` 先归到档位**，`size = auto`（或没给 `size`）取默认档。它的**两个身份**：① **准入闸门**——受理时 `可用额 ≥ 保底额` 才放行，不足即 402 `insufficient_balance`；② **结算参考**——**不是上限**：实收按实际算，估小了可使已结算余额透支，估大了只释放多余占用。它**不由售价派生**。归位规则与回落链见 [定价与保底](docs/design/0007-pricing-floor-and-settlement.md) §6，账户写法见[账户资金设计](docs/design/0013-account-funds-and-reservations.md) §2。
_Avoid_: 把保底额当售价或结算上限、按网关模型或全平台一个数、写进代码、把像素尺寸一律当成"归不出档位"（**按张数乘每张保底额是正确口径**）

**Gross Margin**（毛利）:
`售价（CNY）− 成本折算后 CNY`，按 Job 可查。**两条线分开留痕**：售价 / 保底 / 扣费记 CNY（账本是权威，Price Snapshot 是冻结的那一份），成本记**原币种原值 + 币种 + 当时汇率 + 折算后 CNY**。成本来源可辨（`computed` / `declared` / `unavailable`）；来源是 `unavailable` 时标**"成本未知"**——金额与折算值留空，**不猜**（不写 0、不用费率顶替）。**上游声明的实际金额只影响毛利，不改对客金额**。落点见 [定价、保底与结算](docs/design/0007-pricing-floor-and-settlement.md) §5 与 [代码结构图](docs/architecture.md) §5。
_Avoid_: 用售价顶替成本、拿不到成本就写 0、把毛利算成"售价 − 参考成本"（参考成本只是定价参考）

**Price Snapshot**:
Job 受理时固定的计价单位、单价和公式版本；受理之后不再换算。**对客平面**（一律 CNY）：命中候选的 [Consumer Pricing Form](#consumer-pricing-form对客计价形态) 与它的 [Consumer Rate Vector](#consumer-rate-vector对客费率向量)、档位价目表（只作参考与展示）、该供给的保底表与算定的 [Floor Amount](#floor-amount保底额)、命中的候选。**成本平面**（按该供给声明的成本币种）：[计价形态](#pricing-formula计价形态)与它的参数、参考成本与成本币种、成本来源口径，以及受理时取的那一份 [FX Rate](#fx-rate折算率)。**对客实收 = 冻结的对客价目 × 本次实际量**（对客选 token 四档时读冻结的向量）；对客选 `upstream_declared` 时 = 上游声明金额 × 冻结的 [Markup](#markup加价系数) × 冻结的 [FX Rate](#fx-rate折算率)；**算不出对客价 = 这条供给没有对客计费基准**——发布期就拒，运行时真遇到（只有历史修订才可能）按**平台侧故障**处置，**不按 0 结算**。字段与各量的落点见 [定价、保底与结算](docs/design/0007-pricing-floor-and-settlement.md) §3。
_Avoid_: 当前价格、受理后再换算汇率、按发布物现价结算已受理的 Job、把"没有对客计费基准"当成 0 元计价

**Reconciliation Case**:
Provider 是否受理、是否生成或是否计费无法自动确认时，需要独立处置的业务事实。
_Avoid_: 普通失败、自动重试

**Platform Funding Failure**:
平台在某个 Provider 侧的账户余额、额度或权限不足，导致该渠道拒绝**平台的**调用。它不是消费者的问题，属**运营事件**；对客表现**不得**是"余额不足"，且必须能被运营侧发现与处置。
_Avoid_: 用户欠费、`insufficient_balance`（那是消费者余额的语义）、把渠道的信封原样当成对客语义

**Consumer Insufficient Balance**:
消费者账户扣除处理中请求的占用后，可用额不足以覆盖本次保底额。它在**受理之前**就被拒绝；客户页面显示的余额取自同一口径，但那是读取那一刻的值，受理仍以数据库当前值为准。
_Avoid_: 与 Platform Funding Failure 混用；把平台在渠道侧的额度问题说成"用户余额不足"

**Consumer-Facing Error Code**（对客错误码）:
消费者在 Job 上唯一看得到的一类错误标识，只有三个取值：`platform_unavailable`（平台侧故障）、`outcome_unknown`（受理状态不明，已进对账）、`content_rejected`（消费者内容被渠道拒绝）。渠道的 HTTP 状态码、错误码与原文只留内部。内部另记失败类别：欠费、凭证/权限、平台自身、渠道拒绝了平台的请求、渠道不可用、渠道限流、消费者内容、不确定——其中**渠道不可用、渠道限流、消费者内容被拒不是平台侧事件**（可观测，但不是平台要去修的），其余四类与"不确定"算平台侧事件。语义与理由见 `docs/adr/0017`。
_Avoid_: 把渠道的状态码或错误码当对客码、用 `insufficient_user_quota`/`payment_required` 这类渠道标识符对客、把平台欠费说成消费者余额不足

**Gateway Model**（对外的模型字段 `model`）:
平台**自己的那个模型**：运营给它起名、选它指向哪个 Vendor Model Revision、配它由哪些 Offering 供应、定它的价，并启用或停用它。它是对客面唯一存在的模型身份——调用方提交的 `model` 就是它的名字，也是被受理、被定价、被启停的那个对象。它与 **Vendor Model**（厂商的模型产品）以及真正发给渠道的 **供应商模型名** 是三个分开的角色：同一次调用里，调用方只认 `model`，落到哪个厂商模型、换成什么供应商模型名，由平台内部决定。
_Avoid_: 平台模型、网关模型、把 `model` 当成厂商原生模型名、把这三个角色合并成一个字段

**Acceleration Cache**（加速层）:
路由候选集与账户金额的缓存，**只做加速、不是事实源**：金额判定与选路结果的正确性不依赖它，账户当前值只在 PostgreSQL 事务里改变。缓存写入一律发生在**数据库提交之后**、写的是**提交后的快照**（不是增量命令），与数据库不一致时以数据库为准。这一层没配置时不构造，行为与没有它时相同；配了但连不上、超时或命令报错，一律当"这次没命中"，回源数据库。余额缓存见[账户资金设计](docs/design/0013-account-funds-and-reservations.md) §3。
_Avoid_: 事实源、余额权威、用缓存里的数扣费、增量写入、把缓存不可用当成服务不可用

**Balance Cache Entry**（余额缓存条目）:
余额缓存条目是账户已结算余额、持有中、可用额及数据库单调版本的快照。提交后写回只接受不低于缓存当前版本的值；缓存不足不能独自返回 402，须由数据库确认。
_Avoid_: 拿缓存值直接拒绝客户、让较旧的异步写入覆盖新版本

**Route Cache**（候选集缓存）:
该网关模型当前生效候选集的缓存值，外加**写它那次发布的修订标识**。受理时与当前生效修订比对，不一致（或值里没有这个标识）就当未命中、回源数据库——因此陈旧是**可检的**，不依赖发布后的失效一定成功。网关模型的启用开关不进这个判定。
_Avoid_: 靠"发布后失效成功"保证正确性、把启用开关交给缓存判、把缓存里的候选集当成发布物

**Cache Reconciliation**（缓存对账）:
以数据库为准校正加速层的任务：把账户当前快照按版本写回、把不是当前生效修订的候选集拿掉，发现不一致时覆盖并写审计。它**不是** [Reconciliation Case](#reconciliation-case)：后者是上游受理状态不明、要人工处置的业务事实。
_Avoid_: 把缓存对账当成业务对账、用缓存对账替代账实核对

**Ledger Audit**（账实核对）:
按账户核查**实际收支流水之和**与**已结算余额**、有效 Hold 之和与**持有中**是否一致的独立后台检查；不在请求路径运行，也不默认每 15 分钟全库重算。不一致时建账户级核查案例并告警，**只发现、不改账**。它**不是** [Cache Reconciliation](#cache-reconciliation缓存对账)：后者比的是数据库与缓存两份副本。
_Avoid_: 把账实核对当成缓存对账、让核对任务自动改账、每轮不符都刷告警

**Platform Account**（平台账户）:
账本里承载**平台自担成本**的那个账户（`ledger.accounts.kind = 'platform'`，全库只有一行，由迁移种下）。上游已经扣了钱、而这次执行没让消费者付费（终态失败、或人工解除预授权）时，那笔**折算后 CNY** 记成 `cost` 科目的分录挂在这里（金额为负），**不进任何消费者的余额**；消费者实际扣费只由 `capture` 记录，未收费的占用解除只改变 Hold。它没有 API Key、没有余额缓存、不参与预授权；余额为负是它的常态（那个负数就是累计自担成本）。科目取值与写入方见 [代码结构图](docs/architecture.md) §5；上游成本的**事实**仍记在 `generation.attempts` 的四列上（成功那一次只进毛利口径，不落账本）。
_Avoid_: 把成本记进消费者账户、把平台账户当成运营开得出来的账户或对客凭据、拿 `adjustment` 顶替成本科目、把平台账户的余额说成"欠款"


**Route Policy**（路由策略）:
在一批合格候选（[Offering](#offering)）里"挑哪一条"的**运行期配置**：全局一条、可按 [Gateway Model](#gateway-model) 覆盖，未配置时默认 `priority_failover`。四种策略：`priority_failover`（按档位顺序、档内按权重）、`weighted_random`（不看档位、在全部合格候选里按权重）、`least_cost`（按**折后成本估算**最小）、`user_tag`（按 [Account Tag](#account-tag账户标签) 经映射指定）。它**不进不可变修订**（不是"卖什么"，而是"在已发布的合格候选里怎么选"），改它即刻影响之后的受理、已受理的 [Generation Job](#generation-job) 不受影响。取值空间**只有合格候选**：承载面表达不了这次请求的候选先被排除，策略与标签映射都指定不了它们；`least_cost` 与 `user_tag` 给不出答案时退回默认顺序，不判失败。
_Avoid_: 把策略当成发布内容、用策略或标签映射绕过承载校验、把"档位顺序"说成策略、把折扣率当成本口径、把退回默认顺序说成"策略生效了"

**Account Tag**（账户标签）:
账户上的一个由运营设的字符串，**只有生效的 `user_tag` 策略消费它**：策略里的映射把它对应到某条候选。没有那条策略时，改标签不改变任何选路结果；映射指向的候选这次承载不了时也不选它。
_Avoid_: 把标签当权限或计费档、用标签绕过承载校验、以为设了标签就会改变选路（要有生效的策略）
