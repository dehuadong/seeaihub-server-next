#!/usr/bin/env node
// 打包"没有仓库访问权限时"要传上去的那棵树（docs/operations/production.md §2.2、
// docs/operations/production-docker.md §2.2）：一个包，systemd 与容器两条路线都用它，
// 部署方式在服务器上再定。输出一个解包到部署根即可的 tar.gz。
//
// 为什么要有这个脚本：清单靠手抄，漏一个 `crates/` 成员时 cargo 才报错，漏 `apps/web` 的构建输入时
// `npm run build` 才失败，漏 `Dockerfile` 引用的路径时 `docker build` 才在 COPY 处失败，
// 三处都要等到服务器上才发现。这里在打包时就核对清单完整性。
import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');

const ENTRIES = [
  // workspace 清单与路径依赖的 crate：缺一个成员，cargo 直接报错。
  'Cargo.toml',
  'Cargo.lock',
  'crates',
  'apps/api',
  'apps/worker',
  // 编译期嵌入二进制。
  'migrations',
  // 运行期读。
  'public-docs',
  'config/bootstrap',
  // systemd 路线：装到 /etc/systemd/system/ 的两个单元。
  'deploy/systemd',
  // 容器路线：在服务器上 docker build 与 compose up。
  'Dockerfile',
  '.dockerignore',
  'deploy/compose.prod.yaml',
  // 前端：产物供"服务器不装 Node"用，源码供服务器上打前端或镜像内构建用。
  'apps/web/package.json',
  'apps/web/package-lock.json',
  'apps/web/tsconfig.json',
  'apps/web/vite.config.ts',
  'apps/web/console.html',
  'apps/web/portal.html',
  'apps/web/src',
  'apps/web/e2e/check-bundle-isolation.mjs',
  'apps/web/dist',
];

/// 产物不进版本库，改了源码没重新构建时看不出来，传上去就是一份旧界面。只在 `dist` 存在时比对。
const WEB_DIST_INPUTS = [
  'apps/web/src',
  'apps/web/console.html',
  'apps/web/portal.html',
  'apps/web/vite.config.ts',
  'apps/web/tsconfig.json',
  'apps/web/package-lock.json',
];

function usage(message) {
  if (message) console.error(`${message}\n`);
  console.error(`用法：node scripts/deploy/pack-deploy-tree.mjs [-o 输出文件]

  -o <文件>     输出路径，默认 ./seeai-deploy-<YYYYMMDD>.tar.gz`);
  process.exitCode = 2;
}

function parseArgs(argv) {
  const options = { out: null };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '-o' || arg === '--out') {
      index += 1;
      if (!argv[index]) return usage(`${arg} 缺少文件名`);
      options.out = argv[index];
    } else return usage(`无法识别的参数：${arg}`);
  }
  return options;
}

/// workspace 成员是构建的最小闭包：清单里列出的目录缺一个，cargo 直接报错。逐个核对它们都被覆盖。
function workspaceMembers() {
  const manifest = readFileSync(join(repoRoot, 'Cargo.toml'), 'utf8');
  const block = manifest.match(/members\s*=\s*\[([\s\S]*?)\]/);
  if (!block) throw new Error('Cargo.toml 里找不到 [workspace] members');
  return [...block[1].matchAll(/"([^"]+)"/g)].map((match) => match[1]);
}

/// `Dockerfile` 里非 `--from=` 的 `COPY` 源路径（`COPY . .` 是上下文根，不算）：
/// 少一个文件时 docker build 只在 COPY 那步报错，这里提前查出来。
function dockerfileSources() {
  const dockerfile = readFileSync(join(repoRoot, 'Dockerfile'), 'utf8');
  return dockerfile
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => /^COPY\s/i.test(line) && !line.includes('--from='))
    .flatMap((line) => line.split(/\s+/).slice(1, -1))
    .map((source) => source.replace(/\/+$/, ''))
    .filter((source) => source !== '' && source !== '.');
}

