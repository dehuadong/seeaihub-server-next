# 上传存储（阿里云 OSS）部署自检执行清单

- **用途**：在启用上传前证明部署侧的桶满足[图片上传与对象存储 Spec](../contracts/0007-image-upload-and-object-storage.md) §8 的匿名可读前置条件，并核对桶配置与平台签名在真实服务端可用。
- **性质**：部署侧在真实桶上执行的清单；产生的探针对象与上传对象是外部副作用，不是本平台的业务事实。
- **前置**：上传存储变量已配齐（见本文 §1）；有对象存储控制台或命令行工具；§4 另需服务的对外地址（下面写作 `$BASE`）、一个平台客户 API Key（下面写作 `$API_KEY`）与显式批准。
- **边界**：本文只列执行步骤与停止条件；合同归上述 Spec，机制与四步自检归[对象存储上传设计](../../.agents/notes/implemented/platform/2026-10-04-reference-image-upload.md)。平台的上传路径只有 PUT 与 HEAD 两个操作，不发 GET，也不签发预签名 URL：下面的核验读回与匿名读用对象存储控制台或命令行工具完成。自检不是运行期健康检查，平台在启动或运行期都不探测桶。

## 1. 配置形状检查

- 取值域归[对象存储上传设计](../../.agents/notes/implemented/platform/2026-10-04-reference-image-upload.md)，这里逐条核对：region 形如 `cn-hangzhou`（小写字母、数字与连字符，首尾是字母或数字，长度不超过 63）；bucket 是 3–63 位同类字符、不含点号；`UPLOAD_STORAGE_ACCESS_KEY_ID` 与 `UPLOAD_STORAGE_ACCESS_KEY_SECRET` 成对（要么都给、要么都不给）。
- 显式 endpoint 是含 scheme 的 origin：scheme 为 `https`，或只给 loopback（`127.0.0.1`、`::1`、`localhost`）的 `http`；不带凭证（userinfo）、path、query 与 fragment。省略 endpoint 时按 region 派生 `https://oss-{region}.aliyuncs.com`。
- 整组都不给＝未配置：进程照常启动，上传端点对该请求返回 `503 upload_storage_unavailable`；只给一部分或形状不合法：启动期被拒并点名变量，进程不启动。被点名的变量改回合法取值再启动，不靠自检绕过。
- 启动后确认上传端点不是 `503 upload_storage_unavailable`（即上传存储已配置）。

## 2. 写入探针对象（对象存储控制台或命令行工具）

- 用一个 1×1 合法 PNG 探针字节，对象键取 `reference-media/{调用者账户 id}/probe-{uuid}.png`（这段账户标识由操作者自定），与调用方素材的键不复用。
- 用控制台或命令行工具写入该对象（工具自己完成 V4 签名），期望成功。
- 在控制台核对该键存在，字节长度与内容类型与写入一致。

## 3. 核验、读回与公网 URL

- 对同一对象键核对字节长度与内容类型与写入值一致；不一致即这次自检不通过，按下文处置。
- 用控制台或命令行工具（自带签名）读回该对象，返回字节与写入字节逐字节相同。
- 再用不带凭证的客户端读同一对象的公网 URL（下面写作 `$PUBLIC_URL`）：

```sh
curl -sS -o probe-readback.bin -w '%{http_code}\n' "$PUBLIC_URL"
# 期望 200，且 probe-readback.bin 与写入的探针字节逐字节相同
```

- 公网 URL 取哪种寻址形态、`{endpoint-host}` 与寻址形态的关系见[图片上传与对象存储 Spec](../contracts/0007-image-upload-and-object-storage.md) §6；这里只确认不带凭证的客户端读的是归属该 endpoint 的形态（真实 OSS 虚拟主机式，显式 loopback 端点 path-style）。
- 对象清理：记录本次创建的探针对象键，在对象存储控制台按自己的保留策略清理；平台不代删、不续期。

## 4. 平台签名的真实服务端联调（需显式批准）

