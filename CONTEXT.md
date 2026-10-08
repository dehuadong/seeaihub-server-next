# SeeAI Hub 新服务端

该上下文描述模型供给、图片生成、请求执行与计费之间的业务语言。

## Language

### 供给与模型身份

**Vendor**:
定义模型产品与原生能力的厂商。Vendor 与 Provider 是角色而非身份类别：同一主体可以兼两者。
_Avoid_: Provider、渠道

**Vendor Model**:
由 Vendor 发布、以原生模型 ID 和修订标识确定的模型产品。
_Avoid_: 平台模型、渠道模型

**模型类型**（Model Type）:
Vendor Model 的产品种类，取值为 `image`、`video`、`chat`。它是模型自身的事实，不随渠道、候选或定价变化。
_Avoid_: 计费类型、计价形态、按类型的路由或配额

**Vendor Model Contract**:
调用方针对某个 Vendor Model 提交参数时遵循的合同：字段名、类型、枚举、默认值、组合约束与能力边界。它表达模型语义，不表达任何 Provider 的 HTTP 包装。
_Avoid_: Provider Schema、渠道请求格式、跨厂商统一图片参数

**Provider**:
向平台实际提供模型调用和账单的服务方。它可以是直连型（只供应一家 Vendor 的模型），也可以是聚合型（供应多家）。
_Avoid_: Vendor

**Offering**:
Provider 通过特定 Adapter 和 Channel 提供某个 Vendor Model Revision 的可调用供给。它是工程师配好的资产，不随每次发布重写；一个 Gateway Model 由一组有序的 Offering 供应。
_Avoid_: 模型、渠道、渠道模型

**供应商模型名**（Provider Model）:
平台向某个 Provider 的某个 Channel 请求时使用的模型标识。它由 Provider 的命名习惯决定，与 Vendor 原生名、Gateway Model 名是三个分开的角色。
_Avoid_: 渠道模型、厂商原生名、对外的 `model`

**Offering Parameter Mapping**:
Vendor Model Contract 的参数与某个 Offering 实际要求的渠道包装之间的对应关系，粒度是 Vendor Model × Offering。它是平台内部的事，既不是调用方合同，也不是 Adapter 的职责。
_Avoid_: 全局 Provider 参数表、跨厂商统一转换、Adapter 的字段翻译

**Channel**:
Provider 的一个调用入口及凭证身份。
_Avoid_: Provider、Offering、上传存储

**Gateway Model**（对外的模型字段 `model`）:
平台自己的模型身份：运营给它起名、指定它指向的 Vendor Model Revision、配它由哪些 Offering 供应、定它的价，启用或停用它，并设它的并发名额。它是对客面唯一存在的模型身份，与 Vendor Model、供应商模型名是三个分开的角色。
_Avoid_: 平台模型、网关模型、把 `model` 当成厂商原生模型名

**Model Concurrency Quota**（模型并发名额）:
每个账户在一个 Gateway Model 上**同时在跑**的生成任务上限。它挂在 Gateway Model 上、由运营设置，不随 Runtime Revision 冻结——改了即刻影响之后受理的请求；模型没设时用部署缺省。按「账户 × 该模型」计数，不同模型互不占名额；转对账的 Job 不占名额。
_Avoid_: 把它当发布内容、当账户总量上限、当请求速率（速率与并发是两个量）

**Runtime Revision**:
一次经过校验并发布的不可变运行时目录，固定模型、供给、渠道限制与价格关系。请求与 Job 固定受理时的版本。
_Avoid_: 配置文件、当前缓存

### 上传与输入

**Upload Storage**（上传存储）:
平台写入调用方上传素材的外部对象存储位置。它不是 Channel，不进 Runtime Revision、不参与选路、不计费。
_Avoid_: 上传渠道、资产库、把上传存储当成 Provider 或 Offering

**Object Key**（对象键）:
上传素材在对象存储里的键。它不是调用方文件名，也不是公网 URL。
_Avoid_: 文件名、路径、URL

