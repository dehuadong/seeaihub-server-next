import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { root, rootDir, collect, renderIndex } from './lib.mjs';

const { records, categories, errors } = await collect();
const index = await readFile(path.join(root, 'INDEX.md'), 'utf8').catch(() => null);
if (index !== renderIndex(records, categories)) errors.push('INDEX.md: missing or stale; run node scripts/decisions/update-index.mjs');

// 本地新增：「新工件归属」表只写位置与规则，变更/历史/状态不写进该表。
// 变更归提交历史与 Agent Notes，历史位置归「历史工件与新旧衔接」表。
const registry = await readFile(path.join(rootDir, 'docs/agents/artifacts.md'), 'utf8').catch(() => null);
if (registry) {
  const section = registry.split(/^## /m).find(part => part.startsWith('新工件归属'));
  const forbidden = [
    [/\d{4}-\d{2}-\d{2}/, '具体日期'],
    [/提交\s*`/, '提交号'],
    [/已(移除|删除|修补|更新|迁移|改为|废弃)/, '变更说明'],
  ];
  for (const [pattern, label] of forbidden) {
    if (section && pattern.test(section)) {
      errors.push(`docs/agents/artifacts.md：「新工件归属」表里出现${label}（${pattern}）——该表只写位置与规则，变更与历史写进提交历史、Agent Notes 或下方「历史工件与新旧衔接」表`);
    }
  }
}

if (errors.length) {
  console.error(errors.join('\n'));
  process.exitCode = 1;
} else console.log(`Decision checks passed: ${records.length} records. Approval evidence and factual accuracy require review.`);
