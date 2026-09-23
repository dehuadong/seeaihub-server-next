-- 已受理的 Job 冻结它当时用的**适配器**与 **Provider Model**。
--
-- 一句话：发布现在按**身份**复用既有供给行、只更新可变量（见 0014），于是
-- `supply.offerings.adapter_key` 与 `provider_model_id` 会在重发这条模型时就地改写；而 worker
-- 执行时若现场 JOIN 这张表取这两个值，一个**已受理但还没执行**的 Job 就会用上后来改的适配器与
-- Provider Model。它要发给哪个驱动、往线文里写哪个模型名，在受理那一刻就已经定下来了，不该被
-- 之后的任何一次发布改掉。
--
-- 于是把这两个值在受理时随 Job 落库，与 `carrier_schema` / `parameter_mapping`（0006）同一处：
--   - 新受理的 Job 由受理路径写入；
--   - 既有的历史 Job 就地从它**现在**指向的那条供给行回填。这不等于"受理当时的值"——那个值没有
--     被记录过、无从恢复；但它**正是这次改动之前执行会去读的那个值**，所以回填之后这些 Job 的
--     行为逐位不变，而从此它们也不再跟着供给行变。回填完置 NOT NULL：留空等于"执行时再说"，
--     那正是这条切片要堵的口子。
--
-- 回填只认 `o.id = j.offering_id`。某条 Job 指向的供给行若已不存在（`jobs.offering_id` 的外键
-- 正常挡着这种删除，因此只可能是绕过约束的数据），这行留在 NULL，随后的 `SET NOT NULL` 让整条
-- 迁移**醒目失败**：与 0014 的响亮失败同一条处置，由人决定重建库还是先把数据补齐，不会静默留下
-- 一批执行不了的 Job。

ALTER TABLE generation.jobs ADD COLUMN adapter_key text;
ALTER TABLE generation.jobs ADD COLUMN provider_model_id text;

UPDATE generation.jobs j
SET adapter_key = o.adapter_key,
    provider_model_id = o.provider_model_id
FROM supply.offerings o
WHERE o.id = j.offering_id AND j.adapter_key IS NULL;

ALTER TABLE generation.jobs ALTER COLUMN adapter_key SET NOT NULL;
ALTER TABLE generation.jobs ALTER COLUMN provider_model_id SET NOT NULL;
