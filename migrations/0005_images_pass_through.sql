-- 图片按渠道原形进原形出：平台不再持有静态资产。
--
-- 输入侧不再有资产绑定：调用方直接给公网 URL 或 data URL，平台受理时把它落到被选中候选
-- 自己声明的参数名上，此后 Job 里就是一份普通原生参数（图片不再有独立的身份、尺寸与摘要）。
-- 结果侧不再有资产引用：Job 只留当次结果的**信封**，每项只有渠道给的 `url` 或 `b64_json`。
--
-- 0001 以「建表」方式被应用过，改它不会更新已建好的库，因此这里增量修改：老环境照样能升上来。

ALTER TABLE generation.jobs DROP COLUMN asset_bindings;
ALTER TABLE generation.jobs DROP COLUMN result_asset_ids;
ALTER TABLE generation.jobs ADD COLUMN result_images jsonb;

DROP TABLE generation.assets;
