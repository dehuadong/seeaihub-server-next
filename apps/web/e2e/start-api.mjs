#!/usr/bin/env node
// 端到端测试要跑的那个 API 进程：把两份前端产物托管起来，并按主机名分发。
//
// 与生产同一份代码路径——只有配置不同（`CONSOLE_DEV_HOST` 不设：spec 走 `admin.localhost` 这个真
// 主机名，正好命中 `admin.` 前缀那条判据；不靠任何只给测试用的分支）。
//
// 前置：`npm run build` 已经产出 `apps/web/dist`（Playwright 的 webServer 在起 API 之前先跑它）。
// 用法：`node e2e/start-api.mjs`，由 `playwright.config.ts` 的 webServer 启动并在结束时收掉。
import { spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

// 这个文件在 `apps/web/e2e/`：`..` 是 `apps/web`，再上两级才是仓库根（cargo 要在那里跑）。
const web = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const repo = resolve(web, '..', '..');

const dist = resolve(web, 'dist');
if (!existsSync(dist)) {
  console.error(`没有前端产物：${dist}（先跑 npm run build）`);
  process.exit(1);
}

/// 找 cargo：先看 PATH，再看 Rust 的常规安装位置。
///
/// 不要求调用者先配好环境：仓库的 Rust 工具链在 `~/.cargo/bin`，而它默认不在 PATH 上（本机实测
/// `cargo` 直接就是"不是内部或外部命令"）。找不到就明说，不猜第三个地方。
function findCargo() {
  const exe = process.platform === 'win32' ? 'cargo.exe' : 'cargo';
  const candidates = [
    process.env.CARGO,
    resolve(process.env.USERPROFILE ?? process.env.HOME ?? '', '.cargo', 'bin', exe),
    resolve(process.env.HOME ?? '', '.cargo', 'bin', exe),
  ].filter(Boolean);
  for (const candidate of candidates) {
    if (candidate && existsSync(candidate)) return candidate;
  }
  return 'cargo';
}

/// 绑定 `0.0.0.0` 而不是 `127.0.0.1`：`admin.localhost` 可能解析到 ::1，只听 IPv4 回环会连不上。
///
/// 这里**先把继承来的 `DATABASE_URL` 删掉**：它是 API 真正读的那个变量，而调用方的 shell 里可能残留
/// 指向别的库的值（本机实测踩到过——残留指向交付库，用例拿 e2e 的口令去登，必然失败）。子进程继承
/// 环境是对的，但"这次该连哪个库"必须由这里决定，不能碰巧。
const env = { ...process.env };
delete env.DATABASE_URL;
Object.assign(env, {
  DATABASE_URL: process.env.SEEAI_E2E_DATABASE ?? 'postgres://seeai:seeai@127.0.0.1:5432/seeai_e2e',
  API_BIND: process.env.SEEAI_E2E_API_BIND ?? '0.0.0.0:8090',
  ADMIN_TOKEN: process.env.SEEAI_E2E_ADMIN_TOKEN ?? 'e2e-shared-token',
  ADMIN_EMAIL: process.env.SEEAI_E2E_ADMIN_EMAIL ?? 'ops@example.com',
  ADMIN_PASSWORD: process.env.SEEAI_E2E_ADMIN_PASSWORD ?? 'a-long-enough-password',
  RUST_LOG: process.env.RUST_LOG ?? 'warn',
  // **显式不导入供给素材**：e2e 库每次重置成空库、用例自己造夹具；不设的话默认值
  // （`config/bootstrap`）会让每个用例都先看到仓库那两份素材。
  SUPPLY_MATERIAL_DIR: '',
  // 超时链压到秒级：e2e **不起 Worker**，`portal-settled-balance.spec.ts` 靠"同步入口在窗口后
  // 超时、请求停在持有中"来造一条真实的预授权。生产缺省的基础超时是 180 秒、对客窗口 390 秒，
  // 浏览器用例等不起。四环仍然自洽（窗口 ≥ 上游上限 ≤ 租约），值见
  // `crates/application/src/request_timeout.rs` 的校验。
  PROVIDER_TIMEOUT_BASE_SECONDS: '5',
  PROVIDER_TIMEOUT_INCLUDED_IMAGES: '4',
  PROVIDER_TIMEOUT_PER_IMAGE_SECONDS: '0',
  PROVIDER_TIMEOUT_SECONDS: '5',
  WORKER_LEASE_SECONDS: '5',
  GENERATION_SYNC_WAIT_SECONDS: '5',
});

// 不走 `shell`：参数原样传给子进程（`shell: true` 会把参数拼成命令行，Windows 上有转义与弃用警告）。
const child = spawn(findCargo(), ['run', '-p', 'seeai-api'], {
  cwd: repo,
  env,
  stdio: 'inherit',
});

// Playwright 收服务时发的是这个进程的信号；把子进程一起带走，别留下抢端口的孤儿。
for (const signal of ['SIGINT', 'SIGTERM']) {
  process.on(signal, () => child.kill(signal));
}
child.on('exit', (code) => process.exit(code ?? 0));
