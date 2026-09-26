#!/usr/bin/env node
// 端到端测试的**数据库准备**：把 `SEEAI_E2E_DATABASE` 指着的那个库重置成**空库**，交给 API 启动时
// 自己跑迁移（`HubRepository::migrate` 在启动路径上）。
//
// 为什么每次都重建：夹具库是**从模板克隆**的，模板里一旦有历史行就会污染之后的每一次运行——这个坑
// 实际踩过（见 `.agents/notes/implemented/platform/2026-09-27-identity-sessions-and-consoles.md`）。
// 从 `template0` 建库能拿到真正空的一份，脚本与端口都从环境变量读，不写死本机拓扑。
//
// 用法：`node e2e/ensure-database.mjs`（由 `playwright.config.ts` 的 webServer 先跑）。
import { execFileSync } from 'node:child_process';

/// 基础连接串：形如 `postgres://user:password@host:port/dbname`。
const base = process.env.SEEAI_E2E_DATABASE ?? 'postgres://seeai:seeai@127.0.0.1:54329/seeai_next';
const url = new URL(base);
const database = url.pathname.replace(/^\//, '');
if (!database) {
  console.error(`SEEAI_E2E_DATABASE 必须带库名：${base}`);
  process.exit(1);
}

/// 容器名：库里跑着一份 PostgreSQL 时用它执行 psql，省掉在宿主机装客户端。
const container = process.env.SEEAI_E2E_PG_CONTAINER ?? 'seeaihub-server-next-postgres-1';

/// 执行一条 psql 语句。`ON_ERROR_STOP` 让失败直接变成非零退出，而不是印一行错继续。
function psql(statement, { database: target = 'postgres' } = {}) {
  const args = [
    'exec',
    container,
    'psql',
    '-v',
    'ON_ERROR_STOP=1',
    '-U',
    decodeURIComponent(url.username),
    '-d',
    target,
    '-c',
    statement,
  ];
  return execFileSync('docker', args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
}

try {
  // `template0` 保证是干净的：`template1` 会被上一个用例的遗留行污染。
  psql(`DROP DATABASE IF EXISTS "${database}" WITH (FORCE)`);
  psql(`CREATE DATABASE "${database}" OWNER "${decodeURIComponent(url.username)}" TEMPLATE template0`);
  console.log(`e2e 数据库已重置为空库：${database}（迁移由 API 启动时自己跑）`);
} catch (error) {
  const detail = error.stderr ?? error.message;
  console.error(`重置 e2e 数据库失败：${detail}`);
  process.exit(1);
}
