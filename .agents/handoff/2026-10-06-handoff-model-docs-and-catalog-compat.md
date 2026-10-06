# 交接：模型使用文档已交付 / 目录 OpenAI 兼容待裁决

生成时间：2026-10-06。仓库 `/home/mypc/work/seeaihub-server-next`。本交接按用户指定放在 `.agents/handoff/`，覆盖 handoff 技能的临时目录默认位置。

## 当前状态

工作项 [模型目录关联独立使用文档与公共接口说明](https://github.com/dehuadong/seeaihub-server-next/issues/73) 的实现已交付并推送，状态 OPEN、`proposal:complete`。HEAD == origin/main == `81fd5dc`，工作区干净。

`81fd5dc` 之前还有本任务的后续修正提交，**工作项评论尚未同步到这三条**：

- `0d78004` 模型说明与公共文档的链接改用平台对客基址（新增必填配置 `SEE_BASEURL`）
- `16b0777` public-docs 源码链接改用 `{{SEE_BASEURL}}/v1/docs/<名>`，服务端 `render_public_document` 代入
- `81fd5dc` 对客入口 `/v1/docs/README.md` 成为第四份公开文档；Spec 0008 → v2

CI 未核实：`0d78004` 之后 `gh` 走 GitHub API 持续代理超时；提交本身已推送成功（HEAD == 上游）。

## 唯一未决项：`/v1/models` 的 OpenAI 兼容

用户指出 `/v1/models` 不符合 OpenAI 兼容标准。现状与标准的差别：

```
我们：  {"data":[{"name","vendor_id","revision","type","contract","documentation_url"}]}
标准：  {"object":"list","data":[{"id","object":"model","created","owned_by"}]}
```

这不是漏做：Spec 0001 §6 把单条字段定成 `name`/`vendor_id`/`revision`/`contract`（之后只增 `type`、`documentation_url`），平台没有任何文档主张 `/v1/models` 的 OpenAI 兼容。

已给用户的建议（**等回复，不要未经裁决直接实现**）：只增字段、不破现有形状——

| 新增 | 取值 |
| --- | --- |
| 顶层 `object` | `"list"` |
| 每条 `id` | 平台 `name`（客户端本来就把它填进 `model`） |
| 每条 `object` | `"model"` |
| 每条 `created` | Unix 秒，取该条当前 Runtime Revision 的发布时间 |
| 每条 `owned_by` | `vendor_id` |

`name`/`contract`/`documentation_url` 全部保留。错误响应形状（我们 `{"error":{"code","message"}}` vs OpenAI `{"error":{"message","type","param","code"}}`）这次不动，要做得另开一条。

属主是 Spec 0001 §6 的字段清单；Spec 0006 给 `type` 的"只增字段"是同类先例。裁决后要出 Spec 修订、实现、同步用例。

## 工件与职责

| 内容 | 位置 |
| --- | --- |
| 行为合同与验收 | [模型使用文档 Spec](../../docs/specs/0008-model-usage-documentation.md)（v2 已接受；§2 公开文档含 `README.md` 入口） |
| 技术设计与理由 | [模型使用文档 Agent Note](../notes/implemented/platform/2026-10-05-model-usage-documentation.md) |
| 目录条目的兼容面（待改） | [控制台 Spec](../../docs/specs/0001-admin-and-customer-consoles.md) §6 的字段清单 |
| 渲染与占位符 | `crates/application/src/model_document.rs`：`PUBLIC_DOCUMENTS`、`BASE_URL_PLACEHOLDER`、`validate_base_url`、`render_model_document`、`render_public_document` |
| API 配置与端点 | `apps/api/src/main.rs`：`SEE_BASEURL` 必填、`read_public_document`、`read_model_document` |
| 素材导入 | `crates/persistence/src/material_import.rs`：`narrative_path` 解析、`public_docs` 显式传入、`IMPORT_VALIDATION_BASE_URL` |
| 对客文档源码 | `public-docs/`：`README.md` 是入口；跨文档链接写 `{{SEE_BASEURL}}/v1/docs/<名>` |
| 配置项 | `docs/operations/configuration.md`、`.env.example`、`docs/operations/production.md`、`docs/operations/development.md` |
| 工作流与规则 | [根 AGENTS.md](../../AGENTS.md)、[工程流程](../../docs/agents/engineering.md)、[文档标准](../../docs/AGENTS.md)、[提交与推送](../../docs/agents/git.md) |

工作项拥有选定范围、S1–S5、排除项与进度；合同与技术正文由上述仓库工件拥有，本交接不重写。

## 关键事实与坑

- **`SEE_BASEURL` 是 API 必填配置**（只写源、不带结尾斜杠）；缺了或格式不对进程起不来。它必须在**发布前**定好：正文里的绝对地址在发布时代入，从请求主机取会把管理端地址冻进不可变版本。
- 已发布的模型说明是**冻结快照**：改文案或改基址后旧版本仍返回旧正文；要让目录里的当前版本换形，必须**重新发布**（引用式发布即可，素材文件不用改）。旧版本保持旧域名形态是正确的历史语义。
- 本地开发库 `seeai_next` 里 `image-2.5-plus` 已被重新发布过几次（文档版本换新）；本地 `.env` 已补 `SEE_BASEURL`。**8081 那个进程跑的是旧二进制，要重启才有入口 `/v1/docs/README.md`**；模型说明在库里已更新，不重启也看得到。
- 素材导入**显式接收公开文档目录**（`import_supply_materials(pool, dir, public_docs)`）：集成测试在 crate 根下跑，按进程工作目录找 `public-docs` 会失败。导入期校验用占位基址 `http://import.invalid`。
- 对客正文链接统一由渲染写成 `{SEE_BASEURL}/v1/docs/<名>`；`public-docs/**` 源码里跨文档链接必须写 `{{SEE_BASEURL}}/v1/docs/<名>`，**不要写相对链接**（这是上一次返工的原因）。

## 用户明确表达过的偏好（必须遵守）

- **简单改动不要铺测试**：用户批评"这么简单的功能为什么写那么多测试"。按改动面取最小证据，其余交 CI；改了源码要**当场用真实请求给结果**，不要只报测试条数。
- 对客可见的东西要**真的改到源码里**：上次只在服务端改写、`public-docs/` 源码没动，用户直接问"为什么没有变化"。
- 回复用中文简体，先给结果与事实，不要流水账、不要黑话。

## 已有验证证据（本轮实际跑过）

- `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`：通过。
- `cargo test -p seeai-application --lib model_document`：7 条（含 `{{SEE_BASEURL}}` 代入与非法基址拒绝）。
- http_contract：`cases_model_document`（公共文档与入口）、`publication`/`model_type`/`migrations` 组、`cases_identity`：通过。
- `cargo test -p seeai-persistence --test material_import_idempotency -- --ignored`：4 条通过。
- web e2e：8 份发布夹具 10 条通过。
- 真实请求：`GET /v1/docs/README.md`（200、`text/plain`、绝对链接）；导入 + 重新发布后 `GET /v1/models/image-2.5-plus/llms.txt` 第一段链到入口。

## 建议技能（Suggested skills）

- `planning`：要改 `/v1/models` 的对客形状（对客协议）时，先按规定出一条"只增字段"的 Spec 修订。
- `implement`：用户给出"执行实现"且 Implementation Gate 通过后、写第一行代码之前加载。
- `code-review`：实现结束后执行 Implementation Review，修正后再交付。
- `verify`：交付验证按工程流程执行；不要把本轮渲染器/合同检查当成目录兼容面的验收。
- `handoff`：下次再交接时用。

提交按路径选择本任务文件，不使用 `git add -A` 混入其他任务；推送后核对 HEAD 与上游一致，不改写历史。
