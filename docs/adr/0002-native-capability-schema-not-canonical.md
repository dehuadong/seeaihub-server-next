# 不做跨厂商参数大一统，原生能力由不可变 Revision 发布

平台为每个 Vendor Model Revision 保存一份 Native Image Capability Schema，保留厂商原生字段名与条件规则（`image` / `images` / `mask` 等），**不**建立 Canonical 参数大一统，也不构成跨厂商参数翻译层。若某厂商不使用 `image` 这个字段名，由该厂商自己的 Schema 声明原生字段路径与判定条件。

Schema 不实时依赖上游文档地址，更新流程是「抓取候选 → 差异检查 → 审核 → 发布新 Runtime Revision」；模型 Schema、Offering、Channel 与 Price Plan 都经不可变 Revision 发布，已受理的请求与 Job 固定受理时的版本，旧 Job 继续使用旧修订。运行时请求不随远端文档变化，也不因目录或价格调整而重编译数据面。

平台对未知字段失败关闭。原本把这道防线归给上游的写法已不成立：2026-09-19 实测火山方舟会**静默接受未声明的顶层字段并降级为默认值**（`bogus_field`、`quality`、`sequential_image_generation` 均返回 HTTP 200 并真实出图），AIHubMix 的 `additionalProperties: false` 只是那一个上游的局部性质，不能作为平台通用假设。因此**拒绝未声明字段必须是平台自己受理前的校验**，否则错拼的参数会静默变成默认参数。

实时 Schema 与模型介绍存在冲突时，按实时 Schema 的保守交集发布首版能力——未证实的参数不开启，每个冲突参数经真实 wire 验证后再以新 Schema 修订发布，**不在原修订上静默放宽**。

**事实前提更正（2026-09-19，`dehuadong/seeaihub-server-next#2` Planning）**：上一段此前写「未知字段一律失败关闭（上游 Schema 当前为 `additionalProperties: false`）」，该括注是**对该上游的观察**，已被第二个 Provider 的实测推翻。更正只涉及事实前提，**决策本身不变**（仍不做跨厂商参数大一统，仍由不可变 Revision 发布原生能力）。

> **⚠️ 状态：本节（补充决定）已被 [0015](./0015-vendor-model-contract-and-offering-parameter-mapping.md) 取代（2026-09-20）。**
>
> 取代范围：仅"平台内部只认渠道自己的参数名、不在内部做映射、把差异留给对外消费侧"这一条及其派生的两条收窄。**本文其余部分继续有效**——仍不做跨厂商参数大一统、原生能力仍由不可变 Revision 发布、未证实参数仍不开启。
>
> 取代原因与实测依据见 [0015](./0015-vendor-model-contract-and-offering-parameter-mapping.md)；工作项 [`#6`](https://github.com/dehuadong/seeaihub-server-next/issues/6)。下面保留原文，仅作历史理由，**不再是生效规则**。

**补充决定（2026-09-19，用户决定，见工作项 [#4](https://github.com/dehuadong/seeaihub-server-next/issues/4)）：平台内部只认渠道自己的参数名，统一参数转换留给后期的对外消费侧。**（已被取代，见上方状态块）

- **输入侧（资产绑定）也用渠道原生参数路径**：`AssetBinding.native_parameter_path` 就是该渠道的原生参数路径（AIHubMix 的 `/image`、`/mask`；APIMart 的 `/image_urls/0`、`/mask_url`）。平台**不**在内部把渠道参数名映射成一套统一名。
- **统一参数转换不属于本平台内部**：它属**后期对外消费侧**的能力（调用方看到统一参数、由那一层转成各渠道的原生参数）。现在做会牵动每个渠道的适配与验证，先按渠道各自动作，把每条渠道跑通。
- **已知偏差（如实记录）**：本文第 1 段要求"由该厂商自己的 Schema 声明原生字段路径与判定条件"。平台目前判定"某个参数装的是参考图还是遮罩"用的是**名字约定**（名字以 `image` 开头＝参考图、含 `mask`＝遮罩，其余一律拒绝；实现在 `crates/domain` 的 `AssetParameterKind::classify`，发布期校验与运行期共用）。这是**有意的收窄**，不是"由 Schema 显式声明"：如果用别的字段名（例如 `reference_images`），平台会拒绝该绑定，**不再靠改代码加特例**——那种情况按 [0015](./0015-vendor-model-contract-and-offering-parameter-mapping.md) 第 7 条由 Vendor Model Contract 显式声明解决（**原写"由上面那层统一转换来解决"已随本节的取代而失效**）。名字约定因此是**过渡方案**；它现在的归属是 [0015](./0015-vendor-model-contract-and-offering-parameter-mapping.md) 第 8 条（阶段性兼容规则），将来若把"哪个参数装图片"变成发布物里的显式声明，需要新开决策。

**来源**：技术设计 v3 与 v4，v5 明确继续有效；事实前提由第二阶段 Planning 更正；输入侧与统一转换层的边界由用户于 2026-09-19 决定。**该补充决定已于 2026-09-20 被 [0015](./0015-vendor-model-contract-and-offering-parameter-mapping.md) 取代**（见上方状态块）。
