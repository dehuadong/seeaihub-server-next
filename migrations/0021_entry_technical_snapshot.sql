-- 已发布的修订连"这条候选的技术定义"一起冻结。
--
-- 一句话：`publication.runtime_entries` 今天只记"用了哪条 Offering"，而驱动器、供应商模型名、
-- 承载面、参数映射、限制与渠道三要素都是装配候选时**现场 JOIN 活表**读的（`supply.offerings` /
-- `supply.channels` 的当前值）。于是"这次发布定义了什么"并不由这次发布决定：工程师后来改一条
-- Offering 的承载面、或改一条渠道的地址，**已发布修订**的候选就跟着变了；两条指向同一个
-- `vendor_model_id` 的网关模型还会互相改活对方的候选（发布对 `(vendor_model_id, channel_id)` 走
-- `DO UPDATE`）。这与 `ADR-0009`"一次发布携带完整有序候选集、发布即原子替换"同一条纪律下的
-- "发布即冻结"是矛盾的——`0016` 已经把**执行入口**冻结到 Job 上，这里补的是它之前那一段：
-- 候选本身。
--
-- 于是把这些值随条目落库：新发布的条目由发布路径一并写入；历史条目从它们现在指向的那两条活行
-- 回填——那正是这次改动之前装配候选会读到的值，回填之后已发布修订的受理口径逐位不变。
--
-- **两个 `enabled` 开关不在这里**（`supply.offerings.enabled` / `supply.channels.enabled`）：它们是
-- 运行状态，停用要立刻对之后的受理生效，不能被某次发布的快照钉住。受理先按条目拿到候选与技术定义，
-- 再按活表这两个开关判"现在还让不让走"。
--
-- 回填要求 `offering_id` 与 `channel_id` 都命中活行。`runtime_entries.offering_id` 的外键挡着删除，
-- 因此命不中只可能是绕过约束的数据；那些行留在 NULL 上，紧接着的 `SET NOT NULL` 让**整条迁移醒目
-- 失败**——与 `0015`/`0016` 同一条处置，由人决定重建库还是先把数据补齐，不会静默留下一批
-- "查不到技术定义"的已发布条目。
--
-- 说明：以新迁移增量修改，不就地改 `0001`。

ALTER TABLE publication.runtime_entries ADD COLUMN adapter_key text;
ALTER TABLE publication.runtime_entries ADD COLUMN provider_model_id text;
ALTER TABLE publication.runtime_entries ADD COLUMN carrier_schema jsonb;
ALTER TABLE publication.runtime_entries ADD COLUMN parameter_mapping jsonb;
ALTER TABLE publication.runtime_entries ADD COLUMN restrictions jsonb;
ALTER TABLE publication.runtime_entries ADD COLUMN provider_kind text;
ALTER TABLE publication.runtime_entries ADD COLUMN base_url text;
ALTER TABLE publication.runtime_entries ADD COLUMN credential_env text;

UPDATE publication.runtime_entries re
SET adapter_key = o.adapter_key,
    provider_model_id = o.provider_model_id,
    carrier_schema = o.carrier_schema,
    parameter_mapping = o.parameter_mapping,
    restrictions = o.restrictions,
    provider_kind = c.provider_kind,
    base_url = c.base_url,
    credential_env = c.credential_env
FROM supply.offerings o
JOIN supply.channels c ON c.id = o.channel_id
WHERE o.id = re.offering_id AND re.adapter_key IS NULL;

ALTER TABLE publication.runtime_entries ALTER COLUMN adapter_key SET NOT NULL;
ALTER TABLE publication.runtime_entries ALTER COLUMN provider_model_id SET NOT NULL;
ALTER TABLE publication.runtime_entries ALTER COLUMN carrier_schema SET NOT NULL;
ALTER TABLE publication.runtime_entries ALTER COLUMN parameter_mapping SET NOT NULL;
ALTER TABLE publication.runtime_entries ALTER COLUMN restrictions SET NOT NULL;
ALTER TABLE publication.runtime_entries ALTER COLUMN provider_kind SET NOT NULL;
ALTER TABLE publication.runtime_entries ALTER COLUMN base_url SET NOT NULL;
ALTER TABLE publication.runtime_entries ALTER COLUMN credential_env SET NOT NULL;

COMMENT ON COLUMN publication.runtime_entries.adapter_key IS
    '这次发布冻结的驱动器；受理装配候选读它，不读 supply.offerings 的当前值';
COMMENT ON COLUMN publication.runtime_entries.provider_model_id IS
    '这次发布冻结的供应商模型名';
COMMENT ON COLUMN publication.runtime_entries.provider_kind IS
    '这次发布冻结的渠道三要素之一；enabled 不在快照里，停用仍立刻生效';
