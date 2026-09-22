import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { rootDir, collect } from './lib.mjs';

const { records, errors } = await collect();

// 本地新增：注册表只写位置与边界——「历史工件与新旧衔接」一节是历史与迁移状态的唯一落点，
// 其余部分不写具体日期、提交号与「已移除/已更新」这类变更说明。
const registry = await readFile(path.join(rootDir, 'docs/agents/artifacts.md'), 'utf8').catch(() => null);
if (registry) {
  const parts = registry.split(/^## /m);
  const history = parts.find(part => part.startsWith('历史工件与新旧衔接'));
  const rest = parts.filter(part => part !== history).join('\n## ');
  const forbidden = [
    [/\d{4}-\d{2}-\d{2}/, '具体日期'],
    [/提交\s*`/, '提交号'],
    [/已(移除|删除|修补|更新|迁移|改为|废弃)/, '变更说明'],
  ];
  for (const [pattern, label] of forbidden) {
    if (pattern.test(rest)) {
      errors.push(`docs/agents/artifacts.md：注册表里出现${label}（${pattern}）——变更与历史归提交历史、Agent Notes 与「历史工件与新旧衔接」表，其余部分只写位置与边界`);
    }
  }
}

if (errors.length) {
  console.error(`Agent Note 检查未通过，共 ${errors.length} 项：`);
  for (const error of errors) console.error(`  - ${error}`);
  process.exitCode = 1;
} else {
  console.log(`Agent Note 检查通过：${records.length} 条记录。批准是否属实、交付是否充分、备选方案与替代关系是否属实，仍由审阅判断。`);
}
