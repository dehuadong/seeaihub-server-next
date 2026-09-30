-- 账户当前值：已结算余额之外，另存**当前占用合计**与单调递增的 `version`。
--
-- 可用额由两者相减，不落第三份权威数（`docs/design/0013-account-funds-and-reservations.md` §1）。
-- 余额与可用额允许为负：实收高于预授权额时透支在结算吸收（`docs/specs/0002-account-funds-and-reservations.md` §1）。
-- 预授权只留在 `ledger.holds`，不再进资金流水。
ALTER TABLE ledger.accounts
    ADD COLUMN held_microusd bigint NOT NULL DEFAULT 0 CHECK (held_microusd >= 0),
    ADD COLUMN version bigint NOT NULL DEFAULT 0;

COMMENT ON COLUMN ledger.accounts.held_microusd IS
    '当前占用合计：active 预授权之和。可用额 = balance_microusd − held_microusd（0013 §1）';
COMMENT ON COLUMN ledger.accounts.version IS
    '账户金额的单调递增版本：每次金额变化 +1，供缓存拒绝倒序写回（0013 §3）';

-- 资金流水只留四个科目：入账、实收、调整、平台成本（`0013` §1）。
--
-- 改造前的 `hold` / `release` 行由 `ledger.holds` 的状态承接，本次不承担历史流水转换
-- （`0013` §6），清掉它们才能把科目面收紧到合同要求的四个。
DELETE FROM ledger.entries WHERE kind IN ('hold', 'release');
ALTER TABLE ledger.entries DROP CONSTRAINT entries_kind_check;
ALTER TABLE ledger.entries
    ADD CONSTRAINT entries_kind_check
    CHECK (kind IN ('credit', 'capture', 'adjustment', 'cost'));
