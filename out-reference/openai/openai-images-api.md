# OpenAI Images API（gpt-image-2）参数参考

> **时效标注（2026-09-20 补）**：本文**没有抓取时间戳**，正文（端点、字段、`size` 取值、模型枚举）**只覆盖到 `gpt-image-2`**，不含 2.5 两款。2026-09-20 已另抓三份带时间戳的官方快照并**明确覆盖 `gpt-image-2.5-flare` / `-sunburst`**：同目录的 `openai-images-generate-2026-09-20.md`（Create image）、`openai-images-edit-2026-09-20.md`（Create image edit）、`openai-image-generation-guide-2026-09-20.md`（使用说明）。**凡涉及 2.5 的事实以那三份为准**，本文只作家族面（gpt-image-2）的参照。

> **本文是 OpenAI 官方 API 契约，与任何网关/中转服务无关。**
> [Create image](https://developers.openai.com/api/reference/resources/images/methods/generate/index.md)、
> [Create image edit](https://developers.openai.com/api/reference/resources/images/methods/edit/index.md)
> [使用说明](https://developers.openai.com/api/docs/guides/image-generation?mask-edit-api=image)
> 及 openai/openai-python 类型定义（image_generate_params / image_edit_params）。以官方最新文档为准。

> 最新上线提供 gpt-image-2.5-flare 与 gpt-image-2.5-sunburst 两种模型；与旧模型的核心区别是支持 low（低）、medium（中）、high（高）、xhigh（超高）、max（最高）和 auto（自动）质量设置。



## 端点

| 操作 | 方法 | 路径 | 请求格式 |
|---|---|---|---|
| Create image（文生图） | `POST` | `/v1/images/generations` | JSON |
| Create image edit（图生图） | `POST` | `/v1/images/edits` | multipart/form-data |

---

## 请求参数（两端点合并对照）

| 字段 | 类型 | 必填 | 适用端点 | 说明 | 备注（差异 / 默认 / 约束） |
|---|---|---|---|---|---|
| `model` | string | 是 | 两者 | 图片生成模型 | gpt-image 系列：`gpt-image-1`/`1-mini`/`1.5`/`gpt-image-2`/`gpt-image-2-2026-04-21`；旧模型 `dall-e-2`/`dall-e-3`。**生成端点默认 `dall-e-2`，编辑端点默认 `gpt-image-1.5`** |
| `prompt` | string | 是 | 两者 | **生成**：描述要生成的画面；**编辑**：编辑指令（可含 `<image>` 引用输入图） | GPT image 模型 ≤32000 字符（dall-e-2 1000、dall-e-3 4000） |
| `image` | file / file[] | 是 | **仅 edit** | 待编辑的参考图 | `png`/`webp`/`jpg`，单张 <50MB，最多 16 张（dall-e-2 仅 1 张 <4MB 方形 png） |
| `mask` | file | 否 | **仅 edit** | 编辑范围遮罩 | 透明区 = 要编辑的区域；多图时作用于第一张；PNG <4MB 且与 `image` 同尺寸 |
| `input_fidelity` | string | 否 | **仅 edit** | 输入图保真度 | `low`（默认）/`high`；仅 gpt-image-1/1.5 及以后（mini 不支持）；`high` 显著增加输入 token |
| `n` | integer | 否 | 两者 | 生成张数 | 1–10（dall-e-3 仅 1）；默认 1 |
| `size` | string | 否 | 两者 | 输出尺寸 | 标准 `1024x1024`/`1536x1024`/`1024x1536` 或 `auto`；gpt-image-2 支持任意 `WIDTHxHEIGHT`（见 §尺寸） |
| `quality` | string | 否 | 两者 | 质量档 | **默认 `auto`**；`low`/`medium`/`high`；`hd`/`standard` 仅 dall-e-3（**仅生成端点**）；编辑端点 `standard` 为兼容遗留、无 `hd` |
| `background` | string | 否 | 两者 | 输出背景透明度 | 默认 `auto`；`auto`/`opaque`/`transparent`；透明需 `output_format=png`/`webp`，gpt-image-2 为 preview |
| `output_format` | string | 否 | 两者 | 输出编码 | 默认 `png`；`png`/`jpeg`/`webp`（仅 GPT image 模型） |
| `output_compression` | integer | 否 | 两者 | 输出压缩度 | 0–100；**默认 100**；仅 GPT image + `webp`/`jpeg` 生效 |
| `moderation` | string | 否 | 两者 | 内容审核等级 | 默认 `auto`；`low` 为更宽松过滤（仅 GPT image） |
| `partial_images` | integer | 否 | 两者 | 流式部分图数量 | 0–3（0 为单张一次事件）；仅流式响应 |
| `stream` | boolean | 否 | 两者 | 流式返回 | 默认 `false`；仅 GPT image 模型支持 |
| `response_format` | string | 否 | 两者（旧模型） | 返回形态 | `url`/`b64_json`；**仅 dall-e 系列**——生成端点 dall-e-2/3 可用，编辑端点仅 dall-e-2（默认 `url`）；GPT image 模型不支持该参数，恒返回 base64 |
| `style` | string | 否 | **仅 generate** | 风格 | `vivid`/`natural`；仅 dall-e-3 |
| `user` | string | 否 | 两者 | 终端用户标识 | 用于滥用监控 |

### 关键差异速览

- **输入图**：生成端点不传任何图片；编辑端点**必填 `image`**（多图可用 `image[]`），并多出 `mask`、`input_fidelity` 两个可选参数。
- **请求编码**：生成端点 JSON；编辑端点 multipart/form-data（`image` 等为文件部件）。
- **`prompt` 语义**：生成 = 画面描述；编辑 = 编辑指令（`<image>` 引用输入图）。
- **参数集合**：`style` 仅生成端点（dall-e-3）；`response_format` 支持范围与默认不同（见上表）；`quality` 枚举在编辑端点无 `hd`。
- **其余参数**（`n`/`size`/`background`/`output_format`/`output_compression`/`moderation`/`partial_images`/`stream`/`user`）两端点一致。

### 请求示例

**Create image（JSON）：**

```json
{
  "model": "gpt-image-2",
  "prompt": "A vase of flowers on a table",
  "n": 1,
  "size": "1024x1024",
  "quality": "auto",
  "background": "auto",
  "output_format": "png",
  "moderation": "auto"
}
```

**Create image edit（multipart 表单）：**

```
image:   <file part, image-1.png ... image-2.png>
prompt:  将图1的服装换为图2的服装
model:   gpt-image-2
n:       1
size:    1024x1024
quality: auto
```

---

## 响应输出格式（HTTP 200，两端点一致）

```json
{
  "created": 1785485861,
  "data": [
    { "b64_json": "iVBORw0KGgoAAAANSUhEUg..." }
  ],
  "usage": {
    "input_tokens": 13,
    "input_tokens_details": { "image_tokens": 0, "text_tokens": 13 },
    "output_tokens": 196,
    "output_tokens_details": { "image_tokens": 196, "text_tokens": 0 },
    "total_tokens": 209
  }
}
```

| 字段 | 类型 | 说明 | 备注 |
|---|---|---|---|
| `created` | integer | 生成时间（Unix 秒） | — |
| `data` | array | 图片结果数组 | 长度 = 请求 `n` |
| `usage` | object | token 用量明细 | GPT image 系列返回（token 计费依据）；编辑端点的 `input_tokens_details.image_tokens` 反映参考图折算 |

**`data[]` 项：**

| 字段 | 类型 | 说明 | 备注 |
|---|---|---|---|
| `b64_json` | string | 图片 Base64 编码 | GPT image 模型恒返回 base64 |
| `url` | string | 图片 URL | 仅 dall-e 系列（`response_format=url` 时，URL 60 分钟有效） |
| `revised_prompt` | string | 服务端改写后的提示词 | 可选 |
| `error` | object | 该项生成失败信息 | 存在时该项不代表成功图片 |

**`usage` 明细：**

| 字段 | 类型 | 说明 | 备注 |
|---|---|---|---|
| `input_tokens` | integer | 输入 token 总数 | GPT image 系列返回 |
| `input_tokens_details.text_tokens` | integer | 输入文本 token | — |
| `input_tokens_details.image_tokens` | integer | 输入图片 token | 图生图按输入图折算 |
| `output_tokens` | integer | 输出 token 总数 | — |
| `output_tokens_details.image_tokens` | integer | 输出图片 token | — |
| `output_tokens_details.text_tokens` | integer | 输出文本 token | 图片模型通常为 0 |
| `total_tokens` | integer | 总 token 数 | — |

---

## 尺寸与约束（gpt-image-2）

| 主题 | 要求 |
|---|---|
| 任意分辨率 | 支持 `WIDTHxHEIGHT` 字符串（如 `1536x864`）；宽、高均须为 **16 的倍数** |
| 长宽比 | 请求比例须在 **1:3 与 3:1** 之间 |
| 上限 | 最大分辨率 **`3840x2160`**；**高于 `2560x1440` 为 experimental**（不保证稳定） |
| 像素/边长 | 须满足模型当前的像素与边长限制 |
| 标准尺寸 | `1024x1024`、`1536x1024`、`1024x1536`（GPT image 模型支持） |
| 自动尺寸 | `auto`：允许自动选尺寸的模型可用 |
| dall-e 尺寸 | dall-e-2：`256x256`/`512x512`/`1024x1024`；dall-e-3：`1024x1024`/`1792x1024`/`1024x1792` |

### gpt-image-2 常见尺寸（按比例 × 档位）

> gpt-image-2 支持**任意 `WIDTHxHEIGHT`**（宽高均为 16 倍数、比例 1:3~3:1、总像素与
> 长边满足模型上限）；下表为常见取值（`1024x1024`/`1536x1024`/`1024x1536` 为官方标准
> 尺寸，其余为满足约束的常见示例，并非封闭枚举）；`-` 表示该档位无此比例。

| 比例 | 1K | 2K | 4K |
|---|---|---|---|
| 1:1 | `1024x1024`、`1536x1536` | `2048x2048` | - |
| 3:2 / 2:3 | `1536x1024` / `1024x1536` | `2048x1360` / `1360x2048` | - |
| 16:9 / 9:16 | `1536x864` / `864x1536` | `2048x1152` / `1152x2048` | `3840x2160` / `2160x3840` |
| 4:3 / 3:4 | `1024x768` / `768x1024` | `2048x1536` / `1536x2048` | - |
| 21:9 | `2016x864` | `2688x1152` | `3840x1648` |

> 4K 档（如 `3840x2160`、`2160x3840`）高于 `2560x1440`，为 experimental，不保证稳定。

> 旧模型 `style`、`response_format` 等参数与 GPT image 系列无关；向 gpt-image 模型传入
> 其不支持参数时，官方建议去掉该参数后重试。

## 错误结构

| 项目 | 说明 |
|---|---|
| 错误响应体 | `{"error": {"message": "...", "type": "...", "param": "...", "code": "..."}}` |
| 常见错误 | 400（参数非法/模型不支持某参数）、401（认证失败）、429（限流）、500/503（服务端） |
| 失败建议 | 若因某选项不被所选模型支持而失败，去掉该选项后重试 |