---
title: 网关模型的并发名额：挂在模型上、由运营设置，受理按「账户 × 模型」计数
status: implemented
created: 2026-10-08
updated: 2026-10-08
approval: 用户 2026-10-08 决定并发名额由运营在**模型**上设置、不写进部署配置，并输入「批准，执行实现」授权实施。Plan Review 三线（Standards／Spec／Architecture）与 Implementation Review 两轴均通过，证据记在工作项 #94；产品合同修订见同步网关 Spec v6 与控制台 Spec v24。
verification: 2026-10-08 本地：`cargo fmt --check`；`cargo clippy -p seeai-application -p seeai-persistence -p seeai-api --all-targets` 干净；`cargo test -p seeai-application --lib` 176 通过、`cargo test -p seeai-persistence --lib` 16 通过；`cargo check --workspace --all-targets` 与 `npx tsc --noEmit`（apps/web）通过；真库契约 `cases_model_concurrency` 4/4、`cases_migrations::the_model_quota_migration_leaves_existing_models_unset`、`cases_direct_execution::two_api_replicas_share_the_account_and_channel_capacity`，另有受影响组 `cases_direct_execution`／`cases_lifecycle`／`cases_publication`／`cases_admin_surface`／`cases_routing`／`cases_cache`／`cases_cost_facts`／`cases_model_type` 全绿；`cargo test -p seeai-persistence --test execution_admit -- --ignored` 3/3；浏览器 `npx playwright test e2e/admin-model-concurrency.spec.ts` 连跑两次通过。
---

# Agent Note：网关模型的并发名额：挂在模型上、由运营设置，受理按「账户 × 模型」计数

> **失效范围（2026-10-08 同日）**：本记录里"列可空、`NULL`＝用部署缺省"的部分**已作废**——用户指出进程配置本来就不该是名额来源。取代记录见[并发名额在上架时给出，进程配置不再提供名额](./2026-10-08-quota-set-at-publish.md)（工作项 #99）：列改 `NOT NULL`、名额在上架或编辑时由运营给。仍然有效的部分：判定维度「账户 × 网关模型」、名额落在模型行、取值路径（受理同一次查询带出）、审计动作名与"空改动也记一笔"的形状。

## 问题

受理时的在飞判定曾按**账户合计**数（`generation.jobs` 里该账户 `admitted` / `executing` 的条数），上限是部署级的一个数（`GENERATION_MAX_CONCURRENT_JOBS`，缺省 1）。两处不合需求：

- 同一账户有一个图片任务在跑时，它的视频或对话请求被 `429 too_many_in_flight` 挡住——一个模型的任务占住了所有模型的额度；
- 上限对所有模型是同一个数，运营上架新模型时改不了它，只能改部署配置。

需求是：并发名额按**每个网关模型**给（图片与视频都是"一次一任务"），不同模型互不挡；名额由运营在模型页设置，改完即时生效。

## 决定

