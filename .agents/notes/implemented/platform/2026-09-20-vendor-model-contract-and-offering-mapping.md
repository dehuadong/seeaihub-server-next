---
title: 合同与承载面分层、映射声明化与对客目录（#10 落地）
status: implemented
created: 2026-09-20
updated: 2026-09-20
approval: 用户 2026-09-20 明确"方向需求很明确，你自己判断，提供设计方案落实"，并在讨论中确认三条裁定（图片字段合同外一律 400、defaults 加发布期校验、对客目录必做且暂不引入路由策略层）
verification: 2026-09-20 本地：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets --all-features -- -D warnings` 均 exit 0；`cargo test --workspace --all-features` 全绿；空库端到端 `cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1` **37 passed / 0 failed**（由收口者逐片重跑，零真实计费调用）。后续 AIHubMix 承载面补齐（`background`/`output_compression`/`moderation` 按厂商契约声明）与多图 `image[]`（参考图声明成数组 ≤16、Driver 按张数编码 `image`/`image[]`）已并入同一批素材与用例。
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

## 厂商侧证据（2026-09-20，用户告知）

- **已取到 OpenAI 一手文档并落快照**（2026-09-20，经本机系统代理 `127.0.0.1:7897`；此前直连被 Cloudflare 挡）：`out-reference/openai/openai-images-generate-2026-09-20.md`、`-edit-2026-09-20.md`、`openai-image-generation-guide-2026-09-20.md`（均 HTTP 200）。据此：
  - 厂商模型枚举**明确包含** `gpt-image-2.5-flare`/`-sunburst`（含各自 `2026-09-08` 快照）；
  - 厂商原生 `size` 是**像素型**（`WIDTHxHEIGHT`，宽高被 16 整除、比例 1:3~3:1、上限 `3840x2160`、`>2560x1440` 实验性，标准值 `1024x1024`/`1536x1024`/`1024x1536`，`auto` 适用）——**2.5 被写进这段**，此前"未对 2.5 单独确认"的说法作废；
  - 厂商侧**没有 `resolution`**，那只是 APIMart 的渠道包装；
  - `quality` 默认 `auto`，**2.5 两款额外支持 `xhigh`/`max`**，与用户告知的六档一致。
  `docs/facts/channel-facts.md` 里那条"未结清"已据此改成**已结清**。
- OpenAI 最新上线提供 **`gpt-image-2.5-flare` 与 `gpt-image-2.5-sunburst`** 两种模型；**与旧模型的核心区别是支持 `low`/`medium`/`high`/`xhigh`/`max`/`auto` 六档质量设置**。⇒ 这两个模型名**是厂商侧真实模型**，`quality` 的六档与默认 `auto` 也据此归厂商侧，不再算渠道 schema 的推断。
- 仓库里那份 OpenAI 官方快照（`out-reference/openai/openai-images-api.md`）正文只覆盖到 `gpt-image-2`，**不含 2.5**，且**全仓没有任何文档引用它**——2.5 的其余特有项（例如 `size` 上限是否仍 `3840x2160`、16 的倍数与 1:3~3:1 约束是否照旧）仍按"沿用 gpt-image 家族面、未对 2.5 单独确认"标注，不冒充已确认。
- 素材迁移（一个 Vendor Model 一份顶层合同 + AIHubMix / APIMart 两条 offering）与逐项出处清单见提交历史与素材自身的 `_evidence`。

## 未做 / 边界

- 尺寸声明里的"默认档位"没做成声明项（平台补的 `defaults` 会参与换算）。
- `auto` 这类取值按"不是三型之一"处理：声明了尺寸换算的供给承载不了它。
- 承载面已声明的名字优先于 `rename`（文档化的确定性规则）。
- 渠道停用也会让型号从目录消失（否则目录会列出一个提交即 404 的型号）。
- 真正的多厂商素材（Google/ByteDance 各自的合同与承载面）尚未发布；三家各一份素材的联调与真机验证仍未做。

## 相关记录

- **取代** [AIHubMix 素材的声明面对齐端点 request.schema](./2026-09-20-aihubmix-declared-face-follows-endpoint-schema.md)：那份"声明面以端点 `request.schema` 为准"的口径，在本层（合同与承载面分层）落位后改为**按厂商契约声明**——承载面照厂商契约声明，渠道不接受就表现为渠道报错。失效范围见该记录头注。
