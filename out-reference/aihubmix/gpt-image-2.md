
# aihubmix 图片模型gpt-image-2

渠道供应商aihubmix，对应的三种模型ID如：gpt-image-2;
返回格式aihubmix没有提供，可以先根据上游厂商的格式提取信息。
tests目录写好测试文件，返回信息保存到本地文件（用户调试查看实际的返回格式结构），然后由用户执行。

> **本仓库已收集的返回结构**（2026-09-19 补记，不是上游文档）：
>
> - 逐字样本：`gpt_image_2_generations.json`（第一阶段 `gpt-image-2` 同步文生图，含 2 MB `b64_json`）；
> - **各端点响应结构台账**：[`response-shapes.md`](./response-shapes.md)——同步 `/v1` 文生图与编辑、`/ai/v1` 异步任务对象、错误信封，逐项标注"逐字留存 / 当时脱敏转录 / 尚未收集"；
> - 检查后仍未收集的三处（2.5 同步、`/v1/images/edits` 独立样本、`/ai/v1` 独立样本）已在台账 §0 列明。

> 价格详情：按 Tokens 计费：文本输入 $5 / 1M tokens ｜ 文本输出 $10 / 1M tokens ｜ 图像输入 $8 / 1M tokens ｜ 图像输出 $30 / 1M tokens


### 模型 gpt-image-2 示例


示例1：
```
import base64
from openai import OpenAI

client = OpenAI(
    api_key="<AIHUBMIX_API_KEY>",  # 换成你在 AiHubMix 生成的密钥
    base_url="https://api.inferera.com/v1"
)

response = client.images.generate(
    model="gpt-image-2",
    prompt="A vase of flowers on a table, with intense contrasting colors and thick, expressive brushstrokes. Render the image so it looks painted in Fauvist style.",
    n=1, # 生成图片数量，支持1-10
    size="auto", # 图像尺寸，支持参数：1024x1024, 1024x1536, 1536x1024，auto(默认值)
    quality="auto", # 图像质量，支持参数：high, medium, low，auto(默认值）
)

image_bytes = base64.b64decode(response.data[0].b64_json)
with open("output.png", "wb") as f:
    f.write(image_bytes)
```
示例2：
 ``` 
import base64
from openai import OpenAI

client = OpenAI(
    api_key="<AIHUBMIX_API_KEY>",  # 换成你在 AiHubMix 生成的密钥
    base_url="https://api.inferera.com/v1"
)

prompt = """
Generate a photorealistic image of a gift basket on a white background 
labeled 'Relax & Unwind' with a ribbon and handwriting-like font, 
containing all the items in the reference pictures.
"""

# 确保你的当前目录下有这些图片文件
result = client.images.edit(
    model="gpt-image-2",
    image=open("body-lotion.png", "rb"),
    mask=open("mask.png", "rb"),
    prompt=prompt,
    n=1, # 生成图片数量，支持1-10
    size="auto", # 图像尺寸，支持参数：1024x1024, 1024x1536, 1536x1024，auto(默认值)
    quality="auto", # 图像质量，支持参数：high, medium, low，auto(默认值）
)

image_base64 = result.data[0].b64_json
image_bytes = base64.b64decode(image_base64)

# 将图片保存到文件
with open("gift-basket.png", "wb") as f:
    f.write(image_bytes) 
    
```

示例3：
```
curl https://api.inferera.com/v1/models/openai/gpt-image-2/predictions # Aihubmix 绘图统一接口\
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer <AIHUBMIX_API_KEY>" \
  -d '{
    "input": {
      "prompt": "A deer drinking in the lake, Sakura petals falling, green and clean water, japanese temple, dappled sunlight, cinematic lighting, expansive view, peace",
      "size": "1024x1024", 
      "n": 1,
      "quality": "high",
      "moderation": "low",
      "background": "auto"
    }
  }'
  ```

