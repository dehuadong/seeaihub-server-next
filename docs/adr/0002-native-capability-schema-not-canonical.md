# 不做跨厂商参数大一统，原生能力由不可变 Revision 发布

平台为每个 Vendor Model Revision 保存一份 Native Image Capability Schema，保留厂商原生字段名与条件规则（`image` / `images` / `mask` 等），**不**建立 Canonical 参数大一统，也不构成跨厂商参数翻译层。若某厂商不使用 `image` 这个字段名，由该厂商自己的 Schema 声明原生字段路径与判定条件。

Schema 不实时依赖上游文档地址，更新流程是「抓取候选 → 差异检查 → 审核 → 发布新 Runtime Revision」；模型 Schema、Offering、Channel 与 Price Plan 都经不可变 Revision 发布，已受理的请求与 Job 固定受理时的版本，旧 Job 继续使用旧修订。运行时请求不随远端文档变化，也不因目录或价格调整而重编译数据面。

未知字段一律失败关闭（上游 Schema 当前为 `additionalProperties: false`）。实时 Schema 与模型介绍存在冲突时，按实时 Schema 的保守交集发布首版能力——未证实的参数不开启，每个冲突参数经真实 wire 验证后再以新 Schema 修订发布，**不在原修订上静默放宽**。

**来源**：技术设计 v3 与 v4，v5 明确继续有效。
