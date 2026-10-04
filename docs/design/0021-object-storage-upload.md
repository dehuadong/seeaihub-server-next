主题: 对象存储上传：模块划分、签名、渠道配置与健康检查
当前修订: v1
状态: 已接受
承接: [图片上传与对象存储 Spec v1](../specs/0007-image-upload-and-object-storage.md) §2–§8；并与[同步图片网关 Spec v3](../specs/0005-synchronous-image-gateway.md) §1、§3 的输入图片形态收敛同一变更实施
依赖: [分层架构](0004-layered-architecture.md)、[图片上传与对象存储 Spec](../specs/0007-image-upload-and-object-storage.md)、[同步网关设计](0017-synchronous-image-gateway.md) §2、[图片透传决定](../adr/0019-images-pass-through-without-asset-storage.md)（结果侧不落盘与只有同步形态的结论继续适用）

# 对象存储上传：模块划分、签名、渠道配置与健康检查

本稿拥有上传能力的模块落点、配置项、数据模型、对象键与签名机制、健康检查步骤、重试与失败分类、与参考实现的差异，以及分片实施顺序。上传对调用方的行为合同由[图片上传与对象存储 Spec](../specs/0007-image-upload-and-object-storage.md)拥有；生成请求内的图片形态收敛由[同步图片网关 Spec](../specs/0005-synchronous-image-gateway.md)拥有。

## 1. 模块划分

| 层 | 落点 | 拥有什么 |
| --- | --- | --- |
| 领域 | `crates/domain/src/upload_media.rs`（新） | 允许的媒体类型与扩展名白名单、单文件字节上限、魔数到规范 MIME 的判定、对象键构造（前缀、UTC 日期、随机标识、扩展名）、渠道公开配置的字段白名单与取值域归一化、健康检查的结论三态与探针对象名、失败分类枚举。纯类型与纯函数，不依赖 HTTP、对象存储、数据库与环境变量。 |
| 适配器 | `crates/adapter-object-storage`（新 crate） | 阿里云 OSS V4 自实现签名（header 模式的 PUT / HEAD / GET）、请求构造（host、canonical URI、签名头集合、禁覆盖头、`x-oss-date`、`x-oss-content-sha256`）、有界传输（单请求超时、无隐式重试、不跟随重定向）、HTTP 状态到失败分类的映射、探针写入与元数据核验。实现 application 的 `ObjectStorage` 端口；不读环境变量、不认数据库、不认识渠道表。 |
| 应用 | `crates/application/src/image_upload.rs`、`crates/application/src/upload_channels.rs`（新） | 两个用例。`ImageUploadService`：取激活渠道运行时、校验媒体与上限、构造对象键、写入、元数据核验、组装结果。`UploadChannelService`：列渠道、写公开配置、按不可变配置快照做健康检查并原子激活、停用。端口是 `ObjectStorage` 与 `UploadChannelRepository`；渠道密钥经既有 `CredentialProvider` 只从环境变量解析；对客错误码在此收口。 |
| 持久化 | `crates/persistence/src/upload_channels.rs`（新）与迁移 | `PgUploadChannelRepository`：行投影（不含密钥）、公开配置写入与归一化结果的落库、按指纹的原子激活、停用守卫需要的事实读。SQL 与事务边界只在这里。 |
| API | `apps/api/src/main.rs` | 客户侧 `POST /v1/uploads/images`：复用既有 API Key 认证（`authenticate`）与"认证及准入先于消费正文"的中间件写法；管理端四条路由与 handler；错误信封沿用既有 `{"error":{"code","message"}}`。 |
| 前端 | `apps/web/src/console/pages/UploadChannels.tsx`（新，管理端） | 渠道列表、编辑公开配置、激活与停用、展示密钥是否配置、是否激活与最近一次健康结论。客户控制台（`apps/web/src/portal/`）不加页面。 |

