import { PORTAL_PATHS, PORTAL_ROUTES } from './paths';
import { rangeFromSearch } from './dates';

/// 登录回跳目标：受保护地址 + 认可的日期区间（`docs/specs/0004` §3、`docs/design/0016` §2）。
///
/// 只存在当前标签页 `sessionStorage` 的独立客户键里，认证 URL 不带 `returnTo` 参数。识别、保存、
/// 读取、消费、清除都在这里收口，客户页面与认证页不分别实现；读取时**重新校验**——同源存储里的
/// 原值不可信，缺失、损坏或读写失败都退化为无目标并继续显示页面。
const RETURN_TO_KEY = 'seeai.portal.returnTo';

/// 查询参数一旦涉及凭据就拒绝整个候选，不清洗成合法目标（设计 0016 §2）。
const CREDENTIAL_PARAM =
  /(token|session|api[-_]?key|password|passwd|pwd|secret|credential|authorization|auth|reset|otp|signature|(?:^|[-_])key$)/i;

interface StoredTarget {
  path: string;
  since?: string;
  until?: string;
}

function isProtectedPath(pathname: string): boolean {
  return PORTAL_ROUTES.some((route) => PORTAL_PATHS[route] === pathname);
}

/// 只认**恰好一对**有效的 `since`/`until`：重复日期参数、只给一个或不成形都不保留日期，只留路径。
function approvedRange(search: string): { since: string; until: string } | null {
  const params = new URLSearchParams(search);
  if (params.getAll('since').length !== 1 || params.getAll('until').length !== 1) return null;
  return rangeFromSearch(search);
}

/// 从受保护地址进入找回流程前捕获目标：以当前路径与日期区间覆盖旧目标；不是受保护地址、或查询
/// 参数含凭据时清掉旧目标且不保存（未知地址不捕获）。
export function captureReturnTo(pathname: string, search: string): void {
  clearReturnTo();
  if (!isProtectedPath(pathname)) return;
  const params = new URLSearchParams(search);
  for (const key of params.keys()) {
    if (CREDENTIAL_PARAM.test(key)) return;
  }
  const range = approvedRange(search);
  const target: StoredTarget = range
    ? { path: pathname, since: range.since, until: range.until }
    : { path: pathname };
  try {
    sessionStorage.setItem(RETURN_TO_KEY, JSON.stringify(target));
  } catch {
    // 存储读写失败退化为无目标，页面照常显示。
  }
}

/// 读回跳目标并重新校验，回可直接 `replace` 的地址；无、损坏或不合规时回 `null`。
export function readReturnTo(): string | null {
  let raw: string | null = null;
  try {
    raw = sessionStorage.getItem(RETURN_TO_KEY);
  } catch {
    return null;
  }
  if (!raw) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!parsed || typeof parsed !== 'object') return null;
  const target = parsed as Partial<StoredTarget>;
  if (typeof target.path !== 'string' || !isProtectedPath(target.path)) return null;
  if (typeof target.since !== 'string' || typeof target.until !== 'string') return target.path;
  const range = rangeFromSearch(
    `?since=${encodeURIComponent(target.since)}&until=${encodeURIComponent(target.until)}`,
  );
  if (!range) return target.path;
  return `${target.path}?since=${encodeURIComponent(range.since)}&until=${encodeURIComponent(range.until)}`;
}

/// 消费目标：成功登录或注册时用它取去向，并把记录清掉。
export function consumeReturnTo(): string | null {
  const url = readReturnTo();
  clearReturnTo();
  return url;
}

export function clearReturnTo(): void {
  try {
    sessionStorage.removeItem(RETURN_TO_KEY);
  } catch {
    // 无记录可清。
  }
}
