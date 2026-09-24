---
title: 单次请求成本上限：发布期拒误配、受理期兜住已发布的供给
status: implemented
created: 2026-09-24
updated: 2026-09-24
approval: 用户 2026-09-23 裁定「成本护栏」是平台自己的运营护栏、与客户余额无关，机制由我定、数值留作部署期配置；本条按该口径实施。
verification: 端到端两条（`a_candidate_that_could_cost_more_than_the_ceiling_is_rejected_at_publication`、`a_request_that_would_cost_more_than_the_ceiling_is_a_platform_fault`）+ 模块单测七条；`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`、`cargo test --workspace --all-features` 全绿；空库端到端 106 passed / 0 failed；`node scripts/decisions/check.mjs` 通过
---

# Agent Note：单次请求成本上限：发布期拒误配、受理期兜住已发布的供给

## 问题

受理路径上只有一道与钱有关的闸门：**余额 ≥ 保底额**（不足即 402 `insufficient_balance`）。它回答的是"客户付不付得起"，不回答"这一笔会让我们付给上游多少"。于是三类情形都没有东西挡：发布时把单价配错、合同允许的档位异常大、折算率变差之后平台还按老口径放行——一次请求可能让平台付掉远超预期的上游成本。2026-09-22 已经删掉过一条形式相近的规则（拿**调用方给的** `max_cost_microusd` 与被选中候选的最小可能费用比），删它是因为那个量受理时算不准；本条判的是另一件事：**发布物与折算率**给出的单次成本上界。

## 决定

- **上限是运维配置**：`GENERATION_MAX_REQUEST_COST_MICROUSD`（人民币微单位，默认 10 元）。它不随修订发布、不进 Job 快照、不改对客价。与 `GENERATION_MAX_COST_MICROUSD` 不是同一个量：后者是"查不到供给封顶保底值"时的兜底**保底额**。
- **两处判，一处防误配、一处兜运行期**：发布期按**合同允许的最大输出张数**算候选的最大单次成本，超上限**拒绝整份发布**并点名候选、金额与上限；受理期在冻结定价之后、预检与建 Job 之前按**这一次请求**再判一次。
- **单次最大成本按计价形态取**：`per_image` = 单价 × 张数；`per_call` = 单价；`token_rates` / `upstream_declared` = 发布者给的**参考成本**。判前用那一份折算率折成人民币（上限是运营按人民币设的数）；**算不出成本就不判**。
- **对客是平台侧故障**：新内部错误 `ApplicationError::RequestCostCeilingExceeded` 映射到 **503 `platform_unavailable`**，与"一条候选都不合格"同一条对客语义，不新造对客码，也不说成余额不足；不建 Job、不扣款、不留预授权。
- **判据是 `>`**：恰好等于上限算通过。
- **上限不进发布数据**：它是部署期的一个数，发布期与受理期读同一份；发布期用的折算率是"此刻生效的那一行"（发布期没有"受理时刻"这个概念）。

## 备选方案

- **给按 token 计量量的形态加一个"声明的最坏用量"发布字段**（实施计划里写的是"四档费率 × 声明的最坏用量"）：落选。发布数据里没有这个字段，为一条护栏新造一个字段，等于让每个发布者多填一个只被护栏读的数；**参考成本**本来就是发布者按"该渠道四档费率 × 参考用量"给出的单次成本声明（`docs/design/0007` §2），直接用它，判据与数据都不新增。代价如实记在"后果"里。
- **发布期把超了的那条候选剔掉、其余照发**：落选。候选集连同档位与权重是一个整体，替发布者删一条会让路由悄悄变成另一副样子。
- **只在发布期判**：落选。上限是进程启动时读的数，调小之后已经生效的候选不会自己重判；折算率变化也只有受理期看得见。
- **只在受理期判**：落选。误配要等到第一次真实请求才发现，而那一次可能已经花了钱。
- **判不出来时按"超了"拒**：落选。旧形状、没带参考成本的素材会整批发不出去，而它们本来就不在这条护栏的判据里。
- **复用 402 `insufficient_balance` 或新造一个对客码**：落选。前者把平台的钱说成客户的钱；后者给消费者加了一个他处置不了的码——他能做的只有等平台修。
- **发布期的折算率按"最近一行"读**：落选，与全仓"受理时刻生效的那一行"同一口径，不为护栏另立一条取值规则。

## 后果

- **判据的覆盖边界**（写清，免得读成"平台不会亏"）：只有 `per_image` 真的算得出"最坏一次"；`per_call` 与请求无关；`token_rates` 与 `upstream_declared` 取的是**参考成本**——那是单次成本的**声明**，不是被证明的上界，真实用量更大或上游临时涨价都不会经过它。没带参考成本的旧形状素材同样判不出来。
- 已经发布、但按今天的数算下来超上限的供给会在受理时被 503 挡下，**对客表现为平台不可用**：运营按日志里的理由（点名型号、候选、成本与上限）决定是调上限还是改那条候选的定价。
- 上限默认 10 元，比当前发布面的单次成本（约 0.08 元）高两个数量级：默认值只挡"明显配错"的形态，正常发布逐位不受影响。
- 上限是**进程启动时**读的：改它要重启 API 进程。
- 发布期多一次按币种取折算率的读（同一份发布里同币种的候选共用一次）。
- 没做的：熔断与自动降级（属运营策略）；对客价口径与定价数值不动。

## 验证

| 行为 | 证据 |
| --- | --- |
| 上限恰好落在候选最大单次成本上 ⇒ 发布照常；超一微单位 ⇒ 整份发布被拒，报错点名候选、金额与上限 | `a_candidate_that_could_cost_more_than_the_ceiling_is_rejected_at_publication`（`apps/api/tests/http_contract/cases_cost_ceiling.rs`） |
| 发布时合规、折算率变差后超限 ⇒ 受理 503 `platform_unavailable`，不建 Job、不扣款、不留预授权；折算率调回正常后同一个请求照常跑完（反向对照） | `a_request_that_would_cost_more_than_the_ceiling_is_a_platform_fault`（同文件） |
| 四种计价形态的单次成本、缺参数不是 0、币种对不上或缺折算率不判、恰好等于上限允许（按张与按次两种形态） | `crates/application/src/cost_ceiling/tests.rs` 七条 |
| 正常供给逐位不受影响（回归） | 空库端到端全量 106 passed / 0 failed；`cargo test --workspace --all-features` |
| 门禁 | `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`、`node scripts/decisions/check.mjs` |

## 依据与关联

- 机制与判据的边界见 [`docs/design/0009`](../../../../docs/design/0009-operational-baseline.md) §7；受理闸门的原口径见 [`docs/design/0007`](../../../../docs/design/0007-pricing-floor-and-settlement.md) §6。
- "受理的唯一上限是客户余额"这句话与后加的护栏之间的关系，已就地标注在 [`ADR-0009`](../../../../docs/adr/0009-multiple-active-offerings-and-routing.md) 的 2026-09-24 修订段。
- 参考成本是发布者按"费率 × 参考用量"给出的单次成本声明，见 [`docs/design/0007`](../../../../docs/design/0007-pricing-floor-and-settlement.md) §2；成本事实本身见[渠道成本事实采集](./2026-09-22-provider-cost-facts.md)。
