# 代码结构图

> **本文的定位**：一张"哪个 crate、哪个文件、哪张表负责什么"的落点图，供人和 Agent 快速定位改动位置。
>
> **它不是合同**：分层的**职责与规则**由 [`docs/design/0004-layered-architecture.md`](design/0004-layered-architecture.md) 拥有（五层、四条规则 R1–R4、新 Provider 接入清单与合法例外 E1–E3）；持久决定由 [`docs/adr/`](adr/) 拥有。本文只做索引，**不新增决策**——要改职责边界，改 0004 或 ADR，再回来同步这里。
>
> **维护要求**：新增或移动文件、增删路由、增删表时，在同一个变更里更新本文。

## 1. 分层与依赖方向

```text
 进程         apps/api        控制面：HTTP 接口（生成、发布配置、对账）
             apps/worker     执行面：领取 Job、调上游、结算
                  │ 依赖
                  ▼
 应用层       crates/application
             用例（Service）+ 端口（trait：HubRepository /
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
| **① Model Protocol** | 对外路由、请求/响应外壳、对外生命周期 | `apps/api/src/main.rs`（路由与 handler）、`crates/application` 的 `CreateImageGeneration`、`crates/domain` 的 `JobState`（Job 视图只在内部与管理员面） |
| **② Adapter Driver** | 上游路径、封装格式、响应解析、证据提取、错误分类、轮询与取图（**渠道差异只此一处**） | `crates/adapter-sdk`（接口）、`crates/adapter-aihubmix`、`crates/adapter-apimart` |
| **③ Model Profile** | 型号的**调用方参数合同**（Vendor Model 级，唯一一份）与某 Offering 能**承载**的面（能力子集）：支持参数、值域、默认值、组合规则 | 运行时发布物 `catalog.vendor_models.capability_schema`；素材在 `config/bootstrap/*.json`。**当前实现仍是"每候选各带一份"——合同与能力子集尚未拆开，见 [`docs/adr/0015`](adr/0015-vendor-model-contract-and-offering-parameter-mapping.md) 与 [`#6`](https://github.com/dehuadong/seeaihub-server-next/issues/6) 差距 G5** |
| **④ Offering** | 渠道、上游模型名、用哪个 Driver、渠道限制、路由档位与**档内权重** | `supply.offerings` + `publication.runtime_entries`；发布逻辑在 `crates/application` 的 `RuntimeService` |
| **⑤ Price** | 计价形态、费率、币种、汇率、保底与结算口径 | `pricing.price_plans`（渠道成本费率）+ `publication.runtime_revisions` 的定价列（**按候选**的对客费率向量、参考成本、成本来源、价目表、保底表）+ `pricing.fx_rates`（按币种的折算率）；公式在 `crates/domain` 的 `PriceSnapshot` |

## 3. 对外接口

全部在 `apps/api/src/main.rs`。

| 方法 | 路径 | handler | 谁可以调 |
| --- | --- | --- | --- |
| GET | `/health` | `health` | 任何人 |
| POST | `/api/v1/accounts` | `create_account` | 管理员（`ADMIN_TOKEN`） |
| GET | `/api/v1/accounts/{account_id}` | `read_account_balance` | 管理员（读余额与写入时刻；**读数据库那一行，不读缓存**：缓存可能滞后、也可能来自对账覆盖，用它当答案会把账实不符读成账实相符。账户不存在是 404） |
| POST | `/api/v1/accounts/{account_id}/credits` | `credit_account` | 管理员 |
| POST | `/api/v1/accounts/{account_id}/api-keys` | `issue_api_key` | 管理员 |
| POST | `/api/v1/runtime-revisions` | `publish_runtime` | 管理员（发布 Profile + Offering + Price；顶层 `gateway_model` 是**平台对客名**，缺省回退取 `native_model_id`；每个候选可带**定价**——对客四档 CNY 费率向量、参考成本、成本来源、价目表与保底表，修订级另带 `markup_bps`。**发布期校验：每个候选声明的成本币种必须在 `pricing.fx_rates` 里有一行已生效的折算率**，否则整份发布被拒） |
| GET | `/api/v1/gateway-models` | `list_gateway_models` | 管理员（网关模型清单：对客名、运维开关、候选与承载面、**每个候选的定价**与修订级加价系数；**不回显渠道凭证**。对客名由管理员发布时自己填，平台不预设任何名字） |
| PATCH | `/api/v1/gateway-models/{gateway_model}` | `set_gateway_model_enabled` | 管理员（**只改启用开关**；没发布过的名字是 404，定义只能由发布产生） |
| GET | `/api/v1/route-policies` | `list_route_policies` | 管理员（路由策略清单：全局那条与各网关模型的覆盖；策略是**运行期配置**，不进不可变修订） |
| PUT | `/api/v1/route-policies` | `upsert_route_policy` | 管理员（写入或覆盖一条策略：`gateway_model` 不传即全局；`strategy` 只接受**本层已实现**的取值（`priority_failover` / `weighted_random`），其余返回 400——落成默认会把"配置没生效"伪装成生效。每次写入换版本标识） |
| PUT | `/api/v1/fx-rates` | `upsert_fx_rate` | 管理员（按币种录入**折算率**（渠道币种 → CNY，定点整数，分母 1e6）与生效时间，写审计；同一币种同一生效时刻只有一行，重录即改那一行。**汇率不进不可变修订**：同一时刻同一币种全平台必须是同一个数才对账得起来） |
| GET | `/api/v1/reconciliation-cases` | `list_reconciliation_cases` | 管理员（含上游对账标识） |
| POST | `/api/v1/reconciliation-cases/{job_id}/refund` | `refund_reconciliation` | 管理员（幂等退款） |
| GET | `/api/v1/provider-failures` | `list_provider_failures` | 管理员（平台侧失败清单，含渠道原始码与原文） |
| GET | `/api/v1/provider-cost-gaps` | `list_provider_cost_gaps` | 管理员（**成本缺口清单**：执行发生了、成本本该有金额却拿不到的那些执行，带上游对账标识。这些 Job **不进对账态**——对客结算照常完成，缺口是平台侧的账务缺口） |
| GET | `/v1/models` | `list_models` | 任何人（公开目录，无需鉴权；只列当前可调的**网关模型**：`name` 是平台对客名、`vendor_id` 是厂商标识，另给合同修订 `revision` 与调用方合同 `contract`；厂商原生名不进对客面，`contract` 里的型号身份已换成对客名） |
| POST | `/v1/images/generations` | `generate_image` | 持 Key 的账户（JSON；`model` + 平铺的模型参数 + 参考图/遮罩；幂等键走 `Idempotency-Key` 头） |
| POST | `/v1/images/edits` | `edit_image` | 同上（`multipart/form-data`；`image`/`mask` 是文件部件，文本部件也认、值按 URL/data URL 读；与上一条**同一个能力**） |

对客的**生成面只有这两条路径**，都是**同步**：一个请求把图交回，没有 202 受理、没有 job_id 轮询。**分支由请求内容决定**（有没有参考图/遮罩），不按端点断言——带图的 generations、不带图的 edits 都合法。参考图与遮罩用**公网 URL 或 `data:image/…;base64,…`** 给出（`image` 与 `image_urls` 同义、二选一）；平台**不落盘**：不下载归档、不解码存储，渠道给 `url` 就给 `url`、给 `b64_json` 就给 `b64_json`，原样放进 `data[]`，由客户端判断。成功响应 `{created, data:[{url|b64_json}]}`；内部受理后等 Job 到终态（上限 `GENERATION_SYNC_WAIT_SECONDS`，默认 120s），等不到就按失败回超时错误。

**钱的两条线**（设计口径见 `docs/design/0007` §2–§8）：**对客只有 CNY 单币种**——售价是**按候选发布**的**对客四档 CNY 费率向量**（`consumer_rates_cny`，受理时随 Job 的 Price Snapshot 冻结，结算只读它），**预授权额**按**供给（vendor + offering）维度**的保底表（`floor_amounts`）查得——**像素型的 `size` 先归到档位**（优先用该供给发布的档位像素表、缺失时按最长边阈值兜底，`size = auto` 取默认档 2K），**不由售价派生**；受理闸门是**余额 ≥ 保底额**（不成立即 402 `insufficient_balance`），**结算按实际扣、不封顶在保底额**（实收超过保底额时余额被扣成负数——**透支发生在结算**，随后按当时余额判）。`GENERATION_MAX_COST_MICROUSD` 只作**连该供给的封顶保底值都查不到时**的兜底保底额，**不再是受理上限**。**成本平面按渠道声明的 `currency`** 记原币种原值，用受理时冻结的**折算率**（`pricing.fx_rates` 里"受理时刻生效的那一行"）折成 CNY 只用于毛利核算；`pricing.price_plans` 的费率收窄为**渠道成本费率**，不再是对客结算基数。

请求体上限 16MB（`DefaultBodyLimit`）。这里的 `job_id` 是**内部**执行/审计记录的标识，只在内部与管理员的运营接口出现。

`GET /api/v1/provider-failures` 的 `kind` 取值与落库值同名（`crates/adapter-sdk` 的 `ProviderFailureKind`）：它同时是 DB CHECK 的取值集合与查询参数取值，改枚举名即改接口。不传 `kind` 时只列**平台侧事件**；响应形如 `{failures, count, truncated}`——不翻页，所以必须让调用方看得出被截断。

## 4. 一次生成请求怎么走

```text
消费侧
  │ POST /v1/images/generations 或 /v1/images/edits   apps/api/src/main.rs  generate_image / edit_image
  ▼
GenerationService::create                              crates/application
  ├─ 校验幂等键 / 预算
  ├─ 判定图片分支（文生图 / 图生图 / 带遮罩）           crates/domain  CreateImageGeneration::branch
  ├─ 加速层开着时先做一次轻量读（生效修订标识 + 网关模型开关 + 数据库时钟），
  │    候选集从 route 缓存取；修订标识不一致、开关已关或值读不出来即回源数据库   crates/application  AccelerationService
  ├─ 取该型号的候选供给，按档位选第一个有合格候选的档，      crates/application  select_candidate
  │    再在该档的合格候选里按 weight 确定性分摊
  │    （落点 = hash(账户 ‖ 幂等键)，不引入随机数；不合格的候选不进分摊；
  │      发布期"限制只能收窄"的校验已在此前完成；调用方的图落到候选声明的参数名上）
  ├─ 受理时冻结定价：先把 size 归到档位、再查该供给    crates/application  freeze_pricing
  │    的保底表（像素型按档位像素表/最长边，auto 取 2K）；
  │    汇率按该候选的成本币种取"受理时刻生效的那一行"
  ├─ 缓存里的余额**新鲜**（写穿来源 + 落在新鲜窗口内）且低于保底额 ⇒
  │    提前 402 并写一条审计；不新鲜一律交给下面的数据库条件更新   crates/application  AccelerationService::precheck_balance
  ├─ 写 Job + 路由判定 + 预授权（同一事务）              crates/persistence  create_job
       （Job 是内部执行/审计记录，对客不可见；预授权额 = 保底额，
         闸门是余额 ≥ 保底额，不足即 402）
  └─ 提交后把**扣减后**的余额写进缓存（写穿）             crates/application  AccelerationService::write_balance
  ▼
Worker（独立进程，循环领活）                            apps/worker/src/main.rs
  └─ WorkerService::run_once                           crates/application
       ├─ claim_next_job（accepted → leased，带租约）    crates/persistence
       ├─ 按 adapter_key 造 Driver                       crates/application  AdapterRegistry
       ├─ ImageAdapter::execute ─────────────────────►  crates/adapter-aihubmix / adapter-apimart
       │     ② 的内部：data URL 就地解码、公网 URL 取用或透传、提交、轮询、抽计量证据、分类错误
       ├─ 成功：结果信封写回 Job → complete_job          crates/application  complete_success
       │     写 Metering Evidence、记渠道成本事实（含折算后 CNY）、按实际扣费并结清预授权  crates/persistence  complete_job
       │     （**不封顶在保底额**：差额由余额透支吸收）
       │     提交后把**实收之后**的余额写进缓存（失败收尾释放预授权时同样写）  crates/application  AccelerationService::write_balance
       └─ 失败：failure_from_adapter 决定处置            crates/application
             ├─ 对客码按责任方派生（渠道码与原文只留内部）  crates/application  public_error_code
             ├─ 可证明未受理 / 确定性拒绝 → failed + 释放预授权
             ├─ 结果交付失败 → reconciliation_required + 保留预授权（**成本事实一并落库**）
             └─ 不确定是否已受理 → reconciliation_required + 保留预授权（人工处置）
  │
  ▼ 同步门面等终态后回 { created, data:[{url|b64_json}] }
     GET /api/v1/reconciliation-cases                    对账清单（含上游对账标识，管理员面）
     POST /api/v1/reconciliation-cases/{job_id}/refund    幂等退款，只释放预授权（管理员面）
     GET /api/v1/provider-failures                       平台侧失败清单（欠费/凭证/平台 bug，管理员面）
     GET /api/v1/provider-cost-gaps                      成本缺口清单（成本未知的那些执行，管理员面）
     PUT /api/v1/fx-rates                                录入折算率（渠道币种 → CNY，管理员面）
     定时缓存对账（API 进程内的独立任务）                  以数据库为准覆盖余额与候选集，发现不一致写审计  crates/application  AccelerationService::run_reconciler
```

## 5. 谁拥有哪张表

| Schema / 表 | 是什么事实 | 写入方 |
| --- | --- | --- |
| `catalog.vendor_models` | ③ Profile（Capability Schema，随修订不可变） | `RuntimeService::publish` |
| `supply.channels` / `supply.offerings` | ④ 渠道与供给 | `RuntimeService::publish` |
| `pricing.price_plans` | ⑤ 渠道**成本费率**（四档 token 单价，币种按该渠道声明）：定价时的参考口径与毛利核算用，**不再是对客结算基数** | `RuntimeService::publish` |
| `pricing.fx_rates` | ⑤ **折算率**（按币种的"渠道币种 → CNY"定点比值 + 生效时间）：受理时取"受理时刻生效的那一行"并快照。**外部事实，由管理员录入**，不进不可变修订 | `PricingService::upsert_fx_rate`（`PUT /api/v1/fx-rates`） |
| `publication.runtime_revisions` / `runtime_entries` | 哪次发布生效、各型号的活动供给、优先级与**档内权重**（`routing_priority` 是档位，同档允许多条候选，档内按 `weight` 分摊）；修订上另记这次发布定义的是哪个**网关模型**（对客名）与它指向哪一行厂商模型合同，以及**定价**（修订级 `markup_bps` + **按候选键**的参考成本、成本币种、对客四档 CNY 费率向量、成本来源、档位价目表、保底表）；旧修订的定价列留 NULL ⇒ 受理与结算走旧口径 | `RuntimeService::publish` |
| `routing.route_policies` | **路由策略**：在已发布的合格候选里"挑哪一条"的运行期配置（全局一条 + 按网关模型覆盖）。**运营配置，不进不可变修订**；改它即刻影响之后的受理，已受理 Job 不受影响 | `RoutePolicyService::upsert`（`PUT /api/v1/route-policies`）、受理时取生效那条 |
| `publication.gateway_models` | 网关模型的**运维开关**：这个名字现在开着吗、谁在什么时候改的。**定义不在这里**（候选集、合同、定价只在不可变修订里） | `RuntimeService::publish`（首次发布落行）、`set_gateway_model_enabled` |
| `generation.jobs` | 受理时的请求事实、所选供给、**冻结的定价快照**（对客费率向量、保底额与来源、命中的候选、折算率）、结果信封（渠道给的 `url` 或 `b64_json`）、对客错误码与平台侧失败类别；**对客不可见** | `GenerationService::create`、`complete_job`、`fail_job`、`recover_expired_leases` |
| `generation.routing_decisions` | 受理时为什么选了它（候选、档位、**权重**、是否合格、本次**分流落点**） | 与 Job 同事务写入 |
| `generation.attempts` | 一次执行尝试：状态、**渠道原始错误码与原文**、对账标识、**计量证据**、**渠道成本事实**（来源 `computed`/`declared`/`unavailable` + 原币种金额 + 该渠道声明的币种 + **按冻结汇率折算后 CNY**）。成功与"结果交付失败进对账"两条路径都写成本事实 | `begin_attempt`、`complete_job`、`fail_job` |
| `ledger.accounts` / `ledger.holds` / `ledger.entries` | 余额、预授权、账目。**余额可为负**（透支发生在结算：实收超过保底额时差额把余额扣成负数），**保底额可为 0** | `create_job`（hold）、`complete_job`（capture）、`fail_job`（release） |
| `identity.api_keys` | API Key 摘要 | `IdentityService` |
| `operations.reconciliation_cases` / `audit_events` | 待人工处置的案例与审计（审计也承载"凭缓存提前拒绝"与缓存对账发现的覆盖） | `fail_job`、`ReconciliationService`、`AccelerationService`（`insert_audit_event`） |

`crates/persistence` 是这些表的唯一写入方；其他 crate 只能通过 `crates/application` 的 `HubRepository` 端口访问，不直接写 SQL。

## 6. 文件职责清单

| 文件 | 负责什么 | 明确不负责 |
| --- | --- | --- |
| `apps/api/src/main.rs` | HTTP 路由与 handler、鉴权中间件、请求/响应形状、启动时跑迁移、装配加速层并挂起缓存对账循环 | 业务规则、SQL、上游调用 |
| `apps/worker/src/main.rs` | 进程外壳：读环境变量、装配端口实现（含加速层）、循环 `run_once`、优雅退出 | 生成流程本身（在 `WorkerService`） |
| `apps/api/tests/http_contract.rs` | 端到端合同测试：真实空库 + 真实 API/Worker 进程 + **进程内假上游**与**进程内假 Redis**（零外部费用） | 单元测试（在各 crate 内） |
| `crates/domain/src/lib.rs` | `JobState` 状态机、`ImageBranch`、`OfferingCandidate`（档位 `routing_priority` 与**档内权重** `weight`）、`PriceSnapshot`（对客费率向量 / 成本费率 / 保底额 / 折算率 / 成本来源）、`resolve_size_tier` 与 `FloorTable`（像素型 `size` 归位 + 保底表查表与回落链）、`FxRate` 定点折算、`TokenUsage` / `MeteringEvidence` | IO、持久化 |
| `crates/domain/src/image_parameters.rs` | 图片参数的**唯一**一份规则：调用方契约字段（`image`/`image_urls`/`mask`）、候选声明参数名的判定（名字以 `image` 开头＝参考图、含 `mask`＝遮罩）、`null`/空串＝这一处没有图、把调用方的图落到候选声明的参数名上 | IO；也不认识任何**具体渠道**（参数名本身按 [`docs/adr/0015`](adr/0015-vendor-model-contract-and-offering-parameter-mapping.md) 应来自 Vendor Model Contract；当前实现里它是渠道原生名，属 [`#6`](https://github.com/dehuadong/seeaihub-server-next/issues/6) 差距 G1） |
| `crates/application/src/lib.rs` | 用例（`IdentityService` / `RuntimeService` / `GenerationService` / `WorkerService` / `ReconciliationService` / `PricingService` / `AccountsService`）、端口 trait（含 `CacheStore`）、发布期校验（含"限制只能收窄"与定价"全有或全无"）、候选选择（**先按档位取第一个有合格候选的档，再在档内按权重确定性分摊**）、**受理时冻结定价与保底额**、结算与成本折算、错误→处置映射与对客错误码派生、加速层语义（`AccelerationService`：键名与值形状、候选集的修订标识比对、写穿与来源标记、新鲜度判定与凭缓存拒绝的审计、缓存对账） | SQL、HTTP、上游协议、Redis 命令 |
| `crates/persistence/src/lib.rs` | `PgHubRepository`：SQL、事务边界、迁移、行↔领域类型映射；余额变更一律用 `RETURNING` 把**提交后**的余额带回给用例（供写穿缓存） | 业务判定（只执行用例给出的结论） |
| `crates/cache-redis/src/lib.rs` | 加速层的 Redis 实现：`GET` / `SET … PX` / `DEL` 三条命令、惰性连接与单次操作超时；连不上或命令报错一律返回错误，由用例层当"未命中"处理。`REDIS_URL` 为空时不构造（`from_env` 返回 `None`） | 键名、值形状、新鲜度与拒绝判定（都在 `crates/application`） |
| `crates/adapter-sdk/src/lib.rs` | ② 的接口与共享类型：`ImageAdapter`、`AdapterDescriptor`、`PreparedImageRequest`、结果信封（`url` 或 `b64_json` 恰好其一）、data URL 解码、`ProviderSuccess`、**成本事实报告**（`ProviderCost` 三态 `declared` / `computed` / `unavailable`，成员名与领域取值逐字同名，转换只此一处）、`ProviderCallError`、`RetrySafety` 三态、`ProviderFailureKind` 平台侧失败类别 | 任何具体渠道的协议细节 |
| `crates/adapter-aihubmix/src/lib.rs` | AIHubMix 一族：端点分流（`/v1/images/generations` 与 `/v1/images/edits`）、multipart 封装（参考图需字节：data URL 就地解码，公网 URL 由它自己取）、结果原样交回、响应头 `x-request-id`（有则采集为对账标识）、错误分类、**成本报告"这条渠道不给金额字段"**（成本由平台按实际用量自算） | 平台侧的生命周期与计费规则 |
| `crates/adapter-apimart/src/lib.rs` | APIMart 一族：任务式（提交 → 轮询，**只在 Adapter 内部**）、公网参考图逐字透传、data URL 就地解码后上传换 URL 再回填、四分项计量证据的读取、错误分类与 `SafeBeforeAcceptance`；终态里的 `cost` **采纳为成本事实**（精确换成微单位、币种用渠道声明；缺字段 / 负数 / 解析失败一律按"拿不到"报告，不猜）。`credits_cost` 仍不采纳 | 同上；金额只进成本口径，不替代计量事实、不参与对客金额 |
| `migrations/0001_initial.sql`…`0010_routing_weight_and_decisions.sql` | 表结构与约束（含"每型号每个网关模型下同一条供给只允许一条活动条目"、路由判定表、对客错误码白名单与失败类别取值），以及增量迁移：撤销资产表与列（`0005`）、合同与承载面拆分（`0006`）、网关模型命名两列与开关表（`0007`）、执行尝试上的成本四列与其同形约束（`0008`）、**汇率表 + 修订上的定价七列 + 放宽三处余额/预授权约束**（`0009`：余额可为负、保底额与预授权额可为 0）、**候选上的档内权重 + 唯一索引换成 `(gateway_model, offering_id) WHERE active`**（`0010`：同档允许多条候选）、**路由策略表 `routing.route_policies`**（`0011`：作用域唯一，策略类型只放本层已实现的取值） | 运行时的业务规则 |
| `config/bootstrap/*.json` | 可直接发布的运行时素材（Profile + Offering + Price 三合一） | 不是运行时数据源：必须经发布接口写入 |
| `scripts/decisions/*.mjs` | Agent Notes 的目录、元数据与文件格式检查（不生成索引），自述与本地修补见该目录 `README.md` | 不影响服务运行 |
| `docs/AGENTS.md`、`docs/agents/git.md` | 正文与代码注释的写作规则与 slop 清单；提交、推送与历史改写约定 | 工件位置与归属归 `docs/agents/artifacts.md`；Agent Note 的文件骨架归 `.agents/notes/README.md` |
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
