主题: Vendor Model Contract 与 Offering Parameter Mapping 落地设计
当前修订: v1
状态: 待评审（Plan Review 进行中）

# Vendor Model Contract 与 Offering Parameter Mapping 落地设计

本文是工作项 [`dehuadong/seeaihub-server-next#10`](https://github.com/dehuadong/seeaihub-server-next/issues/10)（调用方合同收口）的技术设计。持久决定归 [`docs/adr/0015`](../adr/0015-vendor-model-contract-and-offering-parameter-mapping.md)，分层职责归 [`docs/design/0004`](./0004-layered-architecture.md)（§3.3 的结构性差距 G5），本文只承载"怎么落地"。

## 1. 问题（一句话）

`capability_schema` 一份数据兼了两个身份：既是**客户端合同**，又是**这条供给能承载的渠道面**。于是对客合同实际等于"命中那条供给的渠道包装面"，随选路变化，且与厂商原生参数不一致。

证据（三家的原生面 vs 渠道包装面）：

| 厂商模型 | 厂商原生面（官方文档） | 现在 offering 声明的面（渠道包装） |
| --- | --- | --- |
| OpenAI `gpt-image-*` | Images API：`size`(像素)、`quality`、`background`、`output_format`、`n`、`image` | AIHubMix `/v1`：`size`(像素，只有 `auto`/`WxH`)、`output_format`、`quality`，**无 `background`** |
| Google `gemini-3.1-flash-image-preview` | `aspect_ratio` + `image_size` | APIMart：`size`=**比例**、`resolution`=**档位**，另加 `nsfw_check`/`official_fallback`/`google_search` |
| ByteDance `seedream-5-lite` | 火山 Ark：`size` 支持**档位或像素（不可混用）**、`watermark`(默认 `true`)、`sequential_image_generation`、`tools`、`response_format` | APIMart 包装面（待按其端点 schema 复核） |

同一个 `size` 在三个模型上分别是像素 / 比例 / 档位；`watermark` 的原生默认是 `true`；火山的执行方式是**同步**，APIMart 把它包成**任务式**。这些差异现在没有承载之处。

## 2. 目标形态（三层，职责单一）

| 层 | 载体 | 内容 |
| --- | --- | --- |
| **Vendor Model Contract**（客户端合同，模型级唯一一份） | `catalog.vendor_models.capability_schema`（已有列） | 客户端能提交哪些字段、类型、枚举、必填、**以及 `size` 这类字段的语义** |
| **Offering 承载面 + 参数映射**（模型 × 渠道） | `supply.offerings` 新增两列 | 这条供给**能承载**合同的哪些字段；以及把合同值转成渠道包装的声明（改名、尺寸语义、显式默认值、枚举映射） |
| **Provider Adapter / Driver**（传输） | `AdapterDescriptor` | 端点、包装形态（JSON / multipart / 任务式）、结果信封、计量与错误分类；只声明**能写上线文的字段名**，不声明"客户端有哪些参数" |

## 3. 数据模型改动

- **素材**：顶层新增 `capability_schema`（Vendor Model Contract）；offering 里原来的 `capability_schema` 改名为 `carrier_schema`（承载面）。过渡期**合同来源唯一**（顶层优先，缺了才用 offering 级旧字段），避免同一个模型落成两行、两份合同。
- **唯一键**：`catalog.vendor_models` 的唯一键由 `(vendor_id, native_model_id, native_revision, schema_hash)` 改成 `(vendor_id, native_model_id, native_revision)`——合同是模型级的唯一一份，不能再按 `schema_hash` 分叉（`ADR-0015`）。`schema_hash` 列随之**删除**：它的两个用途（同修订多份内容各占一行、快照指纹）都已消失；重发按内容比对保证幂等，改内容必须发新修订。
- **合同行不可变**：发布合同一律**插入新行**，不再对既有行 `DO UPDATE`；否则"Job 固定受理时版本"不成立（`ADR-0003`）。
- **Job 冻结**：`carrier_schema` 与 `parameter_mapping` 纳入 `PublishedOffering`（受理时随 Job 冻结）；`publication.runtime_entries` 的快照哈希从 `schema_hash` 换成"合同 + 承载面"的哈希，保证路由判定事后可重建。
- **发布**：`catalog.vendor_models.capability_schema` 写合同；`supply.offerings` 新增 `carrier_schema jsonb` 与 `parameter_mapping jsonb`。
- **迁移**：新增增量迁移（两列 + 唯一键替换）；已发布修订不回填，新发布必须给 `carrier_schema`。

## 4. 校验规则（发布期与受理期各一条线）

发布期：

- **R1** `carrier_schema` 的每个顶层字段必须**从合同可达**：合同直接声明它、被 `rename` 从某个合同字段接过来、或是尺寸换算的目标字段（供给不能凭空多出参数）。
- **R2** `carrier_schema` 的每个字段必须能通过映射落到 Driver 的 `wire_parameters` 上（否则"声明了发不出去"）。
- **R3** 分支与图片数上限仍对 Driver 的 `supported_branches` / `max_images`。

受理期：

- **R4** 请求按**合同**校验（必填在场、字段名属于合同）；合同里没有的字段**丢弃**（用户 2026-09-20 的决定，`ADR-0018` 修订）。
- **R5** 对每条候选做**承载校验**：请求里**用到的**每个字段（非空值）都必须在它的 `carrier_schema` 里，否则该候选**不合格**（写进 `routing_decisions` 的 skip_reason）；全部候选都不合格 → 明确报"无可用供给"（**绝不静默丢参**）。

> R4 与 R5 是两件事：前者是"客户端写了合同没有的字段"（丢弃），后者是"合同里有、但这条供给承载不了"（明确失败或换供给）。交接文档禁止的静默降级说的是后者。

**图片字段是 R4 的例外**：`image`/`image_urls`/`mask` 不是"多带的旋钮"，而是请求的实质——合同没声明它们时**不能丢弃**（丢图等于悄悄生成一张没有参考图的图，还照样计费），一律 **400 `invalid_parameter`**，让调用方换模型或去掉参考图。

**R5 的报码与选路**：全部候选都不合格是**平台侧供给问题**，不是消费者参数错——按 `ADR-0017` 的责任方原则，对客应表现为平台侧故障（`platform_unavailable`），不是 400 `validation_error`。选路方面，R5 默认继续试下一条候选，沿用 `ADR-0009` 的既有做法；交接文档要求"没有运营策略时不得自行换供给"，这条冲突**在此登记**，等路由策略层落地时一并改。

## 5. `size` 的语义（三义分型 + 换算）

- 领域新增 `SizeSpec`：`Pixels { width, height }` / `Ratio { ratio }` / `Tier { tier }`，合同声明该模型接受哪一种（或哪几种）。
- **尺寸档案随发布走，不编进领域**：`SizeProfile`（比例 × 档位 → 像素）作为**发布数据**（合同或映射的一部分）携带，取自厂商官方映射表（`out-reference/doubao/图片生成模型API调用指南.md` 里 lite/pro/4.x 各一张表；Google、OpenAI 各自一份）。领域只留 `SizeSpec` 类型与换算算法——否则接新厂商就要改代码，违背 `docs/design/0004` §2 R3。
- 映射声明该供给要哪种形态；换算在映射层做（`2:3`+`2K` → `1664x2496`；`1024x1024` → `1:1`+`1K`）。换算不出（档案缺该组合）→ 该候选不合格。

## 6. Driver 的收敛

`AdapterDescriptor` 的 `supported_top_level_parameters` 语义改为 **`wire_parameters`：这个 Driver 能写上线文的字段名**（传输能力），不再承担"客户端参数面"。因此：

- AIHubMix 一个 Driver（OpenAI 兼容面：JSON + multipart）；
- APIMart 一个 Driver（任务式：提交 → 轮询 → 结果信封）即可承载它下面**所有**厂商模型（gemini 与 seedream 都是任务式），字段差异由合同 + 映射承担。

## 7. 切片与验收

| 切片 | 内容 | 验收 |
| --- | --- | --- |
| **S1a** 数据模型 + 发布期校验 | 合同/承载面两列、唯一键替换、合同行不可变、冻结纳入 `PublishedOffering`、R1–R3 | **离线可测**：三家各发一份素材；承载面 ⊆ 合同 ⊆ Driver 能写上线文的名字；同一模型只落一行；重发不 `DO UPDATE` |
| **S1b** 受理期校验 + 选路 + 显式默认 | R4/R5、无可用供给的报码、`watermark=false` 这类显式默认 | **离线可测**：按合同提交、合同外字段丢弃、承载不了 → 候选不合格；全部不合格 → 平台侧故障码（不是 400 参数错）；不加水印时不依赖上游默认 |
| **S2** 尺寸语义 | `SizeSpec` + 发布携带的尺寸档案 + 换算 | **离线可测**：`2:3`+`2K` → `1664x2496`（lite 档案）；`1024x1024` → `1:1`+`1K`；档案缺组合 → 候选不合格 |
| **S3** 映射声明化 | `parameter_mapping`：改名、枚举映射 | 渠道字段名与合同字段名不同也能跑通 |
| **S4** 对客目录 | 合同可查询（**必做**，形状待定） | 客户端能拿到"这个模型公开的参数能力" |
| **S5** 素材迁到新形状 | 一个 Vendor Model 一份**顶层合同** + 多条 offering 各带承载面与映射；合同每一项**逐项标出处**（厂商一手 / 厂商增量 / 渠道端点 schema / 平台判断） | **离线可测**：同一模型只有一份合同；两条供给（AIHubMix / APIMart）按承载面正确落选与改道；`rename` 让线上字段名与合同名不同也能跑通；出处清单齐全、没有冒充厂商确认的项 |

S1a/S1b/S2 的验收全部**离线**（进程内假上游 + 直接查库），不产生计费调用；只有真机联调才需要授权与预算。

## 8. 已定的裁定（原未决项）

1. **S4 对客目录**：**必做**，形状定为 `GET /v1/models`（同一 API Key 鉴权）→ `{data:[{name, vendor, revision, contract}]}`；`contract` 就是发布的那份 JSON Schema，客户端据此建表单。
2. **路由策略层**：**暂不引入**。多条活动供给按 `ADR-0009` 的优先级选路维持不变；交接文档那条"选择归运营策略"记为被 `ADR-0009` 取代——等真的出现多个互相竞争的供给、需要按成本或健康度择优时再谈。
3. **`defaults` 的发布期校验**：**加**（**R6**）：`parameter_mapping.defaults` 的每个键必须被该供给的承载面声明，否则发布被拒——声明了却不生效就是"声明了却发不出去"，与 R1–R3 同一条道理。随 S3 落地。
4. **合同外的图片字段**：**400 `invalid_parameter`**，不丢弃（见 §4 的例外说明）。随 S3 把码定死并补测试。

（"合同外字段丢弃"已由 `ADR-0018` 的 2026-09-20 修订定下，要改先改那条 ADR；"合同是模型级唯一一份"已由 `ADR-0015` 定下，`native_revision` 由发布命令携带即可。）
