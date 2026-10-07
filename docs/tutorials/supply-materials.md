# 素材与模型升级

工程师写的发布素材（`config/bootstrap/*.json`）是渠道、供给、厂商模型合同与模型说明的来源。升级素材 = 改素材文件、随新版本一起发布、按需重新发布受影响的模型。本文写这三步各自怎么走、什么情况下服务起不来，以及本地开发库怎么处理。

素材按身份键幂等导入的完整规则与理由归 [`docs/design/0012-platform-model-publishing.md`](../design/0012-platform-model-publishing.md) §3；模型说明的合同与版本保证归 [`docs/specs/0008-model-usage-documentation.md`](../specs/0008-model-usage-documentation.md)。

## 1. 素材什么时候导入

导入是**工程侧**的动作，随 API 启动在迁移之后跑一次：

- 素材目录由 `SUPPLY_MATERIAL_DIR` 给；不设时默认 `config/bootstrap`（相对进程工作目录）。
- 显式设成**空白**、目录不存在或没有 `*.json` 时跳过——测试库与开发库用它保持干净的供给清单。
- 素材本身坏、或与库里已有行冲突，导入**报错并让启动失败**，不静默跳过。

导入只写供给目录（`catalog.vendor_models`、`supply.channels`、`supply.offerings`、`pricing.price_plans` 与模型说明素材版本），**不建立网关模型、不发布、不改价**：对客名与定价是运营发布时的事。

## 2. 改素材后怎么生效

| 改什么 | 换不换 `native_revision` | 导入时 | 何时对客生效 |
| --- | --- | --- | --- |
| 合同 `capability_schema` | **必须换** | 新建一行厂商模型；旧行留作历史 | 部署后**重新发布**该模型 |
| 模型 `type` | **必须换** | 同上 | 同上 |
| 只改模型说明（叙述正文或 `fields` 释义） | 不换 | 追加一条说明素材版本 | **重新发布**后产生新的文档版本；旧版本地址仍读旧正文 |
| 供给技术定义（`adapter_key`、`provider_model_id`、`carrier_schema`、`parameter_mapping`、`restrictions`、`formula`、`cost_unit_price_microusd`） | 不换 | **就地更新** `supply.offerings` 那一行（不动 `enabled`） | 重新发布后才进受理 |
| 渠道身份（`provider_kind`、`base_url`、`credential_env`） | 不换 | 新增一行 `supply.channels`，旧行留着；供给落到新渠道行 | 重新发布后生效 |
| 费率（`price_plan` 的四档） | 不换 | 同 `(offering_id, currency, source_url)` 且四档全同则复用，变了追加一行 | 重新发布后生效 |
| 对客参考价目（顶层 `consumer_reference_rates` 的四档） | 不换 | **就地更新** `catalog.vendor_models` 那一列（模型级一份，与成本形态无关；没写就是没有） | 重新发布后生效（运营发布页的初始价取它） |

「重新发布」是运营在管理接口做的引用式发布（选供给 + 给价），它生成一份新的 Runtime Revision；已发布修订的技术定义与说明是冻结快照，导入不回头改它们。

## 3. 合同或 `type` 变了却没升修订：启动会失败

同一 `(vendor_id, native_model_id, native_revision)` 的合同行**不可变**：改合同或 `type` 而不换 `native_revision`，导入在启动时报错并让进程起不来，例如：

```text
Error: validation failed: config/bootstrap/gpt-image-2.5-flare.json: vendor model gpt-image-2.5-flare revision 2026-09-20-contract-1.0 is already in the catalog; the contract row is immutable, so publish the change under a new native_revision
```

处理：给改动的那份素材换一个新的 `native_revision`（例如 `2026-09-20-contract-1.0` → `2026-09-20-contract-1.1`），重新构建部署。导入会新建一行厂商模型，旧行按不可变规则留作历史；已经发布出去的模型仍指向旧行，直到对它重新发布。

升修订是**合同变更的正常路径**，不是绕开校验的开关：它同时改变目录里该模型的 `revision`，所以只改说明文案时不要动它。

## 4. 本地开发库

开发库（`seeai_next`）是一次性的。遇到上一条那种「旧行与当前素材不一致」，把库清掉重来即可——在维护库上执行，连接参数与建库方式见 [`docs/operations/development.md`](../operations/development.md) §2：

```sql
DROP DATABASE seeai_next WITH (FORCE);
CREATE DATABASE seeai_next;
```

下次启动会重建全部迁移、按当前素材重新导入。要保留库里的已发布修订时不要这么做——那属于生产库，按 §3 升修订并重新发布。`WITH (FORCE)` 终止不了别的角色占用的会话时，先停掉那些会话。

## 5. 验证

启动日志里 `supply materials imported` 带 `materials` / `offerings` / `price_plans` 三个计数（没有素材可导时不出这条），随后 `api listening` 带 `bind`。导入之后确认对客面：

- `GET /v1/models`：当前可调用模型的 `revision` 与 `documentation_url` 都是这次发布的那一份。
- `GET /v1/models/{name}/llms.txt`：参数表与正文来自同版合同；正文里的链接是 `SEE_BASEURL` 的绝对地址（API 的必填配置，[配置项](../operations/configuration.md#1-进程与连接)）；只改文案时，旧文档地址仍返回旧正文。
- `GET /v1/docs/README.md`：对客文档入口，讲怎么查模型、目录各字段的含义与 JSON Schema 读法，并索引 `authentication.md`、`uploads/images.md`、`http-errors.md` 三份。这四份随 API 版本提供，与发布哪一份素材无关。
