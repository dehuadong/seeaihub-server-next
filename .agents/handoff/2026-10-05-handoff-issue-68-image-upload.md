# 交接：图片上传端点与生成入口收敛（#68）

生成时间：2026-10-05。仓库 `/home/mypc/work/seeaihub-server-next`。

面向下一个会话。决定、理由、验收与证据都在仓库里，这里只写"去哪看"和"下一步做什么"，不重复抄。

## 当前状态

- 规划已走完四轮 Plan Review，终轮判定**通过**，明确写出"规划集合可以进入实施门禁"；逐轮结论与处置都在工作项评论里。
- 合同已生效：生成入口按 Spec 0005 v3；上传端点按 Spec 0007 v2（用户已接受，`生效修订 v2`）。
- 设计已接受：Design 0021 的状态头已记设计评审结论。
- 工作项 #68 标签 `proposal:ready`。
- 最新提交 `537401e`，`main` 与 `origin/main` 同步。
- **实现一行未写**：尚未取得执行授权。

## 下一步（先拿授权，再动手）

1. 用户明确说"执行实现"后才开始；这是新工作项，执行授权不从前面的整改继承。
2. 按 Design 0021 §9 的六片顺序实施，每片跑对应门禁后提交推送，中间不逐片请示：
   1. domain 纯规则（媒体类型与魔数、20 MiB 上限、对象键、失败分类）
   2. 对象存储适配器与签名（新 crate；OSS V4 header 模式；移植金标准向量对拍）
   3. 应用上传用例（配置装载、上传事务、端口与 DTO）
   4. HTTP 上传端点（路由级正文上限、平台错误信封、supervisor 第二套名额与独立预算）
   5. 生成入口收敛（只收公网 URL；删 `InputImage::DataUrl`/`from_raw` 与 APIMart 内联上传；改 `data:image` 约 82 处、跨 22 个文件，含被点名的四个测试文件）
   6. 文档与配置收口（代码结构图、配置文档、清理收敛前的代码注释）
3. 实施时的两条硬要求：把 `out-reference/oss-v4-signing-golden.md` 的向量移植成用例并**逐字节复验**设计侧自算的生产 PUT 期望值；第 4 片必须**真发**一个整体合法、请求体超过全局 16 MiB 的 multipart 请求，证明路由级正文上限覆盖生效。
4. 实现完成后走 Implementation Review（两轴）与最终验证，再报交付。

## 合同与关键路径（读这些，不要从对话里猜）

| 要什么 | 在哪 |
| --- | --- |
| 范围、切片、批准依据 | 工作项 `#68`（含四轮评审评论） |
| 生成入口的输入形态与验收 | [`docs/specs/0005-synchronous-image-gateway.md`](../../docs/specs/0005-synchronous-image-gateway.md) |
| 上传端点合同 | [`docs/specs/0007-image-upload-and-object-storage.md`](../../docs/specs/0007-image-upload-and-object-storage.md) |
| 技术设计与实施顺序 | [`docs/design/0021-object-storage-upload.md`](../../docs/design/0021-object-storage-upload.md) |
| 持久决定 | [`docs/adr/0022`](../../docs/adr/0022-reference-image-upload-endpoint.md)（限定 [`0019`](../../docs/adr/0019-images-pass-through-without-asset-storage.md) 的适用范围） |
| 签名金标准向量 | [`out-reference/oss-v4-signing-golden.md`](../../out-reference/oss-v4-signing-golden.md) |
| 部署侧自检清单 | [`docs/verification/object-storage-upload.md`](../../docs/verification/object-storage-upload.md) |
| 本次变更独有的理由与备选 | [`.agents/notes/proposed/platform/2026-10-04-reference-image-upload.md`](../notes/proposed/platform/2026-10-04-reference-image-upload.md) |

参考实现（仓库外，权威样本）：`/mnt/d/workspace/seeaihub/src/service/src/uploads/`（含 `signing/`、`runtime.rs`、`transaction.rs`、`health.rs`）与 `/mnt/d/workspace/seeaihub/src/service/tests/uploads_signing_golden.rs`。

## 这个会话定下的工作方式（继续照做）

- **参考实现是对拍基准**：机制与签名向量以它为准。别从官方文档片段重新推导——官方页的 SK 是占位符，公布的派生值与签名**不可复现**，这一点已实测确认并写进设计。
- **文档写当前状态**：README、`CONTEXT.md`、`docs/architecture.md`、根 `AGENTS.md`、代码注释、`docs/facts/`、`docs/operations/` 只描述代码事实；尚未实现的收敛合同只放 Spec 与设计，并注明"随实现落地"。
- **一个事实一个家**：合同判据与失败后果只在 Spec 陈述，其他位置用链接；别在运维文档里重述合同。
- **评审结论要记进工作项**，不能只留在提交信息里。
- 提交与推送按 [`docs/agents/git.md`](../../docs/agents/git.md)：按改动面留证据后直接提交并推送 `main`，不走 PR。

## 已知记录在案、未做的事（都不阻塞）

- `docs/operations/production.md` 的 nginx 示例当前仍是生成入口的 `16m`；上传端点的 `24m` 要求写在同页要点里，实施落地时同步。
- `docs/architecture.md` §1 基础设施框图漏了真实存在的 `crates/alert-webhook`（既有缺口，不属本次范围）。
- 上传端点的管理端页面、存储表、激活动作、保留期与删除、批量上传都在 Spec 0007 §1 的排除项里；桶的保留期与删除归对象存储控制台。
- 参考实现要求私有桶 + 预签名读，本设计相反（桶匿名可读、不做预签名读取），差异记在 Design 0021 §8。

## 环境与常用命令

- `cargo` 不在默认 PATH：用 `PATH="$HOME/.cargo/bin:$PATH" cargo ...`。
- 真库用例：`HTTP_CONTRACT_DATABASE_URL` 取 `.env` 的 `DATABASE_URL`（别打印它）；串行 `--test-threads=1`；改迁移或 persistence 后先 `touch crates/persistence/src/lib.rs`。
- 文档检查：`node scripts/decisions/check.mjs`；改文档后另跑相对链接与章节引用检查、`git diff --check`。
- 本目录的交接文档属工作区文档，不提交。

## 建议加载的技能

- `implement`：拿到执行授权、写第一行实现代码之前。
- `tdd`：实现期做测试优先时（签名向量、上传事务与端到端用例尤其适用）。
- `code-review`：实现完成、报告完成或提交之前（两轴并行）。
- `verify`：声称交付（PASS/完成）之前。
- `planning`：实施中发现合同缺口或要改 Spec 时。
- `adr`：需要再动持久决定时。