**Input Reference**（输入参考资源）:
请求里作为输入交给模型的外部资源，与资源种类无关；今天接入的种类只有图片，视频、音频以及图片与它们的混合输入属于同一类。它与输出张数 `n` 是两个量：前者说这次能收几个参考资源，后者说这次要生成几张。
_Avoid_: 把输入参考资源与输出张数混为一谈、`max_images` 这类看不出是输入还是输出的名字

**Reference Image / Mask**（参考图与遮罩）:
调用方给出的图片参数值，形式是公网 URL，今天唯一一种 Input Reference。它只是参数值：平台不落盘、不校验其内容、不给它独立身份，接受什么形态由渠道决定。
_Avoid_: Asset、资产、素材库、把渠道参数名当作模型参数名

**Result Envelope**（结果信封）:
上游交付结果时给的 `url` 或 `b64_json`，原样进入对客响应的 `data.result.images[]`。平台不下载、不解码、不归档，链接的有效期与长期保存由调用方负责；渠道为结果地址给出过期时刻时随该项如实带出，平台不推算。
_Avoid_: 平台结果资产、归档、本地副本

### 执行与故障

**Generation Job**:
一次图片生成请求的内部执行与审计记录。它承载路由判定、计量证据、对账与结算；对客只暴露它的标识作为这次调用的 `id`，其余字段不投射成对客协议。
_Avoid_: 对客任务、任务号、Provider Task、HTTP 请求

**Generation Attempt**:
Generation Job 对某个 Offering 和 Channel 发起的一次外部副作用尝试。它同时承载对账标识（Provider 的逐请求标识），该标识只用于对账，不属于 Metering Evidence。
_Avoid_: 重试、Job

**Reconciliation Case**:
Provider 是否受理、是否生成或是否计费无法自动确认时，需要独立处置的业务事实。
_Avoid_: 普通失败、自动重试

**Platform Funding Failure**:
平台在某个 Provider 侧的账户余额、额度或权限不足，导致该渠道拒绝平台的调用。它是运营事件，对客表现不得是「余额不足」。
_Avoid_: 用户欠费、`insufficient_balance`、把渠道的信封原样当成对客语义

**Consumer Insufficient Balance**:
消费者账户扣除处理中请求的占用后，可用额不足以覆盖本次保底额。它在受理之前就被拒绝。
_Avoid_: 与 Platform Funding Failure 混用

**Consumer-Facing Error Code**（对客错误码）:
消费者在 Job 上唯一看得到的错误标识，只有三个取值：`platform_unavailable`（平台侧故障）、`outcome_unknown`（受理状态不明，已进对账）、`content_rejected`（消费者内容被渠道拒绝）。渠道的 HTTP 状态码、错误码与原文只留内部。
_Avoid_: 把渠道的状态码或错误码当对客码、用渠道标识符对客

### 计价与成本

**Pricing Formula**（计价形态）:
一条 Offering 的渠道事实：这个渠道的这个模型按什么计价——`token_rates`（按四分项 token 计量量）、`per_image`（按产出张数）、`per_call`（按调用次数）或 `upstream_declared`（上游终态直接给金额）。它由渠道决定、平台如实登记，不是运营的选项；它决定成本怎么算，不决定对客卖多少钱。
_Avoid_: Cost Formula、把计价形态当运营的定价选项、用成本计价形态决定对客售价

**Price Plan**:
一个 Offering 的渠道成本价目：按 `token_rates` 计价时的那份四档 token 单价（文本输入 / 图像输入 / 文本输出 / 图像输出，每 1M tokens）、成本侧的渠道币种与价目出处。渠道按张、按次计价或由上游直接给金额时，这条 Offering 没有 Price Plan。
_Avoid_: 当前价格、把它的费率当对客售价、把它当成每种计价形态都要有的载体

**Consumer Pricing Form**（对客计价形态）:
平台对客户按什么方式收钱，由运营按候选选择，与 Offering 的 Pricing Formula 相互独立：`token_rates`（四档 token）或 `upstream_declared`（上游声明金额 × Markup）。
_Avoid_: 把成本计价形态当对客形态、把对客形态写进 Offering

