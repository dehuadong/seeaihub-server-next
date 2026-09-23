主题: 独立图片生成服务端技术设计（v5 为准）
当前修订: v5
状态: 已评审通过（2026-09-19，seeaihub-server-next#1 Plan Review PASS）
来源: seeaihub#674 技术设计 v5（2026-09-18T15:52:42Z，comment 5732545152），并合并在本修订中继续有效的 v3、v4 规则

# 独立图片生成服务端技术设计

本文是本仓库图片生成技术设计的权威副本，对应历史提案 [seeaihub#674](https://github.com/dehuadong/seeaihub/issues/674) 的技术设计 v5。上游原 issue 与评论保持不动，只作为冻结的历史来源；本仓库后续提案与进度由 [seeaihub-server-next#1](https://github.com/dehuadong/seeaihub-server-next/issues/1) 及其后续工作项拥有，技术设计以本文为准。

修订关系：v5 基于 2026-09-18 的真实付费验证，取代 v4 的「生产路径优先使用 `/ai/v1` 统一异步接口」结论；v4 的 Provider/Vendor 身份、Native Schema 发布与安全重试规则继续有效并已并入本文（其中的**资产归档**已于 2026-09-20 作废，见下方更正）；v3 的统一 Command/Job 与「文生图、图生图同阶段」结论同样继续有效。v1、v2、v3、v4 的逐版全文仍保留在上游 issue 评论中，作为修订历史，不复制到本仓库。

**2026-09-20 就地更正（非修订级变更）**：本文原写的"输入侧先落成 Asset 并校验 MIME/魔数/尺寸/摘要""结果必须先归档到自有对象存储才算成功""对客有 202 受理与 job_id 轮询"三条**已作废**——图片按渠道原形进原形出，平台不落盘静态资产，对客只有同步形态。依据 [`docs/adr/0019`](../adr/0019-images-pass-through-without-asset-storage.md)，取代 [`docs/adr/0008`](../adr/0008-own-object-storage-is-the-platform-result.md)（已退役）。下文涉及之处已就地改写；Job/Attempt/计量证据/对账/结算不受影响，仍是内部执行与审计单位。

配套工件：实现映射见 [0001-image-generation.md](./0001-image-generation.md)；从本设计抽取的持久决策见 `docs/adr/`。

## 1. 实测结论

2026-09-18 用真实付费调用结清：**AIHubMix 的正式执行路径采用它同步的两个端点**——`/v1/images/generations`（文生图）与 `/v1/images/edits`（图生图 / mask）。它们返回完整的分项 token，能形成可核验的 Metering Evidence。

逐端点的形态、分支、计量事实与实测 token 数，以 [`docs/facts/channel-facts.md`](../facts/channel-facts.md) 的 AIHubMix 节为唯一出处，本文不复述。没有保存密钥、task ID 或短期 URL。

## 2. 身份与供给登记

| 对象 | 首期值 | 说明 |
| --- | --- | --- |
| Vendor | `OpenAI` | 模型原始厂商 |
| Vendor Model | `gpt-image-2` | 保留厂商/渠道公开的原生模型 ID |
| Provider Kind | `AIHubMix` | 实际提供调用、任务和结果下载的渠道 |
| Adapter | `aihubmix-image-v1` | 随代码发布的 wire 与生命周期能力 |
| Offering | AIHubMix → OpenAI `gpt-image-2` | 绑定 Provider、Adapter、模型修订、限制和价格 |
| Provider Model ID | `gpt-image-2` | 本供给不需要改写模型名 |

Provider 与 Vendor 不合并：以后其他 Provider 也供应 `gpt-image-2` 时，只新增 Offering/Channel/Price Plan，不复制 Vendor Model。平台 `native_model_id` 与调用上游所需的 `provider_model_id` 分开固化，Adapter 只使用后者组装供应商请求。决策依据见 `docs/adr/0004-vendor-and-provider-identities-stay-separate.md`。

## 3. 统一应用命令与生命周期

文生图、图生图以及厂商支持时的 mask 编辑是同一个图片生成业务能力的不同输入分支，共用一个 Command、Job、Attempt、Evidence 和结算流程。决策依据见 `docs/adr/0001-unified-image-generation-command.md`。

```text
CreateImageGenerationRequest {        // 调用方看到的形状（对客接口）
  model,                             // 对外的模型字段＝平台型号名（运营发布时的型号标识）
  <合同里的模型参数，扁平放顶层>,        // prompt / n / size / quality / …
  image | image_urls,                // 参考图（同义、二选一）：公网 URL 或 data:image/…;base64,…
  mask,                              // 可选；PNG data URL
  // 幂等键走 `Idempotency-Key` 请求头；预授权额由服务端定，调用方不报
}

CreateImageGeneration {               // 落库与 Worker 看到的形状（已落到某个候选的装载面）
  gateway_model,                     // 库里这一列装的也是平台型号名；三方命名的收口另做
  native_parameters,                  // 同上，但图片已按该候选声明映射成具体参数名
  idempotency_key,
  max_cost_microusd,                  // 服务端按固定数给的预授权额
}
```

两者的换算就是 Offering Parameter Mapping 的起点：调用方只给 `image` / `mask`，平台按选中候选声明的参数面决定装到 `/image`、`/image_urls/0` 还是 `/mask_url`；候选表达不了就是不合格，选路据此判定。图片**只是参数值**——平台不持有字节、不给它独立身份，因此没有"资产绑定"这一层（`docs/adr/0019`）。

请求分支判定（发布期/请求期派生结果，不是客户端字段）：

| 条件 | 内部判定 | 约束 |
| --- | --- | --- |
| 无 `image`、无 `mask` | prompt-only | 使用厂商原生文生图参数合同 |
| 有 `image`、无 `mask` | image-conditioned | 图**只是参数值**（公网 URL 或 data URL）：平台不校验内容、MIME、字节或数量，交给渠道判 |
| 有 `image`、有 `mask` | masked | 仅在候选声明支持时启用；mask 同样是参数值，尺寸/通道由渠道校验 |
| 无 `image`、有 `mask` | 非法 | 调用上游前失败 |

Job 状态机（所有分支共用）：

```text
accepted → leased → submitting → submitted/running → succeeded
                                      ├→ failed
                                      └→ reconciliation_required
```

Job 固化：Vendor Model Revision、派生分支、Offering、Adapter、Channel、Published Revision、Native Parameters 摘要与 Price Snapshot。发布或改价后，已受理 Job 不重新解释输入。事实权威见 `docs/adr/0003-postgresql-is-source-of-truth.md`。

HTTP 只是应用命令的适配层，对客**只有两条路径、同一个能力**：`/v1/images/generations`（JSON）与 `/v1/images/edits`（`multipart/form-data`，`image`/`mask` 是文件部件）。**分支只看请求里有没有参考图/遮罩**，**不按端点断言**——带图的 generations 与不带图的 edits 都合法。两条都走同一个受理路径（`CreateImageGenerationRequest`），只做请求解码，不能自己选路、计费或调用 Provider。**形态是同步的**（2026-09-20 定）：受理后等 Job 到终态，成功回 `{created, data:[{url|b64_json}]}`——渠道给哪种形态就回哪种；失败回错误信封。没有 202 受理、没有 job_id 轮询：Job 是**内部执行/审计记录**，不投射成对客协议。multipart 上 `image`/`mask` 既可以是文件部件（字节只在内存里转成 data URL 语义），也可以是文本部件（值按 URL/data URL 读）——两者同一套语义，但同一个字段不能既当文件又当文本。

## 4. 执行路径与接口职责

首期正式计费 Offering 的执行路径是 `/v1`，Adapter 按原生图片参数分流：

```text
平台调用方
  → CreateImageGeneration
  → 持久 Generation Job
  → AIHubMix Adapter
       ├─ 无 image/images → POST /v1/images/generations
       └─ 有 image/images → POST /v1/images/edits
                              └─ mask 可选
  → 上游给的 url / b64_json 原样成为结果信封
  → usage → MeteringEvidence
  → Price Snapshot 结算
```

结论：

- 平台应用层仍只有一个 Command、一个 Job 和一个结算流程；
- 文生图/图生图判定仍来自原生图片参数，不新增平台 `operation` 字段；
- `generations`/`edits` 的 endpoint 与 JSON/multipart 差异只存在于 Adapter；
- Job 的推进仍是后台的（Worker 领活、租约与心跳），但**对客没有异步形态**：调用方在一次请求里等结果；
- 以后是否增加同步等待型公开 API，不影响这个执行模型。

这正好落实「`image.generations.sync.v1` 与 task 协议不应成为领域拆分，生命周期差异由 Adapter 处理」的方向。决策依据见 `docs/adr/0006-no-settlement-without-metering-evidence.md`。

**关于该渠道的异步任务面**：它**不使用**——任务对象没有可核验的计量事实，无法形成精确的最终 Evidence。若将来它能给出可核验的计量，按新的 Offering 修订发布即可，不需要改应用层或公开协议。各端点的实际形态见 [`docs/facts/channel-facts.md`](../facts/channel-facts.md) 的 AIHubMix 节。

## 5. 原生能力 Schema 与发布

以 AIHubMix 无需鉴权的机器 Schema 为上游证据。导入后形成平台自己的不可变 `NativeImageCapabilitySchema` 修订，至少保存：来源 URL、抓取时间、内容摘要、上游 schema 版本和人工审核记录；必填 `model`、`prompt`；`image` 与 `images` 的同义/归并关系（`images` 最多 16 张）；`mask` 必须与 `image` 或非空 `images` 同时存在；`n` 为 1–10、默认 1；`output_format` 为 `png`/`jpeg`、默认 `png`；`size` 的原生值与图生图分支的受限集合；`quality` 可选参数（枚举与默认值按该端点 schema）（**字段与取值一律以该 Offering 实际调用的端点的 `request.schema` 为定，见下**）；`async`、`webhook_url`、`webhook_events_filter` 的原生条件（属 `/ai/v1` 那族，本平台不采用）；未声明字段由平台丢弃。

**未声明字段由平台在受理前处置**（2026-09-19 更正 + 2026-09-20 用户定处置）：本句原先依赖「上游 `additionalProperties: false`」这一对该上游的观察。第二个 Provider 的实测表明上游可能**静默接受未声明字段并降级为默认值**，因此这层处置必须由平台在受理前执行，不能外包给上游。**2026-09-20 定为丢弃**：按选中候选声明的参数面过滤，没声明的直接丢掉（不报错、也不发上游），见 `docs/adr/0018-open-parameters-by-first-party-docs.md` 的同日修订。

运行时请求不实时依赖 AIHubMix Schema 地址。更新流程是「抓取候选 → 差异检查 → 审核 → 发布新 Runtime Revision」，旧 Job 继续使用受理时固定的旧修订。

**形状必须绑定该 Offering 实际调用的端点（2026-09-20 收口更正）**：上一段的四项可选参数清单取自 AIHubMix 的**机器 Schema**，而那份 Schema 覆盖的端点不止一个，各端点的参数位置与可用面并不相同。声明"原生能力 Schema"时，字段与位置一律取自**该 Offering 实际调用的端点（含各分支）**——规则本身属 [`docs/adr/0015`](../adr/0015-vendor-model-contract-and-offering-parameter-mapping.md)，本节不复制；各端点的实际形态属渠道事实，见 [`docs/facts/channel-facts.md`](../facts/channel-facts.md) 的 AIHubMix 节。

- 上一段清单里的 `background`、`output_compression`、`user`、`moderation` **不在**本平台采用的 `/v1` 端点族的参数集合内（它们属 `/ai/v1` 那族的 `extra`），因此素材**不声明**它们（2026-09-20 定；此前按文档声明过一轮，已按端点 `request.schema` 收回）；
- 已知差距（当前素材把可选参数声明在本 Offering 未采用的那一族端点的形状下，Adapter 能力面又强制该形状）登记在工作项 [`#6`](https://github.com/dehuadong/seeaihub-server-next/issues/6) 的 G1。

已知文档冲突与发布规则：实时 Schema 与模型介绍/旧资料存在差异——实时 Schema 不含 `input_fidelity`、`moderation`、`response_format`，`quality` 不接受 `auto`，`output_format` 不接受 `webp`，`mask` 的类型声明自身也有矛盾。首版能力发布按实时 Schema 的保守交集处理：未声明字段丢弃、上述未证实参数不开启、`mask` 先只接受 string。每个冲突参数经真实 wire 验证后，再以新 Schema 修订发布，不能在原修订上静默放宽。「未声明字段丢弃」是**平台自己的**受理前处置：上游不保证拒绝未声明字段（2026-09-19 实测另一 Provider 静默接受并降级为默认值），因此不能依赖上游返回错误来兜底。决策依据见 `docs/adr/0002-native-capability-schema-not-canonical.md` 与 `docs/adr/0018-open-parameters-by-first-party-docs.md`。

## 6. 计量证据与结算

首期 `AihubmixImageTokenUsage` 至少包含：

```text
input_text_tokens
input_image_tokens
output_text_tokens
output_image_tokens
total_tokens
provider_response_digest
attempt_id
```

Adapter 从 `/v1` 成功响应提取这些字段；结算模块只读取强类型 Evidence 和 Job 固化的 Price Snapshot。价格仍是运行时配置，当前公开候选为文本输入 `$5/M`、图片输入 `$8/M`、图片输出 `$30/M`。

响应字段与总量必须满足内部一致性校验；缺字段、负数、总量不一致或响应解析失败时，不得猜测费用，进入对账。结果**不再解码归档**：上游给什么形态就存什么形态（`url` 或 `b64_json`），随后把 Job 标为成功。结算门槛见 `docs/adr/0006-no-settlement-without-metering-evidence.md`。

## 7. 同步 Provider 调用的可靠性边界

AIHubMix `/v1` 没有公开幂等键，成功调用也不进入可查询任务列表，因此从请求发出到完整响应落库之间存在「上游可能已生成、平台却没有结果」的不确定窗口。首期明确接受这一 Provider 限制，并采用失败关闭策略：

- 本地幂等键和数据库唯一约束保证一个 Job 只建立一个 Attempt；
- Worker 提交前写入 `submitting` 并持续维护租约，通用重试器不得重放 Provider POST；
- 明确的连接前失败可以重试；请求可能已发出、响应超时、连接中断、进程崩溃或解析失败，全部进入 `reconciliation_required`；
- 不自动切换 Channel/Offering，不重新生成；
- 人工通过 AIHubMix 账单/支持渠道核查，不能恢复结果时按运营规则退款或释放预授权；
- 监控该类事件率。如果实际故障率不可接受，再评估 Provider 账单 API、幂等能力或 `/ai/v1` usage，而不是削弱安全规则。

该选择优先保证不重复出图和不重复产生上游成本，代价是极少数模糊失败不能自动恢复结果。Reconciliation Case 保留预授权，当前只允许管理员以幂等业务键退款并释放全部预授权；没有可核验 Metering Evidence 时不能人工确认扣款。以后若要根据上游账单确认扣款，必须另立设计，先定义可核验证据合同。每次处置写入不可变账本和审计。决策依据见 `docs/adr/0006-no-settlement-without-metering-evidence.md` 与 `docs/adr/0007-reconciliation-instead-of-automatic-retry.md`。

## 8. 输入与输出图片

- 参考图/遮罩就是普通参数值：公网 URL 原样交给上游，data URL 就地解码后按上游要的形态发出去（AIHubMix 的编辑端点要文件部件，所以要字节）；
- `image`/`images`/`mask` 仍保留 AIHubMix 原生字段语义，平台只把它们放到这些字段上；
- AIHubMix 输出 URL 约 30 分钟失效，且下载可能需同一 Bearer。平台**不代取**：渠道给 `b64_json` 就回 base64、给 `url` 就回 URL，长期保存由调用方自己负责（决策见 `docs/adr/0019-images-pass-through-without-asset-storage.md`）；
- 上游 URL 和 Bearer 不返回给平台调用方，不作为永久结果；
- 当前正式 `/v1` 路径没有可查询 task id；响应解析失败导致是否已生成、是否已计费不确定时，进入 `reconciliation_required`，不得自动重新提交。只有将来发布带可查询任务标识的新执行策略时，才允许在同一 Attempt 内恢复取结果。

图片就是**候选声明的那个参数的值**（`/image`、`/image_urls/0`、`/mask_url`）；平台不持有字节、不给它独立身份，因此没有资产表，也没有资产引用。决策依据见 `docs/adr/0019-images-pass-through-without-asset-storage.md`。

## 9. Adapter 首期能力

- `submit_generate`：JSON `/v1/images/generations`；
- `submit_edit`：multipart `/v1/images/edits`，支持单图与 mask；
- `parse_result`：Base64 图片、输出元数据与 usage；
- `extract_evidence`：四类 token 与总量；
- `classify_error`：明确未受理、明确失败、受理状态不确定；
- 首期不声明 Provider cancel/poll 能力；
- 多图虽然存在于 `/ai/v1` Schema，但 `/v1/images/edits` 的机器 Schema 只声明单个二进制 `image`，因此首期正式 Offering 先发布单图 + 可选 mask；多图待真实 `/v1` 合同或可计量 `/ai/v1` 合同成立后再发布。

错误分类首期规则：`400` 归为请求永久错误；`401/403` 归为凭证、权限、余额或异步开通问题；`429/503` 只有在明确未受理时才自动退避重试；`task_status_unavailable`、`upstream_bad_response`、`upstream_unreachable`、`result_delivery_failed` 均不能仅凭错误码断言「未生成」。这些规则只决定**平台内部怎么处置**；消费者看到的错误语义由 `docs/adr/0017-provider-errors-are-rewritten-for-consumers.md` 拥有——渠道报的余额、凭证、限流与参数问题都是平台侧故障，不外泄给消费者。

## 10. Base URL

首期 AIHubMix Channel 默认 Base URL 为 `https://api.inferera.com`，保存时去除尾部 `/`；Adapter 分别拼接 `/v1/images/generations`、`/v1/images/edits` 或 `/ai/v1/...`。Base URL 是 Channel 运行时配置，不写死在 Adapter。

2026-09-18 已用同一账户完成无费用只读验证：Inferera 域名的 `/ai/v1/images` 和模型 Schema 均返回 HTTP 200，且可读取此前的 `gpt-image-2` 任务与三条预期 endpoint。随后实施期受控集成测试已从该域名完成 generations、单图 edits 与 mask edits 三个分支，均取得可核验 token usage；本轮合同收口不重复执行付费 POST。

## 11. 价格与目录的动态性

价格是运行时 `Price Plan` 数据，不写进 Adapter 代码；保存币种、计价单位、各计量项单价、来源、抓取/审核时间和生效区间。后续价格变化只发布新 Price Plan/Runtime Revision，不需要编译或重启数据面；已受理 Job 继续使用原 Price Snapshot。目录与价格一并见 `docs/adr/0003-postgresql-is-source-of-truth.md`。

## 12. 验收条件

- 无图和有图请求进入同一 Command/Job，Adapter 分别调用 generations/edits；
- 两条成功响应均能生成强类型 token Evidence，并按相同 Price Snapshot 机制结算；
- Worker 可以安全承载长时间同步调用，客户端连接不等待 Provider；
- 进程在提交中断开后 Job 进入 reconciliation，不发生第二次 Provider POST；
- 结果信封（`url` 或 `b64_json`）写回 Job 后才完成 Job；平台不归档字节，`b64_json` 会原样出现在对客响应里；
- 首期 Offering 拒绝未经发布的多图和冲突参数；
- 新模型、Schema、Offering 和 Price Plan 仍通过 Runtime Revision 动态发布，不重编译数据面；
- 没有可核验 Metering Evidence 时禁止正式结算发布。

## 13. 第一方资料索引

- [AIHubMix 异步任务文档](https://docs.aihubmix.com/en/api/async-tasks.md)：`/ai/v1` 任务创建、状态、轮询、任务列表、结果下载、Webhook、错误与恢复语义的权威来源。
- [GPT Image 2 机器 Schema](https://aihubmix.com/call/schema/models/gpt-image-2/endpoints)：端点、请求字段、条件约束与同步/异步能力的权威来源。
- [GPT Image 2 模型说明](https://aihubmix.com/model/gpt-image-2/llms.txt)
- [AIHubMix 错误码](https://docs.aihubmix.com/en/FAQs/HTTP-Codes.md)

资料更新时先形成候选快照并做差异审查，不能让运行中服务直接跟随远端文档变化。

## 评审历史

技术设计 v3 结论为 REQUIRED REVISION（唯一剩余产品选择是首个 Provider/Adapter）；v4 结论为 REQUIRED REVISION（真实响应/账单中的 Metering Evidence 字段尚未由官方资料证明，资金闭环不能据此定稿）；v5 基于真实付费验证补齐该证据，Plan Review 在 Standards、Spec、Architecture 三轴均 PASS，结论为 **PASS**。逐版 Plan Review 全文保留在上游 issue 评论中；第一阶段取得执行授权后的实现与交付状态由本仓库工作项和 Agent Notes 记录。

**2026-09-19 事实前提更正（`dehuadong/seeaihub-server-next#2` Planning，非修订级变更）**：本文 §5 与 §11 关于「未声明字段失败关闭依赖上游 `additionalProperties: false`」的理由段落已就地更正——第二个 Provider 实测表明上游可能静默接受未知字段并降级为默认值，因此该防线必须由平台在受理前实施。**结论不变**（仍不做跨厂商参数大一统、仍按不可变 Revision 发布原生能力），正文修订头因此仍为 v5。决策属主见 `docs/adr/0002-native-capability-schema-not-canonical.md`。

**与第二个 Provider 的关系**：本文 §5（原生能力 Schema 的理由）、§6（计量证据）、§9（Adapter 能力与错误分类）**只对 AIHubMix 成立**——它假定 token 计量、Base64 同响应返回、上游拒绝未知字段。第二个 Provider 的供给与计量设计见 [0003-doubao-ark-image-adapter.md](./0003-doubao-ark-image-adapter.md)，不在本文重复。
