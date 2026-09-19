---
title: 第二阶段交付：多 Offering 路由与 APIMart Driver
status: implemented
created: 2026-09-19
updated: 2026-09-19
approval: seeaihub-server-next#2 记录的用户执行授权（用户明确输入「执行实现」）；范围收口与回扩见该工作项上的规划 §7.5
verification: 2026-09-19 fmt、clippy（warnings 作为错误）、workspace 单元测试、六个空库端到端合同测试与 decisions check 全部通过
---

# 第二阶段交付：多 Offering 路由与 APIMart Driver

## 实际交付

[第二工作项](https://github.com/dehuadong/seeaihub-server-next/issues/2) 的第二阶段实现已完成并验证：同一 Vendor Model 现在可以由**多个 Provider 同时供应**，平台按已发布的优先级选中第一个合格候选；新增 APIMart 的任务式 Driver。

交付内容：

- **多 Offering 路由**：`publication.runtime_entries` 增加 `routing_priority`，唯一索引由「每型号一个 active 条目」改为「每型号每个优先级一个」；`active_offering` 返回**候选集合**（每个候选自带它自己的 `capability_schema`）；新增 `generation.routing_decisions` 记录受理时的判定。
- **发布接口形状**：`PublishRuntimeCommand` 支持 `offerings` 数组与 `price_plan`（三例确定性形状判别）；数据库端口只接受已核验的 `PublishRuntimeRequest`。
- **APIMart Driver**：任务式（提交 → 轮询 → 取图 → 证据提取 → 错误分类）；错误分类只依据 `error.code`，未知状态继续轮询，查询阶段错误一律进对账。
- **渠道事实**：AIHubMix 2.5 两款与 APIMart 2.5 两款的发布素材；`docs/facts/channel-facts.md` 为渠道事实的单一出处。

技术设计与边界由 [工作项 #2 的规划正文](https://github.com/dehuadong/seeaihub-server-next/issues/2) 与 [分层架构](../../../../docs/design/0004-layered-architecture.md) 拥有，持久决定由 [ADR 目录](../../../../docs/adr/) 拥有（新增 0009–0014）；本记录不复制其正文。

## 验证结果

规划验收条件 25 条：**22 条通过**，第 16–18 条标为**过期条件**（要求的能力在代码中不存在，且其前提已被实测结清）。

经真实空库 + 真实进程验证的关键行为：

| 行为 | 证据 |
| --- | --- |
| 同型号多供给、按优先级选第一个 | `multiple_active_offerings_route_by_priority` |
| APIMart 驱动整条流程，创建请求只发一次 | `apimart_driver_executes_task_flow_against_local_upstream`（进程内假上游 + 真实 Worker） |
| 查询瞬时失败会重试 | `transient_query_failure_is_retried_and_the_job_still_succeeds` |
| 未文档化的状态继续轮询 | `unknown_task_status_keeps_polling_instead_of_failing` |
| 两份 2.5 素材可发布、候选各自携带 Profile | `stage_two_bootstrap_material_publishes_with_per_candidate_profiles` |
| 两类对账的错误码可分 | `worker_sends_delivery_failure_to_reconciliation_with_its_own_code` |

## 交付过程中发现并修正的实质问题

1. **「限制只能收窄」的校验此前不存在**（最重要）。规划与 `ADR-0009` 都要求发布期校验「Offering 的 `restrictions` 不超出该候选 Profile 自己声明的范围」，而原实现只检查了 Adapter 的能力面。已补 `validate_restrictions_within_profile`，并加正反例测试。
2. **`attempts.provider_trace_id` 在成功路径从不写入**：任务式上游的 `task_id` 被直接丢弃，人工对账失去线索。已打通「Adapter → `ProviderSuccess` → `CompleteJob` → UPDATE」。
3. **未发布型号的返回码回归**：无 active 候选时曾返回 `Validation`（400），应为 `NotFound`（404）。
4. **把做不到的能力声明成支持的**：APIMart 声明支持参考图/遮罩，但上游要求公网可访问 URL，而本仓库没有上传链路 —— 已收窄为仅文生图，运行时显式拒绝。
5. **`PricePlanDraft.formula` 从不校验**：未知计价形态曾静默落库。

前四项均由实现评审（Standards / Spec 双轴）发现，第五项为自行核对发现。

## 已知限制（不在本次交付范围）

- APIMart 的参考图/遮罩路径需先实现 `POST /v1/uploads/images` 才能支持；
- `task_id` 不用于跨调用恢复（需新增列与拆分端口，属独立工作项）；
- 火山方舟/Seedream、直连 OpenAI、多图与 `stream`/`tools` 不在本阶段；
- 第 16–18 条验收条件已标为过期，其若要恢复需先结清非 USD 计价或金额型证据。
