# 文生图、图生图与 mask 共用同一个 Image Generation

文生图、图生图与厂商支持的 mask 编辑，是同一个图片生成业务能力的不同输入分支，因此平台只保留一个 `CreateImageGeneration` Command 和一个持久 Generation Job：不拆 `CreateTextToImage` / `CreateImageToImage` 两套应用服务，也不新增平台 `operation` 字段，分支由原生图片参数派生。

选这个方案是因为三类请求共用同一生命周期、幂等、归档与结算流程；拆成两套服务会让 Job 表、状态机和结算规则各复制一份。派生分支只用于选择 Native Schema 条件分支、判断 Offering 能否履约、让 Adapter 选择上游 wire 形态、选择 Price Plan 规则以及记录审计，它不是客户端字段。

**代价与边界**：若厂商把生成和编辑定义成完全不同的正式模型身份或不兼容合同，就尊重厂商原生边界建立不同的 Vendor Model Revision——「统一 Command」不能抹平厂商真实的产品差异。

**来源**：技术设计 v3，v5 明确继续有效。
