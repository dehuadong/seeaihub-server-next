> ## Documentation Index
> Fetch the complete documentation index at: https://api.inferera.com/llms.txt
> Use this file to discover all available pages before exploring further.

# HTTP 状态码

> 关键错误代码映射表

<Note>
  * 400 状态码通常是传参错误，请查看接口文档。大部分 400 错误是上游透传的报错。
  * 错误响应中的 response ID 已更改为 tid (traceId)，用于错误追踪和问题定位。
</Note>

| 状态码 | 错误标识符                     | 错误消息                                                                                                      | 常见原因                |
| :-: | ------------------------- | --------------------------------------------------------------------------------------------------------- | ------------------- |
| 503 | -                         | Incorrect model ID. Please request to view the model page or you do not have permission to use this model | 没有可用的渠道处理请求         |
| 503 | -                         | Rate limited by provider – contact support to request higher concurrency or try again later.              | 模型遇到官方限速            |
| 429 | -                         | The xx（xx: model id） model Too many requests; please try again later.                                     | 请求频率超过限制            |
| 403 | insufficient\_user\_quota | Your account balance is insufficient. Please recharge your account to continue using the API.             | 用户余额不足，需要充值         |
| 403 | -                         | Account suspended.                                                                                        | 用户状态被禁用或在黑名单        |
| 403 | -                         | Forbidden – insufficient permissions.                                                                     | 用户角色权限不够            |
| 403 | -                         | Forbidden – key(请求的 key 后六位) allowed only from approved IP ranges.                                        | IP 不在令牌允许的网段内       |
| 403 | -                         | Forbidden – key(请求的 key 后六位) not authorized to access the requested model.                                | 令牌不支持请求的模型          |
| 403 | -                         | Key error；(请求的 key 后六位)                                                                                   | 非管理员用户尝试指定渠道        |
| 403 | -                         | Forbidden – channel has been disabled.                                                                    | 渠道状态为禁用             |
| 401 | -                         | Unauthorized – no access token supplied; please log in and include a valid token.                         | 未提供 Authorization 头 |
| 401 | -                         | Unauthorized – access token is invalid or expired.                                                        | access token 验证失败   |
| 400 | -                         | Bad Request – invalid channel ID.                                                                         | 渠道 ID 格式错误或不存在      |
| 400 | prompt\_missing           | prompt is required                                                                                        | 图片生成缺少提示词           |
| 400 | prompt\_too\_long         | prompt is too long                                                                                        | 提示词超过长度限制           |
| 400 | text\_too\_long           | input is too long                                                                                         | 音频输入文本过长            |
| 400 | size\_not\_supported      | size not supported                                                                                        | 图片尺寸不被模型支持          |
| 400 | n\_not\_within\_range     | invalid value of n                                                                                        | n 参数值不在有效范围         |

***

更新时间：2026-06-01
