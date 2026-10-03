主题: 身份与控制台的技术设计
当前修订: v1
状态: 待评审
承接: [`0001` 控制台 Spec v15](../specs/0001-admin-and-customer-consoles.md) 的身份与双入口产物；账户金额及账单行为由 [`0002` 账户资金 Spec v3](../specs/0002-account-funds-and-reservations.md) 承接，客户独立页面由 [`0014`](0014-customer-console-navigation-and-history.md) 承接
依赖: [`docs/design/0006`](./0006-gateway-models-and-consumer-surface.md)、[`0007`](./0007-pricing-floor-and-settlement.md)、[`0008`](./0008-routing-strategy-and-caching.md)、[`0009`](./0009-operational-baseline.md)；`ADR-0003`、`ADR-0015`、`ADR-0016`、`ADR-0017`

# 身份与控制台的技术设计

本文负责客户与管理员身份、接口认证及双入口产物；客户控制台的导航、页面内容与历史浏览由[客户控制台设计](0014-customer-console-navigation-and-history.md)负责。技术上如何满足身份与交付要求由本文决定；产品行为不在本文改写。

## 1. 系统边界与职责

| 组件 | 职责 | 不负责 |
| --- | --- | --- |
| `crates/domain` | 身份标识类型（管理员/客户/会话）、密钥与账户的既有领域类型 | 密码学、数据库、HTTP |
| `crates/application` | 身份用例：引导、登录、认会话、退出、改口令、注册；口令与令牌的**密码学**（argon2id、随机令牌、摘要） | SQL、HTTP 形状、页面 |
| `crates/persistence` | 身份与会话的读写（迁移 `0020`）；账户与账本的既有读写 | 口令校验策略、会话有效期策略 |
| `apps/api` | 认证中间件（会话凭据与 `ADMIN_TOKEN` 两条路）、新增端点、静态产物托管 | 页面逻辑 |
| `apps/web` | 两个控制台的界面；调用既有与新增端点 | 任何业务判断（金额、权限、状态都由服务端给） |

**为什么密码学放应用层**：口令与令牌的生成/校验是"怎么做事"，属于用例；仓储只回答"这个邮箱对应哪一行、这条摘要存在吗"。把 argon2 放进仓储会让"哈希算法"变成基础设施的实现细节，将来换算法要动数据层；放在用例层则换算法只改一处，且能用内存替身测。

## 2. 数据模型

### 2.1 新增表（迁移 `0020`）

| 表 | 列与约束 | 说明 |
| --- | --- | --- |
| `identity.admin_users` | `id` PK、`email`、`password_hash`、`created_at`、`updated_at`、`last_login_at`；`lower(email)` 唯一 | 管理员是人，不是共享令牌 |
| `identity.admin_sessions` | `id` PK、`admin_id` FK→`admin_users`（级联删）、`token_hash` 唯一、`created_at`、`expires_at` | 库里只有令牌的 SHA-256 |
| `identity.customers` | `id` PK、`email`、`password_hash`、`account_id` FK→`ledger.accounts`、`created_at`、`last_login_at`；`lower(email)` 唯一、`account_id` 唯一 | 一个客户恰好一个账户 |
| `identity.customer_sessions` | 同 `admin_sessions`，指向 `customers` | 同上 |
| `identity.password_resets` | `id` PK、`subject_kind`（`admin` / `customer`）、`subject_id`、`token_hash` 唯一、`created_at`、`expires_at`、`redeemed_at`；`(subject_kind, subject_id) WHERE redeemed_at IS NULL` 部分唯一索引 | 口令重置令牌：明文只在签发响应里，库里只有 SHA-256；一次有效，行不删（留下"什么时候申请过、用过"的事实），未兑换的令牌一个身份同时只允许一条 |

**重置令牌为什么单独一张表而不是两张**：管理员与客户的重置令牌在结构上完全一样，唯一区别是被重置者属于哪个身份域。用 `subject_kind` + `subject_id` 一张表装下，省掉一份重复的列与索引，也让"这个摘要是什么令牌"只需一次点查。代价是 `subject_id` 不能做外键（它指向两张表之一），因此**被重置者的存在性由用例层在兑换时校验**。

### 2.2 补一列（同一迁移）

`operations.audit_events` 增加 `admin_id uuid NULL`（指向 `identity.admin_users`）：既有审计只记 `actor text`（十二处写死 `admin-api`），而本能力要能回答"这次改动是**哪个管理员**做的"。共享令牌触发的写操作在 `admin_id` 留空、`actor` 仍是 `admin-api`——**既有 actor 取值与语义不变**，新列只补充身份。

### 2.3 不改的表

- `identity.api_keys` 已按 `account_id` 归属，**对客自助列/建/吊销直接复用**，不加列、不加表。
- `ledger.accounts` / `ledger.entries` / `ledger.holds` 是账务权威，本能力**只读**它们（充值仍走既有 `credit_account` 用例）。用量与账单从 `generation.jobs`、`generation.attempts`、`ledger.entries` 取。

