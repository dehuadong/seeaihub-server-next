---
name: rust-build-budget
description: Rust 构建的时间与磁盘预算：编译慢、target/ 越来越大甚至把盘占满、想 cargo clean、CI 反复全量编译，或者单元测试全绿就想交付时用它。
---

# Rust 构建预算

一次全量编译的耗时、`target/` 的体积，以及"跑绿了到底验到哪一层"，是同一笔预算的三面。做法固定：**先量，再改**。

命令、环境变量与门禁怎么划分，按仓库自己的 `AGENTS.md` 与 CI 配置；这里只写可迁移的部分。

## 1. 先量

三段时间加一份体积构成，同一台机器、同一分支：

```sh
du -sh target target/*                                   # 体积构成：最大的是哪一项
CARGO_TARGET_DIR=$(mktemp -d) time cargo build           # 冷全量：不毁掉现有缓存
time cargo build                                         # 无改动重跑：增量基线
touch <被广泛依赖的 crate>/src/lib.rs && time cargo build  # 改一处之后的真实体感
```

`target/debug/` 里通常 `deps/`（每个依赖、每个测试二进制各一份）与 `incremental/`（增量缓存）最大。慢就看 `cargo build --timings`：它按 crate 给编译耗时，用来找最慢的那几个。

**完成判据**：能说出这次慢在哪一层、`target/` 里最大的是哪一项，三段时间与体积构成都记下来了。

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

## 可选加速（要往机器上装东西）

| 手段 | 买什么 | 要什么 |
| --- | --- | --- |
| `lld` 或 `mold` 当链接器 | 链接常快 2–4 倍 | 装链接器，并在 `.cargo/config.toml` 的 `[target.<triple>]` 里配 `rustflags = ["-C", "link-arg=-fuse-ld=lld"]` |
| `sccache` | 跨 `cargo clean`、跨检出复用编译产物 | 装 sccache 并设 `RUSTC_WRAPPER=sccache` |
| `cargo-nextest` | 更快的测试运行、更清楚的失败输出 | 装 nextest 跑 `cargo nextest run`；需要串行的组仍要单线程，忽略用例用 `--run-ignored only` |

装之前先看第 1 步的量：时间如果花在**编译**而不是链接上，换链接器几乎看不出来。