1. **落点与生命周期**：`publication.gateway_models.max_concurrent_jobs integer CHECK (max_concurrent_jobs > 0)`（迁移 `0048`），**可空**——`NULL` 表示"用部署缺省"。这张表是"每个网关模型一行"（`enabled` 开关就在它上面），名额与它同层：**运行状态**，不是发布内容，不随 Runtime Revision 冻结，改了立刻影响之后受理的请求。
2. **它不造第二份权威**：`migrations/0007` 给这张表写过"只放运维开关……存第二份就等于造第二个权威"，说的是**定义**（合同、候选集、价）不在这里。名额与 `enabled` 同类（design `0006` §2.4：可变位是运行状态、不是定义；`ADR-0009` 把两个开关当运行状态、把 `active` 当"按型号全局可变的事实"，`ADR-0020` 把运行期策略与发布物分开）。
3. **受理判据**：按「账户 × 该网关模型」数在飞 Job——`WHERE account_id = $1 AND gateway_model = $2 AND state IN ('admitted','executing')`；到名额回 `429 too_many_in_flight`（码与 `Retry-After` 不变）。`gateway_model` 这一列本就存在（迁移 `0004` 改名而来，受理时本来就写它），没有加列、没有回填。对账态不占名额。
4. **取值路径（模型级取值，不进候选）**：受理读候选的那次查询 `active_offering` 本就 JOIN `publication.gateway_models`（判 `enabled`）；`gm.max_concurrent_jobs` 作为**模型级字段**随同一次查询带出（`ActiveOfferings`），**不放进 `OfferingCandidate`**——那个类型属于发布物（发布接口原样返回它），发布事务也是先写条目再写模型行，发布路径填不出它。生效名额在**应用层**合成：`模型行的值 ?? 进程配置的 GENERATION_MAX_CONCURRENT_JOBS`；持久化只按传进来的数计数，不在 SQL 里兜缺省。
5. **索引**：在飞计数按（账户 × 模型），原有索引只有账户前缀与状态部分索引；迁移加了部分索引 `ON generation.jobs (account_id, gateway_model) WHERE state IN ('admitted','executing')`。
6. **运营面**：搭现成的 `PATCH /api/v1/gateway-models/{gateway_model}`。
   - 请求体两个字段都可省略（`enabled` 由必填改可选，旧调用方继续送它不受影响；只改名额的调用不再被迫带上旧开关值，避免丢失更新）；`max_concurrent_jobs`：省略＝这次不改、`null`＝清成"用部署缺省"、给值＝设成它；`enabled` 给 `null` → 400；两个字段都不给 → 400；未知字段仍拒（`deny_unknown_fields`）；名额给 0、负数或超出 `integer` 列宽的正整数 → **400 并点名该字段**（不落到库层 `CHECK` 变成 500）。
   - 读面：每个模型给原值 `max_concurrent_jobs`（可为 `null`），**响应顶层**给一次 `max_concurrent_jobs_default`——否则控制台只显示空框，运营看不出实际生效多少。
   - 审计：请求里给了 `enabled` 就写 `gateway_model.set_enabled`（沿用今天的动作名与载荷形状，历史不断裂），给了名额就写 `gateway_model.set_max_concurrent_jobs`（载荷给出改前改后）；空改动也记一笔，与 `set_offering_enabled` / `set_channel_enabled` 一致。
   - 控制台模型页（Spec `0001` M1）在卡片上显示 `用部署缺省（当前 N）` 或具体值，并可设可清；非法值由表单规则点名拒绝（`并发名额至少 1`），不在控件层悄悄压成 1。
7. **迁移与兼容**：新迁移（`sqlx::migrate!` 按校验和，历史迁移不就地改）；存量行留 `NULL`，所以**升级不改变任何模型的名额**。`GENERATION_MAX_CONCURRENT_JOBS` **不删除，降级为部署缺省**——生产原来设 8 的部署切换后仍是"每模型 8"，运维可以逐个模型设值后再改这个缺省。
8. **边界**：渠道全局名额（`GENERATION_MAX_CHANNEL_IN_FLIGHT`）与本机执行容量不变；多副本经数据库计数共同遵守（计数在受理事务里、账户行锁下做）；对客面（`/v1/models` 与 429 响应体）不出现名额——它是运营口径；不再有按账户合计的在飞上限。

**同一变更里同步的属主**

- design `0006` §2.4（可变位从"只有 `enabled`"改成两个）与 §2.2 的读响应示例（模型内原值 + 顶层部署缺省）；design `0017` 升 v2、§6 容量表那行改成按模型；design `0009` §3 的容量维度改成"每账户在每个网关模型上的在飞 Job 数"，并去掉把并发说成"请求速率"的措辞。
- Spec `0005` v6（§3、§6、§8 A4/A8）与控制台 Spec `0001` v24（M1、V-D17），都已接受。
- `docs/operations/configuration.md`（环境变量的属主）与 `.env.example`：写清 `GENERATION_MAX_CONCURRENT_JOBS` 现在是**部署缺省**。
- `CONTEXT.md`：Gateway Model 词条补"设它的并发名额"，新增**模型并发名额**（Model Concurrency Quota）词条。
- `crates/persistence/src/lib.rs` 模块表：`publication.gateway_models` 一行扩成"运维开关与并发名额"，`generation.execution_capacity` 一行写明"账户在某模型上的并发名额不在此表"。
- 两份已交付记录同步事实并链接本记录：[对客请求体扁平化…](./2026-09-20-flat-request-body-and-asset-roles.md) 的"额度按**账户**算"改成账户 × 模型；[不做按金额的受理闸门](./2026-10-09-no-amount-based-admission-guards.md) 的"每账户在飞名额"同理。

## 备选方案

