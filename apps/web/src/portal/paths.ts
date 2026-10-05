/// 客户控制台的地址定义：五个受保护页面（`docs/design/0014` §1）与三个公开认证页面
/// （`docs/specs/0004` §2）。这里**不引 React**：Vite 开发服务在 `vite.config.ts` 里读它，把无扩展名
/// 的深链回退到 `portal.html`；生产那条由 API 的静态兜底做（`apps/api/src/main.rs` 的 `serve_from`）。
/// 两处落回同一组地址，地址改了这里也改。
export const PORTAL_PATHS = {
  overview: '/',
  usage: '/usage',
  billing: '/billing',
  keys: '/keys',
  settings: '/settings',
} as const;

/// 公开认证页面：不需要会话即可打开，认证地址本身也参与开发与生产的深链回退。
export const AUTH_PATHS = {
  login: '/login',
  forgotPassword: '/forgot-password',
  resetPassword: '/reset-password',
} as const;

export type PortalRoute = keyof typeof PORTAL_PATHS;
export type AuthRoute = keyof typeof AUTH_PATHS;

/// 由两张地址表派生：新增一页只改表，解析与开发深链自动跟上（两份手抄会漂移）。
export const PORTAL_ROUTES = Object.keys(PORTAL_PATHS) as PortalRoute[];
export const AUTH_ROUTES = Object.keys(AUTH_PATHS) as AuthRoute[];

/// 开发服务要回退成 `portal.html` 的地址。根也在内：Vite 没有 `index.html`，不拦它本机连概览都打不开。
export const PORTAL_DEEP_LINKS = new Set<string>([
  ...PORTAL_ROUTES.map((route) => PORTAL_PATHS[route]),
  ...AUTH_ROUTES.map((route) => AUTH_PATHS[route]),
]);
