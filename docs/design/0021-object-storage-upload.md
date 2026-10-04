主题: 对象存储上传：模块划分、签名与上传存储配置
当前修订: v2
状态: 待评审
承接: [图片上传与对象存储 Spec v2](../specs/0007-image-upload-and-object-storage.md) §2–§8；并与[同步图片网关 Spec v3](../specs/0005-synchronous-image-gateway.md) §1、§3 的输入图片形态收敛同一变更实施
依赖: [分层架构](0004-layered-architecture.md)、[图片上传与对象存储 Spec](../specs/0007-image-upload-and-object-storage.md)、[同步网关设计](0017-synchronous-image-gateway.md) §2、[图片透传决定](../adr/0019-images-pass-through-without-asset-storage.md)（结果侧不落盘与只有同步形态的结论继续适用）、[参考图上传决定](../adr/0022-reference-image-upload-endpoint.md)

# 对象存储上传：模块划分、签名与上传存储配置

本稿拥有上传能力的模块落点、配置项与启动期形状校验、对象键与签名机制、桶匿名可读的部署自检、重试与失败分类、分片实施顺序与验证边界。上传对调用方的行为合同由[图片上传与对象存储 Spec](../specs/0007-image-upload-and-object-storage.md)拥有；生成请求内的图片形态收敛由[同步图片网关 Spec](../specs/0005-synchronous-image-gateway.md)拥有。

## 1. 模块划分

对象存储是平台调用的**外部服务**，不是生成上游：它的适配实现与 ② Adapter Driver 并列，属于**外部服务适配器**一类（[分层架构](0004-layered-architecture.md) §1.1），不进 Runtime Revision，也不参与选路。

| 层 | 落点 | 拥有什么 |
| --- | --- | --- |
| 领域 | `crates/domain/src/upload_media.rs`（新） | 允许的媒体类型与扩展名白名单、单文件字节上限、魔数到规范 MIME 的判定、对象键构造（前缀、UTC 日期、随机标识、扩展名）、失败分类枚举。纯类型与纯函数，不依赖 HTTP、对象存储、数据库与环境变量。 |
| 应用 | `crates/application/src/image_upload.rs`（新） | 上传用例 `ImageUploadService`：读配置、校验媒体与上限、构造对象键、写入、元数据核验、组装结果。配置的取值域与 region 到 endpoint 的派生是平台自己的规则，不落 domain；端口是 `ObjectStorage`；访问密钥经既有 `CredentialProvider` 只从环境变量解析；对客错误码在此收口。 |
| 外部服务适配器 | `crates/adapter-object-storage`（新 crate） | 阿里云 OSS V4 自实现签名（header 模式的 PUT / HEAD / GET）、请求构造（host、canonical URI、签名头集合、禁覆盖头、`x-oss-date`、`x-oss-content-sha256`）、有界传输（单请求超时、无隐式重试、不跟随重定向、不读环境变量、不认数据库）、HTTP 状态到失败分类的映射。实现 application 的 `ObjectStorage` 端口。 |
| API | `apps/api/src/main.rs` | 客户侧 `POST /v1/uploads/images`：复用既有 API Key 认证（`authenticate`）与"认证及准入先于消费正文"的中间件写法；错误信封沿用既有 `{"error":{"code","message"}}`。 |

上传不进 `catalog.*` / `supply.*` / `publication.*`：它不是 Provider 的调用入口，没有 Offering、渠道与发布物。上传也不进 `generation.*` / `ledger.*`：上传没有执行、没有计量、没有资金占用，也不计费、不限配额。生成入口的请求与响应形状不因上传能力改变。

上传存储是部署侧的一组环境变量，平台只支持阿里云 OSS 一种：没有存储表与迁移，没有管理端路由与页面，也没有激活状态与健康结论。