**Consumer Rate Vector**（对客费率向量）:
某个 Offering 的对客四档 CNY token 费率（文本输入 / 图像输入 / 文本输出 / 图像输出，每 1M tokens）。它不是渠道成本费率，两者是两个量；同一个 Gateway Model 的不同候选价格可以不同。
_Avoid_: 把渠道成本费率当对客售价、用一个单值推出四档向量、按网关模型或全平台一个价

**Markup**（加价系数）:
每个 Gateway Model 一个的倍率。它是推导对客 token 价目初始值的乘数；对客选 `upstream_declared` 时，结算按冻结的那份倍率乘上游声明金额。
_Avoid_: 全局加价、改价不用发布、在代码里写一个默认倍率

**FX Rate**（折算率）:
按币种维护的「渠道币种 → CNY」折算率，每行带生效时间，同一币种也各有一行（率恒为 1）。它只用于把成本折成 CNY 做毛利核算，不参与对客金额，也不进不可变修订。
_Avoid_: 把汇率固定在每份发布里、受理之后再换算、用浮点算钱、把某个币种写死成「不用折算」

**Floor Amount**（保底额）:
受理时按供给（vendor + offering）维度查得的预授权额，CNY。它是准入闸门，也是结算参考，但不是上限。
_Avoid_: 把保底额当售价或结算上限、按网关模型或全平台一个数、写进代码

**Price Snapshot**:
Job 受理时固定的计价单位、单价与公式版本，受理之后不再换算。它分对客平面（一律 CNY）与成本平面（按该供给声明的成本币种）。
_Avoid_: 当前价格、受理之后再换算汇率、按发布物现价结算已受理的 Job

**Metering Evidence**:
Provider 成功响应或账单中可核验的计量事实。它不包含平台价格计算结果，也不包含对账标识。
_Avoid_: 费用、估算值

**Token Usage**:
计量证据中「分项 token 计数」这一形态：文本输入、图像输入、文本输出、图像输出四项计数。它不是独立于计量证据之外的第二个概念。
_Avoid_: Metered Usage、费用

**Provider Cost**（渠道成本事实）:
一次执行留下的成本平面事实，由两个分开的量说清：成本由谁定（Cost Basis）与这一笔的金额实际从哪来（Provider Cost Source），加上金额与该渠道声明的币种。它不参与对客金额，只进毛利口径。
_Avoid_: 把渠道报的金额当计量证据或对客售价、用「金额对不对」代替来源判定、拿不到金额时用自算或 0 顶替、把成本口径与成本来源混成一个量

**Cost Basis**（成本口径）:
这条 Offering 的成本由谁定，随发布冻结：`computed`（平台按该条 Offering 的计价形态与实际用量自算）或 `declared`（渠道终态直接给金额）。它与成本来源是两个量。
_Avoid_: 把它与成本来源混成一个量、把计价形态当成成本口径

**Provider Cost Source**（成本来源）:
某一笔执行的金额实际从哪来：`computed`（自算得到）、`declared`（渠道直接给了）或 `unavailable`（本该有金额却拿不到，不得猜测）。它是执行事实，不随发布冻结。
_Avoid_: 拿它当发布物、缺金额时用自算或 0 顶替

**Gross Margin**（毛利）:
售价（CNY）减成本折算后 CNY，按 Job 可查。成本来源是 `unavailable` 时标「成本未知」、金额留空，不猜。
_Avoid_: 用售价顶替成本、拿不到成本就写 0、把参考成本当实际成本

### 路由

**Routing Priority**:
同一 Vendor Model 的候选 Offering 之间的档位，数字小者优先。它是发布决定，不由请求参数、Adapter 或价格推导；多条候选可以落在同一档。
_Avoid_: 价格优先、负载均衡、把档位顺序说成「可配置的策略」

**Routing Weight**（档内权重）:
某个候选 Offering 在同一档位内的分流比，是发布者给出的正整数。它不改变档位顺序，分摊是确定性的：同一请求重放必然落同一条候选。
_Avoid_: 加权轮询 / 负载均衡、按权重改变档位顺序、把权重当健康度或价格择优、用随机数发生器分流

