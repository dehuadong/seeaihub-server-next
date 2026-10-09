---
name: rust-build-budget
description: 处理 Rust 的构建预算——编译或测试慢、target/ 越来越大、想 cargo clean、CI 反复全量编译，或者单元测试跑绿了却说不清验到了哪一层时用它。顺序是先量、再改配置、再改习惯、清的时候只清该清的。
---

# Rust 构建预算

时间与磁盘是同一笔预算：一次全量编译的耗时、`target/` 的体积，以及"跑绿了到底验到哪一层"。这份技能给一套顺序：**先量，再改**。

具体命令、环境变量与门禁的划分在自己的 `AGENTS.md` 与 CI 配置里；这里只写可迁移的做法。

## 1. 先量，别猜

四个数，同一台机器、同一分支，一条条记下来：

```sh
du -sh target target/*          # 体积构成：先看最大的是哪一项
cargo clean && time cargo build # 冷全量（换成你仓库里那条"全量"命令）
time cargo build                # 无改动重跑：这就是增量基线
touch <被广泛依赖的 crate>/src/lib.rs && time cargo build   # 改一处之后的真实体感
```

`target/debug/` 内部通常是 `deps/`（每个依赖、每个测试二进制各一份）和 `incremental/`（增量缓存）两块最大。`cargo build --timings` 出一份每 crate 的编译耗时报告，用来找最慢的那几个。

**完成判据**：能说出这次慢在哪一层、`target/` 里最大的是哪一项，四个数都记下来了。

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

**完成判据**：改完重量第 1 步，冷全量耗时与 `target/` 体积都有前后对比（体积通常降一个数量级）。

## 3. 改习惯：只编这次要验的东西

- 反馈回路用 `cargo check -p <crate>`（不生成代码，最快）；要跑用例才 `cargo test`。
- 只编要验的目标：`cargo test -p <crate> --lib`，或 `cargo test -p <crate> --test <其中一个测试目标> <过滤词>`。
- `--workspace --all-targets` 会把每个 crate 的每个测试二进制都编一遍：那是 CI 合并前门禁的事。
- CI 里加构建缓存（如 `Swatinem/rust-cache`），并设 `CARGO_INCREMENTAL=0`：CI 很少重编同一个 crate，增量缓存只占缓存空间。
- 把"几秒钟就能跑完的格式、文档、记录检查"放进**独立的**流水线：它们不该被全量编译拖着跑，也不该因为路径过滤在纯文档改动上完全不跑。

**`#[ignore]` 那一层**：需要真实数据库、真实 Redis 或独立子进程的用例通常带 `#[ignore]`，普通 `cargo test` 只**编译**不执行——输出里那些 `0 passed` 的测试二进制就是它们，而验收证据往往正在那一层。跑它们要显式 `--ignored`、给足前置条件，并且**串行**（`--test-threads=1`），因为共享的外部服务会让并发用例互相扰动。

**完成判据**：能说出"这次改动的最小证据是哪条命令"，并且说得清本地跑的那几条**没有**覆盖哪一层。

## 4. 清，而不是整清

`cargo clean` 删掉**整个 `target/`**（两套 profile、`incremental/`、构建脚本输出、每个测试二进制），下一次全量重编很慢——它是最后手段，不是日常手段。按代价从低到高：

```sh
rm -rf target/debug/incremental    # 只删增量缓存：可再生成，代价最小
cargo clean -p <crate>             # 只清一个 crate（依赖它的会跟着重编）
cargo clean --release              # 只清 release 产物
cargo install cargo-sweep && cargo sweep --time 30   # 按时间清掉很久没碰的产物
```

`CARGO_TARGET_DIR` 指到更大的盘能解决空间，但会丢掉跨分支的增量缓存；多份检出共用同一个目标目录还会互相打断。

**完成判据**：清完 `target/` 变小，且下一次重编仍是增量的（无改动重跑回到秒级）。

## 可选加速（要往机器上装东西）

| 手段 | 买什么 | 要什么 |
| --- | --- | --- |
| `lld` 或 `mold` 当链接器 | 链接常快 2–4 倍 | 装链接器，并在 `.cargo/config.toml` 配 `-C link-arg=-fuse-ld=lld` |
| `sccache` | 跨 `cargo clean`、跨检出复用编译产物 | 装 sccache 并设 `RUSTC_WRAPPER=sccache` |
| `cargo-nextest` | 更快的测试运行、更清楚的失败输出 | 装 nextest 跑 `cargo nextest run`；需要串行的组仍要单线程 |

装之前先看第 1 步的量：时间如果花在**编译**而不是链接上，换链接器几乎看不出来。
