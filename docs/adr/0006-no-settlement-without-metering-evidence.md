---
status: accepted
---

# 没有可核验 Metering Evidence 就不做正式资金结算

结算只读取强类型 Evidence 与 Job 固化的 Price Snapshot。公开单价只能作为 Price Plan 候选，不能单独证明最终结算依据；不得用 `n`、图片尺寸或请求参数反推用量并当作真实账单；证据缺字段、负数、总量不一致或解析失败时**不得猜测费用**，进对账。未取得可核验计量项前，该 Offering 不能以正式资金结算状态发布。Reconciliation Case **不是**例外：人工处置当前只能退款并释放全部预授权，不能仅凭金额、备注、业务键或运营判断确认扣款。
