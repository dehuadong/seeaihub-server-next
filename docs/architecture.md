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
             AdapterFactory / CredentialProvider）；发布校验与归一；错误→处置映射
                  │ 依赖
                  ▼
 领域层       crates/domain
             类型与规则：Job 状态机、图片分支、调用方图片落到候选参数名的映射、
             计价公式、计量证据（不依赖数据库 / HTTP / Provider）
                  ▲ 实现端口（反向依赖：基础设施依赖应用层，应用层不认识它们）
                  │
 基础设施     crates/persistence      PostgreSQL（SQL、事务、迁移）
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
| **④ Offering** | 渠道、上游模型名、用哪个 Driver、渠道限制、路由优先级 | `supply.offerings` + `publication.runtime_entries`；发布逻辑在 `crates/application` 的 `RuntimeService` |
| **⑤ Price** | 计价形态、费率、币种 | `pricing.price_plans`；公式在 `crates/domain` 的 `PriceSnapshot::charge_microusd` |

## 3. 对外接口

全部在 `apps/api/src/main.rs`。

| 方法 | 路径 | handler | 谁可以调 |
| --- | --- | --- | --- |
| GET | `/health` | `health` | 任何人 |
| POST | `/api/v1/accounts` | `create_account` | 管理员（`ADMIN_TOKEN`） |
| POST | `/api/v1/accounts/{account_id}/credits` | `credit_account` | 管理员 |
| POST | `/api/v1/accounts/{account_id}/api-keys` | `issue_api_key` | 管理员 |
| POST | `/api/v1/runtime-revisions` | `publish_runtime` | 管理员（发布 Profile + Offering + Price） |
| GET | `/api/v1/reconciliation-cases` | `list_reconciliation_cases` | 管理员（含上游对账标识） |
| POST | `/api/v1/reconciliation-cases/{job_id}/refund` | `refund_reconciliation` | 管理员（幂等退款） |
| GET | `/api/v1/provider-failures` | `list_provider_failures` | 管理员（平台侧失败清单，含渠道原始码与原文） |
| GET | `/v1/models` | `list_models` | 任何人（公开目录，无需鉴权；只列当前可调的型号与各自的调用方合同） |
| POST | `/v1/images/generations` | `generate_image` | 持 Key 的账户（JSON；`model` + 平铺的模型参数 + 参考图/遮罩；幂等键走 `Idempotency-Key` 头） |
| POST | `/v1/images/edits` | `edit_image` | 同上（`multipart/form-data`；`image`/`mask` 是文件部件，文本部件也认、值按 URL/data URL 读；与上一条**同一个能力**） |

对客的**生成面只有这两条路径**，都是**同步**：一个请求把图交回，没有 202 受理、没有 job_id 轮询。**分支由请求内容决定**（有没有参考图/遮罩），不按端点断言——带图的 generations、不带图的 edits 都合法。参考图与遮罩用**公网 URL 或 `data:image/…;base64,…`** 给出（`image` 与 `image_urls` 同义、二选一）；平台**不落盘**：不下载归档、不解码存储，渠道给 `url` 就给 `url`、给 `b64_json` 就给 `b64_json`，原样放进 `data[]`，由客户端判断。成功响应 `{created, data:[{url|b64_json}]}`；内部受理后等 Job 到终态（上限 `GENERATION_SYNC_WAIT_SECONDS`，默认 120s），等不到就按失败回超时错误。预授权额**按供给（vendor + offering）维度的保底表**（`floor_amounts`，随修订发布、随 Job 快照冻结；设计口径见 `docs/design/0007` §6）查得，**不由调用方自报、也不由候选价格反算**；受理闸门是**余额 ≥ 保底额**（不成立即 402 `insufficient_balance`）。`GENERATION_MAX_COST_MICROUSD` 只作**连该供给的封顶保底值都查不到时**的兜底保底额，**不再是受理上限**。

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
  ├─ 取该型号的候选供给，按优先级选第一个合格者          crates/application  select_candidate
  │    （发布期"限制只能收窄"的校验已在此前完成；调用方的图落到候选声明的参数名上）
  └─ 写 Job + 路由判定 + 预授权（同一事务）              crates/persistence  create_job
       （Job 是内部执行/审计记录，对客不可见）
  ▼
