# 代码结构图

> **本文的定位**：一张"哪个 crate、哪个文件、哪张表负责什么"的落点图，供人和 Agent 快速定位改动位置。
>
> **它不是合同**：分层的**职责与规则**由 [`docs/design/0004-layered-architecture.md`](design/0004-layered-architecture.md) 拥有（五层、四条规则 R1–R4、新 Provider 接入清单与合法例外 E1–E3）；持久决定由 [`docs/adr/`](adr/) 拥有。本文只做索引，**不新增决策**——要改职责边界，改 0004 或 ADR，再回来同步这里。
>
> **维护要求**：新增或移动文件、增删路由、增删表时，在同一个变更里更新本文。

## 1. 分层与依赖方向

```text
 进程         apps/api        控制面：HTTP 接口（生成、发布配置、对账）
             apps/worker     异常对账：接管过期执行、只读查询、幂等收尾
                  │ 依赖
                  ▼
 应用层       crates/application
             用例（Service）+ 端口（trait：HubRepository / ExecutionRepository /
             AdapterFactory / CredentialProvider / CacheStore）；发布校验与归一；错误→处置映射
                  │ 依赖
                  ▼
 领域层       crates/domain
             类型与规则：Job 状态机、图片分支、调用方图片落到候选参数名的映射、
             计价公式、计量证据（不依赖数据库 / HTTP / Provider）
                  ▲ 实现端口（反向依赖：基础设施依赖应用层，应用层不认识它们）
                  │
 基础设施     crates/persistence      PostgreSQL（SQL、事务、迁移）
             crates/cache-redis      Redis（加速层的键值读写）
             crates/adapter-sdk      ② 的接口与共享类型
             crates/adapter-aihubmix ② AIHubMix 一族
             crates/adapter-apimart  ② APIMart 一族
```

依赖方向是单向的：`apps/* → application → domain`，基础设施**反向实现**应用层的端口。因此换数据库、加渠道都不牵动领域层。平台**不托管静态素材**（图片按渠道原形进原形出），所以没有对象存储这一层。

## 2. 五层落在哪

