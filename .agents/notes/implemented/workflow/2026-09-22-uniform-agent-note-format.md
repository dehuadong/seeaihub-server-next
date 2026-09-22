---
title: 统一 Agent Note 的生命周期与文件格式
status: implemented
created: 2026-09-22
updated: 2026-09-22
approval: 用户在本会话中要求参考上游 deepseek-harness 的文档治理改进本仓库的生命周期（proposed / implemented / rejected）、文件格式规则与写作规则，并授权必要时借鉴其技能与脚本；属仓库文档治理，不启动工程流程
verification: 改造后 `node scripts/decisions/check.mjs` 通过（零报错，15 条既有记录与同批新增记录全部合规）；15 条既有记录的非标题正文行与改造前逐行比对，除 8 处故意的章节引用同步（改成新章节名）外缺失行 0；门禁检出能力用变异实测确认——删掉 `## 备选方案`、把标题行换回旧的裸标题、把 `## 决定` 换成 `## 提案`，检查分别报出对应错误，恢复后文件逐字节回到原状
---

# Agent Note：统一 Agent Note 的生命周期与文件格式

## 问题

本仓库的 Agent Note 只有一份松散的约定（[README](../../README.md)）：路径编码了生命周期与分类，正文「优先采用项目已有格式，脚本不固定章节标题」。15 条记录的写法因此各随其便：同一件事在不同记录里叫 `## 实际交付`、`## 决定与落地`、`## 决定与出处`、`## 方案与范围`，验证章节叫 `## 验证`、`## 验证结果` 或 `## 验证证据`；九条 implemented 记录完全没有「考虑过但落选的方案」，而这份信息正是日后防止同一场争论重来的东西；已交付的记录里还混着提案期措辞。

作者能依靠的只有照抄相邻文件，而相邻文件的写法未必一致；proposed → implemented 的迁移也容易只改 `status` 而不改写正文，因为没有任何门禁要求改写。写作规则（怎么写、哪些不该写）同样没有单一出处，散在根 `AGENTS.md` 的一条引用规则与各篇记录的自觉里；文件格式除 README 之外还有一份可复制的模板，同一套骨架写了两遍。

## 决定