**账户与登录身份是两件事**：账户是账本里的一行（有钱、有 Key、有历史），登录身份是 `identity.customers` 里的一行（邮箱 + 口令 → 指向某个账户）。两者各自存在，可以分开产生：

- **客户自助注册**：新建账户 + 新建身份，一个事务里一起写（§4.2 的 `POST /v1/customers`）。
- **运营替客户开户**：给一个邮箱配身份，账户可以是新建的，也可以是**今天已经存在的**（管理员建的账户此前只有账户没有身份）。`identity.customers.account_id` 上已有唯一约束，所以一个账户至多配一个身份，重复绑定由那条约束挡下并回 `409`。

这个区分决定了配身份的实现方式：账户行早就存在，缺的只是身份行，所以配身份只往客户表里插一行**并指向那个账户**，不动账本、不动密钥、也不需要账户间转账。

## 3. 认证与会话

### 3.1 两条认证路径

管理面接受两种凭据，**判据顺序固定**：

1. `Authorization: Bearer <ADMIN_TOKEN>`：与配置里的共享令牌常量时间比较，命中即放行（自动化与端到端测试走这条）；
2. 否则把凭据当作**会话令牌**，取它的 SHA-256 查 `admin_sessions` + `admin_users`，未过期即放行，并记 `admin_id`（供审计与"谁改的"）。

两条都不命中 ⇒ 拒绝，且**错误码与响应体与"共享令牌写错了"完全一样**：调用方分不出自己拿的是哪种凭据、也分不出凭据是不存在还是过期（Spec §4.1）。既有实现回 `403 admin_forbidden`，本能力沿用同一个状态码与错误码——引入会话不改变这条既有语义。

**为什么保留共享令牌**：端到端测试与运维脚本今天靠它，撤掉等于把全部既有合同用例改一遍；它也承担"所有管理员都进不去"时的自救（签发重置令牌）。它的存在不影响检索"谁做的"——会话认证的写操作在审计里带 `admin_id`，共享令牌触发的写操作 `admin_id` 留空、`actor` 仍是既有的 `admin-api`。

### 3.2 会话令牌

- **两个 UUIDv4 拼接的十六进制（约 244 位随机）**，明文只在登录/注册的响应里出现；
- 库里存 `identity.*_sessions.token_hash` 列（值 = 令牌的 SHA-256）；查询按该列的唯一索引走；
- 有效期是配置项（`SESSION_TTL_SECONDS`，默认 12 小时）；过期行在认证失败时顺手删除；
- **吊销即生效**：认证每次都读库，没有"凭据还有效吗"的缓存（与 API Key 吊销同一条纪律，见 [`docs/design/0009`](./0009-operational-baseline.md) §4；缓存只放"标识 → 账户"的映射，判据仍每次读库，见 [`docs/design/0008`](./0008-routing-strategy-and-caching.md) §7.2）。

**备选（落选）：签名令牌（JWT 式）**。理由：签名令牌只能等它自己过期，退出与"立刻失效"做不到；而本平台的既有认证（API Key）本来就是每次读库换身份，多一次点查换吊销即生效是同一条取舍。

### 3.3 口令

- argon2id（`argon2` crate，`Argon2::default()` 参数），自动加盐；
- 长度下限 8（配置项 `MIN_PASSWORD_LENGTH` 的当前取值）；除长度外不做复杂度规则；
- 登录失败时，**邮箱不存在也走一遍哈希校验**（进程内惰性生成的一条哑哈希），使两条路径耗时不可区分（Spec §4.1、V-A2）；
- 改口令需要当前口令（Spec A4/V-A4）；管理员与客户共用同一套规则。

### 3.4 首次引导

- 环境变量 `ADMIN_EMAIL` + `ADMIN_PASSWORD`：**两个都给**时按邮箱 upsert 账号（首次插入时写入口令哈希，已存在时**不改**口令——否则每次重启都会把运维改过的口令打回环境变量的值）；
- 两个都没给时**不创建**账号，启动日志里 `warn` 一条"没有管理员账号，管理后台登录不可用"，进程照常起（后台不可用要能被发现，但不该让 API 起不来；客户侧不受影响，客户自己注册）；
- 只给一个视为配置错误，启动失败并点名缺哪一个。

## 4. HTTP 端点

### 4.1 新增（管理员身份）

