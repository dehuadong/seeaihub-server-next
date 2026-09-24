---
title: 输入参考资源与它的数量上限：命名收口
status: implemented
created: 2026-09-23
updated: 2026-09-23
approval: 用户 2026-09-23 明确：命名与术语属工程判断、由实施方按材料与设计定（不需要用户决策）；同时指出今天只有图片这一种输入参考资源，将来会有视频、音频以及与图片混合的输入，要求术语定义要留得住
verification: 2026-09-23 本地实际跑的：`cargo fmt --all` exit 0；`cargo clippy --workspace --all-targets --all-features -- -D warnings` exit 0；`cargo test --workspace --all-features` exit 0。端到端 `cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1` **尚未验证**：并行的超时链改动未自洽，API 子进程启动即退出（stderr：`configuration error: GENERATION_SYNC_WAIT_SECONDS (...) is below PROVIDER_TIMEOUT_SECONDS (360s)`），用例停在 "API did not become ready"，"两份素材仍能发布"因此没有端到端证据，待那件自洽后重跑。`node scripts/decisions/check.mjs` 通过。
---

# Agent Note：输入参考资源与它的数量上限：命名收口

## 问题

`max_images` 在这套代码里同时表示过三件不同的事，而其中两件的方向正好相反：

- `AdapterDescriptor.max_images` 与 `restrictions.max_images`：**输入参考图**的张数上限（adapter 能发几张、这条供给允许收几张，AIHubMix 与 APIMart 都声明 16）；
- 并行改动里一度出现的用法：拿它当**输出张数**的上限；
- 渠道侧同名但语义相反的字段：Doubao 的 `sequential_image_generation_options.max_images` 是**输出**张数。

代价已经真实发生过一次：推导上游超时（`超时 = 基础 + n × 每张预算` 那一类的公式）时照这个名字取数，把**输入图的 16** 当成了**输出张数**的上限。

## 决定

**输入**这一侧统一叫 **`Input Reference`（输入参考资源）**，今天的数量上限叫 **`max_reference_images`**——名字里带上"图片"，因为今天只做图片；**输出**张数只由合同声明的 **`n`** 表达，任何地方都不用 `max_images` 表示输出。

发布数据字段随之改名（`restrictions.max_images` → `restrictions.max_reference_images`），覆盖 adapter descriptor、发布期与受理期校验、校验文案、两份 `config/bootstrap/` 素材、全部用例与相关设计正文。**不同时兼容新旧两个键**：仓库尚未上线，发布数据只有我们那两份素材，留双读只会让两种含义长期共存。

**校验与报错文案点明方向**：说的是收图就写 reference image(s)、说的是输出就说是输出张数，不让读者从上下文猜是哪一个。

将来接入视频、音频以及它们与图片的**混合输入**时，这类上限**按资源种类**各自声明，**不复用**图片的名字；这条扩展点写在 `docs/design/0005` 的 R3 下（写成边界，不写成今天已有的能力）。渠道侧真叫 `max_images` 而我们不控制的字段保留**渠道原名**，只加注释点明方向相反（仓库当前没有 Doubao adapter，故暂无落点）。

## 备选方案

**保留 `max_images`，只靠注释说明它是输入。** 落选：这个名字已经误导过一次，而误用它的代价是超时算错（消费者被掐断而上游仍在计费）；注释救不了"名字本身看不出方向"。

**现在就做成按资源种类的映射**（`max_references: { "image": 16 }`）。落选：今天只做图片，凭空多一层种类映射是**为不存在的需求造抽象**；等真接视频/音频时再按种类扩展，那时名字与结构一起定。

**同时兼容新旧两个键，日后择机收敛。** 落选：双读期两套名字并存，正是这次要消除的混淆；而迁移成本此刻为零。

**把"输出张数上限"也起个名字**（例如 `max_output_images`）。落选：输出张数已经有名字——合同里的 `n`。再给它一个别名，等于把这次的混淆换个地方重演。

## 后果

- 发布数据的字段名变了（两份素材、发布期与受理期校验、校验文案、用例、`design/0005` 与 `design/0003` 的字段称呼）；**素材与校验必须同时改对**，否则发布会被直接拒——端到端那条"两份素材仍发布得出去"的用例正是这条验收。
- `CONTEXT.md` 有了 `Input Reference` 词条，并在 `Reference Image / Mask` 里点明"今天唯一一种输入参考资源"。
- 图片的名字不留给将来的资源种类：视频、音频与图片加它们的混合输入接入时，各自声明各自的上限，**不复用** `max_reference_images`；扩展点写在 `docs/design/0005` §4 的 R3 边界。
- 渠道侧真叫 `max_images` 而我们不控制的字段保留**渠道原名**（Doubao 的 `sequential_image_generation_options.max_images` 是输出语义）；仓库当前没有 Doubao adapter，故这个"保留原名并注明方向相反"暂无代码落点，将来写它时按输出张数对待。
- `docs/adr/0009` 里"输入图张数"的表述语义正确、不含旧字段名，未改。
- 并行改动里那处拿 `max_images` 当输出上限的用法，随它自己那件收敛（本件不碰别人的文件）。

## 验证

`cargo fmt --all`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`、`cargo test --workspace --all-features` 三条 exit 0；仓库（除 `out-reference/`）已无 `max_images` 残留。改名覆盖的用例都在这三条里：发布期"限制超出承载面承诺"的拒绝与收窄、受理期按供给声明收图、两份素材的发布构造与路由、Driver 声明的上限断言。

**端到端未验证**：本件完成时，并行的超时链改动让测试装置给出自相矛盾的 env（`PROVIDER_TIMEOUT_SECONDS` ≥ 360 秒配 2 秒/30 秒的对客窗口，被新校验拒绝），API 进程起不来，因此"两份素材仍能发布"这条**尚未实测**；待那件自洽后统一整跑。

## 依据与关联

- 术语：输入参考资源与它的数量上限见 [`CONTEXT.md`](../../../../CONTEXT.md) 的 `Input Reference`、`Reference Image / Mask` 两条。
- 字段落点：[`crates/adapter-sdk/src/lib.rs`](../../../../crates/adapter-sdk/src/lib.rs) 的 `AdapterDescriptor.max_reference_images`；发布期与受理期校验在 [`crates/application/src/lib.rs`](../../../../crates/application/src/lib.rs)；`restrictions` 与承载面的一致性规则、以及按资源种类扩展的 R3 边界见 [`docs/design/0005`](../../../../docs/design/0005-vendor-model-contract-and-offering-mapping.md) §4。
- 输出张数上限的来源：[`crates/application/src/declared_images.rs`](../../../../crates/application/src/declared_images.rs)（读合同 `n.maximum`）；它与超时链的关系归并行的那件改动。
- 渠道原名：[`out-reference/doubao/图片生成模型API调用指南.md`](../../../../out-reference/doubao/图片生成模型API调用指南.md)（原始材料，不随本件改动）；Doubao adapter 设计里对该字段的称呼见 [`docs/design/0003`](../../../../docs/design/0003-doubao-ark-image-adapter.md)。
- 素材：[`config/bootstrap/gpt-image-2.5-flare.json`](../../../../config/bootstrap/gpt-image-2.5-flare.json)、[`config/bootstrap/gpt-image-2.5-sunburst.json`](../../../../config/bootstrap/gpt-image-2.5-sunburst.json)。
