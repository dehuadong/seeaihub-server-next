---
title: 管理员与客户身份邮箱互斥
status: implemented
created: 2026-10-05
updated: 2026-10-05
approval: 用户确认口径并授权执行实现（2026-10-05）
verification: HTTP_CONTRACT_DATABASE_URL=... cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1（223 passed）；cases_identity_email（7 passed）与 cases_migrations（10 passed）连跑；cargo fmt --check；cargo clippy -p seeai-persistence -p seeai-api -p seeai-application --all-targets --all-features -- -D warnings
---

# Agent Note：管理员与客户身份邮箱互斥

## 问题

客户注册可以占用平台管理员的邮箱：`identity.customers` 与 `identity.admin_users` 各自只在**表内**唯一，两张表之间没有交叉校验。运营按邮箱搜客户会搜出与管理员同名的假客户，签发客户重置码、绑账户、充值等操作会落错对象。

## 决定

历史行为依据见[历史控制台 Spec](../../../../docs/specs/0001-admin-and-customer-consoles.md) A6、C2、C13 与 §6 不变式。本记录保存落地方式与后果。

跨两张表的唯一性不能靠单个唯一索引。会写入身份的四条路径（自助注册、运营开户、管理员引导、按邮箱 upsert 管理员口令）在**同一事务**里：

1. 按规范化邮箱取一把事务级咨询锁（`pg_advisory_xact_lock(hashtextextended('identity-email:' || lower($1), 0))`，与发布替换、幂等建任务同一套写法）：只有同一邮箱的并发写串行，不同邮箱并行。
2. 在锁内交叉检查另一张身份表；已被占用就回冲突（客户侧回与普通重复**同一句话**，不透露身份域），或让引导失败并点名（管理员侧）。
3. 两张表的 `lower(email)` 唯一索引仍是域内唯一的兜底。

咨询锁只关并发，不改变读路径：登录、会话、生成都不查它。锁键带 `identity-email:` 前缀，避免与发布、幂等的锁撞键。

迁移 0040 做一次校验：两张表存在同一邮箱时拒绝应用并点名，由运营先解决——登录身份不静默改动。

## 备选方案

- 共享邮箱登记表 + 唯一索引：数据库硬约束，但要新表、回填与全部创建路径写入，还要处理回填冲突；本改动用锁 + 交叉检查达到同样的并发正确性，成本小得多。
- 只查不做锁：并发下“注册 + 引导”可能两边都成功，跨表的唯一索引也挡不住，不满足不变式。
- 迁移自动改掉冲突客户的邮箱：会静默改登录身份，客户将无法用原邮箱登录；不采用。
- 只在注册一侧拦管理员邮箱：反向（客户先占、管理员后引导）仍会留下重复，不是不变式。

## 后果

咨询锁只在事务内有效，绕过仓储直接写库仍可造出重复；平台的身份写入只走仓储这几条路径，这是既有边界。迁移在存在跨域重复时会让进程起不来，需要运营先处理——这是刻意的：登录身份不能静默改。

## 验证

[API 合同用例](../../../../apps/api/tests/http_contract/cases_identity_email.rs)覆盖：管理员邮箱注册与开户被拒、回与普通重复**同一句话**的冲突、且不产生写入（直接数账户、客户身份与审计行）；引导用客户邮箱时启动失败并点名；注册与引导两个方向的并发各自只成功一边（另一条连接持锁把并发固定下来，断言另一方等在锁上）；按邮箱 upsert 管理员口令同样被拒。[迁移用例](../../../../apps/api/tests/http_contract/cases_migrations.rs)覆盖：既有跨域重复时 0040 拒绝应用、点名迁移号与冲突邮箱、且两行 fixture 与账户原样保留。
