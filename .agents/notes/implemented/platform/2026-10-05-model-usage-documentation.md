---
title: 模型使用文档随发布提供
status: implemented
created: 2026-10-05
updated: 2026-10-05
approval: 用户于 2026-10-05 同意方案，并在执行授权后裁定内联发布可自带文档素材（方案 b）
verification: cargo test -p seeai-application --lib model_document（6 条）、cargo test -p seeai-persistence --lib material_import（12 条）、cargo test -p seeai-persistence --test material_import_idempotency -- --ignored（3 条）、cargo test -p seeai-api --test http_contract -- --ignored 的 cases_model_document（6 条）与 cases_publication / cases_migrations、npx playwright test 的 8 份发布夹具（10 条）
---

# Agent Note：模型使用文档随发布提供

## 问题

[目录读取](../../../../apps/api/src/main.rs)只给模型身份和 JSON Schema，客户端能读取参数结构，但缺少参数含义、素材流程和完整调用示例。把各种类型和厂商写进一篇图片说明无法维持模型之间的差别，引用内部规范或渠道文档也不能满足客户仅凭平台说明完成调用的需要。

参数合同已由[厂商模型合同设计](../../../../docs/design/0005-vendor-model-contract-and-offering-mapping.md)与[模型发布设计](../../../../docs/design/0012-platform-model-publishing.md)拥有。同一次发布的说明必须与它一致，不能在目录读取之后又取到别次发布的参数解释。

## 决定

[模型使用文档 Spec](../../../../docs/specs/0008-model-usage-documentation.md)（现 v2）§1–§5 已按本记录交付。没有新建独立 ADR：文档的内容边界属于该 Spec，存储和渲染沿用既有不可变发布机制。

### 文档素材与导入

厂商模型素材顶层新增 `documentation`（与 `capability_schema` 同级）：`narrative_path` 指向 `public-docs/` 下的叙述正文，`fields` 用从合同根算起的 JSON Pointer 绑定属性（`/properties/<名>`）与组合约束（`/allOf/<下标>`）的释义。身份、参数表和示例里的模型值使用受控占位符（`{{platform_name}}`、`{{vendor_id}}`、`{{model_type}}`、`{{contract_revision}}`、`{{parameter_table}}`），不靠全局替换厂商原生名生成平台别名。

导入把 `narrative_path` 解析成正文内容，与合同逐项对齐校验：缺释义、多出合同没有的释义、模板缺 `{{parameter_table}}`、正文链接越出公共文档范围都拒绝；随后按 `(vendor_model_id, content_hash)` 落一条素材版本，同一内容复用、内容变化追加。素材可以由两个厂商模型修订共享，但导入后各归各自的 Vendor Model。

发布读取素材有两条路。引用式发布（运营那条）不接受命令里的文档，只读同一厂商模型最近导入的素材。内联发布（工程侧那条兼容形状）可以在命令顶层带同一份 `documentation`（`narrative` 直接给正文，或 `narrative_path` 由 API 从 `public-docs/` 读成正文），发布把它落成该厂商模型的素材版本。两条路都没有素材即拒绝——不能通过任何一条绕开文档要求。

### 发布与持久化

渲染在发布事务之前进行。参数表从同版合同递归生成：类型、必填、固定值与枚举、数值与长度/数量上下界、数组元素、`anyOf` / `oneOf` 组合取值、默认声明与合同自带的 `description` 都读得到；型号身份用**平台对客名**替换合同里冻结的厂商原生名。叙述里的身份占位符被替换，`{{SEE_BASEURL}}` 代入部署期配置的平台对客基址，指向公共文档的相对链接统一写成 `{SEE_BASEURL}/v1/docs/...` 绝对地址；正文上限 256 KiB，超限拒绝、不截断。基址在**发布时**代入——发布走管理端主机、读取走对客主机，从请求主机取会把管理端地址写进不可变版本，所以它是必填配置，缺了拒绝发布。

渲染结果随发布写进 `publication.model_documents`：独立 UUID、平台对客名、Runtime Revision 外键，一个修订一条，正文不可变。合同、候选与文档在**同一个发布事务**里原子生效，写正文失败整次回滚。已发布正文不再依赖素材文件或最新指针，历史修订可以没有文档关联。

### 读取与切换

目录每条加 `documentation_url`：`/v1/models/{按路径段编码的 name}/llms.txt?version={文档 UUID}`，由目录取数时一次取到合同与文档标识后投射成根相对地址。`GET /v1/models/{name}/llms.txt` 无版本时按与目录**同一条**可调用判据取当前正文，带 `version` 时只按（平台名, 文档标识）读历史正文、不看当前是否启用；未知或错配的版本一律 404，不回退到当前版本。`GET /v1/docs/{名称}` 只提供 Spec §2 的 `authentication.md`、`uploads/images.md`、`http-errors.md` 三份，资源只从 `public-docs/` 选，名称清单只有一处属主；服务时把源码里的相对链接按同一基址写成绝对地址，与模型说明同一形态。

首次开放目录字段之前，启动时 `ensure_current_model_documents` 在**已发布**（`re.active`）集合上为每个还没有文档的模型生成快照，**不看运维开关**：停用的模型之后随时可能被重新启用，那时缺文档就会被目录的取数悄悄藏掉。缺素材即启动失败并点名模型，不隐藏模型、也不返回伪造正文；补齐只新增文档记录，不改既有 Runtime Revision、价格或路由。

## 备选方案

只给 Schema 加 `description` 的改动较小，但无法充分表达素材处理、调用例子与结果读取，也会使普通释义修正碰到合同不可变规则。

直接维护按名字寻址的 Markdown 静态文件容易开始，但平台别名与同版参数靠手工对齐，目录读取和模型重新发布之间可能串版。

选用独立文档素材加发布快照：参数规则从合同生成，叙述单独维护，平台名和版本由发布注入。代价是增加素材记录、发布校验和公开读取，而不增加客户端对渠道的了解要求。

## 后果

不可变正文增加存储量，256 KiB 上限避免无界输入；本轮不建立清理策略或历史文档过期规则。公共规则说明单独维护，修改它们时必须同步客户使用说明；不能把模型正文的冻结解释为冻结服务行为。模型正文的历史保证限定于已保存的模型正文及参数，不扩大为公共规则历史或旧版模型执行保证。

结构约束可以从 Schema 生成，参数含义与适用场景仍需人工核实；渲染器不靠字段名猜含义，发布校验拦住结构遗漏，人工审阅负责语义一致性。

## 验证

离线：`cargo test -p seeai-application --lib model_document` 覆盖参数表（含数组元素、`anyOf`、合同 `description`）、型号身份取对客名、image / video / chat 身份隔离、缺释义与越界链接拒绝、256 KiB 上限。导入：`cargo test -p seeai-persistence --lib material_import` 覆盖 bootstrap 素材解析与素材解析校验；`cargo test -p seeai-persistence --test material_import_idempotency -- --ignored` 覆盖同一内容复用、内容变化追加版本。

真实数据库的 HTTP 合同：`cargo test -p seeai-api --test http_contract -- --ignored` 的 `cases_model_document` 覆盖目录文档地址、公开读取正文与公共文档、历史版本冻结、停用后历史仍可读、发布失败不改旧正文、保留字符模型名往返；`cases_migrations` 的命名迁移用例覆盖既有模型启动补齐；`cases_publication` 与其余分组覆盖发布路径不回归。浏览器：8 份发布夹具的内联发布随 `npx playwright test` 通过。
