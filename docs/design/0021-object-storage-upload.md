主题: 对象存储上传：模块划分、签名与上传存储配置
当前修订: v2
状态: 待评审
承接: [图片上传与对象存储 Spec v2](../specs/0007-image-upload-and-object-storage.md) §2–§8；并与[同步图片网关 Spec v3](../specs/0005-synchronous-image-gateway.md) §1、§3 的输入图片形态收敛同一变更实施；生成入口的指纹输入域与验收见 Spec 0005 §4、§8（A12）。
依赖: [分层架构](0004-layered-architecture.md)、[图片上传与对象存储 Spec](../specs/0007-image-upload-and-object-storage.md)、[同步网关设计](0017-synchronous-image-gateway.md) §2、[图片透传决定](../adr/0019-images-pass-through-without-asset-storage.md)（结果侧不落盘与只有同步形态的结论继续适用）、[参考图上传决定](../adr/0022-reference-image-upload-endpoint.md)

# 对象存储上传：模块划分、签名与上传存储配置

本稿拥有上传能力的模块落点、配置项与启动期形状校验、对象键与签名机制、桶匿名可读的部署自检、重试与失败分类、分片实施顺序与验证边界。上传对调用方的行为合同由[图片上传与对象存储 Spec](../specs/0007-image-upload-and-object-storage.md)拥有；生成请求内的图片形态收敛由[同步图片网关 Spec](../specs/0005-synchronous-image-gateway.md)拥有。

## 1. 模块划分

对象存储是平台调用的**外部服务**，不是生成上游：它的适配实现与 ② Adapter Driver 并列，属于**外部服务适配器**一类（[分层架构](0004-layered-architecture.md) §1.1），不进 Runtime Revision，也不参与选路。

| 层 | 落点 | 拥有什么 |
| --- | --- | --- |
| 领域 | `crates/domain/src/upload_media.rs`（新） | 允许的媒体类型（按魔数判定）、规范 MIME 到对象键扩展名的映射、单文件字节上限、魔数到规范 MIME 的判定、对象键构造（前缀、UTC 日期、随机标识、扩展名）、失败分类枚举。纯类型与纯函数，不依赖 HTTP、对象存储、数据库与环境变量。 |
| 应用 | `crates/application/src/image_upload.rs`（新） | 上传用例 `ImageUploadService`：读配置、校验媒体与上限、构造对象键、写入、元数据核验、组装结果。配置的取值域与 region 到 endpoint 的派生是平台自己的规则，不落 domain；端口是 `ObjectStorage`；访问密钥经既有 `CredentialProvider` 只从环境变量解析；对客错误码在此收口。 |
| 外部服务适配器 | `crates/adapter-object-storage`（新 crate） | 阿里云 OSS V4 自实现签名（header 模式的 PUT 与 HEAD）、请求构造（host、canonical URI、签名头集合、禁覆盖头、`x-oss-date`、`x-oss-content-sha256`）、有界传输（单请求超时、无隐式重试、不跟随重定向、不读环境变量、不认数据库）、HTTP 状态到失败分类的映射。实现 application 的 `ObjectStorage` 端口。 |
| API | `apps/api/src/main.rs` | 客户侧 `POST /v1/uploads/images`：复用既有 API Key 认证（`authenticate`）与"认证及准入先于消费正文"的中间件写法；错误信封沿用既有 `{"error":{"code","message"}}`。 |

上传不进 `catalog.*` / `supply.*` / `publication.*`：它不是 Provider 的调用入口，没有 Offering、渠道与发布物。上传也不进 `generation.*` / `ledger.*`：上传没有执行、没有计量、没有资金占用，也不计费、不限配额。生成入口的请求与响应形状不因上传能力改变。

上传存储是部署侧的一组环境变量，平台只支持阿里云 OSS 一种：没有存储表与迁移，没有管理端路由与页面，也没有激活状态与健康结论。

