# 第二阶段 · 受控验证清单

- **用途**：结清 `dehuadong/seeaihub-server-next#2` 规划 §5.1 的 V1–V6，从而决定「要不要扩展证据形态」，并使 Gate 的「no material unresolved decision」成立。
- **性质**：**本仓库自己的执行清单**（受控验证的步骤、停止条件与留档要求），**不是**上游或第三方材料。
- **位置说明（2026-09-19 从 `out-reference/` 迁入）**：本文原先放在 `out-reference/`，那是错的——`out-reference/` 的定义是「来自本仓库之外的上游或第三方材料」（见 `docs/agents/artifacts.md`「外部参考资源」），而本文是我们自己的执行计划。存放位置**不改变其权威性**：`#2` 的规划正文与本文必须同步，停止条件以规划 §8 第 2 项为准。

> ### 状态更新（2026-09-19）：**AIHubMix 侧已结清，下列多数条目已不需要执行**
>
> 用户 2026-09-19 授权对 AIHubMix 做了受控实测（同步 2 款 + 异步 1 次），结果见 `docs/facts/channel-facts.md` §2.6 与 §5。
>
> | 原编号 | 原拟验证 | 现状 |
> | --- | --- | --- |
> | V1 | APIMart 任务响应是否有分项 `usage` | **已结清**：一次经授权的受控实测拿到四分项 `usage`（`docs/facts/channel-facts.md` §3.3 / §5.3） |
> | V2 | APIMart `task_id` 路径与终态枚举 | **路径已结清**（`data[0].task_id`、`GET /v1/tasks/{id}`）；**终态取值集合未逐一实测**，按两份文档并集容忍未知值（§3.6） |
> | V3 | AIHubMix 2.5 的计量形态 | **已结清**：两款同步 `/v1` 均返回四分项 `usage`，与 `gpt-image-2` 同构 |
> | V4 | `Idempotency-Key` 是否生效 | **判定为不必测**：创建请求本就不重发，只影响优化项，不影响正确性 |
> | V5 | `n` / `quality` 的实际行为 | **已删除**：属上游生成行为，非平台合同事实 |
> | V6 | APIMart 机器 schema（只读） | **已结清**：`/v1/models/{model}/schema` 与 `/v1/models` 均已取到（§3.4/§3.5） |
>
> **本文当前没有待执行项。** 本阶段之后新出现的待验证项是**APIMart 的参考图/遮罩路径**（上传 + `image_urls`）：链路已实现，但**没有任何计费实测**，因此**不在本文已批准的范围内**。要执行需先由用户批准（`#2` 已关闭，应挂在后续工作项上）：调用清单就是下面 4 条，预算上限与停止条件沿用 §2 的既有条款。
>
> 已经**零费用**做完的部分：三个端点在我们配置的域名 `api.apib.ai` 上确实存在（无凭证 401 vs 对照路由 404），以及 401 的错误信封（`docs/facts/channel-facts.md` §3.1/§3.8/§5.5）。剩下需要凭证的：
> 1. 上传接口真实返回的字段与 URL 形态（文档给 `{url, filename, content_type, bytes, created_at}`）；
> 2. `image_urls` 到底吃**字符串数组**还是 `[{"url": …}]` 对象数组（实时 Schema 与生成页示例说前者，上传页示例说后者）；
> 3. 传上传后的 URL 走**图生图**是否真的可用、`usage` 的 `input_image_tokens` 是否随之变化；
> 4. `mask_url` 配第一张参考图是否可用（尺寸/Alpha 要求）。
>
> 另：原「预算 ¥20 / $1」是我提的建议值，**偏高了**——实际只用了 5 次 `n=1 quality=low` 提交（见 `docs/facts/channel-facts.md` §5.4）。
>
> **以下原文保留，作为历史记录与执行模板。**

