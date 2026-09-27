---
title: 嵌套容器面的过滤与发布校验：当前实现与被拒路径
status: implemented
created: 2026-09-27
updated: 2026-09-27
approval: 本条只记录当前实现状态与一条被拒路径的落点，不引入新决定；ADR-0018 的受理前过滤规则（只比顶层字段名）由该条 2026-09-20 修订拥有，发布期与受理期的映射规则由 docs/design/0005 拥有
verification: 2026-09-27 本地：`node scripts/decisions/check.mjs` 通过（本记录与本地链接）。状态事实来自源码与发布素材的当前形状（crates/adapter-sdk/src/lib.rs 的 supported_extra_parameters、crates/application/src/lib.rs 的 validate_adapter_compatibility、crates/domain/src/image_parameters.rs 的顶层过滤、config/bootstrap/gpt-image-2.5-{flare,sunburst}.json）；行号不写进正文，避免随代码腐坏
---

# Agent Note：嵌套容器面的过滤与发布校验：当前实现与被拒路径

## 问题

受理前的参数过滤只比**顶层**字段名：候选声明了 `extra` 这类嵌套容器时，容器会整个留下，里面没有逐键声明的键也照样上行；要收住嵌套面，过滤必须跟着逐键。ADR-0018 的边界（同日登记）说的是这条规则，但当时把那句话与实现状态写在一起——现无 Adapter 或发布素材开放容器面，这条路径今天走不到。ADR 写当前规则，实现状态与"为什么今天走不到"归本记录。

## 决定

**当前没有一条可达路径会走到嵌套容器面。** 发布期已经先挡住一半：承载面若在 `extra` 下声明任何键，而该 Adapter 没有声明支持它，发布被拒（`adapter ... does not support native parameter extra.<key>`）；两个 Adapter 的嵌套容器支持清单都是**空的**。当前发布素材（`config/bootstrap/gpt-image-2.5-flare.json`、`gpt-image-2.5-sunburst.json`）的合同与 AIHubMix 承载面都没有声明 `extra` 这个容器。

**开放的后果**：素材一旦声明 `extra` 容器，载体校验对"容器内没有任何键"是放行的（空容器不触发上面那条拒绝），受理前的过滤又只比顶层字段名——于是容器内的键不逐键判定就上行，等于在嵌套面里把"未声明也放行"开了个口子。

## 备选方案

**把这条边界留在 ADR-0018 里，连实现状态一起写。** 落选：ADR 正文写当前实现状态会随代码腐坏，而"当前没有素材开放它"是可核实的事实、并不改变过滤规则本身。

**把"两个 Adapter 都不开放 `extra`"写进 `docs/facts/channel-facts.md`。** 落选：那是平台侧实现状态，不是渠道事实——渠道侧的 `extra` 归属与取值已经在该台账的 AIHubMix 节（`/v1` 族没有这一层、`/ai/v1` 族有）记着。

**另立一份 `proposed` 记录，把"开放嵌套面要逐键过滤"当待办跟踪。** 落选：待办属工作项，本记录只登记"当前实现与一条被拒路径"，避免把任务状态写进 Agent Note。

## 后果

- 这条边界今天**不可达**；将来真开放嵌套面时，受理前的逐键过滤必须先落地，否则容器内未声明的键会原样上行。
- 发布期那条拒绝只覆盖"Adapter 不支持容器内的某个键"，不覆盖"容器内没有任何键"——两者不能互相顶替。
- 受理前的过滤规则仍归 ADR-0018；本记录不复制它的措辞。

## 验证

- `node scripts/decisions/check.mjs`：通过。状态事实的出处是当前源码与发布素材（见元数据的 `verification`），可用 `crates/adapter-sdk/src/lib.rs` 的 `supported_extra_parameters`、`crates/application/src/lib.rs` 的 `validate_adapter_compatibility`、`crates/domain/src/image_parameters.rs` 的顶层过滤与 `config/bootstrap/` 的两份素材逐处复核。
- 未跑 Rust 门禁：本件只改文档与记录，不触碰生产代码。