上传的本机许可、字节预算与断开检测落在 [`apps/api/src/supervisor.rs`](../../apps/api/src/supervisor.rs)：在现有 Supervisor 上加**第二套上传读名额与独立字节预算**（`UPLOAD_SLOTS` / `UPLOAD_MAX_BUFFER_BYTES`），不挤占生成的 `GENERATION_READ_SLOTS` / `GENERATION_MAX_MEMORY_BYTES`；装配期做上传侧容量组合校验，`UPLOAD_SLOTS × 单次上传预留 ≤ UPLOAD_MAX_BUFFER_BYTES`，其中单次上传预留就是 `UPLOAD_MAX_REQUEST_BYTES`、不另立常数，配不出可用容量就拒绝启动。上传正文的慢读超时取独立的 `UPLOAD_SLOW_READ_TIMEOUT_SECONDS`，不复用生成的 `GENERATION_SLOW_READ_TIMEOUT_SECONDS`（对应 Spec 0007 A9 的受理前 `408`）。连接断开监视与停机排空是进程级机制（连接注册表、Supervisor），上传与生成共用，不新建模块。上传的读取许可或内存预算任一取不到都对客返回 `429 upload_busy`，不排队：上传是一次性、可稍后重试的独立操作，回 `429` 比生成侧容量不足的 `503 platform_unavailable`（[Spec 0005](../specs/0005-synchronous-image-gateway.md) §3）更准确，调用方也能按 `Retry-After` 稍后重试。

上传的请求级限流复用既有的每 API Key 计数机制（[`AccelerationService::consume_request_slot`](../../crates/application/src/lib.rs) 读写的缓存窗口计数），但用独立键前缀：生成是 `rate_limit:{key_id}:{window}`，上传是 `upload_rate_limit:{key_id}:{window}`，两边的窗口计数互不相干，上传不挤占生成的每 API Key 配额。计数仍在缓存、缓存不可用时放行，语义与生成一致；上传的窗口与次数上限由 `UPLOAD_RATE_LIMIT_REQUESTS_PER_WINDOW` 与 `UPLOAD_RATE_LIMIT_WINDOW_MS` 给出（§3）。

## 2. 生成入口的输入形态收敛

上传端点的存在理由是把本地文件变成公网 URL，因此生成入口同时收敛为只收公网 URL，两件事在同一变更里实施：生成接口收 `http(s)` 公网 URL，取值不是公网 URL 的（`data:` URL、multipart 文件部件与任何其他非法文本）一律拒绝，返回 `400 public_image_url_required`。

拒绝点在 [`apps/api/src/main.rs`](../../apps/api/src/main.rs) 的 `interpret_current_inputs`，即幂等预查（`lookup_recorded`）未命中之后、按当前合同解释请求的那一步。命中同一幂等记录时走的是 `replay_recorded`，它按记录冻结的合同与指纹算法比对原始请求面；把形态判定提到预查之前会让以 `data:` URL 或文件部件受理的历史记录无法比对，破坏同键去重合同（[Spec 0005](../specs/0005-synchronous-image-gateway.md) §4）。

`InputImage` 三态定义在 [`crates/adapter-sdk/src/gateway.rs`](../../crates/adapter-sdk/src/gateway.rs)，收敛后按事实收缩：JSON 文本图片的记录面存的是**原始字符串**（`recorded_request_face` 直接用参数值），只有 multipart 文件部件经 `to_data_url()` 编码成 data URL 参与比对，因此 `Bytes` 为 multipart 重放比对保留；`interpret_current_inputs` 只构造 `Url`，其余一律映射为 `400 public_image_url_required`，`DataUrl` 变体与 `from_raw` 在收敛后删除（`from_raw` 的唯一生产调用方就是 `interpret_current_inputs`）。一并删掉 adapter 侧按 `DataUrl` 取值的分支与对应用例：`InputImage::decoded` 与 `to_data_url` 的 `DataUrl` 臂、[`crates/adapter-aihubmix/src/lib.rs`](../../crates/adapter-aihubmix/src/lib.rs) 里 `DataUrl | Bytes` 合用的取字节分支（`Bytes` 一路保留）、APIMart 的内联上传分支（§2 末段）。这些取值在测试夹具里仍被引用（§9 第 5 片点名的四个文件），夹具改用存活的 `Url` 与 `Bytes` 形态构造：`DataUrl` 的处理分支删掉后，留着 `DataUrl` 夹具也没有行为可测。`crates/domain/src/image_parameters.rs` 只负责"契约字段名与空值"的规则，形态判定仍在入口。

APIMart 的内联上传通路随之删除：`crates/adapter-apimart/src/lib.rs` 里把 data URL / 字节上传到上游 `POST /v1/uploads/images` 取 URL 的那条路（含 `MAX_UPLOAD_BYTES` / `MAX_TOTAL_UPLOAD_BYTES`、`upload_image*`、`validate_uploaded_url`、`upload_filename` 等常量与函数，以及相关注释）、对应用例与 `apps/api/tests/http_contract/harness.rs` 的上传闸门一起删掉——生成入口只收公网 URL 之后，APIMart 收公网 URL 逐字透传，内联上传没有入口再用，上游上传失败这种可证明未受理的失败（`SafeBeforeAcceptance`，[ADR 0011](../adr/0011-safe-before-acceptance-does-not-retry-yet.md)）也不再是它的起因。

