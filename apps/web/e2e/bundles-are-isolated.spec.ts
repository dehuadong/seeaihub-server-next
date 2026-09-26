import { expect, test } from '@playwright/test';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { violations } from './check-bundle-isolation.mjs';

/// 两份产物**互不引用**（Spec V-D6 / D4）。判据在 `check-bundle-isolation.mjs` 里，这里只断言"没有违规"。
///
/// 判据本身经过**变异验证**：往管理端产物里注入一个 `/v1/customer/` 标记之后它会报出违规（见该脚本的
/// 注释与提交信息）——所以这条断言不是恒真的。
const dist = resolve(dirname(fileURLToPath(import.meta.url)), '..', 'dist');

test('两份产物互不引用', () => {
  expect(violations(dist)).toEqual([]);
});
