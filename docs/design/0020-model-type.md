主题: 模型类型：素材声明、发布落库与对客暴露
当前修订: v1
生效修订: v1
状态: 已接受
承接: [模型类型与用量记录 Spec v1](../specs/0006-model-type-and-usage-records.md) §1–§5
依赖: [Vendor Model 合同与承载面](0005-vendor-model-contract-and-offering-mapping.md) §8.1；[网关模型与对客面](0006-gateway-models-and-consumer-surface.md) §2.1；[平台模型发布](0012-platform-model-publishing.md) §3；[账户资金](0013-account-funds-and-reservations.md) §5；[客户控制台导航与历史](0014-customer-console-navigation-and-history.md) §2；[身份与控制台](0010-identity-and-consoles.md) §4.2

# 模型类型：素材声明、发布落库与对客暴露

本 RFC 拥有模型类型的事实落点、写入与校验、对客目录暴露、用量记录与账单汇总的取数，以及界面的类型映射。发布与定价的既有规则由[平台模型发布设计](0012-platform-model-publishing.md)与[定价与结算设计](0007-pricing-floor-and-settlement.md)拥有。

## 1. 类型的归属与落点

类型是 Vendor Model 的事实，与 `capability_schema` 同层同生命周期：落在 `catalog.vendor_models.model_type`，取值 `image` / `video` / `chat`，按 `(vendor_id, native_model_id, native_revision)` 不可变。一个网关模型由一次发布引用一个 Vendor Model，它的类型因此由被引用的那一行决定。

类型只是标签：它不参与受理、路由，也不进目录"能不能调"的判据——那条判据仍按生效发布条目、启用的供给与渠道判（[网关模型与对客面](0006-gateway-models-and-consumer-surface.md) §2.1）。类型取值不新增发布闸门，一个供给齐备的模型不会因为声明成哪种类型而变得不可调。

用量记录的类型不给 `generation.jobs` 加列、也不回填：Job 已经冻结 `vendor_model_id`（受理时绑定供给的 Vendor Model，此后不再改写），读取时经它直查 `catalog.vendor_models`，拿到的是受理当时那一行。这条读法与幂等重算重读合同（[persistence](../../crates/persistence/src/lib.rs) 的 `lookup_execution`）相同，网关模型改绑到别的 Vendor Model 不会改变已受理记录的类型。

## 2. 素材、导入与发布

素材 `config/bootstrap/*.json` 顶层新增 `type`，两份现有素材在同一变更里补 `"type": "image"`：服务启动在迁移之后导入这个目录，导入失败会让进程起不来。

素材的 `type` 先按原始值收下，解析成功后在写出前校验取值域，报错串带素材文件与 `native_model_id`——serde 的缺字段与未知取值发生在反序列化时，那时还不知道型号；靠列 CHECK 只会落成约束名。[素材导入](../../crates/persistence/src/material_import.rs)的 `Material` 与 `upsert_vendor_model` 在同一变更里处理这个字段：插入 `catalog.vendor_models` 时写入，命中既有身份键时与合同一起比对，不同则拒并点名，不改写既有行。

发布链上的每一处都在同一变更里加类型：`PublishRuntimeCommand`、`PublishRuntimeRequest`、`into_request`、`resolve_referenced_offerings`（与 `capability_schema` 一起从被引行回写）、被引供给的读取（[persistence](../../crates/persistence/src/lib.rs) 的 `offerings_by_id`）增选 `vm.model_type`、以及 `publish_runtime` 写 `catalog.vendor_models` 那一处的 INSERT 与比对。少改任何一处，管理端在用的引用式发布都会因为请求里没有类型而被拒。

内联路径（`offerings`）要求顶层给 `type`，发布期在归一阶段校验取值域，拒绝时点名 `native_model_id` 与修订。这一条与 `capability_schema` 的规则不同：顶层合同可以省略、回退到逐候选的旧字段，类型没有逐候选回退，所以既有的内联发布调用方（含接口用例夹具）都要在请求里加 `type`——`/api/v1/runtime-revisions` 的内联形状有破坏性变化，引用式形状不受影响。

## 3. 对客目录

[目录读](../../crates/persistence/src/lib.rs)的 `published_models` SQL 增加 `vm.model_type`，[`PublishedModel`](../../crates/domain/src/lib.rs) 增加字段，[`ModelCatalogEntry`](../../apps/api/src/main.rs) 增加 `type`。`GET /v1/models` 既有字段不变，只增一个字段；这次新增是[网关模型与对客面](0006-gateway-models-and-consumer-surface.md) §2.1 对 [`0005` §8.1](0005-vendor-model-contract-and-offering-mapping.md) 已定形状的修订之一，目录用例对既有字段的断言继续成立。

## 4. 用量记录与用量装配

[用量记录读](../../crates/persistence/src/lib.rs)的 `customer_usage` 以 `j.vendor_model_id` 连 `catalog.vendor_models` 取 `model_type`，在这里一次性装配成 [`CustomerUsageView`](../../crates/application/src/lib.rs)：视图带 `model_type`，`image_count: u32` 换成用量值类型 `UsageAmounts`：四个字段各自可有值——`images`、`seconds`、`input_tokens`、`output_tokens`，都用 `i64`，逐条记录与区间合计共用同一个类型。序列化时只出现有值的键，对象可以为空。API 行 DTO 把 `model_type` 映射成对客字段 `type`、把用量值映射成 `usage`。

装配按类型取值：图片模型的记录给 `images`，`jobs.image_count` 缺失按 `Some(0)`（沿用现有口径，处理中与未产出也如此）；视频给 `seconds`、对话给 `input_tokens` 与 `output_tokens`。本次不为视频与对话新增量落点，这两类量随各自执行路径落地时写入；在那之前视图里它们是 `None`，读取端按缺失处理，不用 0 或别的量顶替。