强杀矩阵里以这条通路为注入点的「提交前（已写提交声明、生成请求未发）」格随之退役：通路删掉后该状态在进程外没有可钉的屏障——提交声明之后的下一个外部动作就是生成请求本身，而进程内注入不构成强杀。该格要证的恢复结论由「提交中／接受后句柄未写入」格承担：`submitting` 且没有句柄时保留 Hold 与渠道槽位、进对账、不重提。

AIHubMix 的取图路径形态不变：它的 edits 端点要文件部件，所以由 Adapter 自己从公网 URL 取字节，这是它唯一的参考图来源。

## 3. 配置项与环境变量

| 类别 | 名称 | 说明 |
| --- | --- | --- |
| 环境变量（仅部署侧） | `UPLOAD_STORAGE_REGION` | 必填；形如 `cn-hangzhou`，取值是小写字母、数字与连字符，首尾必须是字母或数字，长度不超过 63 |
| 环境变量（仅部署侧） | `UPLOAD_STORAGE_BUCKET` | 必填；3–63 位小写字母、数字与连字符，首尾必须是字母或数字，**不含点号** |
| 环境变量（仅部署侧） | `UPLOAD_STORAGE_ENDPOINT` | 可选；省略时按 region 派生为 `https://oss-{region}.aliyuncs.com`。显式给出时是**含 scheme 的 origin**（`https://主机[:端口]`；loopback 的 `http` 见 §4），scheme 是 `https` 或 loopback 的 `http`，不带凭证（userinfo），不带 path、query 与 fragment |
| 环境变量（仅部署侧） | `UPLOAD_STORAGE_ACCESS_KEY_ID` | 访问密钥标识；与下一条一起构成完整凭据 |
| 环境变量（仅部署侧） | `UPLOAD_STORAGE_ACCESS_KEY_SECRET` | 访问密钥 |
| 环境变量（本机上限） | `UPLOAD_MAX_REQUEST_BYTES` | 请求体上限，即单文件上限加 multipart 协议余量；余量多少在实施时取值 |
| 环境变量（本机上限） | `UPLOAD_SLOTS` | 本机上传并发许可数，取值在实施时按本机容量定 |
| 环境变量（本机上限） | `UPLOAD_MAX_BUFFER_BYTES` | 本机上传内存预算，取值在实施时定 |
| 环境变量（本机上限） | `UPLOAD_REQUEST_TIMEOUT_SECONDS` | 单次写入的对象存储请求超时，取值在实施时定 |
| 环境变量（本机上限） | `UPLOAD_SLOW_READ_TIMEOUT_SECONDS` | 上传正文从开始接收到读完的上限，超时按受理前 `408 request_timeout` 终止；取值在实施时定 |
| 环境变量（上传限流） | `UPLOAD_RATE_LIMIT_REQUESTS_PER_WINDOW` | 每 API Key 每窗口允许的上传请求数；默认值在实施时定 |
| 环境变量（上传限流） | `UPLOAD_RATE_LIMIT_WINDOW_MS` | 上传限流窗口的毫秒数；默认值在实施时定 |
| 环境变量（上传重试） | `UPLOAD_RETRY_MAX_ATTEMPTS` | 单次上传写入的总尝试次数上限；取值在实施时定 |
| 环境变量（上传重试） | `UPLOAD_RETRY_BACKOFF_BASE_SECONDS` | 固定退避基准秒数，对象存储未给出 `Retry-After` 时按它等待；取值在实施时定 |

桶名不含点号：`{bucket}.{host}` 的虚拟主机形态下，含点的桶名会撞 `*.oss-{region}.aliyuncs.com` 那种只覆盖一级标签的通配符证书，也会让主机名与桶名在点号上分不开。

单文件上限是领域常量（20 MiB，严格小于 20971520 字节），落在 `crates/domain/src/upload_media.rs`，不是环境变量。重试的总尝试次数与退避基准同样由服务端配置、在实施时取值（§7），变量见上表。

## 4. 配置装载与启动期校验

配置装载只判形状，不做活体探测：region 与 bucket 的形状、显式 endpoint 的 scheme / 主机名 / 端口与禁止的凭证、path、query、fragment，以及访问密钥两条要么都有要么都不给。显式 endpoint 只放行 `https`，另外放行 loopback 的 `http`（主机名是 `127.0.0.1`、`::1` 或 `localhost`），其余主机名的 `http` 按形状不合法拒绝。全部上传存储变量都不存在＝未配置：进程照常启动，上传端点对该请求返回 `503 upload_storage_unavailable`。给了其中一部分（缺 region、bucket 或任一条密钥）与取值形状不合法同样按形状不合法处理：装载返回类型化配置或 `Configuration` 错误，由 [`apps/api/src/main.rs`](../../apps/api/src/main.rs) 在装配期调用并据此拒绝启动、点名变量，进程不启动。对象存储可达性、bucket 是否存在与桶是否匿名可读都不在启动判据里。

