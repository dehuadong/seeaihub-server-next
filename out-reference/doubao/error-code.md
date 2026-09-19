本文列举您调用火山方舟 API 可能会涉及的错误码信息，包含方舟错误码和公共错误码。

<span id="common-error-codes-agent"></span>
## 推理错误码


<span aceTableMode="list" aceTableWidth="2,2,3,4,4"></span>
|HTTP<br><br>状态码 |错误类型<br><br>Type |错误码<br><br>Code |错误信息<br><br>Message |含义 |
|---|---|---|---|---|
|400 |BadRequest |MissingParameter |The request failed because it is missing one or multiple required parameters. Request ID: {{id}} |请求缺少必要参数，请查阅 API 文档。 |
|400 |BadRequest |InvalidParameter |One or more parameters specified in the request are not valid. Request ID: {{id}} |请求包含非法参数，请查阅 API 文档。 |
|400 |BadRequest |InvalidParameter |The parameter `instructions` specified in the request are not valid: caching is not supported for instructions. Request id: {{id}} |Responses API 中，当配置过 **instructions** 字段信息，后续轮次无法配置 **Caching** 字段。 |
|400 |BadRequest |InvalidEndpoint.ClosedEndpoint |The request targeted an endpoint that is currently closed or temporarily unavailable. Request ID: {{id}} |推理接入点处于已被关闭或暂时不可用， 请稍后重试，或联系推理接入点管理员。 |
|400 |BadRequest |SensitiveContentDetected |The request failed because the input text may contain sensitive information. |输入文本可能包含敏感信息，请您使用其他 prompt。 |
|400 |BadRequest |SensitiveContentDetected.SevereViolation |The request failed because the input text may contain severe violation information. |输入文本可能包含严重违规相关信息，请您使用其他 prompt |
|400 |BadRequest |SensitiveContentDetected.Violence |The request failed because the input text may contain violence information. |输入文本可能包含激进行为相关信息，请您使用其他 prompt |
|400 |BadRequest |InputTextSensitiveContentDetected |The request failed because the input text may contain sensitive information.Request ID: {{id}} |输入文本可能包含敏感信息，请您更换后重试。 |
|400 |BadRequest |InputImageSensitiveContentDetected |The request failed because the input image may contain sensitive information.Request ID: {{id}} |输入图像可能包含敏感信息，请您更换后重试。 |
|400 |BadRequest |InputVideoSensitiveContentDetected |The request failed because the input video may contain sensitive information. |输入视频可能包含敏感信息，请您更换后重试。 |
|400 |BadRequest |InputAudioSensitiveContentDetected |The request failed because the input audio may contain sensitive information.Request ID: {{id}} |输入音频可能包含敏感信息，请您更换后重试 |
|400 |BadRequest |OutputTextSensitiveContentDetected |The request failed because the output may contain sensitive information. |生成的文字可能包含敏感信息，请您更换输入内容后重试 |
|400 |BadRequest |OutputImageSensitiveContentDetected |The request failed because the output image may contain sensitive information. |生成的图像可能包含敏感信息，请您更换输入内容后重试。 |
|400 |BadRequest |OutputVideoSensitiveContentDetected |The request failed because the output video may contain sensitive information.Request ID: {{id}} |生成的视频可能包含敏感信息，请您更换输入内容后重试。 |
|400 |BadRequest |OutputAudioSensitiveContentDetected |The request failed because the output audio may contain sensitive information.Request ID: {{id}} |生成的音频可能包含敏感信息，请您更换输入内容后重试。 |
|400 |BadRequest |InputTextSensitiveContentDetected.PolicyViolation |The request failed because the input text may be related to copyright restrictions. Request ID: {{id}} |输入文本可能涉及版权限制，请您更换后重试。 |
|400 |BadRequest |InputImageSensitiveContentDetected.PolicyViolation |The request failed because the input image may be related to copyright restrictions. Request ID: {{id}} |输入图片可能涉及版权限制，请您更换后重试。 |
|400 |BadRequest |InputVideoSensitiveContentDetected.PolicyViolation |The request failed because the input video may be related to copyright restrictions. Request ID: {{id}} |输入视频可能涉及版权限制，请您更换后重试。 |
|400 |BadRequest |InputAudioSensitiveContentDetected.PolicyViolation |The request failed because the input audio may be related to copyright restrictions. Request ID: {{id}} |输入音频可能涉及版权限制，请您更换后重试。 |
|400 |BadRequest |OutputVideoSensitiveContentDetected.PolicyViolation |The request failed because the output video may be related to copyright restrictions. Request ID: {{id}} |生成的视频可能涉及版权限制，请您更换输入内容后重试。 |
|400 |BadRequest |OutputAudioSensitiveContentDetected.PolicyViolation |The request failed because the output audio may be related to copyright restrictions. Request ID: {{id}} |生成的音频可能涉及版权限制，请您更换输入内容后重试。 |
|400 |BadRequest |InputImageSensitiveContentDetected.PrivacyInformation |The request failed because the input image may contain real person.Request ID: {{id}} |输入图片可能包含真人，请您更换后重试。 |
|400 |BadRequest |InputVideoSensitiveContentDetected.PrivacyInformation |The request failed because the input video may contain real person.Request ID: {{id}} |输入视频可能包含真人，请您更换后重试。 |
|400 |BadRequest |OutputImageSensitiveContentDetected.DeepFake |The request failed because the output image may contain counterfeit documents or credentials.Request ID: {{id}} |输出图片可能涉及伪造内容风险，请您更换后重试。 |
|400 |BadRequest |InputTextRiskDetection |The request could not be processed because the input text includes sensitive content that violates ContentSecurityDetection.ARKRequest ID:{{id}};CSDRequestId:{{RequestId}};Label:{{Label}};SubLabel:{{SubLabel}} |火山引擎风险识别产品检测到输入文本可能包含敏感信息，请您更换后重试。 |
|400 |BadRequest |InputImageRiskDetection |The request could not be processed because the input image includes sensitive content that violates ContentSecurityDetection.ARKRequest ID:{{id}};CSDRequestId:{{RequestId}};Label:{{Label}};SubLabel:{{SubLabel}} |火山引擎风险识别产品检测到输入图片可能包含敏感信息，请您更换后重试。 |
|400 |BadRequest |OutputTextRiskDetection |The request could not be processed because the output text includes sensitive content that violates ContentSecurityDetection.ARKRequest ID:{{id}};CSDRequestId:{{RequestId}};Label:{{Label}};SubLabel:{{SubLabel}} |火山引擎风险识别产品检测到输出文本可能包含敏感信息，请您更换后重试。 |
|400 |BadRequest |OutputImageRiskDetection |The request could not be processed because the output image includes sensitive content that violates ContentSecurityDetection.ARKRequest ID:{{id}};CSDRequestId:{{RequestId}};Label:{{Label}};SubLabel:{{SubLabel}} |火山引擎风险识别产品检测到输出图片可能包含敏感信息，请您更换后重试。 |
|400 |BadRequest |ContentSecurityDetectionError |Internal error.ARKRequest ID:{{id}};CSDRequestId:{{RequestId}};CSDcode:{};CSDmessage:{} |火山引擎风险识别产品请求失败。 |
|400 |BadRequest |InvalidParameter.{{Parameter}} |The specified parameter {{Parameter}} is invalid. |请求参数值不合法。请检查参数值的正确性后重试。 |
|400 |BadRequest |MissingParameter.{{Parameter}} |The required parameter {{Parameter}} is missing. |缺少必要的请求参数。请确认请求参数后重试。 |
|400 |BadRequest |Duplicate.Tags.Key |The specified object of tags contains duplicate keys. |对象的标签存在重复Key。 |
|400 |BadRequest |InvalidArgumentError |MissingRole：Invalid message: {{Message}} |请求中的 messages 列表里，有消息体缺少 role 字段 |
|400 |BadRequest |InvalidArgumentError.UnknownRole |Unknown the role of message: {{Role}} |消息体中的 role 值不被支持，如`user_`。 |
|400 |BadRequest |InvalidArgumentError.UnknownRole |The Inference role not found: {{Role}} |指定的 inference_role 未在配置中定义。 |
|400 |BadRequest |InvalidArgumentError.InvalidImageDetail |Invalid image detail: {{Parameter}} |image_url 中的 detail 参数值无效，只接受 "auto", "high", "low" |
|400 |BadRequest |InvalidArgumentError.InvalidPixelLimit |Customized min_pixels 100 is greater than max_pixels 50 |用户自定义的图片像素限制（min_pixels, max_pixels）无效（例如 min_pixels \> max_pixels，或超出了服务配置的范围） |
|400 |BadRequest |InvalidImageURL.EmptyURL |Empty base64 image url |传入的图片 URL 为空 |
|400 |BadRequest |InvalidImageURL.InvalidFormat |Invalid base64 image url |无法解析或处理图片，可能是 Base64 格式不正确、图片数据损坏或格式不支持 |
|400 |BadRequest |OutofContextError |Total tokens of image and text exceed max message tokens. |当请求中包含图片时，文本和图片编码后的总 token 数超过了模型上下文长度限制 |
|400 |BadRequest |InvalidParameter.UnsupportedParameter |The parameters {{Parameter}} specified in the request are not supported by this endpoint. |传入的参数 {{Parameter}} 在此推理接入点不可用。 |
|400 |BadRequest |InvalidParameter.TosURLInvalid |TOS URI invalid. url:%s. |TOS URI不合法。 |
|400 |BadRequest |InvalidParameter |The given Lean code is not compilable under Lean version %s |输入的不是一个合法的Lean Code。 |
|400 |BadRequest |InvalidParameter |The format of the given lean code is not supported so far. |输入的Lean code格式暂不支持。 |
|400 |BadRequest |InvalidParameter |/ |输入的Lean code必须包含theorem |
|400 |Forbidden |InvalidSubscription |Your account ({{account_identifier}}) does not have a valid coding plan subscription, or your subscription has expired. Please visit {{subscription_check_url}} to review your subscription status or complete the subscription or renewal process. |Coding Plan 套餐未订阅或已过期。 |
|401 |Unauthorized |AuthenticationError |The API key or AK/SK in the request is missing or invalid. Request ID: {{id}} |请求携带的 API Key 或 AK/SK 校验未通过，请您重新检查设置的 鉴权凭证，或者查看 API 调用文档来排查问题。 |
|401 |Forbidden |InvalidAccountStatus |There is an issue with your account status. If you need assistance, please contact the platform administrators. |当前使用的账号异常。 |
|403 |Forbidden |OperationDenied.InvalidState |The specified context is in invalid state: InProgress.Request ID: {{id}} |请求所关联的Context ID处于非空闲状态，不可调用。 |
|403 |Forbidden |OperationDenied.ConflictedValidationSet |Operation is denied because it is not supported to configure ValidationSet and ValidationPercentage at the same time. |无法同时上传验证集和设置训练集取样为验证集百分比，不支持该操作。 |
|403 |Forbidden |OperationDenied.PermissionDenied |Operation is denied because you are not permitted to access the specified configuration of the FoundationModel. |您没有权限访问基础模型的配置，不支持该操作。 |
|403 |Forbidden |OperationDenied.UnsupportedCustomizationType |Operation is denied because the specified CustomizationType is not supported by the CustomModel. |模型不支持该训练方法，不支持该操作。 |
|403 |Forbidden |OperationDenied.CustomizationNotSupported |Operation is denied because the specified version of the FoundationModel is not configured for the specified type of customization. |基础模型的版本不支持该训练方法，不支持该操作。 |
|403 |Forbidden |OperationDenied.ServiceNotOpen |Operation is denied because the model service is unavailable, please go to the Volcano Ark console activation management page to activate the corresponding model service, or submit a work order to contact us. |模型服务不可用，不支持该操作。请前往火山方舟控制台激活模型服务，或提交工单联系我们。 |
|403 |Forbidden |OperationDenied.ServiceOverdue |Operation is denied because your account balance is overdue, please go to the Volc Trading Center to recharge in order to continue using the service. |您的账单已逾期，不支持该操作。请前往火山费用中心充值。 |
|403 |Forbidden |AccountOverdueError |The request failed because your account has an overdue balance. Request ID: {{id}} |当前账号欠费（余额<0），如需继续调用，请前往 [火山引擎费用中心](https://console.volcengine.com/finance/fund/recharge) 进行充值，详细操作参见 [充值操作指引](https://www.volcengine.com/docs/6269/100434)。 |
|403 |Forbidden |AccessDenied |The request failed because you do not have access to the requested resource. Request ID: {{id}} |没有访问该资源的权限，请检查权限设置，或联系管理员添加白名单。 |
|403 |Forbidden |OperationDenied.InvalidState |Operation is denied because the specified context is in invalid state: InProgress. Request id: {{id}} |请求的缓存信息状态是不可用状态。请查看缓存信息是否正在被更新中。 |
|403 |Forbidden |OperationDenied.UnsupportedPhase |Operation is denied because operation is not supported while the target is in its current phase. |操作失败，操作目标在特殊状态，请检查目标是否存在或者被锁定等特殊状态中。 |
|403 |Forbidden |OperationDenied.FileQuotaExceeded |Your account %s has exhausted its file storage quota. To continue using the service, please delete historical files. |当前账号 %s 已耗尽文件存储额度，如需继续使用，请删除历史文件。 |
|403 |Forbidden |OperationDenied.ArkAccessRoleNotFound |Please go to Ark Project Settings → Project Authorization, and grant permission to the TOS resource before trying again. |请到方舟项目配置\-项目授权，对tos资源授权后再进行操作。 |
|403 |Forbidden |OperationDenied.TosAccessDenied |Access denied for TOS resource %s. TOS responded: %s. Please verify your account / project has permission to access the specified bucket. |无权访问 TOS 资源，请确认您的账户 / 项目对该 bucket 拥有访问权限。 |
|403 |Forbidden |OperationDenied.InvalidState |The specified file is in invalid state: InProgress.Request ID: {{id}} |请求所关联的File ID处于非可用状态，不可调用。 |
|404 |NotFound |InvalidEndpointOrModel.NotFound |The model or endpoint %s does not exist or you do not have access to it. |模型或者推理接入点 %s 不存在或者您无权访问它。 |
|404 |NotFound |ModelNotOpen |Your account %s has not activated the model %s. Please activate the model service in the Ark Console. |当前账号 %s 暂未开通 %s 模型服务，请前往火山方舟控制台开通管理页开通对应模型服务。 |
|404 |NotFound |NotFound.{{Parameter}} |The specified {{ResourceType}} {{ResourceContent}} is not found. |指定资源找不到。请确认参数后重试。 |
|404 |NotFound |InvalidEndpointOrModel.ModelIDAccessDisabled |Accessing the model via Model ID is not allowed for your account. Please use a custom endpoint ID instead. Request id:{{id}} |未能找到指定的模型ID。你的账号不允许使用模型ID来调用模型，请确认你账号权限或者使用有权限的推理接入点 ID 来调用模型服务。 |
|404 |NotFound |UnsupportedModel |The {{model_name}} model does not support the coding plan feature. Please refer to the documentation at {{doc_url}} to select a compatible model. |当前模型不支持 Coding Plan。 |
|429 |TooManyRequests |RateLimitExceeded.EndpointRPMExceeded |The Requests Per Minute (RPM) limit of the associated endpoint for your account has been exceeded. Request ID: {{id}} |请求所关联的推理接入点已超过 RPM (Requests Per Minute) 限制, 请稍后重试。 |
|429 |TooManyRequests |RateLimitExceeded.EndpointTPMExceeded |The Tokens Per Minute (TPM) limit of the associated endpoint for your account has been exceeded. Request ID: {{id}} |请求所关联的推理接入点已超过 TPM (Tokens Per Minute) 限制, 请稍后重试。 |
|429 |TooManyRequests |ModelAccountRpmRateLimitExceeded |RPM (Requests Per Minute) limit of the model is exceeded. Request ID: {{id}} |请求已超过帐户模型 RPM (Requests Per Minute) 限制: 请您稍后重试, 或者联系平台技术同学进行解决 |
|429 |TooManyRequests |ModelAccountTpmRateLimitExceeded |TPM (Tokens Per Minute) limit of the model is exceeded. Request ID: {{id}} |请求已超过帐户模型 TPM (Tokens Per Minute) 限制: 请您稍后重试, 或者联系平台技术同学进行解决 |
|429 |TooManyRequests |APIAccountRpmRateLimitExceeded |The RPM (Requests Per Minute) limit for the API on your account has been exceeded. Request ID: {{id}} |当前账号该接口的RPM (Requests Per Minute)限制已超出，请稍后重试。 |
|429 |TooManyRequests |ModelAccountIpmRateLimitExceeded |IPM (Images Per Minute) limit of the model is exceeded. |请求已超过账户模型 IPM (Images Per Minute) 限制: 请您稍后重试, 或者联系平台技术同学进行解决 |
|429 |TooManyRequests |QuotaExceeded |Your account [%s] has exhausted its free trial quota for the [%s] model. Request ID: {{id}} |当前账号 %s 对 %s 模型的免费试用额度已消耗完毕，如需继续调用，请前往火山方舟控制台开通管理页开通对应模型服务。 |
|429 |TooManyRequests |QuotaExceeded |The request has exceeded the quota. Request ID: {{id}} |当前账号处于排队中状态的任务数已超过限制，请稍后重试。 |
|429 |TooManyRequests |ServerOverloaded |The service is currently unable to handle additional requests due to server overload. Please retry later. Request ID: {{id}} |服务资源紧张，请您稍后重试。常出现在调用流量突增或刚开始调用长时间未使用的推理接入点。<br><br><div data-tips="true" data-tips-type="tip" data-tips-is-title="true">说明</div><br><br><br><div data-tips="true" data-tips-type="tip">调用<code>doubao-seed-1.8</code>及之前版本模型触发突增流量限制时，返回此错误码。可参考<a href="https://docs.volcengine.com/docs/82379/1848593">突发流量处理最佳实践</a>处理。</div><br> |
|429 |TooManyRequests |RequestBurstTooFast |System protection triggered by request burst. Please slow down traffic growth and increase requests gradually before retrying. |请求量激增触发系统保护，请放缓流量提升速度，逐步增加请求量后再尝试<br><br><div data-tips="true" data-tips-type="tip" data-tips-is-title="true">说明</div><br><br><br><div data-tips="true" data-tips-type="tip">调用 <code>doubao-seed-2.0</code>及之后版本模型触发突增流量限制时，返回此错误码，可参考<a href="https://docs.volcengine.com/docs/82379/1848593">突发流量处理最佳实践</a>处理。</div><br> |
|429 |TooManyRequests |SetLimitExceeded |Your account [%s] has reached the set inference limit for the [%s] model, and the model service has been paused. To continue using this model, please visit the Model Activation page to adjust or close the "Safe Experience Mode".<br><br>Request ID: {{id}} |当前账号 %s 对 %s 模型已达到设置的推理限额值，如需继续调用，请前往火山方舟控制台开通管理页修改限额值或关闭安心体验模式。 |
|429 |TooManyRequests |InflightBatchsizeExceeded |The Inflight Batchsize limit has been exceeded.Request ID: {{id}} |您已经达到当前充值金额下的最大并发数限制，您可以充值解锁更大并发额度或降低并发数。 |
|429 |TooManyRequests |AccountRateLimitExceeded |Requests are too frequent. Please reduce your request frequency, wait a short moment, and retry your request. |请求超出RPM / TPM限制。 |
|429 |TooManyRequests |QuotaExceeded |You have exceeded the 5\-hour/weekly/monthly usage quota. It will reset at {{reset_time}}. |使用的额度超出5小时/周/月限额。 |
|500 |InternalServerError |InternalServiceError |The service encountered an unexpected internal error. Please retry later. Request ID: {{id}} |内部系统异常，请您稍后重试。 |


<span id="9b1548df"></span>
## 精调错误码

本部分将介绍模型精调相关的错误码信息及其建议的解决方案。


<span aceTableMode="list" aceTableWidth="2,2,3"></span>
|错误码 |示例错误信息 |说明与建议解决方案 |
|---|---|---|
|InvalidData.MissingKey |Data format is not expected:column not found |数据格式不符合预期, 未找到名为的列, 建议检查并补全数据集中相应键值. |
|InvalidData.UnknownKey |Wrong Key, parsing sample failed |数据中有错误的Key, 解析样本失败, 建议检查错误信息中对应样本的键值 |
|InvalidData.InvalidValue |Unsupported data type:, only pretrain, dialog, dialog\-dpo and multimodal supported |不支持的数据集类型, 仅支持"pretrain", "dialog", "dialog\-dpo", 和"multimodal"类型, 建议检查填入的数据集类型 |
||Content is empty, original text content is: |content字段内容为空, 原文本内容为, 建议检查原文本的数据完整性. |
|InvalidData.InvalidJsonl |not supported |数据集文件格式不支持, 建议调整对应文件为`.jsonl`格式. |
||No jsonl file available |无可用的jsonl文件, 建议检查数据集包含的文件列表. |
|InvalidData.InvalidJson |Expecting value: line 1 column 1 (char 0) in fileatrow |在<文件名\>中第行, JSON解析失败, 建议检查数据集文件内容是否符合JSON规范. |
|InvalidData |failed to init data preprocess builder: training tos bucket: maas\-data\-test, tos path:<br><br>: tos objects do not exist |提供的数据集地址在TOS中不存在, 建议检查TOS中数据集文件是否出现缺失或地址错误. |
|UnknownError |service error occur, please contact customer service for help |平台服务错误, 不可重试, 建议发起工单协助排查 |
|InternalError |task failed, please check the logs |训练失败, 建议检查日志后发起重试, 如仍无法解决问题, 建议发起工单协助排查. |


<span id="managed-agent-error-codes"></span>
## Managed Agent 错误码

本部分列举 Managed Agent 相关接口的错误码信息。Managed Agent 的错误按下发通道分为两类：


* **HTTP 阶段错误码** ：由 HTTP 请求同步返回，携带 HTTP 状态码与结构化错误体，适用于所有同步接口（如 POST create、POST events）。

* **SSE 阶段错误码** ：请求已进入事件流后由服务端通过 `data.type = "session.error"` 事件下发， **无 HTTP 状态码** ，仅在 `RunFailed` / `SessionError` 与 SSE resume/lag 场景出现。


<span id="managed-agent-http-error-codes"></span>
### HTTP 阶段错误码


<span aceTableMode="list" aceTableWidth="2,2,3,4,4"></span>
|HTTP<br><br>状态码 |错误类型<br><br>Type |错误码<br><br>Code |错误信息<br><br>Message |含义 |
|---|---|---|---|---|
|400 |BadRequest |InvalidParameter |One or more parameters specified in the request are not valid. |请求中存在一个或多个非法参数。请对照 API 文档核对参数名、类型、取值范围及参数间依赖关系后重试。 |
|400 |BadRequest |MissingParameter |The request failed because it is missing `%s` parameter. |请求缺少必填参数 `%s`。请补齐后重试。 |
|400 |BadRequest |InvalidAction |The requested action is not valid for the current resource state. |目标资源当前状态不允许该操作。请先将资源迁移到允许该操作的状态（例如结束正在进行的回合、恢复被暂停的资源）后再重试。 |
|400 |BadRequest |InvalidPayload |Request payload is invalid. |POST create / POST events 阶段的同步协议错误的最宽类别，覆盖：请求体 JSON 解析失败、event batch 结构非法、caller event id 非法、goal / outcome 参数非法等。请对照接口 schema 校验请求体。 |
|400 |BadRequest |EmptyEvents |Request must include at least one event. |POST events 请求体中的 `events` 数组为空。本码用于与 `InvalidPayload` 区分：`InvalidPayload` 表示 batch 结构非法，`EmptyEvents` 特指结构合法但长度为 0。 |
|400 |BadRequest |RuntimeRejected |Runtime rejected the request. |POST create 阶段同步 materialize 被 runtime 拒绝。Tool Service 启动期致命错误也会归到此码；create\-time 使用 public override 时 `message` 内容可能不同。请检查 agent snapshot 与 runtime 配置是否兼容。 |
|400 |BadRequest |MissingSessionModel |Request provider mode requires agent.model.name. |POST create 在 request provider 模式下，managed\-agent snapshot 中未包含可用的 `agent.model.name`。请在 agent 定义中补齐模型信息。 |
|400 |BadRequest |MissingSessionId |session_id is required. |POST create 请求体缺失 `session_id` 或该字段为空白字符串。请携带非空 `session_id` 后重试。 |
|400 |BadRequest |ManagedAgentsRequired |Managed agents integration is required for request provider mode. |POST create 使用 request provider 模式，但服务端未配置 managed\-agents loader。请在服务端完成 managed agents 集成后再调用。 |
|400 |BadRequest |ManagedAgentsRejected |Managed agent definition was rejected. |POST create 阶段 managed\-agents 业务层拒绝请求。常见原因：目标 session 不存在、session 已被删除、租户与 session 归属不一致等。 |
|400 |BadRequest |ManagedAgentsInvalidAgent |Managed agent definition is invalid. |POST create 阶段 agent payload 无法翻译成合法的 AgentSpec。常见原因：`agent.name` 为空、权限策略或工具名不被识别、`skill_id` 缺失等。 |
|401 |Unauthorized |AuthenticationError |The API key or AK/SK in the request is missing or invalid. |请求携带的 API Key 或 AK/SK 缺失、非法或已过期。请检查鉴权凭证的设置，或参考 API 调用文档排查。 |
|401 |Unauthorized |MissingHeader |Authorization header is required for this request. |POST events 在 request provider 模式下要求 `Authorization` 请求头存在、非空且为 `Bearer <token>` 形态。请补齐该请求头。 |
|401 |Unauthorized |MCPInvalidCredential |The MCP server rejected the credential; verify or update the API key or token. |出向调用的 MCP server 判定当前凭证无效并拒绝请求。请更新对应的 API key 或 token。 |
|401 |Unauthorized |MCPNeedsReauth |The MCP credential is unauthorized after refresh; re\-authorization is required. |出向 MCP 凭证在自动刷新后仍返回 401，说明 refresh token 已失效。请重新走 OAuth 授权流程获取新凭证。 |
|403 |Forbidden |AccessDenied |The request failed because you do not have access to the requested resource. |调用方对目标资源无访问权限。请检查资源所属账号、项目授权及白名单配置。 |
|403 |Forbidden |OperationDenied(.<cause\>) |Operation is denied [because <cause\>]. |操作被拒绝，携带子原因码 `<cause>` 指明具体原因，例如：商品未开通、子账号缺少所需权限、账号实名认证未通过、账号欠费导致服务暂停等。请根据 `<cause>` 与 `message` 定位并解决。 |
|403 |Forbidden |AccountOverdueError |The request failed because your account has an overdue balance. |当前账号存在欠费或计费项未开通，服务被禁止调用。请前往火山引擎费用中心完成充值或开通对应计费项后重试。 |
|403 |Forbidden |MCPNetworkDenied |Network access to the requested MCP server is denied. |目标 MCP server 未通过接入控制（connector 未启用，或 host 不在出向白名单）。请在方舟控制台开通对应的 MCP 出向策略。 |
|404 |NotFound |PathNotFound |The path of api not found. |请求所指的 API 路径不存在。请核对 endpoint 路径与 API 版本。 |
|404 |NotFound |InvalidSession.NotFound |The session %s does not exist or you do not have access to it. |指定的 `session_id` 不存在，或调用者对该 session 无访问权限。 |
|404 |NotFound |InvalidAgent.NotFound |The agent %s does not exist or you do not have access to it. |指定的 `agent_id` 不存在，或调用者对该 agent 无访问权限。 |
|404 |NotFound |InvalidEnvironment.NotFound |The environment %s does not exist or you do not have access to it. |指定的 `environment_id` 不存在，或调用者对该 environment 无访问权限。 |
|404 |NotFound |InvalidVault.NotFound |The vault %s does not exist or you do not have access to it. |指定的 `vault_id` 不存在，或调用者对该 vault 无访问权限。 |
|404 |NotFound |InvalidCredential.NotFound |The credential %s does not exist or you do not have access to it. |指定的 `credential_id` 不存在，或调用者对该 credential 无访问权限。 |
|404 |NotFound |InvalidMemoryStore.NotFound |The memory_store %s does not exist or you do not have access to it. |指定的 `memory_store_id` 不存在，或调用者对该 memory store 无访问权限。 |
|404 |NotFound |InvalidFile.NotFound |The file %s does not exist or you do not have access to it. |指定的 `file_id` 不存在，或调用者对该文件无访问权限。 |
|404 |NotFound |ResourceNotFound |The requested resource does not exist or you do not have access to it. |请求的资源不存在或调用者无访问权限。当资源类型未落到上述具体 `.NotFound` 分支时使用此通用码。 |
|404 |NotFound |ManagedAgentNotOpen |Your account %s has not activated the managed agent. Please activate the service in the Ark Console. |当前账号 %s 未开通 Managed Agent 服务。请前往火山方舟控制台开通对应服务后再调用。 |
|404 |NotFound |NotSupportedMCPServer.NotFound |The MCP server %q is not supported by the OAuth flow yet; ask operator to register it. |OAuth flow 中传入的 `mcp_server_url` 未在服务端预注册。请联系运营方将该 MCP server 完成注册后再走 OAuth 流程。 |
|409 |Conflict |Conflict |The request conflicts with the current state of the resource. |请求与目标资源的当前状态存在冲突。请刷新资源状态后重试。 |
|409 |Conflict |ResourceConflict |The request conflicts with the current state of the resource. |资源发生冲突（如同名或同路径的资源已存在）。请更换资源标识后重试。 |
|409 |Conflict |SessionBusy |The session is currently busy with another in\-flight request. Please retry after the current request finishes or send a user.interrupt first. |同一 session 上已有 in\-flight 回合正在执行。请等待其结束，或先向该 session 发送 `user.interrupt` 中断当前回合后再重试。 |
|413 |PayloadTooLarge |RequestTooLarge |The request entity is larger than the limit allowed. |业务层单个 content / payload 超过上限（含单文件、zip 包、单条消息等业务粒度）。请拆分或压缩后重试。 |
|413 |PayloadTooLarge |RequestBodyTooLarge |The request body is larger than the limit allowed. |HTTP 请求体的字节数超过了传输层上限。请压缩或分批传输。此码与 `RequestTooLarge` 的区别是：前者是业务粒度限制，本码是传输粒度限制。 |
|424 |FailedDependency |UpstreamUnavailable |An upstream dependency is temporarily unavailable. Please retry later or check that the upstream is healthy. |客户侧上游依赖（如 MCP server、IdP）临时不可达，表现为超时、抖动或返回 5xx。请稍后重试，并排查上游服务健康度。 |
|424 |FailedDependency |MCPInvalidResponse |The MCP server returned an invalid response; verify the server URL and credential. |MCP server 对 `initialize` 请求返回不合法响应。常见原因是 `mcp_server_url` 指向了非 MCP 服务。请核对 server URL 及凭证。 |
|429 |TooManyRequests |APIAccountRpmRateLimitExceeded |The RPM (Requests Per Minute) limit for the API on your account has been exceeded. |当前账号在该 API 上的 RPM (Requests Per Minute) 限额已超出。请降低调用频率或稍后重试。 |
|429 |TooManyRequests |SessionQuotaExceeded |The number of active sessions has reached the quota. Finish or delete existing sessions before creating a new one. |单个 agent 下的活跃 session 数量已达配额上限。请先对已完成的 session 调用 finish 或 delete 释放配额后再创建新的 session。 |
|499 |ClientClosedRequest |RequestCanceled |The request is canceled before response. |服务端在返回响应前请求已被客户端取消，通常由客户端主动断开或超时导致。 |
|500 |InternalServerError |InternalServiceError |The service encountered an unexpected internal error. Please retry later. |服务内部出现未预期错误。可重试；持续复现时请携带 Request ID 提工单排查。 |
|500 |InternalServerError |SideEffectFailed |Request side effect failed. |POST events 在把 command 分派到 runtime **之前** ，服务端 side effect 执行失败（如 goal / outcome journal 的读写、replay、append 等）。可重试。 |
|502 |BadGateway |ManagedAgentsUnavailable |Managed agent definition is temporarily unavailable. |POST create 反查 managed\-agents 的 `InnerGetSession` 出现网络错误、非 2xx 响应或响应解码失败，上游 managed\-agents 服务暂时不可用。请稍后重试。 |


<span id="managed-agent-sse-error-codes"></span>
### SSE 阶段错误码

请求进入事件流后发生的错误，以 `data.type = "session.error"` 事件通过已建立的 SSE 通道下发， **不携带 HTTP 状态码** 。仅在以下场景出现：runtime 判定 `RunFailed` / `SessionError`，或 SSE resume / lag 通道自身错误。事件示例（SSE wire format，片段）：

```text
...

id: sevt-...
data: {"type": "session.error", "id": "sevt-...", "processed_at": "2026-07-17T15:02:00+08:00", "error": {"type": "unknown_error", "message": "Tool execution failed due to a runtime issue."}}

...
```


其中 `data:` 后的 JSON 负载展开如下：

```json
{
  "type": "session.error",
  "id": "sevt-...",
  "processed_at": "2026-07-17T15:02:00+08:00",
  "error": {
    "type": "unknown_error",
    "message": "Tool execution failed due to a runtime issue."
  }
}
```


**客户端分类规则** ：以 `error.type` 作为程序判别的唯一字段——所有分支都会携带该字段。`error.message` 仅供展示，不作为程序分类依据。


<span aceTableMode="list" aceTableWidth="3,7"></span>
|error.type |含义 |
|---|---|
|model_overloaded_error |Typed 分支，不携带 `error.code`。模型上游过载。 |
|model_rate_limited_error |Typed 分支，不携带 `error.code`。模型触发限流。 |
|model_request_failed_error |Typed 分支，不携带 `error.code`。模型请求失败（如上游返回 5xx、连接建立失败等）。 |
|billing_error |`SessionError` 专属 variant，不携带 `error.code`。计费相关错误。 |
|unknown_error |尚未收敛到 typed 分支的多种致命场景当前统一落此 type，需结合上下文与日志进一步定位。可能触发的场景包括：<br><br>\- Provider snapshot 或 provider HTTP 调用失败，且 runtime 重试用尽后进入 `RunFailed`；<br><br>\- Tool Service 执行期出现回合级致命错误（如 exec 无响应、2xx 响应解码失败、前台 bash 流异常终止等）；exec 期 HTTP 非 2xx 通常作为模型可见的 tool error 由模型处理， **不进入 ** **`session.error`** ** 通道** ；<br><br>\- MCP 工具执行期发生致命错误；<br><br>\- 命令进入 runtime 后请求非法（POST 阶段的校验规则不适用于此阶段，不要复用同名码的判定规则）；<br><br>\- Runtime 协议不匹配；<br><br>\- 内核回合占用；<br><br>\- Runtime 内部错误；<br><br>\- Credential host 不可达（typed schema 已声明但 `vault_id` / `credential_id` 字段尚未补齐，当前仍落 `unknown_error`）；<br><br>\- SSE resume 位点超出保留窗口——属于 stream 自身错误，与 runtime / Tool / provider 均无关；<br><br>\- 订阅端消费滞后被服务端主动断开——属于 stream 自身错误。 |


<span id="d674f4be"></span>
## 公共错误码

查询火山引擎的 [公共错误码](https://www.volcengine.com/docs/6369/68677)。