上传的本机许可、字节预算与断开检测复用 [`apps/api/src/supervisor.rs`](../../apps/api/src/supervisor.rs) 已有的机制：许可与预算用独立的上传名额与独立预算（`UPLOAD_SLOTS` / `UPLOAD_MAX_BUFFER_BYTES`），不挤占生成的 `GENERATION_EXECUTION_SLOTS` 与 `GENERATION_MAX_MEMORY_BYTES`；连接断开监视与停机排空是进程级机制（连接注册表、Supervisor），上传与生成共用，不新建模块。

## 2. 生成入口的输入形态收敛

上传端点的存在理由是把本地文件变成公网 URL，因此生成入口同时收敛为只收公网 URL，两件事在同一变更里实施：生成接口收 `http(s)` 公网 URL，`data:` URL 与 multipart 文件部件一律拒绝，返回 `400 public_image_url_required`。

拒绝点在 [`apps/api/src/main.rs`](../../apps/api/src/main.rs) 的 `interpret_current_inputs`，即幂等预查（`lookup_recorded`）未命中之后、按当前合同解释请求的那一步。命中同一幂等记录时走的是 `replay_recorded`，它按记录冻结的合同与指纹算法比对原始请求面；把形态判定提到预查之前会让以 `data:` URL 或文件部件受理的历史记录无法比对，破坏同键去重合同（[Spec 0005](../specs/0005-synchronous-image-gateway.md) §4）。

`InputImage` 的三态可见性按解析路径分开：解析层（multipart 的 `form_image_bytes`、JSON 文本图片）与 `replay_recorded` 仍构造 `Url` / `DataUrl` / `Bytes` 三态，后两态**只用于**按记录冻结的规则比对历史请求；`interpret_current_inputs` 对当前合同只构造 `Url`，其余一律映射为 `400 public_image_url_required`，因此进入执行路径的 `GatewayInput` 只承载公网 URL。`crates/domain/src/image_parameters.rs` 只负责"契约字段名与空值"的规则，形态判定仍在入口。

APIMart 的内联上传通路随之删除：`crates/adapter-apimart/src/lib.rs` 里把 data URL / 字节上传到上游 `POST /v1/uploads/images` 取 URL 的那条路（含 `MAX_UPLOAD_BYTES` / `MAX_TOTAL_UPLOAD_BYTES`、`upload_image*`、`validate_uploaded_url`、`upload_filename` 等常量与函数，以及相关注释）、对应用例与 `apps/api/tests/http_contract/harness.rs` 的上传闸门一起删掉——生成入口只收公网 URL 之后，APIMart 收公网 URL 逐字透传，内联上传没有入口再用，上游上传失败这种可证明未受理的失败（`SafeBeforeAcceptance`，[ADR 0011](../adr/0011-safe-before-acceptance-does-not-retry-yet.md)）也不再是它的起因。

强杀矩阵里以这条通路为注入点的「提交前（已写提交声明、生成请求未发）」格随之退役：通路删掉后该状态在进程外没有可钉的屏障——提交声明之后的下一个外部动作就是生成请求本身，而进程内注入不构成强杀。该格要证的恢复结论由「提交中／接受后句柄未写入」格承担：`submitting` 且没有句柄时保留 Hold 与渠道槽位、进对账、不重提。

AIHubMix 的取图路径形态不变：它的 edits 端点要文件部件，所以由 Adapter 自己从公网 URL 取字节，这是它唯一的参考图来源。

## 3. 配置项与环境变量

