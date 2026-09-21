---
title: 合同与承载面分层、映射声明化与对客目录（#10 落地）
status: implemented
created: 2026-09-20
updated: 2026-09-20
approval: 用户 2026-09-20 明确"方向需求很明确，你自己判断，提供设计方案落实"，并在讨论中确认三条裁定（图片字段合同外一律 400、defaults 加发布期校验、对客目录必做且暂不引入路由策略层）
verification: 2026-09-20 本地：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets --all-features -- -D warnings` 均 exit 0；`cargo test --workspace --all-features` 全绿；空库端到端 `cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1` **33 passed / 0 failed**（由收口者逐片重跑，零真实计费调用）
---

# 合同与承载面分层、映射声明化与对客目录（#10 落地）

## 问题

`capability_schema` 一份数据兼了两个身份：客户端合同与"这条供给能承载的渠道面"。于是对客合同等于命中那条供给的**渠道包装面**（同一 `size` 在三家分别是像素/比例/档位），随选路变化，也与厂商原生参数不一致。设计见 [`docs/design/0005`](../../../../docs/design/0005-vendor-model-contract-and-offering-mapping.md)，决策属 [`docs/adr/0015`](../../../../docs/adr/0015-vendor-model-contract-and-offering-parameter-mapping.md)。

## 实际交付（四个切片）

- **S1a 数据模型 + 发布期校验**：素材顶层 `capability_schema` = **模型级合同**，offering `carrier_schema` = **承载面**（过渡期合同来源唯一，旧素材照常可发布）；`catalog.vendor_models` 唯一键去掉 `schema_hash`（该列删除），改成 `(vendor_id, native_model_id, native_revision)`；**合同行不可变**（不再 `DO UPDATE`），"Job 固定受理时版本"这才成立；承载面与映射随 Job 冻结；发布期 R1（承载面字段从合同可达）/R2（落到 Driver 能写上线文的字段名）/R3。
- **S1b 受理期规则**：R4 按**合同**过滤（合同外字段丢弃、缺必填 400）；R5 逐候选要求"请求实际用到的字段"都在其承载面里，不合格原因进 `routing_decisions`；**全部不合格 → 503 `platform_unavailable`**（平台侧供给问题，不再算消费者 400）；`parameter_mapping.defaults` 注入。
- **S2 `size` 三义**：领域新增 `SizeSpec`（像素/比例/档位）+ 换算算法；**尺寸档案作为发布数据**放在 `parameter_mapping.size`，生产代码里没有任何厂商档位表；组装期换算（`2:3`+`2K` → `1664x2496`），档案缺格 → 该候选不合格。
- **S3 映射声明化**：`rename`（合同名 → 线上名，承载判据与 R1 相应扩展为"从合同可达"）、`enum_map`（取值映射，表外取值 → 候选不合格）、**R6**（`defaults` 的键必须合同里有且承载得了，否则发布 400）、**合同外图片字段一律 400 `invalid_parameter`**（丢图等于悄悄退化成文生图）。
- **S4 对客目录**：`GET /v1/models`（同一 API Key）→ `{data:[{name, vendor, revision, contract}]}`；只列"生效发布里至少有一条 enabled 供给（渠道也启用）"的模型；`contract` 逐字就是发布的那份合同；响应不含 `offering`/`channel`/`provider_kind`/`provider_model_id`/`adapter_key`/job 标识。

## 验证

| 行为 | 证据 |
| --- | --- |
| 合同与承载面分层、合同行不可变、承载面随 Job 冻结 | `http_contract` 内 S1a 用例（重发幂等、换合同被拒、新修订落新行、改发布后旧 Job 读旧面） |
| 合同外字段丢弃、缺必填 400、承载不了换候选、全不合格 503 | S1b 用例 |
| 尺寸换算进上游报文、档案缺格落选 | S2 两条用例 |
| 改名与取值映射进报文与 Job、表外取值落选、R6 发布被拒、合同外图片 400 | S3 四条用例 |
| 目录形状、停用后消失、不含内部词汇 | S4 用例 + `assert_public_only` |
| 迁移在已建过库上升（合并同模型重复行） | `images_pass_through_migration_applies_on_an_existing_database` 同族的迁移用例 |

## 已定的裁定（原未决）

图片字段合同外一律 400（不丢弃、不走 503）；`defaults` 加发布期校验 R6；对客目录必做、形状如上；**路由策略层暂不引入**，沿用 `ADR-0009` 的优先级选路（交接文档那条记为被取代，等真出现多个互相竞争的供给再谈）。

## 未做 / 边界

- 尺寸声明里的"默认档位"没做成声明项（平台补的 `defaults` 会参与换算）。
- `auto` 这类取值按"不是三型之一"处理：声明了尺寸换算的供给承载不了它。
- 承载面已声明的名字优先于 `rename`（文档化的确定性规则）。
- 渠道停用也会让型号从目录消失（否则目录会列出一个提交即 404 的型号）。
- 真正的多厂商素材（Google/ByteDance 各自的合同与承载面）尚未发布；三家各一份素材的联调与真机验证仍未做。