- **状态**：**待用户批准预算与凭证**。**在获得明确批准之前，不发起任何调用**（含只读的 V6 也需凭证）——此条对"恢复两渠道"路径仍然有效。
- **本文中的命令是待批准后才会执行的记录模板，不是已获批准的执行。** 在用户对预算与凭证给出明确批准之前，**一条都不执行**——包括第 1 步那些只读命令（它们同样需要 `APIMART_API_KEY`）。

## 0. 前置条件（缺一不可）

| 项 | 内容 | 谁提供 |
| --- | --- | --- |
| 预算 | AIHubMix 侧上限 **¥20**；APIMart 侧上限 **$1** | 用户批准 |
| 凭证 | `AIHUBMIX_API_KEY`（Machine 级已存在，但当前进程读不到，需服务宿主可读）；`APIMART_API_KEY`（**待用户开通并提供**） | 用户 |
| 纪律 | 凭证只从环境变量读取，不写入仓库、日志、响应或 fixture；不保存真实 URL/task id/签名参数 | `AGENTS.md` |

## 1. 步骤顺序（先只读、后计费）

### 第 1 步：只读、零计费（先做，可回答 V3/V6 与部分 V2）

```bash
# V6（1）APIMart 机器 schema：确认端点形态与 model 字面值（旧探测：/v1/models/{m}/schema 返回 401 而非 404）
curl -s -o /tmp/p1.json -w '%{http_code}\n' https://api.apimart.ai/v1/models/gpt-image-2.5-flare/schema \
  -H "Authorization: Bearer $APIMART_API_KEY"
cat /tmp/p1.json | head -c 2000

# V6（2）模型目录：确认 gpt-image-2.5 两款在册，以及 owned_by/category
curl -s 'https://api.apimart.ai/v1/models?expand=category' -H "Authorization: Bearer $APIMART_API_KEY" \
  | jq '.data[] | select(.id|test("gpt-image-2.5")) | {id, owned_by, category, created}'

# V6（3）账号用量口径（空转，零余额也可用）：确认返回结构与是否含 task_id
curl -s 'https://api.apimart.ai/v1/usage' -H "Authorization: Bearer $APIMART_API_KEY" | jq .

# V3 对照：AIHubMix 侧 2.5 的机器 schema（免鉴权，已在仓库）
#   out-reference/aihubmix/schema-gpt-image-2.5-flare.endpoints.json
```

**V6 的判据**：
- 若 schema 端点返回结构化 JSON ⇒ APIMart 的 Profile 可由**机器契约**导入（证据等级同 AIHubMix）；
- 若返回 404 或非结构化 ⇒ 只能从**文档字段表人工导入**并标注来源与抓取时间（`0002` §5 流程不变）。
- 顺带确认 APIMart 侧 `model` 的**字面值**（V3）：必须是发布期 `model.const` 所用的字符串。

### 第 2 步：APIMart 计费调用（1 次创建 + 轮询，回答 V1/V2/V4/V5）

```bash
# 创建（记录 HTTP 响应头与 body 全文）
curl -s -D /tmp/h.txt -o /tmp/create.json \
  -X POST https://api.apimart.ai/v1/images/generations \
  -H "Authorization: Bearer $APIMART_API_KEY" -H 'Content-Type: application/json' \
  -d '{"model":"gpt-image-2.5-flare","prompt":"a red apple on a wooden table","size":"1:1","resolution":"1k","quality":"low"}'

# 轮询至终态（记录每次响应）
#   GET /v1/tasks/{task_id}  —— 任务 id 取自创建响应（V2 的字段路径在此确认）
```

**要读出的字段**（逐项写进记录，**不保存真实图片 URL**）：
- V1：响应中**是否存在 `usage`**（尤其分项 `input_tokens`/`output_tokens`/`total_tokens`）；
- V2：`task_id` 的**字段路径**；轮询过程中观察到的**全部状态取值**（用于终态集合判定，见规划 §4 的轮询合同）；
- V4：`cost`/`credits_cost` 的在场与量级（**仅记录，不据此推断单价**）；
- V5：把 `n` 从 1 改为 2、`quality` 换一档各调一次，观察是否报错或生效。

