# API 使用说明

先查询模型，选择需要的模型，再阅读该模型的使用说明。下面的示例用 `$BASE_URL` 表示服务地址，用 `$API_KEY` 表示你的 API Key。

## 查询模型

`GET /v1/models` 无需鉴权。

```sh
curl "$BASE_URL/v1/models"
```

成功返回 `200`，模型列表位于 `data`；没有可用模型时为 `{"data":[]}`。

| 字段 | 含义 |
| --- | --- |
| `name` | 调用时填写的模型名称。请使用返回值。 |
| `vendor_id` | 模型所属厂商，例如 `OpenAI`。 |
| `type` | 模型类型：`image` 是图片，`video` 是视频，`chat` 是对话。 |
| `revision` | 这份模型参数说明的合同修订号，用于识别参数规则的变化。 |
| `contract` | JSON Schema，描述该模型允许的参数及其结构和限制。 |

厂商、模型类型和调用方式是不同的信息。请按对应模型的使用说明选择端点和请求编码。

## 阅读参数规则

| JSON Schema 字段 | 含义 |
| --- | --- |
| `properties` | 各参数的定义。 |
| `required` | 必须填写的参数名。 |
| `type` | 值的类型，例如字符串 `string`、整数 `integer`、数组 `array`。 |
| `const` / `enum` | 固定值 / 可选值。 |
| `minimum` / `maximum` | 数值的下界 / 上界。 |
| `minLength` / `maxLength` | 字符串长度限制。 |
| `items` / `minItems` / `maxItems` | 数组元素定义及数量限制。 |
| `pattern` | 字符串须符合的格式。 |
| `default` | 默认值的说明；需要明确的设置时请主动传入该值。 |
| `allOf` / `anyOf` / `oneOf` | 须同时满足所有规则 / 至少满足一条 / 恰好满足一条。 |
| `if` / `then` / `else` | 不同条件下的参数要求。 |

参数含义和完整示例请阅读具体模型的说明。只提交该模型支持的字段；未支持的普通字段可能被忽略。

## 使用说明

- [OpenAI GPT-Image-2.5 图片生成与编辑](models/openai/gpt-image-2.5.md)
- [API Key 鉴权](authentication.md)
- [上传参考图片](uploads/images.md)
- [HTTP 状态码和错误处理](http-errors.md)
