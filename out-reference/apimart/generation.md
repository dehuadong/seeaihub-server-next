> ## Documentation Index
> Fetch the complete documentation index at: https://docs.apimart.ai/llms.txt
> Use this file to discover all available pages before exploring further.

# GPT-Image-2 图像生成



>  - 异步处理模式，返回任务ID用于后续查询
- 基于 OpenAI Images 兼容协议，支持文生图 / 图生图
- 支持 15 种图片比例，通过 `size` 字段传入
- 通过 `resolution`（`1k` / `2k` / `4k`）控制实际输出像素档位
- 通过 `quality`（`low` / `medium` / `high`）控制出图质量与耗时
- 参考图最多 16 张，支持 URL 与 base64 混填
- 按分辨率档位（1K / 2K / 4K）计费 

<RequestExample>
  ```bash cURL theme={null}
  curl --request POST \
    --url https://api.apimart.ai/v1/images/generations \
    --header 'Authorization: Bearer <token>' \
    --header 'Content-Type: application/json' \
    --data '{
      "model": "gpt-image-2",
      "prompt": "一只橘猫坐在窗台上看夕阳，水彩画风格",
      "n": 1,
      "size": "16:9",
      "resolution": "2k"
    }'
  ```

  ```python Python theme={null}
  import requests

  url = "https://api.apimart.ai/v1/images/generations"

  payload = {
      "model": "gpt-image-2",
      "prompt": "一只橘猫坐在窗台上看夕阳，水彩画风格",
      "n": 1,
      "size": "16:9",
      "resolution": "2k"
  }

  headers = {
      "Authorization": "Bearer <token>",
      "Content-Type": "application/json"
  }

  response = requests.post(url, json=payload, headers=headers)

  print(response.json())
  ```

  ```javascript JavaScript theme={null}
  const url = "https://api.apimart.ai/v1/images/generations";

  const payload = {
    model: "gpt-image-2",
    prompt: "一只橘猫坐在窗台上看夕阳，水彩画风格",
    n: 1,
    size: "16:9",
    resolution: "2k"
  };

  const headers = {
    "Authorization": "Bearer <token>",
    "Content-Type": "application/json"
  };

  fetch(url, {
    method: "POST",
    headers: headers,
    body: JSON.stringify(payload)
  })
    .then(response => response.json())
    .then(data => console.log(data))
    .catch(error => console.error('Error:', error));
  ```

</RequestExample>

<ResponseExample>
  ```json 200 theme={null}
  {
    "code": 200,
    "data": [
      {
        "status": "submitted",
        "task_id": "task_01KPQ7J7DWB7QZ3WCEK3YVPBRA"
      }
    ]
  }
  ```

  ```json 400 theme={null}
  {
    "error": {
      "code": 400,
      "message": "参数错误：size 不合法 / resolution 不支持 / 像素违规等",
      "type": "invalid_request_error"
    }
  }
  ```

  ```json 401 theme={null}
  {
    "error": {
      "code": 401,
      "message": "身份验证失败，请检查您的API密钥",
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
      "message": "服务器错误",
      "type": "server_error"
    }
  }
  ```

  ```json 503 theme={null}
  {
    "error": {
      "code": 503,
      "message": "上游暂时不可用，请稍后再试",
      "type": "service_unavailable"
    }
  }
  ```
</ResponseExample>

## Authorizations

