---
title: 并发名额在上架时给出，进程配置不再提供名额
status: implemented
created: 2026-10-08
updated: 2026-10-08
approval: 用户 2026-10-08 明确"`GENERATION_MAX_CONCURRENT_JOBS` 从刚开始的需求本来就是不要的，继续保留就是没有完成工作任务"，并要求上架与编辑两条路径都能设；输入「批准，执行实现 #99」授权实施。Plan Review 四轮（三轴 + 收敛核查）与 Implementation Review 两轴均通过，证据记在工作项 #99；产品合同见控制台 Spec v25 与同步网关 Spec v7。
verification: 2026-10-08 本地：`cargo fmt --check`；`cargo clippy -p seeai-application -p seeai-persistence -p seeai-api --all-targets` 干净；`cargo check --workspace --all-targets`；真库契约 `cases_model_concurrency` 5/5（含"上架给的名额真的管住并发"与发布路径审计三条断言）、`cases_migrations::the_quota_required_migration_backfills_one_and_keeps_pinned_values`、`cases_publication`、`cases_direct_execution`、`cases_lifecycle`、`cases_admin_surface`、`cases_routing`、`cases_cache`、`cases_cost_facts`、`cases_model_type`、`cases_pricing` 全绿；`cargo test -p seeai-application --lib` 176、`-p seeai-persistence --lib` 16、`-p seeai-persistence --test execution_admit -- --ignored` 3；`npx tsc --noEmit`；`npx playwright test e2e/admin-model-concurrency.spec.ts` 连跑两次通过。
---

# Agent Note：并发名额在上架时给出，进程配置不再提供名额

## 问题

#94 把并发名额落在 `publication.gateway_models.max_concurrent_jobs` 上，但当时做成**可空**（`NULL`＝用部署缺省 `GENERATION_MAX_CONCURRENT_JOBS`），而且只有**编辑**路径能设（模型卡片的 `PATCH`）。两处不合需求：

- **上架路径没有设置项**：发布抽屉没有该字段、发布命令不收它、发布事务只写 `(gateway_model, updated_by)`；
- **进程配置仍是名额来源**：模型行为 `NULL` 时读 `GENERATION_MAX_CONCURRENT_JOBS`——用户指出这个变量"本来就是不要的"，留着它就是没做完。

## 决定

1. **进程配置里没有名额来源**：`GENERATION_MAX_CONCURRENT_JOBS` 的读取、`AppState` 字段、`DirectExecutionLimits.default_max_concurrent_jobs`、受理路径里的 `unwrap_or`、读面顶层的 `max_concurrent_jobs_default`、`configuration.md` 与 `.env.example` 的条目、e2e 起进程脚本里的注入，全部删除。名额只有 `publication.gateway_models.max_concurrent_jobs` 一个来源。
2. **列必填**：迁移 `0049` 回填存量行 **1** → `SET NOT NULL` → **重写列注释**（`0048` 的注释写着"`NULL`＝用部署缺省"，与新状态矛盾；`0048` 已应用、按校验和规则不改）。**切换没有降流窗口**：迁移一跑，回填对所有副本当场生效——要保留更大并发的部署，在升级**之前**用 `PATCH /api/v1/gateway-models/{model}`（模型页也能做）把现值逐个钉到模型上，用 `GET /api/v1/gateway-models` 核对，再升级。这条顺序写在 `docs/operations/deployment.md` §3.3，`production.md` §3 与 `production-docker.md` 的升级节各有一处警示。
3. **上架路径**：发布命令加可选字段 `max_concurrent_jobs`（正整数）。写值落在 `publish_runtime` 的**同一事务**里，形状与 `set_gateway_model_settings` 一致：① `SELECT enabled, max_concurrent_jobs … FOR UPDATE`（没有行＝新建，有行＝已有，同时拿到改前值）；② 新建走 `INSERT … (gateway_model, max_concurrent_jobs, updated_by) VALUES ($1, COALESCE($2, 1), $3) ON CONFLICT (gateway_model) DO UPDATE SET max_concurrent_jobs = COALESCE($2, publication.gateway_models.max_concurrent_jobs)`——`ON CONFLICT` 保住"两个并发发布同一个新名字"的幂等；已有走 `UPDATE … SET max_concurrent_jobs = COALESCE($2, 现值)`，时间戳只在值真的变了时动。**两处都不用 `EXCLUDED`**（它省略时带的是新行的 1，会把已有模型的名额悄悄重置），**SET 都不含 `enabled`**（重新发布不能把停用的模型悄悄打开）。语义：命令省略 → 新建按平台固定值 1、已有保持现值；给了值 → 写它。
4. **它仍是运行状态**：名额不进 `runtime_revisions` / `runtime_entries`、不进该次发布的快照与发布响应、不进 `OfferingCandidate`、Job 不固定它；发布事务写它，只是"上架这个模型的同一时刻把额度落在一张运行状态表上"。`ActiveOfferings.max_concurrent_jobs` 仍是 `Option<u32>`：候选集为空时那次读读不到名额行，而那条路会立刻返回 `NotFound`，用不到名额；受理在判空**之后**才把 `None` 当内部错误。
5. **编辑路径**：`PATCH /api/v1/gateway-models/{model}` 的**省略＝这次不改**（启停按钮只发 `{enabled}`，这条必须继续成立）；**给了名额就必须是正整数**，`null`／0／负数／超出 `i32::MAX` → 400 并点名；发布命令的名额走同一套值域判据（0 与超列宽在接口层拒成 400，不落到库层 `CHECK`／落列转换变成 500）。
6. **审计**：两条写入方都用 `gateway_model.set_max_concurrent_jobs`（载荷给改前改后），触发口径不同且这是有意的——`PATCH` 里给了这个字段就写一条（**空改动也记**，"运营按了一次保存"要看得到）；发布路径**只在已有模型的名额真的变了**时写（发布的主语是修订），**新建的模型不另写**（那次发布本身有发布审计）；发布不动开关列，因此不写 `set_enabled`。
7. **控制台**：发布抽屉的字段**只在运营改过它时发送**（型号名是自由文本，上架模式重发已有模型名时不能因预填把名额钉住）；型号名命中已有模型时旁边显示它的当前值；改价态显示当前值、同样只在其被改过时发送；模型卡片的名额改成必填正整数，非法值由表单规则点名拒绝。
8. **合同与属主同一变更里同步**：Spec `0001` **v25**（M1 与 V-D17 改写、M2 加发布命令那一句、新增 V-D18）与 Spec `0005` **v7**（只动 §6）；design `0006` §2.2 读响应示例（删顶层缺省、模型内改成具体数字）与 §2.3/§2.4、`0012` 升 v4（记下发布命令带运行状态字段、同事务写）、`0017` 升 v3（§6 容量表）、`0009` §3、`CONTEXT.md` 的模型并发名额词条、`configuration.md`、`.env.example`、`deployment.md` §3.3 与两份衍生。
9. **与既有记录的边界（部分取代，互链）**：[网关模型的并发名额](./2026-10-08-gateway-model-concurrency-quota.md) 里作废的是——决定 1 的"可空＝用部署缺省"、决定 6 的"`null` 清成缺省"与读面顶层缺省字段、决定 7 的"旧变量降级为部署缺省"，以及验证表里对应的几行；**仍有效**的是：判定维度「账户 × 网关模型」、名额落在模型行、取值路径（受理同一次查询带出）、审计动作名与"空改动也记一笔"的形状；那份记录已标注失效范围并链接本记录。[对客请求体扁平化…](./2026-09-20-flat-request-body-and-asset-roles.md) 里"模型没设时用部署缺省"那句也一并改掉了。

