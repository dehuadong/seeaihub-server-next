# OpenRouter 渠道供应商

## OpenAI GPT Image 2 的 SeeAI Hub 产品契约

[Create image](https://developers.openai.com/api/reference/resources/images/methods/generate/index.md)
[Create image edit](https://developers.openai.com/api/reference/resources/images/methods/edit/index.md)
[Image generation](https://developers.openai.com/api/docs/guides/image-generation?reference-images-api=responses)

OpenAI 上游可接受更广的合法 `size`，但 SeeAI Hub 对 `openai/gpt-image-2`
有独立、闭合的产品尺寸档案。调用 SeeAI Hub 时：

* `resolution` 仅能为 `1K`、`2K`、`4K`，缺省 `1K`。
* `aspect_ratio` 仅能为 `1:1`、`16:9`、`9:16`、`4:3`、`3:4`、`3:2`、`2:3`、`21:9`，缺省 `1:1`。
* `quality` 仅能为 `low`、`medium`、`high`，缺省 `low`。
* `size` 是可选公开参数：客户端可传 `1K`/`2K`/`4K` 档位，或产品尺寸档案内的显式 `WIDTHxHEIGHT`；Gateway 归一后写入内部像素 `size`，`size:auto` 与表外值返回 `invalid_value`。`resolution:auto`、`aspect_ratio:auto`、`quality:auto` 也均为非法值。

Gateway 根据产品尺寸档案唯一查表并向 OpenRouter 发送内部像素 `size`。本节不改变其他 OpenRouter 图片模型各自的 `size` 或 `auto` 能力；应以 Model Catalog 的 Effective Capability 为准。


## 计价结构（OpenRouter 官方端点实证）

`/api/v1/images/models/openai/gpt-image-2/endpoints`）返回的 `pricing` 数组**只有 3 条固定 per-token 费率、无 `variant` 分档条目**：

| billable | unit | cost_usd |
|---|---|---|
| `input_image`（含参考图输入） | token | $0.000008（= $8/M） |
| `input_text`（提示文本） | token | $0.000005（= $5/M） |
| `output_image`（生成图像） | token | $0.00003（= $30/M） |

## 输入示例：

```
curl https://openrouter.ai/api/v1/images \-H "Content-Type: application/json" \-H "Authorization: Bearer $OPENROUTER_API_KEY" \-d '{
    "model": "openai/gpt-image-2",
    "prompt": "A serene mountain landscape at sunset with dramatic clouds"
  }'
  ```
  
## 同步输出示例，：
```  
  {
    "created": 1748372400,
    "data": [
      {
        "b64_json": "<base64-encoded-image>"
      }
    ],
    "usage": {
      "completion_tokens": 4175,
      "cost": 0.04,
      "prompt_tokens": 0,
      "total_tokens": 4175
    }
  }
  ```
> 图像以 Base64 编码的形式返回。当相关信息可用时， usage 字段会显示令牌数量和成本。
> usage.cost  是实际使用成本（美元），需要折算人民币，比如 0.04*7.3
  
关联文档：

[OpenRouter 的专用图像属于文档](./图像生成.md)
[模型端点参数](./images-models.md)
历史 issues 记录：
https://github.com/dehuadong/seeaihub/issues/362
https://github.com/dehuadong/seeaihub/issues/313