不新增 Provider、Offering 与发布物：上传渠道不进 Runtime Revision、不进 `catalog.*` / `supply.*` / `publication.*`。上传也不进 `generation.*` / `ledger.*`：上传没有执行、没有计量、没有资金占用，也不计费、不限配额。生成入口的请求与响应形状不因上传能力改变。

## 2. 生成入口的输入形态收敛

上传端点的存在理由是把本地文件变成公网 URL，因此生成入口同时收敛为只收公网 URL，两件事在同一变更里实施：生成接口收 `http(s)` 公网 URL，`data:` URL 与 multipart 文件部件一律拒绝。

`crates/domain/src/image_parameters.rs` 只负责"契约字段名与空值"的规则，形态判定落在入口：`crates/adapter-sdk/src/gateway.rs` 的 `InputImage::from_raw` 不再从调用方取值构造 `DataUrl` / `Bytes` 两态，入口只构造 `Url`，其余一律映射为 `400 public_image_url_required`。

`apps/api/src/main.rs` 的 multipart 解析里，`image` / `mask` 的文件部件不再经 `form_image_bytes` 构造成 `InputImage::Bytes`，而是直接返回 `public_image_url_required`。文件部件字节因此不再进入执行路径，与[同步图片网关 Spec](../specs/0005-synchronous-image-gateway.md) §2 的载荷边界一致。

拒绝必须发生在幂等预查之后、按当前合同解释请求的那一步：命中同一幂等记录时走的是 `replay_recorded`，它按记录冻结的合同与指纹算法比对原始请求面；把形态判定提到预查之前会让以 `data:` URL 或文件部件受理的历史记录无法比对，破坏同键去重合同。

Adapter 内部仍保留 data URL 与字节两种形态：需要字节的渠道（AIHubMix 的 edits）自己从公网 URL 取图，形态差异按[图片透传决定](../adr/0019-images-pass-through-without-asset-storage.md)留在 ② 内部吸收，平台不搬运。

## 3. 配置项与环境变量

| 类别 | 名称 | 说明 |
| --- | --- | --- |
| 公开配置（入库，管理端可改） | `region` | 必填，如 `cn-hangzhou` |
| 公开配置（入库，管理端可改） | `bucket` | 必填，3–63 位小写字母、数字、连字符与点 |
| 公开配置（入库，管理端可改） | `endpoint` | 可选；省略时按 region 派生为 `https://oss-{region}.aliyuncs.com`，显式给出时必须等于该值（拒绝端口、userinfo、path、query、fragment、自定义域名与内网域名） |
| 环境变量（仅部署侧） | `UPLOAD_STORAGE_ACCESS_KEY_ID` | 访问密钥标识；与下一条一起构成完整凭据 |
| 环境变量（仅部署侧） | `UPLOAD_STORAGE_ACCESS_KEY_SECRET` | 访问密钥；两条只配一条时该渠道按未配置处理，进程照常启动 |
| 环境变量（本机上限） | `UPLOAD_MAX_REQUEST_BYTES` | 请求体上限，即单文件上限加 multipart 协议余量；余量多少在实施时取值 |
| 环境变量（本机上限） | `UPLOAD_SLOTS` | 本机上传并发许可数，取值在实施时按本机容量定 |
| 环境变量（本机上限） | `UPLOAD_MAX_BUFFER_BYTES` | 本机上传内存预算，取值在实施时定 |
| 环境变量（本机上限） | `UPLOAD_REQUEST_TIMEOUT_SECONDS` | 单次写入的对象存储请求超时，取值在实施时定 |
| 环境变量（本机上限） | `UPLOAD_HEALTH_PROBE_TIMEOUT_SECONDS` | 健康探针单请求超时，取值在实施时定 |

单文件上限是领域常量（20 MiB，严格小于 20971520 字节），落在 `crates/domain/src/upload_media.rs`，不是环境变量。重试的总尝试次数与退避基准同样由服务端配置、在实施时取值（§7）。

