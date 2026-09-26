---
title: 输出张数按候选承载面的上界收敛
status: implemented
created: 2026-09-24
updated: 2026-09-24
approval: 用户 2026-09-24 裁定：请求的 `n` 在合同之内、超过该候选承载面声明的 `maximum` 时，按该上限发出去，不返回 503。
verification: 本改动面的定向证据——模块单测四条（`crates/application/src/tests/request_preparation.rs` 的 `the_requested_image_count_is_capped_at_the_carriers_declared_maximum`、`the_cap_follows_the_carriers_wire_name_for_the_image_count`；`crates/application/src/tests/routing.rs` 的 `a_count_above_the_carriers_maximum_does_not_disqualify_it`；`crates/application/src/declared_images/tests.rs` 的 `the_maximum_is_read_by_the_name_the_caller_asks_for`）+ 对客端到端一条（`apps/api/tests/http_contract/cases_parameters.rs` 的 `an_image_count_above_the_carriers_maximum_is_capped_at_that_maximum`）+ 同一路径的合同上界回归用例；`cargo fmt --all -- --check` 通过；`node scripts/decisions/check.mjs` 通过。Rust 全量门禁由 CI 承担。
---

# Agent Note：输出张数按候选承载面的上界收敛

## 问题

[输出张数按合同声明的取值面校验](./2026-09-24-output-image-count-bounds.md)把受理期的取值校验收进了**合同**那一道界，留下一处未决：**合同之内、候选承载面之外**怎么办。当时的处置是"把值原样发给它"——于是 `config/bootstrap/gpt-image-2.5-{flare,sunburst}.json` 里合同声明 `n` 最多 10 张、APIMart 那条候选的承载面只声明最多 4 张时，调用方给 6 会被原样发往只收 1–4 张的渠道，上游与超时窗口、成本护栏也各按各的算。

界本来就是两个数：合同是**调用方**的界面（他能提交什么），承载面是**这条候选**的能力面。请求落在两者之间时，平台要么按候选的能力表达它，要么承认这条候选承载不了。

## 决定

- **候选仍然合格**：请求里 `n` 超过这条候选承载面为它声明的 `maximum` 时，按该上限发出去。改的只是**这一次要几张**，不因此换候选、也不返回 503。
- 理由：`n` 是"最多要几张"，不是"必须给我几张"。给得比候选能做的多，说的是"少给几张也成"，不是一次非法请求；为此判承载不了，等于把一次合法请求说成平台故障。
- **只收敛上界**：承载面声明的 `minimum` 不参与判定——把值往上抬等于替调用方多要图，平台不做这件事；低于它的值原样上行，由上游按自己的 schema 处置。
- **只对 `n`**：别的参数（枚举、区间、类型）的取值处置不变，平台不替上游维护一份取值清单。
- **上界按承载面线上那个名字找**：承载面自己声明了 `n` 就用它，否则看改名表把 `n` 落到哪个名字上——与承载校验是同一条规则，不会因为换了个线上名字就把超界的值原样发出去。
- **与合同那道界分工**：超出**合同**的上界是调用方的参数问题（400 `invalid_parameter`，不建 Job、不扣款），这一条不变。
- **结算与它无关**：最终按实际产出与用量结算（`per_image` 读产出张数、`token_rates` 读实际用量），请求里的张数从来不是计费判据。它只影响超时窗口、单次成本护栏与发往上游的张数——这三处读的都是同一份冻结参数面，因此自动跟着走。

## 备选方案

- **判这条候选承载不了**（换候选，全候选都不合格即 503 `platform_unavailable`）：落选。值在合同之内，调用方没有说错话；把它说成平台故障，消费者既不知道发生了什么，也改不了什么。
- **把上界做成平台级配置**（一个全局最大张数）：落选。同一个模型不同候选声明的上界就不同（10 与 4），全局数必然要么放宽到没用、要么把合法请求拒掉。
- **连下界一起收敛**（请求低于候选声明的 `minimum` 时抬到 `minimum`）：落选。那是替调用方多要图、多花钱，而"这张图是不是他要的"平台说不清。承载面声明的 `minimum` 因此仍然是**空的判据**：承载校验只判字段名，`n` 只有上界这一处收敛。
- **推广到所有声明了上界的整数参数**（例如 `output_compression`）：落选。取值有没有"请求上限"这层语义是**这个参数**的性质，不是"整数"的性质——把压缩率从 100 收到 80 是替调用方接受一个更差的结果，不是"少给一点也成"。范围收在 `n` 上。

