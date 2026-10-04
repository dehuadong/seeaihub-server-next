# 上传存储（阿里云 OSS）部署自检执行清单

- **用途**：在启用上传前证明部署侧的桶满足[图片上传与对象存储 Spec](../specs/0007-image-upload-and-object-storage.md) §8 的匿名可读前置条件，并核对签名与桶配置在真实服务端可用。
- **性质**：部署侧在真实桶上执行的清单；产生的探针对象是外部副作用，不是本平台的业务事实。
- **前置**：上传存储变量已配齐（见本文 §1）；有对象存储控制台或命令行工具可核对桶策略与对象。
- **边界**：本文只列执行步骤与停止条件；合同归上述 Spec，机制与四步自检归[对象存储上传设计](../design/0021-object-storage-upload.md) §6。自检不是运行期健康检查，平台在启动或运行期都不探测桶。

## 1. 配置形状检查

- 取值域归[对象存储上传设计](../design/0021-object-storage-upload.md) §3、§4，这里逐条核对：region 形如 `cn-hangzhou`（小写字母、数字与连字符，首尾是字母或数字，长度不超过 63）；bucket 是 3–63 位同类字符、不含点号；`UPLOAD_STORAGE_ACCESS_KEY_ID` 与 `UPLOAD_STORAGE_ACCESS_KEY_SECRET` 成对（要么都给、要么都不给）。
- 显式 endpoint 是 `https`，或只给 loopback（`127.0.0.1`、`::1`、`localhost`）的 `http`；不带凭证（userinfo）、path、query 与 fragment。省略 endpoint 时按 region 派生 `https://oss-{region}.aliyuncs.com`。
- 整组都不给＝未配置：进程照常启动，上传端点对该请求返回 `503 upload_storage_unavailable`；只给一部分或形状不合法：启动期被拒并点名变量，进程不启动。被点名的变量改回合法取值再启动，不靠自检绕过。
- 启动后确认上传端点不是 `503 upload_storage_unavailable`（即上传存储已配置）。

## 2. PUT 探针对象

- 用服务端生成的 1×1 合法 PNG 探针字节，对象键取 `reference-media/{UTC 日期}/probe-{uuid}.png`，与调用方素材的键不复用。
- 带禁覆盖头 `x-oss-forbid-overwrite: true` 与 V4 签名发出 PUT，期望 `200`。
- 在对象存储控制台核对该键存在，字节长度与内容类型与写入一致。

## 3. HEAD 核验

- 对同一对象键发 HEAD（header 模式签名），期望 `200`。
- 比对返回的字节长度与内容类型与写入值一致；不一致即这次自检不通过，按下文处置。

## 4. 读回比对与公网 URL

- 先用带 `Authorization` 头的 header 模式 GET 读回对象，确认返回字节与写入逐字节相同（证明签名与读权限）。
- 再用不带凭证的客户端 GET 公网 URL，确认返回字节与写入逐字节相同（证明桶匿名可读）。
- 公网 URL 取哪种寻址形态、它与签名输入的关系见[图片上传与对象存储 Spec](../specs/0007-image-upload-and-object-storage.md) §6；这里只确认不带凭证的客户端读的是归属该 endpoint 的形态（真实 OSS 虚拟主机式，显式 loopback 端点 path-style）。
- 签名必查项：header 模式 GET 的 canonical URI 必须恒为 `/{bucket}/{key}`（含 bucket），与寻址形态无关（[对象存储上传设计](../design/0021-object-storage-upload.md) §5）。漏拼 bucket 只会在这里被真实服务端拒。
- 对象清理：记录本次创建的探针对象键，在对象存储控制台按自己的保留策略清理；平台不代删、不续期。

## 失败时看什么

- `401`、`403`、`404`：密钥写错、权限不足（缺 PUT / HEAD / GET 之一）或 bucket / region 写错；对照[对象存储上传设计](../design/0021-object-storage-upload.md) §7 的失败分类表。
- 上传返回 `200` 但匿名 GET 读不到：桶策略没有匿名可读，按[Spec 0007](../specs/0007-image-upload-and-object-storage.md) §8 改桶策略。
- 签名被服务端拒：核对 canonical URI 是否含 bucket、签名头集合是否为全部 `x-oss-*` 加 `content-type` / `content-md5`、`x-oss-date` 是否为当前 UTC。
- 自检失败不产生任何状态变更：不写健康状态、不改配置、不建执行记录、不动账务；处置是部署侧改配置后重跑。

## 停止条件

- 四步任一步失败即停止，不在运行期降级，也不因结论不健康停用上传路径。

## 执行记录

（实现落地后在此填日期、步骤、状态码、探针对象键与结论；不写密钥、签名参数与完整 URL。）