访问密钥**按请求**解析：`UPLOAD_STORAGE_ACCESS_KEY_ID` 与 `UPLOAD_STORAGE_ACCESS_KEY_SECRET` 是两处固定引用，每次上传各经既有 `CredentialProvider::resolve` 取一次（`ProviderCredential` 仍只装一个值，不为上传新增端口），密钥只作调用参数传给 `ObjectStorage` 端口，不存进 `ImageUploadService` 或配置对象、不进日志。启动期只查两条要么都给、要么都不给与形状，不留存取值：只给一条按形状不合法拒绝；两条都不给时密钥视为未配置，整组是否算未配置按上一条判定。密钥解析返回 `Configuration` 类错误时对客映射为 `503 upload_storage_unavailable`，不是 `500 internal_error`：启动期的成对与形状校验使这条兜底在部署配置下不可达，由应用层用例用一个返回 `Configuration` 的凭证解析替身覆盖它对客映射。

`UPLOAD_STORAGE_ENDPOINT` 由部署侧按自己的对象存储填写，也是端到端用例把请求指向进程内假对象存储的注入缝：起真实 API 子进程的装置只能经环境变量注入 `http://127.0.0.1:{port}`，因此装载器放行 loopback 的 `http`（仅 `127.0.0.1`、`::1`、`localhost`），其余仍必须 `https`。适配器不重复判 scheme，按配置里的地址直接发请求；端到端夹具构造的 loopback `http` 配置值不被适配器层挡回。`endpoint` 只是地址，不是凭证——访问密钥仍从环境变量读。寻址形态随之分开：真实 OSS 用虚拟主机式 `https://{bucket}.{endpoint-host}/{object-key}`，loopback 端点走 path-style `http://{endpoint-host}/{bucket}/{object-key}`（[Spec 0007](../specs/0007-image-upload-and-object-storage.md) §6）。

## 5. 对象键与签名

对象键是 `reference-media/{YYYY-MM-DD}/{uuid}.{ext}`：日期取写入时的 UTC 日期，`uuid` 是随机 v4，`ext` 由规范 MIME 反推（`image/jpeg` → `jpg`，`image/png` → `png`，`image/webp` → `webp`）。调用方文件名不进键、不进对象元数据。

签名用阿里云 OSS V4，header 模式：

- 派生密钥链是 `HMAC("aliyun_v4" + SK, date) → region → "oss" → "aliyun_v4_request"`；
- canonical request 是 `Verb`、canonical URI（**恒为 `/{bucket}/{key}`，与寻址形态无关**：bucket 来自这次操作的对象，不从 `Host` 或请求 path 推导）、排序后的 canonical query、canonical headers、附加签名头列表与 `UNSIGNED-PAYLOAD`；
- 默认签名头集合是所有 `x-oss-*`、`content-type` 与 `content-md5`，其余显式头进附加签名头；canonical headers 的键小写、按字典序排列；`percent_encode` 与官方 Python SDK 的 `quote(s, safe)` 一致（`/` 与未保留字符不编码，其余按大写十六进制编码）；`host` 不进 canonical headers——它不是 `x-oss-*`，也不在显式头里，只作为 HTTP 请求的 `Host` 发出；
- `x-oss-date` 每次请求重新取当前 UTC 时刻，不缓存、不复用时间戳；
- 禁止覆盖用请求头 `x-oss-forbid-overwrite: true` 参与签名，不用 query 参数形态；
- 不发 `x-oss-object-acl`，也不发其他 ACL 头：平台不逐个对象改 ACL（[Spec 0007](../specs/0007-image-upload-and-object-storage.md) §8）。

签名器只有一套 header 模式构造，对拍覆盖 PUT、HEAD 与 GET header 模式三组输入，预签名 GET 单独一组；**生产只发 PUT 与 HEAD 两个请求，不对外签发预签名 URL**：预签名 GET 的 query 模式签名构造只存在于适配器的测试模块，用来校核参数编码与签名串，不是生产能力，也不作为返回 URL 的形态（§8）；四组对拍都在适配器的单元测试里执行。不实现 OSS STS 与临时凭证、不实现其他对象存储厂商的签名。

