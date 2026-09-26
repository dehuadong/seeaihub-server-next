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
