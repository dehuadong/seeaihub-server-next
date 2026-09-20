---
status: accepted
---

# 文生图、图生图与 mask 编辑共用同一条 Image Generation 命令

三类请求只是同一业务能力的不同输入分支，因此平台只保留一个 `CreateImageGeneration` 命令和一个持久 Generation Job：不拆两套应用服务，也不新增平台 `operation` 字段，分支由原生图片参数派生。拆开会把 Job 表、状态机与结算规则各复制一份；派生分支只用于选 Schema 条件分支、判断 Offering 能否履约、让 Adapter 选择 wire 形态、选计价规则与写审计，它不是客户端字段。

**边界**：若厂商把生成与编辑定义成完全不同的正式模型身份或不兼容合同，就按厂商边界建立不同的 Vendor Model Revision——统一命令不抹平厂商真实的产品差异。