签名机制与对拍向量以**参考实现**为准，向量值只有一处家：金标准的固定输入与全部期望签名落档在 [`out-reference/oss-v4-signing-golden.md`](../../out-reference/oss-v4-signing-golden.md)（来源：参考实现 `/mnt/d/workspace/seeaihub` 的 `src/service/tests/uploads_signing_golden.rs`、`src/service/src/uploads/signing/oss_v4.rs` 与 `oss_v4_test.rs`；该 golden 自述是那份实现的验收基准），本节只声明哪一组锁什么，不复制向量值；本设计自有的生产 PUT 期望值是唯一例外，留在本节并注明来源与复验要求。

各组对应各自的输入，不是谁替代谁：落档 §2 的 PUT（canonical query `x-oss-forbid-overwrite=true`）与 HEAD（无 query、无显式头）两行锁**算法与派生链**（canonical request 拼装、HMAC 派生、string-to-sign）；落档 §2 的 GET header 模式一行锁**无 query 的 header 模式构造**；落档 §3 的预签名 GET 锁 **query 模式的参数编码与签名串**；落档 §4 的附加签名头一组锁**「其余显式头进附加签名头」这条规则**——落档 §2 与 §3 那几组输入都没有非默认头，只有这一组覆盖得到它。

**本设计要发的 PUT 把同一条禁覆盖放请求头（上文），canonical query 为空、`x-oss-forbid-overwrite: true` 进 canonical headers，期望签名是 `c00baf659ad74992fd003d49e754bb137dfd35f235251529a398517dbce5e093`**：这是设计侧拥有的事实，按本设计规则复算、不是参考实现的输出，落档 §2 末行是同值的复算记录并已注明该来源；实施时要用移植用例对这个值逐字节复验后再钉住。金标准那几组证明算法与参考实现对齐，这一组证明我们真正发出去的请求签得对，缺一不可。

单元测试把落档的固定输入交给签名器，逐字节断言 `Authorization` 与预签名 URL，并逐步断言派生密钥与 string-to-sign。不拿阿里云官方文档页**公布**的 SigningKey `3543b768…` 与 Signature `053edbf5…` 当锚：页面上的 SK 是占位符 `yourAccessKeySecret`，公布的派生值用页面自带的输入复算不出来（应来自撰写示例时的真密钥）；附加签名头那组向量的期望值取的是这批输入在参考实现里的复算结果，不是页面公布的数。canonical URI 恒为 `/{bucket}/{key}`：参考实现把它写死，阿里云 Python v2 与 Go v2 的签名器同样从操作入参取 bucket 拼这一项，不从 `Host` 或请求 path 推导，因此虚拟主机与 path-style 两种形态在这一项上相同。

一次上传的请求序列是：PUT 对象（带禁覆盖头与签名）→ HEAD 读取元数据核验字节长度与内容类型（与提交一致）→ 返回公网 URL。HEAD 核验不一致即失败关闭，不返回 URL（该对象成为孤儿，归对象存储控制台管理）。已确认写入成功的对象不重复 PUT；HEAD 失败只重试 HEAD。

## 6. 桶匿名可读的部署自检

桶的匿名可读是启用上传的运维前置条件，合同与后果见[Spec 0007](../specs/0007-image-upload-and-object-storage.md) §8。部署侧在启用上传前用对象存储控制台或命令行工具自己执行下面的自检（工具自己完成签名），用一个 1×1 合法 PNG 探针字节与独立对象键 `reference-media/{UTC 日期}/probe-{uuid}.png`：

1. 写入探针对象（对象键随机唯一，不复用既有键）；
2. 核验探针对象的字节长度与内容类型与写入一致；
3. 读回探针对象，返回字节与写入字节逐字节相同；
4. 不带凭证的客户端读该对象的公网 URL，返回字节与写入字节逐字节相同。

四步全过才算这个桶可用于上传；任一步失败的处置是部署侧改配置（桶策略、region、bucket、密钥），不是运行期降级——上传路径没有健康状态，也不会因结论不健康而停用。`ObjectStorage` 端口只有 PUT 与 HEAD 两个操作：上传路径不发 GET 请求，也不签发预签名 URL，上面的读回与匿名读在平台侧没有入口，由执行自检的控制台或命令行工具完成。平台自己的签名对真实服务端的结论由受控探针上传给出——向真实配置的 `POST /v1/uploads/images` 发一个 1×1 PNG，PUT 与 HEAD 都由平台的签名实现发出（受控验证，需显式批准并限制调用次数，见 §10；步骤与命令见[上传存储部署自检清单](../verification/object-storage-upload.md) §4）。自检失败不产生任何状态变更：不写健康状态、不改配置、不建执行记录、不动账务，也不删除探针对象。桶策略的写法见[配置项](../operations/configuration.md) §10。

