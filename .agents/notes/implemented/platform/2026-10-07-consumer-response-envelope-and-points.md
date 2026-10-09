---
title: 对客响应信封与积分单位
status: implemented
created: 2026-10-07
updated: 2026-10-07
approval: 用户批准规划集并给出执行实现授权（2026-10-07）；Spec 0005 v5 与 Spec 0002 v5 待接受
verification: cargo fmt/clippy/test、真库端到端 16 模块 181 passed、Playwright 85 passed 连跑两次、文档检查通过
---

# Agent Note：对客响应信封与积分单位

## 问题

对客成功响应此前只有 `{created, data:[{url|b64_json}]}`：调用方拿不到这次调用的标识、状态与实付金额。对客金额也只有 CNY 微单位一种表示，客户看到的是 1e-6 元的整数。

## 决定

**对客响应信封是平台自己的形状，与渠道无关。** 形状与 APIMart 任务对象的成功响应一致：字段集由平台定义，取图值与过期时刻按渠道事实填入，渠道差异留在 Adapter 内部。

- `id`：该次执行的 Generation Job 标识（UUID）。对客响应与对客用量记录用同一条标识，客户能对上账。
- `status`：同步成功为 `completed`。对客状态词表统一为 `completed` / `failed` / `pending` / `canceled`，用量记录里的 `succeeded` 改名。
- `cost`：该笔实收的**正数金额**，单位是积分；用量与账单记录里的同一笔按既有约定带符号（扣费为负）。
- `result.images[]`：一项 = 渠道的一张图，`url` 项保留该图全部地址（字符串数组），`b64_json` 项是内联串；产出张数与按张计费都按项计，不按地址个数计。`expires_at` 只在该渠道为这一项给出过期时刻时出现，平台不推算。
- 对客响应类型与 Adapter 的内部类型分开：Adapter 的内部结果类型承载“地址列表 + 渠道给的过期时刻”或“内联 base64”，对客 wire 类型归 `apps/api`。
- 成功件带上该笔实收，随内存成功事实一起交回 `apps/api`，不读库、不重放。

**积分是对客金额单位，单位与取整合同归 [Spec 0002 §1、§2、§4](../../../../docs/specs/0002-account-funds-and-reservations.md)。** 换算落在两处：算定边界把实收与保底额向上取整后落账，读边界再向上取整一次——账上的金额本来恒为整积分，读侧那次是兜底，非整积分的历史或异常行会被取整并记一条错误，不静默截断、也不在请求路径里 panic。内部账本、费率与计价形态不动。

合同由 [Spec 0005 §1、§3](../../../../docs/specs/0005-synchronous-image-gateway.md) 与 [Spec 0002 §1、§2、§4、§5](../../../../docs/specs/0002-account-funds-and-reservations.md) 拥有；术语见 [GLOSSARY.md](../../../../GLOSSARY.md) 的 Result Envelope、Generation Job 与 Consumer Point。

## 备选方案

- 保持 OpenAI 形状 `{created, data[]}`：调用方拿不到调用标识与实付金额，不采用。
- 给对客单独造一个与 Job 解耦的公开标识：多一列、多一处映射，而对客账本已经在回 Job 标识，收益为零，不采用。
- `cost` 用元的小数：违反“钱与汇率都不走浮点”，且与对客账户读的整数表示分叉，不采用。
- `cost` 用 CNY 微单位整数：能对上内部账本，但把 1e-6 元的内部刻度交给客户，与积分方向相反，不采用。
- 积分比例取 1元 = 100积分或 1元 = 1000000积分：前者把最小粒度压到 0.01元、便宜调用抬价明显，后者客户看到六位数积分；取 1元 = 1000积分是两者的折中，采用。
- 平台给不报过期时刻的渠道推算一个有效期：会在客户那里承诺一个平台控制不了的失效时刻，不采用。
- 只在对客展示层换算、账本与接口仍用微单位：客户余额是积分、接口是微单位，两套单位同时存在于对客面，不采用。

## 后果

对客 `id` 就是内部 Job 标识：公开契约与内部主键绑定，日后换标识格式要改对客合同；无生产数据，现在承担的代价最小。

积分粒度是 0.001元：不足 1 积分的部分向上取整，便宜调用相对抬价；这是用户选定的粒度。

对客与管理端两种单位同时存在：文档与客户端都要写清各自单位，混用会算错钱。

读侧兜底的取整对负数按**绝对值**向上取整（与写侧同向），Spec 0002 §1 字面的“向上取整”是向 +∞；这条路径不可达（账上金额恒为整积分），日后若要求照字面，需在 Spec 里写明负数取向。

`expires_at` 是可选字段：调用方要按可选处理，不能假定一定有。

`adapter-apimart` 现在解析 `expires_at`、按项计数、不再摊平地址数组；同一图片项给出多个地址时产出张数与按张计费按项算，今天实测的渠道每个图片项只给一个地址。

正式调整（`adjustment`）还没有写入口；它一旦建立，必须同样在落账前取整到整积分。

## 验证

- `cargo fmt --check`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`、`cargo test --workspace --lib`、`cargo test -p seeai-api --bins`：通过。
- 真库端到端：`HTTP_CONTRACT_DATABASE_URL=… cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1`（16 个模块）→ 181 passed / 0 failed。对客信封由 `assert_sync_success`、`cases_apimart`（`url` 数组 + `expires_at`）、`cases_billing`（响应的 `id`/`cost` 与用量记录一致）、`cases_parameters`（请求 6、承载面 4、产出 1 → 200 且项数按实际产出）覆盖。
- 浏览器行为：`npm run e2e --prefix apps/web` → 85 passed，连跑两次绿；覆盖客户控制台余额、用量、账单与「积分」文案。
- `node scripts/decisions/check.mjs`：通过。