密钥只从环境变量读；公开配置只入库。两者都不完整时该渠道不可激活，管理端按"未配置"展示，进程启动不因缺密钥失败。

## 4. 数据模型

新增 schema `storage` 与一张表：

| 列 | 类型 | 说明 |
| --- | --- | --- |
| `id` | `bigserial` 主键 | 渠道标识 |
| `provider` | `text NOT NULL` | 渠道类型，今天只有 `aliyun_oss`，CHECK 具名 |
| `name` | `text NOT NULL` | 管理端展示名 |
| `public_config` | `jsonb NOT NULL DEFAULT '{}'` | 白名单归一化后的 region / bucket / endpoint |
| `active` | `boolean NOT NULL DEFAULT false` | 是否激活 |
| `health_state` | `text NOT NULL DEFAULT 'unknown'` | `unknown` / `healthy` / `unhealthy`，CHECK 具名 |
| `health_checked_at` | `timestamptz` | 最近一次健康检查时刻 |
| `health_detail` | `text` | 脱敏、有界的失败说明（阶段、状态码、请求标识） |
| `created_at`、`updated_at` | `timestamptz NOT NULL` | — |

两个唯一约束：`UNIQUE (provider)` 保证一种渠道类型只有一行；`UNIQUE (active) WHERE active` 保证任意时刻至多一个激活渠道。迁移用下一个编号（当前最高 `0039`），种下一行 `aliyun_oss`（空公开配置、未激活），管理端不创建渠道类型。

不记录上传事实。对象存储本身是对象的事实源，平台不承担保留期与删除，再记一张上传表只会造出第二份必然腐坏的事实；将来要配额、审计或用量时另立它自己的属主。

激活按指纹提交：指纹由渠道标识、渠道类型、公开配置与 `updated_at` 组成；激活事务内按 `FOR UPDATE` 重读该行比对指纹，不一致返回冲突，随后清掉既有激活行再置目标行激活，同事务提交。唯一部分索引是兜底，不是唯一防线。

## 5. 对象键与签名

对象键是 `reference-media/{YYYY-MM-DD}/{uuid}.{ext}`：日期取写入时的 UTC 日期，`uuid` 是随机 v4，`ext` 由规范 MIME 反推（`image/jpeg` → `jpg`，`image/png` → `png`，`image/webp` → `webp`）。调用方文件名不进键、不进对象元数据。

签名用阿里云 OSS V4，header 模式：

- 派生密钥链是 `HMAC("aliyun_v4" + SK, date) → region → "oss" → "aliyun_v4_request"`；
- canonical request 是 `Verb`、canonical URI（`/{bucket}/{key}`）、排序后的 canonical query、canonical headers、附加签名头列表与 `UNSIGNED-PAYLOAD`；
- 默认签名头集合是所有 `x-oss-*`、`content-type` 与 `content-md5`，其余显式头进附加签名头；
- `x-oss-date` 每次请求重新取当前 UTC 时刻，不缓存、不复用时间戳；
- 禁止覆盖用请求头 `x-oss-forbid-overwrite: true` 参与签名，不用 query 参数形态。

签名器只实现 header 模式。不实现预签名 URL、不实现 OSS STS 与临时凭证、不实现其他对象存储厂商的签名。

一次上传的请求序列是：PUT 对象（带禁覆盖头与签名）→ HEAD 读取元数据核验字节长度与内容类型（与提交一致）→ 返回公网 URL。已确认写入成功的对象不重复 PUT；HEAD 失败只重试 HEAD。

## 6. 健康检查

健康检查对不可变的配置快照执行，服务端生成 1×1 的合法 PNG 探针字节与独立对象键 `reference-media/{UTC 日期}/probe-{uuid}.png`，不读取调用方文件：

