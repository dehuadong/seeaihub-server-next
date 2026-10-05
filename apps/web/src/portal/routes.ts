import { useCallback, useSyncExternalStore } from 'react';
import {
  AUTH_PATHS,
  AUTH_ROUTES,
  PORTAL_PATHS,
  PORTAL_ROUTES,
  type AuthRoute,
  type PortalRoute,
} from './paths';

/// 客户地址的解析结果：五条受保护页面、三个公开认证页面，或客户侧 404。
export type PortalLocation = PortalRoute | AuthRoute | 'not-found';

function normalize(pathname: string): string {
  return pathname.length > 1 ? pathname.replace(/\/+$/, '') : pathname;
}

function routeFor(pathname: string): PortalLocation {
  const path = normalize(pathname);
  for (const route of PORTAL_ROUTES) {
    if (PORTAL_PATHS[route] === path) return route;
  }
  for (const route of AUTH_ROUTES) {
    if (AUTH_PATHS[route] === path) return route;
  }
  // 开发服务把入口挂在 `/portal.html`：它也算概览，免得本机打开入口就落到 404。
  if (path === '/portal.html') return 'overview';
  return 'not-found';
}

/// 公开认证页面的地址：未登录时按地址挂载对应页面，不等同于受保护页面。
export function isAuthRoute(route: PortalLocation): route is AuthRoute {
  return (AUTH_ROUTES as PortalLocation[]).includes(route);
}

export function portalPath(route: PortalRoute): string {
  return PORTAL_PATHS[route];
}

export function authPath(route: AuthRoute): string {
  return AUTH_PATHS[route];
}

/// 地址的**唯一**订阅点：History API 写入与 popstate 都从这里通知所有消费者。
///
/// 每个 `usePortalRoute` 各持一份局部 state 会漏掉别处发起的 `pushState`/`replaceState`
/// （它们不触发 `popstate`）：公开登录页替换到回跳目标后，外壳仍停在登录地址，守卫会把它抢回
/// 概览。共享一份快照后，任何一处导航都对所有消费者立即生效。
interface Location {
  pathname: string;
  search: string;
}

function readLocation(): Location {
  return { pathname: window.location.pathname, search: window.location.search };
}

let snapshot: Location = readLocation();
const listeners = new Set<() => void>();

function emit(): void {
  snapshot = readLocation();
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

if (typeof window !== 'undefined') {
  window.addEventListener('popstate', emit);
}

function go(kind: 'push' | 'replace', to: string): void {
  if (to === `${window.location.pathname}${window.location.search}`) return;
  if (kind === 'push') window.history.pushState(null, '', to);
  else window.history.replaceState(null, '', to);
  emit();
}

/// 页面地址与浏览器历史同步的**唯一**来源。
///
/// 用 **path + History API** 而不是管理端那套 hash：这些是产品给客户的固定地址，刷新、前进后退与
/// 直接打开都要落回同一页；静态产物由 API 按主机名回退入口 HTML（`docs/design/0010` §4.5），所以
/// 前端不必自己配深链。**地址里只放页面标识与非敏感筛选**，会话令牌与密钥明文都不进 URL（Spec D2、D5）。
export function usePortalRoute(): {
  route: PortalLocation;
  pathname: string;
  search: string;
  navigate: (to: string) => void;
  replace: (to: string) => void;
} {
  const location = useSyncExternalStore(
    subscribe,
    () => snapshot,
    () => snapshot,
  );

  const navigate = useCallback((to: string) => go('push', to), []);
  /// 改写当前这一条历史（不新增）：页面自己补上缺省筛选条件、或认证页替换到回跳目标时用它。
  const replace = useCallback((to: string) => go('replace', to), []);

  return {
    route: routeFor(location.pathname),
    pathname: location.pathname,
    search: location.search,
    navigate,
    replace,
  };
}
