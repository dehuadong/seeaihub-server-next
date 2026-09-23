-- 供给身份：渠道 / 供给按**身份**唯一，发布按身份复用既有行。
--
-- 一件事：给供给行钉上身份。
--   - 渠道身份 = `provider_kind` + `base_url` + `credential_env`：同一个调用入口与凭证身份；
--   - 供给身份 = 它所属的 `vendor_model_id` + `channel_id`：同一个模型经同一个入口的那一条供给。
-- 发布从此按身份复用既有行、只更新可变量，于是 `enabled` 不再被每次发布写回 true——
-- 停用某条渠道或供给之后重发该模型的其它变动，那一行原样留着，停用因此对之后的受理一直有效。
--
-- 既有的同身份重复行**不在这里归并**：归并要改挂 `runtime_entries` / `jobs` / `routing_decisions`
-- 的 `offering_id` 与 `channel_id`，那是已发布修订与已受理 Job 的事实，迁移不该静默重写它们。
-- 有重复行时唯一索引建不上，迁移**响亮失败**，由人决定是重建库还是重新发布——本仓库尚未上线，
-- 生产库没有目录，开发库重建即可。先查有没有重复：
--   SELECT provider_kind, base_url, credential_env, count(*) FROM supply.channels
--   GROUP BY 1,2,3 HAVING count(*) > 1;
--   SELECT vendor_model_id, channel_id, count(*) FROM supply.offerings
--   GROUP BY 1,2 HAVING count(*) > 1;
--
-- 说明：以新迁移增量修改，不就地改 0001。

CREATE UNIQUE INDEX channels_identity
    ON supply.channels (provider_kind, base_url, credential_env);

CREATE UNIQUE INDEX offerings_identity
    ON supply.offerings (vendor_model_id, channel_id);

COMMENT ON INDEX supply.channels_identity IS
    '渠道身份：同一个调用入口与凭证身份只有一行，发布按它复用';
COMMENT ON INDEX supply.offerings_identity IS
    '供给身份：同一个模型经同一个入口只有一条供给，发布按它复用';
