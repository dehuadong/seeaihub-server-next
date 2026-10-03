-- 账户名称必须唯一（Spec `0003` N1，v3 起）：重名会让"按名称识别账户"这件事失效。
--
-- 唯一性是**大小写不敏感**的：`Star` 与 `star` 对运营来说是同一个名字，允许并存等于没唯一。索引用
-- `lower(name)` 表达这条规则（`lower(text)` 是 immutable，可以进索引表达式）。
--
-- 开发阶段的历史行在 0028 里已按 `账户_<id 前 8 位>` 回填（本身不会重名），但 v3 之前改名不挡重名，
-- 因此非空开发库里可能有同名或只差大小写的行。建索引前必须把它们拆开。
--
-- 拆法要同时满足两件事：结果仍在 0028 的长度上限内，且**不会撞上另一行**。
--   * 后缀用该行**完整的账户 id**（32 个十六进制字符）：前缀截到 100 - 33 = 67 个字符后再拼，
--     长度恰好不超过 100；两行 id 不同，后缀就不会相同。
--   * 但"某一行本来就叫 `xxx_<另一行的完整 id>`"在构造上仍可能撞名，所以改名整体放在循环里跑：
--     每一轮把当前仍重名的组里除最早一行以外的行改名，直到一轮下来没有任何改动。
--   * 循环退出后若仍有重名（实际不可能），`CREATE UNIQUE INDEX` 会直接失败并中止迁移，而不是悄悄
--     留下重名。
DO $$
DECLARE
    changed integer;
    round integer := 0;
BEGIN
    LOOP
        round := round + 1;
        WITH collisions AS (
            SELECT id,
                   row_number() OVER (
                       PARTITION BY lower(name) ORDER BY created_at ASC, id ASC
                   ) AS rank
            FROM ledger.accounts
        )
        UPDATE ledger.accounts AS account
        SET name = left(account.name, 100 - 33) || '_' || replace(account.id::text, '-', '')
        FROM collisions
        WHERE account.id = collisions.id AND collisions.rank > 1;

        GET DIAGNOSTICS changed = ROW_COUNT;
        EXIT WHEN changed = 0 OR round >= 10;
    END LOOP;
END $$;

CREATE UNIQUE INDEX accounts_name_key ON ledger.accounts (lower(name));
