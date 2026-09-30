import { useCallback, useEffect, useState } from 'react';
import { PORTAL_PATHS, PORTAL_ROUTES, type PortalRoute } from './paths';

/// 客户页面的解析结果：五条已知地址之一，或客户侧 404。
export type PortalLocation = PortalRoute | 'not-found';

function normalize(pathname: string): string {
  return pathname.length > 1 ? pathname.replace(/\/+$/, '') : pathname;
}

function routeFor(pathname: string): PortalLocation {
  const path = normalize(pathname);
  for (const route of PORTAL_ROUTES) {
    if (PORTAL_PATHS[route] === path) return route;
  }
  // 开发服务把入口挂在 `/portal.html`：它也算概览，免得本机打开入口就落到 404。
  if (path === '/portal.html') return 'overview';
  return 'not-found';
}

export function portalPath(route: PortalRoute): string {
  return PORTAL_PATHS[route];
}

/// 页面地址与浏览器历史同步的**唯一**来源。
///
/// 用 **path + History API** 而不是管理端那套 hash：这五条是产品给客户的固定地址，刷新、前进后退与
/// 直接打开都要落回同一页；静态产物由 API 按主机名回退入口 HTML（`docs/design/0010` §4.5），所以
/// 前端不必自己配深链。**地址里只放页面标识**，会话令牌与密钥明文都不进 URL（Spec D2、D5）。
export function usePortalRoute(): {
  route: PortalLocation;
  pathname: string;
  search: string;
  navigate: (to: string) => void;
} {
  const [location, setLocation] = useState(() => ({
    pathname: window.location.pathname,
    search: window.location.search,
  }));

  useEffect(() => {
    const sync = () =>
      setLocation({ pathname: window.location.pathname, search: window.location.search });
    window.addEventListener('popstate', sync);
    return () => window.removeEventListener('popstate', sync);
  }, []);

  const navigate = useCallback((to: string) => {
    if (to === `${window.location.pathname}${window.location.search}`) return;
    window.history.pushState(null, '', to);
    setLocation({ pathname: window.location.pathname, search: window.location.search });
  }, []);

  return {
    route: routeFor(location.pathname),
    pathname: location.pathname,
    search: location.search,
    navigate,
  };
}
