# 阿里云 OSS V4 签名金标准向量（落档）

> **性质**：参考实现的验收基准向量，外部参考资源，**不是平台接口合同**，运行中的服务不读取。
> **来源**：参考实现 `/mnt/d/workspace/seeaihub` 的 `src/service/tests/uploads_signing_golden.rs`（自述其 golden 是正式实现的验收基准）、`src/service/src/uploads/signing/oss_v4.rs`（生产签名器）与 `src/service/src/uploads/signing/oss_v4_test.rs`（附加签名头向量）；生产请求构造在 `src/service/src/uploads/runtime.rs`。
> **用途**：给[对象存储上传设计](../docs/design/0021-object-storage-upload.md) §5 的签名器对拍提供固定输入与期望值（§2 末行是按该设计规则复算的生产 PUT 值，不是参考实现的输出）；参考实现本身在仓库外，本档让审阅与链接检查在仓库内够得到这些向量。

## 1. 固定输入

| 项 | 值 |
| --- | --- |
| AK id / SK | `AKIDEXAMPLE` / `SKEXAMPLE123456` |
| region | `cn-hangzhou` |
| bucket / object | `my-bucket` / `reference-media/2026-08-12/uuid-123.jpg` |
| canonical URI | `/my-bucket/reference-media/2026-08-12/uuid-123.jpg`（即 `/{bucket}/{key}`） |
| `x-oss-date`（scope 日期） | `20260812T103000Z`（`20260812`） |
| 算法与 `Authorization` 前缀 | `OSS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260812/cn-hangzhou/oss/aliyun_v4_request,Signature=` |

## 2. header 模式向量

| 操作 | 输入 | 期望 Signature |
| --- | --- | --- |
| PUT | canonical query `x-oss-forbid-overwrite=true`、头 `content-type: image/jpeg` | `b3a10a1219b3e7a8dc32be76c288025027602d8227a591db3317f9be10cc8b14` |
| HEAD | 无 query、无显式头 | `90cf7b7b9c6fc4062c5bcb51b2526cf11c24a7e6ded03d07d3532863438fc06a` |
| GET | 头 `content-type: application/octet-stream` | `205d5ebe2322cc91d4657f9947681f95c13af8980e6315fe957bcbf017c486cc` |
| PUT（按本设计规则复算，非参考实现输出） | canonical query 为空、头 `content-type: image/jpeg` 与 `x-oss-forbid-overwrite: true` | `c00baf659ad74992fd003d49e754bb137dfd35f235251529a398517dbce5e093` |

上列前三条输入都没有非默认头，AdditionalHeaders 为空，HashedPayload 是 `UNSIGNED-PAYLOAD`。

最后一行不是参考实现的输出：参考实现的生产 PUT 用请求头形态禁覆盖（§6），它的 golden 只钉 query 形态，这一行按[对象存储上传设计](../docs/design/0021-object-storage-upload.md) §5 的请求构造规则复算（同一构造对上面三条复算后与本档逐字节一致），供实现时对拍；实施时须用移植用例逐字节复验后再钉住。

## 3. 预签名 GET（query 模式）

输入是 §1 的 bucket、object、`x-oss-date`，加 endpoint host `oss-cn-hangzhou.aliyuncs.com` 与 `x-oss-expires=86400`，期望 URL：

```
https://my-bucket.oss-cn-hangzhou.aliyuncs.com/reference-media/2026-08-12/uuid-123.jpg?x-oss-signature-version=OSS4-HMAC-SHA256&x-oss-date=20260812T103000Z&x-oss-expires=86400&x-oss-credential=AKIDEXAMPLE%2F20260812%2Fcn-hangzhou%2Foss%2Faliyun_v4_request&x-oss-signature=9b7e5830a4e4226af611d9a5b05e3abddcbf12ef17e7ad0e2fadd9731ecb9b2c
```

特殊对象键 `reference-media/2026-08-12/uuid 图.jpg` 用同一输入，路径段编码为 `/reference-media/2026-08-12/uuid%20%E5%9B%BE.jpg`，`x-oss-signature` 是 `c34b0269ce103db552b72b14c9c54949d34028023532cee8373933544a35ea95`。

## 4. 附加签名头向量

| 项 | 值 |
| --- | --- |
| 固定输入 | AK id / SK `LTAIEXAMPLE` / `yourAccessKeySecret`；region `cn-hangzhou`；`x-oss-date` `20250411T064124Z`；canonical URI `/examplebucket/exampleobject` |
| 显式头 | `content-type: text/plain`、`content-md5: ICy5YqxZB1uWSwcVLSNLcA==`、`content-length: 3`、`content-disposition: attachment` |
| 期望 AdditionalHeaders | `content-disposition;content-length` |
| 期望 Signature | `d3694c2dfc5371ee6acd35e88c4871ac95a7ba01d3a2f476768fe61218590097` |

## 5. 覆盖与计数结论

`uploads_signing_golden.rs` 覆盖 PUT、HEAD、预签名 GET、特殊对象键与 `Retry-After` 的 [1,300] 整数秒边界，并用脚本化 loopback transport 断言每个子操作的真实请求数：PUT 1 次、HEAD 1 次、预签名 GET 1 次。

## 6. 与生产请求构造的差异

参考实现的生产 PUT（`src/service/src/uploads/runtime.rs`）把禁覆盖放请求头 `x-oss-forbid-overwrite: true` 参与签名，canonical query 为空，其注释记录 query 形态被真实桶拒过；金标准的 PUT 行固定的是 query 形态，只锁算法与派生链。本设计的生产 PUT 与参考实现的生产构造一致，取请求头形态（[设计 §5](../docs/design/0021-object-storage-upload.md)）。