| 类别 | 名称 | 说明 |
| --- | --- | --- |
| 环境变量（仅部署侧） | `UPLOAD_STORAGE_REGION` | 必填；形如 `cn-hangzhou`，取值是小写字母、数字与连字符，首尾必须是字母或数字，长度不超过 63 |
| 环境变量（仅部署侧） | `UPLOAD_STORAGE_BUCKET` | 必填；3–63 位小写字母、数字与连字符，首尾必须是字母或数字，**不含点号** |
| 环境变量（仅部署侧） | `UPLOAD_STORAGE_ENDPOINT` | 可选；省略时按 region 派生为 `https://oss-{region}.aliyuncs.com`，显式给出时必须是 `https`、由主机名与可选端口组成，不带凭证（userinfo），不带 path、query 与 fragment |
| 环境变量（仅部署侧） | `UPLOAD_STORAGE_ACCESS_KEY_ID` | 访问密钥标识；与下一条一起构成完整凭据 |
| 环境变量（仅部署侧） | `UPLOAD_STORAGE_ACCESS_KEY_SECRET` | 访问密钥 |
| 环境变量（本机上限） | `UPLOAD_MAX_REQUEST_BYTES` | 请求体上限，即单文件上限加 multipart 协议余量；余量多少在实施时取值 |
| 环境变量（本机上限） | `UPLOAD_SLOTS` | 本机上传并发许可数，取值在实施时按本机容量定 |
| 环境变量（本机上限） | `UPLOAD_MAX_BUFFER_BYTES` | 本机上传内存预算，取值在实施时定 |
| 环境变量（本机上限） | `UPLOAD_REQUEST_TIMEOUT_SECONDS` | 单次写入的对象存储请求超时，取值在实施时定 |

桶名不含点号：`{bucket}.{host}` 的虚拟主机形态下，含点的桶名会撞 `*.oss-{region}.aliyuncs.com` 那种只覆盖一级标签的通配符证书，也会让主机名与桶名在点号上分不开。

单文件上限是领域常量（20 MiB，严格小于 20971520 字节），落在 `crates/domain/src/upload_media.rs`，不是环境变量。重试的总尝试次数与退避基准同样由服务端配置、在实施时取值（§7）。

## 4. 配置装载与启动期校验

配置装载只判形状，不做活体探测：region 与 bucket 的形状、显式 endpoint 的 scheme / 主机名 / 端口与禁止的凭证、path、query、fragment，以及访问密钥两条要么都有要么都不给。全部上传存储变量都不存在＝未配置：进程照常启动，上传端点对该请求返回 `503 upload_storage_unavailable`。给了其中一部分（缺 region、bucket 或任一条密钥）与取值形状不合法同样按形状不合法处理：启动期拒绝并点名，进程不启动。对象存储可达性、bucket 是否存在与桶是否匿名可读都不在启动判据里。

访问密钥取两处固定引用：`UPLOAD_STORAGE_ACCESS_KEY_ID` 与 `UPLOAD_STORAGE_ACCESS_KEY_SECRET`，各经既有 `CredentialProvider::resolve` 取一次（`ProviderCredential` 仍只装一个值，不为上传新增端口）。两条要么都给、要么都不给：只给一条在启动期按形状不合法拒绝；两条都不给时密钥视为未配置，整组是否算未配置按上一条判定。密钥解析返回 `Configuration` 类错误时对客映射为 `503 upload_storage_unavailable`，不是 `500 internal_error`。

`UPLOAD_STORAGE_ENDPOINT` 由部署侧按自己的对象存储填写，也是端到端用例把请求指向进程内假对象存储的注入缝。`https` 只在配置装载时校验；适配器不重复判 scheme，按配置里的地址直接发请求。端到端夹具直接构造一份指向进程内假对象存储的 `http` 配置值，注入缝不被适配器层挡回。`endpoint` 只是地址，不是凭证——访问密钥仍从环境变量读。

## 5. 对象键与签名

对象键是 `reference-media/{YYYY-MM-DD}/{uuid}.{ext}`：日期取写入时的 UTC 日期，`uuid` 是随机 v4，`ext` 由规范 MIME 反推（`image/jpeg` → `jpg`，`image/png` → `png`，`image/webp` → `webp`）。调用方文件名不进键、不进对象元数据。

签名用阿里云 OSS V4，header 模式：

