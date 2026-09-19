# PostgreSQL 是唯一业务事实权威

目录、发布、Generation Job、Attempt、结算和审计的事实权威是 PostgreSQL。缓存和对象存储清单**不是**事实来源——对象存储承载 Asset 内容，业务事实仍在库中。

Runtime Revision 是不可变的发布产物，Job 受理时固化 Offering、Adapter、Channel、Published Revision、Native Parameters 摘要、Asset Bindings 与 Price Snapshot；发布或改价后，已受理 Job 不重新解释输入。

**来源**：技术设计 v3 与 v5，仓库根 `AGENTS.md` 同样声明此约束。
