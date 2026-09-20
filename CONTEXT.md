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
平台已经受理、可持久恢复的一次图片生成业务请求。
_Avoid_: Provider Task、HTTP 请求

**Generation Attempt**:
Generation Job 对某个 Offering 和 Channel 发起的一次外部副作用尝试。它同时承载对账标识（Provider 的逐请求标识，例如响应头 `x-request-id`），该标识只用于对账，不参与计价，也不属于 Metering Evidence。
_Avoid_: 重试、Job

**Asset**:
经平台授权和校验、由对象存储承载的输入或输出媒体引用。
_Avoid_: 外部 URL、Base64 字符串

**Asset Binding**:
把一张输入 Asset 绑到某个**模型参数**上的记录（`native_parameter_path`、`asset_id`、`position`）。路径的第一段是**该 Vendor Model Contract 声明的参数名**；路径带第二段表示该参数是数组（例如 `/image_urls/0`）。把它变成某个 Offering 实际要求的渠道包装，属 **Offering Parameter Mapping**。平台只在**一处**判定这个参数装的是参考图还是遮罩：名字以 `image` 开头的是参考图、名字含 `mask` 的是遮罩、**其余一律拒绝**（宁可拒绝也不猜；发布期校验与运行期用的是同一个函数）——这是**本阶段的兼容规则**，不是长期通用协议，其归属见 `docs/adr/0015`。
_Avoid_: 统一图片字段、Canonical image 参数、把渠道参数名当作模型参数名、在每个渠道重复一套判定

**Metering Evidence**:
Provider 成功响应或账单中可核验的计量事实，不包含平台价格计算结果。对账标识**不属于**本词条的一部分——它由 Generation Attempt 自己承载，见 `Generation Attempt`。
_Avoid_: 费用、估算值

**Token Usage**:
`Metering Evidence` 中实际启用的计量形态：上游返回的分项 token 计数（文本输入/图像输入/文本输出/图像输出）。领域里由 `TokenUsage` 承载，计量事实本身仍归 `Metering Evidence` 词条。**它不是独立于证据之外的第二个概念**——「上游声明的扣费金额」能否替代或补充它，已由实测结清：两家渠道都返回分项 token，**不需要**金额型证据（`docs/adr/0012` 已作废）。
_Avoid_: Metered Usage（未曾有代码或文档使用该名）、费用

**Routing Priority**:
同一 Vendor Model 的候选 Offering 之间的选择顺序，随 Runtime Revision 发布；数字小者优先。它是**发布决定**，不由请求参数或 Adapter 决定，也不由价格自动推导。**选择规则本身目前不是配置项**——运营方能配的是顺序，差距见 `docs/adr/0009` 与工作项 `#6`。
_Avoid_: 价格优先、负载均衡、把优先级顺序说成"可配置的策略"

**Price Plan**:
某个 Offering 的计价合同：声明**计价形态**（本阶段只有"按 token 计量量"一种；"按上游声明的扣费金额"这一形态已随 `docs/adr/0012` 作废而不再需要）、该形态所需的单价或来源、以及厂商原生币种与发布时固定的汇率。价格变化只通过发布新的 Price Plan 生效。
_Avoid_: 当前价格、费率表

**Price Snapshot**:
Job 受理时固定的计价单位、单价和公式版本。
_Avoid_: 当前价格

**Reconciliation Case**:
Provider 是否受理、是否生成或是否计费无法自动确认时，需要独立处置的业务事实。
_Avoid_: 普通失败、自动重试
