-- 按账户核对占用合计：核对问的是"这个账户当前 active 的预授权之和，是不是等于账户行上的
-- held_microusd"（docs/design/0013-account-funds-and-reservations.md §5）。索引只服务这条按账户的
-- 只读查询，不改变任何金额规则；只建在 active 行上——captured / released 的历史行不进这条等式。
CREATE INDEX holds_active_account
    ON ledger.holds (account_id)
    WHERE status = 'active';
