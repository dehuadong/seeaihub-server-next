---
status: accepted
---

# 渠道成本按各自渠道的口径取数：APIMart 取上游声明的 `cost`，AIHubMix 按已发布费率自算

平台的**计量事实**统一是分项 token，但**成本价**的取数按渠道各自的口径：APIMart 直接给出 `cost`——那就是它的实际成本（含账号倍率，不可复现），平台不再按 token × 公开费率反算；AIHubMix 不给出任何金额字段，成本只能按分项 token × 已发布费率自算。渠道各自响应里有什么、没有什么，属渠道事实，见 `docs/facts/channel-facts.md`。

`cost` 只用于核成本，**不替代计量事实**（[0010](./0010-metering-evidence-is-unit-bearing.md)）。

**后果**：换渠道就换成本口径，逐笔账目必须按渠道分别取数，不能跨渠道套同一套算法；平台对外价与账号折扣是否让利属后期产品决定，不在本条。
