# 历史文档归属切换登记

本文件由[文档入口](../AGENTS.md#历史归属切换)指向，拥有逐文件映射、适用范围与切换缺口。管理根为仓库根。登记日为 2026-10-09，检查基线为 `6c9a758e82bf1e6e61ffac31d4968c6dfd553e76`；开始时工作区无改动。

## 切换顺序与证据

1. 从文档入口登记 Proposal/Issue 与 Notes 的职责，保留阶段授权、Git、词汇表和工具定制。
2. 技术设计复用已有 Note，完整承接适用机制、真实备选、理由与代价，再完成所需评审。
3. 技术范围评审完成后才撤销对应旧设计的权威并修复引用；未完成的技术范围保持登记的过渡状态。
4. 旧 Spec 原地归档，不复制成新的产品合同。未来依赖工作先在对应 Issue 确认历史行为的适用范围、验收和批准证据，再进入实施门禁。

首次切换的三份技术 Note 经两轴评审后取得登记范围的权威。把九份旧 Spec 复制到新合同目录是错误的归属选择，用户随后明确纠正；本次移除新增副本，保留九份原始 Spec 及历史状态，不把它们视为新工作的默认合同。历史归档不等于已经找到产品合同的新属主。本次不批准新产品行为、不做产品实现，也不通过补造工作项或批准记录填补缺口。

## 历史产品合同

下表九份 Spec 保留原文、修订和历史批准事实，只作归档资料。本次没有确认某个现有 Issue 已完整接收其跨工作项行为，因此不宣称合同承接完成。新工作在相关 Proposal/Issue 写全选定的行为、失败、限制和验收；可以链接历史来源，但不能只凭历史的“已接受”状态沿用。归档不改变当前代码行为，也不撤销根项目指令中的事实与安全规则。

| 历史属主（`docs/specs/`） | 新工作属主 | 历史范围／修订 | 处理结果 |
| --- | --- | --- | --- |
| [0001](../specs/0001-admin-and-customer-consoles.md) | 对应 Proposal/Issue；具体承接工作项尚未确认 | 管理员与客户身份、控制台行为、安全与验收，v25 | 原地归档；工作项承接未完成 |
| [0002](../specs/0002-account-funds-and-reservations.md) | 对应 Proposal/Issue；具体承接工作项尚未确认 | 金额、占用、结算、查询、取整与核查，v8 | 原地归档；工作项承接未完成 |
| [0003](../specs/0003-account-names-and-login-identities.md) | 对应 Proposal/Issue；具体承接工作项尚未确认 | 名称、创建与绑定、展示与查找，v4 | 原地归档；工作项承接未完成 |
| [0004](../specs/0004-customer-authentication-pages.md) | 对应 Proposal/Issue；具体承接工作项尚未确认 | 公开认证页面、回跳、密码兑换与尝试限制，v2 | 原地归档；工作项承接未完成 |
| [0005](../specs/0005-synchronous-image-gateway.md) | 对应 Proposal/Issue；具体承接工作项尚未确认 | 同步调用、最小事实、幂等、收费与运行保证，v8 | 原地归档；工作项承接未完成 |
| [0006](../specs/0006-model-type-and-usage-records.md) | 对应 Proposal/Issue；具体承接工作项尚未确认 | 三类类型、分单位用量与汇总，v1 | 原地归档；工作项承接未完成 |
| [0007](../specs/0007-image-upload-and-object-storage.md) | 对应 Proposal/Issue；具体承接工作项尚未确认 | 单文件上传、对象、配置、安全与失败，v4 | 原地归档；工作项承接未完成 |
| [0008](../specs/0008-model-usage-documentation.md) | 对应 Proposal/Issue；具体承接工作项尚未确认 | 公共文档、指定修订与当前文档读取，v2 | 原地归档；工作项承接未完成 |
| [0009](../specs/0009-openai-compatible-model-list.md) | 对应 Proposal/Issue；具体承接工作项尚未确认 | OpenAI 标准列表字段与兼容边界，v1 | 原地归档；工作项承接未完成 |

上述身份与安全、资金与结算、同步调用与幂等、公开文档版本和兼容行为仍可能影响未来开发。依赖工作必须先把适用部分明确写入其 Issue，解决与当前代码、后续决定之间的冲突，并记录所需评审与批准。这里不替代行为正文，不新建第二份合同。

## 技术设计

下表为每份历史设计登记承接范围。目标 Note 存在不等于完整承接；未承接范围的唯一属主仍是该旧文件的适用部分，未来依赖工作先核对内容、设计接受证据和当前实现。特别是“待评审”“待接受”和正文矛盾不能因工作项关闭而默认为接受。

| 旧属主（`docs/design/`） | 现有或拟承接属主 | 结果与影响 |
| --- | --- | --- |
| [0001](../design/0001-image-generation.md) | 初期工作项及[初期交付 Note](../../.agents/notes/implemented/platform/2026-09-19-initial-image-generation-vertical-slice.md) | 仅保存初期范围与实现映射，冻结为历史，不拥有新工作 |
| [0002](../design/0002-image-generation-tech-design.md) | 初期交付 Note 与同步网关 Note | 未完成切换：旧 Worker、Asset、端点与参数规则混合；先逐段划分失效和有效内容 |
| [0003](../design/0003-doubao-ark-image-adapter.md) | 未来引入该厂商的工作项与 proposed Note | 未接受且移出原阶段；不作为 Provider 实现授权，恢复前重新核验上游事实与范围 |
| [0004](../design/0004-layered-architecture.md) | 按 Notes 规则建立分层设计 Note；[架构治理](../architecture.md)继续拥有既有边界 | 未完成切换：原文明确缺评审确认，不能代用户补批准；未来分层设计先解决缺口 |
| [0005](../design/0005-vendor-model-contract-and-offering-mapping.md) | [合同与映射 Note](../../.agents/notes/implemented/platform/2026-09-20-vendor-model-contract-and-offering-mapping.md) | 未完成切换：须核对发布／受理校验、尺寸三义、映射和完整接受范围 |
| [0006](../design/0006-gateway-models-and-consumer-surface.md) | [命名 Note](../../.agents/notes/implemented/platform/2026-09-22-gateway-model-naming.md)及发布设计属主 | 未完成切换：目录形状已有后续合同，原子发布与历史修订机制未完整承接 |
| [0007](../design/0007-pricing-floor-and-settlement.md) | [定价 Note](../../.agents/notes/implemented/platform/2026-09-22-pricing-floor-and-settlement.md) | 未完成切换：核对公式、保底查表、价格快照、原币种成本、毛利；资金写入由 0013 的范围约束 |
| [0008](../design/0008-routing-strategy-and-caching.md) | [策略 Note](../../.agents/notes/implemented/platform/2026-09-22-route-policy-layer.md)、[权重 Note](../../.agents/notes/implemented/platform/2026-09-22-routing-weight-and-decisions.md)、[缓存 Note](../../.agents/notes/implemented/platform/2026-09-22-redis-acceleration-layer.md) | 未完成切换：分清候选、策略、阶段回退、缓存有效范围；不能恢复生成重投 |
| [0009](../design/0009-operational-baseline.md) | 停机、告警、核查、配额等对应 Notes | 未完成切换：健康检查与停机等仍需承接；客户速率、每日金额和单次成本上限已由后续决定删除 |
| [0010](../design/0010-identity-and-consoles.md) | [身份 Note](../../.agents/notes/implemented/platform/2026-09-27-identity-sessions-and-consoles.md) | 未完成切换：完整会话、密码、增量发布与主机分发机制尚未逐项核对 |
| [0011](../design/0011-console-information-architecture.md) | 原交付工作项、[详情页 Note](../../.agents/notes/implemented/platform/2026-10-02-admin-account-and-customer-detail-pages.md) | 未完成切换：页面行为与技术理由混合，部分布局已替代；依赖工作先划分范围 |
| [0012](../design/0012-platform-model-publishing.md) | 素材导入／引用发布的 Note，按同主题复用或有内容时建立 | 未完成切换：Offering 复用、身份不变、引用发布与快照机制仍有活动源码引用 |
| [0013](../design/0013-account-funds-and-reservations.md) | 资金相关 Notes、[缓存版本 Note](../../.agents/notes/implemented/platform/2026-09-30-account-balance-cache-version-guard.md) | 未完成切换：账户条件更新与资金串行、幂等摘要、缓存竞态、账实核查仍需完整承接 |
| [0014](../design/0014-customer-console-navigation-and-history.md) | [客户页面 Note](../../.agents/notes/implemented/platform/2026-09-30-customer-console-pages-and-routing.md)、[历史 Note](../../.agents/notes/implemented/platform/2026-10-02-customer-history-and-key-confirmation.md) | 未完成切换：v2 待接受而生效 v1，日期游标与状态机需明确后续接受范围 |
| [0015](../design/0015-account-names-and-login-identities.md) | [名称身份 Note](../../.agents/notes/implemented/platform/2026-10-03-account-names-and-login-identities.md)、唯一性 Notes | 未完成切换：完整事务、名称分配和查询机制需要核对 |
| [0016](../design/0016-customer-authentication-pages.md) | [页面 Note](../../.agents/notes/implemented/platform/2026-10-03-customer-authentication-pages.md)、[尝试限制 Note](../../.agents/notes/implemented/platform/2026-10-05-public-auth-attempt-limits.md) | 未完成切换：页面状态、晚到响应、回跳校验、代理信任与失败计数分别承接 |
| [0017](../design/0017-synchronous-image-gateway.md) | [同步网关 Note](../../.agents/notes/proposed/platform/2026-10-03-synchronous-image-gateway.md) | 未完成切换：Note 仍为 proposed；不能由关闭工单推断总体已验收，且原机制已被 0018/0019 部分替代 |
| [0018](../design/0018-synchronous-gateway-remediation.md) | 同一同步网关 Note | 未完成切换：容量与内存、所有权、晚到事实、有界 Provider 身份、H1/H2 传输和 Linux 关闭监视尚未完整入 Note |
| [0019](../design/0019-synchronous-gateway-single-path.md) | 同一同步网关 Note | 未完成切换：区分唯一执行路径的现行机制与删除、迁移计划；旧每日上限验收不再适用 |
| [0020](../design/0020-model-type.md) | [模型类型 Note](../../.agents/notes/implemented/domain/2026-10-04-model-type.md) | 已切换：完整承接类型存储、导入发布、目录、用量和汇总、界面及迁移事实；调用标识归后续响应 Note |
| [0021](../design/0021-object-storage-upload.md) | [上传 Note](../../.agents/notes/implemented/platform/2026-10-04-reference-image-upload.md) | 已切换：完整承接模块、配置形状、凭据、许可、签名、对象键、HEAD、匿名可读、取消及同键重试；AIHubMix 下载参考图的旧说法失效 |
| [0022](../design/0022-aihubmix-ai-v1-execution-path.md) | [AIHubMix Note](../../.agents/notes/implemented/platform/2026-10-06-aihubmix-ai-v1-execution-path.md) | 已切换：完整承接端点、媒体引用、容器映射、同步结果、成本、能力校验、标识与失败；信封技术理由归后续 Note，新工作行为由 Issue 确认 |

## 持久决定

| 旧属主（`docs/adr/`） | 新属主或有效范围 | 结果与影响 |
| --- | --- | --- |
| [0001](../adr/0001-unified-image-generation-command.md) | 初期及同步网关 Note | 未完成切换：统一命令与派生分支；旧类型名不能直接沿用 |
| [0002](../adr/0002-native-capability-schema-not-canonical.md) | 合同与映射 Note | 未完成切换：原生 Schema、不可变修订、冲突与未知字段处置仍需完整承接 |
| [0003](../adr/0003-postgresql-is-source-of-truth.md) | 缓存与执行 Notes | 未完成切换：事实权威的跨范围选择、事务迁移成本与真实备选须完整承接 |
| [0004](../adr/0004-vendor-and-provider-identities-stay-separate.md) | 合同与映射 Note | 未完成切换：原文确认没有评估过合并身份，不能补造历史备选；身份边界仍有效 |
| [0005](../adr/0005-billing-path-uses-v1-endpoints.md) | 渠道事实、AIHubMix Note | 已退役：端点实现事实，历史文件保留 |
| [0006](../adr/0006-no-settlement-without-metering-evidence.md) | 相关工作项的证据合同与定价 Note | 未完成切换：证据单位、维度、缺失处置、声明金额与成本边界未完整分属；不得降低结算门槛 |
| [0007](../adr/0007-reconciliation-instead-of-automatic-retry.md) | 同步网关 Note | 未完成切换：不确定创建不重发仍有效，旧 Worker 提交和端点备选理由不能当现行机制 |
| [0008](../adr/0008-own-object-storage-is-the-platform-result.md) | 图片透传 Note；显式上传边界见上传 Note | 已被取代：结果不归档；显式参考图上传不恢复结果资产托管 |
| [0009](../adr/0009-multiple-active-offerings-and-routing.md) | 路由 Notes 与[修订历史 Note](../../.agents/notes/implemented/platform/2026-09-27-adr-0009-revision-history.md) | 未完成切换：发布候选与策略选择需完整承接，不混合历史优先级表述 |
| [0010](../adr/0010-metering-evidence-is-unit-bearing.md) | 原合并属主 0006，待承接其新属主 | 已合并历史编号；不独立生效，0006 仍有切换缺口 |
| [0011](../adr/0011-safe-before-acceptance-does-not-retry-yet.md) | 根项目指令的事实与安全、同步网关 Note | 生成重投范围失效；RetrySafety 的事实分类不自动授予重投能力；旧重试 Note 仅保留历史理由 |
| [0012](../adr/0012-provider-declared-charge-as-evidence.md) | 拒绝记录、AIHubMix Note | 已退役：金额反推 token 的拒绝不禁止以声明金额为独立计量形态 |
| [0013](../adr/0013-retire-gpt-image-2-use-2-5-models.md) | 发布物与渠道事实 | 已退役：可售型号是配置，旧编号不拥有当前目录 |
| [0014](../adr/0014-phase-two-provider-set.md) | 原工作项 | 已退役：阶段范围不是持续架构决定 |
| [0015](../adr/0015-vendor-model-contract-and-offering-parameter-mapping.md) | 合同与映射 Note | 未完成切换：合同归模型、映射归供给及其理由；与设计 0005 一起承接 |
| [0016](../adr/0016-cost-basis-per-channel.md) | 原合并属主 0006，待承接其新属主 | 已合并历史编号；0006 的成本边界仍待承接 |
| [0017](../adr/0017-provider-errors-are-rewritten-for-consumers.md) | [错误改写 Note](../../.agents/notes/implemented/platform/2026-09-20-consumer-facing-provider-error-rewrite.md) | 未完成切换：完整平台码白名单、责任分类及凭据错误边界需核对 |
| [0018](../adr/0018-open-parameters-by-first-party-docs.md) | 合同与映射 Note、渠道事实 | 未完成切换：文档依据、冲突验证和未知字段规则需完整承接，不要求逐参数计费探测 |
| [0019](../adr/0019-images-pass-through-without-asset-storage.md) | [图片透传 Note](../../.agents/notes/implemented/platform/2026-09-20-images-pass-through-without-asset-storage.md)、同步网关及上传 Notes | 未完成切换：结果原形仍有效，旧内联图片、队列与结果持久化已被后续范围取代 |
| [0020](../adr/0020-routing-strategy-layer-configured-by-operations.md) | 路由策略 Note | 未完成切换：候选合格性优先及策略权衡随设计 0008 承接 |
| [0021](../adr/0021-consumer-pricing-method-owned-by-operations.md) | 定价 Note、AIHubMix Note | 未完成切换：运营选择仍受驱动证据能力限制；不得从旧措辞推定 token 与金额形态都可售 |
| [0022](../adr/0022-reference-image-upload-endpoint.md) | 上传 Note；新工作的产品行为归对应 Issue | 已切换：显式独立上传、生成只收 URL、结果不归档、真实备选与反悔成本完整承接 |

## 活动引用与未来工作的缺口

活动范围覆盖根与 Web README、项目指令、文档入口、领域与 Issue 指南、运维／教程／验证清单、Notes 和源码注释。产品历史引用指向原始归档 Spec，并标明历史用途；新工作的行为与验收从 Issue 取得。源码注释保留当地必要合同，移除新增合同目录的引用，不改成引用历史 Spec 或 Issue。已切换的设计引用指向其 Note。尚未承接的设计引用保留并通过本登记说明有效范围。

历史目录内互相引用、`.agents/handoff/` 的当时快照、固定提交的 Issue 链接、SQL 历史迁移及用来验证文档链接的测试字符串保留历史身份，不专项改写；修改已应用迁移的注释也会改变 checksum，因此不动。历史目录不给未来新主题提供默认位置。

GitHub 跟踪器读取可用，当前开放的工作为 #82（共用系列合同与离线素材）和 #83（缓存与数据库访问优化），都为 `proposal:planning`。#83 的设计正文按当时用户“不要本地落盘”要求暂留 Issue 评论，撤销时效与缓存规则仍待批准；本次配置不迁移它的正文、不改变其状态或历史授权。未来继续这两项时，在原 Issue 更新范围和评审证据，再按新归属承接设计，不能另造重复工作项或视为已批准。

未完成切换的共同完成条件：确认具体有效章节和已有接受证据；产品行为在对应 Issue 明确，技术内容完整承接到已有或有内容的新 Note，覆盖机制、范围、真实备选、理由与代价；完成所需评审后才撤销旧权威并修复该范围活动引用。评审确认不足或产品选择不明时在原工作项登记影响，依赖范围保持未就绪。
