# 真实计费调用的留档

- **用途**：记录本仓库**每一次真实渠道计费调用**——日期、授权依据、端点、次数、花费、样本位置与结清了什么。
- **性质**：本仓库自己的证据记录，不是渠道事实，也不是平台合同。**渠道事实**（端点、参数、计量、错误码、成本口径）归纳在 [`docs/facts/channel-facts.md`](../facts/channel-facts.md)；原始样本在 `out-reference/<provider>/`。
- **纪律**：凭证只从环境变量读取，不入库；不保存真实图片 URL 与 task id。
- **引用约定**：本文写成 `§x.y` 的引用指 [`docs/facts/channel-facts.md`](../facts/channel-facts.md) 的对应小节；本文内部引用写"本文 §x"。
- **发新调用的入口**：`scripts/probe/response-shapes.ps1`（本仓库**唯一**会发真实计费调用的入口，默认只演练，必须显式加 `-ConfirmPaidCalls`）；未经批准不发起任何计费调用。

## 1. 异步实测（2026-09-19，经用户授权）

| 项 | 值 |
| --- | --- |
| 渠道 / 端点 | AIHubMix `POST https://api.inferera.com/ai/v1/images/generations`（异步） |
| 授权 | 用户 2026-09-19 明确指示「AIHubMix 异步实测一个试试」 |
| 计费调用次数 | **2 次提交**：第 1 次因 `quality` 顶层不合法被拒（HTTP 400，**未受理**）；第 2 次受理并完成 |
| 请求参数 | `model=gpt-image-2`、`n=1`、`size` 未传、`async=true`、`quality` 未传 |
| 结果 | `status: completed`，约 12 秒 |
| 未做 | 未下载结果图（仅读响应结构）；未测带图编辑 |

## 2. 同步实测（2026-09-19，经用户授权）

| 项 | 值 |
| --- | --- |
| 渠道 / 端点 | AIHubMix `POST https://api.inferera.com/v1/images/generations`（同步） |
| 授权 | 用户 2026-09-19 明确指示「可以同步实测下」 |
| 调用次数 | **2 次**（`gpt-image-2.5-sunburst`、`gpt-image-2.5-flare`），除 `model` 外参数相同 |
| 请求参数 | `n=1`、`size=1024x1024`、`quality=low`、`output_format=png` |
| 结果 | 两次均 **HTTP 200**；sunburst 245,138 bytes / 19.8 秒，flare 321,146 bytes / 14.7 秒；四分项 `usage` 齐全且逐项相同（见 §2.6） |
| 计费 | 每次按四档费率算得 **5950 microusd = $0.005950**（上游未返回金额，需自行计算） |
| 未做 | 未把结果图写入仓库（`b64_json` 仅看长度与前缀）；未测带图编辑 |

## 3. APIMart 受控实测（2026-09-19，经用户授权）

| 项 | 值 |
| --- | --- |
| 渠道 / 端点 | APIMart `POST https://api.apib.ai/v1/images/generations`（异步）+ `GET /v1/tasks/{id}` |
| 授权 | 用户 2026-09-19 提供 `APIMART_API_KEY` 并授权决定后续 |
| 计费调用次数 | **提交 1 次**（另 1 次轮询为只读） |
| 请求参数 | `model=gpt-image-2.5-flare`、`n=1`、`size=1:1`、`resolution=1k`、`quality=low` |
| 结果 | 提交 HTTP 200（`status: submitted`）；10 秒后终态 `completed`，`actual_time=7` |
| 计量 | **四分项 `usage`**：输入文本 14 / 输入图片 0 / 输出图片 196 / total 210（另含 `cached_tokens`） |
| 计费 / 成本 | 上游自报 **`cost = 0.00476 USD`**（这就是本笔成本价）、`credits_cost = 0.0476`；公开费率算得 `0.00595`，差额是上游面板自报的 `Group ratio 0.8`（§5.4） |
| 未做 | 未下载结果图（URL 已脱敏）；未测 `sunburst`；未测 `image_urls` 图生图 |
| 原始记录 | `out-reference/apimart/controlled-probe-2026-09-19.json` |