**Route Policy**（路由策略）:
在一批合格候选里挑哪一条的运行期配置，全局一条、可按 Gateway Model 覆盖，取值是 `priority_failover`（按档位顺序、档内按权重）、`weighted_random`（在全部合格候选里按权重）、`least_cost`（按折后成本估算最小）或 `user_tag`（按 Account Tag 的映射指定）。它不进不可变修订，改它即刻影响之后受理的请求。
_Avoid_: 把策略当成发布内容、用策略或标签映射绕过承载校验、把档位顺序说成策略

**Account Tag**（账户标签）:
账户上一个由运营设定的字符串，只有生效的 `user_tag` 策略消费它。它不是权限，也不是计费档。
_Avoid_: 把标签当权限或计费档、用标签绕过承载校验、以为设了标签就会改变选路

### 账户与资金

**Settled Balance**（已结算余额）:
消费者账户已确认的资金当前值。充值与正式调整改变它，实际扣费减少它，预授权的建立与释放不改变它。
_Avoid_: 把客户页面上那个数说成已结算余额、把它称为可用额、用预授权改写该值

**Held Amount**（持有中）:
已受理且未结清的请求占用金额合计。它不是实际扣费，不写入资金流水，也不作为客户页面上的一个数出现。
_Avoid_: 把占用当作扣款或退款、在客户页面单列持有中或预授权金额

**Customer Balance**（余额）:
客户控制台上唯一的余额数字，取值是可用额（已结算余额 − 持有中）。它允许为负，页面照实显示、不截断成 0。
_Avoid_: 在客户侧再起第二个余额标题、按内部三分解分列金额、把这个数说成「已结算余额」

**Consumer Point**（积分）:
对客金额单位：1元 = 1000积分，恒为整数。客户看到的余额、扣费、单笔实收与对客账单金额都以它计；平台内部账本仍以 CNY 微单位记账，1积分 = 1000 微单位。
_Avoid_: 把积分当内部记账单位、在对客金额上用小数、把它与充值科目 `credit` 混为一谈

**Account Name**（账户名称）:
账户的资料，用于识别这个账户服务的对象。每个账户始终有一个，且在全部账户里唯一（区分大小写）。
_Avoid_: 把路由标签当名称用、按名称登录或授权、把名称当成经验证的法律主体

**Platform Account**（平台账户）:
账本里承载平台自担成本的那个账户，全库只有一行。它不是对客凭据，没有余额缓存、不参与预授权，余额为负是它的常态。
_Avoid_: 把成本记进消费者账户、把平台账户当成运营开得出来的账户、拿调整分录顶替成本

### 缓存与核对

**Acceleration Cache**（加速层）:
路由候选集与账户金额的缓存，只做加速、不是事实源。缓存写入一律发生在数据库提交之后，与数据库不一致时以数据库为准。
_Avoid_: 事实源、余额权威、用缓存里的数扣费、增量写入、把缓存不可用当成服务不可用

**Balance Cache Entry**（余额缓存条目）:
账户已结算余额、持有中、可用额及数据库单调版本的快照。缓存不足不能独自拒绝客户，须由数据库确认。
_Avoid_: 拿缓存值直接拒绝客户、让较旧的异步写入覆盖新版本

**Route Cache**（候选集缓存）:
该网关模型当前生效候选集的缓存值，外加写它那次发布的修订标识。陈旧因而是可检的，不依赖发布后的失效一定成功。
_Avoid_: 靠「发布后失效成功」保证正确性、把启用开关交给缓存判、把缓存里的候选集当成发布物

**Cache Reconciliation**（缓存对账）:
以数据库为准校正加速层的任务：把账户当前快照按版本写回、把不是当前生效修订的候选集拿掉。它不是 Reconciliation Case。
_Avoid_: 把缓存对账当成业务对账、用缓存对账替代账实核对

**Ledger Audit**（账实核对）:
按账户核查实际收支流水之和与已结算余额、有效占用之和与持有中是否一致的独立后台检查，不在请求路径运行。它只发现、不改账，也不是缓存对账。
_Avoid_: 把账实核对当成缓存对账、让核对任务自动改账、每轮不符都刷告警
