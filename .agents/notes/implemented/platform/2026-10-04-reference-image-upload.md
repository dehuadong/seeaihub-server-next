---
title: 参考图收敛为公网 URL 并新增上传端点
status: implemented
created: 2026-10-04
updated: 2026-10-09
approval: 用户 2026-10-04 逐条裁决：生成接口只收公网 URL（`data:` URL 与文件部件一律拒绝）、新增单文件上传端点、上传不计费不计量不限配额；同日追加裁决：上传存储不做后台管理与数据库表、配置只走环境变量、对客上传端点只使用阿里云 OSS；两批裁决均要求记录；用户 2026-10-04 接受图片上传与对象存储 Spec 的 v2 修订
verification: `cargo test -p seeai-domain`（上传规则）、`cargo test -p seeai-application --lib image_upload`（上传用例与配置装载）、`cargo test -p seeai-adapter-object-storage`（金标准签名向量逐字节）、`cargo test -p seeai-api --bin seeai-api`；真库端到端 `cargo test -p seeai-api --test http_contract cases_upload`（上传端点 A1–A11）与 `cases_direct_execution`（生成入口 A12，含历史记录重放）；真实桶按 `docs/verification/object-storage-upload.md` 受控执行
---

# Agent Note：参考图收敛为公网 URL 并新增上传端点

## 问题

[图片透传记录](2026-09-20-images-pass-through-without-asset-storage.md)把参考图只当参数值：`image` / `image_urls` / `mask` 收公网 URL，也收 `data:image/…;base64,…`，edits 端点另有文件部件。素材搬运因此留在生成路径上：`data:` URL 的字节随请求体进内存再解一份 base64，需要字节的渠道由 Adapter 自行取图或上传；请求体要装下整张图，素材的形态判定混在受理与幂等之间。[同步图片网关 Spec v3](../../../../docs/contracts/0005-synchronous-image-gateway.md) 已把不落盘、不代传定在生成请求内，但没回答"调用方手上只有本地文件"这条常见路径。

## 决定

生成接口的参考图与遮罩只收公网 `http(s)` URL；不是公网 URL 的取值（含 `data:` URL、multipart 文件部件与任何其他文本）在幂等预查未命中之后拒绝，返回 `400 public_image_url_required`，不建记录、不取占用、不调上游；命中同一幂等记录的旧请求按记录冻结的规则回应。长期产品行为归[图片上传合同](../../../../docs/contracts/0007-image-upload-and-object-storage.md)与[同步图片网关合同](../../../../docs/contracts/0005-synchronous-image-gateway.md) §1、§3；本记录承接上传的长期技术设计、决定、备选与后果。[历史 ADR-0022](../../../../docs/adr/0022-reference-image-upload-endpoint.md)保留来源身份，归属切换按登记的评审结果生效。

**为什么把图片输入收敛为公网 URL**：`data:` URL 与文件部件让每次生成都替调用方搬一次字节，请求体上限、base64 解码、临时缓冲与素材形态判定因此长在受理路径上；只收 URL 之后，生成请求的大小与素材体积解耦，受理只需判"是不是公网 URL"，形态判定也能放在幂等预查之后（[设计 0021](2026-10-04-reference-image-upload.md) §2）。

**为什么上传端点必须存在**：只收 URL 而不给换 URL 的路，等于把调用方推给外部图床；平台自己给一条显式端点 `POST /v1/uploads/images`，才能保证提交进来的 URL 是平台写进自己桶里的地址。它是独立调用，不绑定 Job、不计费、不计量、不限配额，也不进 Runtime Revision。

**为什么不做后台管理与数据库表**：上传存储只有阿里云 OSS 一种，region / bucket / endpoint 是一次部署定一次的静态取值，密钥本来就只从环境变量读；把它们收进数据库并配一套管理端点，等于为一个不变的选择造出持久结构、激活状态机与第二份事实。配置因此全部走环境变量，进程启动只校验形状、不做活体探测；整组不给时上传端点对该请求回 `503 upload_storage_unavailable`，只给一部分或形状不合法则在启动期拒绝并点名。

**为什么不落盘生成请求**：生成请求里的图仍是参数值，上传端点的素材字节与执行记录无关；这条边界由两个 Spec 各守一半——生成请求内的载荷边界归 Spec 0005 §2，上传素材的落点与保留边界归 Spec 0007。结果侧仍按图片透传决定原形交回，不归档。

## 技术设计

### 模块与数据边界

领域模块 `upload_media` 拥有媒体魔数、规范 MIME、扩展名、单文件严格小于 20 MiB 的上限、对象键规则与失败分类，不依赖 HTTP、环境或存储。应用模块 `image_upload` 拥有 `ImageUploadService`、部署配置取值域、`ObjectStorage` 端口、PUT/HEAD 编排和对客错误收口；对象存储适配器实现 OSS 协议、签名、有界传输与失败映射，不读环境、不认数据库。API 拥有上传路由、API Key 认证、正文解析与进程许可。完整设计来自[历史设计 0021](../../../../docs/design/0021-object-storage-upload.md)，归属接受结果见[切换登记](../../../../docs/agents/document-ownership-transition.md)。

