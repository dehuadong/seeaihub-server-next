---
title: 独立图片生成服务端第一阶段交付
status: implemented
created: 2026-09-19
updated: 2026-09-19
approval: seeaihub-server-next#1 记录的用户执行授权；历史设计批准来自 seeaihub#674 技术设计 v5
verification: 2026-09-19 fmt、clippy、workspace tests、空库 PostgreSQL HTTP contract 与 decisions check 全部通过
---

# 独立图片生成服务端第一阶段交付

## 实际交付

[新仓库承接工作项](https://github.com/dehuadong/seeaihub-server-next/issues/1) 已完成第一阶段合同收口。本仓库可以独立构建、测试和运行，不依赖旧服务端代码或数据；API 与 Worker 共用领域和应用模块，图片生成通过持久 Generation Job 执行。

本次交付覆盖动态 Runtime Revision、Vendor Model / Offering / Channel / Price Plan 发布、AIHubMix Adapter 的 generations/edits 分流、输入输出 Asset、强类型 Metering Evidence、Price Snapshot 结算、租约恢复、Reconciliation Case 和审计。具体技术设计由 [图片生成 RFC](../../../../docs/design/0002-image-generation-tech-design.md) 拥有，持久决定由 [ADR 目录](../../../../docs/adr/) 拥有；本记录不复制其正文。

## 合同收口结果

- 新仓库工作项成为未来提案与进度权威；[旧 #674](https://github.com/dehuadong/seeaihub/issues/674) 只保留为冻结的历史产品与架构来源；
- 当前正式 `/v1` 路径没有可查询 task id，提交、响应解析或归档结果不确定时进入 Reconciliation Case，不自动重提；
- Reconciliation Case 当前只允许退款并释放全部预授权；正常扣款只能来自可核验 Metering Evidence 与受理时固化的 Price Snapshot；
- `/ai/v1` 保留为已验证但未发布的能力，待它能提供可关联的计量证据后再另立工作项。

以上边界分别引用 [无 Evidence 不结算](../../../../docs/adr/0006-no-settlement-without-metering-evidence.md)、[不确定提交进入对账](../../../../docs/adr/0007-reconciliation-instead-of-automatic-retry.md) 与 [自有 Asset 才是平台结果](../../../../docs/adr/0008-own-object-storage-is-the-platform-result.md)。

## 验证证据

- `cargo fmt --check`：PASS；
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`：PASS；
- `cargo test --workspace --all-features`：PASS，22 项 Rust 测试通过；
- 空库执行 `cargo test -p seeai-api --test http_contract -- --ignored`：PASS，覆盖动态发布、API 隔离、对账只退款、无 capture、租约在提交前后分别恢复，以及提交后不会再次入队；
- Implementation Review 第 2 轮：Standards PASS、Spec PASS，material findings 为 0；
- 真实 Provider 的历史受控验证见 [AIHubMix 调研第 13 节](../../../../out-reference/aihubmix/gpt-image-2-inferera-research.md#13-受控实测结果2026-09-18)，本次收口没有再次执行付费调用；
- `node scripts/decisions/check.mjs`：PASS，索引与本地链接有效。

## 后续边界

第二个 Provider/Adapter 与多 Offering 路由不属于本记录范围，应在本仓库另建工作项后进入规划。本阶段不承诺客户端合同，也不迁移旧服务端用户、余额、任务或流量。