| 方法 | 路径 | 认证 | 请求 | 响应 |
| --- | --- | --- | --- | --- |
| `POST` | `/api/v1/admin/sessions` | 无 | `{email, password}` | `200 {token, expires_at, email}`；失败 `400 invalid_parameter` |
| `GET` | `/api/v1/admin/session` | **仅会话** | — | `200 {admin_id, email}` |
| `DELETE` | `/api/v1/admin/sessions` | **仅会话** | — | `204`（幂等） |
| `PUT` | `/api/v1/admin/password` | **仅会话** | `{current_password, new_password}` | `204`；当前口令不对 `400 invalid_parameter`；成功后**删掉该管理员全部会话**（含当前这条） |
| `POST` | `/api/v1/admin/password-resets` | 会话或共享令牌 | `{email}` | `201 {reset_token, expires_at}`（签发，写审计） |
| `POST` | `/api/v1/admin/password-resets/redeem` | 无（凭令牌） | `{reset_token, new_password}` | `204`；令牌无效/过期/已用 `400 invalid_parameter`；成功后删掉该管理员全部会话 |
| `POST` | `/api/v1/customers` | 管理员 | `{email, password?, account_id?}` | `201 {customer_id, email, account_id}`（**替客户开户**：不给 `account_id` 就新建一个空账户；给了就把登录身份配到那个已有账户上。邮箱或账户已被绑定 `409 conflict`；`password` 缺省时不设初始口令，改用重置令牌让客户自己设） |
| `GET` | `/api/v1/customers` | 管理员 | `?email=&limit=` | `200 {customers:[{customer_id, email, account_id, created_at, last_login_at}]}`（**按邮箱找客户账户**：`email` 精确匹配、大小写不敏感，缺省按创建时间倒序列最近若干条；**不含口令哈希、会话与余额**——余额按 `account_id` 走既有的 `GET /api/v1/accounts/{id}`） |
| `GET` | `/api/v1/accounts/{account_id}/summary` | 管理员 | — | `200 {account_id, balance_microusd, tag, email, created_at, updated_at}`（**按标识读账户摘要**，账户详情直达与刷新用；账户不存在 `404`。持有中与可用额仍走既有的 `GET /api/v1/accounts/{id}`，页面不重算金额） |
| `GET` | `/api/v1/customers/{customer_id}` | 管理员 | — | `200 {customer_id, email, account_id, created_at, last_login_at}`（**按标识读客户视图**，客户详情直达与刷新用；客户不存在 `404`。不含口令、会话、API Key 明文与重置令牌） |
| `POST` | `/api/v1/accounts/{account_id}/password-reset` | 管理员 | — | `201 {reset_token, expires_at}`（运营为客户账户签发重置令牌，写审计；那个账户没有邮箱登录身份时 `404`） |
| `GET` | `/api/v1/fx-rates` | 管理员 | — | `200 {rates:[{currency, rate_micros, effective_at}]}`（折算率页要显示"当前录入结果"，今天只有 `PUT`） |

**最后两条为什么需要一个"无认证"的兑换入口**：口令重置的全部意义就是"进不去了"。若兑换也要求会话，A8 就成了一句空话——这正是"共享令牌签发的令牌没人能兑换"那个洞。所以兑换**只认令牌本身**：令牌就是这次的凭据，它与会话令牌同强度、只存摘要、只活一次。兑换成功后不签发会话，调用方用新口令正常登录。

**"仅会话"的三条**：它们问的都是"**我**是谁、改**我**的口令、退**我**这次登录"——共享令牌不指向任何一个管理员，用它回答"我是谁"只能编一个身份出来。因此：

- `require_admin`（**既有的全部管理端点**——读写都算，如 `GET /api/v1/gateway-models`、发布、账户与密钥、对账与诊断，共十余条——加上 §4.3 的客户列表读、§4.4 的折算率读）**接受两种凭据**：会话令牌，或共享令牌；
- `require_admin_session`（本节前三条）**只接受会话令牌**，共享令牌在这里被拒——不是权限不足，而是这个凭据没有能力回答"我是谁"。

**`POST /api/v1/runtime-revisions` 的发布命令支持增量**：候选的**渠道三要素**（`provider_kind`、`base_url`、`credential_env`）与**计价依据**（`formula` 与它配套的 `price_plan`、`cost_unit_price_microusd`、`cost_currency`、`reference_cost_microusd`、`cost_basis`、`tier_prices`、`floor_amounts`）可以省略；`adapter_key` 也随渠道一并省略。省略时服务端按该型号**当前生效**的修订里那条候选所属的渠道复用，不再要求重填。

要解决的是"改价必须重填技术字段"：候选集合每次都给全，但渠道地址、凭证变量名与渠道怎么结算改价时一个字没变，而它们不是运营的决定——管理端视图也不回显计价依据（它给的是对客价与参考成本）。少了计价这一半，运营改一个倍率就会把渠道的四档费率写成空值：看起来改了价，其实改坏了渠道价目。

**渠道三要素要么整组给、要么整组省，不逐个省**。渠道身份是 `provider_kind` + `base_url` + `credential_env`（`supply.channels_identity`），缺任一个都无法定位既有渠道；逐个省会造成"地址沿用旧账号、凭证换了新账号"这类半新半旧的渠道，而那种候选在受理时才会以渠道报错的形式暴露。