探针对象不复用调用方素材的键，平台也不删除探针对象——平台没有删除能力，对象的保留与清理归对象存储控制台。

## 7. 重试与失败分类

| 失败事实 | 分类 | 处置 |
| --- | --- | --- |
| 传输网络错误、连接超时、`408` | 可重试 | 同一对象键重试，总尝试次数有上限 |
| 对象存储 `429` | 可重试 | 遵循有界整数秒 `Retry-After`，否则固定退避 |
| 对象存储 `5xx` | 可重试 | 同上 |
| `401` | 终态 | 凭证无效，`503 object_store_unavailable` |
| `403` | 终态 | 权限不足（缺 PUT 或 HEAD），`503 object_store_unavailable` |
| `404` | 终态 | bucket 或 region 不可访问，`503 object_store_unavailable` |
| `409` | 终态 | 禁止覆盖相撞，不换键重写，`503 object_store_unavailable` |
| HEAD 元数据与提交不一致 | 终态 | 失败关闭、不返回 URL，`503 object_store_unavailable` |
| 上传存储未配置，或密钥解析返回 `Configuration` | 终态 | `503 upload_storage_unavailable`，不是 `500` |
| 其他 `4xx` | 终态 | `503 object_store_unavailable` |
| 客户端断开（上传进行中） | 服务端观测事实 | 不对客返回错误码；PUT 已在途则该对象可能已落盘成孤儿，不删除、不返回 URL、不动账务 |

断开信号来自进程级连接注册表与断开监视（§1）：监视观察到对端离开即置该请求的取消，重试编排随之中止——不再取退避、不再发下一次 PUT；已在途的 PUT 不被回滚，其对象按孤儿处置。断开不改变已完成写入或元数据核验的结论，也不产生对客错误码（[Spec 0007](../specs/0007-image-upload-and-object-storage.md) §5）。

传输层不配置隐式重试、不跟随重定向，重试预算只由上面的编排层拥有，避免内外层相乘。退避基准与总尝试次数由 `UPLOAD_RETRY_BACKOFF_BASE_SECONDS` 与 `UPLOAD_RETRY_MAX_ATTEMPTS` 配置，在实施时按本机上限取值，取值不改变上表语义。

失败只写稳定错误码、HTTP 状态与有界脱敏诊断；密钥、签名、对象字节不进日志、trace、告警与响应。

## 8. 设计约束

上传字节只在内存、不落临时文件：单文件上限 20 MiB，正文不 spool 到磁盘，与[同步图片网关 Spec](../specs/0005-synchronous-image-gateway.md) §2 的不落盘边界一致。

其余形态约束由拥有它们的那一节写，不在这里重复：没有管理面与存储表（§1）、桶匿名可读（[Spec 0007](../specs/0007-image-upload-and-object-storage.md) §8，部署自检见 §6）、单文件无批次状态与只收图片（Spec 0007 §1–§3）、端点与凭证（Spec 0007 §6、§7）、禁覆盖按请求头形态参与签名（§5）。

**与参考实现相反的一条**：参考实现按私有桶 + 86400 秒预签名 GET 取图，健康判据里含「匿名 GET 必须被拒」；本设计的桶与返回 URL 形态以 [Spec 0007](../specs/0007-image-upload-and-object-storage.md) §1、§8 为准，参考实现的 GET 预签名金标准向量在这里只用来校核签名器（§5），不是对外返回 URL 的形态。

与早期方案比较的取舍理由与后果见[本次变更记录](../../.agents/notes/proposed/platform/2026-10-04-reference-image-upload.md) 的「备选方案」与「后果与验证」。

## 9. 分片实施顺序与验证

