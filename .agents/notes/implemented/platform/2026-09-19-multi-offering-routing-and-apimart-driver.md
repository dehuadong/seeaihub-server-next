---
title: 第二阶段交付：多 Offering 路由与 APIMart Driver
status: implemented
created: 2026-09-19
updated: 2026-09-20
approval: seeaihub-server-next#2 记录的用户执行授权（用户明确输入「执行实现」）。**2026-09-20 核对补充：#2 上找不到可复核的授权记录（38 条评论全部出自同一账号，无授权语句），该批准依据待用户事后确认，不由本记录单方面推定**；本次 2026-09-20 的收口改动由用户"按复审结果执行、之前的决策可以推翻"的指令授权（工作项 [#6](https://github.com/dehuadong/seeaihub-server-next/issues/6)）
verification: 2026-09-19 fmt、clippy（warnings 作为错误）、workspace 单元测试、九个空库端到端合同测试与 decisions check 全部通过（**原记录如此，保留不改**）。**2026-09-20 核对到的差异**：HEAD 处 `apps/api/tests/http_contract.rs` 实际有 **10** 个 `#[ignore]` 用例——第 10 个（`post_acceptance_failure_keeps_the_task_id_for_reconciliation`）由 `3f2129e` 加入，即本记录最终修订的那次提交。**本记录未补跑十个用例，因此不声称"十个全部通过"**；该差异属记录过时，已写入下方更正节第 1 条。
---

# 第二阶段交付：多 Offering 路由与 APIMart Driver

## 实际交付

