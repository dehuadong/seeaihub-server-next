# OpenAI GPT-Image-2.5 图片生成与编辑

模型厂商为 OpenAI，模型类型为 `image`。支持文字生成图片、使用参考图编辑，以及带遮罩的局部编辑。调用时使用 `GET /v1/models` 返回的模型 `name`；参数可用值以对应条目的 `contract` 为准。

## 请求方式

`POST /v1/images/generations` 使用 JSON，在同一次 HTTP 响应中返回图片。

请求需要 [API Key 鉴权](../../authentication.md)。示例用 `$BASE_URL` 表示服务地址，用 `$API_KEY` 表示你的 API Key。

```sh
curl -X POST "$BASE_URL/v1/images/generations" \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -H "Idempotency-Key: example-request-001" \
  -d '{"model":"<目录返回的 name>","prompt":"画一张暖色调的阅读角","size":"1024x1024","quality":"medium","n":1}'
```

## 参数

| 字段 | 含义与填写方式 |
| --- | --- |
| `model` | 必填。目录返回的 `name`。 |
| `prompt` | 必填。描述希望生成的图片，或希望修改与保留的内容；非空，最多 32000 字符。 |
| `n` | 请求生成的张数，整数 `1`–`10`，默认说明为 `1`。响应可能少于请求张数，请按实际 `data` 读取结果。 |
| `size` | 输出尺寸：`auto` 或 `宽x高`，例如 `1024x1024`。宽高须为 16 的倍数，宽高比在 1:3 到 3:1 之间；最大尺寸说明为 `3840x2160`，高于 `2560x1440` 属实验范围。 |
| `quality` | 输出质量：`low`、`medium`、`high`、`xhigh`、`max`、`auto`；默认说明为 `auto`。 |
| `output_format` | 图片格式：`png`、`jpeg`、`webp`；默认说明为 `png`。 |
| `output_compression` | JPEG/WebP 压缩百分比，整数 `0`–`100`；仅在 `output_format` 为 `jpeg` 或 `webp` 时使用。 |
| `background` | `auto` 自动选择、`opaque` 不透明、`transparent` 透明；默认说明为 `auto`。透明背景须配 `png` 或 `webp`。 |
| `moderation` | 内容审核强度：`auto` 默认审核、`low` 较宽松审核；默认说明为 `auto`。 |
| `image` | 可选。参考图公网 URL 数组，`1`–`16` 张。 |
| `mask` | 可选。遮罩图公网 URL；使用时必须同时填写 `image`。 |

需要确定的质量、格式和背景设置时，请显式传入相应参数。`output_format` 指图片格式，不决定响应是 URL 还是 base64。

## 使用参考图与遮罩

本地图片先按 [上传参考图片](../../uploads/images.md)取得公网 URL，再加入生成请求。只接受公网可访问的 `http(s)` URL；本地路径、`data:` URL 和图片 base64 值不能用作参考图或遮罩。

```json
{
  "model": "<目录返回的 name>",
  "prompt": "保留房间布局，把墙面改成浅绿色",
  "image": ["<参考图上传响应中的 url>"],
  "n": 1
}
```

局部编辑时，单独上传遮罩图，并加入 `"mask":"<遮罩上传响应中的 url>"`。遮罩应与参考图配合，表示希望编辑的区域。

也可使用 `POST /v1/images/edits` 的 multipart 表单入口，参考图和遮罩填写 URL 文本。多张参考图建议使用上述 JSON 入口；不要在编辑表单中直接附图片文件。

## 成功响应

成功返回 `200`。例如：

```json
{
  "created": 1791158400,
  "data": [{"url": "https://example.com/result.png"}]
}
```

`created` 是 Unix 秒时间戳。`data` 中每项包含 `url` 或 `b64_json` 之一：有 `url` 时读取该地址，有 `b64_json` 时按 base64 解码为图片。请兼容两种结果形式，并及时保存结果。服务不提供生成结果的保存与重放。

## 错误与重复请求

参数错误、图片 URL 不合法、余额不足、限流和超时的处理参见 [HTTP 状态码和错误处理](../../http-errors.md)。

建议为每次生成提供一个新的 `Idempotency-Key`。连接断开或结果未确认后，核对原请求时保留原键、端点和相同请求内容。`409 result_not_retained` 表示原请求已完成，图片无法重放；`502/504 outcome_unknown` 表示结果或费用尚未确认。不要自动换新键重试，换新键会成为另一笔可能收费的生成请求。