1. 领域规则：媒体类型（按魔数判定）、规范 MIME 到对象键扩展名的映射、单文件上限、魔数到规范 MIME、对象键构造、失败分类。验证：`crates/domain` 内单元测试，覆盖媒体类型边界、上限的严格小于语义、日期与扩展名。
2. 端口与对象存储适配器：在 `crates/application/src/image_upload.rs` 定义 `ObjectStorage` 端口与它用到的 DTO（PUT 与 HEAD 的请求与结果；端口不提供读取操作，§6），使 `crates/adapter-object-storage`（新 crate）能独立编译：签名器（header 模式）、传输、host 与 canonical URI 的拼装、失败分类映射。签名器只实现生产要发的 header 模式；预签名 GET 的 query 模式签名构造只放在该 crate 的测试模块，不是生产能力。验证：**移植参考实现的金标准向量对拍**（§5 指定的 PUT / HEAD / GET header 模式 / 预签名 GET 四组向量，固定输入与期望值见[落档](../../out-reference/oss-v4-signing-golden.md)，逐字节断言 `Authorization` 与预签名 URL，并逐步断言派生密钥与 string-to-sign），断言 canonical URI 是 `/{bucket}/{key}`、`host` 不进 canonical headers、canonical headers 小写字典序、禁覆盖按请求头形态进 canonical headers；用脚本化传输断言请求次数、重试次数与不重复 PUT；不发真实网络请求。
3. 应用用例：`ImageUploadService`（校验媒体与上限、构造对象键、写入、元数据核验、组装结果）与配置装载。验证：用假对象存储端口覆盖成功、各失败分类、重试耗尽与禁覆盖冲突；配置整组缺失时上传返回 `503 upload_storage_unavailable`；断言上传不触碰资金与执行记录端口；断言密钥按请求解析、不落进服务对象或日志。进程是否拒绝启动不在本片断言。
4. API 与进程装配：客户侧上传路由自带正文上限（路由级 `DefaultBodyLimit::max(UPLOAD_MAX_REQUEST_BYTES)`，不受合并后 app 的全局 16 MiB 正文上限约束），其超限拒绝映射成平台错误信封 `413 request_too_large`；[`apps/api/src/main.rs`](../../apps/api/src/main.rs) 装配期调用 `ImageUploadService` 的配置装载，形状不合法即拒绝启动；[`apps/api/src/supervisor.rs`](../../apps/api/src/supervisor.rs) 加第二套上传读名额与独立字节预算，含 `UPLOAD_SLOTS × UPLOAD_MAX_REQUEST_BYTES ≤ UPLOAD_MAX_BUFFER_BYTES` 组合校验。验证：`apps/api/tests/http_contract/` 的端到端用例加进程内假对象存储，覆盖 Spec 0007 的 A1–A11（A3 拆为 A3a 与 A3b），并必须真发一个整体合法、请求体超过全局 16 MiB 的 multipart 请求（单文件仍在 20 MiB 上限内）证明 A3b——路由级 `DefaultBodyLimit` 覆盖了全局 16 MiB 正文上限，不能只靠声明 `Content-Length`；进程级启动拒绝用例对齐 `harness.rs` 的 `probe_api_startup_*` 形态；断言无效凭证在正文前被拒、上传前后账户与账本不变。
5. 生成入口收敛与用例同步：`interpret_current_inputs` 只构造公网 URL 形态，`data:` URL、multipart 文件部件与其余非法取值被拒，拒绝发生在幂等预查之后；删除 `InputImage::DataUrl` 与 `from_raw`、adapter 侧的 `DataUrl` 分支、APIMart 的内联上传通路、对应用例与 harness 的上传闸门（§2）。测试面跟着收：仓库里 `data:image` 约 82 处、横跨 22 个文件，至少点名 `apps/api/tests/http_contract/cases_identity.rs`（新增 `/v1` 路由会被它打红，需补探针体）、`cases_aihubmix.rs`、`cases_apimart.rs`、`cases_direct_execution.rs`（含峰值 RSS 用例与 `one_execution_reservation_bytes()`，那组预算常数按 data URL 实测钉的，需重定）、`cases_parameters.rs`、`cases_parameter_mapping.rs`、`cases_retry.rs`、`cases_routing.rs`、`cases_kill_matrix.rs`，以及 `crates/domain/src/image_parameters/tests.rs`、`crates/application/src/tests/request_preparation.rs`，和直接引用被删变体的四个文件：`crates/application/src/direct_execution/tests.rs`（构造 `InputImage::DataUrl` 的夹具）、`crates/adapter-sdk/src/gateway/tests.rs`（约 5 处 data URL 取值与 `InputImage::DataUrl` 断言）、`crates/adapter-aihubmix/src/tests.rs`（约 18 处 data URL 取值）与 `apps/api/src/tests.rs`（`max_wire_request_body()` 按 data URL 贴入口上限构造，另有两处 `InputImage::from_raw`）。这四个文件直接引用被删的变体，处理边界见 §2。验证：端到端用例覆盖 Spec 0005 的 A12 与 A2，含以 `data:` URL 受理的历史记录在改版后仍按原记录回应。
6. 文档、配置与残留说法同步（这些「当前状态」文档在实现落地前继续描述代码事实，实施时才改）：[代码结构图](../architecture.md) 按该文件开头的维护要求在同一变更里全文同步，与本变更冲突的旧说法至少覆盖：§1 的「基础设施里**还没有**对象存储的 crate 或表：生成结果按渠道原形进原形出、不落盘」；§3 的 edits 行（「`image`/`mask` 是文件部件，文本部件也认、值按 URL/data URL 读」）、生成入口形态（「参考图与遮罩用**公网 URL 或 `data:image/…;base64,…`** 给出」）与「请求体上限 16MB（`DefaultBodyLimit`）」；§4 流程图里的「data URL 就地解码」；§6 的 `crates/adapter-sdk/src/gateway.rs` 行（`InputImage`/`GatewayInput` 的形态）、`crates/adapter-aihubmix/src/lib.rs` 行（「data URL 就地解码，公网 URL 由它自己取」）与 `crates/adapter-apimart/src/lib.rs` 行（「data URL 就地解码后上传换 URL 再回填」）；§7 的「**参考图的形态差异**（公网 URL / data URL、上游要 URL 还是要字节）」。同一次同步里补上传端点的路由表行、新文件（`crates/domain/src/upload_media.rs`、`crates/application/src/image_upload.rs`、`crates/adapter-object-storage`）与 `apps/api/src/supervisor.rs` 第二套名额的落点；根 `CONTEXT.md` 增补上传存储与对象键的术语；`docs/operations/configuration.md` §10 与 `.env.example` 增补 §3 的变量与取值形态，其中 `UPLOAD_SLOW_READ_TIMEOUT_SECONDS`、`UPLOAD_RATE_LIMIT_REQUESTS_PER_WINDOW`、`UPLOAD_RATE_LIMIT_WINDOW_MS`、`UPLOAD_RETRY_MAX_ATTEMPTS`、`UPLOAD_RETRY_BACKOFF_BASE_SECONDS` 至少点名前五条（环境变量的属主是配置文档，§3 是设计侧拥有取值域的完整清单，待同步的上传变量共 10 条）；`docs/operations/production.md` §2.5 的反向代理正文上限同步为盖住上传路由的 `UPLOAD_MAX_REQUEST_BYTES`，不能只按生成入口的 16 MiB 写（Spec 0007 §8）；新增 `docs/verification/object-storage-upload.md` 受控验证清单（真实桶四步自检、平台签名的受控探针上传、canonical URI 必查项与对象清理，运行方式见该清单）；清掉两份 Spec 之外的旧说法（`README.md`、`docs/design/0002`、`docs/design/0004`、`docs/design/0017`、`docs/design/0018`、`docs/facts/channel-facts.md` 的平台侧叙述）与代码注释里收敛前的 data URL 说法（`crates/application/src/request_fingerprint.rs`、`crates/application/src/direct_execution.rs`、`apps/api/src/main.rs`）。验证：本片新增与改写的相对链接逐条解析、章节引用与被引文件的实际章节号一致；上面点到名的「当前状态」文档里不再有把 `data:` URL 或 multipart 文件部件当生成入口合法取值的现行叙述，也没有与本变更冲突的对象存储「不存在这一层」的说法；`docs/operations/configuration.md` §10 与 `.env.example` 的变量集合与 §3 表一致，五条要点名的变量都在。

