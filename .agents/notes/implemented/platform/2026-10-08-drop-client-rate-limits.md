---
title: 删掉客户请求限流：不做速率配额
status: implemented
created: 2026-10-08
updated: 2026-10-08
approval: 产品口径由用户定：2026-10-08 明确「还是把用户的限流取消吧，不搞速率配额」，并否掉了只删生成侧的第一版方案，要求两侧都删且不加替代；随后输入「批准，执行实现」授权实施。Plan Review 三轴 + 收敛轮、Implementation Review 两轴与收敛轮均通过。
verification: 2026-10-08 本地：`cargo fmt --check`；`cargo clippy --workspace --all-targets` 干净；`cargo check --workspace --all-targets`；真库契约 `cases_cache`（含新用例 `many_requests_in_one_window_are_all_accepted`）11/11、`cases_upload`（含 `many_uploads_in_a_row_are_all_accepted`）、`cases_auth_attempts` 全绿，三组连同 `cases_direct_execution`、`cases_identity` 共 85 passed；`cargo test -p seeai-application --lib` 176、`-p seeai-persistence --lib` 16。
---

# Agent Note：删掉客户请求限流：不做速率配额

## 问题

平台现在按**每分钟请求数**限制调用方，两处、各有独立命名空间：

- 生成侧：`GENERATION_RATE_LIMIT_REQUESTS_PER_WINDOW`（缺省 60）与 `GENERATION_RATE_LIMIT_WINDOW_MS`（缺省 60000），计数键 `rate_limit:<key_id>:<窗口序号>`，在**认证时**加一（生成端点与客户读自己账户那一处），超限回 `429 rate_limit_exceeded` + `Retry-After`；没有缓存时这一层本来就不生效（启动打一条 warn）。
- 上传侧：`UPLOAD_RATE_LIMIT_REQUESTS_PER_WINDOW` / `UPLOAD_RATE_LIMIT_WINDOW_MS`，同一形状、独立命名空间 `upload_rate_limit:`，由 `ImageUploadConfig.rate_limit` 承载。

用户要求：**不做速率配额，把这两处都删掉**，并且不加任何替代的速率限制。

## 决定

1. **两侧都删**：`GenerationRateLimit` 与上传那套限值类型、四个环境变量、`AccelerationService::consume_request_slot` 的限流分支与 `consume_upload_request_slot`、`IdentityService::authenticate` 里那次占名额、缓存键助手（`rate_limit_key` / `upload_rate_limit_key` / `consume_rate_limit_slot`）、以及"没有缓存时限流不生效"的启动 warn；`authenticate_identity` 与上传路径上"不占速率名额"的注释与内部文档链接（`[GenerationRateLimit]`、`[Self::consume_request_slot]`）同时清掉，不留指向已删项的引用。认证本身不变（仍然验密钥、仍然点查账户；它不读余额，余额是受理路径的事）。
2. **保留防撞库的失败尝试限制**（`AUTH_ATTEMPT_LIMIT_*`）：它是保护**公开鉴权端点**的安全机制，不是客户配额，与"每分钟调多少次生成"无关。`429 rate_limit_exceeded` 这个对客码因此仍然存在，只是只剩它用；`Retry-After` 的语义不变。
3. **不加替代**：过载仍由既有的**容量闸门**挡住——按模型的并发名额（#94／#99）、渠道全局未决上限、本机读取与执行容量。它们按账户与渠道判定、**不依赖缓存**，不足时回 `429 too_many_in_flight` / `429 upload_busy` / `503 platform_unavailable`，不会无界排队。
4. **缓存里少两个命名空间**：不再写 `rate_limit:<key_id>:…` 与 `upload_rate_limit:<key_id>:…`。余额快照、route 条目与鉴权失败计数照旧。老键会自己过期，不需要迁移，也不需要在启动时清理。
5. **对客文档同步**：`public-docs/http-errors.md` 与 `public-docs/uploads/images.md` 的 429 行去掉"请求过密"的口径，只留并发已满（`too_many_in_flight` / `upload_busy`）与公开鉴权端点的 `rate_limit_exceeded`；对客不再承诺"每个 API Key 每分钟 N 次"。
6. **属主同步**：Spec `0005` §3／§6／§8（受理前检查、Redis 降级那段、新增一条连发判据；`§8` 的 A 条目）、Spec `0007` §2.1／§5 错误表／A6、Spec `0004` §5 的 A11（把 `rate_limit_exceeded` 点名为失败尝试限制的码——它不属于客户速率配额）、design `0009` §3（"两个维度"改成一个：并发）、design `0021`（上传限值的正文、环境变量表与同步清单）、design `0016` §3（失败尝试限制那段的原语与前提叙述）、design `0017` §6 容量表（删掉"每 API Key 请求速率"那一行）、design `0008` §1／§7 的两处（"余额与限流的纯加速层"、"速率计数当未命中"）、`configuration.md`（生成侧与上传侧各两行，以及把缓存用途写成"余额与每 Key 速率计数"的那处）、`.env.example`、本机 `.env`。记录侧：三份 implemented 记录里提到限流的地方按"只标失效范围并互链"处理，不改写成相反结论——[Redis 加速层](../../implemented/platform/2026-09-22-redis-acceleration-layer.md)、[参考图上传](../../implemented/platform/2026-10-04-reference-image-upload.md)、[公开鉴权失败尝试限制](../../implemented/platform/2026-10-05-public-auth-attempt-limits.md)。

