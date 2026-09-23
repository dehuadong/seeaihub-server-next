# OpenAI 图片生成 API 要点

> 外部参考摘要，不是本平台接口合同。内容取自 OpenAI 官方文档在 2026-09-20 的页面快照；模型、价格和限制可能变化，接入前应重新核对官方文档。

- 原文：[Image generation](https://developers.openai.com/api/docs/guides/image-generation)
- 快照日期：2026-09-20

## 模型选择

| 模型 | 适用场景 |
| --- | --- |
| `gpt-image-2.5-sunburst` | 优先保证编辑精度和生成质量 |
| `gpt-image-2.5-flare` | 优先保证速度，适合日常高质量生成 |

使用 GPT Image 模型前，组织可能需要完成 API Organization Verification。

## API 选择

| 需求 | 选择 |
| --- | --- |
| 单次文生图、参考图生成或局部编辑 | Image API |
| 多轮对话、连续改图或多步骤流程 | Responses API |

Image API 直接把 GPT Image 模型填入 `model`。Responses API 顶层 `model` 使用支持图片生成工具的主模型，再在 `image_generation` 工具的 `model` 中指定 GPT Image 模型。

## 核心能力

### Image API

- `generations`：根据文字生成图片；`n` 可指定一次生成多张，默认一张。
- `edits`：编辑现有图片、用一张或多张参考图生成新图，或配合蒙版局部编辑。
- 返回 `data[].b64_json`，即 Base64 图片数据。

最小请求示例：

```json
{
  "model": "gpt-image-2.5-sunburst",
  "prompt": "一只戴橙色围巾的水獭，儿童绘本风格",
  "size": "1024x1024",
  "quality": "medium"
}
```

### Responses API

- 支持多轮生成和连续编辑，可传入上一轮的 `previous_response_id`，也可把图片生成结果或图片 ID 放回上下文。
- 输入图片可使用公网 URL、Base64 data URL 或 Files API 返回的文件 ID。
- `action` 可设为 `auto`、`generate` 或 `edit`；默认 `auto`，由模型判断生成新图还是编辑上下文中的图片。
- 主模型会自动改写提示词，可从图片生成调用的 `revised_prompt` 查看实际使用的版本。
- 图片位于 `image_generation_call.result`，内容为 Base64。

工具配置示例：

```json
{
  "model": "支持图片生成工具的主模型",
  "input": "生成一张儿童绘本风格的水獭插画",
  "tools": [
    {
      "type": "image_generation",
      "model": "gpt-image-2.5-sunburst",
      "action": "auto"
    }
  ]
}
```

调用前应查看目标主模型的详情页，确认它支持 `image_generation` 工具。

## 编辑与蒙版

- 蒙版用于指出需要修改的区域，但模型把它当作引导，不保证严格贴合边界。
- 有多张输入图时，蒙版只作用于第一张图。
- 原图与蒙版必须格式、尺寸一致，且文件均小于 50 MB。
- 蒙版必须包含 alpha 通道。
- 提示词应描述完整的新画面，而不是只描述被替换区域。

## 输出参数

| 参数 | 说明 |
| --- | --- |
| `size` | 推荐 `1024x1024`、`1536x1024`、`1024x1536`，也支持符合限制的自定义尺寸 |
| `quality` | `low`、`medium`、`high`、`xhigh`、`max` 或 `auto`；GPT Image 2.5 默认 `auto` |
| `output_format` | `png`、`jpeg` 或 `webp`，默认 `png` |
| `output_compression` | JPEG、WebP 的压缩级别，范围 0–100 |
| `background` | `transparent`、`opaque` 或 `auto` |

自定义尺寸必须同时满足：

- 宽、高都是 16 的倍数；
- 宽高比介于 1:3 和 3:1；
- 任一边不超过 3840 像素；
- 总像素数介于 655,360 和 8,294,400；
- 超过 `2560x1440` 的分辨率仍属实验能力。

透明背景必须使用 `png` 或 `webp`。追求低延迟时优先考虑 `jpeg`；草稿可用 `quality: "low"`，最终素材再按质量、延迟和成本逐级比较。

## 流式返回

Image API 和 Responses API 都支持流式图片生成。`partial_images` 可设为 0–3：

- `0`：只返回最终图片；
- 大于 `0`：尝试返回相应数量的中间图，但生成过快时可能少于请求数量；
- 每张中间图额外计入 100 个图片输出 token。

## 错误与内容安全

所有提示词和生成结果都会经过内容安全检查。`moderation` 支持：

- `auto`：默认标准过滤；
- `low`：限制较少。

错误处理原则：

- 根据 HTTP 状态或 SDK 异常类型处理，并记录 request ID；
- 限流和服务端临时错误可退避重试；
- 配额错误和 `image_generation_user_error` 不应原样自动重试；
- `error.code` 是程序判断错误原因的稳定字段；
- `moderation_blocked` 可能附带 `moderation_details`，其中 `moderation_stage` 表示拦截发生在输入、输出或未知阶段，`categories` 提供粗粒度分类；
- 面向用户保持提示简洁，详细分类只用于日志、客服和排查。

## 成本与延迟

成本由文字输入 token、参考图输入 token、图片输出 token，以及流式中间图的额外 token 组成。Responses API 还会计入顶层主模型的 token 消耗。

快照中的 GPT Image 2.5 单价为：

| 项目 | 每百万 token |
| --- | ---: |
| 图片输入 | $8.00 |
| 缓存图片输入 | $2.00 |
| 图片输出 | $30.00 |
| 文字输入 | $5.00 |
| 缓存文字输入 | $1.25 |

实际成本应以响应中的 `usage` 和最新[官方价格页](https://developers.openai.com/api/docs/pricing)为准。相同单价不代表单张成本相同，模型、尺寸和质量都会影响 token 用量。

## 已知限制

- 复杂请求可能需要约 2 分钟。
- 文字内容、位置和清晰度仍可能不准确。
- 多次生成时，角色和品牌元素的一致性可能波动。
- 对严格布局和精确构图的控制仍有限。

## 接入检查

1. 根据是否需要多轮编辑选择 Image API 或 Responses API。
2. 明确模型、尺寸、质量、格式和背景，不依赖 `auto` 做成本估算。
3. 校验参考图、蒙版、文件大小和自定义尺寸。
4. 解码并按请求格式返回 Base64 图片数据。
5. 记录 request ID、`usage`、稳定错误码和必要的安全拦截信息。
6. 只对临时性错误退避重试，避免对用户输入错误或配额错误重复请求。
