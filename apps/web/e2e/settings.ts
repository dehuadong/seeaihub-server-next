/// 端到端测试的**共享设置**：`playwright.config.ts` 与每个 spec 都从这里取值。
///
/// 为什么不让 spec 直接读 `process.env`：Playwright 是**分进程**跑的——配置在启动进程里求值，
/// 用例在 worker 进程里跑，配置加载时写进 `process.env` 的东西**不会**可靠地传过去。两边一旦取到
/// 不同的口令，登录必然失败（CI 模式实测踩到过：页面上填的是 `e2e-admin-password`，而库里建的是
/// 配置里那个值，于是界面报"邮箱或口令不对"）。同一个模块求值就把这件事钉死了。
///
/// 这里只用 `node:` 内置模块，配置加载与 worker 里都能 import。
import { existsSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, '..', '..', '..');

/// 读仓库根的 `.env`（只取需要的两个键），让"本机怎么起服务"与"用例怎么登"用同一组凭据。
///
/// 不引第三方 dotenv：这里只要两行 `KEY=VALUE`，而多一个依赖就多一处版本要跟。
function fromDotEnv(key: string): string | undefined {
  const path = resolve(repo, '.env');
  if (!existsSync(path)) return undefined;
  for (const line of readFileSync(path, 'utf8').split(/\r?\n/)) {
    const match = line.match(/^\s*([A-Z0-9_]+)\s*=\s*(.*)\s*$/);
    if (match && match[1] === key) {
      // 去掉可选的成对引号。
      return match[2].replace(/^["']|["']$/g, '');
    }
  }
  return undefined;
}

const pick = (key: string, fallback: string): string =>
  process.env[key] ?? fromDotEnv(key) ?? fallback;

export const settings = {
  port: Number(pick('SEEAI_E2E_PORT', '8090')),
  database: pick('SEEAI_E2E_DATABASE', 'postgres://seeai:seeai@127.0.0.1:54329/seeai_e2e'),
  adminEmail: pick('ADMIN_EMAIL', 'ops@example.com'),
  adminPassword: pick('ADMIN_PASSWORD', 'e2e-admin-password'),
  adminToken: pick('ADMIN_TOKEN', 'e2e-shared-token'),
};

/// 两个入口的地址。主机名分发靠 `admin.` 前缀，Chrome 把 `*.localhost` 解析到回环，所以**不用改
/// hosts**，也不用开 `CONSOLE_DEV_HOST` 那个开发出口——测的就是生产那条判据。
export const consoleUrl = `http://admin.localhost:${settings.port}/`;
export const portalUrl = `http://app.localhost:${settings.port}/`;
export const adminApiUrl = `http://127.0.0.1:${settings.port}`;
