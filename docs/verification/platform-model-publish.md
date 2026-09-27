# 平台模型发布的人工验收清单

对象：运营在管理后台发布一个平台模型——**选厂商、勾供给、给价**，不碰技术字段。步骤里的界面元素就是界面上的东西，不再另立代号。

## 0. 准备

1. 仓库根 `.env` 里有 `ADMIN_EMAIL` 与 `ADMIN_PASSWORD`（`scripts/` 下的启动脚本会用它建管理员）。
2. 起服务并让**素材导入**生效：`SUPPLY_MATERIAL_DIR=config/bootstrap`（不设的话可选清单是空的——库里没有任何现成供给）。
3. 折算率页确认有渠道币种的行（素材用 USD）。没有的话，勾完供给后定价处会提示缺哪一行。

## 1. 发布一个平台模型（对客可见）

1. 进「模型目录」→ 右上角「上架新模型」；
2. 填一个平台模型名（例如 `gpt-image-2.5-plus`）→ 选厂商（素材里是 `OpenAI`）→ 加价系数；
3. 下方列出**该厂商下**的供给，每条带渠道、渠道模型名、计价形态、渠道币种；
4. 勾一条或多条，给档位、档内权重，以及**这条候选的价**（按 token 计量的填四档 CNY 费率；按张 / 按次的填参考成本与保底）；
5. 发布 → 抽屉里出现成功答复，模型目录里出现该名字。

**要人眼判断的**：

- 清单按渠道分组/排序读起来顺不顺，一眼能不能看出两条供给的差别（渠道、渠道模型名、计价形态、币种）；
- 四条候选勾在一起、档位与权重各不相同的时候，概览表能不能看出"这次会发布什么"；
- 提示语的措辞说不说得清（例如缺折算率时那句指向的是不是你要他去的地方）。

**下面这两样看着像"人眼判断"，其实是硬事实，已由浏览器用例守着**（判据可判定就不该留在人工清单里）：

- **停用的供给列出来并标明"不能选"**，而不是消失——`apps/web/e2e/platform-model-disabled-offering.spec.ts`（断言它可见、`disabled`、且"已停用，不能选"这行字在）；
- **定价处显示的折算率与折算率页上的是同一个数**——`apps/web/e2e/platform-model-publish.spec.ts`（先看折算率页那一行，再进发布抽屉核对定价处显示的是同一个数）。

## 2. 运营那条路上不该出现的东西

1. 在发布抽屉里从头翻到底（**不展开**底部的折叠区）；
2. 全页查找这些词，**一个都不该出现**：`base_url`、`credential_env`、`adapter_key`、`carrier_schema`、`parameter_mapping`，以及渠道地址的实际值（形如 `https://…`）与凭证变量名（形如 `AIHUBMIX_API_KEY`）。

底部折叠区里是工程师那条"贴整份发布定义"的路（过渡期仍可用），展开后出现技术字段是**预期的**，不算这一节的失败。

## 3. 改价

1. 在模型目录里点某个已发布型号的「改价」；
2. 抽屉里应当带出它现有的候选与价；
3. 只改加价系数或某条候选的价 → 发布；
4. 回到目录：**候选还在、指向同一条供给、倍率是新值**。

**要人眼判断的**：带出来的候选与价与目录里显示的一致；改价过程中界面没有让运营重新选一遍技术东西。

## 4. 停用与对客面

1. 在模型目录里停用刚发布的那个平台模型；
2. 对客目录（`app.localhost` 那个入口的模型列表）里**不再有**它；
3. 用它受理一次 → 得到"模型不存在"，而不是平台侧故障。

## 5. 验收记录

执行人填写（日期、执行人、结论、发现的问题）：

| 小节 | 结论 | 备注 |
| --- | --- | --- |
| 1. 发布 | 待填 | |
| 2. 不该出现的东西 | 待填 | |
| 3. 改价 | 待填 | |
| 4. 停用与对客面 | 待填 | |

### 已经由机器承担的部分

下面这些**不需要**在人工验收里重复做，它们各自有用例守着（判据见工作项里的 P1–P7）：

| 判据 | 机器证据 |
| --- | --- |
| 可选清单按厂商分组、字段齐全、带标识，且**不含**渠道地址与凭证变量名 | `apps/api/tests/http_contract/cases_publication.rs` 的 `the_selectable_offering_list_carries_the_selection_key_without_deployment_facts`（整份答复逐字查泄露） |
| 选厂商 → 勾供给 → 给价 → 发布成功 | `apps/web/e2e/platform-model-publish.spec.ts` |
| 引用不存在的供给被拒并点名 / 引用已停用的被拒并点名 | 同文件的 `a_reference_to_an_offering_outside_the_supply_table_is_rejected_by_name`、`a_reference_to_a_disabled_offering_is_rejected_by_name` |
| 多条供给各有各的对客费率，倍率是平台模型级一个 | `a_referenced_publication_freezes_the_offering_row_it_points_at` 与 e2e 里对发布结果的核对 |
| 给了参考成本也能发出去，成本口径由渠道的计价形态推出来 | `a_referenced_publication_derives_the_cost_basis_from_the_channel_formula` |
| 工程师改渠道地址后，**已发布修订**的受理取值逐位不变 | `a_published_revision_keeps_its_offering_definition_when_the_channel_changes`（先断言旧值不变，再断言重新发布取到新值） |
| 停用平台模型后受理得到"模型不存在" | `cases_publication.rs` 的 `gateway_model_naming_keeps_the_vendor_name_off_the_consumer_surface`；被停用候选的选路见 `cases_cache.rs` 与 `cases_routing.rs` |
| 停用的供给在清单里标明"不能选"，而不是消失 | `apps/web/e2e/platform-model-disabled-offering.spec.ts` |
| 定价处显示的折算率与折算率页是同一个数 | `apps/web/e2e/platform-model-publish.spec.ts` |
| 选完厂商后清单**只列该厂商**的供给 | `apps/web/e2e/platform-model-vendor-filter.spec.ts`（夹具发两个厂商各一条，两个方向都断言：这一家的在、另一家的不在，再换一家反过来） |

**界面里"好不好用"这部分没有机器证据，也不假装有**：清单的分组与排序、概览表能不能一眼看出这次发布什么、提示语的措辞——这三样按仓库规矩归人眼，就是第 1 节要人判断的那些。