## 备选方案

- **保留限流但把缺省调大／默认关闭**：落选。用户要的是"不做速率配额"；一个默认关的旋钮仍然是速率配额，还会留在配置文档与对客承诺里。
- **只删生成侧、留下上传侧**：落选。第一版方案就是这样，被用户否掉——两处对客都是"请求过密就拒"，留一处等于没删。
- **改成运营按账户配速率**：落选。同一句需求否掉了这一层；运营要控量有并发名额与余额两道闸门。
- **保留代码、只把计数改成恒不超限**：落选。死代码 + 一个永不生效的配置项，比删掉更容易在下次改动里复活。
- **用队列代替拒绝**（超限排队等）：落选。那会把"快速失败"变成"无界等待"，且与"不做速率配额"无关，属另一个产品决定。

## 后果

- **过载保护弱一层**：速率限制是"每个调用方最多多快"，删掉之后只剩"同时在跑多少"与"渠道/本机容量"。容量闸门不依赖缓存、按账户与渠道判定，因此**不会**出现"缓存挂了就没人挡"的局面；但一个账号可以在名额内尽可能快地反复调用，直到占满并发名额或渠道容量。
- **账户读取端点靠连接数兜底**：`GET /v1/account`（客户读自己的余额与账户）此前也吃这一层的计数，删掉之后它没有自己的并发名额或读名额——同一把有效密钥可以连续打它，每次两下数据库读（点查密钥 + 读余额）。上限只剩连接池（`API_MAX_CONNECTIONS`）与数据库容量。用户的要求是"完全不搞速率配额"，因此这里**有意不另加界**；真出现异常调用，处理方式是吊销密钥，需要更硬的界时再作为一个新的决定提出。
- **泄漏密钥的代价变大**：以前连续调用会在每分钟的额度上被挡住，现在只受并发与容量限制。止住它的手段是**吊销密钥**（#94 之前已有）与容量闸门；这条代价已向用户说明并接受。
- **对客语义变化**：`http-errors.md` 里"请求过密"的说法必须同步，否则对客文档承诺了不再存在的限制。
- **用例面**：`cases_cache.rs` 与 `cases_upload.rs` 里各有一条断言限流 429 的用例要删或改写；`cases_auth_attempts.rs` 那条**保留不动**（它验证的是防撞库）。

## 验证

| 交付事实 | 证据（真库 + 真进程，2026-10-08） |
| --- | --- |
| 同一把 API Key 连发不再被拒 | `cases_cache::many_requests_in_one_window_are_all_accepted`：61 次全部被受理、61 个 Job、整段 <60 秒；真库通过 |
| 上传同理 | `cases_upload::many_uploads_in_a_row_are_all_accepted`：带缓存连发 61 张全 200；真库通过 |
| 防撞库不变 | `cases_auth_attempts` 全组不改、继续通过（含无缓存时不生效那条） |
| 配置面与代码清干净 | **生产代码、环境变量读取与配置文档**里不再有 `GENERATION_RATE_LIMIT_*` / `UPLOAD_RATE_LIMIT_*`、`GenerationRateLimit`（生成与上传共用同一个类型）、两个 `consume_*` 方法与两个缓存键助手（历史记录里作为史实出现的名字不算）；`configuration.md` 与 `.env.example` 对应四行删除；启动日志不再有"限流不生效"的 warn |
| 夹具与相邻用例收口 | `harness.rs` 的 `ApiProcessSettings.rate_limit`／`ApiRateLimit`／`with_cache_and_rate_limit`／上传夹具的限流字段与它们的 env 写入删掉；`crates/application/src/image_upload/tests.rs` 里的限流用例删掉 |
| 容量闸门仍在 | `cases_direct_execution`、`cases_identity` 等相邻组继续通过（连同本次改动的三组共 85 passed）；`cargo clippy --workspace --all-targets` 与 `cargo check --workspace --all-targets` 干净；`cargo test -p seeai-application --lib` 176、`-p seeai-persistence --lib` 16 |

另有一次组内运行出现过 `the_reconciler_overwrites_corrupted_entries_from_the_database` 失败，单独重跑与整组重跑均通过，判为偶发（与本变更无关的时序用例）。