**`formula` 省不省，就是计价依据省不省**：形态是计价参数的判别式，不知道形态就不知道那份四档费率、单价与成本币种该不该在这儿。

**两组各自独立判断**：只省渠道、却显式给了新计价形态时，计价用新的——"重签渠道价目但不动渠道地址"不会被上一版的旧价目覆盖。

**省略时靠什么找到那条候选**：`provider_model_id` 是必填的（省渠道时 `provider_kind` 也给），认同一条候选的判据就是该型号当前生效修订里渠道模型名相同、且（省渠道时）供应商也相同的那一条。**同名多条时按歧义拒绝**并要求显式给出渠道三要素——不选"最便宜的那条"或"下标最近的那条"顶替。

**"省略"与"给了但为空"必须分得开**：字段缺省（`null` / 不出现）表示沿用上一版；空串或空对象表示发布者显式声明了一个空值，那不是一个合法的渠道部署事实，按参数错误拒绝。

**沿用不到就拒，不猜**：该型号还没有生效修订，或当前修订里没有同身份的候选，而字段又省略了，发布期按校验错误拒绝并点名是哪条候选缺什么。

**继承结果物化进新修订**：补齐的字段随这次发布写进新修订的快照，与既有"发布即原子替换候选集合"（`ADR-0009`）一致。

**不可省略的字段**：对客费率向量（`consumer_rates_cny`）、承载面（`carrier_schema`）、参数映射（`parameter_mapping`）、限制（`restrictions`）。前一个是这次发布**要改变的东西**；后三个是候选承载能力的声明，改价不动它们、但管理端视图回显得到，所以不必让服务端替运营记。

### 4.2 新增（客户身份）

| 方法 | 路径 | 认证 | 请求 | 响应 |
| --- | --- | --- | --- | --- |
| `POST` | `/v1/customers` | 无 | `{email, password}` | `201 {token, expires_at, email, account_id}`；邮箱占用 `409 conflict` |
| `POST` | `/v1/customer/sessions` | 无 | `{email, password}` | `200 {token, expires_at, email, account_id}` |
| `DELETE` | `/v1/customer/sessions` | 客户会话 | — | `204` |
| `PUT` | `/v1/customer/password` | 客户会话 | `{current_password, new_password}` | `204`；成功后**删掉该客户全部会话** |
| `POST` | `/v1/customer/password-resets/redeem` | 无（凭令牌） | `{reset_token, new_password}` | `204`；令牌无效/过期/已用 `400 invalid_parameter`；成功后删掉该客户全部会话 |
| `GET` | `/v1/customer/api-keys` | 客户会话 | — | `200 {keys:[{key_id, label, created_at, revoked_at}]}`（**无明文**） |
| `POST` | `/v1/customer/api-keys` | 客户会话 | `{label}` | `201 {key_id, api_key}`（明文只此一次） |
| `DELETE` | `/v1/customer/api-keys/{key_id}` | 客户会话 | — | `204`；不属于自己 ⇒ `404` |
| `GET` | `/v1/customer/account` | 客户会话 | — | `200 {name, balance_microusd, held_microusd, available_microusd, updated_at}`；客户页面只显示一个「余额」数字（取 `available_microusd`），`name` 是客户可改的账户名称 |
| `PUT` | `/v1/customer/account/name` | 客户会话 | `{name}` | 客户改自己账户的名称：账户由会话确定，不能清空，非法名称 400；改的是资料，不动余额、标签与凭据（规则见 [账户名称 Spec](../specs/0003-account-names-and-login-identities.md)） |
| `GET` | `/v1/customer/ledger` | 客户会话 | 资金记录查询 | 按账户和时间区间读真实收支；筛选、翻页和响应见[客户控制台设计](0014-customer-console-navigation-and-history.md) §3 |
| `GET` | `/v1/customer/usage` | 客户会话 | 调用记录查询 | 对客状态、终态时刻、分组和翻页见[客户控制台设计](0014-customer-console-navigation-and-history.md) §2 |
| `GET` | `/v1/customer/billing` | 客户会话 | 时间区间 | 区间汇总与金额展示见[账户资金 Spec](../specs/0002-account-funds-and-reservations.md) §5 与[客户控制台设计](0014-customer-console-navigation-and-history.md) §3 |

**未认证与越权是两个不同的答复**：没有凭据（或凭据无效、已过期、已退出）访问任一客户端点 ⇒ **未认证**（401，与管理员面同一条）；凭据有效但目标数据不属于这个账户 ⇒ **不存在**（404），不返回 403、也不说明存在性（Spec §4.2、V-C10）。

**为什么把账务拆成四条而不是一个聚合响应**：汇总与明细的"口径"不同——汇总必须按区间全量算，明细按上限截断；塞进一个响应里，改一次页大小就会让"明细求和等于汇总"这条验收条件失效（Spec V-C8）。拆开之后，每条端点的语义各自稳定，页面按需组合。

