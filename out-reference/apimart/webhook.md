> ## Documentation Index
> Fetch the complete documentation index at: https://docs.apimart.ai/llms.txt
> Use this file to discover all available pages before exploring further.

# 任务完成回调（Webhook）

> **已退场（#223 / ADR-0008，2026-07-26）**：Apimart 渠道已从 SeeAI Hub 完整移除，不再是受支持渠道。本文档仅作历史参考保留；运行时不再包含 apimart 的分发、回调与计费路径。

> 提交异步生成任务时带上回调地址，任务完成（成功或失败）后主动把结果 POST 给你，无需轮询查询。

提交视频 / 图片 / 音频等异步生成任务时，可以带上一个回调地址。任务**完成（成功或失败）后**，我们会主动把结果 POST 到你的地址，你就不用一直轮询查询了。

## 快速开始

提交任务时，在请求体里加一个 `webhook` 字段：

```bash theme={null}
curl -X POST https://你的接入域名/v1/images/generations \
  -H "Authorization: Bearer 你的APIKey" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-image-2",
    "prompt": "a red apple on a table",
    "size": "1024x1024",
    "webhook": "https://your-server.com"
  }'
```

任务做完后，我们会向 **`你的地址 + /callback`** 发一个 POST 请求。

<Note>
  视频、音频等其它异步任务接口同理，都是在提交体里加 `webhook` 字段。
</Note>

## 地址规则

你填的 `webhook` 是**基础地址（base）**，我们会自动在后面拼 `/callback`：

| 你填的 `webhook`                  | 我们实际 POST 到                            |
| ------------------------------ | -------------------------------------- |
| `https://your-server.com`      | `https://your-server.com/callback`     |
| `https://your-server.com/api`  | `https://your-server.com/api/callback` |
| `https://your-server.com/api/` | `https://your-server.com/api/callback` |

所以你的服务端需要准备一个接收 `POST .../callback` 的接口。

## 你会收到什么

推送内容**和「[获取任务状态](/cn/api-reference/tasks/status)」接口返回的完全一致**——你用同一套解析逻辑处理即可。

<CodeGroup>
  ```json 成功（status: completed） theme={null}
  {
    "id": "task_01KV7FXR8BEYS1BWHJCT3JMCJ5",
    "status": "completed",
    "progress": 100,
    "created": 1781589029,
    "completed": 1781589058,
    "actual_time": 29,
    "cost": 0.006,
    "credits_cost": 0.06,
    "result": {
      "images": [{ "url": ["https://.../result.png"], "expires_at": 1781675458 }]
    }
  }
  ```

  ```json 失败（status: failed） theme={null}
  {
    "id": "task_xxx",
    "status": "failed",
    "progress": 100,
    "created": 1781589029,
    "completed": 1781589050,
    "error": { "message": "失败原因...", "code": "" }
  }
  ```
</CodeGroup>

<Note>
  视频任务结果在 `result.videos`，音频在 `result.audios`。
</Note>

<Warning>
  只有任务到达**终态**（`completed` / `failed`）才会推送；处理中不推。
</Warning>

## 重试与去重（重要）

* **重试**：如果你的服务端没在约 10 秒内返回 `2xx`，或返回了 `5xx`，我们会自动重试，最多 **3 次**，间隔约 **10 秒、30 秒、60 秒**。3 次都失败就放弃（约 2 分钟内结束）。
* **不重试的情况**：你的接口返回 `4xx`（视为地址 / 请求有问题），我们直接放弃，不重试。
* **去重**：正常情况下一个任务只推一次。但在极端情况下（如我们这边发送后、确认前发生重启）你**可能收到重复推送**。请务必**按 `id`（task\_id）做幂等去重**，避免重复处理。

**你的接收接口建议：**

<Steps>
  <Step title="尽快返回 2xx">
    先收下、入队，再异步处理，别让我们等你处理完。
  </Step>

  <Step title="按 id 去重">
    以 `id`（task\_id）作为幂等键，避免重复处理。
  </Step>

  <Step title="配置并校验签名">
    在生产环境对回调请求做来源校验，拒绝伪造请求。
  </Step>
</Steps>

## 对回调地址的要求

为安全起见，回调地址必须满足：

| 要求    | 说明                                                       |
| ----- | -------------------------------------------------------- |
| 公网可访问 | **不能**是内网 / 本地地址（如 `127.0.0.1`、`10.x`、`192.168.x` 等会被拒绝） |
| 协议    | `http` 或 `https`（推荐 `https`）                             |
| 端口    | 用标准端口（`80` / `443`），非标端口可能被拦截                            |
| 域名    | 不能指向我们自己的服务域名                                            |

不满足要求的地址会被直接丢弃（不会推送、也不会重试）。

## 常见问题

<AccordionGroup>
  <Accordion title="提交了带 webhook 的任务，但没收到推送？">
    逐项排查：

    1. 任务**是否真的完成了**？查一下任务详情，`status` 是不是 `completed` / `failed`（处理中不推）。
    2. 你的地址是不是**公网可访问**？我们能不能连上你的 `/callback`？
    3. 端口是不是标准端口（80 / 443）？非标端口可能被安全策略拦掉。
    4. 你的 `/callback` 是不是**及时返回了 2xx**？返回 4xx 会被直接放弃。
    5. 用 `https` 了吗？证书是否有效？
  </Accordion>

  <Accordion title="收到的 result 里 url 怎么是个数组？">
    部分模型一次会出多张图，`images[].url` 可能是数组，按数组处理即可。
  </Accordion>

  <Accordion title="结果链接有有效期吗？">
    `result` 里若带 `expires_at`（Unix 时间戳），表示该链接的过期时间，请及时转存。
  </Accordion>

  <Accordion title="会推送「处理中」的状态吗？">
    不会。只在任务**最终成功或失败**时推送一次。
  </Accordion>
</AccordionGroup>

## 最小接收端示例

```python Python theme={null}
from http.server import BaseHTTPRequestHandler, HTTPServer
import json

class H(BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(n)
        data = json.loads(body)
        print("收到任务回调:", data["id"], data["status"])
        # TODO: 这里按 id 去重、校验签名、再处理
        self.send_response(200); self.end_headers()
        self.wfile.write(b'{"ok":true}')

HTTPServer(("0.0.0.0", 443), H).serve_forever()
```

收到后请尽快返回 `200`，处理逻辑放后台异步做。