- **按模型类型的部署变量**（`GENERATION_MAX_CONCURRENT_JOBS_IMAGE` / `_VIDEO` / `_CHAT`）：落选。名额仍写在部署配置里，运营上架新模型时改不了它；而且"类型"是模型自身的事实，用它当配额维度会把类型拖进运行期策略（`CONTEXT.md` 的模型类型词条明确避免这一点）。
- **挂在 Vendor Model 上**：落选。同一 Vendor Model 可以发布成多个网关模型（`ADR-0009`），名额是"运营给这个对客模型多少并发"，粒度比 Vendor Model 细。
- **单独的配额表**（`publication.gateway_model_quotas` 之类）：落选。一个模型一行、一个可空整数，单独开表只多一次 JOIN 与一套生命周期。
- **保持部署级、只把默认值调大**：落选。调大解决不了"一个模型占住所有模型"的形态。
- **按 API Key 计数**：落选。并发是"同时在跑几个上游任务"，与"谁调用的"无关；按 Key 会让同一账户多把 Key 各占一份额度，反而放大上游并发。
- **名额随修订冻结**：落选。它会在发布时就固化，运营改一个并发数要重新发布一次；而它不影响已受理的 Job（判定只看当前在飞的其它 Job），不需要版本化。
- **非空列 + 迁移回填 1**：落选。生产把缺省设成 8 时，回填 1 等于**静默降容**；可空 + 部署缺省没有这个问题。

## 后果

- 同一账户可以同时跑一个图片模型任务与一个视频模型任务（各自在名额内），"一次一任务"由每个模型自己的名额表达；运营上架新模型不用等改部署配置。
- **本机容量仍是共享的**：一个账户可以靠"每个模型各占一个名额"占满本机执行名额，之后别的账户/模型拿到 `503`。这是有意保留的（本机容量是全局保护），但运营把很多模型的名额设大时会更早撞上它。
- **计数代价**：受理事务多一次按（账户 × 模型）的计数；有部分索引后是索引内的少量行，但账户在飞数很大时仍是线性扫描——需要时再按实测决定是否改计数表。
- **运营误设**：名额设得很大等于放开上游并发；列上的 `CHECK (> 0)` 与接口层的 `1..=integer` 只挡下界与列宽，上界没有硬约束——靠容量闸门与告警发现。
- **切换期**：缺省值的语义从"每账户"变成"每模型"，同一账户能同时在跑的数从"一个数"变成"各模型名额之和"。要收紧的部署先逐个模型设值、再改缺省，别先调小缺省。

## 验证

| 交付事实 | 证据（真库 + 真进程，2026-10-08） |
| --- | --- |
| 某模型名额设 1：该账户在该模型上并发第二个 → 429；设 2 后（不重新发布）能并发两个 | `cases_model_concurrency::a_quota_set_by_operations_bounds_only_that_model` |
| 同一账户同时调两个不同模型 → 两个都受理（模型 A 占住名额不挡模型 B） | `cases_model_concurrency::another_model_is_not_blocked_by_an_in_flight_request` |
| 清成 `null` 回到部署缺省（不是"没有上限"） | `cases_model_concurrency::clearing_the_quota_falls_back_to_the_deployment_default` |
| 请求体边界各点名字段：两个字段都不给、`enabled: null`、名额 0 / 负数 / 超列宽、未知字段；审计按给了哪一项写 | `cases_model_concurrency::bad_patch_bodies_are_rejected_and_each_change_is_audited` |
| 升级不改变存量模型的名额（列可空、无回填），库层 `CHECK` 挡 0 与负数 | `cases_migrations::the_model_quota_migration_leaves_existing_models_unset` |
| 同一模型的名额跨副本共同遵守（部署缺省 1） | `cases_direct_execution::two_api_replicas_share_the_account_and_channel_capacity` |
| 控制台能设、能清、未设时显示部署缺省，非法值点名拒绝 | `apps/web/e2e/admin-model-concurrency.spec.ts`（连跑两次通过） |
| 相邻行为不回归 | `cases_direct_execution`／`cases_lifecycle`／`cases_publication`／`cases_admin_surface`／`cases_routing`／`cases_cache`／`cases_cost_facts`／`cases_model_type`、`crates/persistence/tests/execution_admit.rs`、两 crate 的单元测试、`cargo check --workspace --all-targets` 与 clippy |

**覆盖边界**：`GENERATION_MAX_CONCURRENT_JOBS` **完全未设**时回落到 1 的那条分支是既有代码（读取函数没改），没有端到端用例专门跑"变量缺席"的进程；生效值这一层由"清成缺省"那条用例覆盖。