输出格式：`./gpt_image_2_generations.json`
说明： input的字段参数和openai官方一样对齐可以透传 ，openai官方参考文档：
[Create image](https://developers.openai.com/api/reference/resources/images/methods/generate/index.md)
[Create image edit](https://developers.openai.com/api/reference/resources/images/methods/edit/index.md)
[Image generation](https://developers.openai.com/api/docs/guides/image-generation?reference-images-api=responses)

### openai 兼容接口

**文生图**

```shellscript theme={null}
POST https://api.inferera.com/v1/images/generations
```

**图片编辑**

```shellscript theme={null}
POST https://api.inferera.com/v1/images/edits
```

### 请求头

```shellscript theme={null}
Authorization: Bearer $AIHUBMIX_API_KEY
Content-Type: application/json
```

### 请求参数

#### 通用参数

| 参数      | 类型      | 必填 | 说明                                                                                                                                      |
| ------- | ------- | -- | --------------------------------------------------------------------------------------------------------------------------------------- |
| prompt  | string  | 是  | 提示词                                                                                                                                     |
| size    | string  | 否  | 图像尺寸，支持`1K`(Doubao-4-5系列不支持)、`2K`、`4K`、`auto` (默认)。Qwen系列支持参数：`512*1024`、`768*512`、 `768*1024`、 `1024*576`、 `576*1024`、 `1024*1024`（默认） |
| image   | string  | 否  | 参考图片路径                                                                                                                                  |
| n       | integer | 否  | 生成图像数量，支持1-10，默认为 1。Imagen 模型该参数不生效                                                                                                     |
| quality | string  | 否  | 渲染质量，支持 `low` 、`medium` 和`high`，质量越高，耗时越长                                                                                               |

#### OpenAI 模型参数

| 参数              | 类型     | 必填 | 说明                                                    |
| --------------- | ------ | -- | ----------------------------------------------------- |
| input\_fidelity | string | 否  | 保真度，支`high`和 `low`（默认）                                |
| moderation      | string | 否  | 内容审核严格程度，支持 `auto` （默认，标准过滤）和 `low`（过滤限制较少），图生图模式下不支持 |
| output\_format  | string | 否  | 输出图片格式，支持 `png` 、 `jpeg`  （默认）  、`webp`            

#### 示例
这是 OpenAI 图片编辑模型，调用时请填写完整模型名 `gpt-image-2`，不能简写为 `image2`。该接口返回 `b64_json` 时，可按下面示例保存为 `edited.png`。
 
<CodeGroup>
  ```powershell PowerShell theme={null}
  curl.exe -sS -X POST "https://api.inferera.com/v1/images/edits" `
    -H "Authorization: Bearer YOUR_API_KEY" `
    -F "model=gpt-image-2" `
    -F "prompt=Replace the background with a blue sky and white clouds" `
    -F "image=@test.png" `
    -F "size=1024x1024" `
    -D headers.txt `
    -o response.json `
    -w "HTTP_CODE:%{http_code}`nTOTAL:%{time_total}`n"

  $b64 = (Get-Content .\response.json -Raw | ConvertFrom-Json).data[0].b64_json
  [IO.File]::WriteAllBytes(".\edited.png", [Convert]::FromBase64String($b64))
  ```

  ```typescript TypeScript theme={null}
  import { readFile, writeFile } from "node:fs/promises";

  const apiKey = process.env.AIHUBMIX_API_KEY;
  if (!apiKey) {
    throw new Error("Set AIHUBMIX_API_KEY first.");
  }

  const form = new FormData();
  form.append("model", "gpt-image-2");
  form.append("prompt", "Replace the background with a blue sky and white clouds");
  form.append("image", new Blob([await readFile("test.png")], { type: "image/png" }), "test.png");
  form.append("size", "1024x1024");

  const response = await fetch("https://api.inferera.com/v1/images/edits", {
    method: "POST",
    headers: {
      Authorization: `Bearer ${apiKey}`,
    },
    body: form,
  });

  if (!response.ok) {
    throw new Error(`${response.status} ${await response.text()}`);
  }

  const result = (await response.json()) as {
    data: Array<{ b64_json: string }>;
  };

  await writeFile("edited.png", Buffer.from(result.data[0].b64_json, "base64"));
  ```
</CodeGroup>