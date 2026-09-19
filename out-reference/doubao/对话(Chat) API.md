# Chat Completions API 文档

## 接口信息

```
POST https://ark.cn-beijing.volces.com/api/v3/chat/completions
```

发送包含文本、图片、视频、音频等模态的消息列表，模型将生成对话中的下一条消息。

---

## 鉴权

本接口支持以下鉴权方式，详情请参见 [Base URL 及鉴权](https://www.volcengine.com/docs/82379/1298459?lang=zh)：

- **【推荐】API Key 鉴权**：请在 [API Key 管理](https://console.volcengine.com/ark/region:ark+cn-beijing/apiKey) 页面获取长效 API Key。
- **【可选】Access Key 鉴权**：请在 [Access Key 管理](https://console.volcengine.com/iam/keymanage) 页面获取 Access Key。

---

## 请求参数

### Body 参数

| 参数 | 类型 | 是否必选 | 说明 |
|------|------|----------|------|
| `model` | `string` | ✅ 必选 | 调用的模型 ID（Model ID）|
| `messages` | `object[]` | ✅ 必选 | 消息列表，不同模型支持不同类型的消息，如文本、图片、视频、音频等 |
| `thinking` | `object` | 可选 | 控制模型是否开启深度思考模式，默认 `{"type":"enabled"}` |
| `stream` | `boolean / null` | 可选 | 是否流式返回，默认 `false` |
| `stream_options` | `object / null` | 可选 | 流式响应的选项，当 `stream` 为 `true` 时可设置 |
| `max_tokens` | `integer / null` | 可选 | 模型回答最大长度（单位 token），默认 `4096` |
| `max_completion_tokens` | `integer / null` | 可选 | 控制模型输出的最大长度（包括回答和思维链），不可与 `max_tokens` 同时设置 |
| `service_tier` | `string / null` | 可选 | 控制使用的在线推理模式，默认 `auto`。取值：`fast`、`auto`、`default` |
| `stop` | `string / string[] / null` | 可选 | 停止生成字符串，最多 4 个，默认 `null` |
| `reasoning_effort` | `string / null` | 可选 | 限制思考的工作量，默认 `medium`。取值：`minimal`、`low`、`medium`、`high`、`max` |
| `response_format` | `object` | 可选 | 指定模型回答格式，默认 `{"type":"text"}`（beta 阶段） |
| `frequency_penalty` | `float / null` | 可选 | 频率惩罚系数，取值范围 [-2.0, 2.0]，默认 `0` |
| `presence_penalty` | `float / null` | 可选 | 存在惩罚系数，取值范围 [-2.0, 2.0]，默认 `0` |
| `temperature` | `float / null` | 可选 | 采样温度，取值范围 [0, 2]，默认 `1` |
| `top_p` | `float / null` | 可选 | 核采样概率阈值，取值范围 [0, 1]，默认 `0.7` |
| `logprobs` | `boolean / null` | 可选 | 是否返回输出 tokens 的对数概率，默认 `false` |
| `top_logprobs` | `integer / null` | 可选 | 每个输出 token 位置最有可能返回的 token 数量，取值范围 [0, 20]，默认 `0` |
| `logit_bias` | `map / null` | 可选 | 调整指定 token 在输出中出现的概率，默认 `null` |
| `tools` | `object[] / null` | 可选 | 待调用工具的列表，默认 `null` |
| `parallel_tool_calls` | `boolean` | 可选 | 是否允许返回多个待调用的工具，默认 `true` |
| `tool_choice` | `string / object` | 可选 | 控制模型返回是否包含待调用的工具 |

---

### messages 消息类型

#### 系统消息（System Message）

模型需遵循的指令，包括扮演的角色、背景信息等。

| 参数 | 类型 | 是否必选 | 说明 |
|------|------|----------|------|
| `role` | `string` | ✅ 必选 | 发送消息的角色，此处应为 `system` |
| `content` | `string / object[]` | ✅ 必选 | 系统消息的内容 |

**content 内容类型：**

- **纯文本内容**：`string` 类型，纯文本消息内容
- **多模态内容**：`object[]` 类型，支持文本、图片、视频、音频等模态

---

#### 用户消息（User Message）

用户角色发送的消息。不同模型支持的字段类型不同。

| 参数 | 类型 | 是否必选 | 说明 |
|------|------|----------|------|
| `role` | `string` | ✅ 必选 | 发送消息的角色，此处应为 `user` |
| `content` | `string / object[]` | ✅ 必选 | 用户信息内容 |

---

#### 模型消息（Assistant Message）

历史对话中，模型角色返回的消息。用以保持对话一致性，多在多轮对话及续写模式使用。

> **说明：** `messages.content` 与 `messages.tool_calls` 至少填写其一。

| 参数 | 类型 | 是否必选 | 说明 |
|------|------|----------|------|
| `role` | `string` | ✅ 必选 | 发送消息的角色，此处应为 `assistant` |
| `content` | `string / array` | 可选 | 模型消息的内容 |
| `reasoning_content` | `string` | 可选 | 模型消息中思维链内容（仅 `doubao-seed-1.8`、`deepseek-v3.2`、`doubao-seed-2.0` 支持） |
| `encrypted_content` | `string` | 可选 | 经加密及压缩处理后的思考内容原文（自 `doubao-seed-2-0-lite-260428` 起支持） |
| `tool_calls` | `object[]` | 可选 | 模型消息中工具调用部分 |

---

#### 工具消息（Tool Message）

历史对话中，调用工具返回的消息。工具调用场景中使用。

| 参数 | 类型 | 是否必选 | 说明 |
|------|------|----------|------|
| `role` | `string` | ✅ 必选 | 发送消息的角色，此处应为 `tool` |
| `content` | `string / array` | ✅ 必选 | 工具返回的消息 |
| `tool_call_id` | `string` | ✅ 必选 | 模型生成的需调用工具请求时生成的 ID |

---

### 多模态内容对象

#### 文本部分（Text）

| 参数 | 类型 | 是否必选 | 说明 |
|------|------|----------|------|
| `type` | `string` | ✅ 必选 | 内容模态，此处应为 `text` |
| `text` | `string` | ✅ 必选 | 文本模态部分的内容 |

---

#### 图片部分（Image）

图片输入支持 `file_id` 和 `url` 两个字段，需二选一传入。详见[图片理解说明](https://www.volcengine.com/docs/82379/1362931?lang=zh)。

| 参数 | 类型 | 是否必选 | 说明 |
|------|------|----------|------|
| `type` | `string` | ✅ 必选 | 内容模态，此处应为 `image_url` |
| `image_url` | `object` | ✅ 必选 | 图片模态的内容 |

**image_url 属性：**

| 参数 | 类型 | 说明 |
|------|------|------|
| `file_id` | `string` | 文件 ID（通过 Files API 上传后返回），需与 API Key 所属项目一致 |
| `url` | `string` | 图片 URL 或 Base64 编码 |
| `detail` | `string` | 精细度，取值：`low`、`high`、`xhigh` |
| `image_pixel_limit` | `object / null` | 图片像素范围，默认 `null`，优先级高于 `detail` |

**image_pixel_limit 属性：**

| 参数 | 类型 | 说明 |
|------|------|------|
| `max_pixels` | `integer` | 最大像素限制 |
| `min_pixels` | `integer` | 最小像素限制 |

> **注意：**
> - 图片像素范围需在 [196, 36,000,000] 之间，否则会直接报错。
> - Chat API 支持通过 `file_id` 传入火山引擎 TOS Bucket 中的文件（doubao-seed-2.0-mini-260428 及后续版本、doubao-seed-2.0-lite 全版本、doubao-seed-2.0-pro 全版本支持）。

---

#### 视频部分（Video）

视频输入支持 `file_id` 和 `url` 两个字段，需二选一传入。详见[视频理解说明](https://www.volcengine.com/docs/82379/1895586)。

| 参数 | 类型 | 是否必选 | 说明 |
|------|------|----------|------|
| `type` | `string` | ✅ 必选 | 内容模态，此处应为 `video_url` |
| `video_url` | `object` | ✅ 必选 | 视频模态的内容 |

**video_url 属性：**

| 参数 | 类型 | 说明 |
|------|------|------|
| `file_id` | `string` | 文件 ID（通过 Files API 上传后返回） |
| `url` | `string` | 视频 URL 或 Base64 编码 |
| `fps` | `float / null` | 抽帧频率，取值范围 [0.2, 5]，默认 `1` |

---

#### 音频部分（Audio）

音频输入支持 `file_id`、`url`、`data` 三个字段，需三选一传入。详见[音频理解](https://www.volcengine.com/docs/82379/2377589?lang=zh)。

| 参数 | 类型 | 是否必选 | 说明 |
|------|------|----------|------|
| `type` | `string` | ✅ 必选 | 内容模态，此处应为 `input_audio` |
| `input_audio` | `object` | ✅ 必选 | 音频模态的内容 |

**input_audio 属性：**

| 参数 | 类型 | 说明 |
|------|------|------|
| `file_id` | `string` | 文件 ID（通过 Files API 上传后返回） |
| `url` | `string` | 音频内容的 URL |
| `data` | `string` | 音频内容的 Base64 编码 |
| `format` | `string` | 音频格式（使用 `data` 时必填） |

**支持的音频格式：**

| 格式 | MIME 类型 |
|------|-----------|
| mp3 | `audio/mpeg` |
| wav | `audio/wav` |
| aac | `audio/aac` |
| m4a | `audio/m4a` |
| pcm | `audio/L16` |
| ac3 | `audio/ac3` |
| alac | `audio/m4a` |

> **注意：**
> - 文件大小不超过 25 MB。
> - 单次请求音频总时长不超过 120 分钟（仅统计纯音频时长，视频内嵌音频不计入）。

---

#### 文件部分（File）

当前仅支持 PDF 文件。输入支持 `file_id`、`file_data`、`file_url` 三个字段，需三选一传入。详见[文档理解](https://www.volcengine.com/docs/82379/1902647?lang=zh)。

| 参数 | 类型 | 是否必选 | 说明 |
|------|------|----------|------|
| `type` | `string` | ✅ 必选 | 消息模态，此处应为 `file` |
| `file` | `object` | ✅ 必选 | 文件模态的内容 |

**file 属性：**

| 参数 | 类型 | 说明 |
|------|------|------|
| `file_id` | `string` | 文件 ID（通过 Files API 上传后返回） |
| `file_data` | `string` | 文件内容的 Base64 编码，单个文件不超过 50 MB |
| `filename` | `string` | 文件名（使用 `file_data` 时必填） |
| `file_url` | `string` | 文件的可访问 URL，文件大小不超过 50 MB |

---

### 其他请求参数详解

#### thinking（深度思考）

| 参数 | 类型 | 是否必选 | 说明 |
|------|------|----------|------|
| `type` | `string` | ✅ 必选 | 取值：`enabled`（开启）、`disabled`（关闭）、`auto`（自动） |

#### stream_options（流式选项）

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `include_usage` | `boolean / null` | `false` | 是否在输出结束前返回 token 用量信息 |
| `chunk_include_usage` | `boolean / null` | `false` | 是否在每个 chunk 中返回累计 token 用量 |

#### response_format（回答格式）

| 参数 | 类型 | 说明 |
|------|------|------|
| `type` | `string` | 取值：`text`（文本格式）、`json_object`（JSON 对象格式）、`json_schema`（JSON Schema 格式，beta 阶段） |
| `json_schema` | `object` | JSON 结构体定义（当 `type` 为 `json_schema` 时必填） |

**json_schema 属性：**

| 参数 | 类型 | 是否必选 | 说明 |
|------|------|----------|------|
| `name` | `string` | ✅ 必选 | 用户自定义的 JSON 结构名称 |
| `description` | `string / null` | 可选 | 回复用途描述 |
| `schema` | `object` | ✅ 必选 | JSON Schema 对象定义 |
| `strict` | `boolean / null` | 可选 | 是否严格遵循 schema，默认 `false` |

#### tools（工具调用）

| 参数 | 类型 | 是否必选 | 说明 |
|------|------|----------|------|
| `type` | `string` | ✅ 必选 | 工具类型，此处应为 `function` |
| `function` | `object` | ✅ 必选 | 函数定义 |

**function 属性：**

| 参数 | 类型 | 是否必选 | 说明 |
|------|------|----------|------|
| `name` | `string` | ✅ 必选 | 函数名称 |
| `description` | `string` | 可选 | 函数描述 |
| `parameters` | `object` | 可选 | JSON Schema 格式的参数定义 |

**parameters 示例：**
```json
{
  "type": "object",
  "properties": {
    "参数名": {
      "type": "string | number | boolean | object | array",
      "description": "参数说明"
    }
  },
  "required": ["必填参数"]
}
```

#### tool_choice（工具选择）

| 类型 | 取值/参数 | 说明 |
|------|-----------|------|
| **选择模式（string）** | `none` | 模型返回不可含有待调用工具 |
| | `required` | 模型返回必须含有待调用工具 |
| | `auto` | 模型自行判断（默认） |
| **工具调用（object）** | `type: "function"` | 指定待调用工具范围 |
| | `function.name` | 待调用工具名称 |

---

## 响应参数

### 非流式调用响应

| 参数 | 类型 | 说明 |
|------|------|------|
| `id` | `string` | 本次请求的唯一标识 |
| `model` | `string` | 本次请求实际使用的模型名称和版本 |
| `service_tier` | `string` | 请求使用的模式：`scale`（TPM 保障包）、`default`（常规）、`fast`（低延迟） |
| `created` | `integer` | 请求创建时间的 Unix 时间戳（秒） |
| `object` | `string` | 固定为 `chat.completion` |
| `choices` | `object[]` | 模型输出内容 |
| `usage` | `object` | token 用量信息 |

#### choices 对象

| 参数 | 类型 | 说明 |
|------|------|------|
| `index` | `integer` | 列表索引 |
| `finish_reason` | `string` | 停止原因：`stop`、`length`、`content_filter`、`tool_calls` |
| `message` | `object` | 模型输出的消息内容 |
| `logprobs` | `object / null` | 对数概率信息 |
| `moderation_hit_type` | `string / null` | 命中的风险分类标签（视觉理解模型 + 基础内容护栏方案返回） |

#### message 对象

| 参数 | 类型 | 说明 |
|------|------|------|
| `role` | `string` | 固定为 `assistant` |
| `content` | `string` | 模型生成的消息内容 |
| `reasoning_content` | `string / null` | 思维链内容（深度推理模型支持） |
| `tool_calls` | `object[] / null` | 模型生成的工具调用 |

#### tool_calls 对象

| 参数 | 类型 | 说明 |
|------|------|------|
| `id` | `string` | 工具调用 ID |
| `type` | `string` | 固定为 `function` |
| `function` | `object` | 调用的函数信息 |
| `function.name` | `string` | 函数名称 |
| `function.arguments` | `string` | JSON 格式的函数参数 |

#### usage 对象

| 参数 | 类型 | 说明 |
|------|------|------|
| `total_tokens` | `integer` | 总 token 数量（输入 + 输出） |
| `prompt_tokens` | `integer` | 输入 token 数量 |
| `prompt_tokens_details` | `object` | 输入 token 详情 |
| `completion_tokens` | `integer` | 输出 token 数量 |
| `completion_tokens_details` | `object` | 输出 token 详情 |

**prompt_tokens_details 属性：**

| 参数 | 类型 | 说明 |
|------|------|------|
| `cached_tokens` | `integer` | 缓存命中的输入 token 总数 |
| `audio_tokens` | `integer` | 音频输入 token 数量 |
| `audio_cached_tokens` | `integer` | 缓存命中的音频 token 数量 |

**completion_tokens_details 属性：**

| 参数 | 类型 | 说明 |
|------|------|------|
| `reasoning_tokens` | `integer` | 思维链输出 token 数量 |

---

### 流式调用响应

流式调用返回 SSE 协议格式的数据，结构与非流式类似，主要差异如下：

| 参数 | 类型 | 说明 |
|------|------|------|
| `object` | `string` | 固定为 `chat.completion.chunk` |
| `choices` | `object[]` | 包含 `delta` 而非 `message` |
| `usage` | `object / null` | 默认返回 `null`，需设置 `stream_options.include_usage: true` 才返回 |

#### delta 对象

| 参数 | 类型 | 说明 |
|------|------|------|
| `role` | `string` | 固定为 `assistant` |
| `content` | `string` | 增量内容 |
| `reasoning_content` | `string / null` | 思考内容（深度推理模型支持） |
| `encrypted_content` | `string` | 加密压缩后的思考内容（自 `doubao-seed-2-0-lite-260428` 起支持） |
| `tool_calls` | `object[] / null` | 工具调用 |

> **注意：** 针对长文本生成、深度推理等耗时场景，建议适当调大首 Token 超时时间（TTFT）与逐 Token 生成超时时间（TPOT），避免请求因超时而中断。

---

### finish_reason 取值说明

| 取值 | 说明 |
|------|------|
| `stop` | 自然结束或因命中 `stop` 参数截断 |
| `length` | 因达到 `max_tokens`、`max_completion_tokens` 或上下文长度限制而截断 |
| `content_filter` | 被内容审核拦截 |
| `tool_calls` | 模型调用了工具 |

### moderation_hit_type 取值说明

| 取值 | 说明 |
|------|------|
| `severe_violation` | 涉及严重违规 |
| `violence` | 涉及激进行为 |

> **注意：** 当前仅视觉理解模型支持返回该字段，且需在控制台或 CreateEndpoint 接口中将内容护栏方案设置为「基础方案（Basic）」。