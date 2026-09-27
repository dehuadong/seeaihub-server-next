---
status: accepted
---

# 对外参数合同属于 Vendor Model，渠道差异由 Offering Parameter Mapping 吸收

调用方按所选模型的 **Vendor Model Contract** 提交参数：它表达模型语义（字段名、类型、枚举、默认值、组合约束、能力边界），**不**表达任何 Provider 的 HTTP 包装。同一 Vendor Model 在不同 Provider 之间的差异（字段改名、参数位置、枚举拼写、单位与表达形式、默认值、字段拆分合并、能力子集）由 **Offering Parameter Mapping** 在平台内部吸收，粒度是 **Vendor Model × Offering**。Provider Adapter 只处理渠道级传输能力（鉴权、HTTP、上传、同步/异步、轮询、错误与结果归一），不定义调用方合同；它向发布校验声明的是**传输装载能力**，不是参数名清单。

声明的形状必须绑定该 Offering **实际调用的端点（含分支差异）**——同一 Provider 另一个端点族的包装不是本 Offering 的合同。参数兼容性只产生"能否安全执行"的事实与保护，Offering 的选择规则来自运营侧发布的策略（见 [0009](./0009-multiple-active-offerings-and-routing.md)），核心服务不内置价格、优先级或健康度择优。**跨厂商统一图片参数语义（`reference_images`、`control_image`、`style_reference` 一类）不做**，属后期独立规划（见 [`docs/design/0006`](../design/0006-gateway-models-and-consumer-surface.md) §3）；Adapter 的传输装载面见 [`docs/design/0005`](../design/0005-vendor-model-contract-and-offering-mapping.md) §6。

**被取代的旧决定**：原先"平台内部只认渠道自己的参数名、差异留给对外消费侧"已由本决定取代——它把渠道包装当成了调用方合同，实测后果是**调用方写哪些字段就决定了选中哪个候选**（同一 Vendor Model 的两个候选，参考图字段一个叫 `image`、另一个叫 `image_urls`，两份 Schema 都是 `additionalProperties: false`），`routing_priority` 因此不起决定作用。落地所需的数据模型改动与待答问题见工作项 [`#6`](https://github.com/dehuadong/seeaihub-server-next/issues/6)。

**2026-09-24 修订（数量类取值按"谁声明的更窄"收敛到线上）**：上文"差异由 Offering Parameter Mapping 在平台内部吸收"补上**数量类取值**这一格——同一型号的两个候选为同一个参数声明不同的取值上界时（`n` 合同声明 10、APIMart 的承载面声明 4），**承载面那份上界决定发往上游的取值**：请求给得更多时报"这条候选能收的上限"，候选**仍然合格**，不因此换候选、也不返回平台侧故障。理由与"两份 Schema 谁说了算"是同一条：合同是**调用方**的界面（他能提交什么），承载面是**这条候选**的能力面（它最多能做多少）；`n` 是"最多要几张"，给得比候选能做的多，表达的是"少给几张也成"，不是一次非法请求——最终按实际产出与用量结算，**请求里的张数从不是计费判据**。收敛只有上界这一侧：承载面声明的 `minimum` 不参与判定，低于它的值原样上行，由上游按自己的 schema 处置。

**修订依据**：用户 2026-09-24 裁定"`n` 大于该候选承载面的上限时按上限发出，不返回 503"。
