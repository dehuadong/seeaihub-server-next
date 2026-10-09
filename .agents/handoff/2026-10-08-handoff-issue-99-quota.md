# 交接：并发名额的规划收尾与落地（工作项 #99）（2026-10-08）

面向下一个会话的交接。**决定、理由、验收与证据都在仓库里**（工作项、Agent Note、Spec、提交），这里只写"去哪看"和"下一步做什么"，不重复抄正文。本份按用户要求**提交**（早期几份"不提交"的约定不适用于本份）。

## 1. 现在在哪一步

| 工作项 | 状态 | 说明 |
| --- | --- | --- |
| [#94](https://github.com/dehuadong/seeaihub-server-next/issues/94) | 已关闭（complete） | 并发名额按「账户 × 网关模型」判定、落在模型行、编辑路径（模型卡片 `PATCH`）能设。提交 `71ef330`；设计记录 [网关模型的并发名额](../notes/implemented/platform/2026-10-08-gateway-model-concurrency-quota.md)。 |
| [#99](https://github.com/dehuadong/seeaihub-server-next/issues/99) | **已交付并验证、已关闭**（提交 `bd2c26b`） | 上架时也能设名额；删掉 `GENERATION_MAX_CONCURRENT_JOBS`；列改 `NOT NULL`（回填 1）。Spec `0001` v25 与 `0005` v7 已接受；设计记录与验证表见 [并发名额在上架时给出](../notes/implemented/platform/2026-10-08-quota-set-at-publish.md)。 |
| [#97](https://github.com/dehuadong/seeaihub-server-next/issues/97) | **已获授权（与 #99 同一句），待实施** | 删掉客户请求限流（生成侧与上传侧两处），不做速率配额；它的 Spec/设计修订还没起草（实施前先在 Planning 里固化）。 |
| #95 / #96 | 已关闭（取消） | 速率配额、以及"标签即组名"的分组管理，都不做。 |
| [#91](https://github.com/dehuadong/seeaihub-server-next/issues/91) | 已被 `b733d82` 修掉 | 合同夹具不再读仓库根 `.env`；可以直接关。 |

## 2. 下一步（建议顺序）

1. **#99 已完成**（`bd2c26b`）：进程配置里不再有名额来源，名额在**上架或编辑**时由运营给。全貌读工作项 #99 与那份 implemented 记录。
2. **#97 的 Planning 已收敛，等执行授权**：方案是两侧（生成与上传）都删、不加替代；合同草稿在 Spec `0005` §3／§6／§8（当前 v8）、Spec `0007` §2.1／§5／A6（当前 v4）、Spec `0004` §5（当前 v2），设计在 design `0009` §3、`0021`（当前 v4）、`0016`、`0017` §6、`0008`，设计记录是 `.agents/notes/proposed/platform/2026-10-08-drop-client-rate-limits.md`。执行前先让用户接受这几份修订并给出执行授权。
3. #91 可直接关（已被 `b733d82` 修掉）。

## 3. 关键文件（下一步会碰到的）

- 工作项与决策记录：[#99](https://github.com/dehuadong/seeaihub-server-next/issues/99)（范围、验收、三轮 Plan Review 的发现与修正都在正文）。
- 设计：[proposed Note](../notes/implemented/platform/2026-10-08-quota-set-at-publish.md)（写入形状、无窗口 RUNBOOK、审计口径、边界与类型收紧、验证切入点）。
- 合同草稿：[`docs/specs/0001-admin-and-customer-consoles.md`](../../docs/specs/0001-admin-and-customer-consoles.md)（M1／M2／V-D17／V-D18）、[`docs/specs/0005-synchronous-image-gateway.md`](../../docs/specs/0005-synchronous-image-gateway.md)（§6）。
- #97 的规划要先读：`docs/specs/0007-*.md`（上传与素材）、`docs/specs/0005-*.md` §4、`docs/design/0009-operational-baseline.md` §3、`docs/design/0021-*.md`，以及 `crates/application/src/lib.rs` 的限流端口与 `apps/api/src/main.rs` 的两个环境变量。
- 实施会改到（#99 已完成，留作参考）：`migrations/`（**新迁移**：回填 1 → `SET NOT NULL` → 重写列注释；已应用的 `0048` 不许改）、`crates/persistence/src/lib.rs`（发布事务与设置接口）、`crates/application/src/lib.rs`（`DirectExecutionLimits`／`ActiveOfferings`／发布命令）、`apps/api/src/main.rs`（env 读取、`AppState`、读面、`PATCH`）、`apps/web/src/console/*`（发布抽屉、改价态、卡片文案、`client.ts`）、`apps/api/tests/http_contract/harness.rs`（并发形参改走发布命令）。
- 契约与运维文档：`docs/design/0006`（§2.2 读示例、§2.3/§2.4）、`docs/design/0012`（v4）、`docs/design/0017`（v3）、`docs/design/0009` §3、`CONTEXT.md`、`docs/operations/configuration.md`、`.env.example`、`docs/operations/deployment.md` §3.3 与两份衍生。

## 4. 本机环境与验证命令

- PostgreSQL 17 与 Redis 已就绪（连接串见仓库根 `.env` 的 `DATABASE_URL`，这里不抄凭据）。契约用例用 `seeai_contract`、浏览器用例用 `seeai_e2e`（`ensure-database.mjs` 会把它重置成空库）。
- 契约用例（真库 + 真进程，串行）：

  ```sh
  HTTP_CONTRACT_DATABASE_URL="$(grep -m1 '^DATABASE_URL=' .env | cut -d= -f2- | sed 's#/seeai_next#/seeai_contract#')" \
    cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1 <过滤词>
  ```

- 浏览器用例（一条命令自己拉起空库、构建、起 API）：`npx playwright test e2e/<spec>.spec.ts`（在 `apps/web` 下；交付判据是同一条命令连跑两次都绿）。
- 改动面静态检查：`cargo fmt --check`、`cargo clippy -p seeai-application -p seeai-persistence -p seeai-api --all-targets`、`cargo check --workspace --all-targets`、`npx tsc --noEmit`（`apps/web`）。Rust 全量门禁交给 CI，不在本地重跑。

## 5. 纪律与坑（本会话踩过的）

- **执行授权**必须由用户明确给；「确认／可以／同意」不算。开工前读工作项、沿引用链读 Spec/RFC（`AGENTS.md`「开工前读合同」）。
- **已应用的迁移不许就地改**（`sqlx::migrate!` 校验和）；列注释、`NOT NULL`、回填都要走新迁移。
- **同一个仓库有别的会话在推提交**（本会话期间出现过 `1`／`1`／`b733d82`／`1` 四个提交，其中一个改了 `AGENTS.md`）。提交时**逐个文件 `git add`**，复制别人的改动前先看 `git status`。
- **不要用长 `sleep` 阻塞等评审**：把评审子代理派出去就回话，结果到了再处理；规划评审按三轴并行（Standards／Spec／Architecture），实现评审按两轴，每轮后确认发现是否落地。
- **Agent Note 生命周期**：proposed → 交付并验证后才 `implemented`（并补 `verification` 字段、移动目录、修入站链接）；已交付记录不改写成相反结论，只标失效范围并互链（`.agents/notes/README.md`）。
- **术语**：`CONTEXT.md` 是唯一词汇表；"模型并发名额"由运营按模型给，不写成运行时缺省。
- **汇报风格**：简体中文、简明、不抛技术选择题、不用黑话；实测优先于推测（用户反复强调）。

## 6. 建议加载的技能

- `planning`——接受两份 Spec 草稿、把合同固化到属主文档。
- `implement`——Implementation Gate 通过、写第一行实现代码之前。
- `code-review`——实现完成、报告完成或提交之前（两轴并行）。
- `verify`——审查通过、声称交付之前。
- `domain-modeling`——改 `CONTEXT.md` 的模型并发名额词条时。
- `writing-for-agents`——若要改 `AGENTS.md` 或技能文档。