1. PUT 探针对象，同样带禁覆盖头与签名；
2. HEAD 核验探针对象的字节长度与内容类型与写入一致；
3. GET 探针对象（header 模式签名，`Authorization` 头），返回字节与写入字节逐字节相同；
4. 不带凭证的客户端 GET 该对象的公网 URL，返回字节与写入字节逐字节相同。

四步全过判健康，写入 `health_state = 'healthy'` 与时刻；任一步失败判不健康并写入脱敏说明，激活不发生，原激活渠道与其配置不变。第 4 步判的是平台对外返回的那个公网 URL 真的匿名可读——桶配错时上传会返回 `200` 而 URL 读不到，这一条把这种组合挡在激活之前。

探针对象不复用调用方素材的键，平台也不删除探针对象——平台没有删除能力，对象的保留与清理归对象存储控制台。

## 7. 重试与失败分类

| 失败事实 | 分类 | 处置 |
| --- | --- | --- |
| 传输网络错误、连接超时、`408` | 可重试 | 同一对象键重试，总尝试次数有上限 |
| 对象存储 `429` | 可重试 | 遵循有界整数秒 `Retry-After`，否则固定退避 |
| 对象存储 `5xx` | 可重试 | 同上 |
| `401` | 终态 | 凭证无效，`503 upload_storage_unavailable` |
| `403` | 终态 | 权限不足（缺 PUT/HEAD/GET 之一），`503 upload_storage_unavailable` |
| `404` | 终态 | bucket 或 region 不可访问，`503 upload_storage_unavailable` |
| `409` | 终态 | 禁止覆盖相撞，不换键重写，`503 upload_storage_unavailable` |
| 其他 `4xx` | 终态 | `503 upload_storage_unavailable` |

传输层不配置隐式重试、不跟随重定向，重试预算只由上面的编排层拥有，避免内外层相乘。退避基准与总尝试次数在实施时按本机上限配置取值，取值不改变上表语义。

失败只写稳定错误码、HTTP 状态与有界脱敏诊断；密钥、签名、对象字节不进日志、trace、告警与响应。

## 8. 与参考实现的差异

参考实现是同一产品上一代的 uploads 模块，在本仓库之外，不作为合同：`/mnt/d/workspace/seeaihub/src/service/src/uploads/`、`/mnt/d/workspace/seeaihub/src/service/src/admin_upload_channels/`、`/mnt/d/workspace/seeaihub/src/frontend/src/pages/admin/UploadChannels.tsx`。本稿与它的差异如下。

- **删预签名读取**：参考实现把 86400 秒的 GET 预签名 URL 当作上传结果返回，响应里带 `expires_at`。本设计返回不带签名的公网 URL，不签发预签名 URL，签名器只做 header 模式。
- **不要求私有桶，改为要求匿名可读**：参考实现在健康检查里断言"匿名 GET 必须被拒"以证明桶私有。本设计返回不带签名的公网 URL，健康检查第 4 步要求匿名 GET 成功且字节逐字节一致，桶的公网可读因此是激活的必要条件；桶配错只体现为激活失败，不会再出现"上传 200、URL 读不到"。
- **删有效期与保留期机制**：参考实现的临时凭证剩余有效期下限（86400 + 300 秒）、预签名有效期常量与 `expires_at` 字段整体不存在；本设计没有 TTL、没有续期、没有平台侧保留期与删除。
- **只做阿里云 OSS**：参考实现预置阿里云 OSS 与腾讯云 COS 两种渠道，含 COS XML 签名、临时 token 契约与 STS 拒绝分支。本设计只有 `aliyun_oss` 一种渠道类型，删掉 COS 签名、token 契约与双渠道预置。
- **单文件，无 manifest 与批次状态**：参考实现一次最多 16 个文件，用 manifest 文本部件把文件部件与 `client_ref` 关联，逐项失败放在 `200` 的 `items` 里并给 `ready` / `partial` / `failed`。本设计一个文件一个结果，失败即请求级错误。
- **图片字节不落临时文件**：参考实现把超过 64 KiB 的部件 spool 到临时文件，总量上限 320 MiB。本设计单文件上限 20 MiB，字节只在内存，与[同步图片网关 Spec](../specs/0005-synchronous-image-gateway.md) §2 的不落盘边界一致。
- **收窄媒体类型面**：参考实现同时收图片、视频与音频并分三类上限。本设计只收图片。
- **鉴权与端点路径**：参考实现是 `/api/v1/uploads/reference-media` 加 OAuth 会话；本设计是 `/v1/uploads/images` 加平台客户 API Key，与生成接口同一套凭证。
- **密钥来源**：参考实现用按渠道嵌套的环境变量键名；本设计用平铺的环境变量名（§3）。
- **禁覆盖的签名位置**：参考实现早期用 query 参数形态，实测被对象存储拒绝后改为请求头形态。本设计直接按请求头形态参与签名。

