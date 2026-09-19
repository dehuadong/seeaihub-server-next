> ## Documentation Index
> Fetch the complete documentation index at: https://docs.apimart.ai/llms.txt
> Use this file to discover all available pages before exploring further.

# 获取任务状态

> **已退场（#223 / ADR-0008，2026-07-26）**：Apimart 渠道已从 SeeAI Hub 完整移除，不再是受支持渠道。本文档仅作历史参考保留；运行时不再包含 apimart 的分发、回调与计费路径。

>  - 查询异步任务的执行状态和结果
- 实时状态更新和进度跟踪
- 任务完成时获取生成结果
- 支持多语言返回（zh/en/ko/ja） 

<RequestExample>
  ```bash cURL theme={null}
  curl --request GET \
    --url 'https://api.apimart.ai/v1/tasks/task-unified-1757156493-imcg5zqt?language=zh' \
    --header 'Authorization: Bearer <token>'
  ```

  ```python Python theme={null}
  import requests

  url = "https://api.apimart.ai/v1/tasks/task-unified-1757156493-imcg5zqt"

  headers = {
      "Authorization": "Bearer <token>"
  }

  params = {
      "language": "zh"
  }

  response = requests.get(url, headers=headers, params=params)

  print(response.json())
  ```

  ```javascript JavaScript theme={null}
  const url = "https://api.apimart.ai/v1/tasks/task-unified-1757156493-imcg5zqt?language=zh";

  const headers = {
    "Authorization": "Bearer <token>"
  };

  fetch(url, {
    method: "GET",
    headers: headers
  })
    .then(response => response.json())
    .then(data => console.log(data))
    .catch(error => console.error('Error:', error));
  ```
 
 
   
</RequestExample>

<ResponseExample>
  ```json 200 - 图像生成任务 theme={null}
  {
    "code": 200,
    "data": {
      "id": "task_01KA040M0HP1GJWBJYZMKX1XS1",
      "status": "completed",
      "cost": 0.15,
      "credits_cost": 1.5,
      "progress": 100,
      "result": {
        "images": [
          {
            "url": [
              "https://upload.apimart.ai/f/image/9998236911693428-e8d7441f-f7b4-4130-97ad-9ef8a0dde2ce-image_task_01KA0413RT2GGNZJ9GWQ4PXF2F_0.png"
            ],
            "expires_at": 1763174708
          }
        ]
      },
      "created": 1763088289,
      "completed": 1763088308,
      "estimated_time": 60,
      "actual_time": 19
    }
  }
  ```

  ```json 400 theme={null}
  {
    "error": {
      "code": 400,
      "message": "无效的任务ID",
      "type": "invalid_request_error"
    }
  }
  ```

  ```json 401 theme={null}
  {
    "error": {
      "code": 401,
      "message": "身份验证失败，请检查您的API密钥",
      "type": "authentication_error"
    }
  }
  ```

  ```json 402 theme={null}
  {
    "error": {
      "code": 402,
      "message": "账户余额不足，请充值后再试",
      "type": "payment_required"
    }
  }
  ```

  ```json 403 theme={null}
  {
    "error": {
      "code": 403,
      "message": "访问被禁止，您没有权限访问此资源",
      "type": "permission_error"
    }
  }
  ```

  ```json 429 theme={null}
  {
    "error": {
      "code": 429,
      "message": "请求过于频繁，请稍后再试",
      "type": "rate_limit_error"
    }
  }
  ```

  ```json 500 theme={null}
  {
    "error": {
      "code": 500,
      "message": "服务器内部错误，请稍后重试",
      "type": "server_error"
    }
  }
  ```

  ```json 502 theme={null}
  {
    "error": {
      "code": 502,
      "message": "网关错误，服务器暂时不可用",
      "type": "bad_gateway"
    }
  }
  ```
</ResponseExample>

## Authorizations

<ParamField header="Authorization" type="string" required>
  所有接口均需要使用Bearer Token进行认证

  获取 API Key：

  访问 [API Key 管理页面](https://apimart.ai/keys) 获取您的 API Key

  使用时在请求头中添加：

  ```
  Authorization: Bearer YOUR_API_KEY
  ```
</ParamField>

## Path parameters

<ParamField path="task_id" type="string" required>
  生成API返回的任务ID
</ParamField>

## Query parameters

<ParamField query="language" type="string">
  返回内容的语言，支持以下值：

  * `zh` - 中文
  * `en` - 英文
  * `ko` - 韩文
  * `ja` - 日文

  默认返回英文
</ParamField>

## Response

<ResponseField name="id" type="string">
  任务唯一标识符
</ResponseField>

<ResponseField name="status" type="string">
  任务状态值：

  * `pending` - 排队等待处理
  * `processing` - 处理中
  * `completed` - 成功完成
  * `failed` - 失败
  * `cancelled` - 用户取消
</ResponseField>

<ResponseField name="cost" type="number">
  本次任务扣费金额
</ResponseField>

<ResponseField name="credits_cost" type="number">
  本次任务扣费积分
</ResponseField>

<ResponseField name="progress" type="integer">
  任务进度百分比（0–100）
</ResponseField>

<ResponseField name="result" type="object">
  任务结果，仅在状态为 `completed` 时返回

  <Expandable title="属性">
    <ResponseField name="images" type="array">
      生成的图像对象数组（图像生成任务）
    </ResponseField>

    <ResponseField name="videos" type="array">
      生成的视频对象数组（视频生成任务）
    </ResponseField>
  </Expandable>
</ResponseField>

<ResponseField name="created" type="integer">
  任务创建时间戳
</ResponseField>

<ResponseField name="completed" type="integer">
  任务完成时间戳（仅在完成时存在）
</ResponseField>

<ResponseField name="estimated_time" type="integer">
  预计完成时间（秒）
</ResponseField>

<ResponseField name="actual_time" type="integer">
  实际完成时间（秒）（仅在完成时存在）
</ResponseField>

<ResponseField name="error" type="object">
  错误详情（仅在状态为 `failed` 时存在）

  <Expandable title="属性">
    <ResponseField name="code" type="integer">
      错误代码
    </ResponseField>

    <ResponseField name="message" type="string">
      错误消息
    </ResponseField>

    <ResponseField name="type" type="string">
      错误类型
    </ResponseField>
  </Expandable>
</ResponseField>