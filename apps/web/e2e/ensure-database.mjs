#!/usr/bin/env node
// 端到端测试的**数据库准备**：把 `SEEAI_E2E_DATABASE` 指着的那个库重置成**空库**，交给 API 启动时
// 自己跑迁移（`HubRepository::migrate` 在启动路径上）。
//
// 为什么每次都重建：库可能是**从模板克隆**来的，模板里一旦有历史行就会污染之后的每一次运行——这个坑
// 实际踩过（见 `.agents/notes/implemented/platform/2026-09-27-identity-sessions-and-consoles.md`）。
// 从 `template0` 建库能拿到真正空的一份。
//
// **默认库名是 `seeai_e2e`**，不是别的用途的库：这个脚本每次都会把它删掉重建，指向正在用的开发库或
// 交付库就等于把它们清空（实际差点踩到——交付库是从另一个库克隆来的，而那个库一度正是这里的默认值）。
//
// 走 Node 的 Postgres 客户端而不是 `docker exec ... psql`：同一个脚本要能在开发机与 CI 里都跑，
// 而 CI 的数据库是 workflow 起的 service（没有 docker，也没有 psql 客户端）。
//
// 用法：`node e2e/ensure-database.mjs`（由 `playwright.config.ts` 的 webServer 先跑）。
import postgres from 'postgres';

/// 基础连接串：形如 `postgres://user:password@host:port/dbname`。
///
/// `playwright.config.ts` 会把同一个值**显式**交给这个进程（来自 `e2e/settings.ts`），所以正常运行
/// 时不依赖这里的默认值；默认值只是让这个脚本能单独跑一下。
const base = process.env.SEEAI_E2E_DATABASE ?? 'postgres://seeai:seeai@127.0.0.1:5432/seeai_e2e';
const url = new URL(base);
const database = decodeURIComponent(url.pathname.replace(/^\//, ''));
if (!database) {
  console.error(`SEEAI_E2E_DATABASE 必须带库名：${base}`);
  process.exit(1);
}

/// 连到 `postgres` 维护库上执行建库/删库：这两件事不能在目标库里做。
const admin = postgres(
  `postgres://${encodeURIComponent(decodeURIComponent(url.username))}:${encodeURIComponent(decodeURIComponent(url.password))}@${url.host}/postgres`,
  { max: 1, onnotice: () => {} },
);

try {
  // `WITH (FORCE)` 断开残留连接：上一次跑崩留下的连接会让删库失败。
  await admin.unsafe(`DROP DATABASE IF EXISTS "${database}" WITH (FORCE)`);
  // `template0` 保证是干净的：`template1` 可能已经被上一个用例的遗留行污染。
  await admin.unsafe(`CREATE DATABASE "${database}" TEMPLATE template0`);
  console.log(`e2e 数据库已重置为空库：${database}（迁移由 API 启动时自己跑）`);
} catch (error) {
  console.error(`重置 e2e 数据库失败：${error.message}`);
  process.exitCode = 1;
} finally {
  await admin.end();
}
