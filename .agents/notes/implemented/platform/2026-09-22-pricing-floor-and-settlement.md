---
title: 定价、保底与结算：对客费率向量、汇率表、保底表与透支
status: implemented
created: 2026-09-22
updated: 2026-09-23
approval: 用户在会话中授权实施 P2b（定价、保底与结算）；范围与验收见提案 [#13](https://github.com/dehuadong/seeaihub-server-next/issues/13) 的 P2b 工单 [#16](https://github.com/dehuadong/seeaihub-server-next/issues/16)
verification: 验收合同为工单 [#16](https://github.com/dehuadong/seeaihub-server-next/issues/16) 的逐条可勾选验收清单（依据 `docs/design/0007` §2–§9）。**全部离线**（真实空库 + 真实 API/Worker 进程 + 本机假上游，零真实计费调用、零外网）。Verify 阶段另起**独立于实现用例**的端到端驱动（自建 Node 驱动直打对客 HTTP + 直查库，不复用仓库测试的断言）：56 项检查 **55 PASS**，唯一未过项是「上游终态给了金额但结果为空」那条**适配器**路径（不属于本条验收条目所指的「结算失败进对账」路径；已转工单 [#17](https://github.com/dehuadong/seeaihub-server-next/issues/17)，见「后果」）；结算失败进对账那条路径另行预埋撞 `business_key` 的 `capture` 分录实测（Job 进对账、四列落库）；增量迁移在旧库（只应用 0001–0008 + 旧数据）上实测 8 项全过。门禁：`cargo fmt --all --check` exit 0；`cargo clippy --workspace --all-targets --all-features -- -D warnings` exit 0；`cargo test --workspace --all-features` 全绿（22 / 32 / 2 / 3 / 62 / 59 各 crate 单测通过，55 条端到端按设计 ignore）；`node scripts/decisions/check.mjs` 通过。`cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1`：Verify 阶段一次性整跑曾因容器时钟漂移不稳定（同一份代码分别跑出 34/54、17/54、24/54、11/54 通过，失败信息全是发布期"没有已生效的折算率"）；**收口时已修**（"立即生效"的折算率改由数据库盖章，见「后果」），修后**连续三次整跑均 55 passed / 0 failed**（54 条原有 + 1 条新增的盖章钉桩用例）。**2026-09-23 把对客价补齐到四种计价形态**（成本单价 × 倍率 × 折算率，见「决定」）后复跑四项门禁：`cargo fmt --all --check` exit 0、`cargo clippy --workspace --all-targets --all-features -- -D warnings` exit 0、`cargo test --workspace --all-features` 全绿、`node scripts/decisions/check.mjs` 通过、`cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1` **75 passed / 0 failed**。逐条证据见正文「验证」一节。
---

# Agent Note：定价、保底与结算：对客费率向量、汇率表、保底表与透支

## 问题

对客扣费此前**就是渠道结算基数**（按已发布费率 × 真实 token）：既没有平台自己的 CNY 售价，也没有加价与汇率折算，卖出去等于原价转手；预授权是服务端一个固定数（`GENERATION_MAX_COST_MICROUSD`，默认 $0.02），实际费用超过它就进对账——一笔正常完成的生成被扣在对账里；上游成本虽然上一片已经采集（[渠道成本事实采集](../../implemented/platform/2026-09-22-provider-cost-facts.md)），但折算列恒为 NULL，**毛利算不出来**。

本片把这条线补齐：售价按**候选**发布并对客冻结、汇率按币种维护并在受理时快照、预授权按**供给维度**的保底表查得、结算**按实际扣且允许透支**、成本用冻结的汇率折成人民币供算毛利。范围与逐条验收见工单 [#16](https://github.com/dehuadong/seeaihub-server-next/issues/16)。

## 决定

- **对客只有 CNY 单币种**：售价是**按候选发布的对客四档 CNY 费率向量**（`runtime_revisions.consumer_rates_cny`，按候选键的 jsonb 映射），受理时随 Job 的 Price Snapshot 冻结，结算只读它。`reference_cost_microusd` **只作定价参考，不是售价的被乘数**——单值推不出四档向量，而且实际金额要等上游回来才知道。**同一网关模型的不同候选价格不同**。
- **加价系数 `markup_bps` 是修订级**（每个网关模型一个），随修订发布、随快照冻结；它**参与**"这个网关模型的价是怎么定的"（管理员按"成本单价 × 倍率 × 折算率"推导对客费率向量），而且**在按 token 计量量的候选上可以缺省**——管理员直接录入对客费率向量时它一次都不参与计算。发布期拒绝两件自相矛盾的事：负加价，以及给了加价系数却没有任何候选带定价或按它算价。**按张 / 按次 / 上游给金额的候选反过来必须给**（见下一条）。数值由后台录入，不是设计决策。
- **汇率是外部事实**：`pricing.fx_rates`（按币种的"渠道币种 → CNY"定点比值 + 生效时间，分母 1e6，不用浮点），由管理员经 `PUT /api/v1/fx-rates` 录入（写审计；同一币种同一生效时刻只有一行，重录即改那一行）。**受理时按候选的成本币种取"受理时刻生效的那一行"并原值快照**，受理之后不再换算。**发布期校验每个候选声明的币种有一行已生效的折算率**，否则整份发布被拒——受理时取不到汇率就算不出成本，而那时拒的是消费者的请求。
- **保底表按供给维度**：`runtime_revisions.floor_amounts`（按候选键），供给内按 `(size, quality)` 给额，`quality` 留空即按 `size` 档。**像素型的 `size` 先归到档位**（设计 §6 本轮补的那一段；用户口径：「OpenAI vendor 系列模型可以按分辨率保底、**通过 `size` 判断 1K/2K/4K**」）：① 优先按该供给发布的**档位像素表**（就是它的尺寸档案：档位 → 比例 → 像素，随修订发布、随 Job 冻结）反向查出档位——各供给的档位像素不同，只有它自己声明的那张表才是它的档位定义；② 该表缺失、或表里没有那一格时按**最长边**阈值兜底（≤1024 → `1K`、≤2048 → `2K`、>2048 → `4K`；依据是四档的"K"本来就按长边命名、厂商文档里的档位像素表也按长边分档，**这是兜底不是精确口径**）；③ `size = auto`（**或 `size` 字段缺失**）取**默认档 `2K`**（中间档：估太小会让结算频繁透支，取最大档会把每一次没钉尺寸的请求都按最贵的一档冻住）；**空串不是"没给"**——它是"给了个空"，照字面归档位（归不出即回落），平台不替它 trim、也不把它悄悄换成默认档；④ 归不出档位（比例型只说了形状、空串、纯空白、认不出的取值）、或该档位在表里没有 ⇒ 回落该供给**封顶保底值**，再回落平台兜底数。查表结果连同**来源**（`tier` / `auto_tier` / `supply_cap` / `platform_default`）随快照冻结，事后分得清"这次为什么冻这么多"。档位键按尺寸取值规范化（`2k` 与 `2K` 同一个档），规范化后撞档直接拒绝（同一档两个保底额，查出来的数就不确定了）。
  > **本轮对设计 §6 的修订**：原文写的是"`size = auto` ⇒ 取该供给保底表里的**最大档**"，本轮按用户口径改成"取**默认档 2K**"，并把像素→档位的归位规则补成上面那一条（§2 的汇率行、§3 的 `hold_source` 注释、§6 的兜底链与 §10 的已定案同步改写）。这是**澄清用户已表达的口径**，不是新决策。
  > 同轮另有两处设计同步：§7/§9.2 写明**成本缺口用只读清单承载**（不开对账案例、不推进对账态，且它是 §9.2「不开查询接口」的唯一例外）；§2 的汇率行写明**发布期币种校验的判据是"候选声明的币种"、与带不带定价无关**，并如实记下后果（迁移后重发旧素材前，该素材声明的币种必须先有折算率）。
- **预授权只是保底**：hold = 查表得到的保底额（**不由售价派生**），受理闸门是**余额 ≥ 保底额**（不足即 402）；**结算按实际扣、不封顶在保底额**，实收超过保底额时余额被扣成负数——**透支发生在结算**，随后按当时余额判。`GENERATION_MAX_COST_MICROUSD` 退为"连该供给封顶保底值都没有时"的兜底保底额，不再是任何形式的上限（查得到保底额时它一次都不读）。
- **毛利 = 售价（CNY）− 成本折算后 CNY**：折算用快照里的汇率，**币种对不上就不折**（留空，不拿另一个币种的汇率去乘）。两条线分开留痕：售价/保底/扣费记 CNY（账本是权威），成本记原币种原值 + 币种 + 当时汇率 + 折算后 CNY。
- **旧修订走旧口径**：迁移后的旧修订定价列留 NULL，受理出的 Job 快照不带对客费率向量与保底额 ⇒ 对客扣费按已发布费率、预授权回落平台兜底数，与今天逐位相同；历史 `price_snapshot` 仍能解析（新字段都带 `#[serde(default)]`）。
- **Price Plan 不是每条供给必填**：它只是"按 token 计量量计价"这一种**计价形态**的参数，形态落在供给上（`supply.offerings.formula` + 按张/按次的 `cost_unit_price_microusd`），因此 `publication.runtime_entries.price_plan_id` 可空（`migrations/0013_offering_pricing_formula.sql`）。形态必填且与参数配套（按 token 计量量缺那份四档费率、按张/按次缺单价，发布期 400），对客定价那一条维不变。
- **对客价只有一条乘法：成本单价 × 倍率 × 折算率**（`markup_bps` 是倍率的基点表示），四种计价形态共用它——形态只决定**成本单价是几个数**（四档 token 费率 / 每张单价 / 每次单价 / 上游这次声明的金额）。**按 token 计量量的候选**的对客价是随修订发布的那份四档 CNY 向量（`consumer_rates_cny`，管理员按上式推导或直接录入），受理时随快照冻结；**其余三种形态没有这个载体**，结算按快照里冻结的成本单价（上游给金额时是这次声明的金额）× 冻结的倍率 × 冻结的折算率算出对客价，再乘本次实际量（产出张数 / 1 次）。倍率与折算率都取自快照，所以受理之后改价、改汇率都不影响已受理的 Job（`ADR-0003`）。
- **折算率按该供给声明的成本币种取，不假定 USD**：`pricing.fx_rates` 里**同币种也录一行**（例如 `CNY → CNY`），那一行的率恒为 1、折出来逐位不变——代码里没有"这个币种不用折算"的分支，同币种渠道（例如按张计价的人民币渠道）因此发布得出去、且不产生折算。
- **实收的取值链**：按 token 计量量时是命中候选的对客费率向量，或没有它时的旧口径（已发布费率兼作对客费率）；其余三种形态由成本单价乘倍率算出。**算不出对客价 = 这条供给没有对客计费基准**：**发布期拒**（按 token 计量量缺向量与费率、其余形态缺倍率 / 单价，或对客四档向量落在非 token 形态上——那份向量在那些形态下永远不会被读）；运行时真遇到（只有历史修订才可能，或缺倍率 / 折算率 / 单价 / 上游声明的金额）按**平台侧故障**处置，**不按 0 结算**（0 元等于白送，还会在账上留下一条"收过钱"的 0 元记录）。**折算率按"这条供给声明了成本币种"冻结**（每条新发布的供给都声明它，没有 Price Plan 时显式声明）：上游声明的金额与按形态自算的金额都要折成人民币才算得出毛利；旧修订受理出的历史 Job 快照里没有这条声明，那时不冻结。
- **`charge_microusd` 投影列不做**：账本已经查得到，它只是查询便利，按设计缓做。
- **本轮内部整理**（不改行为、不改响应形状）：`ConsumerRatesCny` 的四档字段名去掉 `usd`（这个载体只有 CNY，与 `FxRate::rate_micros` 同一条口径）；**成本缺口清单移到 `PricingService`**（它不是对账案例，挂在 `ReconciliationService` 上会让那个服务因为两种不相干的理由被改）；运营清单的条数上限收成 `MAX_OPERATIONAL_LIMIT` **一处**、只在 HTTP 层 clamp 一次（两处各 clamp 一次时，响应里的 `truncated` 会与实际返回条数对不上）；`size` / `quality` 的字面量读取复用领域那份判据（`literal_parameter_text`，与 `is_used_parameter_value` 的差别写在它的注释里），应用层不再自写一套空值约定；四档费率的算式改收一个费率结构体，不再按位置传四个 `u64`（按位置传时把两档写反了编译器不会吭声）。

### 承接上一片（[#15](https://github.com/dehuadong/seeaihub-server-next/issues/15)）留下的两个执行单元

1. **对账路径的成本事实落库**：结果交付失败进对账那条路径上执行已经发生、上游成本也拿得到，但此前只有成功路径才写成本列。现在 `AttemptFailure` 带上成本事实，`fail_job` 把它与失败事实一起写下来（`provider_cost_microusd` / `provider_cost_currency` / `provider_cost_source` / `provider_cost_cny_microusd`）；适配器失败分支一律给出成本事实（Driver 没报就按 `unavailable`），只有"请求根本没交到渠道"的执行才四列留空——那是"这次没有成本事实可落"，与"成本是 0"不是一回事。
2. **成本缺口的处置**：`unavailable` 那笔**不开对账案例、也不把 Job 推进对账态**——对账态是"受理/执行状态不明"，会把消费者的钱扣在对账里；成本缺口是平台侧的账务缺口，对客结算照常完成。它由**成本缺口清单**（`GET /api/v1/provider-cost-gaps`，带上游对账标识，仅管理员）交给运营核账单，毛利侧标"成本未知"（金额与折算值留空）。**补录归账实核对那条线**（工单 [#11](https://github.com/dehuadong/seeaihub-server-next/issues/11)），补录完成后这一笔不再出现在清单里（清单的判据就是"来源是 `unavailable`"）。

### 迁移

增量迁移 `migrations/0009_pricing_floor_and_settlement.sql`：新建 `pricing.fx_rates`（**不预置任何数值**；同币种同生效时刻唯一 + 取值索引）；`publication.runtime_revisions` 增七个可空定价列（`markup_bps` + 六个按候选键的 jsonb 映射，`markup_bps` 另加非负约束），**不回填**；**放宽三处 CHECK**——`ledger.accounts.balance_microusd` 去掉非负约束（透支要能把余额扣成负数），`ledger.holds.amount_microusd` 与 `generation.jobs.max_cost_microusd` 由 `> 0` 改 `>= 0`（保底额可为 0）。只放宽、不收紧。

## 备选方案

- **定价按候选 vs 一个单值乘出来**：按候选。售价是四档向量，单值推不出向量；而且售价按候选算，向量就必须按候选发布——一个网关模型一个价表达不出"不同候选不同价"。
- **汇率放进每份发布 vs 按币种维护一张表**：按币种维护。汇率是外部事实，同一时刻同一币种全平台必须是同一个数才对账得起来；放进发布里改一次汇率要重发所有型号。
- **发布期校验只针对带定价的候选 vs 按候选声明的币种一律校验**：一律校验。币种是这条供给的成本口径，有没有定价不该让同一个币种在"能不能发布"这件事上前后不一致。
- **快照里把定价打包成一个可空子对象 vs 平铺可空字段**：平铺。设计与验收都按平铺的字段面写（`consumer_rates_cny` / `hold_microusd` / `fx_rate` …），"缺对客费率向量与保底额 ⇒ 旧口径"这条规则也因此一眼可读。
- **`size` 的"字面量"与"没给"怎么分**：只有**字段缺失**或字面 `auto` 取默认档 `2K`；**空串算"给了个空"**，照字面归档位（归不出即回落封顶值）。字面量就是调用方说的那个尺寸，平台不 trim、不认别名，也不把"写了但写歪了"悄悄换成默认档——那会把一次写错的请求变成一次按中间档冻结的正常请求。取**中间档**而不是最大档：估太小会让结算频繁透支，取最大档会把每一次没钉尺寸的请求都按最贵的一档冻住。
- **像素→档位先查该供给的档位像素表 vs 一律用最长边阈值**：先查表。各供给的档位像素不同（同一个"1K"在不同模型上可能是 1024 或 1536），只有它自己声明的那张表才是它的档位定义；最长边阈值是**兜底**（表缺失或没有那一格时），依据是"K"本来就按长边命名、厂商文档的档位像素表也按长边分档。复用**已发布的尺寸档案**而不是新加一列"档位像素表"：那份档案本来就是这条供给的档位定义，且已经随 Job 冻结，再加一列就是同一件事存两份。
- **比例型 `size` 归不出档位**：只说了形状、没说分辨率，按封顶保底值处理——不猜一个档位。
- **折算币种对不上时留空 vs 用快照汇率硬乘**：留空。拿另一个币种的汇率去乘就是编数，而"编一个数"比"承认折算不出来"糟得多。
- **结算超过保底额进对账 vs 透支**：透支。预授权只是保底，实际多少就扣多少；把正常完成的生成扣在对账里，消费者要等一个本不该有的人工结论。
- **成本缺口开对账案例 vs 单独的运营清单**：单独的清单。对账案例的处置路径是退款，而成本缺口没有任何东西可退（对客结算已经完成）；开案例会把两件不同的事混成一个待办。
- **定价随单条供给发布 vs 随候选数组发布**：随候选数组——一次只发一条供给的价时，"同一个网关模型的不同候选各有各的成本"这件事表达不出来。
- **自算失败记 `unavailable` vs 让整个结算失败**：记 `unavailable`。用量自相矛盾或溢出时，本该有金额却算不出来——那也是缺口，用别的数顶替才是错的。
- **非 token 形态的对客价：从发布数据现算 vs 新加一个"每张 / 每次对客单价"的发布字段**：现算（成本单价 × 冻结的倍率 × 冻结的折算率）。加字段要给发布数据与快照加载体、动迁移，而且运营得同时维护"成本单价"和"对客单价"两个数，两者不一致时谁说了算没有答案；现算只有一条乘法，输入全都随快照冻结，结果与"发布时就把价算好冻住"逐位相同。

## 后果

- **成本缺口只"看得见"，没有告警**：运营要主动查清单；缺口进账本与账实核对、补录结果回填归工单 [#11](https://github.com/dehuadong/seeaihub-server-next/issues/11)。
- **上游声明的币种与快照的成本币种不一致时留空**：这是有意的取舍（见上），代价是那一笔毛利算不出来、会落在"成本未知"一侧。
- **最长边兜底是"兜底"不是精确口径**：它只在**该供给没发布档位像素表、或表里没有那一格**时生效。想让某个像素落到某个档位，正确做法是给这条供给发布尺寸档案，而不是依赖阈值。
- **比例型 `size` 归不出档位**：它只说了形状、没说分辨率，按封顶保底值处理（不猜一个档位）；比例型与档位组合的合同（如"比例 + 档位"两字段）走的是尺寸换算那条线，不在这里。
- **`markup_bps` 与对客费率向量的推导关系不校验**：管理员按"成本单价 × 倍率 × 折算率"**设定/推导**，也可以直接录入；平台不反算校验（成本单价与折算率都随发布与时间变，反算出来的数不等于管理员该录入的数）。它只是"这个价是怎么定的"的可查依据。
- **非 token 形态的对客价只能靠倍率或成本单价调**：按张 / 按次 / 上游给金额的候选没有对客价载体，要给它们一个与成本无关的价就得先加一个载体（动发布数据与迁移）。这是"只有一条乘法"的直接代价。
- **汇率行删了会让受理失败**：发布期保证了"有已生效的一行"，所以受理时取不到只可能是汇率表被改过——那时按平台侧配置问题处置（不是这次请求的问题），这一点写在代码注释里。
- **上游终态给了金额、但结果为空时，成本事实由适配器随错误带回**（Verify 阶段实测为丢失，随后修掉）：`adapter-apimart` 的 `finish` 与 `adapter-aihubmix` 的 `parse_response` 在终态之后判定失败时，把已经读到的成本附在 `ProviderCallError` 上带回平台；Worker 的失败分支与成功路径**共用同一套映射**落四列，失败件同样能落 `declared` / `unavailable` 并进缺口清单（细节见[渠道成本事实采集](../../implemented/platform/2026-09-22-provider-cost-facts.md)）。它不是本片验收条目所指的路径（那条是"结算失败进对账"，即 `complete_success` 出错，本片已实测落四列）。
- **"立即生效"的折算率曾由应用时钟盖章、由数据库时钟判生效**（Verify 阶段实测；**收口时已修**）：`PUT /api/v1/fx-rates` 不给 `effective_at` 时原由 API 用 `Utc::now()` 写入，而发布期校验与受理取值都用库里的 `now()` 比。两个时钟不一致时，**刚录入的那一行会被判成"尚未生效"**：本机（Docker Desktop / WSL2）实测容器时钟相对宿主呈锯齿漂移——约 **-100ms/s** 倒退、每 ~30 秒回跳一次，幅度在 **-1.6s ~ +1.4s** 之间；偏差为负时"录完立刻发布"**15/15 被拒**（错误体是"没有已生效的折算率"）、"录完立刻受理"仍按旧汇率折算；偏差为正时同一序列 **0/15 失败**。它直接打穿了本片验收的门禁：`http_contract -- --ignored --test-threads=1` 一次性整跑在同一份代码上 **20–43/54 失败**（全部卡在夹具"启动时落折算率、随后立刻发布"这一步）。**修法（已落地）**：把"未指定生效时间"这一情形交给数据库盖章——写入口的生效时刻收成 `Option<DateTime<Utc>>`（`NewFxRate`，端口不再要求调用方先读一个时钟），SQL 写 `coalesce($4, now())` 并 `RETURNING effective_at`（审计记的是真正落库的时刻），API 侧删掉 `unwrap_or_else(Utc::now)`。**钱与生效时刻只认一个时钟**。钉桩用例 `an_fx_rate_without_an_effective_time_is_stamped_by_the_database_clock`：不指定生效时刻录一行后，直查库断言这一行的 `effective_at` 与**同一事务里那条审计事件的库侧 `created_at` 逐位相同**（两者都取该事务的 `now()`，若改回进程时钟盖章就必然差出漂移），并断言库里没有一行 `effective_at > now()`。**本条不是产品语义错**（"取受理时刻生效的那一行"这条规则本身实测正确：未来行不参与取值）。同型检查：API 侧"进程盖章、库里比较"**仅此一处**；`crates/persistence` 里租约到期时刻（`lease_expires_at = Utc::now() + lease_duration`）是同型写法，但它不在钱与生效时刻这条线上、量级（数百秒租约对秒级漂移）也无害，未在本片改动。

## 验证

验收按工单 [#16](https://github.com/dehuadong/seeaihub-server-next/issues/16) 的逐条可勾选清单在**空库 + 真实 API 进程 + 真实 Worker 进程 + 本机假上游**上跑（离线、零计费调用），下表的用例名是仓库端到端套件里承担该条的用例；**Verify 阶段另跑了一遍独立驱动**（不复用这些用例的断言，只走对客 HTTP 并直查库），逐条结论与下表一致，额外证据见本节末尾。

| 验收（工单 [#16](https://github.com/dehuadong/seeaihub-server-next/issues/16)） | 证据 |
| --- | --- |
| 对客费率向量与后台设定逐位一致；`hit_candidate` 记实际命中的候选；**命中不同候选售价不同** | `pricing_is_frozen_into_the_job_and_settlement_only_reads_that_snapshot`（快照里的向量与发布值逐位相等、`hit_candidate.offering_id` 等于 Job 的 `offering_id`、重发后新 Job 用新价）、`the_charge_follows_the_hit_candidate_and_ignores_the_reference_cost`（两个候选各带一份向量，用承载面差异把请求逼到优先级 1 的那条，实收按**命中候选**那份算） |
| **参考成本只作定价参考、不是售价的被乘数**：只改 `reference_cost_microusd` 重发 → 对客 `charge` **只随 `consumer_rates_cny` 变** | `the_charge_follows_the_hit_candidate_and_ignores_the_reference_cost`（重发后快照里的参考成本已换成新值，实收仍逐位不变） |
| 保底按供给维度查表；缺档回落供给封顶值；再回落平台兜底 | `the_hold_resolves_the_tier_then_walks_the_supply_floor_chain`、`an_unpriced_revision_and_an_empty_floor_table_fall_back_to_the_platform_default` |
| **像素型 `size` 归到档位后按该档查保底**：`1024x1024` → 1K（¥0.16）、`2048x2048` → 2K（¥0.25）、`3840x2160` → 4K（¥0.3） | `the_hold_resolves_the_tier_then_walks_the_supply_floor_chain`（最长边兜底路径）、`pixel_sizes_resolve_to_a_tier_before_the_lookup`（`crates/domain`） |
| **该供给发布的档位像素表优先于最长边兜底**（同一像素在别的供给上可能属于别的档位） | `the_supply_size_profile_wins_over_the_longest_edge_fallback`（`crates/domain`） |
| `size = auto`（或 `size` 字段缺失）取默认档 2K；**空串不是"没给"**（照字面归档位、归不出即回落） | `the_hold_resolves_the_tier_then_walks_the_supply_floor_chain`（`auto`、"没给 size"、空串各一条断言，来源分别记 `auto_tier` 与 `supply_cap`）、`auto_and_a_missing_size_take_the_default_tier` 与 `an_empty_or_padded_size_is_given_and_is_not_the_missing_size`（`crates/domain`） |
| 比例型 `size` 归不出档位 ⇒ 回落供给封顶值 | `the_hold_resolves_the_tier_then_walks_the_supply_floor_chain`（`16:9`）、`floor_lookup_falls_back_to_the_supply_cap_when_the_tier_cannot_be_resolved`（`crates/domain`） |
| `quality` 维留空即按 `size` 档 | 同上（`low` / `high` / `xhigh` / `auto` 都查到同一个 `2K` 档） |
| 透支：结算把余额扣成负数，随后同一账户再发请求 402、不产生 Job、不扣款 | `an_overdraft_settles_into_a_negative_balance_and_the_next_request_is_refused` |
| 透支撞库约束已放宽（余额可为负、保底额可为 0） | `the_pricing_migration_relaxes_the_balance_checks_on_an_existing_database`（负余额、`max_cost_microusd = 0`、`holds.amount_microusd = 0` 三处直写成功） |
| 汇率按受理时刻生效的那一行取值；受理后新增/改动不影响已受理 Job | `the_rate_effective_at_acceptance_is_frozen_and_a_missing_rate_blocks_publication` |
| **不指定生效时刻时，落库的生效时刻由数据库决定**（不由 API 进程时钟盖章） | `an_fx_rate_without_an_effective_time_is_stamped_by_the_database_clock`（该行 `effective_at` 与同一事务里那条审计事件的库侧 `created_at` 逐位相同；库里没有 `effective_at > now()` 的行） |
| 发布期拒绝没有折算率的币种，整份发布不落任何行 | 同上（`EUR` 发布得到 400，`runtime_revisions` 里没有它的行） |
| 折算值落地；币种对不上时留空 | `a_declared_cost_is_taken_as_is_and_converted_with_the_frozen_rate`（11354 微美元 × 7.1 ⇒ 80614 微元）、`the_cost_is_converted_with_the_frozen_rate_of_its_own_currency`（`crates/application`：币种对不上 ⇒ `None`） |
| 实收按实际、不封顶；实收低于保底额时差额释放 | `pricing_is_frozen_into_the_job_and_settlement_only_reads_that_snapshot`（余额 = 初始 − 实收） |
| 旧修订 + 新 Job 走旧口径；历史快照逐位不变 | `an_unpriced_revision_and_an_empty_floor_table_fall_back_to_the_platform_default`、`a_snapshot_without_the_pricing_keys_still_parses`（`crates/domain`） |
| 售价不受固定数限制（保底额 > 平台兜底数照常受理） | `pricing_is_frozen_into_the_job_and_settlement_only_reads_that_snapshot`（保底额 250000 ≫ 平台兜底数 20000，照常受理） |
| 管理员读列出每个候选的定价与修订级加价系数 | `the_admin_view_lists_the_published_pricing` |
| 发布期校验计价形态：必填、取值受控、与参数配套（按张**不带**费率表也能发布，落库与快照都带形态与单价） | `publication_requires_a_pricing_formula_that_matches_its_parameters` |
| **对客价按四种计价形态都能算出来**：按 token 读那份向量、按张读产出张数、按次读 1 次、上游给金额读它这次声明的金额；**倍率是发布数据不是常量**（同一个单价换倍率，实收成比例变） | `a_per_image_charge_follows_the_markup_coefficient`（20% 与 50% 两版：170400 与 213000，成比例；旧 Job 金额不动）、`a_per_call_charge_is_one_unit_regardless_of_the_usage`、`an_upstream_declared_supply_needs_no_rate_card_and_takes_the_upstream_amount`（对客实收 96737 = 11354 × 1.2 × 7.1，与成本折算 80614 是两个量）；领域侧 `a_per_image_supply_sells_at_its_cost_unit_price_times_the_markup`、`a_per_call_supply_charges_once_per_call`、`an_upstream_declared_supply_sells_at_the_declared_amount_times_the_markup`、`the_markup_coefficient_scales_the_charge_proportionally`、`a_derived_consumer_price_needs_all_of_its_inputs` |
| **同币种也能录折算率且不产生折算**：`CNY → CNY = 1` 录得进去，声明 `CNY` 的按张供给能发布、受理、结算 | `a_cny_supply_publishes_and_is_charged_without_conversion`（对客实收 360000 = 300000 × 1.2，折算值 300000 逐位不变）、`a_same_currency_supply_is_not_converted`（`crates/domain`） |
| 毛利可逐笔算出；来源三态可辨 | 上面两条成本用例（`computed` / `declared` 各自带 CNY 折算值）、`a_cost_gap_is_listed_for_operations_without_pushing_the_job_into_reconciliation`（`unavailable` 三样留空） |
| 成本缺口处置：不进对账态、对客结算照常、运营看得见 | 同上（`reconciliation_cases` 为 0、`captured_microusd` 照扣、缺口清单带对账标识且仅管理员可读） |
| 对账路径的成本落库 | `the_reconciliation_path_records_the_cost_fact_it_already_has`（直调仓库端口：带成本事实时四列落库、不带时四列留空）、`worker_sends_settlement_failure_to_reconciliation_with_its_own_code`（`crates/application`：失败事实里带着成本事实） |
| 历史行为不变 | 既有 43 条端到端用例逐位通过（收口修正后**连续三次整跑 55/55 全过**；修前一次性整跑因容器时钟漂移不稳，见「后果」的时钟条目） |
| 门禁 | `cargo fmt --all --check` exit 0；`cargo clippy --workspace --all-targets --all-features -- -D warnings` exit 0；`cargo test --workspace --all-features` 全绿；`node scripts/decisions/check.mjs` 通过；`cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1` **连续三次整跑 55/55 全过**（收口修正见「后果」的时钟条目） |

### Verify 阶段的独立证据（2026-09-22）

在 `seeai_p2b_verify`（全新库，迁移器应用 0001–0009）上起 `API_BIND=127.0.0.1:8091`、`ADMIN_TOKEN=replace-me`、`GENERATION_MAX_COST_MICROUSD=20000` 的 API 与真 Worker，外加自建本机假上游（同步渠道 / 任务式渠道 / 任务式无金额 / 任务式无结果四种形态），用一个只走对客 HTTP + 直查库的驱动跑 **56 项检查**：

- **汇率**：录入与未来生效行；非 USD 币种（HKD）发布成功、受理快照取"受理时刻生效的那一行"（不是未来那行）；无折算率的币种（EUR）与删掉折算率后重发都整份被拒且不落行；受理后改汇率不动已受理 Job、之后受理的 Job 用新率折算。
- **定价**：不带 `markup_bps` 直接录对客费率向量可发布；受理快照逐位等于发布值、`hit_candidate` 等于 Job 的 `offering_id`；两个候选（便宜 / 正常）用 `restrictions.allowed_branches` 差异把请求逼到第二个 → 实收按命中候选那份算（-43680，不是 -210）；只改参考成本重发实收不变；重发换价与 markup 只影响新 Job。
- **保底**：`2K` / `1024x1024` / `2048x2048` / `3840x2160` / `auto` / 缺字段 / 空串 / `16:9` / 表里没有的档位 / 带 `quality` 共 10 条，`hold_microusd` 与 `hold_source` 逐条等于设计 §6 的期望（空串落 `supply_cap` 300000，不是默认档 250000）；表里只有 1K + 封顶值 → `supply_cap`；表为空 → `platform_default` 20000；保底额 0 与保底额 5000000（≫ 平台兜底数）都照常受理。
- **闸门与透支**：余额 < 保底额 → 402 `insufficient_balance`、无 Job、无 hold、不扣款；保底额 1000 实收 43680 → 余额 -42680（`release 1000 + capture -43680`，即 `refund = 授权 − 实收` 为负）、不进对账；随后同账户再发 → 402 且不产生 Job。
- **成本与毛利**：`computed` 5950/USD/42245、`declared` 11354/USD/80614（自算是 5950，取的是上游声明）、`unavailable` 三样留空且不进对账态、缺口清单列得出来（带 `provider_trace_id` 与 `completed_at`，无凭证 401）；对客响应与快照的对客平面不含外币。
- **结算失败进对账那条路径**（本片承接的执行单元 ①）：在无 Worker 时受理、给该 Job 预埋一条撞 `business_key` 的 `capture` 分录，再起真 Worker → 结算事务失败、Job 进 `reconciliation_required`、`attempts` 四列照落（`5950 / USD / computed / 29750`）、对客回 502 `outcome_unknown`。
- **迁移（增量）**：另建库只应用 `0001`–`0008` 并造旧数据（余额为 0 的账户、`amount=1000` 的 hold、`max_cost=20000` 的 Job、没有定价的旧修订），再由真迁移器补 `0009` → 迁移记录 `1..9`、旧行逐字不变、七个定价列留 NULL 无回填、`pricing.fx_rates` 落成空表、两处旧 CHECK 名消失且新的非负约束在位、余额可写成 -1、零额 hold 与零 `max_cost` 可写入。
- **回归**：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`、`cargo test --workspace --all-features`（22 / 32 / 2 / 3 / 62 / 59 全绿，55 条端到端按设计 ignore）、`node scripts/decisions/check.mjs` 全过。`cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1` 修前**一次性整跑在本机不稳**：同一份代码分别跑出 34/54、17/54、24/54、11/54 通过，失败信息全是发布期"没有已生效的折算率"（根因见「后果」的时钟条目：夹具"启动时落折算率、随后立刻发布"要求"刚录入＝已生效"，而应用时钟盖章、数据库时钟判生效）；**收口修正后连续三次整跑均 55 passed / 0 failed**（54 条原有 + 1 条盖章钉桩用例），不再受时钟漂移摆布。

56 项里唯一未过的是"上游终态给了金额、结果为空"那条**适配器**路径（成本四列留空）——**随后修掉**：失败件与成功件同源同形地落成本四列（见[渠道成本事实采集](../../implemented/platform/2026-09-22-provider-cost-facts.md) 的「验证」）；它不是本表任一验收条目所指的路径。Verify 阶段发现的时钟盖章缺陷已在收口时修掉（见同节），修后门禁连续三次整跑全绿。

## 依据与关联

- 设计：定价公式与各量落点、售价与保底快照随 Job 冻结、扣费与命中渠道、毛利记录、预授权只是保底、成本来源可辨、两个币种平面、记录与日志的落点见 [`docs/design/0007`](../../../../docs/design/0007-pricing-floor-and-settlement.md) §2–§9；定价列的位置见 [`docs/design/0006`](../../../../docs/design/0006-gateway-models-and-consumer-surface.md) §1.6/§2.2。
- 决策：PostgreSQL 是业务事实权威、已受理 Job 固定受理时版本见 [`ADR-0003`](../../../../docs/adr/0003-postgresql-is-source-of-truth.md)；成本按渠道各自口径取数、金额不替代计量事实、缺字段不得猜测费用、**已就地修订的币种段与汇率段**见 [`ADR-0006`](../../../../docs/adr/0006-no-settlement-without-metering-evidence.md)；**预授权/上限条款的修订史**（预授权由调用方给的 `max_cost_microusd` 改为按供给保底表查得）见 [`ADR-0009 的修订史`](./2026-09-27-adr-0009-revision-history.md)，现行条款见 [`ADR-0009`](../../../../docs/adr/0009-multiple-active-offerings-and-routing.md)。
- 事实台账：AIHubMix 四档费率与无金额字段见 [`docs/facts/channel-facts.md`](../../../../docs/facts/channel-facts.md) 的 AIHubMix 费率节；APIMart 终态 `cost` 与实测样例见同文件的 APIMart 计量节。
- 迁移：[`0009_pricing_floor_and_settlement.sql`](../../../../migrations/0009_pricing_floor_and_settlement.sql)。
- 索引同步：[`docs/architecture.md`](../../../../docs/architecture.md) §2（⑤ Price）、§3（路由）、§4（一次请求怎么走）、§5（表）、§6（文件与迁移）；词汇表 [`GLOSSARY.md`](../../../../GLOSSARY.md) 新增 `Consumer Rate Vector` / `Markup` / `FX Rate` / `Floor Amount` / `Gross Margin`，并改写 `Price Plan` / `Price Snapshot` / `Provider Cost`。
- 相邻记录：[渠道成本事实采集](../../implemented/platform/2026-09-22-provider-cost-facts.md)（本片接它的折算列与发布期币种校验，并落它留下的两个执行单元）、[网关模型命名层](../../implemented/platform/2026-09-22-gateway-model-naming.md)（定价列按 P1 定下的位置落）。
- 工作项：提案 [#13](https://github.com/dehuadong/seeaihub-server-next/issues/13) 的 P2b 工单 [#16](https://github.com/dehuadong/seeaihub-server-next/issues/16)。
