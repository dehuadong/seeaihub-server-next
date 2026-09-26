-- 管理员身份与会话。
--
-- 今天管理员只有 `ADMIN_TOKEN` 这一种身份（共享令牌，见 `apps/api/src/main.rs` 的 `require_admin`），
-- 浏览器里手填共享令牌既不好用也没法追责。这两张表把管理员做成**具体的人**：一个有邮箱与口令的
-- 账号，以及一组可吊销的会话。
--
-- 口令只存 **argon2id 哈希**（`password_hash`），明文既不落库也不进日志；会话令牌同理——库里只存
-- 它的 SHA-256（`token_hash`），明文只在登录那一次响应里出现。
CREATE TABLE identity.admin_users (
    id uuid PRIMARY KEY,
    email text NOT NULL,
    password_hash text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    last_login_at timestamptz
);

-- 邮箱是身份键：大小写不敏感（统一存小写），同一时刻只能有一个账号。
CREATE UNIQUE INDEX admin_users_email ON identity.admin_users (lower(email));

CREATE TABLE identity.admin_sessions (
    id uuid PRIMARY KEY,
    admin_id uuid NOT NULL REFERENCES identity.admin_users(id) ON DELETE CASCADE,
    token_hash text NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);

CREATE INDEX admin_sessions_admin ON identity.admin_sessions (admin_id);

-- 对客侧的自助开户：一个邮箱对应一个账户。`account_id` 指向账本那一行——钱仍然只记在
-- `ledger.accounts` 上，这张表只回答"这个邮箱是哪个账户"。
CREATE TABLE identity.customers (
    id uuid PRIMARY KEY,
    email text NOT NULL,
    password_hash text NOT NULL,
    account_id uuid NOT NULL REFERENCES ledger.accounts(id),
    created_at timestamptz NOT NULL DEFAULT now(),
    last_login_at timestamptz
);

CREATE UNIQUE INDEX customers_email ON identity.customers (lower(email));
CREATE UNIQUE INDEX customers_account ON identity.customers (account_id);

CREATE TABLE identity.customer_sessions (
    id uuid PRIMARY KEY,
    customer_id uuid NOT NULL REFERENCES identity.customers(id) ON DELETE CASCADE,
    token_hash text NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);

CREATE INDEX customer_sessions_customer ON identity.customer_sessions (customer_id);

-- 口令重置令牌：管理员与客户的结构完全一样，只有"被重置者属于哪个身份域"不同，因此一张表装下。
--
-- `subject_id` 指 `admin_users` 或 `customers` 之一，所以做不了外键——被重置者的存在性由用例层在
-- 兑换时校验。令牌与会话一样只存 SHA-256；有效期短（默认 30 分钟）且**一次有效**：兑换时校验
-- `expires_at > now()` 且 `redeemed_at IS NULL`，用过写 `redeemed_at`。
--
-- 行**不删**："什么时候申请过、什么时候用过"是要留的事实。同一身份签新令牌时，此前未兑换的那些
-- 由下面的部分唯一索引挡住（一个 subject 同时只允许一条未兑换令牌），免得旧令牌在运营看不见的
-- 地方继续可用。
CREATE TABLE identity.password_resets (
    id uuid PRIMARY KEY,
    subject_kind text NOT NULL CHECK (subject_kind IN ('admin', 'customer')),
    subject_id uuid NOT NULL,
    token_hash text NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    redeemed_at timestamptz
);

CREATE INDEX password_resets_subject ON identity.password_resets (subject_kind, subject_id);

CREATE UNIQUE INDEX password_resets_one_open
    ON identity.password_resets (subject_kind, subject_id)
    WHERE redeemed_at IS NULL;

-- 审计补一列身份：既有 `actor text` 只能说明"经管理 API 做的"（十二处写死 `admin-api`），
-- 回答不了"哪个管理员做的"。共享令牌触发的写操作在这一列留空、`actor` 仍是 `admin-api`，
-- 因此既有取值与语义不变。
ALTER TABLE operations.audit_events
    ADD COLUMN admin_id uuid REFERENCES identity.admin_users(id);
