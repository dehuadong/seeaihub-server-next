# 上传参考图片

本地参考图和遮罩图先上传，取得公网 URL，再填入模型的图片参数。已经有公网可访问的图片 URL 时，可以直接使用。

## 请求

```http
POST /v1/uploads/images
Authorization: Bearer YOUR_API_KEY
Content-Type: multipart/form-data; boundary=...
```

一次请求恰好上传一个名为 `file` 的文件部件，不要附带其他字段。

```sh
curl -X POST "$BASE_URL/v1/uploads/images" \
  -H "Authorization: Bearer $API_KEY" \
  -F "file=@./reference.png"
```

支持 JPEG、PNG、WebP。文件必须严格小于 20 MiB（20971520 字节）；空文件不支持。文件部件声明了 `Content-Type` 时，声明必须与实际内容一致。

## 成功响应

成功返回 `200`：

```json
{
  "url": "https://example.com/reference.png",
  "media_type": "image/png",
  "byte_length": 12345
}
```

| 字段 | 含义 |
| --- | --- |
| `url` | 将这张图交给模型时使用的公网 URL。 |
| `media_type` | 根据实际内容判定的图片类型。 |
| `byte_length` | 实际上传的字节数。 |

上传不收取生成费用。URL 不带签名或过期时间；任何持有该 URL 的人都可以读取图片。服务不承诺保留期限，也不提供删除或续期接口，请及时使用。

参考图和遮罩分别上传。字段名、图片数量和遮罩要求以你选择的模型说明为准。

## 上传失败

| HTTP | `error.code` | 处理方式 |
| --- | --- | --- |
| `400` | `invalid_multipart` | 检查表单编码，恰好提交一个带文件名的 `file` 部件。 |
| `400` | `unsupported_media_type` | 使用非空的 JPEG、PNG 或 WebP 图片。 |
| `400` | `media_type_mismatch` | 修正文件部件声明的类型，使其与内容一致。 |
| `408` | `request_timeout` | 请求正文上传过慢；改善连接后重新上传。 |
| `413` | `image_too_large` / `request_too_large` | 缩小文件或请求体。 |
| `429` | `rate_limit_exceeded` / `upload_busy` | 按 `Retry-After` 等待后再上传。 |
| `503` | `upload_storage_unavailable` / `object_store_unavailable` | 上传服务暂不可用；稍后再上传，或使用已有的公网图片 URL。 |

鉴权与其他错误参见 [API Key 鉴权]({{SEE_BASEURL}}/v1/docs/authentication.md)和 [HTTP 状态码和错误处理]({{SEE_BASEURL}}/v1/docs/http-errors.md)。上传未返回 URL 时，不要把本地文件路径作为图片参数提交。
