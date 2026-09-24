-- 账实核对的对账案例：被核对的是"某个账户的余额与它自己那本账"，不是某一次执行——所以案例
-- 可以没有 Job、也没有 Attempt。状态仍只有既有那两个取值（open / resolved），这里不新造状态。
ALTER TABLE operations.reconciliation_cases ALTER COLUMN job_id DROP NOT NULL;
ALTER TABLE operations.reconciliation_cases ALTER COLUMN attempt_id DROP NOT NULL;
ALTER TABLE operations.reconciliation_cases
    ADD COLUMN account_id uuid REFERENCES ledger.accounts(id);

-- 案例说到底问的是"哪个账户的钱出了问题"，所以这一列对两种来源都必填：执行类的案例账户取自
-- 它那个 Job（老行在这里补上）。
UPDATE operations.reconciliation_cases rc
SET account_id = j.account_id
FROM generation.jobs j
WHERE j.id = rc.job_id AND rc.account_id IS NULL;
ALTER TABLE operations.reconciliation_cases ALTER COLUMN account_id SET NOT NULL;

-- 一个账户同时只留一条未结案的账实案例：核对是周期跑的，没有这条索引的话，只要账户一直对不上，
-- 每跑一轮就会多出一条一模一样的案例。结案之后再来一条是另一回事（那是一次新的发现）。
CREATE UNIQUE INDEX reconciliation_cases_open_account
    ON operations.reconciliation_cases (account_id)
    WHERE status = 'open' AND job_id IS NULL;
