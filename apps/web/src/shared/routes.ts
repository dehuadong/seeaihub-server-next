import { useEffect, useState } from 'react';

/// 管理端的工作区（侧栏一项）与地址解析的**唯一**来源。
///
/// 用 hash 而不是路径：产物是静态文件，部署时不必为前端配一条"所有路径都回 index.html"的规则。
///
/// 列表与详情**共用同一个工作区**：`#/accounts` 与 `#/accounts/{account_id}` 都算「账户」这一项，
/// 侧栏因此保持选中；详情多一段不透明标识。切换工作区丢掉那段标识（进列表），这是设计要的入口
/// （`docs/design/0011-console-information-architecture.md` §4.3–4.4.1）。
///
/// 金额与时间的展示规则不在这里——它们在 `shared/format.ts`，因为客户控制台也要用同一套。
export type Route =
  | 'models'
  | 'publish'
  | 'rates'
  | 'routing'
  | 'accounts'
  | 'customers'
  | 'diagnostics';

const ROUTES: Route[] = [
  'models',
  'publish',
  'rates',
  'routing',
  'accounts',
  'customers',
  'diagnostics',
];

/// 解析结果：工作区，加上账户／客户详情地址里的那段标识（列表页为 `null`）。
///
/// `not-found` 是**未知页面**：地址里的工作区不在上表之内。它不回落到模型目录——把敲错的地址
/// 显示成另一个页面，运营会以为自己看的是对的（`docs/design/0011` §4.4.1）。
export interface ConsoleLocation {
  page: Route | 'not-found';
  detailId: string | null;
}

function segment(value: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    // 地址里出现非法百分号编码时按原文处理：标识本来就不透明，解析失败不该把页面变成白的。
    return value;
  }
}

export function parseLocation(hash: string): ConsoleLocation {
  const value = hash.replace(/^#\/?/, '');
  // 空地址（例如刚打开根地址）落到模型目录：那是运营的第一站。
  if (!value) return { page: 'models', detailId: null };
  const parts = value.split('/');
  const head = parts[0] ?? '';
  if (!(ROUTES as string[]).includes(head)) return { page: 'not-found', detailId: null };
  // 已知工作区后面最多跟一段标识；多出来的段说明这不是我们的地址，照 §4.4.1 显示找不到，
  // 而不是把 `#/accounts/{id}/junk` 当成详情。
  if (parts.length > 2) return { page: 'not-found', detailId: null };
  const page = head as Route;
  const tail = parts[1] ?? '';
  if (!tail) return { page, detailId: null };
  // 只有账户与客户有详情地址。别的已知工作区后面再跟一段，同样是"不是我们的地址"——静默把那段
  // 当噪声丢掉，就是把敲错的地址显示成一个看似正确的页面。
  if (page === 'accounts' || page === 'customers') return { page, detailId: segment(tail) };
  return { page: 'not-found', detailId: null };
}

/// 账户详情地址（`#/` 之后的那一段）。标识编码后再进地址，页面侧再解回来。
export function accountPath(accountId: string): string {
  return `accounts/${encodeURIComponent(accountId)}`;
}

/// 客户详情地址。
export function customerPath(customerId: string): string {
  return `customers/${encodeURIComponent(customerId)}`;
}

/// 页面地址与浏览器历史同步的**唯一**来源（管理端用）。
///
/// `navigate` 收的是 `#/` 之后的那一段：工作区用 [`Route`]，详情用 [`accountPath`] 或
/// [`customerPath`] 拼出来。写 hash 会触发 `hashchange`，浏览器前进后退也走同一条路。
export function useHashRoute(): [ConsoleLocation, (path: string) => void] {
  const [location, setLocation] = useState<ConsoleLocation>(() =>
    parseLocation(window.location.hash),
  );

  useEffect(() => {
    const onChange = () => setLocation(parseLocation(window.location.hash));
    window.addEventListener('hashchange', onChange);
    return () => window.removeEventListener('hashchange', onChange);
  }, []);

  const navigate = (path: string) => {
    window.location.hash = `#/${path}`;
  };

  return [location, navigate];
}
