# SeeAI Hub 新服务端

该上下文描述模型供给、图片生成、任务执行和计费之间的业务语言。

## Language

**Vendor**:
定义模型产品与原生能力的厂商，例如 OpenAI。
_Avoid_: Provider、渠道

**Vendor Model**:
由 Vendor 发布、以原生模型 ID 和修订标识确定的模型产品。
_Avoid_: 平台模型、渠道模型

**Provider**:
向平台实际提供模型调用和账单的服务方，例如 AIHubMix。
_Avoid_: Vendor

**Offering**:
Provider 通过特定 Adapter 和 Channel 提供某个 Vendor Model Revision 的可调用供给。
_Avoid_: 模型、渠道

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
Generation Job 对某个 Offering 和 Channel 发起的一次外部副作用尝试。
_Avoid_: 重试、Job

**Asset**:
经平台授权和校验、由对象存储承载的输入或输出媒体引用。
_Avoid_: 外部 URL、Base64 字符串

**Metering Evidence**:
Provider 成功响应或账单中可核验的计量事实，不包含平台价格计算结果。
_Avoid_: 费用、估算值

**Price Snapshot**:
Job 受理时固定的计价单位、单价和公式版本。
_Avoid_: 当前价格

**Reconciliation Case**:
Provider 是否受理、是否生成或是否计费无法自动确认时，需要独立处置的业务事实。
_Avoid_: 普通失败、自动重试
