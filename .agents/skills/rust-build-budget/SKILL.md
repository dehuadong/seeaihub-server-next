---
name: rust-build-budget
description: Rust 构建的时间与磁盘预算：编译慢、target/ 越来越大甚至把盘占满、想 cargo clean、CI 反复全量编译、单元测试全绿就想交付，或者要把这些命令与环境变量落进项目的 AGENTS.md 与 CI 时用它。
---

# Rust 构建预算

一次全量编译的耗时、`target/` 的体积，以及"跑绿了到底验到哪一层"，是同一笔预算的三面。做法固定：**先量，再改**。

这份技能只写可迁移的做法；**具体命令、环境变量与门禁划分属于目标项目自己**，落在它的 `AGENTS.md`（或 `CLAUDE.md`）、开发文档与 CI 工作流里。所以开工先读它们，收工把缺的补回去（第 5 步）。

## 1. 先读项目的约定，再量

先读目标项目的 `AGENTS.md`、CI 工作流与它自己的开发文档：本地跑哪几条、验收层是哪几条命令、要哪些服务与环境变量、门禁怎么分工。**记下缺哪一块**——第 5 步要补上。

然后量三段时间加一份体积构成，同一台机器、同一分支：

```sh
du -sh target target/*                                   # 体积构成：最大的是哪一项
CARGO_TARGET_DIR=$(mktemp -d) time cargo build           # 冷全量：不毁掉现有缓存
time cargo build                                         # 无改动重跑：增量基线
touch <被广泛依赖的 crate>/src/lib.rs && time cargo build  # 改一处之后的真实体感
```

`target/debug/` 里通常 `deps/`（每个依赖、每个测试二进制各一份）与 `incremental/`（增量缓存）最大。慢就看 `cargo build --timings`：它按 crate 给编译耗时，用来找最慢的那几个。

**完成判据**：说得出这个项目已有的命令、环境变量与门禁划分，也列得出缺哪一块；三段时间与体积构成都记下来了。

## 2. 改配置：调试信息是磁盘的主要来源

默认的开发 profile 给**每个依赖**都留完整调试信息（`debug = 2`），多数项目的 `target/` 主要就是它。仓库根 `Cargo.toml`：

```toml
[profile.dev]
debug = "line-tables-only"   # 自己的 crate 只留"文件:行号"：panic 回溯够用

[profile.dev.package."*"]
debug = false                # 依赖不留调试信息（"*" 不含本工作区的成员）

[profile.release]
strip = true                 # 发布产物更小；要留着符号做线上调试就配 split-debuginfo
```

代价只有一条：panic 落在**依赖内部**时回溯会变薄。要钻进依赖单步调试，临时去掉这两段重编一次。

**完成判据**：改完只需重量 `target/` 的体积与一次无改动重跑——两者都有前后对比，就够判断这笔改动值不值。

## 3. 改习惯：只编这次要验的东西

- 反馈回路用 `cargo check -p <crate>`；要跑用例才 `cargo test`。
- 只编要验的目标：`cargo test -p <crate> --lib`，或 `cargo test -p <crate> --test <测试目标> <过滤词>`。先确认那个目标里真有测试——用例放在 `tests/` 的仓库，`--lib` 会跑 0 条，正是下面那个陷阱的样子。
- `--all-features` 会把所有 feature 组合并进来编，不是免费的；先确认这次改动真的需要它。
- 全量门禁（`--workspace --all-targets` 那一类）按仓库自己的约定跑：它会把每个 crate 的每个测试二进制都编一遍。

**`#[ignore]` 那一层**：需要真实数据库、真实 Redis 或独立子进程的用例通常带 `#[ignore]`，普通 `cargo test` 只**编译**不执行——输出里那些 `0 passed` 的测试二进制就是它们，而验收证据往往正在那一层。跑它们要显式 `--ignored`（nextest 是 `--run-ignored only`）、给足前置条件，并且**串行**（`--test-threads=1`），因为共享的外部服务会让并发用例互相扰动。

**完成判据**：能说出"这次改动的最小证据是哪条命令"，并且说得清本地跑的那几条**没有**覆盖哪一层。

## 4. 清，而不是整清

`cargo clean` 删掉**整个 `target/`**（两套 profile、`incremental/`、构建脚本输出、每个测试二进制），下一次全量重编很慢——它是最后手段。按你实际编过什么挑最小的一刀：

```sh
rm -rf target/debug/incremental   # 只删增量缓存：可再生成，代价最小
cargo clean -p <crate>            # 只清一个 crate，依赖它的会跟着重编
cargo clean --release             # 只清 release 产物，代价取决于你编过多少 release
cargo sweep --time 30 --dry-run   # 按时间清很久没碰的产物；先 dry-run（该工具上游已不常维护）
```

`CARGO_TARGET_DIR` 指到更大的盘能解决空间，但会丢掉跨分支的增量缓存；多份检出共用同一个目标目录还会互相打断。CI 的产物用缓存 action 管，不进本机目录。

**完成判据**：只删 `incremental/` 之后，下一次改动重编仍是增量的（无改动重跑回到秒级）。`cargo clean -p` 与整清之后本来就要重编，别拿它们当"清完还很快"的判据。

## 5. 把结论写回项目

量出来的数、定下来的命令与门禁划分**属于这个项目**，写进它自己的属主文件——技能里不复制。逐项核对：

- **命令与环境变量**：反馈（`cargo check -p <crate>`）、单元、验收层（`--ignored`，含它需要的服务与环境变量、必须串行的原因）、浏览器层，各写清"什么时候跑哪一条"。
- **验收层的判据**：写明"单元层全绿不是验收证据"，以及验收层是哪几条命令——否则下一个会话还会拿绿色单元测试当交付。
- **门禁划分**：本地跑什么、CI 跑什么、CI 里哪份工作流管哪一层、路径过滤会让哪些改动触发不到它（纯文档改动尤其容易漏）。
- **构建预算**：`Cargo.toml` 的 profile 改动留一句"为什么"（依赖不留调试信息、代价是什么）；CI 开构建缓存并关掉增量。
- **清理纪律**：写清"别整清、先删 `incremental/` 或 `clean -p`"，以及本机 `target/` 归谁管。

放在项目已有的属主文件里（开发文档、构建文档、`AGENTS.md` 的验证段）：命令行能直接复制，验收判据能逐条核对，写"现在是什么"而不是"这次改了什么"。项目没有这个属主时，就地写在 `AGENTS.md` 的验证段里就够；独立文档只在内容撑得起时才开。

**完成判据**：在目标项目的属主文件与 CI 里，上面五项各能查到；查不到的那项要么补上，要么在结论里说明这个项目不需要它。

## 可选加速（要往机器上装东西）

| 手段 | 买什么 | 要什么 |
| --- | --- | --- |
| `lld` 或 `mold` 当链接器 | 链接常快 2–4 倍 | 装链接器，并在 `.cargo/config.toml` 的 `[target.<triple>]` 里配 `rustflags = ["-C", "link-arg=-fuse-ld=lld"]` |
| `sccache` | 跨 `cargo clean`、跨检出复用编译产物 | 装 sccache 并设 `RUSTC_WRAPPER=sccache` |
| `cargo-nextest` | 更快的测试运行、更清楚的失败输出 | 装 nextest 跑 `cargo nextest run`；需要串行的组仍要单线程，忽略用例用 `--run-ignored only` |

装之前先看第 1 步的量：时间如果花在**编译**而不是链接上，换链接器几乎看不出来。
