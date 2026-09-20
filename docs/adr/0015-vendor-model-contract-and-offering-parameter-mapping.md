---
status: accepted
---

# 对外参数合同属于 Vendor Model，渠道差异由 Offering Parameter Mapping 吸收

调用方按所选模型的 **Vendor Model Contract** 提交参数：它表达模型语义（字段名、类型、枚举、默认值、组合约束、能力边界），**不**表达任何 Provider 的 HTTP 包装。同一 Vendor Model 在不同 Provider 之间的差异（字段改名、参数位置、枚举拼写、单位与表达形式、默认值、字段拆分合并、能力子集）由 **Offering Parameter Mapping** 在平台内部吸收，粒度是 **Vendor Model × Offering**。Provider Adapter 只处理渠道级传输能力（鉴权、HTTP、上传、同步/异步、轮询、错误与结果归一），不定义调用方合同；它向发布校验声明的是**传输装载能力**，不是参数名清单。

声明的形状必须绑定该 Offering **实际调用的端点（含分支差异）**——同一 Provider 另一个端点族的包装不是本 Offering 的合同。参数兼容性只产生"能否安全执行"的事实与保护，Offering 的选择规则来自运营侧发布的策略（见 [0009](./0009-multiple-active-offerings-and-routing.md)），核心服务不内置价格、优先级或健康度择优。本阶段**不**建设跨厂商统一图片参数协议：`reference_images`、`control_image`、`style_reference` 等语义统一属后期独立规划，"按各模型自己的合同提交"不等于"所有厂商共用一套字段"；参考图/遮罩参数的名字约定只是本阶段的兼容规则，不是长期协议。

**被取代的旧决定**：原先"平台内部只认渠道自己的参数名、差异留给对外消费侧"已由本决定取代——它把渠道包装当成了调用方合同，实测后果是**参数写在哪个位置就决定了选中哪个渠道**（同一模型的两个候选中，一个要求 `extra.quality`、另一个要求顶层 `quality`，两份 Schema 都是 `additionalProperties: false`），`routing_priority` 因此不起决定作用。落地所需的数据模型改动与待答问题见工作项 [`#6`](https://github.com/dehuadong/seeaihub-server-next/issues/6)。
