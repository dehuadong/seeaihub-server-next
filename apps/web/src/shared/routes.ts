import { useEffect, useState } from 'react';

/// 页面标识与地址栏同步的**唯一**来源（管理端用）。
///
/// 用 hash 而不是路径：产物是静态文件，部署时不必为前端配一条"所有路径都回 index.html"的规则。
///
/// 金额与时间的展示规则不在这里——它们在 `shared/format.ts`，因为客户控制台也要用同一套。
export type Route = 'models' | 'publish' | 'rates' | 'routing' | 'accounts' | 'customers' | 'diagnostics';

const ROUTES: Route[] = [
  'models',
  'publish',
  'rates',
  'routing',
  'accounts',
  'customers',
  'diagnostics',
];

function parse(hash: string): Route {
  const value = hash.replace(/^#\/?/, '');
  return (ROUTES as string[]).includes(value) ? (value as Route) : 'models';
}

export function useHashRoute(): [Route, (route: Route) => void] {
  const [route, setRoute] = useState<Route>(() => parse(window.location.hash));

  useEffect(() => {
    const onChange = () => setRoute(parse(window.location.hash));
    window.addEventListener('hashchange', onChange);
    return () => window.removeEventListener('hashchange', onChange);
  }, []);

  const navigate = (next: Route) => {
    window.location.hash = `#/${next}`;
  };

  return [route, navigate];
}
