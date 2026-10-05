# API Key 鉴权

调用图片生成、编辑和上传接口时，在请求头携带你的平台 API Key：

```http
Authorization: Bearer YOUR_API_KEY
```

`GET /v1/models` 无需此请求头。

缺少凭证或格式不正确时返回 `401 authorization_required`；密钥无效或已吊销时返回 `401 invalid_api_key`。请检查密钥与请求头，不要反复提交同一个无效凭证。

JSON 图片生成请求还须携带 `Content-Type: application/json`。上传文件时，使用 `multipart/form-data`；使用 `FormData` 或 cURL 的 `-F` 时，由工具自动设置包含 boundary 的请求头。

其他失败参见 [HTTP 状态码和错误处理](http-errors.md)。
