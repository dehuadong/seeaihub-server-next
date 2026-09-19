# 上游原文快照（外部参考资源，不是平台接口合同）
# 来源: https://docs.apimart.ai/cn/api-reference/images/gpt-image-2.5/generation.md
# 抓取时间: 2026-09-19
# HTTP: 200
# 说明: 原样保存，供离线复核；运行中服务不读取本目录。

---

> ## Documentation Index
> Fetch the complete documentation index at: https://docs.apimart.ai/llms.txt
> Use this file to discover all available pages before exploring further.

# GPT-Image-2.5 图像生成

>  - 提供 gpt-image-2.5-flare 与 gpt-image-2.5-sunburst 两种模型
- 异步处理模式，返回 task_id 用于后续查询
- 支持文生图与最多 16 张参考图的图像编辑
- 支持 15 种比例、精确像素尺寸以及 1K / 2K / 4K 分辨率
- 支持 low / medium / high / xhigh / max 五档质量 

<Info>
  **如何选择模型：** `gpt-image-2.5-flare` 速度更快，适合日常高质量出图、批量生成和快速原型；`gpt-image-2.5-sunburst` 更强调编辑精度，适合成品级商品图、投放创意和多轮精细编辑。两者计费标准相同。
</Info>

<RequestExample>
  ```bash cURL theme={null}
  curl --request POST \
    --url https://api.apimart.ai/v1/images/generations \
    --header 'Authorization: Bearer <token>' \
    --header 'Content-Type: application/json' \
    --data '{
      "model": "gpt-image-2.5-flare",
      "prompt": "雨天窗边温暖舒适的阅读角，暖色台灯，电影感光影",
      "size": "1:1",
      "resolution": "1k",
      "quality": "medium",
      "n": 1
    }'
  ```

  ```python Python theme={null}
  import requests

  response = requests.post(
      "https://api.apimart.ai/v1/images/generations",
      headers={
          "Authorization": "Bearer <token>",
          "Content-Type": "application/json",
      },
      json={
          "model": "gpt-image-2.5-flare",
          "prompt": "雨天窗边温暖舒适的阅读角，暖色台灯，电影感光影",
          "size": "1:1",
          "resolution": "1k",
          "quality": "medium",
          "n": 1,
      },
  )

  print(response.json())
  ```

  ```javascript JavaScript theme={null}
  const response = await fetch(
    "https://api.apimart.ai/v1/images/generations",
    {
      method: "POST",
      headers: {
        Authorization: "Bearer <token>",
        "Content-Type": "application/json",
      },
      body: JSON.stringify({
        model: "gpt-image-2.5-flare",
        prompt: "雨天窗边温暖舒适的阅读角，暖色台灯，电影感光影",
        size: "1:1",
        resolution: "1k",
        quality: "medium",
        n: 1,
      }),
    },
  );

  console.log(await response.json());
  ```
</RequestExample>

<ResponseExample>
  ```json 200 theme={null}
  {
    "code": 200,
    "data": [
      {
        "status": "submitted",
        "task_id": "task_01KXXXXXXXXXXXXXXX"
      }
    ]
  }
  ```

  ```json 400 theme={null}
  {
    "error": {
      "code": 400,
      "message": "请求参数无效",
      "type": "invalid_request_error"
    }
  }
  ```

  ```json 401 theme={null}
  {
    "error": {
      "code": 401,
      "message": "身份验证失败，请检查您的 API 密钥",
      "type": "authentication_error"
    }
  }
  ```

  ```json 402 theme={null}
  {
    "error": {
      "code": 402,
      "message": "账户余额不足，请充值后再试",
      "type": "payment_required"
    }
  }
  ```

  ```json 429 theme={null}
  {
    "error": {
      "code": 429,
      "message": "请求过于频繁，请稍后再试",
      "type": "rate_limit_error"
    }
  }
  ```

  ```json 500 theme={null}
  {
    "error": {
      "code": 500,
      "message": "服务器内部错误，请稍后重试",
      "type": "server_error"
    }
  }
  ```
</ResponseExample>

## 认证

