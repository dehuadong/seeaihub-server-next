主题: 火山方舟 Doubao Seedream 供给与计量计价泛化
当前修订: v1
状态: 待评审；**已移出第二阶段范围**（2026-09-19，见工作项 `dehuadong/seeaihub-server-next#2` 的规划范围）——它是「引入另一个 Vendor 的 Vendor Model」这一**独立工作项**的设计草案，不属于「同一 Vendor Model 由多个 Provider 供应」的范畴

# 火山方舟 Doubao Seedream 供给与计量计价泛化

本文是**火山方舟接入**这一独立工作项的技术设计权威位置。它**不再是第二阶段（`dehuadong/seeaihub-server-next#2`）的实现依据**：第二阶段的 Vendor 固定为 OpenAI，首批 Provider 为 AIHubMix 与 APIMart，见[工作项 #2](https://github.com/dehuadong/seeaihub-server-next/issues/2) 的规划范围。

**为什么移出**：火山方舟供应的是 **ByteDance 自有的 Seedream**，它**不供应 `gpt-image-*`、也不供应 `gemini-image`**。因此它与 AIHubMix 不在同一个 Vendor Model 上竞争，「AIHubMix + 火山方舟」**不构成**「同一 Vendor Model 由多个 Provider 供应」，而后者正是 #2 的核心命题。它是另一件事，应当独立规划。

**为什么另立本文而不是并入 [0002-image-generation-tech-design.md](./0002-image-generation-tech-design.md)**：0002 的 §5（原生能力 Schema 的理由）、§6（计量证据）、§9（Adapter 首期能力与错误分类）都**只对 AIHubMix 成立**——它假定 token 计量、Base64 同响应返回、`additionalProperties: false` 的上游。第二个 Provider 在这三处都不成立（按张计量、预签名 URL 异步取图、静默放行未知字段）。把两套 Provider 合同塞进同一份设计会让 0002 无法独立演进，因此按 `docs/agents/artifacts.md`「独立技术设计 RFC 需要独立评审、复用或演进时拆出」的规定另立本文。0002 保持其已评审通过状态，其通用部分（统一 Command、身份分离、Revision 发布、归档优先）继续有效。

**本文件与 ADR 的职责边界**：持久决定在 `docs/adr/`（本设计引用 [0002](../adr/0002-native-capability-schema-not-canonical.md)、[0003](../adr/0003-postgresql-is-source-of-truth.md)、[0004](../adr/0004-vendor-and-provider-identities-stay-separate.md)、[0005](../adr/0005-billing-path-uses-v1-endpoints.md)、[0006](../adr/0006-no-settlement-without-metering-evidence.md)、[0007](../adr/0007-reconciliation-instead-of-automatic-retry.md)、[0008](../adr/0008-own-object-storage-is-the-platform-result.md)、[0009](../adr/0009-multiple-active-offerings-and-routing.md)、[0010](../adr/0010-metering-evidence-is-unit-bearing.md)、[0011](../adr/0011-safe-before-acceptance-does-not-retry-yet.md)），本文只承载该 Provider 的技术设计细节，不复制决策正文。其中 0005（正式计费路径走 OpenAI 兼容 `/v1`、`/ai/v1` 暂列为已验证能力）是本 Provider 选择「同步 POST + 预签名 URL 取图」这一执行策略的同类决策依据。

## 1. Provider 与模型身份

| 对象 | 值 |
| --- | --- |
| Vendor | `ByteDance`（**平台目录命名决定**，不是上游声明的字段） |
| Vendor Model | `doubao-seedream-5-0-260128` |
| Provider Kind | `VolcengineArk` |
| Adapter | `doubao-ark-image-v1` |
| Provider Model ID | `doubao-seedream-5-0-260128` |
| Channel Base URL | `https://ark.cn-beijing.volces.com` |
| Credential | 环境变量 `DOUBAO_API_KEY` |

首期只发布基础模型。排除 `doubao-seedream-5-0-pro-260628`（图层拆分的第二套 `size` 语义与第二套计价）、别名 `doubao-seedream-5-0-lite-260128`（身份未证）、`stream`、组图、图层拆分、`tools`、`image` 数组。

## 2. 上游 wire 合同

### 2.1 请求

`POST /api/v3/images/generations`，`Authorization: Bearer $DOUBAO_API_KEY`，`Content-Type: application/json`。

首期允许的顶层字段（闭合集合，`additionalProperties: false`）：

| 字段 | 类型 | 约束 |
| --- | --- | --- |
| `model` | string | `const` `doubao-seedream-5-0-260128` |
| `prompt` | string | minLength 1 |
| `size` | string | 枚举已声明值；另有像素总数区间（基础模型下限 3,686,400；一手文档给出上限 6000×6000 = 36,000,000，**基础模型的实际区间上限未实测**）与宽高比 [1/16,16] 的合取约束 |
| `output_format` | string | enum `jpeg` / `png`（上游默认 `jpeg`） |
| `watermark` | boolean | 上游默认 `true`；**首期由 Adapter 固定为 `false`，不可配置**（见 §2.2 末） |
| `sequential_image_generation` | string | `const "disabled"` |
| `image` | string | 单参考图；**不接受数组**；单张 ≤ 30MB |
| `optimize_prompt_options` | object | 闭合对象，`mode` enum `standard` / `fast` |

**不声明**：`n`、`quality`、`stream`、`tools`、`layer_decomposition`、`background`、`sequential_image_generation_options`、**`response_format`**。

**`watermark` 不可配置（首期固定 `false`）**：`restrictions` 只认 `allowed_branches`/`max_images`，Adapter Descriptor 只有参数名白名单，`validate_native_request` 也不注入策略默认值——因此该字段当前**无处承载发布策略值**。本阶段决定由 Adapter 固定为 `false`（避免上游默认 `true` 引入水印），**不新增发布字段**；代价是它不能按 Offering 变化，若将来需要按 Offering 配置，必须新增受校验的 `native_parameter_defaults` 并明确它能设置哪些字段。

**为什么 `response_format` 不开放**：上游支持 `url` 与 `b64_json` 两种返回，但本设计的响应处理规则（§2.4）与取图路径（§2.5）**只覆盖 URL 分支**。若在 Schema 里声明 `b64_json`，就会发布出**平台无法执行的能力**——正是 #2「不能伪造能力」的反面。首期由平台在请求组装时固定 `response_format = url` 并在受理前拒绝客户端传入该字段；将来支持内联 Base64 时，必须先把响应处理与结果交付两条路径都补齐再发布。

**为什么把 `n` 排除在 Schema 之外**：实测 `n=2` 返回 HTTP 200 但只出 1 张——上游**静默忽略**该参数。在 Schema 里声明它会造成「该参数生效」的错觉，因此不声明，并让平台在受理前拒绝它。

### 2.2 尺寸约束的表达分工

「像素总数 ∈ [下限, 上限] ∧ 宽高比 ∈ [1/16, 16]」是**合取约束，JSON Schema 无法表达**（枚举与正则都不足以表达像素乘积与比值）。分工：

- **Native Capability Schema**：声明 `size` 的枚举与类型，作为第一道；
- **Adapter 的发布期校验**（`validate_publication`）：证明被发布的枚举值都满足该模型的像素与宽高比约束；
- **受理期校验**（`crates/application`）：对每个请求值计算像素数与宽高比并判定。

这个分工必须显式记录，否则会误以为 Schema 已经封住该约束。Schema 仍是「原生字段与取值」的权威，只是**不独自承担**这条数值约束。

### 2.3 响应

成功：`{ model, created, data: [ { url, size, output_format? } ], usage: { generated_images, output_tokens, total_tokens, input_images? } }`

- `data[].size` 为 `宽x高` 字符串（实测存在）；
- `usage.generated_images` 是**唯一计费依据**；`output_tokens`/`total_tokens` 是 `宽×高/256` 的派生量，平台可自行复算，不构成独立计量事实；
- `usage.input_images` 在基础模型响应中**实测缺席**（仅 pro 出现），因此首期不得依赖该字段；
- 响应体**不含请求 id**；`x-request-id` 只在响应头。

错误：`{ error: { code, message, param, type } }`。**判定读 `error.code`，不读 HTTP 状态码**。

### 2.4 响应处理规则

| 情形 | 处置 |
| --- | --- |
| 2xx，`data[]` 恰好 1 个成功元素 | 正常路径 |
| **2xx，且 `usage.generated_images == 0`** | **可判定失败**：Provider 自述没有成功输出（审核等原因未出图即不计费）⇒ `failed` + 释放预授权，**不写金额、不进对账**。与「元素数 ≠ 张数」是不同情形，不得按同一条规则送进人工队列 |
| 2xx，元素数 ≠ `usage.generated_images`（且 `generated_images > 0`） | `reconciliation_required`（`metering_count_mismatch`） |
| 2xx，元素含 `error` 且 `generated_images = 0` | `failed`，释放预授权 |
| 2xx，元素含 `error` 且 `generated_images > 0` | `reconciliation_required` |
| 2xx，`data[].size` 缺失或不可解析 | `reconciliation_required`（`metering_size_unparsable`） |
| 2xx，像素落在已声明档位之外 | `reconciliation_required`（`metering_band_missing`） |
| 2xx，但 `usage` 缺失 | `reconciliation_required` |
| **非 2xx 或顶层 `error`，但响应携带 `usage.generated_images > 0`** | **不得按 `error.code` 判失败**：Provider 已自述成功出图 ⇒ 按「已确认生成、未交付」处理，`reconciliation_required` + 保留预授权。**不得把「有 usage」等价于「产生了费用」，也不得把「有 error」等价于「未生成」** |
| 非 2xx 且 `error.code` 存在，且无 `usage` 或 `generated_images == 0` | 按 §4 分类表 |
| 非 2xx 且无 `error.code` / 解析失败 | `reconciliation_required` |

「单图请求是否仍可能出现 `data[].error`」**未实测**，因此上表两种情形都实现。

### 2.5 取图

结果 `url` 是 TOS 预签名 URL（URL 参数含 `X-Tos-Expires=86400`，即 24 小时）。**下载由 Adapter 在 `execute` 内完成**，并完成魔数校验与尺寸解析后返回字节：

- `WorkerService` 不需要新增「拉取上游 URL」的端口能力，`AssetStore` 仍只服务自有对象存储；
- 下载耗时计入 Adapter 调用预算。现有 `PROVIDER_TIMEOUT_SECONDS=660` 与 `WORKER_LEASE_SECONDS=900` 留有余量，`execute_with_heartbeat` 在等待期间持续续租；Adapter 必须在超时前返回，并区分「调用超时」与「下载超时」；
- 下载失败 / URL 过期 ⇒ 已确认生成、未交付 ⇒ 对账，**不重新生成**；
- 结果必须先归档到自有对象存储才能标记成功（ADR-0008）。

**仍未实测**：下载是否需要额外鉴权。实施期受控验证必须覆盖（见 §6）。

## 3. 计量、计价与结算

### 3.1 计量证据

```text
MeteredUsage::Images { generated_images, images: [ { size, width, height } ], input_image_count }
```

- `generated_images` 是**唯一 Provider 计量事实**（声明计费了几张）；
- `images[].size` 是**计价所需的 Provider 结果属性**（决定每张落在哪个像素档位），与前者角色不同但缺一不可；
- `input_image_count` 是**受理时固化的请求侧事实**（由 `PreparedImageRequest.assets` 中 `native_path == "/image"` 的项计数），**不是** Provider 计量事实——放进 Evidence 是因为计价公式需要它。实测基础模型**不返回** `usage.input_images`，故不能从响应取；「非 pro 是否承诺不返回」在一手文档中未获承诺（保留该保留）；
- 金额 = Provider 事实 × 已发布单价，不是平台估算。硬约束见 `docs/adr/0010-metering-evidence-is-unit-bearing.md`。

### 3.2 首期 Price Plan

| 项 | 值 |
| --- | --- |
| 计价形式 | `PerImageBands`，单档覆盖全域（**首档 `min_pixels` 必须为 0**，发布期强制） |
| 档位 | `[{ min_pixels: 0, max_pixels: None, unit_microusd }]` |
| 输入图 | 免费（`input_image_microusd = 0`） |
| 原生价 | **候选值**：`CNY`，`amount_minor = 22`（0.22 元/张），`minor_units_per_major = 100` —— **归属待 V5 结清** |
| 汇率 | `fx_microusd_per_major` 整数，附 `fx_source_url` 与 `fx_captured_at`（发布必填）；不得使用规划工件里那个**无出处的算术示例值** |
| 价格来源 | `https://docs.volcengine.com/docs/82379/1544106`，抓取时间 2026-09-19 |

**为什么单档覆盖全域**：一手价格页**只有 `lite` 行 0.22 元/张，没有基础模型 `doubao-seedream-5-0-260128` 那一行**；「基础模型 = lite 别名」依据同一最小像素约束，只是**推论**。发布一个显式覆盖全域的单档，使「每张图必须唯一落到已声明档位」这条校验始终有对象，同时不发明没有依据的分档。

**阻断项 V5**：在「基础模型实际单价」用一次受控付费调用 + 控制台账单核对结清前，本 Price Plan 的 `unit_microusd` **只能作为候选值**用于算术与合同测试，**不得作为已审核发布的正式计费值上线**（`docs/adr/0006` 的证据门槛）。

**未决**：小尺寸档位（例如 1024×1024 是 0.11 还是 0.22）同样无法由文档判定，需同一次校准覆盖，**阻塞细分档位发布，但不阻塞首期**（任何尺寸落同一档）。

### 3.3 传输与迁移

端口改动、`metering_evidence` jsonb 的兼容性（只写不读，加判别字段 + `#[serde(default)]`）、`pricing.price_plans` 的四列 → `formula`/`native_pricing` 迁移、以及计价分派落在 `crates/domain` 的理由，见工作项 #2 的规划工件；本文不复制其正文。

## 4. 错误分类

判定**只依据 `error.code`**。分类与处置见 `docs/adr/0011-safe-before-acceptance-does-not-retry-yet.md` 与 #2 规划工件的分类表；本文只记录该 Provider 特有的注意点：

- **`QuotaExceeded` 有两义**：免费额度耗尽（未受理）与排队任务数超限；前者不可重试，后者不能证明未生成。必须按 message 区分，无法区分时按 `AcceptanceUnknown`。
- **生成期失败是独立的 code**：`image` 指向不可达 URL 时返回 400，message 为下载失败，**不是** `InvalidParameter`。这类失败不能证明「上游未生成」，不得归入 `NotRetryable`。
- **不得用「HTTP 400 ⇒ 不可重试」作规则**：参数校验错误可能以其它状态码承载（同类上游有 500 承载 `build_request_failed` 的先例），必须读 `error.code`；无 code 时按 `AcceptanceUnknown`。
- **499 与连接中断不能判定**，按 `AcceptanceUnknown`。

## 5. 能力边界（该 Provider 的结构性限制）

- **无异步任务、无 task id、无幂等键**：唯一对账标识是响应头 `x-request-id`。因此「受理是否确定」在本 Provider 上**只能降级、不能技术恢复**——`reconciliation_required` 无法自动收敛，只能人工提单。这是相对 AIHubMix `/ai/v1` 的能力倒退。
- **无账单/用量 API**（实测 `/api/v3/billing/*`、`/api/v3/usage` 均 404）：费用只能在火山控制台人工核对，对账流程不得假设存在 Provider 账单 API。
- **未知字段静默放行**：平台必须自己受理前校验，不能依赖上游拒绝（见 `docs/adr/0002`）。
- **不提供机器可读 schema 端点**：Native Capability Schema 的上游「版本」只能记为文档页 + 抓取时间，不得伪造版本号。

## 6. 验收条件（本 Provider 部分）

- 文生图与图生图（单参考图）两分支各产出可核验 `MeteredUsage::Images`；
- `output_format=webp`、`size='1x1'`、`n=2`、未知字段四类请求在**受理前**被平台拒绝，且零上游调用；
- 结果在 Adapter 内完成下载与魔数校验；下载失败 ⇒ 对账且无第二次上游 POST；
- `data[].error` 的两种情形（`generated_images` 为 0 / 大于 0）各有可判定测试；
- 内容审核拒绝的真实 code 经一次受控付费调用确认，并据此校正分类表；
- 结果下载是否需要额外鉴权经一次受控验证确认。

## 7. 第一方资料索引

- 图片生成 API：`https://docs.volcengine.com/docs/82379/1541523`（2026-09-19）
- 图片生成教程：`https://docs.volcengine.com/docs/82379/1824121`（2026-09-19）
- 模型价格：`https://docs.volcengine.com/docs/82379/1544106`（2026-09-19）
- 错误码：`https://docs.volcengine.com/docs/82379/1299023`（现网返回 SPA 骨架，无法逐字核对）
- 免费推理额度：`https://docs.volcengine.com/docs/82379/1399514`

脱敏后的本机实测记录：`out-reference/doubao/doubao-ark-image-research.md` 与其中 §15 的实测补录（外部参考资源，不是平台接口合同）。资料更新时先形成候选快照并做差异审查。