## 9. 分片实施顺序与验证

1. 领域规则：媒体类型与扩展名白名单、单文件上限、魔数到规范 MIME、对象键构造、公开配置白名单与归一化、健康结论三态、失败分类。验证：`crates/domain` 内单元测试，覆盖白名单边界、上限的严格小于语义、日期与扩展名、公开配置的非法取值与未知字段。
2. `crates/adapter-object-storage`：签名器与传输。验证：对官方 V4 测试向量与既有实测样本的单元测试；用脚本化传输断言请求次数、重试次数与不重复 PUT；不发真实网络请求。
3. 持久化：`storage.upload_channels` 表、迁移、`PgUploadChannelRepository`。验证：迁移用例断言表与唯一约束存在、种下行未激活；激活用例断言并发激活不产生双激活与零激活、指纹变化返回冲突。
4. 应用用例：`ObjectStorage` 与 `UploadChannelRepository` 端口、`ImageUploadService`、`UploadChannelService`。验证：用假对象存储端口覆盖成功、各失败分类、重试耗尽、禁覆盖冲突、健康检查四步的失败注入；断言上传不触碰资金与执行记录端口。
5. API：客户侧上传路由与管理端四条路由。验证：`apps/api/tests/http_contract/` 的端到端用例加进程内假对象存储，覆盖 Spec 0007 的 A1–A10；断言无效凭证在正文前被拒、上传前后账户与账本不变。
6. 前端：管理端渠道页。验证：浏览器用例拦管理端接口，断言公开配置编辑、激活/停用、健康结论展示，以及页面不出现密钥。
7. 生成入口收敛：`InputImage` 只构造公网 URL 形态、multipart 文件部件被拒、拒绝发生在幂等预查之后。验证：端到端用例覆盖 Spec 0005 的 A12 与 A2，含以 `data:` URL 受理的历史记录在改版后仍按原记录回应。
8. 文档与配置同步：[代码结构图](../architecture.md)、根 `CONTEXT.md` 的参考图词条、`docs/operations/configuration.md` 与 `.env.example` 增补 §3 的环境变量。

第 1–6 片之间只有端口依赖，可按上表顺序串行；第 7 片与第 5 片改同一批入口文件，必须串行实施，不能并行编辑。

## 10. 验证边界

上传的自动化验证只打进程内的假对象存储，不访问真实 OSS：普通测试不得产生外部对象，也不得依赖真实桶。

真实桶只用于人工受控验证（健康检查、跨区域访问、大文件边界、桶的公网可读配置），需要显式批准，并记录创建的对象键以便在对象存储控制台清理。它不产生模型调用费用，但会产生对象存储的请求费用与对象残留，属外部副作用，按[受控验证清单](../verification/)的既有约定留档。

签名正确性不靠真实桶证明：单元测试对官方 V4 测试向量，端到端用例只证明请求构造与编排，真实桶验证只补齐签名与桶配置在真实服务端的联调结论。
