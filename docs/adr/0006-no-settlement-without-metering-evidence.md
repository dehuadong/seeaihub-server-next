# 没有可核验 Metering Evidence 就不做正式资金结算

结算模块只读取强类型 Evidence 和 Job 固化的 Price Snapshot。首期 `AihubmixImageTokenUsage` 至少包含 `input_text_tokens`、`input_image_tokens`、`output_text_tokens`、`output_image_tokens`、`total_tokens`、`provider_response_digest`、`attempt_id`。

硬性约束：公开单价只作为 Price Plan 候选，**不能**单独证明最终结算 Evidence；不得用 `n`、图片尺寸或请求参数反推 token 用量并当作真实账单；未取得可核验计量项前，可以完成目录、任务与结果归档验证，但该 Offering 不能以正式资金结算状态发布。

响应字段与总量必须满足内部一致性校验；缺字段、负数、总量不一致或响应解析失败时**不得猜测费用**，进入对账。

Reconciliation Case 不构成这道门槛的例外：当前人工处置只能退款并释放全部预授权，不能仅凭金额、备注、业务键或运营判断确认扣款。未来若要按上游账单确认扣款，必须另立设计并先定义可核验、可关联到 Attempt 的 Metering Evidence。

**来源**：技术设计 v4 与 v5。v4 曾因该证据缺失给出 Architecture REQUIRED REVISION，v5 的真实付费验证补齐了 token 字段。
