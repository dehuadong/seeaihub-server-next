import { readdir, readFile, access } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
export const root = fileURLToPath(new URL('../../.agents/notes/', import.meta.url));
// 仓库管理根目录：lib.mjs 在 scripts/decisions/ 下，`../../` 才是仓库根（写成 `../../../` 会落到仓库外，
// 读取注册表时被 `.catch` 静默吞掉，检查等于空跑——实测过）。
export const rootDir = fileURLToPath(new URL('../../', import.meta.url));
export const lifecycles = ['proposed', 'implemented', 'rejected'];

/** 元数据块允许的键与顺序：只用这套键，按这个顺序。各生命周期的必填与禁用见 metadataRules。 */
export const metadataKeys = ['title', 'status', 'created', 'updated', 'approval', 'verification', 'reason'];

/** 只有 implemented 记录带 verification，只有 rejected 记录带 reason。 */
const metadataRules = {
  proposed: { required: [], forbidden: ['verification', 'reason'] },
  implemented: { required: ['verification'], forbidden: ['reason'] },
  rejected: { required: ['reason'], forbidden: ['verification'] },
};

/** 格式建立日。该日（含）之前创建的记录，备选方案无法还原时可用豁免注释顶替。 */
export const formatAdopted = '2026-09-22';

/** 顶替 `## 备选方案` 的豁免注释，必须逐字一致。 */
export const grandfatherComment = '<!-- agent-note-format: alternatives-not-recorded (pre-format Agent Note) -->';

/** 正文第一个章节，三种生命周期一致。 */
export const opener = '## 问题';

/** 每个生命周期的正文骨架：必需章节与禁用章节。`## 备选方案` 由下面的统一检查负责（可用豁免注释顶替）。 */
export const formatRules = {
  proposed: { required: ['## 提案', '## 验收条件', '## 风险'], banned: [] },
  implemented: { required: ['## 决定', '## 后果', '## 验证'], banned: ['## 提案', '## 计划', '## 迁移计划', '## 验收条件'] },
  rejected: { required: [], banned: [] },
};

// 逐段解析相对链接目标，完全不依赖 path.resolve / path.normalize / path.join：
// 实测本机 Node v24.10.0（Windows）会丢弃 `..` 而不上跳（`../..` 也只上一级），
// 用这些原语会把正确的跨目录相对链接误判为断链。
export function resolveTarget(baseDir, target) {
  const base = baseDir.replaceAll('\\', '/').replace(/\/+$/, '');
  const i = base.indexOf(':');                       // 盘符（或空）单独处理，不参与 `..` 计数
  const drive = i === -1 ? '' : base.slice(0, i + 1);
  const rest = (i === -1 ? base : base.slice(i + 1)).replace(/^\/+/, '');
  const stack = rest ? rest.split('/').filter(Boolean) : [];
  for (const seg of target.replaceAll('\\', '/').split('/')) {
    if (!seg || seg === '.') continue;
    if (seg === '..') { stack.pop(); continue; }
    stack.push(seg);
  }
  return drive + '/' + stack.join('/');
}

const dateOK = v => /^\d{4}-\d{2}-\d{2}$/.test(v ?? '') && !Number.isNaN(Date.parse(v)) && new Date(v).toISOString().slice(0,10) === v;

// 检查一份记录的元数据块与文件内格式，规则以 .agents/notes/README.md 的「文件格式」为准。
function checkRecord(relative, lifecycle, text, filenameDate) {
  const errors = [];
  const header = /^---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)/.exec(text);
  if (!header) return { errors: [`${relative}: 缺少元数据块`], meta: {} };
  const meta = {}, keys = [];
  for (const line of header[1].split(/\r?\n/)) {
    const field = /^([a-z]+): (.+)$/.exec(line);
    if (!field) { errors.push(`${relative}: 元数据行必须是 \`key: value\`：${line}`); continue; }
    if (Object.hasOwn(meta, field[1])) { errors.push(`${relative}: 元数据键重复：${field[1]}`); continue; }
    meta[field[1]] = field[2].trim();
    keys.push(field[1]);
  }
  const ordered = metadataKeys.filter(key => Object.hasOwn(meta, key));
  if (keys.join(',') !== ordered.join(',')) {
    errors.push(`${relative}: 元数据只能用 ${metadataKeys.join(' / ')} 这套键并按此顺序；现在这些键：${keys.join(' / ')}`);
  }
  for (const key of ['title','status','created','updated','approval']) if (!meta[key]) errors.push(`${relative}: 缺少 ${key}`);
  const rules = metadataRules[lifecycle];
  for (const key of rules.required) if (!meta[key]) errors.push(`${relative}: ${lifecycle} 记录必须有 ${key}`);
  for (const key of rules.forbidden) if (meta[key]) errors.push(`${relative}: ${key} 只用于 ${key === 'reason' ? 'rejected' : 'implemented'} 记录`);
  if (meta.status !== lifecycle) errors.push(`${relative}: status 必须与所在生命周期目录一致`);
  for (const key of ['created','updated']) if (!dateOK(meta[key])) errors.push(`${relative}: ${key} 必须是 YYYY-MM-DD`);
  if (meta.created && meta.created !== filenameDate) errors.push(`${relative}: created 与文件名日期不一致`);
  if (dateOK(meta.created) && dateOK(meta.updated) && meta.updated < meta.created) errors.push(`${relative}: updated 早于 created`);

  // 格式检查：标题行、首个章节、各生命周期的规范章节，以及被禁的提案期章节。
  const firstLine = text.slice(header[0].length).split(/\r?\n/).find(line => line.trim() !== '');
  const expectedTitle = `# Agent Note：${meta.title}`;
  if (firstLine !== expectedTitle) errors.push(`${relative}: 标题行必须是 \`${expectedTitle}\``);
  const prose = text.replace(/```[\s\S]*?```/g, '');   // 代码块里的示例不算文件结构
  const sections = prose.split(/\r?\n/).filter(line => line.startsWith('## ')).map(line => line.trimEnd());
  if (sections[0] !== opener) errors.push(`${relative}: 正文第一个章节必须是 \`${opener}\`（现在是 ${sections[0] ?? '没有章节'}）`);
  for (const section of formatRules[lifecycle].required) if (!sections.includes(section)) errors.push(`${relative}: 缺少必需章节 \`${section}\``);
  for (const section of sections) if (formatRules[lifecycle].banned.includes(section)) errors.push(`${relative}: ${lifecycle} 记录不得出现提案期章节 \`${section}\``);

  const hasAlternatives = sections.includes('## 备选方案');
  const hasGrandfather = prose.split(/\r?\n/).includes(grandfatherComment);
  if (hasAlternatives && hasGrandfather) errors.push(`${relative}: 同时有 \`## 备选方案\` 与豁免注释，删掉注释`);
  if (!hasAlternatives && !hasGrandfather) errors.push(`${relative}: 缺少必需章节 \`## 备选方案\`（格式建立前创建、且无法从既有内容还原备选方案的记录，用豁免注释顶替）`);
  if (hasGrandfather && meta.created > formatAdopted) errors.push(`${relative}: 豁免注释只对 ${formatAdopted}（含）之前创建的记录有效`);
  return { errors, meta };
}

