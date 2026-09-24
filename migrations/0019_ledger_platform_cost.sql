-- 平台自担的成本进账本：账户分「消费者」与「平台」两类，上游实扣的那笔费用记成 `cost` 条目。
--
-- 为什么单独一个账户：账本的每一笔分录都同时改所属账户的余额（账实核对的判据就是这个等式），
-- 把成本记进消费者账户等于改了消费者的钱，而这笔钱与消费者无关——消费者那一侧该收的收、该退的退，
-- 早就由 `capture` / `release` 记清了。
--
-- 为什么单独一个科目（不复用 `adjustment`）：混进去就分不清一条分录是"上游实扣的成本"还是
-- "运营改正的金额"。科目取值与领域枚举 `LedgerEntryKind` 一一对应（漏一个，读流水的接口会把
-- 整个账户的流水读成 500）。
ALTER TABLE ledger.accounts
    ADD COLUMN kind text NOT NULL DEFAULT 'consumer'
    CHECK (kind IN ('consumer', 'platform'));

COMMENT ON COLUMN ledger.accounts.kind IS
    '账户类别：consumer = 消费者账户（有 API Key 与余额缓存）；platform = 平台账户（承载平台自担的上游成本，无缓存、无预授权）';

-- 平台账户**只有一行**：它是账本的内部账户，不是运营开得出来的账户。
CREATE UNIQUE INDEX accounts_single_platform ON ledger.accounts (kind) WHERE kind = 'platform';

-- 固定 id：运营按它查成本流水（`GET /api/v1/accounts/{id}/entries`）。代码不认这个常量——
-- 它按 `kind` 找账户，账户是数据不是代码。
INSERT INTO ledger.accounts (id, balance_microusd, kind)
VALUES ('00000000-0000-4000-8000-000000000001', 0, 'platform');

-- 成本条目的科目取值。
ALTER TABLE ledger.entries DROP CONSTRAINT entries_kind_check;
ALTER TABLE ledger.entries
    ADD CONSTRAINT entries_kind_check
    CHECK (kind IN ('credit', 'hold', 'capture', 'release', 'adjustment', 'cost'));
