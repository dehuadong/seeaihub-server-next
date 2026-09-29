# 对外定价录入与核对清单

- **用途**：运营把**真实对外价**录进平台，并核对"录的值真的生效了"。定价机制与归属见 [`docs/design/0007`](../design/0007-pricing-floor-and-settlement.md) §1–§3，这里只写**怎么录、怎么核**。
- **适用的读者**：持有管理员令牌（`ADMIN_TOKEN`）的运营或运维。消费侧看到的合同与价格由发布决定，不在这里。
- **前置**：服务的对外地址（下面写作 `$BASE`）与管理员令牌（`$ADMIN_TOKEN`）。令牌只从环境变量读，不写进仓库、日志或响应。

## 0. 录的是什么

| 要录的东西 | 落在哪 | 性质 |
| --- | --- | --- |
| 渠道币种 → CNY 的**折算率** | `pricing.fx_rates`（`PUT /api/v1/fx-rates`） | 外部事实，按币种维护；同一时刻同一币种全平台一个数 |
| **加价系数** `markup_bps` | 发布命令（`POST /api/v1/runtime-revisions`） | 每个网关模型一个，随修订发布、随 Job 快照冻结 |
| **按候选的对客费率向量** `consumer_rates_cny` | 同上（按候选） | 按 token 计量量那一种形态的对客价载体 |
| 对客选上游金额 × 倍率的候选 | 同上：给 `markup_bps` | 对客价 = 上游声明金额 × 冻结倍率 × 冻结折算率 |
| 保底额与档位价目表 `floor_amounts` | 同上（按候选） | 预授权用；不填则回落平台兜底数，受理闸门仍成立 |

**对客价 = 对客价目 × 本次实际量**（token 读四档向量；上游金额形态 = 声明金额 × 倍率 × 折算率）（实际分项 token / 上游这次声明的金额）。改价＝**发布新修订**，已受理 Job 不受影响——它们的价格随快照冻结在 Job 上。

## 1. 折算率（先做：没有它的币种在发布期就会被拒）

```bash
curl -s -o /dev/null -w '%{http_code}\n' -X PUT "$BASE/api/v1/fx-rates" \
  -H "Authorization: Bearer $ADMIN_TOKEN" -H 'Content-Type: application/json' \
  -d '{"currency":"USD","rate_micros":7100000}'
# 期望：204
```

- `rate_micros` 是**微单位**：`7100000` = 7.1（1 USD = 7.1 CNY）。
- `effective_at` 不给＝**立即生效**，生效时刻由数据库盖章；给了未来时刻就是**调价预告**，受理时取的仍是受理时刻之前已生效的那一行。
- 同币种（CNY → CNY）的率恒为 1，不需要录。
- 判据：**发布期会拒绝没有生效折算率的成本币种**——所以这一步排在发布之前。若发布返回 400 且消息指向缺折算率，就是这一步没做成。

## 2. 发布带价的新修订

请求体就是发布素材（`config/bootstrap/*.json`）那一份，把**夹具值换成正式价**：

```bash
curl -s -X POST "$BASE/api/v1/runtime-revisions" \
  -H "Authorization: Bearer $ADMIN_TOKEN" -H 'Content-Type: application/json' \
  -d @published-revision.json
```

发布命令里与钱有关的字段：

| 字段 | 位置 | 说明 |
| --- | --- | --- |
| `markup_bps` | 顶层（修订级） | 基点：`2000` = 1.2 倍。**对客选上游金额形态时必须给**；对客 token 价目由运营给，倍率只用于推导初始值 |
| `consumer_rates_cny` | 每个候选 | 四档 CNY 费率（每 1M token）。初始值按该 vendor/模型已知渠道价目 ×(1+`markup_bps`)× 折算率 推导、运营可改，或**直接录入** |
| `consumer_formula` | 每个候选 | 对客计价形态（运营按候选选，与成本计价形态独立） |
| `cost_unit_price_microusd` | 每个候选 | 按张 / 按次的**成本**单价（不是售价） |
| `reference_cost_microusd` | 每个候选 | 定价**参考**成本，只作展示与 `least_cost` 的比较输入，不进账 |
| `floor_amounts` | 每个候选 | 预授权用的保底表；不填只影响预授权额，不影响售价 |

- **同一修订号重发只允许内容逐字相同**：改价必须换 `native_revision`。
- 发布是一次**原子替换**：同一个网关模型的候选集整体换成这一份，不保留旧的。
- 400 的常见原因见 §5。

## 3. 核对"录的值真的生效了"