**`usage` 只回对客能看的事实**：`gateway_model`、`kind`（同步生成 / 图片编辑）、`status`、`created_at`、`terminal_at`、`image_count`、`charged_microusd`——**不含 Generation Job 的标识与内部状态**。[`CONTEXT.md`](../../CONTEXT.md) 把 Generation Job 定为"对客不可见、不投射成对客协议"，所以这一条读是把执行记录**投影**成对客事实，不是把记录本身交出去；`kind` 取的是对客协议里本来就有的两类调用（`generation.jobs.branch` 的同步/编辑），不是内部任务类型。

`status` 是收敛后的四值（`succeeded` / `failed` / `pending` / `canceled`），由内部 Job 状态与结算结果映射而来；未结案的 `reconciliation_required` 对客仍是 `pending`，不提前宣告失败。映射写在拥有它的读函数上，与既有对客错误改写同一条纪律（`ADR-0017`：内部状态与渠道错误取值不进对客响应）。`failed` 只说这次未产出，不改写渠道侧的错误细节。

**汇总怎么算**：`requests` 与 `images` 按区间内到达终态的执行记录计，`charged_microusd` 按区间内入账的实际扣费与正式调整（`capture`、`adjustment`）有符号金额求和。预授权与释放不产生资金流水；跨天归属及逐笔核对见[账户资金 Spec](../specs/0002-account-funds-and-reservations.md) §5。`[since, until)` 是半开区间、按 UTC 解释。

**为什么重置令牌也走摘要入库**：它等同于一次登录凭据（能改口令），因此与会话令牌同一条纪律——明文只在响应里，库里只有 SHA-256，且有独立更短的过期（`PASSWORD_RESET_TTL_SECONDS`，默认 30 分钟），用完即删。

**对客账务读的路径与既有对客面分开**：充值记录、用量与账单走 `/v1/customer/*`。`/v1/account` 的余额字段改为已结算余额并增加可用额字段；金额语义以[账户资金 Spec](../specs/0002-account-funds-and-reservations.md) §4 为准。

### 4.3 账户读与流水端点

`GET /api/v1/accounts/{id}/entries`（管理员流水）保留分页与区间查询，条目只含实际收支；对客流水在 §4.2 的 `ledger` 里按 `WHERE account_id = <会话账户>` 收窄，不展示预授权或释放。`/v1/account` 返回三个金额字段，客户控制台只渲染 `available_microusd` 为一个「余额」数字；内部占用与已结算余额仍用于受理判定与对账。

### 4.4 页面 → 端点（逐页承接 Spec M1–M6、C5–C12）

| 页面 / 板块 | 端点 |
| --- | --- |
| M1 网关模型 | `GET /api/v1/gateway-models`；`PATCH /api/v1/gateway-models/{name}`；`PATCH /api/v1/offerings/{id}` |
| M2 发布修订 | `POST /api/v1/runtime-revisions` |
| M3 折算率 | `GET/PUT /api/v1/fx-rates`（`GET` 为本次新增） |
| M4 路由策略 | `GET/PUT /api/v1/route-policies` |
| M5 账户与密钥 | `POST /api/v1/accounts`；`GET /api/v1/accounts`（列账户，供"先搜到再操作"——见 [`0011-console-information-architecture.md`](0011-console-information-architecture.md) §5）；`GET /api/v1/accounts/{id}`；`GET /api/v1/accounts/{id}/summary`（按标识读账户摘要，账户详情直达与刷新）；`GET /api/v1/accounts/{id}/entries`；`POST /api/v1/accounts/{id}/credits`；`PUT /api/v1/accounts/{id}/tag`；`POST /api/v1/accounts/{id}/api-keys`；`DELETE /api/v1/api-keys/{key_id}`；`POST /api/v1/customers`（替客户开户）；`GET /api/v1/customers`（按邮箱找客户账户）；`GET /api/v1/customers/{id}`（按标识读客户视图，客户详情直达与刷新）；`POST /api/v1/accounts/{id}/password-reset`（为客户账户签发重置令牌） |
| M6 对账与诊断 | `GET /api/v1/reconciliation-cases`；`POST /api/v1/reconciliation-cases/{job_id}/refund`；`GET /api/v1/provider-failures`；`GET /api/v1/provider-cost-gaps` |
| 管理员登录/退出/改口令/重置 | §4.1 的六条 |
| C5 密钥自助 | §4.2 的 `api-keys` 三条 |
| C7–C10 账务 | §4.2 的 `account` / `ledger` / `usage` / `billing` 四条 |
| C4／C12 客户改口令与重置 | §4.2 的 `PUT /v1/customer/password` 与 `password-resets/redeem`；签发在 §4.1 的 `POST /api/v1/accounts/{id}/password-reset` |

