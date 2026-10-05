# 交接：模型目录关联独立使用文档

生成时间：2026-10-05。仓库 `/home/mypc/work/seeaihub-server-next`。本交接按用户指定放在 `.agents/handoff/`，覆盖 handoff 技能的临时目录默认位置。

## 当前状态

方案已获用户同意，规划审查已收敛；尚未实施 API。工作项为 [模型目录关联独立使用文档与公共接口说明](https://github.com/dehuadong/seeaihub-server-next/issues/73)，状态 OPEN、标签 `proposal:ready`，同时承接本次实施，没有子工单。

用户只说过“同意方案设计”，没有给出“执行实现”授权。本轮新授权是生成交接并提交、推送文档，不包含接口实现。下一会话先读取工作项及下列工件，再按工程流程检查实施门禁；不能直接开工。

## 工件与职责

| 内容 | 位置 |
| --- | --- |
| 行为合同与验收 | [模型使用文档 Spec](../../docs/specs/0008-model-usage-documentation.md)，当前与生效修订均为 v1，状态“已接受” |
| 文档素材、渲染、存储、发布、迁移与验证设计 | [模型使用文档 Agent Note](../notes/implemented/platform/2026-10-05-model-usage-documentation.md)，已交付 |
| 客户端使用说明入口 | [public-docs/README.md](../../public-docs/README.md)；公共鉴权、上传、错误说明及 OpenAI GPT-Image-2.5 模型族说明均在该目录 |
| 仓库文档入口 | [README.md](../../README.md)，增加 API 使用文档链接 |
| 规划审查记录 | [工作项审查评论](https://github.com/dehuadong/seeaihub-server-next/issues/73#issuecomment-5996899144)，包含 Standards、Spec、Architecture 三项自审 |
| 工作流与执行授权规则 | [根 AGENTS.md](../../AGENTS.md)、[工程流程](../../docs/agents/engineering.md) |
| 文档与提交规则 | [文档标准](../../docs/AGENTS.md)、[提交与推送](../../docs/agents/git.md) |

工作项拥有选定范围、实施步骤 S1–S5、排除项和进度；合同与技术正文由上述仓库工件拥有，不要在交接中重写。

## 用户纠正，必须保留

- `/v1/models` 面向不同厂商和 `image`、`video`、`chat`。每个模型独立提供说明，公共事项独立成页，不能用一篇 GPT 图片说明代替整个模型目录。
- 对客文档只服务 API 调用者：直接说明参数含义、请求、响应、素材上传和错误处理。不得混入平台开发 Spec、RFC、Agent Note、数据库与发布实现、工作项或上游渠道资料；尤其不能把 APIMart 等渠道名写成模型厂商或调用协议。
- 用户给的参考资料用于核实说明结构与参数语义，不是要求客户端阅读的“依据”。公开正文的全部链接也要符合这个边界。
- 早先错误的未跟踪文件 `docs/client-image-api.md` 已移除。当前 `public-docs/` 是使用说明源码；其中模型族叙述还需要按设计转成发布素材，不能直接当成每个模型的最终版本正文。

## 接续时的事实边界

新增 `documentation_url`、模型 `llms.txt` 路由、公共文档路由、文档素材导入、发布快照和存量补齐均未实现。当前目录说明只描述现有字段，未声称新字段已上线。本轮没有修改 Rust 代码。

新需求原先没有承接工单，本次由工作项 73 首次承接。其祖先与相关已关闭工单的核查结论已写入工作项；不要把它们的关闭误当成本次文档接口已交付。

[控制台 Spec](../../docs/specs/0001-admin-and-customer-consoles.md) 存在其他任务留下的待接受修订。模型使用文档 Spec 单独扩展其公开端点枚举，不接受或整理那些无关修订。

首次开启目录新字段必须覆盖所有当前可调用模型。现有准备范围是两个 GPT-Image-2.5 厂商模型及其平台别名；若实施时发现其他当前模型需要材料，按工作项与 Spec 返回 Planning，不能隐藏模型、编造说明或自行扩充模型范围。

## 已有验证证据

- `node scripts/decisions/check.mjs`：通过，检查 53 份记录。
- `git diff --check`：通过；原有八份变更文档的本地链接检查共 37 个目标，无缺失。
- 对客文档的内部文档与渠道术语扫描：无匹配；两个现有厂商模型的参数合同核对结果为除模型名称外一致。
- 本轮交接另检查本地链接与暂存区空白。实际提交与推送结果由本次 Git 历史及工作项后续记录提供。

未运行 Rust 全量检查、浏览器测试或真实 Provider 调用。本轮只有文档与规划改动，不产生外部费用。

## 建议技能（Suggested skills）

- `implement`：用户明确授权“执行实现”且 Implementation Gate 通过后、写第一行实现代码之前加载。
- `code-review`：实现结束后按工程流程执行 Implementation Review，修正后再交付。
- `verify`：按工程流程定位并读取该技能，再做交付验证；不要把本轮文档检查当作接口验收。
- `planning`：只有发现范围或合同缺口、需要改变既定方案时回到规划。
- `architect`：只分析实施中新发现的架构问题，已有设计直接引用 Agent Note。

提交按路径选择本任务文件，不使用 `git add -A` 混入其他任务；普通推送后分别核对 HEAD 与上游提交一致，不改写历史。
