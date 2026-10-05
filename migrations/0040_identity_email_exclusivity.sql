-- 身份邮箱互斥：同一个邮箱不能同时是管理员与客户的登录身份（控制台 Spec v21 A6/C2/C13/§6）。
--
-- 跨两张表没有单个唯一索引可用，创建路径在同一事务里按邮箱取咨询锁并交叉检查；这条迁移只做一次
-- 存量校验：存在跨域重复时拒绝应用并点名，由运营先解决——登录身份不静默改动。
DO $$
DECLARE
    conflicts text;
BEGIN
    SELECT string_agg(email, ', ' ORDER BY email) INTO conflicts
    FROM (
        SELECT DISTINCT lower(c.email) AS email
        FROM identity.customers c
        JOIN identity.admin_users a ON lower(a.email) = lower(c.email)
    ) AS duplicated;
    IF conflicts IS NOT NULL THEN
        RAISE EXCEPTION 'migration 0040 refuses to run: the same email is both an admin and a customer login identity: %; resolve the duplicates before applying this migration', conflicts;
    END IF;
END $$;
