# 交接：并发名额的规划收尾与落地（工作项 #99）（2026-10-08）

面向下一个会话的交接。**决定、理由、验收与证据都在仓库里**（工作项、Agent Note、Spec、提交），这里只写"去哪看"和"下一步做什么"，不重复抄正文。本份按用户要求**提交**（早期几份"不提交"的约定不适用于本份）。

## 1. 现在在哪一步

| 工作项 | 状态 | 说明 |
| --- | --- | --- |
| [#94](https://github.com/dehuadong/seeaihub-server-next/issues/94) | 已关闭（complete） | 并发名额按「账户 × 网关模型」判定、落在模型行、编辑路径（模型卡片 `PATCH`）能设。提交 `71ef330`；设计记录 [网关模型的并发名额](../notes/implemented/platform/2026-10-08-gateway-model-concurrency-quota.md)。 |
| [#99](https://github.com/dehuadong/seeaihub-server-next/issues/99) | **已获授权，实施中（2026-10-08）** | 上架时也能设名额；**删掉 `GENERATION_MAX_CONCURRENT_JOBS`**；列改 `NOT NULL`（回填 1）。Spec `0001` v25 与 `0005` v7 已接受；设计记录 [并发名额在上架时给出](../notes/implemented/platform/2026-10-08-quota-set-at-publish.md) 随本次交付迁入 `implemented/`。 |
| [#97](https://github.com/dehuadong/seeaihub-server-next/issues/97) | **已获授权（与 #99 同一句），待实施** | 删掉客户请求限流（生成侧与上传侧两处），不做速率配额；它的 Spec/设计修订还没起草（实施前先在 Planning 里固化）。 |
| #95 / #96 | 已关闭（取消） | 速率配额、以及"标签即组名"的分组管理，都不做。 |
| [#91](https://github.com/dehuadong/seeaihub-server-next/issues/91) | 已被 `b733d82` 修掉 | 合同夹具不再读仓库根 `.env`；可以直接关。 |

## 2. 下一步（建议顺序）

1. **拿执行授权**：只有用户明确输入「批准，执行实现」才算授权。**不要从"需要解决""继续""你看着办"里推断**——本会话在这个点上被用户纠正过两次。
2. **接受 #99 的两份 Spec 草稿**（现在 `生效修订` 未动、修订行标"待评审／无"）：接受＝进位 `生效修订`、修订行改"已接受／vN"，并同步工作项的决策记录。
3. **过 Implementation Gate**（`docs/agents/engineering.md` §2），加载 `implement` 技能，按 #99 的「实施步骤」与设计记录的「决定」落地。**动手前先把设计记录里「同步到属主的改动」清单做完**，其中包含 `docs/operations/deployment.md` §3.3 与它的两份衍生（`production.md`、`production-docker.md`）——它们现在写着"升级＝先部署后观察、没有单独的迁移步骤"，照做会把容量静默降到 1。
4. 实现完成 → `code-review`（Standards／Spec 两轴）→ `verify`（真库 + 真浏览器）→ 提交推送 → 关 #99。
5. #97 是另一件工作：等用户授权后从 Planning 起步（它的方案在第一轮里已被用户改过两次，见 issue 正文）。

## 3. 关键文件（下一步会碰到的）

- 工作项与决策记录：[#99](https://github.com/dehuadong/seeaihub-server-next/issues/99)（范围、验收、三轮 Plan Review 的发现与修正都在正文）。
- 设计：[proposed Note](../notes/implemented/platform/2026-10-08-quota-set-at-publish.md)（写入形状、无窗口 RUNBOOK、审计口径、边界与类型收紧、验证切入点）。
- 合同草稿：[`docs/specs/0001-admin-and-customer-consoles.md`](../../docs/specs/0001-admin-and-customer-consoles.md)（M1／M2／V-D17／V-D18）、[`docs/specs/0005-synchronous-image-gateway.md`](../../docs/specs/0005-synchronous-image-gateway.md)（§6）。
- 实施会改到：`migrations/`（**新迁移**：回填 1 → `SET NOT NULL` → 重写列注释；已应用的 `0048` 不许改）、`crates/persistence/src/lib.rs`（发布事务与设置接口）、`crates/application/src/lib.rs`（`DirectExecutionLimits`／`ActiveOfferings`／发布命令）、`apps/api/src/main.rs`（env 读取、`AppState`、读面、`PATCH`）、`apps/web/src/console/*`（发布抽屉、改价态、卡片文案、`client.ts`）、`apps/api/tests/http_contract/harness.rs`（并发形参改走发布命令）。
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
