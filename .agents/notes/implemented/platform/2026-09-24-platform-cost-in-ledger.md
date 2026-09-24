---
title: 平台自担成本进账本：成本科目与平台账户
status: implemented
created: 2026-09-24
updated: 2026-09-24
approval: 用户 2026-09-23 裁定成本条目的口径——"它就是上游实际扣的那笔费用＝平台的成本"，不需要"未定价成本"这种新说法；并认可用自己的科目（不复用 `adjustment`）、挂在平台账户上。本件按该口径实施。
verification: 端到端两条（`the_reconciliation_path_records_the_cost_fact_it_already_has`、`the_platform_cost_is_queryable_under_its_own_kind_on_the_platform_account`）+ 领域单测一条；`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`、`cargo test --workspace --all-features` 全绿；空库端到端 107 passed / 0 failed
---

# Agent Note：平台自担成本进账本：成本科目与平台账户

## 问题

上游已经扣了钱、而我们这次没让消费者付费（终态失败、或对账退款结案）时，那笔钱在账上**一条记录都没有**：`generation.attempts` 的四个成本列只回答"这一次花了多少"，是执行事实，不是账目；`ledger.entries` 的 `adjustment` 科目从建库起就没有写入方。后果是运营从账上看不出平台为哪些失败付了钱、付了多少，也就核不了上游账单。

## 决定

- **成本记成它自己的科目 `cost`**，金额为负。不复用 `adjustment`：混进去就分不清一条分录是"上游实扣的钱"还是"运营对账目的的改正"。
- **挂在平台账户上**：`ledger.accounts` 增 `kind`（`consumer` / `platform`），迁移 [`0019`](../../../../migrations/0019_ledger_platform_cost.sql) 种下**唯一一行**平台账户（固定 id，运营按它查成本流水）。成本不能记进消费者账户——账本的每笔分录都同时改所属账户的余额（账实核对的判据就是这个等式），记进消费者账户就是改了消费者的钱。
- **写入时刻是执行收尾那一次事务**：`fail_job` 把成本事实落到 attempts 那一行时，同事务再在账本上记一条 `cost`。与预授权怎么处置无关——它就是账上该看见这笔钱的时刻。金额取**折算后 CNY**（`provider_cost_cny_microusd`，与账本同一个单位），为 0 或没有折算值时不写：那不是"花掉 0 元"，是这次没有可记账的成本事实。
- **对账退款不补记**：退款退的是消费者的预授权，成本在上面那次事务里已经记过。业务键按执行唯一（`job:{job}:attempt:{attempt}:cost`），同一笔记两次会被唯一约束挡下。
- **成功那一次不落账本**：它的成本留在 attempts 四列，只进毛利口径（[`docs/design/0007`](../../../../docs/design/0007-pricing-floor-and-settlement.md) §5）。
- **平台账户不消费者化**：没有 API Key、不参与预授权、不进余额缓存的增量（`accounts_updated_within` 只要 `consumer`）。它的余额就是**累计自担成本的负数**——没有充值分录，这是它的定义；账实核对对它用同一条规则（余额 = 该账户分录之和）。

## 备选方案

- **复用 `adjustment`**：落选，见上——来源分不清（与当初不肯把 `credit` 并进 `adjustment` 同一个理由）。
- **不建账户，只在分录上标"平台"**：落选。账本的每笔分录都同时改某个账户的余额，"没有账户"就没有余额那一侧可改；账实核对是**逐账户**比 `balance` 与 `SUM(amount)`，少了这个账户就少了一边的判据。
- **记进消费者账户**：落选。那一列余额是消费者的钱，成本与消费者无关；硬记还要给消费者账户配一条反向分录，等于把两个口袋混成一个。
- **在退款结案时才记成本**：落选。成本事实在失败那一刻就已确定（attempts 四列同事务写入），拖到退款会让**没人处置的案例**永远不进账；两处都写又会重复记账。
- **成功的执行也记一条成本**：落选。那会把"平台自担"与"平台收得回来"混成一个数；成功那笔的成本事实属于毛利口径。

## 后果

- 平台账户的余额会**越来越负**，那个负数就是累计自担成本；它没有上限也没有充值来源。
- 成本记录**查得到**：平台账户与消费者账户共用同一个管理员流水接口（`GET /api/v1/accounts/{account_id}/entries`）。`LedgerEntryKind` 与库层 `CHECK` 因此在同一次变更里加取值——漏认一个科目，整条流水会读成 500，而不是"少一条"。
- 迁移 `0019` 给既有账户补的 `kind` 一律是 `consumer`（列带默认值）；平台账户只此一行，部分唯一索引挡住第二行。
- **没做的**：成本缺口补录、上游账单周期核对（`/v1/usage`）、取消（同步对客面没有任务号，消费者没有东西可取消）。

## 验证

| 行为 | 证据 |
| --- | --- |
| 两种预授权处置（终态失败释放、进对账保留）各留一条成本记录，金额为负、挂在平台账户上，且平台账户余额等于它承担的成本总额 | `the_reconciliation_path_records_the_cost_fact_it_already_has`（`apps/api/tests/http_contract/cases_cost_facts.rs`） |
| 对账退款只释放消费者的预授权，成本条目**不加**；消费者余额里始终没有成本那一笔 | 同上 |
| 没有成本事实的执行一条都不记（"没有"不是"0"） | 同上 |
| 平台账户的成本在管理员流水里查得到，`kind` 是 `cost`、金额为负、指得出是哪次执行；消费者的流水里没有它 | `the_platform_cost_is_queryable_under_its_own_kind_on_the_platform_account`（同文件） |
| 科目取值与库层 `CHECK` 一一对应、读得回来 | `ledger_entry_kinds_round_trip_through_their_stored_form`（`crates/domain`） |
| 门禁 | `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`、`cargo test --workspace --all-features`，空库端到端 `cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1` → 107 passed / 0 failed |

## 依据与关联

- 账本落点与写入方见 [`docs/architecture.md`](../../../../docs/architecture.md) §5；术语见 [`CONTEXT.md`](../../../../CONTEXT.md) 的 `Platform Account`。
- 成本事实本身的采集与三态（`computed` / `declared` / `unavailable`）见[渠道成本事实采集](./2026-09-22-provider-cost-facts.md)。
- 账实核对（比对余额与分录之和，只发现不改账）见[账实核对](./2026-09-23-ledger-balance-audit.md)。
- 金额不替代计量事实、缺证据不得猜测见 [`ADR-0006`](../../../../docs/adr/0006-no-settlement-without-metering-evidence.md)；PostgreSQL 是业务事实权威见 [`ADR-0003`](../../../../docs/adr/0003-postgresql-is-source-of-truth.md)。
