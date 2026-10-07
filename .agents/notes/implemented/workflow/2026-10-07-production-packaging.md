---
title: 生产打包：在构建机把 /opt/seeai 编进二进制
status: implemented
created: 2026-10-07
updated: 2026-10-07
approval: 用户 2026-10-07 要求检查生产部署文档并创建打包脚本，给出执行实现授权
verification: `build/package.sh` 端到端跑通（前端构建、bubblewrap 沙箱编译、组装、GLIBC 核对）；把包解到沙箱 `/opt/seeai` 起 API：`GET /health` 回 `200`，`admin.example.com/` 与 `app.example.com/` 回不同的入口产物，`GET /v1/docs/README.md` 回 `200`，日志有 `supply materials imported` 且没有 `no web build found`
---

# Agent Note：生产打包：在构建机把 /opt/seeai 编进二进制

## 问题

生产部署原先要在服务器上构建：仓库文件多，上传与编译都慢。API 二进制又把编译期仓库路径写死（[生产环境](../../../../docs/operations/production.md) §1.1），只在编译时仓库根下的 `apps/web/dist` 找前端产物，所以二进制不能搬到别的绝对路径。

## 决定

新增 [`build/package.sh`](../../../../build/package.sh)：在构建机上用 bubblewrap 把仓库挂到 `/opt/seeai` 再编译，把 `/opt/seeai` 编进二进制，组装成只能解到 `/opt/seeai` 的 tar 包。包内含两个二进制、前端产物、空的 `apps/api/`、`public-docs/`、`config/bootstrap/` 与 `deploy/systemd/` 两个单元。

脚本核对二进制里确有 `/opt/seeai/apps/api`（对不上直接失败），并量出二进制要求的最高 GLIBC 版本写进包内 `PACKAGE.txt`。

systemd 单元从文档抽到 [`deploy/systemd/`](../../../../deploy/systemd/)，文档与包共用一份；[容器部署](../../../../docs/operations/production-docker.md) 补上装 Docker、`docker save`/`docker load` 与用 `sudo env` 传镜像 tag 的写法。

## 备选方案

- **在服务器上构建**：不用新工具，但服务器要有完整工具链，每次升级都要重编。保留为[生产环境](../../../../docs/operations/production.md) §2.3 的备选，给 glibc 比包要求的旧的目标机用。
- **用 Docker 出包再导出**：systemd 部署要裸二进制；镜像里的编译期路径是 `/app`，与 `/opt/seeai` 不一致，导出的二进制仍要求解到 `/app`。容器部署自己走 `docker save`/`docker load`。
- **给前端产物路径加运行时环境变量覆盖**：能去掉路径写死，但改的是 Spec/RFC 拥有的合同，超出本次范围。
- **用 root 把仓库复制到 `/opt/seeai` 再构建**：污染构建机的 `/opt`，且要 root。bubblewrap 用挂载命名空间达到同样效果。

## 后果

- 包只能解到 `/opt/seeai`；换部署路径要同时改脚本与单元文件。
- 二进制要求构建机的 glibc：Ubuntu 26.04 上编出的包要求 `GLIBC_2.38`，服务器更旧时只能用就地构建。脚本把该值写进包，部署前先核对。
- 构建机要有 `bubblewrap`（Ubuntu 的 `bubblewrap` 包），首次构建要下载 crate 与 npm 依赖。

## 验证

`build/package.sh` 端到端跑通：构建两份前端产物，bubblewrap 沙箱里编译出两个二进制，组装成 `build/dist/seeai-<版本>.tar.gz`（18 MB），量出 `GLIBC_2.38`。

把包解到 bubblewrap 沙箱的 `/opt/seeai`（模拟生产路径）起 API，用一次性库 `seeai_pkgcheck`：

- `GET /health` → `200 {"status":"ok"}`；
- `Host: admin.example.com` 的 `/` 与 `Host: app.example.com` 的 `/` 回两份不同产物；
- `GET /console.html`、`GET /v1/docs/README.md` → `200`；
- 日志有 `supply materials imported`（2 素材、4 Offering），没有 `no web build found`。

文档侧跑 `node scripts/decisions/check.mjs`。
