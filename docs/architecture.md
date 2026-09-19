# 代码结构图

> **本文的定位**：一张"哪个 crate、哪个文件、哪张表负责什么"的落点图，供人和 Agent 快速定位改动位置。
>
> **它不是合同**：分层的**职责与规则**由 [`docs/design/0004-layered-architecture.md`](design/0004-layered-architecture.md) 拥有（五层、四条规则 R1–R4、新 Provider 接入清单与合法例外 E1–E3）；持久决定由 [`docs/adr/`](adr/) 拥有。本文只做索引，**不新增决策**——要改职责边界，改 0004 或 ADR，再回来同步这里。
>
> **维护要求**：新增或移动文件、增删路由、增删表时，在同一个变更里更新本文。

## 1. 分层与依赖方向

```text
 进程         apps/api        控制面：HTTP 接口（受理生成、发布配置、资产、对账）
             apps/worker     执行面：领取 Job、调上游、归档结果、结算
                  │ 依赖
                  ▼
 应用层       crates/application
             用例（Service）+ 端口（trait：HubRepository / AssetStore /
             AdapterFactory / CredentialProvider）；发布校验与归一；错误→处置映射
                  │ 依赖
                  ▼
 领域层       crates/domain
             类型与规则：Job 状态机、图片分支、资产绑定路径、计价公式、计量证据
             （不依赖数据库 / HTTP / Provider）
                  ▲ 实现端口（反向依赖：基础设施依赖应用层，应用层不认识它们）
                  │
 基础设施     crates/persistence      PostgreSQL（SQL、事务、迁移）
             crates/object-storage   本地文件 / S3 兼容对象存储
             crates/adapter-sdk      ② 的接口与共享类型
             crates/adapter-aihubmix ② AIHubMix 一族
             crates/adapter-apimart  ② APIMart 一族
```

依赖方向是单向的：`apps/* → application → domain`，基础设施**反向实现**应用层的端口。因此换数据库、换对象存储、加渠道都不牵动领域层。

## 2. 五层落在哪

| 层 | 负责什么 | 在本仓库的落点 |
| --- | --- | --- |
| **① Model Protocol** | 对外路由、请求/响应外壳、对外生命周期 | `apps/api/src/main.rs`（路由与 handler）、`crates/application` 的 `CreateImageGeneration` / `JobView`、`crates/domain` 的 `JobState` |
| **② Adapter Driver** | 上游路径、封装格式、响应解析、证据提取、错误分类、轮询与取图（**渠道差异只此一处**） | `crates/adapter-sdk`（接口）、`crates/adapter-aihubmix`、`crates/adapter-apimart` |
| **③ Model Profile** | 某渠道供某型号的合同：支持参数、值域、默认值、组合规则 | 运行时发布物 `catalog.vendor_models.capability_schema`；素材在 `config/bootstrap/*.json` |
| **④ Offering** | 渠道、上游模型名、用哪个 Driver、渠道限制、路由优先级 | `supply.offerings` + `publication.runtime_entries`；发布逻辑在 `crates/application` 的 `RuntimeService` |
| **⑤ Price** | 计价形态、费率、币种 | `pricing.price_plans`；公式在 `crates/domain` 的 `PriceSnapshot::charge_microusd` |

## 3. 对外接口

全部在 `apps/api/src/main.rs`。

| 方法 | 路径 | handler | 谁可以调 |
| --- | --- | --- | --- |
| GET | `/health` | `health` | 任何人 |
| POST | `/admin/accounts` | `create_account` | 管理员（`ADMIN_TOKEN`） |
| POST | `/admin/accounts/{account_id}/credits` | `credit_account` | 管理员 |
| POST | `/admin/accounts/{account_id}/api-keys` | `issue_api_key` | 管理员 |
| POST | `/admin/runtime-revisions` | `publish_runtime` | 管理员（发布 Profile + Offering + Price） |
| GET | `/admin/reconciliation-cases` | `list_reconciliation_cases` | 管理员（含上游对账标识） |
| POST | `/admin/reconciliation-cases/{job_id}/refund` | `refund_reconciliation` | 管理员（幂等退款） |
| POST | `/v1/assets` | `upload_asset` | 持 Key 的账户（`Content-Type` + `x-asset-role`） |
| GET | `/v1/assets/{asset_id}` | `download_asset` | 同上，限本账户 |
| POST | `/v1/image-generations` | `create_generation` | 同上（需 `idempotency_key` 与 `max_cost_microusd`） |
| GET | `/v1/image-generations/{job_id}` | `get_generation` | 同上，限本账户 |

请求体上限 16MB（`DefaultBodyLimit`）；资产字节走对象存储，不经过数据库。

## 4. 一次生成请求怎么走

