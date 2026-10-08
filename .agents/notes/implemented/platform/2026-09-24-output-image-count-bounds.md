---
title: 输出张数按合同声明的取值面校验
status: implemented
created: 2026-09-24
updated: 2026-09-24
approval: 用户 2026-09-24 裁定：受理时按**合同**为 `n` 声明的取值面校验，且不得假定某个平台级的数（各型号声明的界不同）。
verification: 端到端一条（`a_requested_image_count_beyond_the_contracts_maximum_is_rejected_before_acceptance`）+ 模块单测三条（`crates/application/src/tests/contract_face.rs`）；`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`、`cargo test --workspace --all-features` 全绿；空库端到端 107 passed / 0 failed；`node scripts/decisions/check.mjs` 通过
---

# Agent Note：输出张数按合同声明的取值面校验

## 问题

受理时请求按**合同**校验只判了"必填在场、字段名属于合同"（`ADR-0018` 的 2026-09-20 修订），取值一律不判。对大多数参数这是有意的：上游按自己的 schema 处置取值，平台替它判等于把"上游认得的取值"变成平台要维护的清单。但**输出张数 `n` 不是普通参数**：这一次请求的**超时窗口**按张数推导，它同时又是发给上游的"要几张"。于是合同写 `maximum: 10` 时，调用方给 `n = 100` 会被原样接受：平台按 10 张算超时窗口，上游却可能真的生成 100 张。

`n` 的界**不是一个平台级的数**：`config/bootstrap/gpt-image-2.5-{flare,sunburst}.json` 的顶层合同声明 `maximum: 10`，AIHubMix 那条候选的承载面也是 `10`，而 **APIMart 那条候选的承载面是 `4`**。

## 决定

- 受理时按**合同自己**为 `n` 声明的 `type` / `minimum` / `maximum` 校验调用方给的值：超界或类型不符 ⇒ `ApplicationError::InvalidParameter` ⇒ 对客 **400 `invalid_parameter`**，不建 Job、不扣款。值是调用方给的，改法也在他那一侧——这是**参数问题**，不是平台侧故障。
- **判据只取自合同**：合同没声明 `n`、或没声明 `type` 与上下界时**不判**——把一条不存在的条款变成对客错误，比放过它更糟。
- `3.0` 算整数（JSON Schema 的 `integer` 就是"没有小数部分"），不按类型严格拒绝一个合法取值。
- **别的参数的取值仍然不判**（枚举、区间、类型、未知字段的既有口径不变）：它们不影响平台这一侧算账，判它们只是替上游维护一份清单。

## 备选方案

- **校验所有声明过的参数的取值**（用 `jsonschema` 整份校验请求）：落选。那会把平台变成上游 schema 的执行者，且对客错误会从"能改的一处"变成"照着上游文档猜"；`n` 之所以要判，是因为平台自己要用它算账，不是因为"该有个校验器"。
- **只判 `maximum` 不判类型与 `minimum`**：落选。`n = "100"` 或 `n = 2.5` 同样绕过超时链（平台按 1 张算超时、上游按 100 张生成），正是这道校验要堵的洞；`minimum` 与类型是同一份声明里的东西，一起判才自洽。
- **把界做成平台配置**（一个全局最大张数）：落选。同一个模型不同候选的界就不同（10 与 4），全局数必然要么放宽到没用、要么把合法请求拒掉。
- **超出**该候选**承载面声明的界**时判该候选承载不了**（换候选，全不行即 503）：落选（本件不做）。那是**选路行为变更**：合同是调用方的界面，承载面是供给的能力面，今天承载校验只判"字段名在不在"，不判取值。该处置后来另行裁定：见[输出张数按候选承载面的上界收敛](./2026-09-24-output-image-count-cap-on-carrier-surface.md)。
- **越界返回平台侧故障 503**：落选。值是他给的，平台侧故障会把"改一下参数"说成"平台坏了"。

## 后果

- 越界的请求在受理之前就结束：没有 Job、没有预授权、没有上游调用；对客拿到的是 `400 invalid_parameter` 加一条说明上界的消息（例如 `n for model gpt-image-2.5-flare must be at most 10, got 11`）。
- 既有用例里那条"声明过的参数取值不校验"的证据换了个参数继续成立（`quality: 42` 不在它声明的 `enum` 里也照原样留下），`n` 成了这条规则的**唯一例外**，例外本身有用例。
- 别的参数（`seed`、`extra` 之类）仍然放行透传：平台不因为这次改动扩大取值校验的面。

## 验证

| 行为 | 证据 |
| --- | --- |
| 合同声明上界 ⇒ 超过它的请求 400 `invalid_parameter`、不建 Job、不扣款；恰好等于上界放行 | `a_requested_image_count_beyond_the_contracts_maximum_is_rejected_before_acceptance`（`apps/api/tests/http_contract/cases_parameters.rs`，用**真素材**的顶层合同） |
| 越界、低于下界、非整数分别被拒且报错说清界与实得值；恰好落在两端放行 | `a_requested_image_count_outside_the_contracts_bounds_is_rejected_as_a_parameter_problem`、`a_requested_image_count_that_is_not_an_integer_is_rejected`（`crates/application/src/tests/contract_face.rs`） |
| 合同没声明界（或没声明 `n`）时行为与今天一致；别的参数取值仍不判 | `a_contract_without_declared_bounds_leaves_the_value_alone`（同文件）、`parameters_the_contract_never_declared_are_dropped_without_an_error`（`crates/application/src/tests/request_preparation.rs`） |
| 正常请求逐位不受影响（回归） | 空库端到端全量 107 passed / 0 failed；`cargo test --workspace --all-features` |
| 门禁 | `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`、`node scripts/decisions/check.mjs` |

## 依据与关联

- 参数开启依据与"取值不校验"的原口径、以及本件作为第一个取值例外，见 [`ADR-0018`](../../../../docs/adr/0018-open-parameters-by-first-party-docs.md) 的 2026-09-24 修订段。
- 受理期校验规则清单（R4/R5）见 [`docs/design/0005`](../../../../docs/design/0005-vendor-model-contract-and-offering-mapping.md) §4。
- 拿 `n` 当输入的地方：超时链在 `crates/application/src/request_timeout.rs`（模块头有完整口径）。原先的成本护栏已删除，见[受理没有金额型护栏](./2026-10-09-no-amount-based-admission-guards.md)。
