# {{platform_name}} 图片生成与编辑

模型厂商为 {{vendor_id}}，模型类型为 `{{model_type}}`，合同修订为 `{{contract_revision}}`。支持文字生成图片、使用参考图编辑，以及带遮罩的局部编辑。调用时使用 `GET /v1/models` 返回的模型 `name`；参数可用值以对应条目的 `contract` 为准。目录字段含义与 JSON Schema 读法见 [API 使用说明]({{SEE_BASEURL}}/v1/docs/README.md)。

## 请求方式

`POST /v1/images/generations` 使用 JSON，在同一次 HTTP 响应中返回图片。

请求需要 [API Key 鉴权]({{SEE_BASEURL}}/v1/docs/authentication.md)。示例用 `$BASE_URL` 表示服务地址，用 `$API_KEY` 表示你的 API Key。

```sh
curl -X POST "$BASE_URL/v1/images/generations" \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -H "Idempotency-Key: example-request-001" \
  -d '{"model":"{{platform_name}}","prompt":"画一张暖色调的阅读角","size":"1024x1024","quality":"medium","n":1}'
```

## 参数

{{parameter_table}}

## 使用参考图与遮罩

本地图片先按 [上传参考图片]({{SEE_BASEURL}}/v1/docs/uploads/images.md)取得公网 URL，再加入生成请求。只接受公网可访问的 `http(s)` URL；本地路径、`data:` URL 和图片 base64 值不能用作参考图或遮罩。

```json
{
  "model": "{{platform_name}}",
  "prompt": "保留房间布局，把墙面改成浅绿色",
  "image": ["<参考图上传响应中的 url>"],
  "n": 1
}
```

局部编辑时，单独上传遮罩图，并加入 `"mask":"<遮罩上传响应中的 url>"`。遮罩应与参考图配合，表示希望编辑的区域。

`POST /v1/images/edits` 与上面的 `POST /v1/images/generations` 接受同一个 JSON 请求，两个地址等价，用哪个都行。

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

参数错误、图片 URL 不合法、余额不足、限流和超时的处理参见 [HTTP 状态码和错误处理]({{SEE_BASEURL}}/v1/docs/http-errors.md)。

建议为每次生成提供一个新的 `Idempotency-Key`。连接断开或结果未确认后，核对原请求时保留原键、端点和相同请求内容。`409 result_not_retained` 表示原请求已完成，图片无法重放；`502/504 outcome_unknown` 表示结果或费用尚未确认。不要自动换新键重试，换新键会成为另一笔可能收费的生成请求。