| 要核对的 | 怎么看 |
| --- | --- |
| 管理员面读到的定价就是刚发的 | `GET /api/v1/gateway-models`（带管理员令牌）：每个型号带它当前生效的候选与定价 |
| 对客价随快照冻结、改价不影响已受理 Job | 发一次请求，取该 Job 的 `price_snapshot`：里面有 `formula`、`markup_bps`、`consumer_rates_cny`、`fx_rate`、`floor_amounts`。改修订之后重发同一个请求，两份快照应当**不同**，而旧 Job 的那份仍是旧值 |
| 实收是"按实际量算出来的" | 结算后看账本 `capture` 分录：`token_rates` 读**实际分项 token × 冻结的向量**、上游给金额读**它声明的金额 × 冻结倍率 × 冻结折算率** |
| 保底额（预授权）与实际扣款是两个量 | `generation.jobs.max_cost_microusd` 是预授权额（保底额查表得来）；`ledger.entries` 的 `capture` 才是实收。实际超过保底额时**透支发生在结算**，余额可以为负 |

**一次完整核对的形状**：录折算率 → 发布带价修订 → 发一次最小请求（`n = 1`）→ 取 Job 的 `price_snapshot` 与 `capture` 分录 → 手算一遍"对客价目 × 实际量"，三个数应当对得上。

## 4. 机制已由这些用例验过

清单里的每一步都有端到端用例，**不必为了确认机制再花上游的钱**（夹具上游是进程内的假上游）：

| 行为 | 用例 |
| --- | --- |
| 定价随 Job 冻结，结算只读那份快照 | `pricing_is_frozen_into_the_job_and_settlement_only_reads_that_snapshot` |
| 实收按**命中候选**算，参考成本不参与 | `the_charge_follows_the_hit_candidate_and_ignores_the_reference_cost` |
| 折算率不给生效时刻时由**数据库**盖章 | `an_fx_rate_without_an_effective_time_is_stamped_by_the_database_clock` |
| 管理员面能读到已发布的定价 | `the_admin_view_lists_the_published_pricing` |
| 保底额查表与回落链 | `the_hold_resolves_the_tier_then_walks_the_supply_floor_chain` |
| 没有定价的旧修订与空保底表回落平台兜底数 | `an_unpriced_revision_and_an_empty_floor_table_fall_back_to_the_platform_default` |
| 透支发生在结算、下一笔被拒 | `an_overdraft_settles_into_a_negative_balance_and_the_next_request_is_refused` |

用例在 `apps/api/tests/http_contract/cases_pricing.rs`，需要空库（`HTTP_CONTRACT_DATABASE_URL`）并带 `--ignored` 跑。

## 5. 发布被拒时先看这几条

| 400 消息指向 | 通常是什么 |
| --- | --- |
| 缺折算率 / 无生效折算率 | §1 没做，或录的币种与候选声明的成本币种对不上 |
| `markup_bps` 缺 | 对客选上游金额形态没给倍率（它靠倍率乘上游声明金额）；其余形态的价目已由运营给出，不受影响 |
| 对客形态取值不在两种内 | 发布 `consumer_formula` 给了按张 / 按次（对客形态只有 `token_rates` / `upstream_declared`） |
| 对客费率向量与计价形态不配套 | 给对客选上游金额的候选发了 `consumer_rates_cny`（那个载体只属对客按 token 四档） |
| 成本单价缺 | 按张 / 按次形态没给 `cost_unit_price_microusd` |
| 单次最大成本超上限 | 该候选按**合同允许的最大输出张数**算出来的最坏成本超过运营设的单次上限；要么调上限，要么改价 |
| 合同与承载面不匹配 | 改的是合同/承载面，不是价格——见 [`docs/design/0005`](../design/0005-vendor-model-contract-and-offering-mapping.md) |

## 6. 别混的两组数

- **成本价**（渠道实际扣的）与**对外价**（对客收的）是两个量：前者只进毛利口径，后者才进客户账单。渠道优惠是否传导到对客价是定价决定，不是机制自动做的。
- **保底额（预授权）** 与**实收**是两个量：保底额只用于受理闸门，实收按实际量算。
- **参考成本** 只作定价参考与 `least_cost` 的比较输入，**不进账本**。

## 7. 与真实 Provider 的边界

本清单不含任何真实上游调用：核对定价不需要花上游的钱。要验真实渠道的**成本**口径（上游实际扣了多少），走 [`docs/verification/paid-provider-calls.md`](./paid-provider-calls.md)，那是计费调用，必须先获批预算与凭证。