| 层 | 负责什么 | 在本仓库的落点 |
| --- | --- | --- |
| **① Model Protocol** | 对外路由、请求/响应外壳、对外生命周期 | `apps/api/src/main.rs`（路由与 handler）、`crates/application` 的 `CreateImageGenerationRequest` 与 `DirectExecutionService`、`crates/domain` 的 `JobState`（Job 视图只在内部与管理员面） |
| **② Adapter Driver** | 上游路径、封装格式、响应解析、证据提取、错误分类、轮询与取图（**渠道差异只此一处**） | `crates/adapter-sdk`（接口）、`crates/adapter-aihubmix`、`crates/adapter-apimart` |
| **③ Model Profile** | 型号的**调用方参数合同**（Vendor Model 级，唯一一份）与某 Offering 能**承载**的面（能力子集）：支持参数、值域、默认值、组合规则 | 运行时发布物 `catalog.vendor_models.capability_schema`；素材在 `config/bootstrap/*.json`。**当前实现仍是"每候选各带一份"——合同与能力子集尚未拆开，见 [`docs/adr/0015`](adr/0015-vendor-model-contract-and-offering-parameter-mapping.md) 与 [`#6`](https://github.com/dehuadong/seeaihub-server-next/issues/6) 差距 G5** |
| **④ Offering** | 渠道、上游模型名、用哪个 Driver、渠道限制、路由档位与**档内权重** | `supply.offerings` + `publication.runtime_entries`；渠道与 Offering 行由服务启动时的**素材导入**写成（`crates/persistence/src/material_import.rs`，素材目录由 `SUPPLY_MATERIAL_DIR` 给，匹配键见 [`docs/design/0012`](design/0012-platform-model-publishing.md) §3）；发布逻辑在 `crates/application` 的 `RuntimeService` |
| **⑤ Price** | 渠道**计价形态**（按 token 计量量 / 按产出张数 / 按调用次数 / 上游直接给金额）、费率与单价、币种、汇率、保底与结算口径 | `supply.offerings.formula` 与 `cost_unit_price_microusd`（渠道计价事实）+ `pricing.price_plans`（**按 token 计量量计价时**的渠道成本费率）+ `publication.runtime_revisions` 的定价列（**按候选**的对客费率向量、参考成本、成本来源、价目表、保底表）+ `pricing.fx_rates`（按币种的折算率）；公式在 `crates/domain` 的 `PriceSnapshot` |

## 3. 对外接口

全部在 `apps/api/src/main.rs`。

| 方法 | 路径 | handler | 谁可以调 |
| --- | --- | --- | --- |
| GET | `/health` | `health` | 任何人（探一次事实源是否可达：只做一次 `SELECT 1`，**缓存不可用只算降级、不算不健康**——缓存不可用时系统按"没有缓存"继续服务，把它算不健康会让编排系统重启一个本来能服务的实例。探测超时由 `HEALTH_PROBE_TIMEOUT_MS` 配，缺省 2000ms；超时按不可用处理。可达回 200，不可达回 503） |
| POST | `/api/v1/accounts` | `create_account` | 管理员（`ADMIN_TOKEN`） |
| GET | `/api/v1/accounts` | `list_accounts` | 管理员（**列账户**：按创建时间倒序，可按 `tag` 精确或 `email`（大小写不敏感）收窄，两个条件是「与」。列表项带绑定的登录邮箱（没有登录身份时为 `null`）与余额；没有登录身份的账户也在列表里。只读、不写审计、不读缓存） |
| GET | `/api/v1/accounts/{account_id}` | `read_account_balance` | 管理员（读余额与写入时刻；**读数据库那一行，不读缓存**：缓存可能滞后、也可能来自对账覆盖，用它当答案会把账实不符读成账实相符。账户不存在是 404） |
| GET | `/api/v1/accounts/{account_id}/summary` | `read_account_summary` | 管理员（按账户标识读既有 `AccountSummary`，账户详情页直达与刷新用；金额仍由余额那条读给。不存在是 404） |
| POST | `/api/v1/accounts/{account_id}/credits` | `credit_account` | 管理员 |
| POST | `/api/v1/accounts/{account_id}/ledger-audit` | `trigger_ledger_audit` | 管理员（按账户触发一次账实核对：核对**在后台任务里跑**，触发即返回 202；发现不一致只建账户级案例并告警，不改账。它**没有默认周期**） |
| GET | `/api/v1/accounts/{account_id}/entries` | `list_account_entries` | 管理员（账目流水：**时间倒序**，`since` 是 RFC3339 的增量起点（不含）、`until` 是上界（不含）、`offset` 翻页、`limit` 截断（缺省 100）；响应带 `count`（本页条数）与 `total`（同一区间条件下的总条数），`truncated` 表示"这个位置之后还有没有更多"。每条带 `kind`（`credit` / `capture` / `adjustment` / `cost`，与账本存储取值同名；预授权不进流水）与金额（人民币微单位，**正负号有语义**）。读 `ledger.entries`、**不读缓存**；**只读**——不改状态、不写审计。账户不存在 404，与"还没有流水"（空数组）分开） |
| POST | `/api/v1/accounts/{account_id}/api-keys` | `issue_api_key` | 管理员 |
| POST | `/api/v1/runtime-revisions` | `publish_runtime` | 管理员（发布 Profile + Offering + Price；顶层 `gateway_model` 是**平台对客名**，缺省回退取 `native_model_id`；每个候选带**计价形态**（`formula`：`token_rates` / `per_image` / `per_call` / `upstream_declared`）与它的参数——按 token 计量量要那份四档费率（`price_plan`），按张 / 按次要单价（`cost_unit_price_microusd`），上游直接给金额两种参数都不要；另可带**定价**——对客四档 CNY 费率向量、参考成本、成本来源、价目表与保底表，修订级另带 `markup_bps`。**发布期校验：形态必填且与参数配套、每个候选声明的成本币种必须在 `pricing.fx_rates` 里有一行已生效的折算率**，否则整份发布被拒） |
| GET | `/api/v1/gateway-models` | `list_gateway_models` | 管理员（网关模型清单：对客名、运维开关、候选与承载面、**每个候选的定价**与修订级加价系数；**不回显渠道凭证**。对客名由管理员发布时自己填，平台不预设任何名字） |
| PATCH | `/api/v1/gateway-models/{gateway_model}` | `set_gateway_model_enabled` | 管理员（**只改启用开关**；没发布过的名字是 404，定义只能由发布产生） |
| GET | `/api/v1/route-policies` | `list_route_policies` | 管理员（路由策略清单：全局那条与各网关模型的覆盖；策略是**运行期配置**，不进不可变修订） |
| GET | `/api/v1/offerings` | `list_selectable_offerings` | 管理员（**可被运营选中的 Offering 清单**：按厂商分组用，给 `offering_id` 作选择键，带渠道与渠道侧模型名、驱动器、计价形态、渠道币种与费率、以及"能不能选"。**不回显渠道地址与凭证变量名**——那是渠道部署事实；**不过滤停用的**：停用的照样在列并标 `enabled: false`，运营要能看出"为什么它选不了"，见 [`0012`](design/0012-platform-model-publishing.md) §2.1） |
| PUT | `/api/v1/route-policies` | `upsert_route_policy` | 管理员（写入或覆盖一条策略：`gateway_model` 不传即全局；`strategy` 取 `priority_failover` / `weighted_random` / `least_cost` / `user_tag`，其余返回 400——落成默认会把"配置没生效"伪装成生效；`discount_rates`（候选 → 万分比）只作 `least_cost` 的比较输入、**不进成本**，`tag_channel_map`（标签 → 候选）供 `user_tag` 用。每次写入换版本标识） |
| PUT | `/api/v1/accounts/{account_id}/tag` | `set_account_tag` | 管理员（设账户标签：标签只被生效的 `user_tag` 策略消费，没有那条策略时不改变任何选路结果；账户不存在 404。**不动** `updated_at`——那列是余额最后一次变动的时刻） |
| PUT | `/api/v1/fx-rates` | `upsert_fx_rate` | 管理员（按币种录入**折算率**（渠道币种 → CNY，定点整数，分母 1e6）与生效时间，写审计；同一币种同一生效时刻只有一行，重录即改那一行。**汇率不进不可变修订**：同一时刻同一币种全平台必须是同一个数才对账得起来） |
| GET | `/api/v1/reconciliation-cases` | `list_reconciliation_cases` | 管理员（含上游对账标识） |
| POST | `/api/v1/reconciliation-cases/{job_id}/refund` | `refund_reconciliation` | 管理员（幂等退款） |
| GET | `/api/v1/provider-failures` | `list_provider_failures` | 管理员（平台侧失败清单，含渠道原始码与原文） |
| GET | `/api/v1/provider-cost-gaps` | `list_provider_cost_gaps` | 管理员（**成本缺口清单**：执行发生了、成本本该有金额却拿不到的那些执行，带上游对账标识。这些 Job **不进对账态**——对客结算照常完成，缺口是平台侧的账务缺口） |
| GET | `/v1/models` | `list_models` | 任何人（公开目录，无需鉴权；只列当前可调的**网关模型**：`name` 是平台对客名、`vendor_id` 是厂商标识，另给合同修订 `revision` 与调用方合同 `contract`；厂商原生名不进对客面，`contract` 里的型号身份已换成对客名） |
| GET | `/v1/account` | `read_own_account` | 持 Key 的账户（**只有自己的**三个金额字段：已结算余额、持有中与可用额，**分开给、不合成一个数**——合成"总资产"会让"这笔钱到底扣没扣"说不清。三个数都以**数据库**为准、不读缓存：缓存可能滞后、也可能刚被对账覆盖写回，而这条读的用途正是查看与核对） |
| POST | `/v1/images/generations` | `generate_image` | 持 Key 的账户（JSON；`model` + 平铺的模型参数 + 参考图/遮罩；幂等键走 `Idempotency-Key` 头） |
| POST | `/v1/images/edits` | `edit_image` | 同上（`multipart/form-data`；`image`/`mask` 是文件部件，文本部件也认、值按 URL/data URL 读；与上一条**同一个能力**） |
| POST | `/api/v1/admin/sessions` | `login_admin` | 公开（邮箱 + 口令 → 会话；邮箱不存在与口令不对回同一个答复，两条路都走一次口令哈希） |
| GET / DELETE | `/api/v1/admin/session`、`/api/v1/admin/sessions` | `read_admin_session`、`logout_admin` | **仅会话**（共享 `ADMIN_TOKEN` 不指向任何管理员，在这些端点上被拒） |
| PUT | `/api/v1/admin/password` | `change_admin_password` | **仅会话**（需当前口令；成功后该管理员**全部**会话失效） |
| POST | `/api/v1/admin/password-resets`、`…/redeem` | 签发 / 兑换重置令牌 | 前者认会话或共享令牌（运维自救入口），后者**无需凭据**——口令重置的意义就是"进不去了"；令牌一次性、只存摘要 |
| POST / GET | `/api/v1/customers`、`/api/v1/customers/{…}` | `open_customer`、`list_customers` | 管理员（替客户开户：新建账户或给**已有账户**配登录身份；按邮箱找账户，给客户充值/签重置令牌都要那个标识） |
| GET | `/api/v1/customers/{customer_id}` | `read_customer_view` | 管理员（按客户标识读既有 `CustomerView`，客户详情页直达与刷新用；不含口令、会话与令牌。不存在是 404） |
| POST | `/api/v1/accounts/{account_id}/password-reset` | `issue_customer_password_reset` | 管理员（为客户账户签一次性重置令牌；该账户没有登录身份时 404） |
| GET | `/api/v1/fx-rates` | `list_fx_rates` | 管理员（每个币种**当前生效**的那一行，供折算率页显示录入结果） |
| POST | `/v1/customers`、`/v1/customer/sessions` | `register_customer`、`login_customer` | 公开（客户自助注册与登录） |
| GET / DELETE | `/v1/customer/api-keys`、`/v1/customer/api-keys/{key_id}` | 客户自助密钥 | 客户会话（列：**不含明文**；建：明文只此一次；吊销：不属于自己的回 404） |
| PUT | `/v1/customer/password` | `change_customer_password` | 客户会话（需当前口令；成功后该客户全部会话失效） |
| POST | `/v1/customer/password-resets/redeem` | `redeem_customer_password_reset` | 无需凭据（**凭令牌**；对客面没有"提交邮箱就拿到令牌"的入口——不发邮件时那等于知道邮箱就能接管账户） |
| GET | `/v1/customer/account`、`/ledger`、`/usage`、`/billing` | 对客账务读 | 客户会话（账户读返回已结算余额、持有中与可用额三个字段，页面只显示一个标题为「余额」的**可用额**数字；用量是执行记录的**对客投影**，不含 Job 标识与内部状态，已完成按终态时刻、处理中按受理时刻；账单按 `[since, until)` 全量算、已完成请求与张数按终态时刻，扣费总额只计 `capture` 与 `adjustment`） |

对客的**生成面只有这两条路径**，都是**同步**：一个请求把图交回，没有 202 受理、没有 job_id 轮询。**分支由请求内容决定**（有没有参考图/遮罩），不按端点断言——带图的 generations、不带图的 edits 都合法。参考图与遮罩用**公网 URL 或 `data:image/…;base64,…`** 给出（`image` 与 `image_urls` 同义、二选一）；平台**不落盘**：不下载归档、不解码存储，渠道给 `url` 就给 `url`、给 `b64_json` 就给 `b64_json`，原样放进 `data[]`，由客户端判断。成功响应 `{created, data:[{url|b64_json}]}`；内部受理后等 Job 到终态（上限 `GENERATION_SYNC_WAIT_SECONDS`，默认 120s），等不到就按失败回超时错误。

**钱的两条线**（设计口径见 `docs/design/0007` §1–§8）：**对客只有 CNY 单币种**——售价是**对客价目**（token 四档向量；初始值按该 vendor/模型已知渠道价目 × 倍率 × 折算率推导、运营可改）（按 token 是 `consumer_rates_cny`；上游金额形态按声明额 × 冻结倍率 × 冻结折算率；都随 Job 的 Price Snapshot 冻结、结算只读它），**预授权额**按**供给（vendor + offering）维度**的保底表（`floor_amounts`）查得——**每张额**，受理时 `hold = n × 每张额`（`n` 缺省 1）——**像素型的 `size` 先归到档位**（优先用该供给发布的档位像素表、缺失时按最长边阈值兜底，`size = auto` 取默认档 2K），**不由售价派生**；受理闸门是**余额 ≥ n × 每张保底额**（不成立即 402 `insufficient_balance`），**结算按实际扣、不封顶在保底额**（实收超过保底额时余额被扣成负数——**透支发生在结算**，随后按当时余额判）。`GENERATION_MAX_COST_MICROUSD` 只作**连该供给的封顶保底值都查不到时**的兜底保底额，**不再是受理上限**。**成本平面按该供给声明的成本币种**记原币种原值，用受理时冻结的**折算率**（`pricing.fx_rates` 里"受理时刻生效的那一行"）折成 CNY 只用于毛利核算；成本怎么算由该供给的**计价形态**决定（上游给了金额就先取它，否则按形态自算），`pricing.price_plans` 的费率收窄为**渠道成本费率**、且只是"按 token 计量量计价"这一种形态的参数，不再是对客结算基数。

请求体上限 16MB（`DefaultBodyLimit`）。这里的 `job_id` 是**内部**执行/审计记录的标识，只在内部与管理员的运营接口出现。

`GET /api/v1/provider-failures` 的 `kind` 取值与落库值同名（`crates/adapter-sdk` 的 `ProviderFailureKind`）：它同时是 DB CHECK 的取值集合与查询参数取值，改枚举名即改接口。不传 `kind` 时只列**平台侧事件**；响应形如 `{failures, count, truncated}`——不翻页，所以必须让调用方看得出被截断。

## 4. 一次生成请求怎么走

```text
消费侧
  │ POST /v1/images/generations 或 /v1/images/edits   apps/api/src/main.rs  generate_image / edit_image
  ▼
入口中间件：认证、速率与读取准入都发生在消费正文之前，
  取不到读取许可直接拒绝，不排队                        apps/api/src/main.rs  require_generation_access
  ▼
DirectExecutionService::execute                        crates/application  direct_execution.rs
  ├─ 校验幂等键；同键预查命中即按原记录投影，不新建、不再占用   crates/persistence  ExecutionRepository::lookup_execution
  ├─ 判定图片分支（文生图 / 图生图 / 带遮罩）           crates/domain  CreateImageGenerationRequest::branch
  ├─ 按模型读候选与合同，过滤出已识别参数并算请求指纹     crates/application  direct_execution.rs
  ├─ 取该型号的候选供给，按档位选第一个有合格候选的档，      crates/application  select_candidate
  │    再在该档的合格候选里按 weight 确定性分摊
  │    （落点 = hash(账户 ‖ 幂等键)，不引入随机数；不合格的候选不进分摊；
  │      调用方的图在内存里落到候选声明的参数名上）
  ├─ 受理时冻结定价：先把 size 归到档位、再查该供给    crates/application  freeze_offering_pricing
  │    的保底表（像素型按档位像素表/最长边，auto 取 2K）；
  │    汇率按该候选的成本币种取"受理时刻生效的那一行"
  ├─ 读当天已结算实收，超每日上限即 429，不建任何记录    crates/persistence  daily_spend_microusd
  ├─ admit（同一事务）：幂等判定、资金闸门、账户在飞名额与
  │    渠道全局槽位、最小 Job + 路由判定 + 占用          crates/persistence  ExecutionRepository::admit
       （Job 是内部执行/审计记录，对客不可见；占用额 = 保底额，
         闸门是可用额 ≥ 保底额，不足即 402）
  ├─ 提交后把**变更后**的余额快照（余额 / 占用 / 可用额 / 版本）写进缓存（写穿）   crates/application  AccelerationService::write_balance
  ├─ begin_submission（写 executing + submitting Attempt 与租约）之后才发外部请求   crates/persistence  ExecutionRepository::begin_submission
  ├─ GatewayAdapter::execute ──────────────────────────►  crates/adapter-aihubmix / adapter-apimart
  │     载荷只在内存：data URL 就地解码、公网 URL 取用或透传、提交、轮询、抽计量证据、分类错误
  ├─ 成功：settle（写计量证据、渠道成本事实含折算后 CNY、按实际扣费、结清预授权、
  │    释放渠道槽位、写产出张数），再在内存里归一回 { created, data:[{url|b64_json}] }
  │    （**不封顶在保底额**：差额由余额透支吸收）
  └─ 失败：按 RetrySafety 与责任方处置                  crates/application
       ├─ 对客码按责任方派生（渠道码与原文只在内部使用）    crates/application  public_error_code
       ├─ 可证明未受理 / 确定性拒绝 → 同一请求内换新 Attempt 有界重投，或 failed + 释放预授权
       ├─ 结果交付失败 → reconciliation_required + 保留预授权（**成本事实一并落库**）
       └─ 不确定是否已受理 → reconciliation_required + 保留预授权与渠道槽位（人工处置）
  │
  ▼ 执行期间不持有数据库连接与事务；同步门面等结果后回 { created, data:[{url|b64_json}] }
     跨进程恢复：Worker（独立进程，只跑异常对账循环）      apps/worker/src/main.rs
       └─ ExecutionReconciliationService::run_once      crates/application  execution_reconciliation.rs
            ├─ takeover_expired_executions（比较并交换所有权，接管才让 fencing token 加一）
            ├─ 有句柄只读查询同一任务：按证据 settle，或按确定失败 fail_or_reconcile
            ├─ reap_unsubmitted_admissions（回收从未写下提交声明的孤儿）
            └─ 领取并消费晚到事实、补终态后的成本缺口、慢周期账务核对
     GET /api/v1/reconciliation-cases                    对账清单（含上游对账标识，管理员面）
     POST /api/v1/reconciliation-cases/{job_id}/refund    幂等退款，只释放预授权（管理员面）
     GET /api/v1/provider-failures                       平台侧失败清单（欠费/凭证/平台 bug，管理员面）
     GET /api/v1/provider-cost-gaps                      成本缺口清单（成本未知的那些执行，管理员面）
     PUT /api/v1/fx-rates                                录入折算率（渠道币种 → CNY，管理员面）
     定时缓存对账（API 进程内的独立任务）                  以数据库为准覆盖余额、清理陈旧 route 条目，发现不一致写审计  crates/application  AccelerationService::run_reconciler
```

## 5. 谁拥有哪张表

| Schema / 表 | 是什么事实 | 写入方 |
| --- | --- | --- |
| `catalog.vendor_models` | ③ Profile（Capability Schema，随修订不可变） | `RuntimeService::publish` |
| `supply.channels` / `supply.offerings` | ④ 渠道与供给；供给上另记**计价形态**（`formula`）与按张 / 按次的**单价**（`cost_unit_price_microusd`）——渠道事实，决定成本怎么算 | `RuntimeService::publish` |
| `pricing.price_plans` | ⑤ 渠道**成本费率**（四档 token 单价，币种按该渠道声明）：**只有"按 token 计量量计价"的供给有**，定价时的参考口径与毛利核算用，**不再是对客结算基数** | `RuntimeService::publish` |
| `pricing.fx_rates` | ⑤ **折算率**（按币种的"渠道币种 → CNY"定点比值 + 生效时间）：受理时取"受理时刻生效的那一行"并快照。**外部事实，由管理员录入**，不进不可变修订 | `PricingService::upsert_fx_rate`（`PUT /api/v1/fx-rates`） |
| `publication.runtime_revisions` / `runtime_entries` | 哪次发布生效、各型号的活动供给、优先级与**档内权重**（`routing_priority` 是档位，同档允许多条候选，档内按 `weight` 分摊）；修订上另记这次发布定义的是哪个**网关模型**（对客名）与它指向哪一行厂商模型合同，以及**定价**（修订级 `markup_bps` + **按候选键**的参考成本、成本币种、对客四档 CNY 费率向量、**对客计价形态**、成本来源、档位价目表、保底表）；旧修订的定价列留 NULL ⇒ 受理与结算走旧口径 | `RuntimeService::publish` |
| `routing.route_policies` | **路由策略**：在已发布的合格候选里"挑哪一条"的运行期配置（全局一条 + 按网关模型覆盖），含**折扣率表**（只作 `least_cost` 的比较输入，不进成本）与**标签映射**（供 `user_tag`）。**运营配置，不进不可变修订**；改它即刻影响之后的受理，已受理 Job 不受影响 | `RoutePolicyService::upsert`（`PUT /api/v1/route-policies`）、受理时取生效那条 |
| `publication.gateway_models` | 网关模型的**运维开关**：这个名字现在开着吗、谁在什么时候改的。**定义不在这里**（候选集、合同、定价只在不可变修订里） | `RuntimeService::publish`（首次发布落行）、`set_gateway_model_enabled` |
| `generation.jobs` | 受理时的请求摘要与分支、所选供给、**冻结的定价快照**（对客费率向量、保底额与来源、命中的候选、折算率）、**产出张数**（`image_count`：结算时按内存里的实际张数写入，拿不到留 NULL、读取按 0）、对客错误码与平台侧失败类别，以及**终态时刻**（`terminal_at`：成功/失败/人工解除对账时与那次状态变更同事务落；对账态不是终态，留空）。记录另有执行所有权与租约（`execution_owner` / `fencing_token` / `lease_expires_at`）与上游任务句柄（`provider_task_handle`）；请求参数、参考图/遮罩与结果信封都不落库，**对客不可见** | `ExecutionRepository::admit`、`ExecutionRepository::begin_submission` / `record_acceptance` / `settle` / `fail_or_reconcile`、`refund_reconciliation`、`ExecutionRepository::renew_execution_ownership` / `takeover_expired_executions` / `reap_unsubmitted_admissions` |
| `generation.routing_decisions` | 受理时为什么选了它（候选、档位、**权重**、是否合格、本次**分流落点**） | 与 Job 同事务写入 |
| `generation.attempts` | 一次执行尝试：阶段（prepared/submitting/accepted/terminal/unknown）、对账标识（`provider_trace_id`）、**计量证据**、**渠道成本事实**（来源 `computed`/`declared`/`unavailable` + 原币种金额 + 该渠道声明的币种 + **按冻结汇率折算后 CNY**）。渠道原始错误码与原文不落在这里，那两列对存活的执行记录保持 NULL。成功、"结果交付失败进对账"、Driver 在终态之后判定失败三条路径都写成本事实（拿不到事实时写 `unavailable`）；只有请求根本没交到渠道的执行四列留空 | `ExecutionRepository::begin_submission` / `record_acceptance` / `settle` / `fail_or_reconcile` / `record_terminal_provider_cost` |
| `generation.execution_capacity` | **渠道全局未决任务槽位**：与受理同事务获取，唯一关联 Job；确定终态才释放，从未写下提交声明的孤儿由回收释放（账户执行名额不在此表，沿用既有在飞计数） | `ExecutionRepository::admit`（获取）、`settle` / `fail_or_reconcile`（确定终态释放）、`reap_unsubmitted_admissions`（回收孤儿） |
| `generation.late_facts` | **晚到事实收件箱**：原提交者在执行 token 失效后交付的有界 task handle 或计量/成本事实，只存 Spec 0005 §2 允许的最小事实；`claimed_by` / `claimed_at` 是领取标记（领取超时可重领），只有消费成功才写 `consumed_at` | `ExecutionRepository::offer_late_facts`（写入）、`claim_unconsumed_late_facts` / `mark_late_fact_consumed`（异常对账领取与消费） |
| `ledger.accounts` / `ledger.holds` / `ledger.entries` | 已结算余额、占用、预授权与账目。账户行另存**占用合计**（`held_microusd`，= active 预授权之和）与**版本**（`version`，每次金额变化 +1）；可用额 = 余额 − 占用合计，不落第三份。账户分**消费者**与**平台**两类（`kind`）：消费者账户有 API Key 与余额缓存；平台账户只有一行（`migrations/0019_ledger_platform_cost.sql` 种下的固定 id），承载**平台自担的上游成本**，无缓存、无预授权。**余额与可用额可为负**（透支发生在结算：实收超过保底额时差额把余额扣成负数；平台账户的余额就是它承担的累计成本），**保底额可为 0**。资金流水科目：`credit` / `capture` / `adjustment` / `cost`——预授权只留在 `ledger.holds`（`active` / `captured` / `released`），不进流水 | `ExecutionRepository::admit`（占用：`held += 保底额`）、`ExecutionRepository::settle`（`balance -= 实收`、`held -= 保底额`）、`ExecutionRepository::fail_or_reconcile`（`held -= 保底额`，以及**成本条目**：终态失败且这次执行有折算后 CNY 成本时，记一条 `cost` 挂平台账户）、`refund_reconciliation`（`held -= 保底额`） |
| `ledger.daily_spend` | **每账户每 UTC 自然日一行的已完成实收合计**（正数）：每日消费限额的判据——受理只读当天一行，不扫历史流水 | `ExecutionRepository::settle`（与 `capture` 同事务累加） |
| `identity.api_keys` | API Key 摘要 | `IdentityService` |
| `operations.reconciliation_cases` / `audit_events` | 待人工处置的案例与审计（审计也承载缓存对账发现的覆盖） | `ExecutionRepository::fail_or_reconcile`、`ReconciliationService`、`AccelerationService`（`insert_audit_event`）、`LedgerAuditor`（按账户触发的账实不符案例） |

`crates/persistence` 是这些表的唯一写入方；其他 crate 只能通过 `crates/application` 的 `HubRepository` / `ExecutionRepository` 端口访问，不直接写 SQL。

## 6. 文件职责清单

| 文件 | 负责什么 | 明确不负责 |
| --- | --- | --- |
| `apps/api/src/main.rs` | HTTP 路由与 handler、鉴权中间件、请求/响应形状、启动时跑迁移、装配加速层并挂起缓存对账循环 | 业务规则、SQL、上游调用 |
| `apps/api/src/supervisor.rs` | 直接同步执行的本机所有权与许可：执行 slot、内存字节预算、读/发送 permit、one-shot 结果通道、断开释放图片、正文慢读期限、停机排空；成功响应的许可随 body 持有到发送完成；每次执行另起独立续约任务（间隔为租约的三分之一），续约冲突立即取消该次执行 | 业务规则、SQL、上游调用、期限预算与结算（在应用层） |
| `apps/worker/src/main.rs` | 进程外壳：读环境变量、装配端口实现（含加速层与告警出口）、只跑异常对账循环（`ExecutionReconciliationService::run_once`）、优雅退出 | 生成流程本身（在 `DirectExecutionService`） |
| `apps/api/tests/http_contract/` | 端到端合同测试：真实空库 + 真实 API/Worker 进程 + **进程内假上游**与**进程内假 Redis**（零外部费用）。`main.rs` 是模块根，`harness.rs` 是共享装置，`cases_*.rs` 是按主题分的用例，`harness_check.rs` 是夹具自身的检查（不启进程、不用库） | 单元测试（在各 crate 内） |
| `crates/*/src/tests.rs`、`crates/*/src/<模块>/tests.rs`、`crates/application/src/tests/` | 各 crate 的单元测试（`crates/application` 的按主题分在 `src/tests/` 下）。落点约定见根 [`AGENTS.md`](../AGENTS.md) 的「通用约定」 | 端到端合同测试（在 `apps/api/tests/http_contract/`） |
| `crates/domain/src/execution_protocol.rs` | 同步网关执行的最小事实：`ExecutionStage`（admitted/executing/succeeded/failed/reconciliation_required）、`AttemptStage`（prepared/submitting/accepted/terminal/unknown）、`FencingToken`、`ProviderTaskState` 与有界 Provider 标识校验（`MAX_PROVIDER_IDENTIFIER_BYTES` / `is_bounded_provider_identifier`） | 持久化、HTTP 与 Provider 细节 |
| `crates/application/src/request_fingerprint.rs` | 幂等键不可逆标识（无密钥 SHA-256）与请求指纹（带密钥 HMAC-SHA256、按版本轮换的指纹密钥）及规范化（端点/型号/已识别参数/参考图与 mask/n） | 具体请求解析与选路 |
| `crates/adapter-sdk/src/gateway.rs` | 新协议 Adapter 生命周期接口：`InputImage`/`GatewayInput`、`ExecutionContext`/`AcceptedHandle`、`GatewayAdapter`、`ProviderOutput`/`AccountingFacts` | 具体渠道协议与平台财务规则 |
| `crates/domain/src/lib.rs` | `JobState` 状态机、`ImageBranch`、`OfferingCandidate`（档位 `routing_priority` 与**档内权重** `weight`）、`PricingFormula`（计价形态：按 token 计量量 / 按张 / 按次 / 上游直接给金额）、`PriceSnapshot`（成本计价形态与其成本单价、对客计价形态与对客费率向量 / 成本费率 / 保底额 / 折算率 / 成本来源）、`resolve_size_tier` 与 `FloorTable`（像素型 `size` 归位 + 保底表查表与回落链）、`FxRate` 定点折算、`TokenUsage` / `MeteringEvidence` | IO、持久化 |
| `crates/domain/src/image_parameters.rs` | 图片参数的**唯一**一份规则：调用方契约字段（`image`/`image_urls`/`mask`）、候选声明参数名的判定（名字以 `image` 开头＝参考图、含 `mask`＝遮罩）、`null`/空串＝这一处没有图、把调用方的图落到候选声明的参数名上 | IO；也不认识任何**具体渠道**（参数名本身按 [`docs/adr/0015`](adr/0015-vendor-model-contract-and-offering-parameter-mapping.md) 应来自 Vendor Model Contract；当前实现里它是渠道原生名，属 [`#6`](https://github.com/dehuadong/seeaihub-server-next/issues/6) 差距 G1） |
| `crates/application/src/lib.rs` | 用例（`IdentityService` / `RuntimeService` / `DirectExecutionService` / `ExecutionReconciliationService` / `ReconciliationService` / `PricingService` / `AccountsService`）、端口 trait（含 `CacheStore`）、发布期校验（含"限制只能收窄"与定价"全有或全无"）、**成本护栏**（`cost_ceiling.rs`：发布期按合同允许的最大输出张数判、受理期按本次请求兜底判）、候选选择（**先按档位取第一个有合格候选的档，再在档内按权重确定性分摊**）、**受理时冻结定价与保底额**、结算与成本折算、错误→处置映射与对客错误码派生、加速层语义（`AccelerationService`：余额快照的键名与值形状、写穿与来源标记、**版本闸门**（低版本快照不得覆盖高版本）、缓存对账与 route 条目清理、**每把 API Key 的速率计数**） | SQL、HTTP、上游协议、Redis 命令 |
| `crates/application/src/direct_execution.rs` | 同步网关直接执行用例：指纹/幂等摘要 → 选路与冻价 → admit → begin_submission → GatewayAdapter::execute → 结算或处置；请求内 SafeBeforeAcceptance 有界重投、结算提交未知先 read_finalization 确认、收尾提交后写穿余额；预算 D 减 R 与 SupervisedExecutionContext；首次提交声明落库后经 ExecutionOwnershipRegistrar 把所有权交给调用方起续约 | SQL、HTTP、渠道协议 |
| `crates/application/src/execution_reconciliation.rs` | 异常对账 Worker 用例：接管过期 v1 执行，按 `operations.reconciliation_cases` 的 `next_query_at`/`attempts` 做退避与重试上限内的有界只读查询，按证据幂等结算或建缺口/案例，回收未提交孤儿，领取并消费晚到事实、补终态后的成本缺口，慢周期账务核对 | SQL、HTTP、渠道协议、生成请求 |
| `crates/persistence/src/lib.rs` | `PgHubRepository`：SQL、事务边界、迁移、行↔领域类型映射；余额变更一律用 `RETURNING` 把**提交后**的余额带回给用例（供写穿缓存） | 业务判定（只执行用例给出的结论） |
| `crates/cache-redis/src/lib.rs` | 加速层的 Redis 实现：`GET` / `SET … PX` / `DEL` 三条命令、惰性连接与单次操作超时；连不上或命令报错一律返回错误，由用例层当"未命中"处理。`REDIS_URL` 为空时不构造（`from_env` 返回 `None`） | 键名、值形状与版本判定（都在 `crates/application`） |
| `crates/alert-webhook/src/lib.rs` | 平台故障告警出口的 HTTP 实现：把一条 `PlatformAlert` 以 POST JSON 发出、有界超时与重试。`PROVIDER_ALERT_WEBHOOK` 为空时不构造（`from_env` 返回 `None`），地址不可用时构造即失败 | 何时告警、告警内容、发送失败如何收口（都在 `crates/application` 的 `PlatformAlerter`） |
| `crates/adapter-sdk/src/lib.rs` | ② 的接口与共享类型：`ImageAdapter`、`AdapterDescriptor`、`PreparedImageRequest`、结果信封（`url` 或 `b64_json` 恰好其一）、data URL 解码、`ProviderSuccess`、**成本事实报告**（`ProviderCost` 三态 `declared` / `computed` / `unavailable`，成员名与领域取值逐字同名，转换只此一处）、`ProviderCallError`（**失败件同样带成本事实报告**：终态之后判定失败时把已经读到的成本随错误交回平台）、`RetrySafety` 三态、`ProviderFailureKind` 平台侧失败类别 | 任何具体渠道的协议细节 |
| `crates/adapter-aihubmix/src/lib.rs` | AIHubMix 一族：端点分流（`/v1/images/generations` 与 `/v1/images/edits`）、multipart 封装（参考图需字节：data URL 就地解码，公网 URL 由它自己取）、结果原样交回、响应头 `x-request-id`（有则采集为对账标识）、错误分类、**成本报告"这条渠道不给金额字段"**（成本由平台按实际用量自算） | 平台侧的生命周期与计费规则 |
| `crates/adapter-apimart/src/lib.rs` | APIMart 一族：任务式（提交 → 轮询，**只在 Adapter 内部**）、公网参考图逐字透传、data URL 就地解码后上传换 URL 再回填、四分项计量证据的读取、错误分类与 `SafeBeforeAcceptance`；终态里的 `cost` **采纳为成本事实**（精确换成微单位、币种用渠道声明；缺字段 / 负数 / 解析失败一律按"拿不到"报告，不猜）。`credits_cost` 仍不采纳 | 同上；金额只进成本口径，不替代计量事实、不参与对客金额 |
| `migrations/0001_initial.sql`…`0019_ledger_platform_cost.sql` | 表结构与约束（含"每型号每个网关模型下同一条供给只允许一条活动条目"、路由判定表、对客错误码白名单与失败类别取值），以及增量迁移：撤销资产表与列（`0005`）、合同与承载面拆分（`0006`）、网关模型命名两列与开关表（`0007`）、执行尝试上的成本四列与其同形约束（`0008`）、**汇率表 + 修订上的定价七列 + 放宽三处余额/预授权约束**（`0009`：余额可为负、保底额与预授权额可为 0）、**候选上的档内权重 + 唯一索引换成 `(gateway_model, offering_id) WHERE active`**（`0010`：同档允许多条候选）、**路由策略表 `routing.route_policies`**（`0011`：作用域唯一，策略类型只放本层已实现的取值）、**策略的第二批输入**（`0012`：放宽策略取值面加入 `least_cost` 与 `user_tag`、策略上增折扣率表与标签映射、账户上增标签列）、**供给上的计价形态与单价 + 放开 `runtime_entries.price_plan_id` 非空**（`0013`：渠道不按 token 计量量计价时没有 Price Plan）、**渠道与供给的身份唯一索引**（`0014`：发布按身份复用既有行，停用不再被重发写回）、**Job 上冻结供给身份**（`0015`：`adapter_key` 与 `provider_model_id`）、**Job 上冻结渠道端点**（`0016`：`base_url` 与 `credential_env`）、**对账案例的账户维度**（`0017`：`job_id` / `attempt_id` 放开非空并加 `account_id`，账户级账实案例没有 Job）、**一个 Job 允许多次执行**（`0018`：去掉 `UNIQUE (job_id)`、加 `attempt_no` 与 `UNIQUE (job_id, attempt_no)`）、**账户类别与平台账户 + 账本科目增 `cost`**（`0019`） | 运行时的业务规则 |
| `config/bootstrap/*.json` | 可直接发布的运行时素材（Profile + Offering + Price 三合一） | 不是运行时数据源：必须经发布接口写入 |
| `scripts/decisions/*.mjs` | Agent Notes 的目录、元数据与文件格式检查（不生成索引），自述与本地修补见该目录 `README.md` | 不影响服务运行 |
| `docs/AGENTS.md`、`docs/agents/git.md` | 文档分层、位置、职责与写法；提交、推送与历史改写约定 | Agent Note 的生命周期与文件骨架归 `.agents/notes/README.md` |
| `docs/design/`、`docs/adr/` | 设计与决策的权威位置 | — |
| `docs/facts/channel-facts.md` | 各渠道的**事实台账**（端点、参数、计量与成本口径、实测记录） | 不是接口合同，服务不读取 |
| `docs/verification/` | 受控验证清单（步骤、停止条件、留档要求） | — |
| `out-reference/` | 上游原始材料（文档、Schema 快照、实测响应） | 不属于平台合同，服务不读取 |
| `.agents/notes/` | 工程变更与交付记录；生命周期、文件骨架与写作规则见该目录的 `README.md` 与 `AGENTS.md` | — |

## 7. 扩展点

- **新增一个渠道族**：在 `crates/adapter-*` 加 Driver（②），发布新的 Profile/Offering（③④），不动 ① 与领域层——判据与合法例外见 0004 §4。
- **新增一个型号或调整参数面**：只发布新的运行时素材（`config/bootstrap/*.json` 的形状），不重新编译。
- **参考图的形态差异**（公网 URL / data URL、上游要 URL 还是要字节）：只在 ② Adapter 内部吸收，平台不搬运、不托管；判据见 [`docs/adr/0019`](adr/0019-images-pass-through-without-asset-storage.md)。
- **参数合同的归属**：调用方按 **Vendor Model Contract** 提交参数；同一 Vendor Model 在不同 Provider 的字段/位置/枚举差异由 **Offering Parameter Mapping** 在平台内部吸收。决策见 [`docs/adr/0015`](adr/0015-vendor-model-contract-and-offering-parameter-mapping.md)。
- **面向消费侧的跨厂商统一简化接口**：**不在本仓库内部**，属后期独立规划（见 [`docs/adr/0015`](adr/0015-vendor-model-contract-and-offering-parameter-mapping.md)）。**注意：这与上面那条不是同一件事**——"按各模型自己的合同提交"不等于"所有厂商共用一套字段"。
