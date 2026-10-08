#!/usr/bin/env node
// 打包"没有仓库访问权限时"要传上去的那棵树（docs/operations/deployment.md §2.5）：
// 一个包，systemd 与容器两条路线都用它，部署方式在服务器上再定；`--no-web-src` 去掉前端源码、
// 只带 apps/web/dist。输出一个解包到部署根即可的 tar.gz。
//
// 为什么要有这个脚本：清单靠手抄，漏一个 `crates/` 成员时 cargo 才报错，漏 `apps/web` 的构建输入时
// `npm run build` 才失败，漏 `Dockerfile` 引用的路径时 `docker build` 才在 COPY 处失败，
// 三处都要等到服务器上才发现。这里在打包时就核对清单完整性。
import { execFileSync } from 'node:child_process';
import { closeSync, existsSync, openSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');

/// 前端源码。与 `apps/web/dist` 是二选一的关系：不带源码时产物必须在，否则服务器上没有界面。
/// 在服务器上打前端、或在服务器上 `docker build`（镜像里跑 `npm run build`）都要它。
const WEB_SRC = [
  'apps/web/package.json',
  'apps/web/package-lock.json',
  'apps/web/tsconfig.json',
  'apps/web/vite.config.ts',
  'apps/web/console.html',
  'apps/web/portal.html',
  'apps/web/src',
  'apps/web/e2e/check-bundle-isolation.mjs',
];

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
  // 前端源码与产物。
  ...WEB_SRC,
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
  console.error(`用法：node scripts/deploy/pack-deploy-tree.mjs [--no-web-src] [-o 输出文件]

  --no-web-src       不带前端源码，只带 apps/web/dist
  -o, --out <文件>   输出路径，默认 ./seeai-deploy-<YYYYMMDD>.tar.gz
  -h, --help         显示本说明`);
}

function parseArgs(argv) {
  const options = { out: null, noWebSrc: false, help: false, bad: false };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '-h' || arg === '--help') options.help = true;
    else if (arg === '--no-web-src') options.noWebSrc = true;
    else if (arg === '-o' || arg === '--out') {
      index += 1;
      if (!argv[index]) {
        usage(`${arg} 缺少文件名`);
        options.bad = true;
        return options;
      }
      options.out = argv[index];
    } else {
      usage(`无法识别的参数：${arg}`);
      options.bad = true;
      return options;
    }
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
if (options.bad) process.exit(2);
if (options.help) {
  usage();
  process.exit(0);
}

// `apps/web/dist` 是可选的：没有它，服务器上打前端或镜像内构建照常，只是"服务器不装 Node"那条不成立。
// 反过来，`--no-web-src` 不带源码，产物就成了唯一的前端，缺了它服务器上两个界面都打不开。
const hasDist = existsSync(join(repoRoot, 'apps/web/dist'));
if (options.noWebSrc && !hasDist) {
  console.error('--no-web-src 不带前端源码，所以 apps/web/dist 必须在；先跑 npm --prefix apps/web ci && npm --prefix apps/web run build。');
  process.exit(1);
}
const entries = ENTRIES.filter((entry) => hasDist || entry !== 'apps/web/dist').filter(
  (entry) => !options.noWebSrc || !WEB_SRC.includes(entry),
);

const missing = entries.filter((entry) => !existsSync(join(repoRoot, entry)));
const uncovered = workspaceMembers().filter(
  (member) => !entries.some((entry) => member === entry || member.startsWith(`${entry}/`)),
);
// `Dockerfile` 要读的路径也得在包里：`COPY . .` 之外还有前端输入与运行期素材。
// 目录与文件两种语义都收：`COPY apps/web apps/web` 只要求上下文里有 `apps/web` 下的东西，
// `COPY apps/web/package.json ...` 要求那个文件本身在。
const overlaps = (a, b) => a === b || a.startsWith(`${b}/`) || b.startsWith(`${a}/`);
const dockerMissing = dockerfileSources().filter(
  (source) => !entries.some((entry) => overlaps(source, entry)),
);
// `--no-web-src` 下缺的若正是前端源码，那是这个开关的预期后果：包不能 docker build，但别的都照常。
const dockerExcluded = options.noWebSrc
  ? dockerMissing.filter((source) => WEB_SRC.some((entry) => overlaps(source, entry)))
  : [];
const dockerFatal = dockerMissing.filter((source) => !dockerExcluded.includes(source));
if (missing.length > 0 || uncovered.length > 0 || dockerFatal.length > 0) {
  for (const entry of missing) console.error(`清单里的路径不存在：${entry}`);
  for (const member of uncovered) console.error(`workspace 成员没有被清单覆盖：${member}`);
  for (const source of dockerFatal) console.error(`Dockerfile 的 COPY 要读的路径没有被清单覆盖：${source}`);
  process.exit(1);
}

if (dockerExcluded.length > 0) {
  console.error(`警告：包里没有前端源码（Dockerfile 的 COPY 源缺 ${dockerExcluded.join('、')}），这份包不能 docker build；镜像要在别的机器上构建再导入。`);
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
//
// 输出走 `-f -` 加把 stdout 接到已打开的文件描述符，不把输出路径交给 tar：Windows 上从
// Git Bash 调用时 PATH 里是 GNU tar，它会把 `C:\...` 当成 `host:path` 去解析。
const outFd = openSync(out, 'w');
try {
  execFileSync('tar', ['-czf', '-', '-T', listFile], { cwd: repoRoot, stdio: ['ignore', outFd, 'inherit'] });
} catch (error) {
  closeSync(outFd);
  rmSync(out, { force: true });
  console.error(
    error.code === 'ENOENT'
      ? '找不到 tar 命令。Windows 10 1803 起自带 C:\\Windows\\System32\\tar.exe；没有就装 Git for Windows，或用 WSL 跑这个脚本。'
      : `打包失败：${error.message}`,
  );
  process.exit(1);
}
closeSync(outFd);

for (const entry of entries) console.log(`  ${entry}  ${human(sizeOf(entry))}`);
const name = out.split(/[\\/]/).pop();
console.log(`\n已打包：${out}（${human(statSync(out).size)}，${entries.length} 项）`);
console.log(`传到服务器：scp ${out} <服务器>:/tmp/`);
console.log(`服务器上：sudo install -d -m 755 <部署根> && sudo tar -xzf /tmp/${name} -C <部署根>`);
console.log('之后按 docs/operations/deployment.md §2.5 继续：systemd 走 docs/operations/production.md，容器走 docs/operations/production-docker.md。');