上传没有目录、供给、发布、Job、账本或健康状态表。OSS 是外部服务适配器，与生成渠道适配器并列；PUT/HEAD 端口没有 GET 或删除能力。素材字节只在内存，对象键为 `reference-media/{调用者账户 id}/{随机 v4 UUID}.{规范扩展名}`，不使用调用方文件名或写入日期；成功 URL 不使用预签名参数。

### 入口、许可与取消

API 在消费正文前认证并取得上传读许可与字节预算。Supervisor 为上传持有独立许可和预算，启动验证 `UPLOAD_SLOTS × UPLOAD_MAX_REQUEST_BYTES ≤ UPLOAD_MAX_BUFFER_BYTES`；上传慢读期限独立于生成。许可或预算不足返回 `429 upload_busy`，不排队。正文路由的上限覆盖全局 16 MiB，超限统一投影 `413 request_too_large`。没有按 API Key 的上传速率计数。

上传与生成共用进程级连接断开监视和停机排空。断开停止退避与下一次 PUT，已在途写入不能回滚；可能留下孤儿对象，不删除、不返回 URL、不改账。生成入口的公网 URL 判定仍在幂等预查未命中之后，旧记录使用其冻结指纹规则。APIMart 不在生成路径内隐式上传素材；AIHubMix 也不下载参考图，当前通路见[AIHubMix Note](2026-10-06-aihubmix-ai-v1-execution-path.md)，历史设计末尾的旧 edits 取字节说明不再适用。

### 配置与凭据

