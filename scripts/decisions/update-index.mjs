import { writeFile } from 'node:fs/promises';
import path from 'node:path';
import { root, collect, renderIndex } from './lib.mjs';

const { records, categories, errors } = await collect({ ignoreIndex: true });
if (errors.length) {
  console.error(errors.join('\n'));
  process.exitCode = 1;
} else {
  await writeFile(path.join(root, 'INDEX.md'), renderIndex(records, categories), 'utf8');
  console.log(`Updated index: ${records.length} decisions.`);
}