第 1→2→3→4 片按端口依赖串行（第 2 片定义端口与 DTO，第 3 片消费它）；第 5 片与第 4 片改同一批入口文件，必须串行实施，不能并行编辑；第 5 片与第 6 片同样改同一批文件（用例与文档），也必须串行。

## 10. 验证边界

上传的自动化验证只打进程内的假对象存储，不访问真实 OSS：普通测试不得产生外部对象，也不得依赖真实桶。

真实桶只用于人工受控验证（桶的匿名可读配置、跨区域访问、大文件边界、签名联调），需要显式批准并限制调用次数，并记录创建的对象键以便在对象存储控制台清理。桶的公网可读（桶策略）是这些验证的前置条件：验证时先按 §6 的四步读探针对象的公网 URL，读不到就不继续。真实桶不产生模型调用费用，但会产生对象存储的请求费用与对象残留，属外部副作用，按[受控验证清单](../verification/)的既有约定留档。

签名正确性不靠真实桶证明：单元测试对[落档](../../out-reference/oss-v4-signing-golden.md)的金标准向量，端到端用例只证明请求构造与编排，真实桶验证只补齐签名与桶配置在真实服务端的联调结论。真实桶联调必须把 canonical URI 当必查项：请求用虚拟主机形态发出，签名的 canonical URI 仍必须拼成 `/{bucket}/{key}`——金标准向量在两种寻址形态下是同一个值，测不出漏拼 bucket 的写法，只有真实服务端会拒。