<ParamField header="Authorization" type="string" required>
  所有接口均使用 Bearer Token 认证。访问 [API Key 管理页面](https://apimart.ai/keys) 获取密钥。

  ```
  Authorization: Bearer YOUR_API_KEY
  ```
</ParamField>

## 模型选择

| 模型                       | 特点        | 适用场景                     |
| ------------------------ | --------- | ------------------------ |
| `gpt-image-2.5-flare`    | 默认款，生成速度快 | 社媒内容、商品图、视觉搜索、快速原型、大批量生成 |
| `gpt-image-2.5-sunburst` | 编辑精度优先    | 投放级创意、成品级商品图、多轮精细编辑      |

两个模型的单价和相同参数下的 token 消耗一致，选择时只需考虑速度与质量取舍。

与 `gpt-image-2` 相比，GPT-Image-2.5 新增 `xhigh` 和 `max` 两个质量档位；`medium` 与 `high` 的输出 token 消耗约为上一代同名档位的四分之一。

## 请求参数

<ParamField body="model" type="string" required>
  图像生成模型名称。可选值：

  * `gpt-image-2.5-flare`
  * `gpt-image-2.5-sunburst`
</ParamField>

<ParamField body="prompt" type="string" required>
  图像生成或编辑的文本描述。

  支持中英文。建议说明主体、场景、构图、风格、光线以及需要保留或修改的内容。
</ParamField>

<ParamField body="size" type="string" default="auto">
  输出图像的比例或精确像素尺寸。

  支持：

  * `auto`：由模型根据提示词或参考图决定
  * 比例名：`1:1`、`3:2`、`2:3`、`4:3`、`3:4`、`5:4`、`4:5`、`16:9`、`9:16`、`2:1`、`1:2`、`21:9`、`9:21`、`3:1`、`1:3`
  * 精确像素：例如 `1600x1200`

  <Tip>
    图生图时建议不传 `size`，系统会根据输入图比例和 `resolution` 自动计算输出尺寸。
  </Tip>
</ParamField>

<ParamField body="resolution" type="string" default="1k">
  分辨率档位，与比例形式的 `size` 配合决定实际输出像素。

  * `1k`（默认）
  * `2k`
  * `4k`

  当 `size` 使用精确像素格式时，此字段会被忽略。
</ParamField>

<ParamField body="quality" type="string" default="auto">
  图片质量档位。

  * `low`
  * `medium`
  * `high`
  * `xhigh`
  * `max`
  * `auto`（默认，由模型运行时决定）

  <Warning>
    `xhigh` 和 `max` 仅 GPT-Image-2.5 支持。将其传给 `gpt-image-2` 会同步返回 400，不会自动降级。
  </Warning>
</ParamField>

<ParamField body="n" type="integer" default="1">
  生成图片数量，取值范围为 `1` \~ `4`。

  必须传入数字，不要使用字符串。
</ParamField>

<ParamField body="output_format" type="string" default="png">
  输出文件格式。

  * `png`（默认，支持透明背景）
  * `jpeg`
  * `webp`（支持透明背景）
</ParamField>

<ParamField body="output_compression" type="integer">
  输出压缩强度，范围为 `0` \~ `100`，仅对 `jpeg` 和 `webp` 生效。
</ParamField>

<ParamField body="background" type="string">
  背景模式。可选值：`transparent`、`opaque`、`auto`。

  <Warning>
    `background: "transparent"` 只能与 `output_format: "png"` 或 `output_format: "webp"` 搭配。JPEG 不支持 Alpha 通道。
  </Warning>
</ParamField>

<ParamField body="moderation" type="string" default="low">
  内容审核强度。可选值：`auto`、`low`。

  未传时，APIMart 会显式使用 `low`；传入 `auto` 时则按 `auto` 执行。
</ParamField>

<ParamField body="image_urls" type="string[]">
  图生图或图像编辑使用的参考图 URL 数组，最多 `16` 张。传入后自动进入编辑模式。

  仅接受公网可访问的 HTTP(S) URL。本地图片请先调用 `POST /v1/uploads/images` 上传，再使用返回的 `url`。
</ParamField>

## 尺寸规则

使用精确像素尺寸时，宽高必须同时满足以下条件：

* 宽和高均为 `16` 的倍数
* 任意单边不超过 `3840` 像素
* 长边与短边之比不超过 `3:1`
* 总像素在 `655,360` \~ `8,294,400` 之间

<Warning>
  高于 2560×1440 的分辨率属于实验性范围，稳定性可能低于常用分辨率。
</Warning>

### 比例与分辨率映射

| `size` | `1k`      | `2k`      | `4k`      |
| ------ | --------- | --------- | --------- |
| `1:1`  | 1024×1024 | 2048×2048 | 2880×2880 |
| `3:2`  | 1536×1024 | 2048×1360 | 3520×2336 |
| `2:3`  | 1024×1536 | 1360×2048 | 2336×3520 |
| `4:3`  | 1024×768  | 2048×1536 | 3312×2480 |
| `3:4`  | 768×1024  | 1536×2048 | 2480×3312 |
| `5:4`  | 1280×1024 | 2560×2048 | 3216×2576 |
| `4:5`  | 1024×1280 | 2048×2560 | 2576×3216 |
| `16:9` | 1536×864  | 2048×1152 | 3840×2160 |
| `9:16` | 864×1536  | 1152×2048 | 2160×3840 |
| `2:1`  | 2048×1024 | 2688×1344 | 3840×1920 |
| `1:2`  | 1024×2048 | 1344×2688 | 1920×3840 |
| `21:9` | 2016×864  | 2688×1152 | 3840×1648 |
| `9:21` | 864×2016  | 1152×2688 | 1648×3840 |
| `3:1`  | 1536×512  | 3072×1024 | 3840×1280 |
| `1:3`  | 512×1536  | 1024×3072 | 1280×3840 |

也可以直接传入满足尺寸规则的任意精确像素值，不限于上表中的组合。

## 使用示例

### 文生图

```json theme={null}
{
  "model": "gpt-image-2.5-flare",
  "prompt": "未来感城市中的空中花园，清晨薄雾，建筑摄影",
  "size": "16:9",
  "resolution": "2k",
  "quality": "high",
  "n": 1
}
```

### 使用 Sunburst 精细编辑

```json theme={null}
{
  "model": "gpt-image-2.5-sunburst",
  "prompt": "保留商品主体和包装文字，将背景替换为柔和的米白色摄影棚，并增加自然投影",
  "image_urls": [
    "https://example.com/product.png"
  ],
  "resolution": "2k",
  "quality": "xhigh"
}
```

### 多参考图编辑

```json theme={null}
{
  "model": "gpt-image-2.5-sunburst",
  "prompt": "以第一张图的商品为主体，参考第二张图的布景和第三张图的光线，生成横版广告图",
  "image_urls": [
    "https://example.com/product.png",
    "https://example.com/set.jpg",
    "https://example.com/lighting.jpg"
  ],
  "size": "16:9",
  "resolution": "2k",
  "quality": "max"
}
```

### 透明背景

```json theme={null}
{
  "model": "gpt-image-2.5-flare",
  "prompt": "一双白色运动鞋的电商产品图，主体完整，透明背景",
  "size": "1:1",
  "resolution": "2k",
  "quality": "high",
  "background": "transparent",
  "output_format": "png"
}
```

### 精确像素尺寸

```json theme={null}
{
  "model": "gpt-image-2.5-flare",
  "prompt": "极简风格的产品发布会主视觉",
  "size": "1600x1200",
  "quality": "medium"
}
```

## 提交响应

提交成功后会立即返回异步任务 ID：

```json theme={null}
{
  "code": 200,
  "data": [
    {
      "status": "submitted",
      "task_id": "task_01KXXXXXXXXXXXXXXX"
    }
  ]
}
```

<Warning>
  `data` 是数组，请读取 `data[0].task_id`。
</Warning>

## 查询任务结果

使用提交响应中的 `task_id` 调用 [任务查询接口](/cn/api-reference/tasks/status)：

```bash theme={null}
curl --request GET \
  --url https://api.apimart.ai/v1/tasks/task_01KXXXXXXXXXXXXXXX \
  --header 'Authorization: Bearer <token>'
```

建议每 2 \~ 5 秒轮询一次，直到状态变为 `completed` 或 `failed`。

### 任务成功

```json theme={null}
{
  "code": 200,
  "data": {
    "id": "task_01KXXXXXXXXXXXXXXX",
    "status": "completed",
    "progress": 100,
    "cost": 0.01325,
    "credits_cost": 0.1325,
    "result": {
      "images": [
        {
          "url": [
            "https://upload.apimart.ai/f/image/example.png"
          ],
          "expires_at": 1789000000
        }
      ]
    },
    "usage": {
      "input_tokens": 16,
      "output_tokens": 439,
      "total_tokens": 455
    }
  }
}
```

图片地址位于 `data.result.images[].url[]`。请及时下载并转存，不要将临时 URL 用作长期存储。

批量查询多个任务时，可使用 `POST /v1/tasks/batch`。

| 状态           | 含义                                |
| ------------ | --------------------------------- |
| `submitted`  | 任务已提交                             |
| `processing` | 正在生成                              |
| `completed`  | 生成成功，可读取 `result.images`          |
| `failed`     | 生成失败，查看 `error.message`；预扣费用会自动退回 |

## 计费说明

GPT-Image-2.5 按实际 token 用量计费，Flare 与 Sunburst 单价相同。最终费用请以 [价格页面](https://apimart.ai/pricing) 或 `/api/pricing` 返回的实时值为准。

### 官方 token 单价

| 项目         | 每 100 万 token 单价 |
| ---------- | ---------------- |
| 图片输出       | \$30.00          |
| 图片输入       | \$8.00           |
| 图片输入（缓存命中） | \$2.00           |
| 文本输入       | \$5.00           |
| 文本输入（缓存命中） | \$1.25           |

实际扣费还会受到账号分组倍率和折扣影响。

### 1024×1024 输出 token 参考

| `quality` | 输出 token | 官方输出成本    |
| --------- | -------- | --------- |
| `low`     | 196      | \$0.00588 |
| `medium`  | 439      | \$0.01317 |
| `high`    | 1756     | \$0.05268 |
| `xhigh`   | 3122     | \$0.09366 |
| `max`     | 7024     | \$0.21072 |

<Warning>
  `quality: "auto"` 的实际档位由模型运行时决定。提交时会按当前尺寸的最高档 `max` 预留额度，任务完成后再按真实 token 用量结算并退回差额。余额敏感时建议显式指定 `quality`。
</Warning>

当 `n > 1` 时，预扣额度会按图片数量线性增加，最终按实际生成张数结算。任务失败会自动退款。

## 输出 token 参考表

下表为各比例、分辨率和质量组合的单张图片输出 token 参考值。实际账单还会包含提示词及参考图的输入 token。

| 尺寸        | 实际像素      | low | medium | high | xhigh | max   |
| --------- | --------- | --- | ------ | ---- | ----- | ----- |
| `1:1`     | 1024×1024 | 196 | 439    | 1756 | 3122  | 7024  |
| `3:2`     | 1536×1024 | 158 | 343    | 1372 | 2459  | 5488  |
| `2:3`     | 1024×1536 | 158 | 343    | 1372 | 2459  | 5488  |
| `4:3`     | 1024×768  | 134 | 301    | 1204 | 2140  | 4815  |
| `3:4`     | 768×1024  | 134 | 301    | 1204 | 2140  | 4815  |
| `5:4`     | 1280×1024 | 173 | 378    | 1510 | 2702  | 6119  |
| `4:5`     | 1024×1280 | 173 | 378    | 1510 | 2702  | 6119  |
| `16:9`    | 1536×864  | 120 | 280    | 1078 | 1917  | 4312  |
| `9:16`    | 864×1536  | 120 | 280    | 1078 | 1917  | 4312  |
| `2:1`     | 2048×1024 | 132 | 295    | 1180 | 2098  | 4720  |
| `1:2`     | 1024×2048 | 132 | 295    | 1180 | 2098  | 4720  |
| `21:9`    | 2016×864  | 105 | 225    | 943  | 1617  | 3682  |
| `9:21`    | 864×2016  | 105 | 225    | 943  | 1617  | 3682  |
| `3:1`     | 1536×512  | 56  | 134    | 535  | 937   | 2140  |
| `1:3`     | 512×1536  | 56  | 134    | 535  | 937   | 2140  |
| `1:1@2k`  | 2048×2048 | 397 | 892    | 3568 | 6343  | 14272 |
| `3:2@2k`  | 2048×1360 | 211 | 460    | 1838 | 3216  | 7351  |
| `2:3@2k`  | 1360×2048 | 211 | 460    | 1838 | 3216  | 7351  |
| `4:3@2k`  | 2048×1536 | 247 | 556    | 2223 | 3952  | 8892  |
| `3:4@2k`  | 1536×2048 | 247 | 556    | 2223 | 3952  | 8892  |
| `5:4@2k`  | 2560×2048 | 377 | 826    | 3303 | 5911  | 13385 |
| `4:5@2k`  | 2048×2560 | 377 | 826    | 3303 | 5911  | 13385 |
| `16:9@2k` | 2048×1152 | 157 | 367    | 1413 | 2511  | 5650  |
| `9:16@2k` | 1152×2048 | 157 | 367    | 1413 | 2511  | 5650  |
| `2:1@2k`  | 2688×1344 | 180 | 405    | 1617 | 2874  | 6466  |
| `1:2@2k`  | 1344×2688 | 180 | 405    | 1617 | 2874  | 6466  |
| `21:9@2k` | 2688×1152 | 143 | 306    | 1285 | 2202  | 5016  |
| `9:21@2k` | 1152×2688 | 143 | 306    | 1285 | 2202  | 5016  |
| `3:1@2k`  | 3072×1024 | 103 | 247    | 988  | 1729  | 3952  |
| `1:3@2k`  | 1024×3072 | 103 | 247    | 988  | 1729  | 3952  |
| `1:1@4k`  | 2880×2880 | 659 | 1483   | 5930 | 10542 | 23719 |
| `3:2@4k`  | 3520×2336 | 450 | 982    | 3926 | 6870  | 15703 |
| `2:3@4k`  | 2336×3520 | 450 | 982    | 3926 | 6870  | 15703 |
| `4:3@4k`  | 3312×2480 | 491 | 1104   | 4413 | 7845  | 17650 |
| `3:4@4k`  | 2480×3312 | 491 | 1104   | 4413 | 7845  | 17650 |
| `5:4@4k`  | 3216×2576 | 535 | 1173   | 4690 | 8393  | 19006 |
| `4:5@4k`  | 2576×3216 | 535 | 1173   | 4690 | 8393  | 19006 |
| `16:9@4k` | 3840×2160 | 371 | 865    | 3336 | 5930  | 13342 |
| `9:16@4k` | 2160×3840 | 371 | 865    | 3336 | 5930  | 13342 |
| `2:1@4k`  | 3840×1920 | 300 | 675    | 2700 | 4799  | 10798 |
| `1:2@4k`  | 1920×3840 | 300 | 675    | 2700 | 4799  | 10798 |
| `21:9@4k` | 3840×1648 | 234 | 500    | 2099 | 3598  | 8196  |
| `9:21@4k` | 1648×3840 | 234 | 500    | 2099 | 3598  | 8196  |
| `3:1@4k`  | 3840×1280 | 139 | 332    | 1328 | 2324  | 5311  |
| `1:3@4k`  | 1280×3840 | 139 | 332    | 1328 | 2324  | 5311  |

## 限制

| 项目         | 限制                   |
| ---------- | -------------------- |
| 单次生成数量 `n` | 1 \~ 4               |
| 参考图数量      | 最多 16 张              |
| 单边像素       | 不超过 3840，且为 16 的倍数   |
| 总像素        | 655,360 \~ 8,294,400 |
| 长短边比例      | 不超过 3:1              |
| 输出格式       | PNG / JPEG / WebP    |
| 透明背景       | 仅 PNG / WebP         |
| 流式部分图      | 暂不支持                 |

## 常见错误

| 场景                                 | 原因与处理                                       |
| ---------------------------------- | ------------------------------------------- |
| `quality` 不受支持                     | `xhigh` / `max` 仅支持 GPT-Image-2.5；更换模型或调整档位 |
| 精确像素宽高不是 16 的倍数                    | 调整到满足尺寸规则的像素值                               |
| `background=transparent` 搭配 `jpeg` | 将 `output_format` 改为 `png` 或 `webp`         |
| 参考图超过 16 张                         | 减少 `image_urls` 中的图片数量                      |
| 账户余额不足                             | 充值后重试；使用 `auto` 时需注意最高档预留额度                 |

## Response

<ResponseField name="code" type="integer">
  响应状态码，提交成功时为 200。
</ResponseField>

<ResponseField name="data" type="array">
  提交响应数据。

  <Expandable title="数组元素">
    <ResponseField name="status" type="string">
      初始状态为 `submitted`。
    </ResponseField>

    <ResponseField name="task_id" type="string">
      任务唯一标识符，用于查询任务状态和生成结果。
    </ResponseField>
  </Expandable>
</ResponseField>