## 4. 累计

2026-09-19 经用户授权的计费提交共 **7 次**（AIHubMix 4 次 + APIMart 3 次）：

| # | 渠道 / 端点 | 结果 | 是否必要 |
| --- | --- | --- | --- |
| 1 | AIHubMix `/ai/v1`（异步） | HTTP 400 **未受理**（顶层 `quality` 非法） | 顺带确认了参数位置 |
| 2 | AIHubMix `/ai/v1`（异步） | 完成 | **重复验证**——第一阶段 §13.1 已有三场景更完整的同类结论 |
| 3 | AIHubMix `/v1`（同步，sunburst） | 完成 | **必要**（2.5 首次付费验证） |
| 4 | AIHubMix `/v1`（同步，flare） | 完成 | **必要**（补上 flare 缺口） |
| 5 | APIMart `/v1`（异步，flare） | 完成 | **必要**（结清 V1：`usage` 是否存在与粒度） |
| 6 | APIMart `/v1`（异步，flare，**带参考图 + 遮罩**） | 完成 | **必要**（结清图生图合同：`image_urls` 形态、`mask_url`、`input_image_tokens`） |
| 7 | 同上，但**走我们自己的 API + Worker** | **`succeeded`** | **必要**（第一次让真实凭证跑通自家全链路：上传 → 生成 → 取图 → 归档 → 结算） |

第 **1–4 次（AIHubMix）**：三次完成（其中 1 次是重复验证）+ 1 次未受理（$0）。两次同步 2.5 各按已发布单价算得 **$0.005950**（上游不返回金额，只能自算）；异步那次上游同样不返回金额，**金额未知**。

第 **5–7 次（APIMart）**：上游三次都有折后实际扣费可对——**$0.004760**（纯文生图，本文 §3）+ **$0.011390**（直连图生图）+ **$0.011374**（走自家服务），合计 **$0.027524**。

可核对总额 ≈ **$0.0394**（AIHubMix 2 次 + APIMart 3 次），另加 1 次金额未知的 AIHubMix 异步调用。全部使用 `n=1`、`quality=low` 的最小配置。**未发起任何火山方舟调用。** 另完成 3 次 APIMart **只读**探测（§3.5）与 4 次**无凭证**路由探测（本文 §5），零费用。