## 后果

- 调用方要 6 张、命中候选最多 4 张时：响应照常返回这次真实产出的图（假上游夹具里是 1 张，那是夹具的行为），Job 上冻结的 `n` 是 **4**，上线文里的 `n` 也是 4；超时窗口与单次成本护栏按 4 张算。
- 调用方要 6 张、两条候选分别声明 10 与 4 时：两条都合格，选路结果与这道收敛**无关**（选的仍是既有策略选中的那条，只是落到 4 那张候选时发 4）。想优先要满 6 张的供给，由发布顺序与路由策略表达，不由这条收敛表达。
- 承载面没声明 `n`（或没声明 `maximum`）时一个字都不改：没声明 `n` 就是这条候选承载不了这个字段（既有口径），没声明 `maximum` 就是无从夹起。
- **对账上看得见**：路由判定记录里不额外记"夹过"这件事——`jobs.native_parameters` 与上游请求体里就是实际发出去的那个数，调用方拿到的结果张数也是实际产出。

## 验证

| 行为 | 证据 |
| --- | --- |
| 承载面声明 4、合同声明 10、请求 6 ⇒ 参数面里是 4（不是不合格），1/3/4 原样保留；上限声明成 1 时按 1 发 | `the_requested_image_count_is_capped_at_the_carriers_declared_maximum`（`crates/application/src/tests/request_preparation.rs`） |
| 上限按**承载面线上那个名字**读（`num_images`）；承载面写 `maximum: 0` 就是"一张都出不了"（读法本身不带取值语义） | `the_maximum_is_read_by_the_name_the_caller_asks_for`（`crates/application/src/declared_images/tests.rs`）、`a_zero_maximum_is_not_a_declaration`（同文件，超时链那一侧的口径） |
| 两条候选分别声明 10 与 4、请求 6 张时**两条都合格**，权重分流与选路照旧，选中那条发 4 | `a_count_above_the_carriers_maximum_does_not_disqualify_it`（`crates/application/src/tests/routing.rs`） |
| 承载面把输出张数声明成线上名 `num_images`、由改名表接过去时，超界的值照样夹到上限并落在那个线上名上 | `the_cap_follows_the_carriers_wire_name_for_the_image_count`（同文件） |
| 合同之内、承载面之外的请求受理成功，上线文与 Job 冻结的都是上限 | `an_image_count_above_the_carriers_maximum_is_capped_at_that_maximum`（`apps/api/tests/http_contract/cases_parameters.rs`，自带承载面与合同的夹具） |
| 合同上界的行为不变：超过 10 张仍 400、恰好 10 张放行 | `a_requested_image_count_beyond_the_contracts_maximum_is_rejected_before_acceptance`（同目录 `cases_parameters.rs`，本件未改动它） |
| 承载面没声明 `n` 时仍是"这条候选承载不了" | 同第一条用例的后半段 |
| 本改动面的检查 | `cargo fmt --all -- --check` 通过；`node scripts/decisions/check.mjs` 通过。Rust 全量门禁（格式、Clippy、Workspace 测试与 ignored 合同测试）由 CI 承担 |

## 依据与关联

- 裁定与分工：`docs/adr/0015` 与 `docs/adr/0018` 各自的 2026-09-24 修订段；受理期规则见 [`docs/design/0005`](../../../../docs/design/0005-vendor-model-contract-and-offering-mapping.md) §4 的 R5a。
- 同一主题的另一半（合同那道界、`n` 作为第一个取值例外）：[输出张数按合同声明的取值面校验](./2026-09-24-output-image-count-bounds.md)（本件部分取代它备选方案与后果里那一项）。
- 拿 `n` 的**上界**当输入的两处读取点：超时窗口与受理期成本护栏读候选承载面那份（`crates/application/src/lib.rs` 的 `cap_output_image_count` 落值，`request_timeout.rs` 与 `cost_ceiling.rs` 消费）；发布期成本护栏与超时链自校验读**合同**那份（`declared_images::declared_output_images`）。两者共用一个读法：`declared_images::declared_output_image_maximum`。
