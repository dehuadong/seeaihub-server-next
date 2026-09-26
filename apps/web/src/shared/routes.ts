import { useEffect, useState } from 'react';

/// 页面标识与地址栏同步的**唯一**来源。
///
/// 用 hash 而不是路径：产物是静态文件，部署时不必为前端配一条"所有路径都回 index.html"的规则。
export type Route =
  | 'models'
  | 'publish'
  | 'rates'
  | 'routing'
  | 'accounts'
  | 'diagnostics';

const ROUTES: Route[] = ['models', 'publish', 'rates', 'routing', 'accounts', 'diagnostics'];

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

/// 把微单位人民币换算成展示用的元。**只在展示层做**：传输与判断都保持整数微单位。
export function yuan(micros: number): string {
  return (micros / 1_000_000).toFixed(6).replace(/0+$/, '').replace(/\.$/, '');
}

/// 时间戳按本地时区展示；空值原样显示为 `—`，不猜。
export function when(value: string | null | undefined): string {
  if (!value) return '—';
  const at = new Date(value);
  return Number.isNaN(at.getTime()) ? value : at.toLocaleString();
}
