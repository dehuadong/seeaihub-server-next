---
title: 渠道成本事实采集：成本来源三态与按渠道声明的币种
status: implemented
created: 2026-09-22
updated: 2026-09-23
approval: 用户在会话中授权实施 P2a（成本事实采集）；范围与验收见提案 [#13](https://github.com/dehuadong/seeaihub-server-next/issues/13) 的 P2a 工单 [#15](https://github.com/dehuadong/seeaihub-server-next/issues/15)
verification: 验收合同为工单 [#15](https://github.com/dehuadong/seeaihub-server-next/issues/15) 的逐条可勾选验收清单（依据 `docs/design/0007` §1/§7/§8/§9）。**手工端到端**（真实 API `127.0.0.1:8091` + 真实 Worker + 假上游 `127.0.0.1:9099`，全新库 `seeai_p2a_verify` 上迁移 0001–0008 全新应用；零真实计费调用）：`declared` 落 `11354` / `USD`（上游声明的 11354 与渠道费率自算的 5950 不同，证明是直接取而非自算）、非 USD 声明（`CNY`）落声明值、字符串形态 `"0.011354"` 同样读出 11354、`unavailable` 三种（终态缺字段 / `cost = -0.01` / `cost = "n/a"`）金额与币种与折算值**全为 NULL**、`computed` 落 `5950`（= 14 文本输入 × 5 + 196 图像输出 × 30，每 1M）且币种 `USD`、失败执行四列 NULL（该形态已由「失败件也带成本事实」取代：见「决定」与「验证」）；同批断言对客实收恒为 `-5950`、计量证据 `total_tokens = 210`，即采集成本**不改对客金额与计量事实**。**库层"不猜"**：对 `generation.attempts` 直写 15 条（10 条违反 CHECK + 5 条合法对照）——首轮实测发现"来源 NULL 但金额非空"被库**接受**（`CHECK` 表达式求值为 NULL 时算通过，而来源为 NULL 时 `provider_cost_source IN (...)` 求值为 NULL），已就地修正 `0008` 的 `attempts_provider_cost_shape`（两支补显式 `provider_cost_source IS NOT NULL`）；修正后 10 条全部 REJECTED、5 条对照全部 ACCEPTED，另补"来源 NULL + 仅币种"与"来源 NULL + 仅折算值"两条半填同样 REJECTED。**迁移增量**：在只应用 0001–0007 的库上造一条旧 `attempts` 行，再应用 0008 → 旧行四列 NULL、其余字段逐字未变、四列无 DEFAULT（无回填）、三条 CHECK 与部分索引就位、旧行不计入 `unavailable` 缺口；增量库上新约束同样拒半填。**门禁**：`cargo fmt --all --check` exit 0；`cargo clippy --workspace --all-targets --all-features -- -D warnings` exit 0；`cargo test --workspace --all-features` 全绿（22 / 32 / 2 / 3 / 56 / 46 各 crate 单测通过，43 条端到端用例按设计 ignore）；空库端到端 `cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1` **43 passed / 0 failed**，含本次新增的三条成本用例与币种用例；`node scripts/decisions/check.mjs` 通过。另做一次反向探针：临时把来源判定改成一律记 `unavailable`，`declared` / `computed` / 币种三条单测立刻失败（`left: Unavailable`），确认它们钉得住来源判定而不是靠断言互相抵消过关；探针已完全撤除。
---

# Agent Note：渠道成本事实采集：成本来源三态与按渠道声明的币种

## 问题

上游成本此前**一家都没留痕**：APIMart 的任务终态直接给 `cost`（实测样例 `0.011354`，与按公开费率算出的金额不一致），却既不采纳也不留存；AIHubMix 不给任何金额字段，金额要平台按实际用量 × 该渠道成本费率自己算，也没存。后果是**毛利算不出来**，事后也答不出"这一笔的成本是按哪种来源取的"。

另一处是发布期的硬校验：`currency != "USD"` 直接拒绝。而四档费率表本来就是**按渠道各自记、按该渠道币种标注**的，硬写"必须是 USD"等于替渠道改币种。

本项只做**采集成本事实并落库**与**按渠道声明接受币种**——定价公式、保底表、售价快照、汇率表、结算与透支、Redis、路由策略都不在本次交付里。

## 决定

- **成本来源三态**（`ProviderCost` 报告 → 领域 `ProviderCostSource`）：`declared`（渠道终态**直接给了金额**，直接取它，不自己算）、`computed`（渠道**不给金额字段**，平台按该供给登记的**计价形态**自算——按 token 计量量是**本次实际用量** × 该渠道的**成本费率**，按产出张数是张数 × 每张单价，按调用次数是 1 次 × 每次单价）、`unavailable`（本该有金额却拿不到、或按登记的形态算不出来——**不得猜测**）。
- **计价形态是渠道事实，不是运营选项**：它由渠道决定（这个渠道的这个模型按什么计价），平台如实登记，落在 `supply.offerings.formula` 上并随发布与快照冻结；`token_rates` 要一份 Price Plan，`per_image` / `per_call` 要单价，`upstream_declared` 什么参数都不要（上游给了金额就先取它）。对客卖多少钱与它无关，走按候选发布的对客费率向量。
- **为什么是三态而不是"可选金额"**：判据是**成本从哪来**，不是"金额对不对"。合成一个 `None`，会把"这条渠道本就不报金额"误记成成本缺口，也会让本该由上游声明、却没拿到的数被自算的费率悄悄顶替。
- **三态只在两处各写一遍，转换只写一处**：SDK 的报告与领域的来源取值**逐字同名**（`computed` / `declared` / `unavailable`），SDK → 领域的映射只有一处，落库字符串只有一处；库层白名单是同一组取值，不另立判据。同一件事判两遍，漂移的表现是同一笔成本被记成两种来源。
- **Driver 只报它看到的事实**：`ProviderSuccess` 带成本报告；`PreparedImageRequest` 带受理时冻结的**渠道声明币种**（上游报的金额不带币种，Driver 拿不到"这个数是什么钱"，也不假定任何币种）。APIMart 从任务终态读出 `cost`，按十进制**精确**换成微单位（不经过浮点乘 1e6：钱差 1 微单位就是对不上账）；AIHubMix 明确报"这条渠道不给金额字段"。`credits_cost` 仍不采纳。
- **币种的权威分来源**：`declared` 的币种取**上游随金额报回的那一份**，`computed` 的币种取**渠道声明的成本币种**（两者实践中同源，但"以哪一份为准"只有一个答案）。渠道声明的成本币种只有一个取值点，受理、执行、落账三处共用它，不各拼一遍字段链。
- **自算成本与对客扣费分开成两个入口**：同一个算式，读各自的费率——对客扣费读对客费率快照，成本自算读该渠道的**成本费率**。今天价格计划表暂时兼作成本费率，两条路算出来的数相同；但对客费率一旦拆成自己的 CNY 向量，复用对客金额会让**成本跟着售价漂移**。用例把两份费率人为设成不同值，钉住这一点。
- **落库四列**（`generation.attempts`）：`provider_cost_microusd`（原币种微单位）、`provider_cost_currency`（该渠道声明的币种）、`provider_cost_source`、`provider_cost_cny_microusd`（折算后 CNY，毛利用）。库层约束把"不猜"钉住：有金额的两种来源必须有金额与币种，`unavailable` 必须两者都为空，**来源为空时四列全为空**（失败的执行不许只填一半）；另有非负、来源取值白名单，以及一条按来源筛缺口的**部分索引**。后一支"来源为空时四列全为空"要**显式判 `provider_cost_source IS NOT NULL`**：`CHECK` 的表达式求值为 NULL 时算通过（只有 FALSE 才拒绝），而来源为 NULL 时 `provider_cost_source IN (...)` 求值就是 NULL，半填的行会从 `FALSE OR NULL` 里漏过去——这是验证阶段实测出来的，已修正并回归。
- **折算列先落、值先留空**：折算要用受理时冻结的汇率，而汇率表与快照里的汇率位（连同它的定点分母）属定价切片。本片**不自己发明一个分母**，所以快照里没有那份汇率（或币种与它对不上）时这一列留空——它是"没有折算值"，不是 0。
- **失败件也带成本事实**：Driver 在终态之后判定失败时，把已经读到的成本随错误一起交回平台（错误上的成本位与 `trace_id` 一样"只增不改"），Worker 的失败分支与成功路径**共用同一处映射**落四列。失败件没有成本事实时落 `unavailable`——来源可辨、进缺口清单，而不是四列留空：留空会让这笔成本在账上与缺口两头都看不见。
- **失败件上算不出自算成本**：`computed` 要**本次执行证据**（按 token 计量量要实际用量、按张要产出的张数），而失败件手里只有 Driver 报回来的成本事实、没有这些，所以它一律按 `unavailable` 落（缺口可见，不猜一个数）。只有**请求根本没交到渠道**的执行（取不到凭证、装不起 Driver、参数被挡在请求之外）才四列留空，那里的空值是"根本没采"。
- **币种按渠道声明接受**：去掉 `currency != "USD"` 的硬校验，只要求非空；「该币种在汇率表里有折算率」这条校验随汇率表落地时才生效。`pricing.price_plans.currency` 保留为**成本侧历史字段**，币种权威是供给声明的那个值。
- **成本与计量分开**：成本只进毛利口径，不改对客金额、也不进 `metering_evidence`。端到端用例在同一个请求里同时断言"上游声明的 11354 落了库"与"对客实收仍是 5950"。

## 备选方案

- **可选金额 vs 三态**：见上；三态必须分开，否则 `unavailable` 落不下去、自算会顶替声明。
- **Driver 报金额时带币种 vs 平台侧自己配对**：币种权威是供给声明，但金额本身不带币种。把受理时冻结的那份声明交给 Driver、由它随金额一起报回来，落库处就只有一个来源——不必在平台侧再配一次，也不会两处各判一遍。
- **复用对客扣费当成本 vs 成本侧另起一路自算**：另起一路。今天两者数值相同只是"价格计划表暂时兼作成本费率"的巧合，复用会让成本跟着售价走；两个入口分开后，对客费率拆出去时成本侧不用改。
- **浮点换算 vs 十进制精确换算**：`0.011354 × 1e6` 在浮点下会落在 11354.000000000002 这类值上，选精确换算并按第 7 位四舍五入。
- **金额解析只认数字 vs 也认字符串与指数写法**：也认。这是**容忍上游的表示差异**（同一家的响应形状会随版本变），不是行为承诺——读不出来照样按"没拿到"处理。
- **失败件写 `unavailable` vs 留空**：写 `unavailable`。留空看着更"诚实"（确实没采到），代价是这笔成本从账上与缺口清单里一起消失——而"去核上游账单"正是缺口清单要承载的处置。
- **把 `cost` 也塞进计量证据**：不做。计量事实仍是上游给的分项 token，金额不替代它（该形态的候选决策早已被否决退役）。

## 验证

| 行为 | 证据 |
| --- | --- |
| 上游直接声明金额 ⇒ `declared`，**直接取它**（实测样例 11354）、币种按声明 | `apimart_driver_executes_the_task_flow_against_a_local_upstream` |
| 渠道不给金额字段 ⇒ `computed`，按登记形态自算（14 文本输入 × 5 + 196 图像输出 × 30 = 5950） | `a_cost_the_channel_never_reports_is_computed_from_the_actual_usage` |
| 按张 / 按次计价时成本 = 数量 × 单价，且不吃 token 用量；形态是"上游给金额"而上游没给 ⇒ 缺口，不猜 | `a_supply_priced_per_image_or_per_call_computes_from_its_unit_price`（`crates/application`） |
| 渠道直接给金额的供给可以没有 Price Plan：发布成功、受理照常、成本取上游声明的金额；对客实收仍按它自己发布的对客费率向量算 | `an_upstream_declared_supply_needs_no_rate_card_and_takes_the_upstream_amount` |
| 没有对客计费基准（既无对客费率向量、又无价目表费率）的供给在发布期被拒（400） | `publication_requires_a_pricing_formula_that_matches_its_parameters` |
| 声明了却拿不到 ⇒ `unavailable`，金额与币种留空、缺口按来源查得出来 | `a_declared_cost_that_never_arrives_is_recorded_as_a_gap_not_guessed` |
| 缺字段 / 负数 / 非数字 / 超范围一律读不出金额；十进制换算逐位准确 | `a_cost_it_cannot_read_is_never_guessed`、`decimal_amounts_convert_to_micro_units_exactly`（`crates/adapter-apimart`） |
| 币种非 USD 的供给不再被硬拒，落库币种是声明值 | `a_channel_declared_currency_other_than_usd_is_accepted_and_recorded` |
| 采集成本**不改对客金额**（实收仍是费率 × 实际分项 token） | 上面两条端到端用例里的 `captured_microusd` 断言 |
| 成本自算读**渠道成本费率**、对客扣费读对客费率，两份费率不同时互不顶替 | `computed_cost_reads_the_channel_cost_rates_not_the_consumer_charge`（`crates/domain`）、`the_recorded_computed_cost_is_not_the_consumer_charge`（`crates/application`） |
| 来源判定的三态映射与折算值留空 | `provider_cost_source_follows_where_the_cost_came_from`（`crates/application`）、`worker_settles_the_provider_image_envelope_with_the_evidence` |
| 终态给了金额、这次没有结果图 ⇒ 失败件照样按 `declared` 落四列（金额取声明值、币种按渠道声明） | `a_terminal_without_images_still_records_the_cost_it_already_declared`（`apps/api`）、`a_terminal_without_images_still_carries_the_cost_it_declared`（`crates/adapter-apimart`）、`a_failure_carries_the_cost_the_adapter_already_read`、`worker_records_the_cost_fact_the_adapter_reported_on_failure`（`crates/application`） |
| 终态没有金额、也没有结果图 ⇒ `unavailable` 且这笔进成本缺口清单；请求根本没交到渠道的执行仍四列留空 | `a_terminal_without_images_or_amount_lands_in_the_cost_gap_list`（`apps/api`）、`adapter_failures_keep_the_channel_code_internal`、`worker_sends_ambiguous_provider_response_to_reconciliation_once`（`crates/application`） |
| 渠道不给金额字段的失败件 ⇒ Driver 报 `computed`，平台手里没有本次用量时按缺口落 | `a_response_without_images_still_reports_where_the_cost_comes_from`（`crates/adapter-aihubmix`）、`provider_cost_source_follows_where_the_cost_came_from`（`crates/application`） |
| 落库字符串与库层 CHECK 取值自洽 | `provider_cost_sources_round_trip_through_their_stored_form`（`crates/domain`） |

## 后果

- **成本只留痕，不进账本**：成功件与失败件的成本都只落在执行尝试的四列上；成本进账本与缺口补录归账实核对那条线。
- **缺口只"查得出来"，没有告警**：按来源筛得到，但不会主动通知运营；毛利侧标"成本未知"。
- **请求根本没交到渠道的执行四列留 NULL**：那是"根本没采"，不是漏写（见上）。

## 依据与关联

- 设计：成本来源三态（`computed` / `declared` / `unavailable`）、落地边界与"成本来源可辨"、两个币种平面、六项信息的落点见 [`docs/design/0007`](../../../../docs/design/0007-pricing-floor-and-settlement.md) §1/§7/§8/§9。
- 决策：成本按渠道各自口径取数、金额不替代计量事实、缺字段不得猜测费用见 [`ADR-0006`](../../../../docs/adr/0006-no-settlement-without-metering-evidence.md)；PostgreSQL 是业务事实权威见 [`ADR-0003`](../../../../docs/adr/0003-postgresql-is-source-of-truth.md)。
- 事实台账：AIHubMix 无金额字段与四档费率见 [`docs/facts/channel-facts.md`](../../../../docs/facts/channel-facts.md) 的 AIHubMix 费率节；APIMart 终态 `cost` 与实测样例见同文件的 APIMart 计量节。
- 迁移：[`0008_attempt_provider_cost.sql`](../../../../migrations/0008_attempt_provider_cost.sql)。
- 索引同步：[`docs/architecture.md`](../../../../docs/architecture.md) §4（一次请求怎么走）、§5（表）、§6（文件与迁移）；词汇表 [`CONTEXT.md`](../../../../CONTEXT.md) 新增 `Provider Cost`。
- 相邻记录：[网关模型命名层](../../implemented/platform/2026-09-22-gateway-model-naming.md)（本片不改命名层，只把成本事实落在执行尝试上）。
- 工作项：提案 [#13](https://github.com/dehuadong/seeaihub-server-next/issues/13) 的 P2a 工单 [#15](https://github.com/dehuadong/seeaihub-server-next/issues/15)。
