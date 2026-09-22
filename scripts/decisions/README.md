# 决策记录工具

Agent Notes 的校验工具。`check.mjs` 按 [`.agents/notes/README.md`](../../.agents/notes/README.md) 的「生命周期」「文件格式」「检查」三节校验记录；`lib.mjs` 放共用的遍历、元数据与格式规则。**不生成索引**：`{生命周期}/{分类}/` 目录树就是清单，根目录出现 `INDEX.md` 会被拒绝。

从仓库根运行，用 Node.js，无第三方依赖：

```sh
node scripts/decisions/check.mjs
```

报错信息是中文，逐条改即可。检查通过只代表机械规则成立——批准是否属实、交付是否充分、备选方案与替代关系是否属实仍由审阅判断。

## 本地修补（升级 setup bundle 时先逐文件比对，不要覆盖）

- `lib.mjs` 的 `resolveTarget()` 用显式栈逐段解析相对链接，不用 `path.resolve` / `path.normalize` / `path.join`：本机 Node v24.10.0（Windows）上这些原语会把正确的跨目录相对链接误判为断链。
- `lib.mjs` 的 `rootDir` 是仓库管理根目录（`../../`）。原版写成 `../../../` 会落到仓库外一层，读注册表失败又被 `.catch(() => null)` 吞掉——检查会静默空跑。
- `check.mjs` 有一处本地新增：检查 `docs/agents/artifacts.md` 除「历史工件与新旧衔接」一节之外没有具体日期、提交号与变更说明。

bundle 里若带索引渲染器（`update-index.mjs`）与索引新鲜度检查，不随升级引入。
