-- 名称唯一的口径改为**区分大小写**（Spec `0003` N1，v4 起）：`Star` 与 `star` 是两个不同的名称，
-- 只有逐字符完全相同的名称才算撞名。
--
-- 为什么是新增迁移而不是改 0029：迁移一经应用就不可改（sqlx 会校验已应用迁移的校验和），开发库与
-- 测试库都已经跑过 0029。0029 的去重按 `lower(name)` 分组，比现在这条口径**更严**——它可能把只差
-- 大小写的行拆开过（那两行现在本可以并存）。开发阶段不恢复这种历史改名：行还带着自己的 id 片段，
-- 运营或客户随时可以改回来。
DROP INDEX ledger.accounts_name_key;

CREATE UNIQUE INDEX accounts_name_key ON ledger.accounts (name);