- **生命周期**（[README § 生命周期](../../README.md#生命周期)）：写清三态的进入与离开条件。移动生命周期是一次内容改写而不是改字段——proposed → implemented 要把 `## 提案` 改写成现在时的 `## 决定`，把 `## 验收条件` 与 `## 风险` 折进 `## 后果`（或折进 `## 验证`），计划与迁移步骤换成实际交付的事实；proposed → rejected 只把否决结论写进 `status` 与 `reason` 并冻结正文；被完全取代的记录可以并入持有该决定的记录并删除，前提是当前记录已收下旧记录独有的理由、备选方案、影响与验证，且入站链接修好。
- **文件格式**（[README § 文件格式](../../README.md#文件格式)）：元数据块只允许 `title` / `status` / `created` / `updated` / `approval` / `verification` / `reason` 这套键并按此顺序，其中 `verification` 只属于 implemented、`reason` 只属于 rejected；标题行固定为 `# Agent Note：<title>` 且与 `title` 字段逐字一致；正文第一个章节固定为 `## 问题`；各生命周期的必需章节是 proposed 的 `## 提案`/`## 备选方案`/`## 验收条件`/`## 风险`、implemented 的 `## 决定`/`## 备选方案`/`## 后果`/`## 验证`、rejected 的 `## 备选方案`（保留提案期章节结构）；implemented 不得出现 `## 提案`、`## 计划`、`## 迁移计划`、`## 验收条件`。独特的技术章节仍可自由插在固定章节之间。
- **备选方案必须写**：真实考虑过并落选的方案与原因。格式建立日（2026-09-22，含）之前创建、且无法从既有内容还原备选方案的记录，用一条逐字一致的豁免注释顶替；该注释对之后的记录无效。
- **写作规则只有一个家**：[`docs/AGENTS.md`](../../../../docs/AGENTS.md) 承载「一个事实一个家」「写当前状态」「保留完整命题」「注释与 doc comment 陈述完整的合同，不是推理记录」「用能指向具体对象的词」「不写状态标注」等规则与 slop 清单，并按位置给出细则（模块头与公开项 doc comment、其他注释、测试、README 与 `docs/` 正文、Agent Note、技能与指令、对客串与诊断）与一次审查的走法；它同时管 Markdown 正文与 `crates/`、`apps/` 的注释。根 [`AGENTS.md`](../../../../AGENTS.md) 留入口，[`.agents/notes/README.md`](../../README.md) 与 [`.agents/notes/AGENTS.md`](../../AGENTS.md) 只指向它，不另列条文。
- **门禁**：[`scripts/decisions/check.mjs`](../../../../scripts/decisions/check.mjs) 除既有的目录、元数据与链接检查外，还查元数据键集合与键序、生命周期必填与禁用字段、标题行、首个章节、规范章节与禁用章节、备选方案或豁免注释；报错信息是中文，便于作者照单改。
- **子树指令**在 [`.agents/notes/AGENTS.md`](../../AGENTS.md)：新建前先搜同主题记录、`implemented/` 记录与实际交付保持同步（只改事实不改决定）、记录不是决策正文的家、正文写法指向 `docs/AGENTS.md`。**不留可复制的模板**：元数据块、标题行与正文骨架由 README 拥有、由门禁强制，抄写件就是同一条规则的第二个家。
- **语料库与新格式一致**：15 条既有记录全部按新格式改写，不设过渡期，也不容忍两套格式并存。

门禁只管机械可判的部分：批准是否属实、交付是否充分、备选方案与替代关系是否属实，仍由审阅判断。

## 备选方案

- **保留 YAML 元数据块，而不是学上游只留一行 `Status:`**：保留。本仓库的 `approval` 与 `verification` 是「批准不等于执行授权」和「已交付必须有验证证据」这两条规则的机器可检载体；换成上游式头部就得把 15 条记录里大段字段搬进正文，还要重建等效检查，换来的只是形式上的对齐。
- **只写约定、不上门禁**：不采纳。改造前的 15 条记录已经证明光靠自觉走不到统一格式——同一件事有四种章节名。
- **允许双格式过渡期，旧记录凭日期豁免格式**：不采纳。两套格式长期并存会让「照抄相邻文件」继续有效，而那正是要消除的失败模式；豁免只留给一种内容（备选方案无法还原时的注释），不留给格式。
- **把「未做 / 限制 / 风险」留给各自的自定义章节，不并入 `## 后果`**：不采纳。那样每篇的收尾内容在哪要读者自己猜；把 `## 后果` 定义成「换来了什么、付了什么代价、留下哪些限制与未决事项」之后，一篇记录只有一个收尾章节。
- **搬上游的 `archived/` 生命周期与归档技能**：不采纳。本仓库记录量小，README 原有的「被取代仍留在 implemented、写清失效范围、双向链接」已经够用；多一层生命周期就多一处要维护的状态与配对。
- **搬上游的文档技能（`dsh-doc` / `dsh-prose-standard`）**：不采纳。那两份技能承载的是上游的 i18n 配对、生成目录、字数预算与 JSDoc 门禁，本仓库没有这些对象；能机械化的部分已经进了门禁，编辑判断写进 `docs/AGENTS.md`——再放一份技能等于给同一条规则找第二个家。
- **让 9 条缺失「备选方案」的记录补写**：不采纳。那会编造原本没讨论过的方案；改用豁免注释，把缺口如实留在记录里。
- **保留一份可复制的模板**（`templates/record.md`）：不采纳。模板里的元数据块、标题行与正文骨架就是 README § 文件格式的第二次抄写，改一次要改两处；而门禁会直接报出缺哪个章节，照 README 写比照模板改更不容易漏。

## 后果

- 每条记录多了几行固定结构。强制的 `## 备选方案` 是有意留的阻力：记录决定击败了什么，才能防止同一场争论重来。
- proposed → implemented 现在必须当场改写正文（提案 → 决定、验收条件与风险 → 后果或验证），不能再只改 `status`。
- 9 条既有记录带着「备选方案未记录」的豁免注释。那是记录下来的缺口，不是补编的内容，也不构成「当时没有备选方案」的证据。
- 写作规则有了唯一出处 `docs/AGENTS.md`：根 `AGENTS.md`、[`.agents/notes/README.md`](../../README.md) 与 [`.agents/notes/AGENTS.md`](../../AGENTS.md) 都指向它，不再另存一份。
- 个别记录的章节名在改造中失去了原来的括注（如 `## 实际交付（四个切片）`、`## 已知限制与未决项（不在本次交付范围）`），因为门禁要求章节名逐字等于规范名；这类限定语归正文。
- 门禁仍是提醒而不是把关人：它不能判断批准是否真实、交付证据是否充分、备选方案是否属实。

## 验证

- `node scripts/decisions/check.mjs` 在改造后通过，零报错：目录、元数据键集合与键序、生命周期必填与禁用字段、标题行、首个章节、规范章节、备选方案与本地链接全部合规。
- **正文没有被改造改掉**：脚本把 15 条既有记录的非标题正文行与改造前逐行比对（多集包含关系），除下面 8 处**故意的章节引用同步**外缺失行 0——`2026-09-19-multi-offering-routing-and-apimart-driver` 2 处（「实际交付」→「决定」、「未决项」→「后果」）、`2026-09-22-pricing-floor-and-settlement` 5 处（含元数据 `verification` 字段 1 处，「风险与未决事项」→「后果」）、`proposed/workflow/2026-09-19-unauthorized-paid-provider-probe` 1 处（「风险与未决事项」→「风险」）。
- 每条记录只多出新增章节与豁免注释：**6 条新写了 `## 问题`**（`2026-09-19-initial-image-generation-vertical-slice`、`2026-09-19-multi-offering-routing-and-apimart-driver`、`2026-09-20-apimart-failure-narrowing`、`2026-09-20-consumer-facing-provider-error-rewrite`、`2026-09-20-flat-request-body-and-asset-roles`、`2026-09-20-images-pass-through-without-asset-storage`），**9 条用豁免注释**顶替 `## 备选方案`，其余 6 条复用记录里已经写过的备选方案内容。
- **门禁有检出能力**（变异实测，不是推断）：在一条记录上临时删掉 `## 备选方案` → 报「缺少必需章节 `## 备选方案`」；把标题行换回旧的裸标题 → 报「标题行必须是 `# Agent Note：…`」；把 `## 决定` 换成 `## 提案` → 同时报「缺少必需章节 `## 决定`」与「implemented 记录不得出现提案期章节 `## 提案`」。三次都恢复后检查重新通过，文件逐字节回到原状。
- 改造只动了 Markdown 与 `scripts/decisions/*.mjs`，未触碰 Rust 代码，`cargo fmt` / `clippy` / `test` 门禁不受影响。

## 依据与关联

- 上游设计理由：[Agent Note 的统一受门禁约束的文件内格式](https://github.com/deepseek-ai/deepseek-harness/blob/master/.agents/notes/implemented/process/2026-07-05-uniform-agent-note-format.zh.md)、[无需生成索引即可发现 Agent Note](https://github.com/deepseek-ai/deepseek-harness/blob/master/.agents/notes/implemented/process/2026-07-19-remove-generated-agent-note-index.zh.md)、上游 `.agents/notes/README.zh.md` 与 `docs/AGENTS.md`（写作规则与 slop 清单的来源）。
- 生效格式：[`.agents/notes/README.md`](../../README.md) 的「生命周期」「文件格式」「写作规则」三节；子树指令 [`.agents/notes/AGENTS.md`](../../AGENTS.md)。
- 写作标准：[`docs/AGENTS.md`](../../../../docs/AGENTS.md)。
- 门禁：[`scripts/decisions/check.mjs`](../../../../scripts/decisions/check.mjs) 与 [`scripts/decisions/lib.mjs`](../../../../scripts/decisions/lib.mjs)。
- 登记与落点：[`scripts/decisions/README.md`](../../../../scripts/decisions/README.md)、[`docs/architecture.md`](../../../../docs/architecture.md) §6；写作标准的入口在根 [`AGENTS.md`](../../../../AGENTS.md) 的「文档与约定」表。
- 相邻记录：[取消生成式 Agent Notes 索引](./2026-09-22-remove-generated-agent-note-index.md)——同一轮治理改动取消集中索引，本记录补上文件内格式与写作规则。
