# 不做跨厂商参数大一统，原生能力由不可变 Revision 发布

平台为每个 Vendor Model Revision 保存一份 Native Image Capability Schema，保留厂商原生字段名与条件规则（`image` / `images` / `mask` 等），**不**建立 Canonical 参数大一统，也不构成跨厂商参数翻译层。若某厂商不使用 `image` 这个字段名，由该厂商自己的 Schema 声明原生字段路径与判定条件。

Schema 不实时依赖上游文档地址，更新流程是「抓取候选 → 差异检查 → 审核 → 发布新 Runtime Revision」；模型 Schema、Offering、Channel 与 Price Plan 都经不可变 Revision 发布，已受理的请求与 Job 固定受理时的版本，旧 Job 继续使用旧修订。运行时请求不随远端文档变化，也不因目录或价格调整而重编译数据面。

平台对未知字段失败关闭。原本把这道防线归给上游的写法已不成立：2026-09-19 实测火山方舟会**静默接受未声明的顶层字段并降级为默认值**（`bogus_field`、`quality`、`sequential_image_generation` 均返回 HTTP 200 并真实出图），AIHubMix 的 `additionalProperties: false` 只是那一个上游的局部性质，不能作为平台通用假设。因此**拒绝未声明字段必须是平台自己受理前的校验**，否则错拼的参数会静默变成默认参数。

实时 Schema 与模型介绍存在冲突时，按实时 Schema 的保守交集发布首版能力——未证实的参数不开启，每个冲突参数经真实 wire 验证后再以新 Schema 修订发布，**不在原修订上静默放宽**。

**事实前提更正（2026-09-19，`dehuadong/seeaihub-server-next#2` Planning）**：上一段此前写「未知字段一律失败关闭（上游 Schema 当前为 `additionalProperties: false`）」，该括注是**对该上游的观察**，已被第二个 Provider 的实测推翻。更正只涉及事实前提，**决策本身不变**（仍不做跨厂商参数大一统，仍由不可变 Revision 发布原生能力）。

**来源**：技术设计 v3 与 v4，v5 明确继续有效；事实前提由第二阶段 Planning 更正。
