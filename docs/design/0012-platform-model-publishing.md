主题: 平台模型发布：可选择的 Offering 与选择式发布
当前修订: v2
状态: 待评审
来源: 提案 [#13](https://github.com/dehuadong/seeaihub-server-next/issues/13) 范围第 2 条；承接 Spec [`0001`](../specs/0001-admin-and-customer-consoles.md) §5.3 的 V-D8/V-D9
依赖: [`0006`](./0006-gateway-models-and-consumer-surface.md)（网关模型对象）、[`0007`](./0007-pricing-floor-and-settlement.md)（定价归属）、[`0005`](./0005-vendor-model-contract-and-offering-mapping.md)（合同/承载面/映射分层）；[`CONTEXT.md`](../../CONTEXT.md)（Offering / Gateway Model / 供应商模型名）；`ADR-0003`、`ADR-0009`、`ADR-0015`

# 平台模型发布：可选择的 Offering 与选择式发布

`0006` 定下了 Gateway Model 这个对象，`0007` 定下了它的定价归属。本文补的是两者都留空的那一块：**运营发布时，"候选"具体从哪里来、以什么粒度被选中**。

## 1. 缺口（核实过的现状）

`0006` §1.1 把 Gateway Model 定义成"平台型号名 + 一个 Vendor Model Revision + 一组有序候选（Offering × Channel × 供应商模型名）+ 对客定价"，但 §1.2 把 `supply.offerings` 一句话带过："网关模型候选集的元素；候选自身的定义不改"。命令层于是只有一条路：`POST /api/v1/runtime-revisions` 的每条候选**内联**渠道三要素、驱动器、承载面与参数映射。

核实过的代码事实：

| 事实 | 依据 |
| --- | --- |
| `supply.channels` 的 INSERT 只有一处，在发布事务里 | `crates/persistence/src/lib.rs` 的 `publish_runtime` |
| `supply.offerings` 的 INSERT 只有一处，同一函数 | 同上；发布对 `(vendor_model_id, channel_id)` 走 `ON CONFLICT ... DO UPDATE` |
| `pricing.price_plans` 的 INSERT 也只有这一处 | 同上 |
| 与供给有关的管理端点只有一个，且只改启用开关 | `apps/api/src/main.rs` 的 `PATCH /api/v1/offerings/{id}` |

结论：**渠道与 Offering 没有独立于发布的创建路径**，它们是发布动作的产物、之后按身份被复用。所以"工程师配好、运营只选"这件事今天不存在——不是界面没做，是这条路径没有家。

`#14`（P1）的交付范围写着"本片只做命名层与对客面……管理员用**整份发布**定义它"，所以这不是实现漏了一条：`#13` 范围第 2 条要的"**选择** vendor → **选择**对应渠道支持的模型"从来没有被设计过。

## 2. 运营能选的粒度是 Offering

**Offering 就是"某厂商模型经某渠道、用某个供应商模型名提供"的那一条可调用供给**（`CONTEXT.md` 词条）。它由工程师配一次，之后长期存在、可被多个 Gateway Model 复用。

一条 Offering 的构成与归属：

| 部分 | 来自 | 谁配 |
| --- | --- | --- |
| 厂商标识、厂商模型名与修订、调用方合同 | `catalog.vendor_models` | 工程师 |
| 渠道三要素（供应商、地址、凭证变量名）与启用状态 | `supply.channels` | 工程师 |
| 驱动器、供应商模型名、承载面、参数映射、限制 | `supply.offerings` | 工程师 |
| 该渠道的成本费率与币种 | `pricing.price_plans` 与 Offering 上的 `cost_currency` | 工程师 |
| 对客费率向量、成本口径、参考成本、保底表 | 发布物（`0007` §2） | **运营** |
| 加价系数 | 发布物，**每个 Gateway Model 一个** | **运营** |

**运营选的粒度是 Offering 这一个**，不是它里面的任何一个部分。选了 Offering，合同、驱动器、渠道地址、凭证变量名、承载面、参数映射**全部已定**——这正是"选 vendor → 选该 vendor 适配的渠道模型"能成立的原因。

### 2.1 可选择的清单

管理员读一条**可选 Offering 清单**，按厂商分组：厂商标识、厂商模型名与修订、渠道、供应商模型名、驱动器、渠道币种、渠道成本费率、启用状态。

**不含渠道地址与凭证变量名**：那是渠道部署事实，选择不需要它们，回显它们只会把部署细节摆到运营面前（`0006` 已定的"不回显渠道凭证"）。

清单里的每一项都带 `offering_id`——它是选择的键，也是发布命令里候选引用的东西。今天这条清单只能由既有的 `supply.offerings` 投影。

### 2.2 一个 Gateway Model 可以有几条 Offering

`0006` §1.1 的"一组有序候选"就是它：运营选 vendor 之后可以选**一条或多条**该 vendor 下的 Offering，给每条排档位、设权重——容灾（前一条不可用时回落）与分摊（同档按权重分流）本来就在这组候选里。少选几条，模型就没有容灾。

### 2.3 一个 Gateway Model 只属于一个厂商

```
publication.runtime_revisions.vendor_model_id   uuid NOT NULL
```

单值，一次发布只指向一行厂商模型。所以"同一个 Gateway Model 同时走 OpenAI 与 Gemini"今天不成立——那是**两个 Gateway Model**。这是 `0006` §1.2 已有的边界，本文不改；要改是另一个决定，改的是"Gateway Model 能否跨厂商"这件事本身。

## 3. 可选择的 Offering 从哪来：由工程师的素材导入

Offering 是**工程师配好的资产**，不是运营的输入。它由**已经存在的发布素材**（`config/bootstrap/*.json`，这正是工程师今天写渠道与供给的地方）导入而来，导入是**幂等**的：按 `(vendors 身份, 渠道身份)` 匹配既有行，有就更新技术定义、没有就新建。它随服务启动时已有的迁移步骤一起跑，不新增运营入口、也不新增工程端点。

因此运营在主路径上**没有任何配置渠道的动作**：渠道、Offering、合同、驱动器、承载面、参数映射、渠道成本费率全部在素材里由工程师写一次。

**运营创建 Gateway Model 时至少选一条 Offering**：一条调不动的模型不是商品，没有任何入口能让它先占个名字。名字在首次发布时建立（`publication.gateway_models` 由发布事务插入），与 `0006` §1.6 的写入方一致。

## 4. 发布：选 + 给价

候选从"内联全部技术字段"改成"引用一条 Offering + 给这条候选的价"：

| 候选携带 | 内容 |
| --- | --- |
| 引用 | `offering_id`（选择的结果） |
| 路由 | `routing_priority`、`weight` |
| 定价 | `consumer_rates_cny`、`cost_basis`、`reference_cost_microusd`、`tier_prices`、`floor_amounts`（`0007` §2） |

渠道三要素、驱动器、供应商模型名、承载面、参数映射、限制**不再出现在命令里**：由被引用的 Offering 决定，发布期从库里取，取不到或被停用就拒绝并点名是哪一条。

命令仍保持"完整、有序的候选集合"这条性质（`ADR-0009`）：运营提交的是他这次要的**那组 Offering 引用**，原子替换的是这组引用与它们的价。

## 5. 已发布修订不受 Offering 后续改动影响

`publish_runtime` 今天对 `(vendor_model_id, channel_id)` 走 `DO UPDATE`，而受理读的是**活表**（`supply.offerings` 与 `supply.channels` 的当前值）。因此共享同一个 Vendor Model 的两个 Gateway Model 会互相改活候选，工程师后来改渠道地址也会影响已发布修订的受理取值。

**引用式发布必须把这件事定死**：发布时把被引用 Offering 的**技术定义快照进这次修订**（与"随 Job 快照冻结"同一条纪律），受理读快照，不读活表。否则"发布即冻结"这句话是假的。这是本文要求的行为改变，改的是受理取数，不改定价口径。

## 6. 定价与折算率

沿用 `0007` §2，本文不改：`markup_bps` 每个 Gateway Model 一个；`consumer_rates_cny` **按候选**（多条 Offering 成本不同，各有各的价）；`cost_basis` 与 `reference_cost_microusd` 按候选；`tier_prices` / `floor_amounts` 按候选。

折算率是 `币种 → CNY` 的全局事实（`0007` §2），同币种也录一行、率恒为 1。它**不是**某条 Offering 的属性，所以它**不占运营的一个导航项**：运营给某条候选定价时要在意的只是"这个渠道币种有没有生效的折算率、是多少"，所以它出现在**发布页那条候选的定价处**——缺就当场录、有就显示当前生效的那一行。管理端的独立折算率页面撤掉，它的读接口保留给运维与排障。

## 7. 迁移

- 已发布修订**不回填、不改写**：它们的候选本来就带内联技术定义，`runtime_revisions` 的定价列已就位，受理与结算按 §5 的快照化之前的口径读，逐位不变；
- 既有 `supply.offerings` 行就是可选清单的来源，不需要数据搬迁；
- 新发布走引用式命令；老形状的整份发布**在过渡期继续接受**（等价于"用内联定义配一条 Offering，再引用它"），但它不再是运营的路径。过渡何时结束由实施时定，定在 Spec 里而不是本文。

## 8. 验证边界

- **接口契约**：素材导入是幂等的（同一份素材跑两次不产生第二行）；引用不存在的 Offering 被拒并点名；引用已停用、或缺折算率的 Offering 被拒；发布不带任何 Offering 被拒；多条 Offering 各自带自己的对客费率，倍率是 Gateway Model 级一个；发布后受理取到的是**快照**而不是活表（改渠道地址后已发修订的取值不变）；
- **浏览器行为**：运营在主路径上**看不到**渠道地址、凭证变量名、驱动器、承载面与参数映射；导航里没有折算率页；发布页在候选的定价处显示该渠道币种的当前折算率、并在缺它时能就地录入；
- **不可自动化**：清单按厂商分组、排序与权重好不好用，归人眼与人工验收清单。
