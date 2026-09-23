-- 已受理的 Job 连"打到哪个入口、用哪份凭证"一起冻结。
--
-- 一句话：`generation.jobs` 已经带着适配器与 Provider Model，但"发到哪里"（`supply.channels.base_url`）
-- 与"用哪个凭证引用"（`credential_env`）仍是执行时现场 JOIN 渠道行读的。这两列今天没有应用内的
-- 写入方（发布按身份复用渠道行、`ON CONFLICT DO NOTHING`；启停只改 `enabled`），可它们也没有任何
-- 约束挡着就地改：一次直接改库（迁移或运维脚本）换掉入口，一台已受理、还没被领走的 Job 就会带着
-- 受理时的 Provider Model 打到新入口，或者按新的变量名去取凭证。它打在哪个入口、凭证怎么取，与
-- 适配器和 Provider Model 同一批在受理那一刻就定下来了。
--
-- 于是把这两个值随 Job 落库：新受理的 Job 由受理路径一并写入；历史 Job 从它现在指向的那条渠道行
-- 回填——那正是这次改动之前执行会读到的值，回填之后它们的执行行为逐位不变。
--
-- 回填只认 `c.id = j.channel_id`。某条 Job 指向的渠道行若已不存在（`jobs.channel_id` 的外键正常挡着
-- 这种删除，因此只可能是绕过约束的数据），那些行回填命不中、两列留在 NULL 上，紧接着的
-- `SET NOT NULL` 让**整条迁移醒目失败**：与 0015 同一条处置，由人决定重建库还是先把数据补齐，
-- 不会静默留下一批"查不到执行入口或凭证名"的 Job。

ALTER TABLE generation.jobs ADD COLUMN base_url text;
ALTER TABLE generation.jobs ADD COLUMN credential_env text;

UPDATE generation.jobs j
SET base_url = c.base_url,
    credential_env = c.credential_env
FROM supply.channels c
WHERE c.id = j.channel_id AND j.base_url IS NULL;

ALTER TABLE generation.jobs ALTER COLUMN base_url SET NOT NULL;
ALTER TABLE generation.jobs ALTER COLUMN credential_env SET NOT NULL;
