> 归属状态（2026-10-09）：本文件仅保留退役、合并或初期交付的历史身份，不能作为新工作的默认实施依据；合并条目的有效内容按登记查找属主。 全部映射与未决影响见[归属切换登记](../agents/document-ownership-transition.md)。以下原文与状态头保留其历史身份，不扩大本文权威。

---
status: deprecated
---

# 【已退役】AIHubMix 的正式计费路径走 OpenAI 兼容的 `/v1`

本条**不是决定**，已于 2026-09-20 按 ADR 准入门槛**退役**（降级为一句话存根，保留文件名与编号以便旧链接可解析）：它记的是"调用 AIHubMix 的哪一条上游端点"，那属于**渠道自己的实现面、由 Adapter 承载**，不是平台级决策。它所依赖的平台规则——正式计费路径必须能拿到可核验的计量事实——由 [0006](./0006-no-settlement-without-metering-evidence.md) 拥有；该渠道端点的实际形态属渠道事实，见 [`docs/facts/channel-facts.md`](../facts/channel-facts.md) 的 AIHubMix 节。历史全文见 git 历史。