[第二工作项](https://github.com/dehuadong/seeaihub-server-next/issues/2) 的第二阶段实现已完成并验证：同一 Vendor Model 现在可以由**多个 Provider 同时供应**，平台按已发布的优先级选中第一个合格候选；新增 APIMart 的任务式 Driver。

交付内容：

- **多 Offering 路由**：`publication.runtime_entries` 增加 `routing_priority`，唯一索引由「每型号一个 active 条目」改为「每型号每个优先级一个」；`active_offering` 返回**候选集合**（每个候选自带它自己的 `capability_schema`）；新增 `generation.routing_decisions` 记录受理时的判定。
- **发布接口形状**：`PublishRuntimeCommand` 支持 `offerings` 数组与 `price_plan`（三例确定性形状判别）；数据库端口只接受已核验的 `PublishRuntimeRequest`。
- **APIMart Driver**：任务式（提交 → 轮询 → 取图 → 证据提取 → 错误分类）；错误分类只依据 `error.code`，未知状态继续轮询，查询阶段错误一律进对账。
- **参考图/遮罩路径（上传）**：APIMart 只接受公网 URL 且不再接受 base64，因此 Driver 在**提交生成任务之前**先调 `POST /v1/uploads/images` 换取 URL，再把 URL 回填到原生参数；上传失败＝**可证明未受理**（`SafeBeforeAcceptance`，`docs/adr/0011`）⇒ Job `failed` + 释放预授权，**不进对账**。**2026-09-19 经用户批准做了真实受控验证**（1 次直连 + 1 次走我们自己的 API+Worker，两次生成合计不足 $0.03），据此两条分支已开放（`allowed_branches` 加上 `image_conditioned`/`masked`，`max_images: 16`）。证据见 `docs/facts/channel-facts.md` §3.7 与 `docs/verification/paid-provider-calls.md` §6。
- **资产绑定路径不再写死字段名**：`AssetBinding.native_parameter_path` 现在真正是**厂商原生参数路径**（APIMart 的 `/image_urls/0`、`/mask_url`），不再硬编码 `image`/`images`/`mask`——这正是 `docs/adr/0002` 要求的"由厂商自己的 Schema 声明原生字段路径"。平台只在**一处**判定"这个参数装的是参考图还是遮罩"（名字以 `image` 开头 / 含 `mask`，其余一律拒绝），发布期校验与运行期用的是同一个函数；Driver 侧同样按路径回填，不自己决定键名。
- **渠道事实**：AIHubMix 2.5 两款与 APIMart 2.5 两款的发布素材；`docs/facts/channel-facts.md` 为渠道事实的单一出处。

技术设计与边界由 [工作项 #2 的规划正文](https://github.com/dehuadong/seeaihub-server-next/issues/2) 与 [分层架构](../../../../docs/design/0004-layered-architecture.md) 拥有，持久决定由 [ADR 目录](../../../../docs/adr/) 拥有（本阶段新增 0009–0011；2026-09-20 收口新增 [`0015`](../../../../docs/adr/0015-vendor-model-contract-and-offering-parameter-mapping.md)；原 0012–0014 已于 2026-09-20 按 ADR 准入门槛退役，见下方更正节第 10 条）；本记录不复制其正文。

## 验证结果

规划验收条件 25 条：**22 条通过**，第 16–18 条标为**过期条件**（要求的能力在代码中不存在，且其前提已被实测结清）。

经真实空库 + 真实进程验证的关键行为：

| 行为 | 证据 |
| --- | --- |
| 同型号多供给、按优先级选第一个 | `multiple_active_offerings_route_by_priority` |
| APIMart 驱动整条流程，创建请求只发一次 | `apimart_driver_executes_task_flow_against_local_upstream`（进程内假上游 + 真实 Worker） |
| 查询瞬时失败会重试 | `transient_query_failure_is_retried_and_the_job_still_succeeds` |
| 未文档化的状态继续轮询 | `unknown_task_status_keeps_polling_instead_of_failing` |
| 同一份 2.5 素材可发布、候选各自携带 Profile | `stage_two_bootstrap_material_publishes_with_per_candidate_profiles` |
| 参考图先上传、生成请求带公网 URL 且不泄露 `asset://`，且字段全在 Profile 声明内 | `apimart_driver_uploads_reference_images_before_submitting` |
| 参考图 + 遮罩各上传一次、各就各位 | `apimart_driver_uploads_reference_image_and_mask_together` |
| 上传失败 → Job 失败 + 释放 hold（不进对账） | `upload_failure_fails_the_job_instead_of_asking_for_reconciliation` |
| **真实上游**：参考图 + 遮罩 → Job `succeeded`、结果图归档、按真实 usage 结算 | `docs/verification/paid-provider-calls.md` §6（2026-09-19 受控实测，非自动化用例） |
| 两类对账的错误码可分 | `worker_sends_delivery_failure_to_reconciliation_with_its_own_code` |

## 交付过程中发现并修正的实质问题

1. **「限制只能收窄」的校验此前不存在**（最重要）。规划与 `ADR-0009` 都要求发布期校验「Offering 的 `restrictions` 不超出该候选 Profile 自己声明的范围」，而原实现只检查了 Adapter 的能力面。已补 `validate_restrictions_within_profile`，并加正反例测试。
2. **`attempts.provider_trace_id` 在成功路径从不写入**：任务式上游的 `task_id` 被直接丢弃，人工对账失去线索。已打通「Adapter → `ProviderSuccess` → `CompleteJob` → UPDATE」。
3. **未发布型号的返回码回归**：无 active 候选时曾返回 `Validation`（400），应为 `NotFound`（404）。
4. **把做不到的能力声明成支持的**：APIMart 声明支持参考图/遮罩，但上游要求公网可访问 URL，而本仓库没有上传链路 —— 当时先收窄为仅文生图、运行时显式拒绝；**随后按用户指定补上了上传链路**。补完之后仍未立刻放开这两条分支：`docs/adr/0002` 要求"未证实的参数不开启、经真实 wire 验证后再发布新修订"，而 `image_urls` 的取值形态与上传接口的真实行为都还没经验证。**2026-09-19 经用户批准做了真实受控验证后，两条分支已开放**（见"实际交付"与 `docs/verification/paid-provider-calls.md` §6）。
5. **`PricePlanDraft.formula` 从不校验**：未知计价形态曾静默落库。
6. **资产绑定路径写死了字段名**：`image`/`images`/`mask` 被硬编码在领域与用例层，导致"字段名不是 `image` 的厂商"（APIMart 的 `image_urls`）无法绑定任何输入图。已改为按路径取厂商原生参数名，并把"参考图/遮罩"的判定收敛成**一个**有测试的函数（运行期与发布期共用），认不出的参数名直接拒绝而不是静默忽略。
7. **上传失败的分类不实**：`upload_failure` 曾把"生成任务可证明未受理"标成 `NotRetryable`（"确定性拒绝"）。三态里对应的是 `SafeBeforeAcceptance`（`docs/adr/0011`），已改正并补测试；`code`/`message` 仍按 `error.code` 保留。
8. **凭据类失败被误判成"受理状态不确定"**（零费用探测发现）：APIMart 的 401 信封里 `error.code` 是**空字符串**，只有 `type: "apimart_error"`，于是"只依据 `error.code`"这条规则会把一个明确没进到生成的请求送进人工对账。已加一条兜底：**凭据/权限类 HTTP 状态（401/402/403）判为确定性拒绝**，**5xx 仍不看状态码**（`build_request_failed` 会以 500 承载参数错误）。
9. **渠道事实里把 `APIMART_API_KEY` 写成"不存在"**：实际 User 与 Machine 级都有，当时只看了进程环境。已更正——这条错误会让读者以为受控验证根本做不了。
10. **对账时没有 task id 可用**（用户问"task_id 跨调用恢复是什么"时查出来）：APIMart 的 task id **只在成功路径**落库（`CompleteJob`），提交成功后如果轮询/取图失败，这个 id 直接丢掉——于是进对账的 Job **连"该去上游查哪个任务"都没有线索**，人工对账是盲的。已补：Driver 在提交后的每一步失败上都附上 task id（`with_task_id`，不改 code/message/`retry_safety`），并把它挂到 `/api/v1/reconciliation-cases` 的返回里，让人工能看到。**注意这仍不是"跨调用恢复"**——拿 task id 自动去补齐结果属于独立工作项（见"已知限制"）。

第 4 项在实现评审中两次被质疑、两次都按 ADR 收紧了声明；第 1–4、7 项由实现评审（Standards / Spec 双轴）发现，第 5、6、8、9 项为自行核对发现（第 8 项来自零费用路由探测）。

## 真实上游的受控验证（2026-09-19，经用户批准）

用户在本会话明确批准后执行，方案与用量当时就写明（上传 1~2 张 + 最多 2 次生成、`n=1`、`quality=low`、预算上限 $1）。实际：**上传 2 次 + 生成 2 次，合计不足 $0.03**。

1. **先不碰凭证拿事实**：4 次无 Authorization 头的请求，确认三个端点在 `api.apib.ai` 上真实存在，并取到 401 的错误信封（据此修掉上面第 8 条）。
2. **再直连探合同**：上传两张 512×512 测试图（参考图 + 带 alpha 的遮罩），随后一次生成带 `image_urls`（字符串数组）与 `mask_url`，11 秒完成 —— 一次结清四条待验项。
3. **最后走我们自己的服务**：发布真实素材 → 平台接口上传两张图 → 受理 Job（`/image_urls/0` + `/mask_url`）→ 真实 Worker 执行 → **`succeeded`**：结果图 `image/png` 1,486,934 bytes / 1024×1024 归档到自有对象存储，Evidence 记 `input_text=29 / input_image=1024 / output_image=196`，结算 **14217 microusd**，与按已发布单价算出的金额逐位一致；上游自报金额与公开单价的比值正好 **0.8**（账号折扣，与 §3.3 一致）。

敏感信息未入库：**不保存**真实图片 URL 与 task id，原始响应只留本机临时目录。据此，两个 APIMart 发布素材的 `allowed_branches` 已加上 `image_conditioned` / `masked`。

**成本价：各渠道来源不同（2026-09-19，含上游账单面板核对）**

- **APIMart 直接声明金额**：任务响应里的 `cost`（两次实测 `$0.011374` / `$0.011390`，第三笔 `$0.004760`）——**那就是成本价**，不需要按公开费率再算一遍。面板写明它 = `Base × Group ratio 0.8 × Channel ratio 1 × Discount ratio 1`，`credits = cost × 10`。
- **AIHubMix 只给 token**：响应里没有任何金额字段，成本价按上游公开的四档 token 费率自算（文本in $5 / 文本out $10 / 图像in $8 / 图像out $30，每 1M），两次实测各 `$0.005950`。
- 两者都有**四分项 token**，所以平台侧 `TokenUsage` 归一不变；差别只是"上游给不给金额"，留在各自 ② Driver（`0004` R1）。
- 平台侧 capture `14217 microusd` 恰好等于面板的 `Base cost`，说明"费率 × 分项 token"这套算法与上游的 Base 完全一致，差额只在账号倍率上。
- **平台对外价没定，也不该在这阶段定**（加价、是否让利属后期产品决定，工作项 [#5](https://github.com/dehuadong/seeaihub-server-next/issues/5)）；两个素材 `price_plan` 里填的是上游公开费率，角色是**结算基数**，已在文件里用 `_note` 标注。
- **按 Tokens 计费不考虑缓存**：不为缓存加字段、也不作为待办。

逐笔核对表见 `docs/facts/channel-facts.md` §5。

## 本变更对领域模型的影响（`0004` §4 的例外说明）

这次改动**动了领域层**（`AssetBinding` 的路径语义、`branch()` 的判定、新增 `DomainError::UnsupportedAssetParameter`），按 `docs/design/0004` §4 的判据必须逐项说明理由：

- 领域层改动**不是**为了适应 APIMart，恰恰相反：旧代码把 `image`/`images`/`mask` 三个**渠道字段名**写死在领域里，那才是渠道差异污染领域。现在领域只认识"路径的第一段是厂商参数名"这一件事，认不认得某个名字由一个**与具体渠道无关**的函数（`AssetParameterKind::classify`）回答；换一个字段名不同的 Provider，**不需要再改领域**。
- 因此它更接近 §4 的 **E1（平台侧新能力）**：把"资产绑到哪个原生参数"从硬编码升级为平台自己的通用能力，而不是新增某渠道的分支。
- 已知代价：渠道若用不以 `image` 开头、也不含 `mask` 的名字（例如 `reference_images`），平台会**拒绝**该绑定。这是**有意的**——用户 2026-09-19 决定平台内部只认渠道原生参数名，统一参数转换留给**后期对外消费侧**（见下面"未决项"第一条与 `docs/adr/0002` 的补充段）。**⚠️ 2026-09-20 更正：该决定已被 [`docs/adr/0015`](../../../../docs/adr/0015-vendor-model-contract-and-offering-parameter-mapping.md) 取代**——调用方所见参数名归 Vendor Model Contract，渠道包装差异归 Offering Parameter Mapping，"拒绝认不出的名字"只是本阶段的兼容规则。上句仅保留为当时的理由。

## 已知限制与未决项（不在本次交付范围）

- APIMart 的**图生图/遮罩分支已开放并已受控实测**（2026-09-19，见上）；仍未测的是 `sunburst` 的图生图（与 flare 同渠道族、同端点、同参数面）、`base64` 路径，以及 20MB / 16 张 / 256MB 这些**边界**——代码已按文档上限拒绝（单张 20MB + 单次总量 256MB），但没有逐个压测。
- **"哪个原生参数装图片"目前靠名字约定** —— **已由用户决定（2026-09-19）**：平台内部只认渠道自己的参数名，**不做**统一参数转换；统一转换属**后期对外消费侧**的能力，现在做会牵动每个渠道的适配与验证，所以先把各条渠道跑通。决定记在 `docs/adr/0002` 的补充段（工作项 #4 据此关闭）；名字约定因此是明确的过渡方案，`reference_images` 这类名字由那一层解决。**⚠️ 2026-09-20 更正：该决定已被 [`docs/adr/0015`](../../../../docs/adr/0015-vendor-model-contract-and-offering-parameter-mapping.md) 取代**（0015 把名字约定定性为阶段性兼容规则）；`reference_images` 这类名字改由 Vendor Model Contract 显式声明解决，不再留给"那一层"。此条仅保留为当时的理由，**不再是现行规则**。
- **`task_id` 不用于跨调用恢复**：现在只做到"失败时把 task id 留下、对账的人能查到"（见上第 10 条）。**拿它自动去补齐结果**需要改造 Attempt 模型——`generation.attempts` 有 `UNIQUE (job_id)`（一个 Job 只能一个 Attempt），且状态机只允许 `reconciliation_required → failed`、**不允许 → succeeded**（有测试钉着，理由是不确定是否已产生费用时不能自作主张，见 `docs/adr/0007`/`0011`）。要做属于独立工作项。
- 火山方舟/Seedream、直连 OpenAI **用户 2026-09-19 明确暂不做**（先把两家渠道跑通；该范围由工作项 #2 的规划保持不变）；多图与 `stream`/`tools` 同样不在本阶段；
- 平台**对外价**等运营后台管理设计定了再考虑（工作项 [#5](https://github.com/dehuadong/seeaihub-server-next/issues/5)）；
- 第 16–18 条验收条件已标为过期，其若要恢复需先结清非 USD 计价或金额型证据。

## 交付后发现并更正的事项（2026-09-20 复审收口）

2026-09-19 的复审交接文档指出：交付记录把"实现已完成 / 已受控验证 / 已获批准上线"混在一起，并质疑对外参数合同的归属。据此只读核对后，更正与登记如下（工作项 [`#6`](https://github.com/dehuadong/seeaihub-server-next/issues/6)）：

1. **端到端用例数的差异**：frontmatter 记的"九个"是**当时实际跑过的数目**，不改写；但 HEAD 处实际为 **10** 个 `#[ignore]` 用例，第 10 个由 `3f2129e` 加入（即本记录最后一次修订的那次提交，因此本记录自那时起就已过时）。本记录**未补跑**十个用例，故不声称十个全部通过。
2. **"已开放"不等于"已上线"**：本记录写 APIMart 的图生图/遮罩"两条分支已开放"，指的是**发布素材里的 `allowed_branches`**，不是产品上线。素材的 `_status` 仍是"草案 · 未发布"；用户 2026-09-20 明确**本阶段是阶段性任务，不存在上线批准**。
3. **调用方可见形状取自未被调用的端点族（最实质的一条）**：AIHubMix 素材把 `quality`/`background`/`output_compression`/`user` 声明在 `extra` 内——那是该渠道自有异步 API `/ai/v1/*` 的位置；本阶段实际调用的是 OpenAI 兼容的 `/v1/images/*`，该端点族 `quality` 在**顶层**且**不存在 `extra`**（`docs/facts/channel-facts.md` §2.5）。该形状由 `crates/adapter-aihubmix` 的能力面（把 `extra` 列为顶层参数名）与 `crates/application` 的发布校验强制，Adapter 再在出网时把 `extra.quality` 摊平回顶层，于是形成"发布校验认一套形状、上行发另一套"的翻译层。归属与目标形态见 [`docs/adr/0015`](../../../../docs/adr/0015-vendor-model-contract-and-offering-parameter-mapping.md)，修复登记为 #6 的差距 G1。
4. **同一模型的两个候选对调用方的字段形状不同**：AIHubMix 素材要求 `extra.quality`、APIMart 素材要求顶层 `quality`，两份 Schema 都是 `additionalProperties: false`。结果是**调用方把参数写在哪，直接决定哪个候选合格**，`routing_priority` 不起决定作用。这是复审"兼容性事实与路由选择必须分开"的实际形态。
5. **逐个发布素材会静默替换候选**：发布语义是"该模型全部 active 条目原子替换"，`routing_priority` 由候选数组下标决定。现成素材是每渠道一个文件、各含一个候选；**按文件逐个发布会让该模型只剩最后一个候选**。要表达"同一模型两个 Provider"必须把候选合并成一次发布（`apps/api/tests/http_contract.rs` 的用例即合并写法）。登记为 #6 的差距 G4。
6. **三个"已声明未验证"参数**：素材声明了 `background`、`output_compression`、`user`，但实测的 `/v1/images/generations` 顶层参数集不含这三项且 `additionalProperties: false`，与 `docs/adr/0002`"未证实的参数不开启"不符。登记为 #6 的差距 G2（验证要花钱，须单独批准）。
7. **路由规则是硬编码的**：运营方能配置的是**顺序**，不是**规则**。已记入 [`docs/adr/0009`](../../../../docs/adr/0009-multiple-active-offerings-and-routing.md) 的"待迁移的差距"节与 #6 的差距 G3。
8. **`docs/adr/0002` 的补充决定被取代**：原"平台内部只认渠道自己的参数名、统一转换留给对外消费侧"已由 [`docs/adr/0015`](../../../../docs/adr/0015-vendor-model-contract-and-offering-parameter-mapping.md) 取代；Vendor Model Contract 与 Offering Parameter Mapping 已进入 `CONTEXT.md`。
9. **授权证据不可复核**：`#2` 的 38 条评论全部出自同一账号、无授权语句，见 frontmatter 的 approval 说明；不作为后续任务的持续授权。

本次收口**未改动实现代码、未发起任何真实渠道调用**。

10. **ADR 集合按准入门槛整改（2026-09-20，用户指示）**：原 0012–0014 三篇**不是持久决定**，已移出 `docs/adr/`（编号不复用，历史全文见 git 历史）。判据与去向：
    - **0012「Provider 声明的扣费金额能否作为计量证据」**——被实测回答掉的**候选问题**，不是决定 → 否决理由落成 `.agents/notes/rejected/domain/2026-09-19-provider-declared-charge-as-metering-evidence.md`，渠道侧事实留在 `docs/facts/channel-facts.md` §3.3/§5；
    - **0013「退役 `gpt-image-2`，改用两款 2.5」**——**目录运营状态**（改数据就能回退）→ 在售型号由 `config/bootstrap/*.json` 的发布声明表达；其中"`gpt-image-2-official` 与 `gpt-image-2` 视为同一 Vendor Model"是**运营方的显式配置判断**，其规则属 [`docs/adr/0004`](../../../../docs/adr/0004-vendor-and-provider-identities-stay-separate.md)（Vendor 与 Provider 身份不合并），不在 ADR 里记录具体型号；
    - **0014「第二阶段的 Provider 集合」**——**规划范围**（属 Proposal）→ 归工作项 `#2` 的规划正文。
    
    准入门槛已写入 [`docs/agents/artifacts.md`](../../../../docs/agents/artifacts.md)：ADR 只收"难反悔 + 不看记录会奇怪 + 真权衡过"三条都成立的决定；目录状态、价格、阶段范围、被实测回答掉的问题、当前实现状态一律走发布物 / `docs/facts/` / 工作项 / Agent Notes。
