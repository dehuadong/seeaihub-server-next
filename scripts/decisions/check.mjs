import { collect } from './lib.mjs';

const { records, errors } = await collect();

if (errors.length) {
  console.error(`Agent Note 检查未通过，共 ${errors.length} 项：`);
  for (const error of errors) console.error(`  - ${error}`);
  process.exitCode = 1;
} else {
  console.log(`Agent Note 检查通过：${records.length} 条记录。批准是否属实、交付是否充分、备选方案与替代关系是否属实，仍由审阅判断。`);
}
