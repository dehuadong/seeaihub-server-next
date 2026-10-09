> 归属状态（2026-10-09）：归属切换未完成；本文件适用且已接受的范围暂保留旧属主，原待评审／待接受内容不因此获批准。依赖它的新工作先核对有效内容及评审缺口，不得默认沿用。 全部映射与未决影响见[归属切换登记](../agents/document-ownership-transition.md)。以下原文与状态头保留其历史身份，不扩大本文权威。

---
status: accepted
---

# PostgreSQL 是唯一业务事实权威

目录、发布、Generation Job、Attempt、结算与审计的事实权威是 PostgreSQL；缓存不是事实来源——余额、候选集与选路结果的正确性不得依赖一个可丢、可陈旧、可被绕过的外部服务。Runtime Revision 是不可变的发布产物，Job 受理时固化 Offering、Adapter、Channel、Published Revision、原生参数摘要与 Price Snapshot，此后发布或改价都不重新解释已受理的 Job。

**反悔成本**：换掉这个权威要把 `crates/persistence` 的仓储实现整体重写，并把账本的余额扣减与结算、Job/Attempt 的状态机转换、审计与对账的取数一并迁走——这些改动的原子性现在都由同一个数据库事务给出，迁走意味着逐个重建；已受理 Job 里固化的版本、供给与 Price Snapshot 也要跟着迁。

**权衡**：两条备选都是这套系统里真实出现过的路径。① **让缓存当权威**（把余额与候选集的判定交给 Redis）：落选——金额判定与选路结果的正确性不能依赖一个可丢、可陈旧、可被绕过的外部服务；这个取舍与它接受的代价（多一个运行时依赖与一条失败路径、受理多一次轻量读）由 [`docs/design/0008`](../design/0008-routing-strategy-and-caching.md) §7 与 [`.agents/notes/implemented/platform/2026-09-22-redis-acceleration-layer.md`](../../.agents/notes/implemented/platform/2026-09-22-redis-acceleration-layer.md) 拥有。② **让平台自己持有的存储当结果权威**（先归档到自有对象存储，再标 Job 成功）：曾经如此（`ADR-0008`，已退役），2026-09-20 由 [ADR-0019](./0019-images-pass-through-without-asset-storage.md) 取代——图片按渠道原形进原形出，代价是平台不保留结果、长期保存由调用方负责，换来的是不必替调用方承担素材权限、尺寸、摘要与存储寿命。选 PostgreSQL 换来的是一处事务同时管住钱、状态与审计；代价是热点读必须另加一层只能当加速的缓存。