### 4.5 静态产物的服务方式

前端是**一个工程、一份依赖、一条构建命令**，构建产出**两个入口产物**（`console.html` 与 `portal.html`，各自一套脚本与样式，互不引用）。两份产物由 `apps/api` 托管，**按主机名分发**：

| 主机名 | 回什么 |
| --- | --- |
| 管理主机（`admin.<domain>`） | 管理入口产物；找不到且路径不含 `.` 时回 `console.html`（深链） |
| 客户主机（`app.<domain>`） | 客户入口产物；找不到且路径不含 `.` 时回 `portal.html`（深链） |
| 其他未命中路径 | 既有的 JSON 错误体（`/api/v1/…`、`/v1/…` 的 404 语义一字不变） |

**为什么按主机名而不是按路径前缀**：两个界面是两种身份、两拨人，用两个地址（`admin.` 与 `app.`）比用 `/console/` 与 `/portal/` 更直白，也让入口产物的资源路径可以是根路径（不用给每个应用配 `base`），深链判定就是"这个主机名下的未知路径回哪个入口"。代价是两套 DNS 与证书——运营自定。

**兜底不得吃掉 API 的 404**：未注册的 `/api/v1/…` 或 `/v1/…` 路径必须仍然回既有的 JSON 错误体与 404 状态（Spec §6 要求既有 API 形状不变），不能因为兜底而回 200 与一份 HTML——那会让调用方把"路径写错了"读成"调用成功"。

### 4.6 一个工程、两个入口

目录与入口的划分见 §6；本节只定它在开发期的形态：`vite dev` 一份配置同时提供两个入口（默认端口下的 `/console.html` 与 `/portal.html`），并把 `/api` 与 `/v1` 代理到 `127.0.0.1:8081`。开发期与生产期的差别只有地址形式，页面代码不变。

**备选（落选）：前置反代提供静态产物**。理由：多一个部署单元与一份配置，而本平台的 API 与界面同源、同生命周期；等将来界面需要独立扩缩容再拆。

## 5. 实施分解（本次规划覆盖的全部工作）

`apps/web` 里已经有管理后台的六页（网关模型、发布修订、折算率、路由策略、账户与密钥、对账与诊断）与它们的取数路径；本次给这六页接上会话认证，并新增对客控制台。既有六页的行为是本轮必须不破坏的回归面。

单元按依赖排序，每个都能独立验证，且不改变既有行为。

| 单元 | 内容 | 触及 | 验证切入点 |
| --- | --- | --- | --- |
| **U1 身份与会话的读写** | 迁移新增的四张表（管理员、管理员会话、客户、客户会话）与口令重置令牌表；应用层的哈希、校验、令牌与摘要；`IdentityService` 的引导、登录、认会话、退出、改口令、重置、注册；仓储端口及其实现 | `migrations/`、`crates/application`、`crates/persistence` | 模块用例（哈希/校验/令牌/邮箱归一化/哑哈希路径/过期判定）+ 真库迁移实测 |
| **U2 管理员认证端点** | §4.1 的六条端点（登录、认身份、退出、改口令、签发重置令牌、兑换重置令牌，其中后四条都落在管理员口令上）、§4.3 的客户列表读、§4.1 的折算率读、`require_admin` 接受会话令牌、启动引导、改口令与重置都吊销该管理员全部会话 | `apps/api`、`crates/application`（清会话的端口）、`crates/persistence` | 端到端：V-A1…V-A8；含"共享令牌仍可用"的既有回归 |
| **U3 客户身份与自助端点** | §4.2 的注册/登录/退出/改口令/兑换重置令牌/密钥三件套；密钥列表的读；§4.1 的客户开户与列表读（新建账户、或给已有账户配身份）；管理端按账户签发重置令牌的端点 | `apps/api`、`crates/application`、`crates/persistence` | 端到端：V-C1…V-C5、V-C9…V-C14 |
| **U4 对客账务读** | §4.2 的 `account` / `ledger` / `usage` / `billing` 四条；按 `[since, until)` 取用量与扣费 | 同 U3 + 按账户聚合与分页的读 | 端到端：V-C6…V-C8（与账本 `capture` 逐笔对账） |
| **U5 管理员登录界面** | 登录/退出/改口令页；**路由守卫**：未认证时不发起任何管理 API 取数请求，深链与未认证访问一律落在登录页；既有六页接上会话认证；把现有单入口拆成 `console.html` 与 `src/console/`（配置加第二个入口前先完成这一步） | `apps/web`（`console.html`、`src/console`、`vite.config.ts`） | 浏览器实测：V-D1、V-D3、V-D4 + 六页读夹具数据（V-D2） |
| **U6 客户控制台与入口拆分** | 注册/登录/凭令牌重置口令；密钥自助；余额与充值记录；用量与账单；把现有单入口拆成 console.html 与 portal.html（两个入口与 vite 配置一次改到位） | `apps/web`（`portal.html`、`src/portal`） | 浏览器实测：V-D5、V-D6 + 与 U4 的接口数据一致 |
| **U7 静态产物托管与文档** | §4.5 的两个入口产物与各自的兜底、构建说明、部署说明；`apps/web/README.md` 与 `vite.config.ts` 里"单入口 + 反代回 dist"的旧说法一并改掉 | `apps/api`、`apps/web/README.md` | 构建后打开两个主机名（V-D1）+ 未注册的 API 路径仍是 JSON 404 + 越出目录的路径取不到文件 + 客户入口的脚本不含管理端代码（V-D6）。本机没有域名可指时用 `CONSOLE_DEV_HOST`（缺省关闭）打开管理端 |