function* walkAll(entry) {
  const stats = statSync(join(repoRoot, entry));
  yield stats;
  if (!stats.isDirectory()) return;
  for (const child of readdirSync(join(repoRoot, entry), { withFileTypes: true })) {
    yield* walkAll(`${entry}/${child.name}`);
  }
}

function sizeOf(entry) {
  let total = 0;
  for (const stats of walkAll(entry)) if (!stats.isDirectory()) total += stats.size;
  return total;
}

function newestMtime(entry) {
  let newest = 0;
  for (const stats of walkAll(entry)) if (stats.mtimeMs > newest) newest = stats.mtimeMs;
  return newest;
}

function human(bytes) {
  return bytes >= 1024 * 1024 ? `${(bytes / 1024 / 1024).toFixed(1)} MB` : `${Math.ceil(bytes / 1024)} KB`;
}

const options = parseArgs(process.argv.slice(2));
if (process.exitCode === 2) process.exit(2);

// `apps/web/dist` 是可选的：没有它，服务器上打前端或镜像内构建照常，只是"服务器不装 Node"那条不成立。
const hasDist = existsSync(join(repoRoot, 'apps/web/dist'));
const entries = hasDist ? ENTRIES : ENTRIES.filter((entry) => entry !== 'apps/web/dist');

const missing = entries.filter((entry) => !existsSync(join(repoRoot, entry)));
const uncovered = workspaceMembers().filter(
  (member) => !entries.some((entry) => member === entry || member.startsWith(`${entry}/`)),
);
// `Dockerfile` 要读的路径也得在包里：`COPY . .` 之外还有前端输入与运行期素材。
// 目录与文件两种语义都收：`COPY apps/web apps/web` 只要求上下文里有 `apps/web` 下的东西，
// `COPY apps/web/package.json ...` 要求那个文件本身在。
const dockerMissing = dockerfileSources().filter(
  (source) =>
    !entries.some(
      (entry) => entry === source || entry.startsWith(`${source}/`) || source.startsWith(`${entry}/`),
    ),
);
if (missing.length > 0 || uncovered.length > 0 || dockerMissing.length > 0) {
  for (const entry of missing) console.error(`清单里的路径不存在：${entry}`);
  for (const member of uncovered) console.error(`workspace 成员没有被清单覆盖：${member}`);
  for (const source of dockerMissing) console.error(`Dockerfile 的 COPY 要读的路径没有被清单覆盖：${source}`);
  process.exit(1);
}

if (hasDist) {
  const staleInput = WEB_DIST_INPUTS.filter((entry) => newestMtime(entry) > newestMtime('apps/web/dist'));
  if (staleInput.length > 0) {
    console.error(`警告：apps/web/dist 比这些输入旧，可能没重新构建：${staleInput.join('、')}`);
    console.error('先跑 npm --prefix apps/web run build 再打包。');
  }
} else {
  console.error('提示：apps/web/dist 不存在，包里没有前端产物；服务器上要跑 npm ci && npm run build（或先在本地构建）。');
}

const date = new Date().toISOString().slice(0, 10).replaceAll('-', '');
const out = resolve(options.out ?? `seeai-deploy-${date}.tar.gz`);
const listFile = join(tmpdir(), `seeai-deploy-${process.pid}.txt`);
writeFileSync(listFile, `${entries.join('\n')}\n`);
// `-T` 而不是把清单放进 argv：条目会随清单增长，argv 在 Windows 上有长度上限。
execFileSync('tar', ['-czf', out, '-T', listFile], { cwd: repoRoot, stdio: ['ignore', 'ignore', 'inherit'] });

for (const entry of entries) console.log(`  ${entry}  ${human(sizeOf(entry))}`);
console.log(`\n已打包：${out}（${human(statSync(out).size)}，${entries.length} 项）`);
console.log(`服务器上：sudo install -d -m 755 <部署根> && sudo tar -xzf ${out.split(/[\\/]/).pop()} -C <部署根>`);
console.log('之后按 docs/operations/production.md §2.2（systemd）或 docs/operations/production-docker.md §2.2（容器）继续。');
