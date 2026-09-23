# 来源: https://docs.apib.ai/cn/api-reference/uploads/images.md

> 抓取时间 2026-09-19；HTTP 200。上游原始文档，**未改写**（仅转载正文与代码示例）。
> 用途：APIMart 参考图/遮罩路径的**上传接口**一手合同（归纳后的渠道事实见 [`docs/facts/channel-facts.md`](../../docs/facts/channel-facts.md) 的 APIMart 上传节）。

## 上传图片

> 上传图片获取 URL，用于图像/视频生成接口

**文档 Playground 不支持文件上传**：请使用下方的 cURL、Python 或 JavaScript 代码示例进行测试。

**临时存储：** 上传的图片仅保存 **72 小时**，到期后图片 URL 将失效。请在有效期内使用，或及时将图片保存到您自己的存储服务中。

**重要变更：** 为了更好的性能和成本控制，我们不再支持在生成接口中直接传入 base64 图片数据。请使用本接口上传图片，获取 URL 后再调用生成接口。

### 为什么需要先上传图片？

1. **性能优化** - base64 编码会使数据膨胀 33%，先上传可显著减少请求体大小
2. **复用图片** - 上传一次，URL 可多次使用，无需重复传输

### 使用流程

```
客户端->>APIMart: POST /v1/uploads/images (上传图片文件)
APIMart->>存储: 保存图片
存储-->>APIMart: 返回存储路径
APIMart-->>客户端: 返回图片 URL
客户端->>APIMart: POST /v1/images/generations (使用图片 URL)
```

### 请求示例

```bash
curl --request POST \
  --url https://api.apimart.ai/v1/uploads/images \
  --header 'Authorization: Bearer <token>' \
  --form 'file=@/path/to/your/image.jpg'
```

```python
import requests

# 上传图片
with open('image.jpg', 'rb') as f:
    response = requests.post(
        "https://api.apimart.ai/v1/uploads/images",
        headers={
            "Authorization": "Bearer <token>"
        },
        files={
            "file": f
        }
    )

result = response.json()
image_url = result['url']
print(f"图片 URL: {image_url}")

# 使用上传的图片进行生成
response = requests.post(
    "https://api.apimart.ai/v1/images/generations",
    headers={
        "Authorization": "Bearer <token>",
        "Content-Type": "application/json"
    },
    json={
        "model": "gemini-3-pro-image-preview",
        "prompt": "基于这张图片创作变体",
        "image_urls": [{"url": image_url}]
    }
)
```

```javascript
// 上传图片
const formData = new FormData();
formData.append('file', fileInput.files[0]);

const uploadResponse = await fetch('https://api.apimart.ai/v1/uploads/images', {
  method: 'POST',
  headers: {
    'Authorization': `Bearer <token>`
  },
  body: formData
});

const uploadResult = await uploadResponse.json();
const imageUrl = uploadResult.url;
console.log(`图片 URL: ${imageUrl}`);

// 使用上传的图片进行生成
const genResponse = await fetch('https://api.apimart.ai/v1/images/generations', {
  method: 'POST',
  headers: {
    'Authorization': `Bearer <token>`,
    'Content-Type': 'application/json'
  },
  body: JSON.stringify({
    model: 'gemini-3-pro-image-preview',
    prompt: '基于这张图片创作变体',
    image_urls: [{url: imageUrl}]
  })
});
```

### 响应示例

```json
// 200
{
  "url": "https://upload.apimart.ai/f/image/9990000123456-a1b2c3d4-photo.jpg",
  "filename": "photo.jpg",
  "content_type": "image/jpeg",
  "bytes": 235680,
  "created_at": 1743436800
}
```

```json
// 400 - 缺少文件字段
{
  "error": {
    "message": "missing or invalid file field: http: no such file",
    "type": "invalid_request_error"
  }
}
```

```json
// 400 - 不支持的格式
{
  "error": {
    "message": "unsupported image type: application/pdf, allowed: jpeg, png, gif, webp",
    "type": "invalid_request_error"
  }
}
```

```json
// 413 - 文件过大
{
  "error": {
    "message": "file size 25165824 exceeds maximum 20971520 bytes",
    "type": "invalid_request_error"
  }
}
```

