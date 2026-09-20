# 对外参数合同属于 Vendor Model，渠道差异由 Offering Parameter Mapping 吸收

> **状态：生效。** 依据：用户于 2026-09-20 明确指示"按照复审交接文档执行、之前的决策可以推翻、按最新的复审结果执行"（授权记录见工作项 [`#6`](https://github.com/dehuadong/seeaihub-server-next/issues/6) 的评论）。本文取代 [0002](./0002-native-capability-schema-not-canonical.md) 的补充决定（已双向链接），落地所需的数据模型改动与待答问题登记在 `#6` 的差距 G1、G5–G8。

**决策**：

1. **对外参数合同以 Vendor Model 为单位。** 调用方按所选模型的 **Vendor Model Contract** 提交参数；该合同表达模型语义（字段名、类型、枚举、默认值、组合约束与能力边界），**不表达任何 Provider 的 HTTP 包装**。
2. **同一 Vendor Model 在不同 Provider 之间的差异，由 Offering Parameter Mapping 在平台内部吸收。** 职责范围至少包括（**这是职责举例，不是要实现的函数清单**；实现形态留给后续 Planning）：字段改名、参数位置（顶层与嵌套）、枚举拼写与大小写、单位与表达形式、默认值补充、一个字段拆成多个、多个字段合成一个、能力子集声明，以及 Offering 不承载某参数时的**显式拒绝**。
3. **映射的粒度是「Vendor Model × Offering」。** 它既不是全局 Vendor 关系，也不是全局 Provider 关系——同一 Provider 供应不同 Vendor Model 时可以有完全不同的映射。
4. **Provider Adapter 只处理渠道级传输能力**：鉴权、HTTP、上传、同步/异步、轮询、错误分类与结果归一。Adapter 不定义调用方所见的参数合同。**推论（2026-09-20 收口补充）**：Adapter 向发布校验声明的是**传输装载能力**（能否承载顶层对象、嵌套对象、表单字段、数组位置等），**不是调用方可见的参数名清单**；用一份写死的参数名清单去把关"调用方合同能声明哪些字段"，就等于让 Adapter 定义调用方合同，与本条冲突。该改造登记为工作项 [`#6`](https://github.com/dehuadong/seeaihub-server-next/issues/6) 的 G1。
5. **参数兼容性只产生事实与执行保护。** 判断"某 Offering 能否完整承载本次参数"是执行安全约束，不是选择策略；Offering 的选择规则来自运营侧发布的策略（见 [0009](./0009-multiple-active-offerings-and-routing.md)），核心服务不内置价格、优先级或健康度的择优规则。
6. **声明的形状必须绑定实际发往的端点集合（含分支差异）。** 导入或声明任何"原生 Schema"时，字段与位置必须取自**该 Offering 实际调用的那一个（或那几个）端点**，以及各分支实际可用的参数面。同一 Provider 的另一个端点族的形状（例如聚合渠道自有异步 API 的包装）**不是**本 Offering 的调用方合同。注意同一个 Offering 的"文生图"与"编辑"分支本身参数面就可能不同（例如编辑分支才有的遮罩、值域更窄的尺寸），单份、与分支无关的 schema 不足以表达。
7. **本阶段不建设跨厂商统一图片参数协议。** `reference_images`、`control_image`、`style_reference` 等跨模型统一语义属于后期独立规划。第 1 条要求的是"按各 Vendor Model 自己的合同提交"，**不是**"所有厂商共用一套字段"。
8. **"哪个参数装参考图/遮罩"目前的名字约定只是阶段性兼容规则**，不是长期通用协议；它应随 Vendor Model Contract 的显式声明一并落位，不能扩写成平台公共字段。

## 取代关系

本 ADR **取代** [0002](./0002-native-capability-schema-not-canonical.md) 的「补充决定（2026-09-19）：平台内部只认渠道自己的参数名，统一参数转换留给后期的对外消费侧」，及其派生的两条收窄：

- 「输入侧资产绑定也用渠道原生参数路径，平台不在内部把渠道参数名映射成统一名」——改为：调用方按 **Vendor Model 的**参数名提交，渠道路径由 Offering Parameter Mapping 承担；
- 「名字约定是有意的过渡方案，`reference_images` 这类名字由对外消费侧解决」——改为：由 Vendor Model Contract 显式声明，名字约定只是本阶段的兼容规则。

[0002](./0002-native-capability-schema-not-canonical.md) 中「不做跨厂商参数大一统、原生能力由不可变 Revision 发布、未证实参数不开启」的部分**继续有效**，不被本 ADR 取代。

## 为什么不沿用旧决定

旧决定把"渠道包装"直接当成调用方合同，其后果已在仓库中实测出现（工作项 [#6](https://github.com/dehuadong/seeaihub-server-next/issues/6) 有完整记录）：

- **合同与路由互相耦合**：`POST /v1/image-generations` 收 `native_parameters` 与 `asset_bindings[].native_parameter_path`；候选是否合格由**该候选自己的** Schema 决定。于是调用方把某个参数写在哪，直接决定哪个候选合格，`routing_priority` 不再起决定作用。
- **同一语义参数出现两种调用方写法**：同一 Vendor Model 的两个候选，一个要求 `extra.quality`、另一个要求顶层 `quality`；两份 Schema 都是 `additionalProperties: false`。
- **声明形状取自未调用的端点族**：AIHubMix 素材把 `quality` 声明在 `extra` 内，那是该渠道**自有异步 API** `/ai/v1/*` 的位置；实际调用的是 `/v1/images/*`，该端点族 `quality` 在顶层且不存在 `extra`。Adapter 因此在出网前把 `extra.quality` 摊平回顶层，形成"发布校验认一套形状、上行的却是另一套"的翻译层。

以上三条都不是"少写了一个转换函数"，而是**合同归属错位**：调用方合同被写成了某个 Provider 的包装。

## 依据

- 分层依据：`docs/design/0004-layered-architecture.md` 的 ①「Model Protocol ── 对外一致（消费侧契约）」与 §4 第 5 步「接入新 Provider 时 ① 无需改动（对外契约不变）」已经要求对外契约与 Provider 无关；本 ADR 把该要求落到"合同按 Vendor Model 声明、差异由 Offering Mapping 吸收"。
- 事实依据：`docs/facts/channel-facts.md` §2.5（AIHubMix 三个端点族的顶层参数与 `quality` 位置对照）、§3（APIMart 的端点与参数面）。
- 实现现状与差距：工作项 [#6](https://github.com/dehuadong/seeaihub-server-next/issues/6) 的"已核对的仓库事实"与"差距"两节。

## 落地前必须回答的（本 ADR 不代答，登记在工作项 #6）

本 ADR 只定**归属**，不定实现。以下问题必须在落地前的 Planning 里回答，逐条登记在工作项 [`#6`](https://github.com/dehuadong/seeaihub-server-next/issues/6)：

- **合同没有唯一载体（结构性差距 G5）**：`catalog.vendor_models` 的唯一键含 `schema_hash`（即合同内容一变就产生新行），且 `capability_schema` 现在**同时**充当"调用方请求的校验面"与"候选自身的支持面"。要落第 1、2 条，必须先拆开"合同"与"能力子集"两件事；相关验收断言（"每个候选各自携带自己的 Profile"）也要一并复核。这**不是改素材就能完成的**，改的是数据模型。
- **迁移与命名**：`AssetBinding.native_parameter_path` 与对外字段名 `native_parameters` 在"渠道原生"的语义下命名，而第 1 条要求调用方按模型合同提交。历史 Job 的 `asset_bindings`/`native_parameters` 是 jsonb，没有语义版本标记；是否改名、如何解释历史数据，必须显式回答，不能留给实现者临时决定。
- **兼容性事实的消费者与失败语义**：第 5 条说兼容性不是选择策略。落到执行上还要回答：兼容性事实进不进选路输入？被选中的 Offering 装不下本次参数时是拒绝、改选、还是交策略决定？
- **合同参数面与各 Offering 支持面不同时的规则**：合同是并集还是交集？若是并集，调用方提交了某个候选不支持的值时，是否又变成"参数决定渠道"？
- **"哪个参数装参考图/遮罩"的归属**：第 8 条把它降级为阶段性规则，但显式声明的归属（Vendor Model Contract 的参数级语义、Offering 声明，或维持约定）尚未选定。

## 本阶段不决定的事

- Vendor Model Contract 的**具体字段与枚举**不在本 ADR 内确定；它作为发布数据随不可变 Runtime Revision 发布，其内容必须以已确认的模型资料和产品决定为准，不得据本 ADR 的举例直接创建字段。
- Offering Parameter Mapping 的实现形态（配置化声明的最小能力，还是 Adapter/Offering 承担）留给后续 Planning；本 ADR 只确定归属与粒度。
- 是否、以及何时恢复"跨厂商统一简化接口"属后期产品决定。

**来源**：2026-09-19 复审交接文档给出的架构方向；用户于 2026-09-20 明确授权"之前的决策可以推翻，按该复审结果执行"。工作项 [`#6`](https://github.com/dehuadong/seeaihub-server-next/issues/6)。