## 6. 前端结构（一个工程、两个入口）

```
apps/web/
  console.html      管理入口（构建产物之一）
  portal.html       客户入口（构建产物之二）
  vite.config.ts    两个入口的构建配置（多页构建，`build.rollupOptions.input`）
  src/
    console/        运营后台：六个管理页面 + 登录/改口令 + 它自己的会话存放
    portal/         客户控制台：注册/登录/密钥/账务 + 它自己的会话存放
    shared/         两边共用的取数与展示骨架（HTTP 调用与错误解析、金额与时间格式、通用组件）
```

| 界面 | 源码 | 入口产物 | 消费者 |
| --- | --- | --- | --- |
| 运营后台 | `apps/web/src/console` | `console.html` | 管理员 |
| 客户控制台 | `apps/web/src/portal` | `portal.html` | 客户 |
| 共用骨架 | `apps/web/src/shared` | 被两个入口各自引用 | — |

**为什么做成两个入口而不是两个工程**：要的只是"客户浏览器里不出现管理端代码"（Spec D4），那是**产物**的性质。多页构建给的就是这个：两个 HTML 入口各引各的 chunk，`portal.html` 引不到 `src/console/` 的代码。两个工程则会额外付出两份依赖、两次构建、两份工具配置——那些代价换不来更多隔离。

**两个入口互不引用**：`src/console/` 不从 `src/portal/` 取任何东西，反之亦然。`shared` 只放**与身份无关**的东西——HTTP 调用与错误解析、金额与时间格式、通用展示组件；**会话的存放与"未认证时怎么办"各入口自己实现**，两边不共用会话存储的键名，因此同一个浏览器同时用两个界面也不会串。

**界面骨架一致不等于权限一致**：真正的隔离仍然靠服务端判权（管理 API 只认管理员凭据，对客端点只认客户凭据）。入口分包只解决"不该看到的东西不送到浏览器"。

## 7. 已定的三件事

这三件事曾是待裁决项；结论如下，实施按此进行。

### 7.1 两个界面的承载形态

**一个前端工程、多页构建出两个入口产物**（§6），按主机名分发（§4.5）。

### 7.2 静态产物的提供方式

**由 `apps/api` 托管**（§4.5），生产可以再叠一层反代只做 TLS 与域名，但反代不负责分发静态产物。

**代价**：界面与 API 的生命周期绑在一起（改界面要重新部署 API）。缓和：产物是独立构建的目录，部署时替换即可；等界面需要独立扩缩容或独立域名，再把分发挪到反代——届时 Spec D1 的表述要相应修订。

### 7.3 首次引导的口令放置

**环境变量 `ADMIN_EMAIL` + `ADMIN_PASSWORD`** 引导（§3.4），并且**只在账号不存在时写入**；运维登录后自行改口令。

**代价**：环境变量可能出现在编排配置或进程环境里。缓解：引导不覆盖已有口令（改过之后环境变量再泄露也没用）、口令下限、登录失败不区分原因。

**被否的备选**：只给邮箱、首次登录免密强制改口令——等于开一个"谁先登录谁就是管理员"的窗口，不可接受；运维手工写 SQL 落哈希——等于把密码学搬进部署手册，很容易写错格式。

## 8. 安全与运维约束

