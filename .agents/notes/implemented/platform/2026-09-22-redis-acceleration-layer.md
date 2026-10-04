---
title: Redis 加速层：纯加速的路由与余额缓存
status: implemented
created: 2026-09-22
updated: 2026-10-04
approval: 用户在会话中授权实施 P4（Redis 加速层）；范围与验收见提案 [#13](https://github.com/dehuadong/seeaihub-server-next/issues/13) 的 P4 工单 [#19](https://github.com/dehuadong/seeaihub-server-next/issues/19)
verification: 验收合同为工单 [#19](https://github.com/dehuadong/seeaihub-server-next/issues/19) 的逐条可勾选清单（依据 `docs/design/0008` §7）。**全部离线**（真实空库 + 真实 API/Worker 进程 + 进程内假上游与假 Redis，零真实计费调用、零外网）：`apps/api/tests/http_contract.rs` 新增 7 条端到端用例，`cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1` 在**最终代码**上 **65 passed / 0 failed**（180.77s）；`crates/cache-redis` 另有一条对着真实 Redis 的 `#[ignore]` 用例与两条失败路径单测；门禁 `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`、`cargo test --workspace --all-features`、`node scripts/decisions/check.mjs` 全部通过。逐条证据见正文「验证」。
---

# Agent Note：Redis 加速层：纯加速的路由与余额缓存

## 问题

受理一次生成要读两样东西：该网关模型的候选集（连合同、承载面、参数映射与定价）与账户余额。候选集是只读发布数据、余额是每个请求都要判一次的账本事实，两者都在 PostgreSQL 里，而这两条读处在每个请求的关键路径上。

把 Redis 放进来会引入一条新的失败路径与一个新的运行时依赖，所以约束先于做法：**缓存不是事实源**（[`ADR-0003`](../../../../docs/adr/0003-postgresql-is-source-of-truth.md)），扣减与余额事实只在数据库事务里发生；缓存不可用时平台必须照常正确工作，只是变慢；缓存与数据库不一致时以数据库为准。另一条约束来自可解释性：平台不允许"缓存说余额不够"这种**没有业务记录**的拒绝悄悄发生——预检不建 Job、不扣款、不写状态，事后只能靠审计解释"为什么拒了这个客户"。

**部分取代**：余额快照的形状、写回版本闸门与预检口径已由 [账户余额缓存：带版本的快照与数据库最终判定](2026-09-30-account-balance-cache-version-guard.md) 取代。route 缓存的**读路径**（受理前轻量读 + 候选集修订比对）已随 API 直接执行成为唯一路径而删除：受理每次直读数据库，route 条目只剩失效清理与对账审计。本记录的写穿时机、对账与降级仍适用。

## 决定

按 `docs/design/0008-routing-strategy-and-caching.md` §7 落地，范围见工单 [#19](https://github.com/dehuadong/seeaihub-server-next/issues/19)：

- **端口与实现分开**：`crates/application` 有 `CacheStore` 端口（`GET` / `SET … PX` / `DEL`）与 `AccelerationService`（键名、值形状、写穿、失效清理、对账）；`crates/cache-redis` 只把这三条命令发给 Redis，连接惰性建立、单次操作带超时（`CACHE_OPERATION_TIMEOUT_MS`，默认 200 毫秒），任何失败都返回错误，由用例层当"未命中"。缓存语义留在用例层，是为了不让"余额"与"限流计数"的语义在实现方各自漂移。
- **route 条目只剩失效清理**：`route:<gateway_model>` 的历史值是候选集 + 写它那次发布的修订标识，受理期已不再读它——直接执行每次受理都直读数据库取候选（生效发布条目 + 启用的供给与渠道由取数 SQL 一次判完）。发布与启停入口在事务提交后删除该键，对账器按修订标识与网关模型开关删除陈旧条目并写 `cache.route_invalidated` 审计。
- **写穿**：充值、受理占用、结算、失败减占用与对账解除五条改动账户金额的路径，都在事务提交后把**变更后的快照**写进 `user_balance:<account_id>`（不用 `DECRBY`），写入时间取数据库给出的 `updated_at`。为此 `HubRepository` 的这几条写路径用 `RETURNING` 返回 `BalanceChange`（账户、余额、占用、可用额、版本与数据库时刻），幂等重放与"保留预授权的失败收尾"返回当前余额。
- **预检可拒绝但必留审计（已由 [新记录](2026-09-30-account-balance-cache-version-guard.md) 取代）**：缓存条目来源是写穿路径（`db_commit`）**且**写入时间落在新鲜窗口内**且**这次不是重放，才允许在余额低于保底额时提前回 402；拒绝前先写 `operations.audit_events`（缓存余额、写入时间、来源、本次保底额、网关模型），审计写不下去就不拒绝，交给数据库的条件更新。
- **重放不受预检管辖**：重放会去重成原来那个 Job，不新建也不扣款；预检要避免的正是"新建一个 Job 却扣不动钱"。受理前那次轻量读里折了一个按 `(account_id, idempotency_key)` 唯一索引的探测，用来判这次是不是重放，不额外多一次往返。
- **定时对账**：API 进程内的独立任务（`AccelerationService::run_reconciler`，周期 `CACHE_RECONCILE_INTERVAL_MS`）按 `updated_at` 增量把余额写回（来源标记 `reconciler`）、把不是当前生效修订的 route 条目删掉；只覆盖**真的不一致**的条目，覆盖时写一条审计，相等的不动——重写会把来源降级成 `reconciler`，没必要为一次没发生的不一致付这个代价。
- **降级**：`REDIS_URL` 未配置时加速层根本不构造，受理行为与没有这一层时逐位相同；配了但连不上、超时或命令报错，每次操作都当未命中，业务事实一律以数据库为准。

### 配置

`REDIS_URL`（空 = 不启用）、`CACHE_BALANCE_TTL_SECONDS`（360）、`CACHE_RECONCILE_INTERVAL_MS`（180000）、`CACHE_OPERATION_TIMEOUT_MS`（200）。

### 迁移

无。Redis 是外部服务，审计表 `operations.audit_events` 与 `ledger.accounts.updated_at` 都已存在。

## 备选方案

- **缓存语义放进 Redis 实现 vs 留在用例层**：留在用例层。实现方一旦也懂"余额"与"限流计数"，两边的语义会各自漂移，而漂移的表现是"缓存说的和数据库说的不一样"。
- **余额写穿用 `DECRBY` 之类的增量命令 vs 写提交后的值**：写提交后的值。增量表达不了"以数据库为准"，重放还会漂移。
- **写穿前再查一次余额 vs 用 `RETURNING` 把提交后的余额带回来**：用 `RETURNING`。查一次会多一次往返，而且两次之间余额可能已被别的请求改过，写回去的就不是"这次写入的结果"。
- **新鲜度用进程时钟判 vs 用数据库时钟判**：用数据库时钟。写入时间由数据库盖章，拿它跟 API 进程的时钟比会因两个时钟的漂移把刚写的值判成旧的（或反过来）。
- **重放也走预检 vs 重放不受预检管辖**：重放不受管辖。不豁免时，余额刚好够扣一次保底额的账户"立刻重发同一个键"会得到 402，而那个 Job 本来就在——这是把"重发"变成看余额脸色的行为，`a_replayed_request_is_never_refused_by_the_cached_balance` 就是为它写的。
- **缓存 `api_key:<sha256>` 映射 vs 不做**：不做。仓库今天没有吊销入口（`identity.api_keys.revoked_at` 只有读侧过滤），缓存它会让"直接改库吊销"的键在 TTL 内继续可用——那是安全口径的退步，而工单的验收清单里没有这一条。
- **把 redis 服务写进 `compose.yaml` vs 不加**：不加。仓库现有 compose 只有 postgres，本片开工时的口径是"没有该服务就不要自行新增，改用未配置即降级的路径验收"。
- **端到端用真实 Redis vs 进程内假 Redis**：端到端用进程内假 Redis。验收要构造"缓存被人为改错""失效没成功""缓存服务停掉"三种情形，真实服务上没法稳定复现；假 Redis 认不出的命令一律报错，避免"命令名写错了却悄悄通过"。真实服务那一路另有一条 `#[ignore]` 用例对着 `REDIS_URL` 验命令语法与 TTL。

## 后果

- **换来的是**：余额快照多了一条写穿与对账；缓存整个不可用时行为与没有它时逐位相同。
- **付的代价**：多一个可选运行时依赖与一条失败路径；缓存操作本身的时间会加到写穿路径上，上界是 `CACHE_OPERATION_TIMEOUT_MS`。
- **对账只做增量**：窗口取对账周期的三倍，窗口之外没被动过的账户，其缓存条目只在下次写穿或对账触及时纠正；发现"很久没动过的账户的缓存被改错"需要全量扫描，本片不做。
- **`supply.offerings.enabled` / `supply.channels.enabled` 的生效不经缓存**：受理的候选取数 SQL 直接按这两列过滤，停用与重新启用写入即生效（启停接口见 [供给身份与启停](../../proposed/platform/2026-09-23-supply-identity-and-enable-disable.md)）。
- **余额判定一律由数据库条件更新确认**：缓存快照不参与 402 的判定（见 [新记录](2026-09-30-account-balance-cache-version-guard.md)）。
- **对账任务挂在 API 进程上**：API 不在跑就没有对账；缓存里留下的错值由受理直读数据库兜住正确性，对账只负责把它纠正回来。
- **`compose.yaml` 里没有 redis 服务**：本地要验真实 Redis 路径需自己起一个并把 `REDIS_URL` 指过去。

## 验证

验收按工单 [#19](https://github.com/dehuadong/seeaihub-server-next/issues/19) 的逐条可勾选清单，在**空库 + 真实 API/Worker 进程 + 进程内假上游与假 Redis** 上跑（离线、零计费调用、零外网）。

| 验收（工单 [#19](https://github.com/dehuadong/seeaihub-server-next/issues/19)） | 证据 |
| --- | --- |
| 充值后缓存立即可见 | `cache_write_through_makes_the_balance_visible_after_every_write`（充值后缓存里的余额、来源标记 `db_commit`，并与数据库逐位比对） |
| 受理占用后缓存立即可见 | 同一条（不跑 Worker、同步入口 1 秒后超时，缓存里的余额/占用/可用额 = 数据库那一行；受理不改变已结算余额） |
| 结算后缓存立即可见 | 同一条（起 Worker 跑完同一个 Job，缓存 = 初始 − 实收 = 数据库值） |
| 停掉 Redis：结果逐位相同 | `stopping_the_cache_leaves_acceptance_and_settlement_bit_identical`（同一场景跑两遍：充值后把假 Redis 关掉 / 完全不配 `REDIS_URL`，实收、最终余额、Job 终态与对客响应结构逐位相同） |
| 陈旧缓存不得拒绝 | `a_stale_balance_entry_never_rejects`（来源改成 `reconciler`、以及写穿来源但写入时间在一小时前 → 两次都照常成功、都真的扣了钱、没有凭缓存拒绝的审计） |
| 误拒有审计（已由 [新记录](2026-09-30-account-balance-cache-version-guard.md) 取代） | 该口径与 `a_fresh_cache_rejection_is_audited` 已移除：缓存不足不再单独产生 402，改由数据库条件更新确认 |
| 重放不受缓存余额影响 | `a_replayed_request_is_never_refused_by_the_cached_balance`（余额刚好够扣一次保底额 → 受理之后缓存里是低于保底额的值 → 同键立刻重发去重成原 Job、余额不变、无审计） |
| 人为改错 → 对账以数据库为准覆盖并留审计 | `the_reconciler_overwrites_corrupted_entries_from_the_database`（对账周期 1 秒：改错余额 → 余额被覆盖回数据库的值且来源变成 `reconciler`，并留一条审计） |
| 真实 Redis 路径 | `crates/cache-redis/tests/real_redis.rs` 的 `a_real_redis_round_trip_keeps_values_and_honours_the_ttl`（`#[ignore]`，`REDIS_URL` 指向真实服务时验 `GET` / `SET … PX` / `DEL` 与 TTL 到期；没给就跳过） |
| 缓存不可用时的失败路径 | `crates/cache-redis` 单测 `an_unreachable_cache_reports_a_miss_instead_of_hanging`（指向没人监听的端口：三条命令都很快失败，不挂住请求）与 `a_malformed_address_is_a_configuration_error` |
| 零配置行为不变 | 既有 58 条端到端用例在不配 `REDIS_URL` 时逐位通过（整跑 65/65，其中 7 条为本片新增） |
| 门禁 | `cargo fmt --all -- --check` exit 0；`cargo clippy --workspace --all-targets --all-features -- -D warnings` exit 0；`cargo test --workspace --all-features` 全绿（22 / 32 / 2 / 3 / 67 / 2 / 59 各 crate 单测，65 条端到端按设计 ignore）；`cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1` 在最终代码上 **65 passed / 0 failed**（180.77s）；`node scripts/decisions/check.mjs` 通过 |
