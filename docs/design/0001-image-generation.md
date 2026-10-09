> 归属状态（2026-10-09）：本文件仅保留退役、合并或初期交付的历史身份，不能作为新工作的默认实施依据；合并条目的有效内容按登记查找属主。 全部映射与未决影响见[归属切换登记](../agents/document-ownership-transition.md)。以下原文与状态头保留其历史身份，不扩大本文权威。

主题: 独立图片生成服务端
当前修订: v2
状态: 已评审通过（2026-09-19，seeaihub-server-next#1 合同收口）

# 独立图片生成服务端

本仓库后续提案与进度权威为 [seeaihub-server-next#1](https://github.com/dehuadong/seeaihub-server-next/issues/1)。[seeaihub#674](https://github.com/dehuadong/seeaihub/issues/674) 与其中的技术设计 v1–v5 只作为冻结的历史产品与架构来源；本文登记新仓库中的实现映射，不复制提案正文。

- 本仓库的技术设计副本（v5 为准）见 [0002-image-generation-tech-design.md](./0002-image-generation-tech-design.md)。
- 从该设计抽取的持久决策见 `docs/adr/`（编号 0001–0008）。

## 实现映射

- API 与 Worker 共用领域和应用模块，独立运行；
- PostgreSQL 是目录、发布、Job、计费和审计事实权威；
- Provider 调用、结果格式和同步/异步差异封装在 Adapter；
- Native Capability Schema、Offering、Channel 和 Price Plan 通过 Runtime Revision 发布；
- 每个原生模型独立选择活动 Runtime Entry；发布新模型不会替换其他模型；
- 平台 `native_model_id` 与供应商调用所需 `provider_model_id` 分开固化，Adapter 只使用后者组装供应商请求；
- AIHubMix Channel 默认 Base URL 为 `https://api.inferera.com`，属于运行时配置；
- 首期正式计费路径使用 OpenAI 兼容 generations/edits 响应中的 token usage；
- Provider POST 结果不确定时创建 Reconciliation Case，不自动重提。
- Reconciliation Case 保留预授权，当前只允许管理员以幂等业务键退款并释放全部预授权；没有可核验 Metering Evidence 时不能人工确认扣款。每次处置写入不可变账本和审计。
- Metering Evidence 是包含 Attempt ID、Provider 响应摘要和强类型 token usage 的不可变对象。
