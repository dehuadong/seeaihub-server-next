# APIMart 计费口径的两条事实（不含价格页内容）

- **来源**：`https://apimart.ai/zh/pricing` 与 `https://docs.apimart.ai/cn/api-reference/images/gpt-image-2.5/generation.md`
- **抓取时间**：2026-09-19
- **性质**：外部参考资源，不是平台接口合同；运行中服务不读取本目录。
- **本文只记录「结算以什么为准」，不摘录价格页的任何数字、档位名或站点展示用的模型别名**——那些是营销口径，不是结算依据，抄进仓库只会误导判断。

## 1. 结算口径以响应为准，不以页面为准

`gpt-image-2.5` 文档原文：「最终费用请以价格页面或 `/api/pricing` 返回的实时值为准。」

而 `/api/pricing` 在本机只读探测的 **6 个候选路径上全部 404**：

```text
apimart.ai/api/pricing
api.apimart.ai/api/pricing
api.apimart.ai/v1/pricing
apimart.ai/v1/pricing
api.apimart.ai/pricing
www.apimart.ai/api/pricing
```

⇒ **不存在可用的机器可读价格端点**。价格只能从真实调用的响应取得。

## 2. 差额是折扣，不是计量误差

文档原文：「实际扣费还会受到**账号分组倍率和折扣**影响。」

⇒ 即便平台按已发布单价算出的金额与上游声明的金额**存在系统性差额，那也是账号折扣**。对账逻辑必须容纳该差额，**不得把它当异常反复追查**；同时意味着**仅凭公开单价无法核对实际扣费**——可信度来自金额能否逐笔关联到某个 Attempt（任务查询返回金额，但不含 token 用量，见 `tasks-status.cn.md`）。

## 结论

APIMart 侧的权威计费口径**只能由真实调用结清**，属实施期的显式受控验证项。**在获批准前不发起任何计费调用。**
