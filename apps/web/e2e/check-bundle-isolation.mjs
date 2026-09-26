#!/usr/bin/env node
// 两份产物**互不引用**的核对（Spec V-D6 / D4）：客户入口引的脚本里不该出现管理端的代码，反之亦然。
//
// 为什么要有这个脚本：这条性质靠**打包结果**保证（两个入口各引各的模块），一次 import 改动就可能破坏
// 它，而肉眼 grep 不会每次都做。`e2e/bundles-are-isolated.spec.ts` 在 spec 里跑同一份判据（顺带证明
// 浏览器那一层没串），这个脚本让 CI 与本地能**不启服务、不跑浏览器**单独核对一条命令。
//
// 判据用**带引号的精确路径**：裸路径会假阳性——`/v1/customers` 是管理端自己的 `/api/v1/customers` 的
// 子串（写 `apps/web/README.md` 那两条核对命令时踩过）。
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

/// 管理端**独有**的路径前缀、客户端**独有**的路径前缀，以及两个控制台各自的会话键。
export const ADMIN_ONLY = ['"/api/v1/'];
export const PORTAL_ONLY = ['"/v1/customer/'];
export const CONSOLE_KEY = 'seeai.console.session';
export const PORTAL_KEY = 'seeai.portal.session';

/// 从入口 HTML 里取出它引用的资源文件名。
export function assetsOf(dist, entry) {
  const html = readFileSync(resolve(dist, entry), 'utf8');
  return [...html.matchAll(/(?:src|href)="(\/assets\/[^"]+)"/g)].map((match) => match[1]);
}

/// 从入口出发，把**真正会被加载的**资源都收齐：先取 HTML 里的静态引用，再从每个脚本的内容里找它
/// 引用到的产物文件名（`import(...)`、`from"./x.js"`、`new URL("./x.js")` 都算），递归下去。
///
/// 为什么要遍历而不能只看 HTML：**动态 `import()` 出来的分块不在入口 HTML 里**。只看 HTML 的话，
/// 一个 `import('./portal-chunk.js')` 就能把另一份入口的代码挂到这一份上而核对发现不了。
export function reachableAssets(dist, entry) {
  const seen = new Set();
  const queue = assetsOf(dist, entry);
  while (queue.length > 0) {
    const asset = queue.shift();
    if (seen.has(asset)) continue;
    const path = resolve(dist, asset.replace(/^\//, ''));
    if (!existsSync(path)) continue;
    seen.add(asset);
    // 只从文本产物里找下一跳；图片与字体里不会有文件名引用。
    if (!/\.(js|css|html)$/.test(asset)) continue;
    const text = readFileSync(path, 'utf8');
    // 产物文件名形如 `console-XXXXXXXX.js` / `styles-XXXXXXXX.css`：按目录清单匹配比猜正则可靠。
    for (const name of readdirSync(resolve(dist, 'assets'))) {
      if (name.endsWith('.map')) continue;
      if (name === asset.replace(/^\/assets\//, '')) continue;
      if (text.includes(name)) queue.push(`/assets/${name}`);
    }
  }
  return [...seen];
}

function contentsOf(dist, assets) {
  return assets
    .map((asset) => readFileSync(resolve(dist, asset.replace(/^\//, '')), 'utf8'))
    .join('\n');
}

/// 核对一个产物目录，返回**违规清单**（空数组＝通过）。
///
/// 返回清单而不是抛异常：这样 spec 可以断言"清单为空"，脚本可以打印，变异验证也可以直接看它是否非空。
export function violations(dist) {
  const found = [];
  for (const entry of ['console.html', 'portal.html']) {
    if (!existsSync(resolve(dist, entry))) {
      found.push(`缺少入口文件：${entry}（先跑 npm run build）`);
    }
  }
  if (found.length > 0) return found;

  const portal = contentsOf(dist, reachableAssets(dist, 'portal.html'));
  const consoleBundle = contentsOf(dist, reachableAssets(dist, 'console.html'));

  for (const needle of ADMIN_ONLY) {
    if (portal.includes(needle)) found.push(`客户产物里出现了管理端端点 ${needle}`);
  }
  if (portal.includes(CONSOLE_KEY)) found.push('客户产物里出现了控制台的会话键');
  for (const needle of PORTAL_ONLY) {
    if (consoleBundle.includes(needle)) found.push(`管理产物里出现了对客端点 ${needle}`);
  }
  if (consoleBundle.includes(PORTAL_KEY)) found.push('管理产物里出现了客户端的会话键');

  // 反向断言：各自**应当**带着自己的那一份——否则上面那些"不含"可能是"什么都没打包"而恒真。
  if (!portal.includes(PORTAL_ONLY[0])) found.push('客户产物里没有对客端点，说明它没打包出内容');
  if (!consoleBundle.includes(ADMIN_ONLY[0])) found.push('管理产物里没有管理端点，说明它没打包出内容');

  // 两个入口的脚本文件必须是不同的两份。
  const bundleNames = readdirSync(resolve(dist, 'assets'));
  const portalEntry = bundleNames.find((name) => name.startsWith('portal-') && name.endsWith('.js'));
  const consoleEntry = bundleNames.find((name) => name.startsWith('console-') && name.endsWith('.js'));
  if (!portalEntry || !consoleEntry) {
    found.push('缺入口脚本：console-*.js 或 portal-*.js');
  } else {
    if (readFileSync(resolve(dist, 'portal.html'), 'utf8').includes(consoleEntry)) {
      found.push('客户入口引用了管理端的入口脚本');
    }
    if (readFileSync(resolve(dist, 'console.html'), 'utf8').includes(portalEntry)) {
      found.push('管理入口引用了客户端的入口脚本');
    }
  }
  return found;
}

// 直接运行（`node e2e/check-bundle-isolation.mjs`）时打印结论并以退出码表达通过与否。
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const dist = resolve(dirname(fileURLToPath(import.meta.url)), '..', 'dist');
  const found = violations(dist);
  if (found.length === 0) {
    console.log(`产物隔离核对通过：${dist}`);
  } else {
    console.error(`产物隔离核对失败（${dist}）：`);
    for (const item of found) console.error(`  - ${item}`);
    process.exitCode = 1;
  }
}
