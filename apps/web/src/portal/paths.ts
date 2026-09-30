/// 客户控制台的五条页面地址（`docs/design/0014` §1）。这里**不引 React**：Vite 开发服务在
/// `vite.config.ts` 里读它，把无扩展名的深链回退到 `portal.html`；生产那条由 API 的静态兜底做
/// （`apps/api/src/main.rs` 的 `serve_from`）。两处落回同一组地址，地址改了这里也改。
export const PORTAL_PATHS = {
  overview: '/',
  usage: '/usage',
  billing: '/billing',
  keys: '/keys',
  settings: '/settings',
} as const;

export type PortalRoute = keyof typeof PORTAL_PATHS;

/// 由 `PORTAL_PATHS` 派生：新增一页只改上面那张表，解析与开发深链自动跟上（两份手抄会漂移）。
export const PORTAL_ROUTES = Object.keys(PORTAL_PATHS) as PortalRoute[];

/// 开发服务要回退成 `portal.html` 的地址。根也在内：Vite 没有 `index.html`，不拦它本机连概览都打不开。
export const PORTAL_DEEP_LINKS = new Set<string>(PORTAL_ROUTES.map((route) => PORTAL_PATHS[route]));
