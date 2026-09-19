---
title: 第二阶段交付：多 Offering 路由与 APIMart Driver
status: implemented
created: 2026-09-19
updated: 2026-09-19
approval: seeaihub-server-next#2 记录的用户执行授权（用户明确输入「执行实现」）；范围收口与回扩见该工作项上的规划 §7.5
verification: 2026-09-19 fmt、clippy（warnings 作为错误）、workspace 单元测试、九个空库端到端合同测试与 decisions check 全部通过
---

# 第二阶段交付：多 Offering 路由与 APIMart Driver

## 实际交付

[第二工作项](https://github.com/dehuadong/seeaihub-server-next/issues/2) 的第二阶段实现已完成并验证：同一 Vendor Model 现在可以由**多个 Provider 同时供应**，平台按已发布的优先级选中第一个合格候选；新增 APIMart 的任务式 Driver。

交付内容：

- **多 Offering 路由**：`publication.runtime_entries` 增加 `routing_priority`，唯一索引由「每型号一个 active 条目」改为「每型号每个优先级一个」；`active_offering` 返回**候选集合**（每个候选自带它自己的 `capability_schema`）；新增 `generation.routing_decisions` 记录受理时的判定。
- **发布接口形状**：`PublishRuntimeCommand` 支持 `offerings` 数组与 `price_plan`（三例确定性形状判别）；数据库端口只接受已核验的 `PublishRuntimeRequest`。
- **APIMart Driver**：任务式（提交 → 轮询 → 取图 → 证据提取 → 错误分类）；错误分类只依据 `error.code`，未知状态继续轮询，查询阶段错误一律进对账。
- **参考图/遮罩路径（上传）**：APIMart 只接受公网 URL 且不再接受 base64，因此 Driver 在**提交生成任务之前**先调 `POST /v1/uploads/images` 换取 URL，再把 URL 回填到原生参数；上传失败＝**可证明未受理**（`SafeBeforeAcceptance`，`docs/adr/0011`）⇒ Job `failed` + 释放预授权，**不进对账**。**2026-09-19 经用户批准做了真实受控验证**（1 次直连 + 1 次走我们自己的 API+Worker，两次生成合计不足 $0.03），据此两条分支已开放（`allowed_branches` 加上 `image_conditioned`/`masked`，`max_images: 16`）。证据见 `docs/facts/channel-facts.md` §3.7/§5.6。
- **资产绑定路径不再写死字段名**：`AssetBinding.native_parameter_path` 现在真正是**厂商原生参数路径**（APIMart 的 `/image_urls/0`、`/mask_url`），不再硬编码 `image`/`images`/`mask`——这正是 `docs/adr/0002` 要求的"由厂商自己的 Schema 声明原生字段路径"。平台只在**一处**判定"这个参数装的是参考图还是遮罩"（名字以 `image` 开头 / 含 `mask`，其余一律拒绝），发布期校验与运行期用的是同一个函数；Driver 侧同样按路径回填，不自己决定键名。
- **渠道事实**：AIHubMix 2.5 两款与 APIMart 2.5 两款的发布素材；`docs/facts/channel-facts.md` 为渠道事实的单一出处。

技术设计与边界由 [工作项 #2 的规划正文](https://github.com/dehuadong/seeaihub-server-next/issues/2) 与 [分层架构](../../../../docs/design/0004-layered-architecture.md) 拥有，持久决定由 [ADR 目录](../../../../docs/adr/) 拥有（新增 0009–0014）；本记录不复制其正文。

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
| **真实上游**：参考图 + 遮罩 → Job `succeeded`、结果图归档、按真实 usage 结算 | `docs/facts/channel-facts.md` §5.6（2026-09-19 受控实测，非自动化用例） |
| 两类对账的错误码可分 | `worker_sends_delivery_failure_to_reconciliation_with_its_own_code` |

## 交付过程中发现并修正的实质问题

1. **「限制只能收窄」的校验此前不存在**（最重要）。规划与 `ADR-0009` 都要求发布期校验「Offering 的 `restrictions` 不超出该候选 Profile 自己声明的范围」，而原实现只检查了 Adapter 的能力面。已补 `validate_restrictions_within_profile`，并加正反例测试。
2. **`attempts.provider_trace_id` 在成功路径从不写入**：任务式上游的 `task_id` 被直接丢弃，人工对账失去线索。已打通「Adapter → `ProviderSuccess` → `CompleteJob` → UPDATE」。
3. **未发布型号的返回码回归**：无 active 候选时曾返回 `Validation`（400），应为 `NotFound`（404）。
4. **把做不到的能力声明成支持的**：APIMart 声明支持参考图/遮罩，但上游要求公网可访问 URL，而本仓库没有上传链路 —— 当时先收窄为仅文生图、运行时显式拒绝；**随后按用户指定补上了上传链路**。补完之后仍未立刻放开这两条分支：`docs/adr/0002` 要求"未证实的参数不开启、经真实 wire 验证后再发布新修订"，而 `image_urls` 的取值形态与上传接口的真实行为都还没经验证。**2026-09-19 经用户批准做了真实受控验证后，两条分支已开放**（见"实际交付"与 `docs/facts/channel-facts.md` §5.6）。
5. **`PricePlanDraft.formula` 从不校验**：未知计价形态曾静默落库。
6. **资产绑定路径写死了字段名**：`image`/`images`/`mask` 被硬编码在领域与用例层，导致"字段名不是 `image` 的厂商"（APIMart 的 `image_urls`）无法绑定任何输入图。已改为按路径取厂商原生参数名，并把"参考图/遮罩"的判定收敛成**一个**有测试的函数（运行期与发布期共用），认不出的参数名直接拒绝而不是静默忽略。
7. **上传失败的分类不实**：`upload_failure` 曾把"生成任务可证明未受理"标成 `NotRetryable`（"确定性拒绝"）。三态里对应的是 `SafeBeforeAcceptance`（`docs/adr/0011`），已改正并补测试；`code`/`message` 仍按 `error.code` 保留。
8. **凭据类失败被误判成"受理状态不确定"**（零费用探测发现）：APIMart 的 401 信封里 `error.code` 是**空字符串**，只有 `type: "apimart_error"`，于是"只依据 `error.code`"这条规则会把一个明确没进到生成的请求送进人工对账。已加一条兜底：**凭据/权限类 HTTP 状态（401/402/403）判为确定性拒绝**，**5xx 仍不看状态码**（`build_request_failed` 会以 500 承载参数错误）。
9. **渠道事实里把 `APIMART_API_KEY` 写成"不存在"**：实际 User 与 Machine 级都有，当时只看了进程环境。已更正——这条错误会让读者以为受控验证根本做不了。

第 4 项在实现评审中两次被质疑、两次都按 ADR 收紧了声明；第 1–4、7 项由实现评审（Standards / Spec 双轴）发现，第 5、6、8、9 项为自行核对发现（第 8 项来自零费用路由探测）。

## 真实上游的受控验证（2026-09-19，经用户批准）

用户在本会话明确批准后执行，方案与用量当时就写明（上传 1~2 张 + 最多 2 次生成、`n=1`、`quality=low`、预算上限 $1）。实际：**上传 2 次 + 生成 2 次，合计不足 $0.03**。

1. **先不碰凭证拿事实**：4 次无 Authorization 头的请求，确认三个端点在 `api.apib.ai` 上真实存在，并取到 401 的错误信封（据此修掉上面第 8 条）。
2. **再直连探合同**：上传两张 512×512 测试图（参考图 + 带 alpha 的遮罩），随后一次生成带 `image_urls`（字符串数组）与 `mask_url`，11 秒完成 —— 一次结清四条待验项。
3. **最后走我们自己的服务**：发布真实素材 → 平台接口上传两张图 → 受理 Job（`/image_urls/0` + `/mask_url`）→ 真实 Worker 执行 → **`succeeded`**：结果图 `image/png` 1,486,934 bytes / 1024×1024 归档到自有对象存储，Evidence 记 `input_text=29 / input_image=1024 / output_image=196`，结算 **14217 microusd**，与按已发布单价算出的金额逐位一致；上游自报金额与公开单价的比值正好 **0.8**（账号折扣，与 §3.3 一致）。

敏感信息未入库：**不保存**真实图片 URL 与 task id，原始响应只留本机临时目录。据此，两个 APIMart 发布素材的 `allowed_branches` 已加上 `image_conditioned` / `masked`。

**结算口径的第一手核对（2026-09-19，用户提供上游账单面板）**：上游控制台的"详情"面板自己写明 `Base cost` = 各分项 token × 费率之和，`Actual cost = Base × Group ratio 0.8 × Channel ratio 1 × Discount ratio 1`。两次调用**逐位对上**：29/1024/196 ⇒ Base `$0.014217` ⇒ Actual `$0.011374`，而**平台侧 capture 正好是 14217 microusd**（= Base）；另一支 33/1024/196 ⇒ Base `$0.014237` ⇒ Actual `$0.011390`。⇒ 对外计费用的是已发布单价（= 上游 list 价），20% 折扣留在平台侧。面板还暴露两件之前不知道的事：**缓存档位**（缓存文本 $1.25/1M、缓存图片 $2/1M）与 `credits = USD × 10`；我们的 Price Plan 没有缓存字段、`cached_tokens` 也被丢弃，已记为待办。逐笔核对表见 `docs/facts/channel-facts.md` §3.9。

## 本变更对领域模型的影响（`0004` §4 的例外说明）

这次改动**动了领域层**（`AssetBinding` 的路径语义、`branch()` 的判定、新增 `DomainError::UnsupportedAssetParameter`），按 `docs/design/0004` §4 的判据必须逐项说明理由：

- 领域层改动**不是**为了适应 APIMart，恰恰相反：旧代码把 `image`/`images`/`mask` 三个**渠道字段名**写死在领域里，那才是渠道差异污染领域。现在领域只认识"路径的第一段是厂商参数名"这一件事，认不认得某个名字由一个**与具体渠道无关**的函数（`AssetParameterKind::classify`）回答；换一个字段名不同的 Provider，**不需要再改领域**。
- 因此它更接近 §4 的 **E1（平台侧新能力）**：把"资产绑到哪个原生参数"从硬编码升级为平台自己的通用能力，而不是新增某渠道的分支。
- 已知代价：渠道若用不以 `image` 开头、也不含 `mask` 的名字（例如 `reference_images`），平台会**拒绝**该绑定。这是**有意的**——用户 2026-09-19 决定平台内部只认渠道原生参数名，统一参数转换留给**后期对外消费侧**（见下面"未决项"第一条与 `docs/adr/0002` 的补充段）。

## 已知限制与未决项（不在本次交付范围）

- APIMart 的**图生图/遮罩分支已开放并已受控实测**（2026-09-19，见上）；仍未测的是 `sunburst` 的图生图（与 flare 同渠道族、同端点、同参数面）、`base64` 路径，以及 20MB / 16 张 / 256MB 这些**边界**——代码已按文档上限拒绝（单张 20MB + 单次总量 256MB），但没有逐个压测。
- **"哪个原生参数装图片"目前靠名字约定** —— **已由用户决定（2026-09-19）**：平台内部只认渠道自己的参数名，**不做**统一参数转换；统一转换属**后期对外消费侧**的能力，现在做会牵动每个渠道的适配与验证，所以先把各条渠道跑通。决定记在 `docs/adr/0002` 的补充段（工作项 #4 据此关闭）；名字约定因此是明确的过渡方案，`reference_images` 这类名字由那一层解决。
- `task_id` 不用于跨调用恢复（需新增列与拆分端口，属独立工作项）；
- 火山方舟/Seedream、直连 OpenAI、多图与 `stream`/`tools` 不在本阶段；
- 第 16–18 条验收条件已标为过期，其若要恢复需先结清非 USD 计价或金额型证据。
