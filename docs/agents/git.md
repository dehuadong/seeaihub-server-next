# 提交、推送与落地

本文件规定改动怎么变成提交、怎么推上去，以及历史能不能改写。验证命令本身在根 [`AGENTS.md`](../../AGENTS.md) 的「验证」，这里只管**跑哪些、什么时候跑、推之前核对什么**。

## 按改动面选证据

- 每处改动选择能挡住回归的**最小**证据：Rust 改动运行相关 crate 的定向用例，传输或协议改动补对客端到端；Agent Note 运行 `node scripts/decisions/check.mjs`；其他代理文档检查本地链接、运行 `git diff --check` 并按写作规则审阅。
- **Rust 改动按层选命令**（后两条**串行**跑，本机依赖与库怎么起见 [`../operations/development.md`](../operations/development.md) §2／§7）：

  ```sh
  cargo check -p <crate>                                     # 反馈：只做类型与借用检查
  cargo test -p <crate> --lib                                # 进程内单元用例
  HTTP_CONTRACT_DATABASE_URL=postgres://seeai:seeai@127.0.0.1:5432/seeai_contract \
    cargo test -p seeai-persistence -- --ignored --test-threads=1
  HTTP_CONTRACT_DATABASE_URL=postgres://seeai:seeai@127.0.0.1:5432/seeai_contract \
    cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1 <过滤词>
  npm --prefix apps/web run e2e                              # 浏览器层（自己拉起空库、产物与 API）
  ```

- **"单元层全绿"不是验收证据**：需要真实数据库、真实 Redis 或独立子进程的用例都带 `#[ignore]`，普通 `cargo test` 只编译不执行它们。声称交付前，按上面的层拿到对得上验收条件的证据（见技能 `verify`）。
- **要提交或推送，不构成把已经通过的检查再跑一遍的理由。** 根 `AGENTS.md` 里的三条命令是完整 Rust 门禁，按改动面需要时运行，不是每次提交的仪式。
- **只报告实际跑过的命令**：没跑就写没跑，不用「应该没问题」代替证据。
- **Rust 全量门禁由 CI 承担**，划分是三份流水线：[`ci.yml`](../../.github/workflows/ci.yml) 的 rust job 依次跑格式、Clippy（`-D warnings`）、Workspace 测试与两条 `--ignored` 层（契约、端口），web-e2e job 跑浏览器层（构建产物 + 真实后端）；[`docs.yml`](../../.github/workflows/docs.yml) 单独跑 Agent Note 与记录里的链接检查。纯文档路径被 `ci.yml` 的 `paths-ignore` 排除，所以文档、记录与技能改动**只有** docs 流水线在管，本地仍按上一条验证。本地重复跑 Rust 全量只用于用户明确要求、排查 CI 失败，或改动确实横跨整个仓库。

## 提交

- 一次提交只讲一件事：独立改动拆开，别把顺手整理与行为改动混在一起。
- 发现前一次提交引入的问题，先修那次提交或明确说明，再往下传播。
- 提交标题使用 `<type>: <一句话>`，`type` 取 `feat` / `fix` / `docs` / `test` / `chore`；正文说清改了什么、为什么，与已批准的设计或决定有偏差时写明偏差；最后一段用 `验证：` 列出**实际跑过**的证据（命令、用例名、结果）。引用遵循 [`docs/AGENTS.md`](../AGENTS.md) 的「合同与记录在仓库内闭环」与 [`domain.md`](domain.md) 的「引用写法」。
- 文件尾只留一个换行；提交前用 `git diff --cached --check` 查空白错误。本机 `core.autocrlf` 会把仓库里的 LF 报成「LF will be replaced by CRLF」——那是本机配置的提示，不是文件有问题，不要把 LF 改成 CRLF 去消掉它。

## 推送与历史改写

本仓库默认直接提交并推送 `main`，不要求通过 PR；操作前仍核对当前分支及其上游。下面的安全要求与是否经过 PR 无关。

- 普通推送：确认本改动面已有有效证据 → `git push` → 分别运行 `git rev-parse HEAD` 与 `git rev-parse '@{upstream}'`，核对两个 OID 一致。
- 只有用户明确要求改写历史时才执行 rebase 或强制推送。强制推送只允许 `--force-with-lease=<branch>:<观察到的远端 OID>`：推送前先取远端并记下当时的 OID，远端一旦被别人推进，这次推送必须中止并重新看一遍。**禁止裸 `--force`**，也不要在 lease 不成立时换别的写法绕过去。
- 改写之后，改写前那次推送的证据都不再是当前证据：重新取远端 head，重新核对检查结论；若这次改动走了 PR，还要重新核对评审线程、批准与可合并状态。
- 若当前环境限制 SSH 或网络访问，`git push` / `git fetch` 失败后如实报告，不绕过安全限制，也不把失败说成成功。
- 本文件不要求安装 Git 钩子。真要装也只装窄的、能当场修的事（暂存区 lint 与空白、提交信息格式）；测试、构建与文档门禁留给本地按需运行与 CI。钩子一宽就会有人 `--no-verify` 绕过去，等于没有。
