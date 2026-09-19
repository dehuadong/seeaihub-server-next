import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { root, collect, renderIndex } from './lib.mjs';

const { records, categories, errors } = await collect();
const index = await readFile(path.join(root, 'INDEX.md'), 'utf8').catch(() => null);
if (index !== renderIndex(records, categories)) errors.push('INDEX.md: missing or stale; run node scripts/decisions/update-index.mjs');
if (errors.length) {
  console.error(errors.join('\n'));
  process.exitCode = 1;
} else console.log(`Decision checks passed: ${records.length} records. Approval evidence and factual accuracy require review.`);