<ParamField header="Authorization" type="string" required>
  所有接口均需要使用 Bearer Token 进行认证

  获取 API Key：

  访问 [API Key 管理页面](https://apimart.ai/keys) 获取您的 API Key

  使用时在请求头中添加：

  ```
  Authorization: Bearer YOUR_API_KEY
  ```
</ParamField>

## Body

<ParamField body="model" type="string" default="gpt-image-2" required>
  图像生成模型名称

  固定填写 `gpt-image-2`
</ParamField>

<ParamField body="prompt" type="string" required>
  图像生成的文本描述

  * 支持中英文，建议详细描述
  * 提交前会经过平台敏感词 / 安全审核，命中违规内容会直接返回错误
</ParamField>

<ParamField body="n" type="integer" default="1">
  生成图片张数

  取值范围：1 - 10

  <Warning>
    必须传入纯数字（如 `1`），不要加引号
  </Warning>
</ParamField>

<ParamField body="size" type="string" default="1:1">
  图像生成的比例

  支持以下比例，也可传入 `auto` 由服务端自动选择合适比例：

  | size   | 类型 |
  | ------ | -- |
  | `auto` | 自动 |
  | `1:1`  | 正方 |
  | `3:2`  | 横图 |
  | `2:3`  | 竖图 |
  | `4:3`  | 横图 |
  | `3:4`  | 竖图 |
  | `5:4`  | 横图 |
  | `4:5`  | 竖图 |
  | `16:9` | 横图 |
  | `9:16` | 竖图 |
  | `2:1`  | 横图 |
  | `1:2`  | 竖图 |
  | `3:1`  | 横图 |
  | `1:3`  | 竖图 |
  | `21:9` | 横图 |
  | `9:21` | 竖图 |

  也支持直接传入像素尺寸，例如 `1881x836` / `887x1774`。

  <Warning>
    当 `size` 传入 `auto` 时，默认比例为 `1:1`。
  </Warning>
</ParamField>

<ParamField body="resolution" type="string" default="1k">
  输出分辨率档位

  可选值：`1k` / `2k` / `4k`

  `size × resolution` → 实际像素对应关系：

  | size   | `1k`                  | `2k`      | `4k`          |
  | ------ | --------------------- | --------- | ------------- |
  | `1:1`  | 1024×1024 / 1254×1254 | 2048×2048 | **2880×2880** |
  | `3:2`  | 1536×1024             | 2048×1360 | **3520×2336** |
  | `2:3`  | 1024×1536             | 1360×2048 | **2336×3520** |
  | `4:3`  | 1024×768              | 2048×1536 | **3312×2480** |
  | `3:4`  | 768×1024              | 1536×2048 | **2480×3312** |
  | `5:4`  | 1280×1024 / 1448×1086 | 2560×2048 | **3216×2576** |
  | `4:5`  | 1024×1280 / 1122×1402 | 2048×2560 | **2576×3216** |
  | `16:9` | 1536×864 / 1672×941   | 2048×1152 | **3840×2160** |
  | `9:16` | 864×1536 / 941×1672   | 1152×2048 | **2160×3840** |
  | `2:1`  | 2048×1024 / 1774×887  | 2688×1344 | **3840×1920** |
  | `1:2`  | 1024×2048 / 887×1774  | 1344×2688 | **1920×3840** |
  | `3:1`  | 1881×836 / 1536×512   | 3072×1024 | **3840×1280** |
  | `1:3`  | 887×1774 / 512×1536   | 1024×3072 | **1280×3840** |
  | `21:9` | 2016×864 / 1915×821   | 2688×1152 | **3840×1648** |
  | `9:21` | 864×2016 / 821×1915   | 1152×2688 | **1648×3840** |

  <Warning>
    4K 支持上述 15 个比例；也可以直接通过 `size` 传入表格中的像素尺寸。
  </Warning>
</ParamField>

<ParamField body="quality" type="string" default="low">
  图片质量

  * `low` - 快速省钱，轮廓够用
  * `medium` - 平衡
  * `high` - 最高精度（4K + high 耗时 >120s）
</ParamField>

<ParamField body="image_urls" type="array">
  参考图数组（OpenAI 标准字段），传入后走图生图模式

  <Expandable title="详细说明">
    * 最多 16 张参考图，超过会返回 `image_urls exceeds max 16`
    * 单张图最大 20M，总体上限 256M
    * 支持 `图片 URL`（公网可访问的稳定链接）
    * 支持 `base64 data URI`（形如 `data:image/png;base64,...`）
    * 同一数组里可以 URL 与 base64 混填，服务端会自行处理
    * 不传 `size` 时输出分辨率 = 输入图分辨率；传 `size` 则强制按指定尺寸出图
  </Expandable>
</ParamField>

<Note>
  其它 OpenAI 标准字段（`response_format`、`style`）不支持，会被忽略。任务结果只返回 `url`——如需 base64 请自行下载转换。
</Note>

<ParamField body="official_fallback" type="boolean" default="false">
  是否使用官方渠道兜底

  * `false`：不使用（默认）
  * `true`：使用官方渠道
</ParamField>

## 使用场景示例

**文生图（最简请求）**

```json theme={null}
{
  "model": "gpt-image-2",
  "prompt": "一只橘猫坐在窗台上看夕阳，水彩画风格"
}
```

**文生图（指定比例 + 2K）**

```json theme={null}
{
  "model": "gpt-image-2",
  "prompt": "a corgi astronaut on the moon, cinematic, 8k",
  "size": "16:9",
  "resolution": "2k"
}
```

**文生图（4K 输出）**

```json theme={null}
{
  "model": "gpt-image-2",
  "prompt": "星空下的古老城堡",
  "size": "16:9",
  "resolution": "4k"
}
```

**文生图（高质量 + 多张）**

```json theme={null}
{
  "model": "gpt-image-2",
  "prompt": "星空下的古老城堡",
  "size": "16:9",
  "resolution": "4k",
  "quality": "high",
  "n": 2
}
```

**图生图（参考图 = URL）**

```json theme={null}
{
  "model": "gpt-image-2",
  "prompt": "把这张照片变成水彩画风格",
  "image_urls": [
    "https://example.com/photo.jpg"
  ]
}
```

**图生图（参考图 = base64）**

```json theme={null}
{
  "model": "gpt-image-2",
  "prompt": "把这张照片变成水彩画风格",
  "image_urls": [
    "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAA..."
  ]
}
```

**图生图（多参考图融合，URL + base64 混填）**

```json theme={null}
{
  "model": "gpt-image-2",
  "prompt": "把这两张照片融合成一张海报",
  "size": "4:3",
  "resolution": "2k",
  "image_urls": [
    "https://example.com/photo-a.jpg",
    "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAA..."
  ]
}
```

## Response

<ResponseField name="code" type="integer">
  响应状态码
</ResponseField>

<ResponseField name="data" type="array">
  返回数据数组

  <Expandable title="属性">
    <ResponseField name="status" type="string">
      任务状态

      * `submitted` - 已提交
    </ResponseField>

    <ResponseField name="task_id" type="string">
      任务唯一标识符，用于后续查询任务结果
    </ResponseField>
  </Expandable>
</ResponseField>

## 查询任务结果

提交成功后返回 `task_id`，通过 `GET /v1/tasks/{task_id}` 轮询任务状态，详见 [任务查询接口](./status.md)。

### 成功响应示例

```json theme={null}
{
  "code": 200,
  "data": {
    "id": "task_01KPQ7J7DWB7QZ3WCEK3YVPBRA",
    "status": "completed",
    "progress": 100,
    "created": 1776748674,
    "completed": 1776748726,
    "actual_time": 52,
    "cost": 0.05279,
    "credits_cost": 0.5279,
    "estimated_time": 100,
    "result": {
      "images": [
        {
          "url": [
            "https://upload.apimart.ai/f/image/xxxxxxxx-gpt_image_2_task_xxx_0.png"
          ],
          "expires_at": 1776835126
        }
      ]
    }
  }
}
```

取图方式：`data.result.images[0].url[0]`

### 任务状态说明

| 状态           | 含义                    |
| ------------ | --------------------- |
| `submitted`  | 已提交                   |
| `processing` | 上游处理中                 |
| `completed`  | 成功，`result.images` 可用 |
| `failed`     | 失败，查看 `error.message` |