- 前置：上传存储变量指向真实桶；一个平台客户 API Key；显式批准——这一步会在真实桶写入一个对象，产生对象存储的请求费用与残留。
- 起 API 后发探针上传，**限制调用次数**：只发一次（`probe.png` 就是 §2 用的那个 1×1 PNG）：

```sh
curl -sS -X POST "$BASE/v1/uploads/images" \
  -H "Authorization: Bearer $API_KEY" \
  -F file=@probe.png
# 期望 200 与 {"url":"…","media_type":"image/png","byte_length":N}
```

- 这次上传的 PUT（带禁覆盖头）与 HEAD 元数据核验都由平台的签名实现发出：PUT 被真实服务端接受，即证明签名与 canonical URI 的拼装都对。
- 签名必查项：canonical URI 必须恒为 `/{bucket}/{key}`（含 bucket），与寻址形态无关（[对象存储上传设计](../../.agents/notes/implemented/platform/2026-10-04-reference-image-upload.md)）。请求走虚拟主机形态，漏拼 bucket 只会在这里被真实服务端拒。
- 对返回的 `url` 重跑 §3 的不带凭证读取，确认字节与上传一致；记录对象键以便清理。

## 失败时看什么

- `401`、`403`、`404`：密钥写错、权限不足（平台的上传路径需要 PUT 与 HEAD 两个权限）或 bucket / region 写错；对照[对象存储上传设计](../../.agents/notes/implemented/platform/2026-10-04-reference-image-upload.md) 的失败分类表。
- 平台上传返回 `200` 但匿名读不到：按[图片上传与对象存储 Spec](../contracts/0007-image-upload-and-object-storage.md) §8 处置。
- 平台上传被服务端拒签名：核对 canonical URI 是否含 bucket、签名头集合是否为全部 `x-oss-*` 加 `content-type` / `content-md5`、`x-oss-date` 是否为当前 UTC。
- 自检失败不产生任何状态变更：不写健康状态、不改配置、不建执行记录、不动账务；处置是部署侧改配置后重跑。

## 停止条件

- §2 与 §3 的四步任一步失败即停止，不在运行期降级，也不因结论不健康停用上传路径。
- §4 失败同样停止：先按「失败时看什么」改配置或修签名，再重跑。

## 执行记录

- 2026-10-05 平台签名与真实服务端联调（本机 `API_BIND=127.0.0.1:8081` 的 API；`UPLOAD_STORAGE_REGION` / `UPLOAD_STORAGE_BUCKET` / `UPLOAD_STORAGE_ACCESS_KEY_ID` / `UPLOAD_STORAGE_ACCESS_KEY_SECRET` 已配置，endpoint 省略、按 region 派生）：
  - §1 配置形状：进程照常启动，上传端点不回 `503 upload_storage_unavailable`。
  - §4 探针上传：`POST /v1/uploads/images` 发一次，`200`，`media_type=image/png`、`byte_length=67`；对象键 `reference-media/2026-10-05/12f251db-2154-4225-9bc0-bfca0a505fe1.png`（该次联调在键形态改为按调用者账户之前执行，第一段是当时的写入日期）。带禁覆盖头的 PUT 与 HEAD 元数据核验都被真实服务端接受，证明签名与 `/{bucket}/{key}` canonical URI 的拼装正确。
  - §3 匿名读回：**通过**。桶 `seeai` 改为匿名可读后，不带凭证读该对象返回 `200`、`image/png`、67 字节，与探针逐字节相同（带与不带代理环境变量各一次）。首次联调时桶 ACL 为私有，同样的一次读返回 `403 AccessDenied`（`EC 0003-00000001`）；按 Spec 0007 §8 改为公共读后通过。
  - 未做：本机没有 `ossutil` / `aliyun` CLI，带签名的逐字节读回没跑（PUT 后 HEAD 已按字节长度与内容类型核验）；探针对象由部署侧按保留策略清理。