- **凭证纪律**（Spec §6）：口令、会话凭据、密钥明文、重置令牌都不进日志、不进响应其它字段、不进测试 fixture；错误信息只回"邮箱或口令不正确"。
- **越权一律 404**：对客端点按会话里的 `account_id` 收窄查询；查不到就是 404，不先判"这条属于谁"（避免用 403/404 的差异泄露存在性）。
- **不可区分耗时**：登录失败两条路径都走一次 argon2（§3.3）。
- **改口令与重置口令都吊销该身份的全部会话**：Spec A4/A8/C4/C12 要求"旧凭据立刻不能再用"。反过来（不吊销）会让"改了口令但别人的会话还活着"成为默认结果，那不是可接受的默认；代价是改口令的那个浏览器也要重新登录，由界面说清。
- **新旧口令相同**：允许，不单独拦。拦它要额外存一份旧哈希或做一次额外校验，而它能挡的事（反复用同一个口令）在长度下限之外没有真实收益。
- **忘记口令的出路由运营触发**（Spec A8/C12）：管理员一条由运维（共享令牌）或另一个管理员签发重置令牌；客户一条由运营在管理端按账户签发（`POST /api/v1/accounts/{id}/password-reset`）。平台**不提供**"客户自己申请重置"的未认证入口——不发邮件、不做邮箱验证的前提下，那等于"知道邮箱就能接管账户"。
- **重置令牌的存储与生命周期**：只存 SHA-256、独立更短的过期（`PASSWORD_RESET_TTL_SECONDS`，默认 30 分钟）、**一次有效**（用过写 `redeemed_at`）。行**不删**——"什么时候申请过、什么时候用过"是要留的事实；判定有效看 `expires_at > now()` 且 `redeemed_at IS NULL`。同一身份签新的重置令牌时，此前未兑换的那些**立即作废**（同一个 `subject` 只留一条未兑换令牌），免得旧令牌在运营看不见的地方继续可用。
- **会话清理**：认证时顺手删过期行；不引入定时任务（过期行留着只占空间，删它们不需要额外正确性）。
- **登录与重置都写审计**：`operations.audit_events` 记 `admin.login` / `admin.password_reset` / `customer.login` / `customer.password_reset`；会话认证的写操作填 `admin_id`，共享令牌的留空。

## 9. 验证切入点

| 手段 | 覆盖 |
| --- | --- |
| 应用层模块用例（内存替身） | 哈希与校验、令牌生成与摘要、邮箱归一化与口令下限、登录失败的同一错误、对客注册的账户-身份同事务（替身层面）、会话过期判定 |
| 端到端合同用例（真库 + 真 API 进程） | V-A1…V-A8、V-C1…V-C14：登录与失败语义（V-A1/V-A2）、会话过期与退出（V-A3）、改口令与旧会话失效（V-A4）、引导幂等（V-A5）、共享令牌回归（V-A6）、重置令牌（V-A7）、引导三种配置（V-A8）；注册与冲突（V-C1/V-C2）、密钥三件套（V-C3/V-C4）、越权 404（V-C5）、充值可见（V-C6）、用量与账本一致（V-C7）、账单口径（V-C8）、口令重置（V-C9）、未认证被拒（V-C10）、无自助申请入口（V-C11）、客户改口令（V-C12）、运营替客户开户与给已有账户配身份（V-C13/V-C14） |
| 浏览器实测（headless Chrome，本机可用） | V-D1…V-D7、V-D13：两个地址各自的登录界面、管理页读**端到端夹具自造**的数据（V-D2）、刷新保持登录与 URL 无凭据（V-D3）、未认证不发起取数请求（V-D4）、客户页面首屏余额与独立导航（V-D5）、客户入口产物不含管理端代码（V-D6）、直接打开客户页面地址（V-D13）、人工登录验收（V-D7） |
| 人工验收（浏览器，由人执行并留档） | V-D7：服务起着、用引导出来的邮箱口令登录、逐个打开六个管理页面确认数据 |

**人工验收怎么留档**：写在工单评论里（谁、什么时候、哪个地址与账号、每个页面看到什么、发现的问题与处理），不写进 Spec 或 RFC——那是交付记录，归工作项。
| 代码复核 | 凭证不进日志、静态托管只回两份 `dist` 下的文件（不得越出目录）、两份产物互不包含对方代码 |

**不做**：不为登录界面写自动化 UI 测试（本仓库没有 UI 测试框架，引一套的成本高于收益）；浏览器的往返由人工实测留档。

**会话过期怎么验**：应用层用替身覆盖时钟验判定逻辑；端到端则在真库里**直接插一条已过期的会话行**，再用它调用——不靠 `sleep` 等 TTL。两条各验一半：替身验"过期判定与清理"，端到端验"过期凭据真的被拒"。

## 10. 兼容与迁移

- **向上兼容**：新增表与端点；既有端点形状不变；`ADMIN_TOKEN` 仍可用（V-A6 是一条硬回归）。
- **历史账户配身份**：`ledger.accounts` 里已有的账户可以**由运营配上登录身份**（§2.3、`POST /api/v1/customers` 带 `account_id`），配好之后客户就能登录看自己的余额与历史；配身份不动账本、不动密钥，也不做账户间转账。
- **回滚**：`0020` 只新增表与一列，回滚即删除新增的五张表与 `audit_events.admin_id`（会话、身份与重置令牌都是新数据，删掉不影响账本与 Job）。

## 11. 开放的技术细节（留给实施）

参数命名、模块内函数划分、页面组件结构、CSS 与图表的取舍、构建产物文件名——这些不影响上文任何决定，实施时按仓库既有风格就近决定。
