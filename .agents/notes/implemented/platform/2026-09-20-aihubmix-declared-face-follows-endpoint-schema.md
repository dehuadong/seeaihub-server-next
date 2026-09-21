---
title: AIHubMix 素材的声明面对齐端点 request.schema
status: implemented
created: 2026-09-20
updated: 2026-09-20
approval: 用户 2026-09-20 指出 AIHubMix `/v1` 那族的 `capability_schema.properties` 存在问题，指定以 `out-reference/aihubmix/schema-gpt-image-2.5-{flare,sunburst}.endpoints.json` 里 `kind: openai_compatible` 的 `/v1/images/edits`、`/v1/images/generations` 的 `request.schema` 为准核对字段、枚举与取值范围；并明确 Driver 能力面（`supported_top_level_parameters`）可以保留（那是能力上限，不是这次发布的声明面）
verification: 2026-09-20 本地：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets --all-features -- -D warnings` 均 exit 0；`cargo test --workspace --all-features` 全绿（0 failed）；空库端到端 `cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1` **18 passed / 0 failed**（含用真实素材发布并断言声明面的用例）。零真实计费调用
---

# AIHubMix 素材的声明面对齐端点 request.schema

## 问题

三份 AIHubMix 发布素材（`config/bootstrap/aihubmix-gpt-image-2.5-flare.json`、`-sunburst.json`、`aihubmix-gpt-image-2.json`）的 `capability_schema.properties` 与它们**实际调用的端点**对不上。本平台走的是 OpenAI 兼容的 `/v1` 族，而素材里混进了 `/ai/v1` 那族才有的字段，还有几处枚举是自己发明的：

| 素材原本声明 | `/v1` 端点 `request.schema` | 处置 |
| --- | --- | --- |
| `background`、`output_compression`、`user`（2.5 另有 `moderation`） | **没有这四个**（`additionalProperties: false`；它们属 `/ai/v1` 族的 `extra`） | 删除 |
| 一条 `allOf`：`background=transparent` → `output_format=png` | 没有（因为 `background` 不存在） | 删除 |
| `size`：enum `["auto","1024x1024","1536x1024","1024x1536"]` | `string|null`，**无枚举** | 去掉枚举，只留类型 |
| `output_format`：enum `["png","jpeg"]` + default `png` | `string|null`，**无枚举**，default `png` | 去掉枚举，保留 default |
| `quality`（gpt-image-2）：含 `auto`、default `auto` | enum 只有 `low`/`medium`/`high`，**无 default** | 改成三个值、去掉 default |
| `prompt`/`image`/`mask` 的 `minLength`/`maxLength` | 只有 `type: string` | 去掉自造的长度约束 |

对得上、保留不动的：`model`（收窄成 `const` 是平台路由需要）、`prompt`、`image`/`mask`、`n`（1–10、default 1）、2.5 的 `quality` 枚举与 default、`output_format` 的 default `png`。`image` 在 edits 端必填、在 generations 端不存在，正好对应平台"有图走 edits、无图走 generations"的分流。

## 决定与落地

- **口径**：声明面以**该模型对应端点**的 `request.schema` 为准（字段、枚举、取值范围都取它），不以第一方文档的宽面为准。写进 [`ADR-0002`](../../../../docs/adr/0002-native-capability-schema-not-canonical.md) 与 [`ADR-0018`](../../../../docs/adr/0018-open-parameters-by-first-party-docs.md) 的 2026-09-20 修订。
- **Driver 能力面保留**：`AdapterDescriptor.supported_top_level_parameters` 不动（它只是"这个 Adapter 能承载什么"的上限，不是这次发布的声明面）。因此 AIHubMix 仍可发布声明 `background` 等名字的素材；要不要发由素材决定。
- **发布期校验放宽一处**：`crates/adapter-aihubmix/src/lib.rs` 原先要求 `size`/`output_format`/`quality` 必须带字符串枚举；端点 schema 对 `size`/`output_format` 只声明类型，故改为"类型必须是 string，枚举可有可无（有则必须是字符串枚举）"。`background`/`moderation` 仍要求枚举。
- 素材的 `_comment`/`_evidence` 注明参数面来源（哪份端点快照）。

## 验证

| 行为 | 证据 |
| --- | --- |
| 三份素材通过该 Adapter 的发布校验 | `accepts_bootstrap_capability_contract`（读真实素材） |
| 用真实素材发布后，声明过的参数上行、未声明的在上游报文里不出现 | `declared_parameters_go_upstream_and_undeclared_ones_never_leave_the_platform` |
| 平台装载的参考图不被过滤掉 | `filtering_never_drops_the_images_the_platform_places` |
| 全量回归 | fmt/clippy exit 0；workspace 单测 0 failed；端到端 18 passed / 0 failed |

## 未做

- `n`、`quality`、`size`、`output_format` 在 schema 里都允许 `null`；素材按平台的空值约定只声明非空类型（`null` 等于"没给"），没有把 `["string","null"]` 写进声明面。
- `model` 声明成 `const`（端点 schema 只写 `string`）：这是平台为了把请求路由到该型号而做的收窄，保留。