Worker（独立进程，循环领活）                            apps/worker/src/main.rs
  └─ WorkerService::run_once                           crates/application
       ├─ claim_next_job（accepted → leased，带租约）    crates/persistence
       ├─ 按 adapter_key 造 Driver                       crates/application  AdapterRegistry
       ├─ ImageAdapter::execute ─────────────────────►  crates/adapter-aihubmix / adapter-apimart
       │     ② 的内部：data URL 就地解码、公网 URL 取用或透传、提交、轮询、抽计量证据、分类错误
       ├─ 成功：结果信封写回 Job → complete_job          crates/application  complete_success
       │     写 Metering Evidence、捕获预授权             crates/persistence  complete_job
       └─ 失败：failure_from_adapter 决定处置            crates/application
             ├─ 对客码按责任方派生（渠道码与原文只留内部）  crates/application  public_error_code
             ├─ 可证明未受理 / 确定性拒绝 → failed + 释放预授权
             └─ 不确定是否已受理 → reconciliation_required + 保留预授权（人工处置）
  │
  ▼ 同步门面等终态后回 { created, data:[{url|b64_json}] }
     GET /api/v1/reconciliation-cases                    对账清单（含上游对账标识，管理员面）
     POST /api/v1/reconciliation-cases/{job_id}/refund    幂等退款，只释放预授权（管理员面）
     GET /api/v1/provider-failures                       平台侧失败清单（欠费/凭证/平台 bug，管理员面）
