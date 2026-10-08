-- 网关模型的并发名额：每个模型一行、由运营在控制台设置（工作项 #94）。
--
-- 可空：NULL 表示"用部署缺省"（`GENERATION_MAX_CONCURRENT_JOBS`，缺省 1）。存量行留 NULL，
-- 所以升级不改变任何模型的名额。它不属于发布内容——改了立刻影响之后受理的请求，不用重新发布；
-- 受理按「账户 × 网关模型」数在飞的 Job，不同模型之间互不占名额。
ALTER TABLE publication.gateway_models
    ADD COLUMN max_concurrent_jobs integer CHECK (max_concurrent_jobs > 0);

COMMENT ON COLUMN publication.gateway_models.max_concurrent_jobs IS
    '每个账户在该网关模型上同时在跑的生成任务上限；NULL＝用部署缺省（GENERATION_MAX_CONCURRENT_JOBS）';

-- 在飞计数按（账户 × 网关模型）：没有这条索引时，每次受理都要扫该账户的全部历史 Job。
CREATE INDEX jobs_account_gateway_model_in_flight
    ON generation.jobs (account_id, gateway_model)
    WHERE state IN ('admitted', 'executing');