完整环境变量及默认值由[配置参考](../../../../docs/operations/configuration.md#10-上传端点与上传存储)维护，不在本记录抄第二份取值表。应用配置验证 region、bucket 和 endpoint：region 与 bucket 使用小写字母、数字、连字符，首尾是字母或数字；region 最长 63，bucket 长 3–63 且不含点。含点 bucket 会破坏虚拟主机通配符证书匹配。endpoint 为无凭证、路径、query、fragment 的 origin；只收 HTTPS，loopback（127.0.0.1、::1、localhost）另允许 HTTP；省略时按 region 派生 OSS origin。

整组存储变量缺失允许进程启动，上传返回 `503 upload_storage_unavailable`；部分配置或形状错误拒绝启动并点名变量。启动不探测桶存在性、可达性或匿名可读性。两条访问密钥每次请求经 `CredentialProvider` 解析，各取一次，作为端口参数传入，不保存在服务或配置对象中。密钥、签名和素材字节不进入日志、trace、告警或响应。

### OSS V4 签名与写入核验

真实 OSS 请求使用虚拟主机 `{bucket}.{endpoint_host}`，loopback 测试端点使用 `/{bucket}/{key}` 的 path-style，签名的 canonical URI 恒为 `/{bucket}/{key}`，不能从请求 path 或 Host 推导。生产只发 header 模式 PUT 与 HEAD，签名日期使用 UTC，payload 标识为 `UNSIGNED-PAYLOAD`；规范头小写并按字典序排列，`host` 不参与 canonical headers。默认签名头为 `x-oss-*`、`content-type`、`content-md5`，其他显式头进入附加签名头集合。

PUT 的禁覆盖使用请求头 `x-oss-forbid-overwrite: true`，canonical query 为空，禁覆盖头参与签名。生产 PUT 金标准期望签名为 `c00baf659ad74992fd003d49e754bb137dfd35f235251529a398517dbce5e093`；固定输入与其他向量由[签名金标准](../../../../out-reference/oss-v4-signing-golden.md)保留，本记录承接历史设计对该生产形态的解释。算法与派生链、PUT/HEAD、GET header、预签名 GET query、附加签名头分别验证；GET/query 构造仅存在测试，不成为生产读取或预签名能力。官方示例公布值与占位密钥不能复算一致，不充当锚点。

派生链为 `HMAC("aliyun_v4" + SK, date) → region → "oss" → "aliyun_v4_request"`；canonical request 逐行拼接 Verb、canonical URI、排序的 canonical query、canonical headers、附加签名头列表和 payload 标识。string-to-sign 逐行拼接 `OSS4-HMAC-SHA256`、UTC timestamp、`date/region/oss/aliyun_v4_request` 和 canonical request 的 SHA-256 十六进制；派生密钥对该串做 HMAC-SHA256，写成 Authorization。路径按段编码，保留分隔符 `/` 和未保留字符，其余字节用大写十六进制百分号编码。`x-oss-date` 每次 PUT 或 HEAD 重新取当前 UTC，不缓存时间戳。平台不发 ACL 头，不实现 OSS STS、临时凭据或其他存储厂商签名。

PUT 后 HEAD 核对长度与规范 MIME，匹配后才组装公网 URL；元数据不匹配失败关闭。匿名可读是部署前置条件，由人工按[部署自检清单](../../../../docs/verification/object-storage-upload.md)对已知探针对象验证，不增加 GET 端口或健康状态。真实服务联调还须核对虚拟主机请求签名中包含 bucket。对象保留与清理归存储控制台。

### 失败分类与重试

应用编排唯一拥有重试预算；适配器不隐式重试、不跟随重定向。网络错误、连接超时、408、429、5xx 在总尝试次数与期限内按同一对象键重试，429/5xx 优先使用 [1, 300] 的整数秒 `Retry-After`，否则用固定退避。401、403、404、409、其他 4xx 与 HEAD 不一致是终态，返回 `503 object_store_unavailable`；409 不换键重写。存储缺配或密钥解析配置错误返回 `503 upload_storage_unavailable`，不投影 500。生成请求创建不重发的规则不限制此独立对象写入的同键重试。

PUT 和 HEAD 分开执行与计数，各自使用同一配置的总尝试上限。PUT 已确认成功后不再写入；HEAD 的可重试失败只重试 HEAD，不回到 PUT，不换对象键。媒体由内容魔数判定；调用方给了规范 MIME 时还须与判型一致，否则在写入前拒绝。该两阶段保证由 `upload_retries_head_without_writing_the_object_again` 用例验证，历史验证证据保留于下文。

## 备选方案

- 调用方自备图床：平台不碰素材，但把"能不能被渠道取到"变成调用方的问题，平台也失去对提交 URL 可读性的任何判据，不采用。
- 生成接口内联收字节（`data:` URL / 文件部件）：请求体要装下整张图、每次生成多一次搬运与解码，素材责任混进受理与幂等，不采用。
- 私有桶 + 预签名读取：URL 带签名与有效期，调用方拿到的是会失效的地址，上游渠道取图也要赶在有效期内，与"公网 URL 就是参数值"的语义冲突，不采用。
- 生成接口收字节、由平台在受理内自动上传换 URL：等于隐式上传，调用方看不见一次独立的副作用与失败面，不采用。
- 上传端点收批量文件、引入 manifest 与逐项状态：失败面从请求级裂成条目级，上传却仍不绑定 Job，批次状态没有归宿，不采用。
- 公开配置入库、管理端激活：多一套管理端点、一张配置表与激活状态机，只为在一种存储类型上改几个静态参数，不采用。

## 后果

改变公网 URL 决定需为已发出的匿名 URL 建兼容或撤销规则；改存储厂商需替换端口实现与签名、重新验证桶配置和已有对象可达性。恢复结果归档需要重建结果侧存储与资产身份，改变执行事实及不保存结果的边界；恢复生成入口内联字节需要为历史请求与新形态分别保留指纹解释，否则同键旧记录会被当前规则重新解释。两者都不能由上传存储配置自动推出。

- 调用方多一次调用；URL 可匿名读，分享 URL 等于分享图片；平台不承诺保留期、不代删；桶的匿名可读是运维前置条件，它的后果与处置归 [Spec 0007](../../../../docs/contracts/0007-image-upload-and-object-storage.md) §8。
- 上游渠道对公网 URL 的可达性不受平台控制（跨区域、鉴权头、防盗链），取不到图要到生成时才暴露；平台只保证自己写进去的对象匿名可读。
- 对象键前缀与对象存储配置是新增的运维面；上线后改用私有桶或预签名读，要为已发出的公网 URL 留解释规则。
- 上传端点是新的对外失败面（对象存储不可达、上传并发与内存预算已满、桶配错），错误只给稳定码与脱敏诊断，诊断能力靠阶段字段与对象存储请求标识。**失效范围（2026-10-08）**：本记录里"上传请求速率限制"这一层已随客户速率配额的取消而删除，见[删掉客户请求限流](../../implemented/platform/2026-10-08-drop-client-rate-limits.md)。

## 验证

端到端用例只打进程内假对象存储与假上游：`crates/domain` 覆盖媒体类型、单文件上限与对象键规则；`crates/application` 覆盖上传成功、各失败分类与重试、密钥按请求解析；`crates/adapter-object-storage` 对 [`out-reference/oss-v4-signing-golden.md`](../../../../out-reference/oss-v4-signing-golden.md) 的金标准向量逐字节复验签名。`apps/api/tests/http_contract/cases_upload.rs` 覆盖上传端点 Spec 0007 A1–A11，含真发整体超过全局 16 MiB 的合法 multipart 与声明、分块两条 `413 request_too_large`；`cases_direct_execution.rs` 覆盖生成入口 Spec 0005 A12，含以 `data:` URL 受理的历史记录在改版后仍按原记录回应。真实桶只用于人工受控验证（签名联调、跨区域访问、桶的匿名可读配置），按[上传存储部署自检执行清单](../../../../docs/verification/object-storage-upload.md)留档；签名正确性不靠真实桶证明：官方 V4 文档页公布的派生值与签名来自被替换过的真密钥，同样的输入复算不出，不能当锚。