两条读共用这一份视图里的用量值，但**各自保留自己的行投影**：客户侧的 `read_customer_usage` 不回 Job 标识、响应带翻页游标，管理端的 `list_account_usage` 回 `job_id`、响应不带游标。只有用量的装配下沉到视图，两个行 DTO 与两个外层响应各自保留，API 层不再各装配一份用量。

这两个端点只服务本仓库控制台（对客那条认客户会话，管理端那条认管理员），`image_count` 换成 `usage` 是它们自己的字段替换，没有外部调用方；对外公开的 `/v1/models` 只增字段。这次替换是[模型类型 Spec](../../docs/specs/0006-model-type-and-usage-records.md) 的合同决定，同一变更里同步描述该字段的属主：[客户控制台导航与历史](0014-customer-console-navigation-and-history.md) §2、[身份与控制台](0010-identity-and-consoles.md) §4.2 的端点表、`apps/api` 的路由定义、[管理后台信息架构](0011-console-information-architecture.md) 的扣费记录说明、[账户资金设计](0013-account-funds-and-reservations.md) §5 的汇总口径与[人工验收清单](../verification/consoles-manual-acceptance.md)。

## 5. 账单汇总

[账单汇总读](../../crates/persistence/src/lib.rs)的 `customer_billing` 按 `model_type` 分组，把同一区间内已完成请求的量按类型分别求和，响应里的顶层 `images` 换成 `usage` 对象，键与用量记录相同（`images` / `seconds` / `input_tokens` / `output_tokens`）：图片是产出张数合计，视频是秒数合计，对话是输入与输出 token 合计。请求数、扣费净额与区间谓词不变，不同类型的量不相加；某类型的量没有值时（区间内没有该类型的请求，或该类型的量落点还没有值），对应的键缺省，界面显示占位「—」。

## 6. 界面

类型到量键与单位的映射写成一个共享模块（放在 `apps/web/src/shared` 下，与既有格式化辅助同处），[用量记录页](../../apps/web/src/portal/pages/Usage.tsx)的两个表、[账单页](../../apps/web/src/portal/pages/Billing.tsx)与管理端[账户调用明细](../../apps/web/src/console/pages/Accounts.tsx)都引用它：图片显示「N 张」，视频显示「N 秒」，对话显示「输入 N / 输出 M tokens」；对应量缺失显示占位「—」，不显示 0。账单页按类型映射列出各项，不从 `usage` 的键反推该显示哪些类型。图片类型处理中与未产出仍按现有口径显示 0。

用量记录与账单的共享行类型只保留一处声明；[`PublicModel`](../../apps/web/src/shared/types.ts) 是 `/v1/models` 的镜像，随 `type` 字段一起更新，避免与响应漂移。

## 7. 迁移与兼容

`catalog.vendor_models` 增列 `model_type text NOT NULL`，约束具名 `vendor_models_model_type_known`，取值 `image` / `video` / `chat`，并加 `COMMENT ON COLUMN`。迁移用下一个编号 `0041`（当前最高 `0040`），先把列加成可空、把存量行回填 `image`、再置 `NOT NULL`：到本次改动为止，仓库的驱动、承载面与计价形态只覆盖图片模型，回填是事实而不是默认值；列不留默认值，之后的插入必须显式声明。迁移用例的列探测补一条，并按既有先例同时断言老库升级后该列的取值是 `image`。

库夹具里凡直接向 `catalog.vendor_models` 插行的，都在同一变更加上该列——包括迁移用例里升级后直插的每一处；表示升级前老库的夹具保持原样，由回填覆盖。

## 8. 验证承接

| Spec 验收 | 验证切入点 |
| --- | --- |
| A1、A3、A4 | 接口用例：素材导入的缺失、非法与改值；目录响应含 `type` 且既有字段不变。 |
| A2、A7 | 接口用例与浏览器用例：客户与管理端用量记录的类型与用量；图片未产出仍显示 0。 |
| A5 | 接口用例：受理记录在网关模型改绑后仍返回受理时的类型。 |
| A6 | 接口用例按内联发布声明 `type: "video"` 发布（受理不看类型），走一次同步调用拿到一条 Job，断言该记录 `type` 为 `video`、`usage` 为空。 |
| A8 | 接口用例：同一区间里图片与视频记录并存时，汇总的 `usage.images` 只等于 image 类型记录的张数（video 记录经图片路径产出的图不计入）、不含视频 `seconds`（落点未落地）、两者不相加，请求数与扣费净额不变。 |

A6、A7、A8 的界面半边由浏览器用例用 `page.route` 拦截读数（客户侧 `/v1/customer/usage`、对客 `/v1/customer/billing`、管理端 `/api/v1/accounts/*/usage`）返回夹具行来断言占位与 0：A8 拦 `/v1/customer/billing` 返回只含 `usage.images` 的汇总，断言视频项显示「—」而不是 0。e2e 库不导供给素材、不起 Worker，往 `generation.jobs` 插行要 `vendor_model_id`、`offering_id`、`channel_id` 与 `runtime_revision_id` 整条供给链，拦截是既有先例（`portal-navigation.spec.ts`、`admin-console-detail-addresses.spec.ts`）。既有断言里读逐条 `image_count` 或汇总顶层 `images` 的 Rust 合同用例（`apps/api/tests/http_contract` 的 `cases_pricing.rs`、`cases_billing.rs`、`cases_cost_facts.rs`）跟着换成 `usage`；浏览器用例里没有这类断言。浏览器行为按 [Web 规则](../../apps/web/AGENTS.md)在同一命令下运行；本稿没有执行证据。