```

## 5. 谁拥有哪张表

| Schema / 表 | 是什么事实 | 写入方 |
| --- | --- | --- |
| `catalog.vendor_models` | ③ Profile（Capability Schema，随修订不可变） | `RuntimeService::publish` |
| `supply.channels` / `supply.offerings` | ④ 渠道与供给 | `RuntimeService::publish` |
| `pricing.price_plans` | ⑤ 计价配置 | `RuntimeService::publish` |
| `publication.runtime_revisions` / `runtime_entries` | 哪次发布生效、各型号的活动供给与优先级 | `RuntimeService::publish` |
| `generation.jobs` | 受理时的请求事实、所选供给、价格快照、结果信封（渠道给的 `url` 或 `b64_json`）、对客错误码与平台侧失败类别；**对客不可见** | `GenerationService::create`、`complete_job`、`fail_job`、`recover_expired_leases` |
| `generation.routing_decisions` | 受理时为什么选了它（候选、优先级、是否合格） | 与 Job 同事务写入 |
| `generation.attempts` | 一次执行尝试：状态、**渠道原始错误码与原文**、对账标识、**计量证据** | `begin_attempt`、`complete_job`、`fail_job` |
| `ledger.accounts` / `ledger.holds` / `ledger.entries` | 余额、预授权、账目 | `create_job`（hold）、`complete_job`（capture）、`fail_job`（release） |
| `identity.api_keys` | API Key 摘要 | `IdentityService` |
| `operations.reconciliation_cases` / `audit_events` | 待人工处置的案例与审计 | `fail_job`、`ReconciliationService` |

`crates/persistence` 是这些表的唯一写入方；其他 crate 只能通过 `crates/application` 的 `HubRepository` 端口访问，不直接写 SQL。

## 6. 文件职责清单

| 文件 | 负责什么 | 明确不负责 |
| --- | --- | --- |
| `apps/api/src/main.rs` | HTTP 路由与 handler、鉴权中间件、请求/响应形状、启动时跑迁移 | 业务规则、SQL、上游调用 |
| `apps/worker/src/main.rs` | 进程外壳：读环境变量、装配端口实现、循环 `run_once`、优雅退出 | 生成流程本身（在 `WorkerService`） |
| `apps/api/tests/http_contract.rs` | 端到端合同测试：真实空库 + 真实 API/Worker 进程 + **进程内假上游**（零外部费用） | 单元测试（在各 crate 内） |
| `crates/domain/src/lib.rs` | `JobState` 状态机、`ImageBranch`、`PriceSnapshot` 计价公式、`TokenUsage` / `MeteringEvidence` | IO、持久化 |
| `crates/domain/src/image_parameters.rs` | 图片参数的**唯一**一份规则：调用方契约字段（`image`/`image_urls`/`mask`）、候选声明参数名的判定（名字以 `image` 开头＝参考图、含 `mask`＝遮罩）、`null`/空串＝这一处没有图、把调用方的图落到候选声明的参数名上 | IO；也不认识任何**具体渠道**（参数名本身按 [`docs/adr/0015`](adr/0015-vendor-model-contract-and-offering-parameter-mapping.md) 应来自 Vendor Model Contract；当前实现里它是渠道原生名，属 [`#6`](https://github.com/dehuadong/seeaihub-server-next/issues/6) 差距 G1） |
| `crates/application/src/lib.rs` | 用例（`IdentityService` / `RuntimeService` / `GenerationService` / `WorkerService` / `ReconciliationService`）、端口 trait、发布期校验（含"限制只能收窄"）、候选选择、错误→处置映射与对客错误码派生 | SQL、HTTP、上游协议 |
| `crates/persistence/src/lib.rs` | `PgHubRepository`：SQL、事务边界、迁移、行↔领域类型映射 | 业务判定（只执行用例给出的结论） |
| `crates/adapter-sdk/src/lib.rs` | ② 的接口与共享类型：`ImageAdapter`、`AdapterDescriptor`、`PreparedImageRequest`、结果信封（`url` 或 `b64_json` 恰好其一）、data URL 解码、`ProviderSuccess`、`ProviderCallError`、`RetrySafety` 三态、`ProviderFailureKind` 平台侧失败类别 | 任何具体渠道的协议细节 |
| `crates/adapter-aihubmix/src/lib.rs` | AIHubMix 一族：端点分流（`/v1/images/generations` 与 `/v1/images/edits`）、multipart 封装（参考图需字节：data URL 就地解码，公网 URL 由它自己取）、结果原样交回、响应头 `x-request-id`（有则采集为对账标识）、错误分类 | 平台侧的生命周期与计费规则 |
| `crates/adapter-apimart/src/lib.rs` | APIMart 一族：任务式（提交 → 轮询，**只在 Adapter 内部**）、公网参考图逐字透传、data URL 就地解码后上传换 URL 再回填、四分项计量证据的读取、错误分类与 `SafeBeforeAcceptance` | 同上；上游声明的 `cost`/`credits_cost` **不进平台证据**（计费事实由分项 token 推出，金额只在渠道事实台账里作为成本口径记录） |
| `migrations/0001_initial.sql`…`0005_images_pass_through.sql` | 表结构与约束（含"每型号每个优先级一个活动条目"、路由判定表、对客错误码白名单与失败类别取值），以及本次撤销资产表与列的增量迁移 | 运行时的业务规则 |
| `config/bootstrap/*.json` | 可直接发布的运行时素材（Profile + Offering + Price 三合一） | 不是运行时数据源：必须经发布接口写入 |
| `scripts/decisions/*.mjs` | Agent Notes 的索引生成与一致性检查 | 不影响服务运行 |
| `docs/design/`、`docs/adr/` | 设计与决策的权威位置 | — |
| `docs/facts/channel-facts.md` | 各渠道的**事实台账**（端点、参数、计量与成本口径、实测记录） | 不是接口合同，服务不读取 |
| `docs/verification/` | 受控验证清单（步骤、停止条件、留档要求） | — |
| `out-reference/` | 上游原始材料（文档、Schema 快照、实测响应） | 不属于平台合同，服务不读取 |
| `.agents/notes/` | 工程变更与交付记录 | — |

## 7. 扩展点

- **新增一个渠道族**：在 `crates/adapter-*` 加 Driver（②），发布新的 Profile/Offering（③④），不动 ① 与领域层——判据与合法例外见 0004 §4。
- **新增一个型号或调整参数面**：只发布新的运行时素材（`config/bootstrap/*.json` 的形状），不重新编译。
- **参考图的形态差异**（公网 URL / data URL、上游要 URL 还是要字节）：只在 ② Adapter 内部吸收，平台不搬运、不托管；判据见 [`docs/adr/0019`](adr/0019-images-pass-through-without-asset-storage.md)。
- **参数合同的归属**：调用方按 **Vendor Model Contract** 提交参数；同一 Vendor Model 在不同 Provider 的字段/位置/枚举差异由 **Offering Parameter Mapping** 在平台内部吸收。决策见 [`docs/adr/0015`](adr/0015-vendor-model-contract-and-offering-parameter-mapping.md)。
- **面向消费侧的跨厂商统一简化接口**：**不在本仓库内部**，属后期独立规划（见 [`docs/adr/0015`](adr/0015-vendor-model-contract-and-offering-parameter-mapping.md)）。**注意：这与上面那条不是同一件事**——"按各模型自己的合同提交"不等于"所有厂商共用一套字段"。