**成本价来源（两渠道不同）**：**AIHubMix** 上游只给四分项 token，成本价按四档费率自算（§2.4）；**APIMart** 上游在响应里直接声明 `cost`，那就是成本价（§5.1）。**平台对外价未定**（后期，[#5](https://github.com/dehuadong/seeaihub-server-next/issues/5)）。逐笔见 §5。

## 5. 零费用路由探测（2026-09-19，**无凭证**）

| 项 | 值 |
| --- | --- |
| 目的 | 确认我们**实际配置的域名**上三个端点真的存在（此前只有文档写着） |
| 方法 | 对 `https://api.apib.ai` 发**不带任何凭证**的请求，只看状态码；不触达任何账号 |
| 次数 | **4 次**（`POST /v1/uploads/images`、`POST /v1/images/generations`、`GET /v1/tasks/nonexistent`、`GET /v1/nonexistent-route` 对照） |
| 结果 | 三个真实端点均 **401**（存在但需鉴权）；对照的不存在路由 **404** ⇒ 路由判定有效 |
| 计费 | **零**（无凭证、未生成、未上传任何文件） |
| 附带事实 | 401 错误信封与失败路径的请求标识（见 §3.8） |
| 未做 | **没有带凭证调用上传接口**，也没有任何生成调用 |

## 6. 图生图 + 遮罩受控实测（2026-09-19，经用户批准）

| 项 | 值 |
| --- | --- |
| 授权 | 用户 2026-09-19 在本会话明确选择「批准，按这个方案跑」；方案当时写明：上传 1~2 张测试图 + **最多 2 次生成**、`n=1`、`quality=low`、预算上限 $1 |
| 实际用量 | **上传 2 次**（512×512 参考图、512×512 带 alpha 遮罩）+ **生成 2 次**（1 次 curl 直连探合同、1 次走我们自己的 API+Worker） |
| 生成参数 | `model=gpt-image-2.5-flare`、`n=1`、`size=1:1`、`resolution=1k`、`quality=low`、`image_urls=[<上传后的 URL>]`、`mask_url=<上传后的 URL>` |
| 直接调用结果 | 上传 HTTP 200（字段与文档一致）；提交 HTTP 200（`status: submitted`）；11 秒后 `completed`，`cost = 0.01139 USD` |
| 直接调用的计量 | `input_tokens=1057`（`image_tokens=1024`、`text_tokens=33`）、`output_tokens=196`（`image_tokens=196`）、`total=1253` |
| **走我们自己服务的端到端** | 发布真实素材 → 平台接口上传两张图 → 受理 Job（`/image_urls/0` + `/mask_url`）→ 真实 Worker 执行 → **`succeeded`** |
| 端到端计量 | Evidence 记 `input_text=29 / input_image=1024 / output_image=196`（与上游 `usage` 逐项一致） |
| **成本价** | 上游自报 **`cost`**：直连那次 `$0.011390`、走自家服务那次 `$0.011374`（面板的 `Actual cost`，逐笔见 §5.4） |
| 平台侧结算与成本的关系 | capture `14217 microusd` = 面板 `Base cost`（= 公开费率 × 分项 token）；成本价 = 它 × 账号倍率 0.8。差额来自账号倍率，**不是**计量误差（§5.4） |
| 端到端结果 | 结果图归档到自有对象存储：`image/png`、**1,486,934 bytes、1024×1024**；`provider_trace_id` 已落库（真实 task id） |
| 面板核对（**已结清**） | 面板写明 `Base cost = Σ(token × 费率)`、`Actual = Base × Group ratio 0.8`，两次与 `cost` 逐位一致（§5.4） |
| 敏感信息 | **未保存**真实图片 URL 与 task id；上表只记存在性、主机名与长度级信息。原始响应只留在本机临时目录，不入仓库。用户后来提供的上游账单面板含 task id 与密钥标签，**截图同样不入库**，只把结算数字转录进 §5.4 |
| 未做 | 未下载上游结果图（结果图是我们自己服务完成取图后归档的）；未测 `sunburst`；未测 base64；未压测 20MB/16 张/256MB 边界 |

**这一轮调用同时结清了 [`docs/verification/phase2-controlled-verification.md`](./phase2-controlled-verification.md) 里列的四条**（上传返回、`image_urls` 形态、图生图可用 + `usage` 变化、`mask_url` 可用），因此两个发布素材据此放开两条分支。

**转录已落盘**：`out-reference/apimart/transcript-image-edit-2026-09-19.json`——上传 2 次、提交、4 次轮询、终态（含四分项 `usage` 与 `cost`/`credits_cost`）、401 错误信封、以及平台侧那次的 Evidence 与面板数字。**这是转录（字段名与取值照当时输出记录），不是逐字报文**：逐字报文当时写在临时目录，收尾时被删除——这层缺口如实记在 `out-reference/apimart/response-shapes.md` §0。

## 7. 端点复核（2026-09-20，经用户授权）

| 项 | 值 |
| --- | --- |
| 授权 | 用户 2026-09-20 指示「你现在实测下，看看结果如何」 |
| 端点 / 次数 | AIHubMix `/ai/v1/images/generations`（不带 `async`）1 次 + `/v1/images/generations` 1 次；各 `gpt-image-2.5-flare`、`n=1`、`size=1024x1024`、`quality=low` |
| 结果 | 两次均 HTTP 200；结论见 §2.6/§2.7（**按用户要求未留样本文件**） |
| 计费 | 2 次，自算各约 $0.00595，合计约 **$0.012** |

**累计（截至 2026-09-20）**：AIHubMix 6 次 + APIMart 3 次 = **9 次计费提交**，可核对金额 ≈ **$0.0514**（另 1 次 AIHubMix 异步金额未知）。