```json
// 429 - 请求频率限制
{
  "error": {
    "code": 429,
    "message": "Rate limit exceeded. Please try again later",
    "type": "rate_limit_error"
  }
}
```

```json
// 500 - 上传失败
{
  "error": {
    "message": "failed to upload image",
    "type": "server_error"
  }
}
```

### Authorization

所有接口均需要使用 Bearer Token 进行认证。访问 API Key 管理页面获取 API Key，使用时在请求头中添加：

```
Authorization: Bearer YOUR_API_KEY
```

### Body

- `file`（file，必填）：图片文件。支持格式 JPEG (.jpg, .jpeg)、PNG (.png)、WebP (.webp)、GIF (.gif)；最大文件大小 **20MB**。

### Response

- `url`（string）：图片的公开访问 URL，可直接用于生成接口（**72 小时有效**）
- `filename`（string）：原始文件名
- `content_type`（string）：检测到的 MIME 类型，如 `image/jpeg`
- `bytes`（integer）：文件大小（字节）
- `created_at`（integer）：上传时间的 Unix 时间戳（秒）

### 完整示例：图生图工作流（上游原文）

```python
import requests
import time

API_KEY = "your-Apimart-key"
BASE_URL = "https://api.apimart.ai"

# 第一步：上传参考图片
def upload_image(file_path):
    with open(file_path, 'rb') as f:
        response = requests.post(
            f"{BASE_URL}/v1/uploads/images",
            headers={"Authorization": f"Bearer {API_KEY}"},
            files={"file": f}
        )
    return response.json()['url']

# 第二步：创建生成任务
def create_generation(image_url, prompt):
    response = requests.post(
        f"{BASE_URL}/v1/images/generations",
        headers={
            "Authorization": f"Bearer {API_KEY}",
            "Content-Type": "application/json"
        },
        json={
            "model": "gemini-3-pro-image-preview",
            "prompt": prompt,
            "image_urls": [{"url": image_url}],
            "size": "16:9"
        }
    )
    return response.json()['id']

# 第三步：轮询任务状态
def wait_for_result(task_id):
    while True:
        response = requests.get(
            f"{BASE_URL}/v1/images/generations/{task_id}",
            headers={"Authorization": f"Bearer {API_KEY}"}
        )
        result = response.json()

        if result['status'] == 'completed':
            return result['url']
        elif result['status'] == 'failed':
            raise Exception(f"生成失败: {result.get('fail_reason')}")

        time.sleep(2)

# 执行工作流
image_url = upload_image("reference.jpg")
print(f"图片已上传: {image_url}")

task_id = create_generation(image_url, "将这张照片转换为吉卜力动画风格")
print(f"任务已创建: {task_id}")

result_url = wait_for_result(task_id)
print(f"生成完成: {result_url}")
```

---

## 抓取当时的核对备注（本仓库加的，不是上游正文）

1. **示例里的 `image_urls` 形状与权威 Schema 冲突**：本例写 `[{"url": ...}]`（对象数组），
   而 `GET /v1/models/gpt-image-2.5-flare/schema` 的 `image_urls.items.type` 是 **`string`**。
   三处一致指向字符串数组：机器 Schema（`out-reference/apimart/schema-gpt-image-2.5-flare.input.json`）、
   `generation.md` 的字段说明与三个示例（`"image_urls": ["https://…"]`）、`gpt-image-2.5-generation.cn.md` 的 `string[]`。
   平台**以机器 Schema 为准**（发字符串数组）；本页的 Python 示例是唯一的例外。
2. **示例里的轮询路径不存在**：本例用 `GET /v1/images/generations/{task_id}`，
   而实测该路径 **404**、`GET /v1/tasks/{task_id}` 返回 401（存在但需鉴权）。
3. **上传失败的错误体没有 `error.code`**（只有 `type` 与 `message`），429 例外。
4. **本页说 base64 不再被生成接口接受**，而 `generation.md` 仍写「支持 `base64 data URI`，可与 URL 混填」。
   平台取**更严的一侧**：一律先上传换 URL，不在生成请求里塞 base64。