export async function collect() {
  const errors = [], records = [], documents = [];
  let categories = [];
  try {
    const config = JSON.parse(await readFile(path.join(root,'config.json'),'utf8'));
    if (config.version !== 1 || !Array.isArray(config.categories)) throw Error('expected version 1 and categories array');
    const ids = new Set();
    for (const c of config.categories) {
      if (!c || !/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(c.id ?? '') || ![c.name,c.scope].every(v => typeof v === 'string' && v.trim() && !/[\r\n]/.test(v)) || ids.has(c.id)) throw Error('categories require unique slug ids, names and scopes');
      ids.add(c.id);
    }
    categories = config.categories;
  } catch (e) { errors.push(`config.json: ${e.message}`); }
  const ids = categories.map(c => c.id);
  async function walk(dir) {
    for (const e of await readdir(dir,{withFileTypes:true})) {
      const full = path.join(dir,e.name), rel = path.relative(root,full).replaceAll('\\','/'), parts = rel.split('/');
      if (e.isDirectory()) {
        if (parts.length === 1 && !lifecycles.includes(e.name)) errors.push(`${rel}: 不是已知的生命周期目录`);
        if (parts.length === 2 && !ids.includes(e.name)) errors.push(`${rel}: 不是 config.json 里登记的项目分类`);
        if (parts.length > 2) errors.push(`${rel}: 记录目录不支持再嵌套`);
        await walk(full);
      } else if (e.isFile() && e.name.endsWith('.md')) documents.push(full);
    }
  }
  await walk(root);
  for (const full of documents.sort()) {
    const relative = path.relative(root,full).replaceAll('\\','/');
    // 本目录不设生成的集中索引：{生命周期}/{分类}/ 目录树就是清单，浏览目录或搜索仓库负责发现。
    // 根目录若又出现 INDEX.md，直接报错，避免索引生成那套被重新引入。
    if (relative === 'INDEX.md') { errors.push('INDEX.md: 本目录不生成集中索引（生命周期/分类目录树即清单），请删除该文件'); continue; }
    const text = await readFile(full,'utf8'), parts = relative.split('/');
    if (!['README.md','AGENTS.md'].includes(relative)) {
      const name = /^(\d{4}-\d{2}-\d{2})-[a-z0-9]+(?:-[a-z0-9]+)*\.md$/.exec(parts[2] ?? '');
      if (parts.length !== 3 || !lifecycles.includes(parts[0]) || !ids.includes(parts[1]) || !name) { errors.push(`${relative}: 路径必须是 {生命周期}/{分类}/YYYY-MM-DD-topic-slug.md`); continue; }
      const checked = checkRecord(relative, parts[0], text, name[1]);
      errors.push(...checked.errors);
      records.push({...checked.meta, category:parts[1], relative});
    }
    const body = text.replace(/```[\s\S]*?```/g,'').replace(/`[^`\n]+`/g,'');
    for (const link of body.matchAll(/\[[^\]]*\]\((<[^>]+>|[^\s)]+)\)/g)) {
      const target = link[1].replace(/^<|>$/g,'').split('#')[0];
      if (!target || /^[a-z][a-z\d+.-]*:/i.test(target)) continue;
      try {
        const dest = resolveTarget(path.dirname(full), decodeURIComponent(target));
        await access(dest);
      } catch { errors.push(`${relative}: 本地链接指向不存在的文件 ${target}`); }
    }
  }
  const names = new Set();
  for (const r of records) { const name = path.basename(r.relative); if (names.has(name)) errors.push(`${r.relative}: 记录文件名重复`); names.add(name); }
  return {records,categories,errors};
}