- 派生密钥链是 `HMAC("aliyun_v4" + SK, date) → region → "oss" → "aliyun_v4_request"`；
- canonical request 是 `Verb`、canonical URI（虚拟主机形态下是 `/{key}`）、排序后的 canonical query、canonical headers、附加签名头列表与 `UNSIGNED-PAYLOAD`；
- 默认签名头集合是所有 `x-oss-*`、`content-type` 与 `content-md5`，其余显式头进附加签名头；`host` 不进 canonical headers——它不是 `x-oss-*`，也不在显式头里，只作为 HTTP 请求的 `Host` 发出；
- `x-oss-date` 每次请求重新取当前 UTC 时刻，不缓存、不复用时间戳；
- 禁止覆盖用请求头 `x-oss-forbid-overwrite: true` 参与签名，不用 query 参数形态；
- 不发 `x-oss-object-acl`，也不发其他 ACL 头：对象的匿名可读由桶策略给（§6），平台不逐个对象改 ACL。

签名器只实现 header 模式。不实现预签名 URL、不实现 OSS STS 与临时凭证、不实现其他对象存储厂商的签名。

对拍向量只有一个锚：阿里云官方 [在 Authorization 头中包含 V4 签名（推荐）](https://www.alibabacloud.com/help/en/oss/developer-reference/recommend-to-use-signature-version-4) 的 `PutObject` 可复现示例。

| 项 | 值 |
| --- | --- |
| AK id | `LTAI****************` |
| SK | `yourAccessKeySecret` |
| 时间戳 / `x-oss-date` | `20250411T064124Z` |
| bucket / object / region | `examplebucket` / `exampleobject` / `cn-hangzhou` |
| canonical headers | `content-disposition:attachment`、`content-length:3`、`content-md5:ICy5YqxZB1uWSwcVLSNLcA==`、`content-type:text/plain`、`x-oss-content-sha256:UNSIGNED-PAYLOAD`、`x-oss-date:20250411T064124Z` |
| AdditionalHeaders | `content-disposition;content-length` |
| HashedPayload | `UNSIGNED-PAYLOAD` |
| 期望 canonical request 哈希 | `c46d96390bdbc2d739ac9363293ae9d710b14e48081fcb22cd8ad54b63136eca` |
| 期望 SigningKey | `3543b7686e65eda71e5e5ca19d548d78423c37e8ddba4dc9d83f90228b457c76` |
| 期望 Signature | `053edbf550ebd239b32a9cdfd93b0b2b3f2d223083aa61f75e9ac16856d61f23` |

单元测试把这一组输入交给签名器，逐步断言派生密钥、string-to-sign 与最终签名；不拿本仓库自己的第二次实现互算当锚。向量用的是 path-style 的 canonical URI（`/examplebucket/exampleobject`），它锚的是算法本身；本设计请求用虚拟主机形态，canonical URI 因此是 `/{key}`，两处的差异只在这一项输入。

一次上传的请求序列是：PUT 对象（带禁覆盖头与签名）→ HEAD 读取元数据核验字节长度与内容类型（与提交一致）→ 返回公网 URL。HEAD 核验不一致即失败关闭，不返回 URL（该对象成为孤儿，归对象存储控制台管理）。已确认写入成功的对象不重复 PUT；HEAD 失败只重试 HEAD。

## 6. 桶匿名可读与部署自检

桶必须匿名可读，这是启用上传的运维前置条件：平台不逐对象发 `x-oss-object-acl` 或任何 ACL 头，运行期返回的公网 URL 的可读性由桶策略决定。平台在启动或运行期都不探测桶，也不因桶不可读拒绝启动；部署侧在启用上传前自己执行下面的自检，用服务端生成的 1×1 合法 PNG 探针字节与独立对象键 `reference-media/{UTC 日期}/probe-{uuid}.png`：

1. PUT 探针对象，带禁覆盖头与签名；
2. HEAD 核验探针对象的字节长度与内容类型与写入一致；
3. GET 探针对象（header 模式签名，`Authorization` 头），返回字节与写入字节逐字节相同；
4. 不带凭证的客户端 GET 该对象的公网 URL，返回字节与写入字节逐字节相同。

四步全过才算这个桶可用于上传；任一步失败的处置是部署侧改配置（桶策略、region、bucket、密钥），不是运行期降级——上传路径没有健康状态，也不会因结论不健康而停用。自检由部署侧用平台自己的签名实现跑受控验证，或用对象存储控制台与命令行工具完成同样的四步。桶策略的写法见[配置项](../operations/configuration.md) §10。

探针对象不复用调用方素材的键，平台也不删除探针对象——平台没有删除能力，对象的保留与清理归对象存储控制台。

## 7. 重试与失败分类

| 失败事实 | 分类 | 处置 |
| --- | --- | --- |
| 传输网络错误、连接超时、`408` | 可重试 | 同一对象键重试，总尝试次数有上限 |
| 对象存储 `429` | 可重试 | 遵循有界整数秒 `Retry-After`，否则固定退避 |
| 对象存储 `5xx` | 可重试 | 同上 |
| `401` | 终态 | 凭证无效，`503 object_store_unavailable` |
| `403` | 终态 | 权限不足（缺 PUT/HEAD/GET 之一），`503 object_store_unavailable` |
| `404` | 终态 | bucket 或 region 不可访问，`503 object_store_unavailable` |
| `409` | 终态 | 禁止覆盖相撞，不换键重写，`503 object_store_unavailable` |
| HEAD 元数据与提交不一致 | 终态 | 失败关闭、不返回 URL，`503 object_store_unavailable` |
| 上传存储未配置，或密钥解析返回 `Configuration` | 终态 | `503 upload_storage_unavailable`，不是 `500` |
| 其他 `4xx` | 终态 | `503 object_store_unavailable` |

传输层不配置隐式重试、不跟随重定向，重试预算只由上面的编排层拥有，避免内外层相乘。退避基准与总尝试次数在实施时按本机上限配置取值，取值不改变上表语义。

失败只写稳定错误码、HTTP 状态与有界脱敏诊断；密钥、签名、对象字节不进日志、trace、告警与响应。

## 8. 与上一代上传模块的差异

上一代上传模块（同一产品先前一代的 uploads 与 admin_upload_channels）在本仓库之外，仓库内的读者无法复核它，因此本节只留会约束本设计的差异结论，不引用其路径。

- **删管理面与存储表**：上一代有 admin_upload_channels 管理面、按类型的存储行、激活与健康状态。本设计只有一组环境变量与一个客户端点，没有存储表、迁移、管理路由与页面，也没有激活动作。
- **删预签名读取**：上一代把 GET 预签名 URL 当作上传结果返回。本设计返回不带签名的公网 URL，不签发预签名 URL，签名器只做 header 模式。
- **不要求私有桶，改为要求匿名可读**：上一代在健康检查里断言"匿名 GET 必须被拒"以证明桶私有。本设计返回不带签名的公网 URL，部署自检的第 4 步要求匿名 GET 成功且字节逐字节一致，桶的公网可读是启用上传的前置条件；桶配错时上传仍返回 `200` 而 URL 读不到，平台不做活体探测，这条前置条件由部署侧自行证明。
- **删有效期与保留期机制**：上一代的临时凭证剩余有效期下限、预签名有效期常量与 `expires_at` 字段整体不存在；本设计没有 TTL、没有续期、没有平台侧保留期与删除。
- **只做阿里云 OSS**：上一代预置阿里云 OSS 与腾讯云 COS 两种存储类型。本设计只有阿里云 OSS 一种，删掉 COS 签名、token 契约与第二种存储类型的配置面。
- **单文件，无 manifest 与批次状态**：上一代一次最多 16 个文件，用 manifest 文本部件把文件部件与 `client_ref` 关联，逐项失败放在 `200` 的 `items` 里。本设计一个文件一个结果，失败即请求级错误。
- **图片字节不落临时文件**：上一代把超过 64 KiB 的部件 spool 到临时文件。本设计单文件上限 20 MiB，字节只在内存，与[同步图片网关 Spec](../specs/0005-synchronous-image-gateway.md) §2 的不落盘边界一致。
- **收窄媒体类型面**：上一代同时收图片、视频与音频。本设计只收图片。
- **鉴权与端点路径**：上一代是 `/api/v1/uploads/reference-media` 加 OAuth 会话；本设计是 `/v1/uploads/images` 加平台客户 API Key，与生成接口同一套凭证。
- **密钥来源**：上一代用按类型嵌套的环境变量键名；本设计用平铺的环境变量名（§3）。
- **禁覆盖的签名位置**：上一代早期用 query 参数形态，改用请求头形态。本设计直接按请求头形态参与签名。

## 9. 分片实施顺序与验证

1. 领域规则：媒体类型与扩展名白名单、单文件上限、魔数到规范 MIME、对象键构造、失败分类。验证：`crates/domain` 内单元测试，覆盖白名单边界、上限的严格小于语义、日期与扩展名。
2. `crates/adapter-object-storage`：签名器与传输。验证：对官方 V4 向量（§5）逐步断言派生密钥、string-to-sign 与签名，并断言 `host` 不进 canonical headers；用脚本化传输断言请求次数、重试次数与不重复 PUT；不发真实网络请求。
3. 应用用例：`ObjectStorage` 端口、`ImageUploadService` 与配置装载。验证：用假对象存储端口覆盖成功、各失败分类、重试耗尽与禁覆盖冲突；配置整组缺失时上传返回 `503 upload_storage_unavailable` 且进程照常启动，形状不合法在启动期拒绝并点名；断言上传不触碰资金与执行记录端口。`ObjectStorage` 的方法集是 put（带签名写入、禁覆盖）、head（元数据核验）、signed get（header 模式签名）、匿名 get 公网 URL（不带凭证、不跟随重定向、有界超时）。
4. API：客户侧上传路由。验证：`apps/api/tests/http_contract/` 的端到端用例加进程内假对象存储，覆盖 Spec 0007 的 A1–A9；断言无效凭证在正文前被拒、上传前后账户与账本不变。
5. 生成入口收敛：`interpret_current_inputs` 只构造公网 URL 形态、`data:` URL 与 multipart 文件部件被拒、拒绝发生在幂等预查之后；删除 APIMart 的内联上传通路、对应用例与 harness 的上传闸门（§2）。验证：端到端用例覆盖 Spec 0005 的 A12 与 A2，含以 `data:` URL 受理的历史记录在改版后仍按原记录回应。
6. 文档、配置与残留说法同步：[代码结构图](../architecture.md)、根 `CONTEXT.md`、`docs/operations/configuration.md` §10 与 `.env.example` 增补 §3 的环境变量；清掉两份 Spec 之外的旧说法（`README.md`、`docs/design/0002`、`docs/design/0004`、`docs/design/0017`、`docs/design/0018`、`docs/facts/channel-facts.md` 的平台侧叙述）与代码注释里的 data URL 说法（`crates/application/src/request_fingerprint.rs`、`crates/application/src/direct_execution.rs`、`apps/api/src/main.rs`、`apps/api/tests/http_contract/cases_aihubmix.rs`）。

第 1–4 片之间只有端口依赖，可按上表顺序串行；第 5 片与第 4 片改同一批入口文件，必须串行实施，不能并行编辑。

## 10. 验证边界

上传的自动化验证只打进程内的假对象存储，不访问真实 OSS：普通测试不得产生外部对象，也不得依赖真实桶。

真实桶只用于人工受控验证（桶的匿名可读配置、跨区域访问、大文件边界、签名联调），需要显式批准，并记录创建的对象键以便在对象存储控制台清理。桶的公网可读（桶策略）是这些验证的前置条件：验证时先按 §6 的四步读探针对象的公网 URL，读不到就不继续。真实桶不产生模型调用费用，但会产生对象存储的请求费用与对象残留，属外部副作用，按[受控验证清单](../verification/)的既有约定留档。

签名正确性不靠真实桶证明：单元测试对官方 V4 向量，端到端用例只证明请求构造与编排，真实桶验证只补齐签名与桶配置在真实服务端的联调结论。