**V4（幂等）另做**：同一 `Idempotency-Key` + 同 Body 连发两次，观察是否重放原响应（若返回 409 相关 code，记录其准确字符串）。

### 第 3 步：AIHubMix 计费调用（1 次，回答「2.5 的响应是否与上一代同构」）

```bash
curl -s -D /tmp/hb.txt -o /tmp/hb.json \
  -X POST https://api.inferera.com/v1/images/generations \
  -H "Authorization: Bearer $AIHUBMIX_API_KEY" -H 'Content-Type: application/json' \
  -d '{"model":"gpt-image-2.5-flare","prompt":"a red apple on a wooden table","size":"auto","quality":"low"}'
```
读出是否存在分项 token usage（第一阶段在 `gpt-image-2` 上已确认存在，本次确认 2.5 是否同构）。

## 2. 停止条件（任一命中即停止全部调用，并回 Planning 报告）

**本节以规划 §8 第 2 项为准**（此前本节只有 5 条，与规划不一致，已对齐）：

1. **V1–V6 各取得一次可用证据**——判据是「每个 V 编号都有一条可归因、可复核的记录」，不是「调用次数用满」；
2. 累计花费达到上限（AIHubMix ¥20 / APIMart $1）——**不足则停下重新申请，不自行加码**；
3. **创建成功但按 `task_id` 查询返回 404**（说明任务 id 不可用于对账 ⇒ 对账基础不成立）；
4. 响应**既无 `usage` 也无 `cost`/`credits_cost`**（⇒ 无可核验计量事实，该 Offering 不得以正式计费状态发布）；
5. `model.const` 与 `native_model_id` **不同名**且无法通过 Offering 的 `provider_model_id` 对齐（⇒ 身份或发布合同需回到 Planning）；
6. 出现**任何一次无法归因的扣费**（对账基础不成立，必须先解决）；
7. 连续 2 次调用得到互相矛盾的计量事实。

**执行前置停止条件**（不在规划 §8 的计费类停止条件内，属本地检查，**先于第 1 步执行**）：

- **本地先确认 `APIMART_API_KEY` 与 `AIHUBMIX_API_KEY` 均已注入当前进程**（`[ -n "$APIMART_API_KEY" ]`，**纯本地、不发请求、不计费**）。任一为空则**停下报告「缺少凭证」**，不得改用硬编码、不得从仓库文件读取、不得跳过该项继续。
- 执行中若收到 **401/403** ⇒ 凭证无效或权限不足，**立即停止全部调用并报告**；**不得用同一把 Key 重试、不得猜测其它端点、不得改用别的凭证来源**。
- 若 §1 第 1 步的只读端点**全部**返回 401（含 `$APIMART_API_KEY` 已注入的情况），说明该 Key 尚无对应权限 ⇒ 记为「V6 无法以机器契约完成」，按 §1 第 1 步的判据降级为**文档字段表人工导入**，而不是继续试探端点。

## 3. 留档要求

每次调用记录：请求参数、HTTP 状态、**响应头**、响应体摘要（脱敏）、计量字段原文、结果尺寸、以及本次归属的 V 编号。
**不保存**：真实图片 URL、签名参数、`task_id`（可记其存在与长度，不记全文）。

结论写回：
- `out-reference/apimart/apimart-response-shape-findings.md`（新建，脱敏）；
- 工作项 #2 的规划 §5.2：**明确落到哪一条路**（含分项 token ⇒ 领域零改动、`adr/0012` 作废；只有金额 ⇒ 必须改领域类型、回到 Planning）。

## 4. 结论对 Gate 的影响

| 结果 | Gate 影响 |
| --- | --- |
| 含分项 token | 未决材料决策消失 ⇒ 可实现，`docs/adr/0012` 作废 |
| 只有金额 | **必须回到 Planning** 扩展证据形态（属规划 §1 的例外 E2），Gate 延后 |
| 两者都没有 | APIMart 侧不得以正式计费状态发布；本阶段范围需重新界定 |
