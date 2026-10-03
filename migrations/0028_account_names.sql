-- 账户名称：每个账户始终有一个名称（Spec `0003` N3/N4）。
--
-- 名称在创建时由调用方给出或由服务端自动生成，所以这一列**非空**：产品里没有“未命名账户”这种状态。
-- 开发阶段不保留历史兼容，已存在的行在这里一次性回填一个能过约束的起点（平台账户那一行写成
-- 「平台账户」），之后由运营或客户改成真名。回填不是产品行为——产品只会产出生成规则（`0003` N3）
-- 算出来的名字，历史行拿到的只是“等着被改名”的占位名。
ALTER TABLE ledger.accounts ADD COLUMN name text;

UPDATE ledger.accounts
SET name = CASE
        WHEN kind = 'platform' THEN '平台账户'
        ELSE '账户_' || left(id::text, 8)
    END
WHERE name IS NULL;

ALTER TABLE ledger.accounts ALTER COLUMN name SET NOT NULL;
ALTER TABLE ledger.accounts
    ADD CONSTRAINT accounts_name_check CHECK (char_length(name) BETWEEN 1 AND 100);