```text
消费侧
  │ ① POST /v1/image-generations                      apps/api/src/main.rs  create_generation
  ▼
GenerationService::create                              crates/application
  ├─ 校验幂等键 / 预算 / 资产角色与尺寸
  ├─ 判定图片分支（文生图 / 图生图 / 带遮罩）           crates/domain  CreateImageGeneration::branch
  ├─ 取该型号的候选供给，按优先级选第一个合格者          crates/application  select_candidate
  │    （发布期"限制只能收窄"的校验已在此前完成）
  └─ 写 Job + 路由判定 + 预授权（同一事务）              crates/persistence  create_job
  │
  ▼ 202 { job_id }
Worker（独立进程，循环领活）                            apps/worker/src/main.rs
  └─ WorkerService::run_once                           crates/application
       ├─ claim_next_job（accepted → leased，带租约）    crates/persistence
       ├─ prepare：读输入资产并核对 sha256               crates/application + object-storage
       ├─ 按 adapter_key 造 Driver                       crates/application  AdapterRegistry
       ├─ ImageAdapter::execute ─────────────────────►  crates/adapter-aihubmix / adapter-apimart
       │     ② 的内部：上传换 URL、提交、轮询、取图、抽计量证据、分类错误
       ├─ 成功：结果写对象存储 → complete_job            crates/application  complete_success
       │     归档结果、写 Metering Evidence、捕获预授权   crates/persistence  complete_job
       └─ 失败：failure_from_adapter 决定处置            crates/application
             ├─ 可证明未受理 / 确定性拒绝 → failed + 释放预授权
             └─ 不确定是否已受理 → reconciliation_required + 保留预授权（人工处置）
  │
  ▼ ② GET /v1/image-generations/{job_id}                查询 Job 与结果资产
     GET /admin/reconciliation-cases                    对账清单（含上游对账标识）
     POST /admin/reconciliation-cases/{job_id}/refund    幂等退款，只释放预授权
```

## 5. 谁拥有哪张表

| Schema / 表 | 是什么事实 | 写入方 |
| --- | --- | --- |
| `catalog.vendor_models` | ③ Profile（Capability Schema，随修订不可变） | `RuntimeService::publish` |
| `supply.channels` / `supply.offerings` | ④ 渠道与供给 | `RuntimeService::publish` |
| `pricing.price_plans` | ⑤ 计价配置 | `RuntimeService::publish` |
| `publication.runtime_revisions` / `runtime_entries` | 哪次发布生效、各型号的活动供给与优先级 | `RuntimeService::publish` |
| `generation.jobs` | 受理时的请求事实、所选供给、价格快照、结果资产 | `GenerationService::create`、`complete_job`、`fail_job` |
| `generation.routing_decisions` | 受理时为什么选了它（候选、优先级、是否合格） | 与 Job 同事务写入 |
| `generation.attempts` | 一次执行尝试：状态、上游错误、对账标识、**计量证据** | `begin_attempt`、`complete_job`、`fail_job` |
| `generation.assets` | 输入/输出媒体引用（字节在对象存储） | `AssetService::upload`、`complete_success` |
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
| `crates/domain/src/lib.rs` | `JobState` 状态机、`ImageBranch`、`AssetBinding`（渠道原生参数路径）、`PriceSnapshot` 计价公式、`TokenUsage` / `MeteringEvidence` | IO、持久化；也不认识任何**具体渠道**——它只知道"绑定路径的第一段是渠道自己的参数名"这一条通用约定 |
| `crates/application/src/lib.rs` | 用例（`IdentityService` / `RuntimeService` / `AssetService` / `GenerationService` / `WorkerService` / `ReconciliationService`）、端口 trait、发布期校验（含"限制只能收窄"）、候选选择、错误→处置映射、结果归档 | SQL、HTTP、上游协议 |
| `crates/persistence/src/lib.rs` | `PgHubRepository`：SQL、事务边界、迁移、行↔领域类型映射 | 业务判定（只执行用例给出的结论） |
| `crates/object-storage/src/lib.rs` | `AssetStore` 的本地与 S3 实现 | 资产归属与授权（在用例层） |
| `crates/adapter-sdk/src/lib.rs` | ② 的接口与共享类型：`ImageAdapter`、`AdapterDescriptor`、`PreparedImageRequest`、`ProviderSuccess`、`ProviderCallError`、`RetrySafety` 三态 | 任何具体渠道的协议细节 |
| `crates/adapter-aihubmix/src/lib.rs` | AIHubMix 一族：端点分流（`/v1/images/generations` 与 `/v1/images/edits`）、multipart 封装、`b64_json` 解码、`x-request-id` 对账标识、错误分类 | 平台侧的生命周期与计费规则 |
| `crates/adapter-apimart/src/lib.rs` | APIMart 一族：任务式（提交 → 轮询 → 取图）、提交前上传换 URL、渠道原生参数名回填、四分项计量证据的读取、错误分类与 `SafeBeforeAcceptance` | 同上；上游声明的 `cost`/`credits_cost` **不进平台证据**（计费事实由分项 token 推出，金额只在渠道事实台账里作为成本口径记录） |
| `migrations/0001_initial.sql`、`0002_multiple_active_offerings.sql` | 表结构与约束（含"每型号每个优先级一个活动条目"、路由判定表） | 运行时的业务规则 |
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
- **换对象存储**：`ASSET_STORE=local|s3`，实现仍在 `crates/object-storage`。
- **面向消费侧的统一参数转换**：**不在本仓库内部**，属后期对外消费侧能力（见 [`docs/adr/0002`](adr/0002-native-capability-schema-not-canonical.md) 的补充决定）。
