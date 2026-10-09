# HTTP 状态码和错误处理

错误响应通常使用以下结构：

```json
{
  "error": {
    "code": "invalid_parameter",
    "message": "..."
  }
}
```

请按 HTTP 状态与 `error.code` 判断处理方式，`message` 用于说明原因。部分请求格式错误或代理返回的响应可能是普通文本，客户端应兼容非 JSON 错误响应。

下表适用于模型查询、图片生成、编辑和图片上传；其他模型类型请同时阅读各自的使用说明。

| HTTP | 适用接口与错误码 | 含义与处理 |
| --- | --- | --- |
| `200` | 模型查询、上传、生成 | 请求成功。 |
| `400` | 生成：`missing_model`、`invalid_parameter`、`validation_error` | 请求格式或参数不符合要求，检查模型名、必填字段和参数组合。 |
| `400` | 生成：`public_image_url_required` | 参考图与遮罩必须是公网 `http(s)` URL。 |
| `400` | 生成：`content_rejected` | 提交的内容被拒绝，修改内容后再提交。 |
| `400` | 上传：`invalid_multipart`、`unsupported_media_type` | 按上传说明修正表单与图片文件。 |
| `401` | 上传、生成：`authorization_required`、`invalid_api_key` | 检查 `Authorization: Bearer ...` 与密钥有效性。 |
| `402` | 生成：`insufficient_balance` | 余额不足，充值后再提交。 |
| `404` | 生成：`not_found` | 模型不存在或当前不可调用，重新查询目录。 |
| `408` | 上传、生成：`request_timeout` | 请求正文读取超时。 |
| `409` | 生成：`request_in_progress` | 同一幂等键的原请求仍在执行，按 `Retry-After` 等待后核对。 |
| `409` | 生成：`idempotency_conflict` | 同一幂等键对应的请求内容或端点发生变化，请核对原请求。 |
| `409` | 生成：`result_not_retained` | 原请求已完成并结算，图片无法重放；新键代表新的计费请求。 |
| `413` | 上传：`image_too_large`、`request_too_large`；生成请求体也可能过大 | 缩小文件或请求体。 |
| `415` | JSON 生成入口 | 检查 `Content-Type: application/json`。响应可能是普通文本。 |
| `429` | 生成：`too_many_in_flight`；上传：`upload_busy`；公开登录类端点：`rate_limit_exceeded` | 并发已满（按模型）或尝试过密（公开登录类端点）；响应带 `Retry-After` 时按其秒数等待。 |
| `500` | 查询、上传、生成：`internal_error` | 服务内部故障。生成失败时保存原幂等键，避免重复提交。 |
| `502` | 生成：`outcome_unknown` | 结果或费用未确认，保持原幂等键，不要自动换键重试。 |
| `502` | 生成：`platform_unavailable` | 本次生成未能完成。 |
| `503` | 生成：`platform_unavailable`；上传：`upload_storage_unavailable`、`object_store_unavailable` | 当前服务不可用，稍后再试。 |
| `504` | 生成：`request_timeout` | 超时前未发起生成。 |
| `504` | 生成：`outcome_unknown` | 超时前未能确认结果，可能已产生费用；不要自动换键重试。 |
| `504` | 生成：`result_delivery_timeout` | 已完成并收费，但图片未能及时返回，结果无法重放；不要自动换键重试。 |

断开连接或收到超时响应不等于没有产生费用。核对同一次生成请求时，应保持原幂等键、端点和相同内容。