## 备选方案

- **保留配置缺省，只把它标成可选**：落选。用户明确说它"本来就是不要的"；留着它等于"没设过的模型由配置文件决定"，正是需求排除的形态。
- **列保持可空、`NULL` 当 1**：落选。"未设"会长期存在，读面与控制台都要解释两层含义；必填 + 上架时给出更直白。
- **名额随修订冻结**：落选（同 #94：改一个并发数不该要求重新发布，且它不影响已受理的 Job）。
- **上架成功后另调一次设置接口**：落选。发布成功与名额写入会分成两个事务，留下"发布成功、名额没写成"的半成品。
- **发布命令把 `enabled` 一起写**：落选。那会让重新发布把停用的模型悄悄打开。
- **回填沿用部署现值**：做不到。迁移读不到进程环境；只能回填平台固定值 1，并把"逐个模型设回"写进升级说明。
- **回填迁移不做 NOT NULL、继续留可空**：落选。那等于把"未设"这一态永久留下，与"配置里不再有名额"的意图相反。

## 后果

- 名额只有一个来源、一条判定路径：运营在**上架**或**编辑**时给，正整数值域在两处接口都当场判。
- **升级即改容**：原来把变量设成 N 的部署，升级后每个模型是 1，直到运营逐个设回。这是"进程配置不再是名额来源"的必然结果；无窗口的恢复顺序写在 `deployment.md` §3.3。
- 上架抽屉默认留空：新建的模型按 1 跑（文案说明"一次一任务填 1"），已有模型不改。
- 读面形状变了一次（删掉顶层 `max_concurrent_jobs_default`、模型内由 `null` 变数字）——破坏性变化，控制台与用例在同一变更里改完。
- 发布与 `PATCH` 各写一次名额：两处逻辑必须合读（判定维度、锁序与审计触发口径不同），这是有意保留的分工。

## 验证

| 交付事实 | 证据（真库 + 真进程 + 真浏览器，2026-10-08） |
| --- | --- |
| 上架命令给的名额真的管住并发（2 个在飞被受理、第三个 429） | `cases_model_concurrency::the_published_quota_bounds_the_in_flight_requests` |
| 上架不给名额 → 新建按 1；重发不给 → 保持；重发给新值 → 更新；重发不改 `enabled` | `cases_model_concurrency::the_publish_path_sets_the_quota_and_republishing_keeps_or_updates_it` |
| 发布路径的审计只在"已有模型的名额真的变了"时写 | 同上（0／1／仍 1 三条断言） |
| 名额必填：`null`／0／负数／超列宽 → 400 点名；只给 `{enabled}` 仍成功 | `cases_model_concurrency::the_quota_is_required_and_each_change_is_audited` |
| 编辑路径设的名额只管这个模型、改完即时生效；不同模型互不挡 | `a_quota_set_by_operations_bounds_only_that_model`、`another_model_is_not_blocked_by_an_in_flight_request` |
| 迁移：钉住的值保留、`NULL` 回填 1、列 `NOT NULL`、列注释改写、库层 `CHECK` 拒 0 与负数 | `cases_migrations::the_quota_required_migration_backfills_one_and_keeps_pinned_values` |
| 没有可调候选的模型仍然 404（不是 500） | `cases_routing`（已跑组内） |
| 控制台：卡片能改不能清空、非法值点名；改价抽屉显示当前值且不改就不发送 | `apps/web/e2e/admin-model-concurrency.spec.ts`（连跑两次通过） |
| 相邻行为不回归 | `cases_publication`／`cases_direct_execution`／`cases_lifecycle`／`cases_admin_surface`／`cases_routing`／`cases_cache`／`cases_cost_facts`／`cases_model_type`／`cases_pricing`、`crates/persistence/tests/execution_admit.rs`、两 crate 单元测试、`cargo check --workspace --all-targets` 与 clippy |
