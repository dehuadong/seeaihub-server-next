import { useCallback, useMemo, useState } from 'react';
import { AdminClient } from './client';
import { useAdminSession } from './session';
import { useHashRoute, type Route } from './routes';
import { useLoadable } from './ui';
import { AccountsPage } from './pages/Accounts';
import { DiagnosticsPage } from './pages/Diagnostics';
import { ModelsPage } from './pages/Models';
import { PublishPage } from './pages/Publish';
import { RatesPage } from './pages/Rates';
import { RoutingPage } from './pages/Routing';

const NAV: { route: Route; label: string }[] = [
  { route: 'models', label: '网关模型' },
  { route: 'publish', label: '发布修订' },
  { route: 'rates', label: '折算率' },
  { route: 'routing', label: '路由策略' },
  { route: 'accounts', label: '账户与密钥' },
  { route: 'diagnostics', label: '对账与诊断' },
];

export function App() {
  const { token, setToken } = useAdminSession();
  const [route, navigate] = useHashRoute();
  const [draft, setDraft] = useState('');

  /// 令牌的取值函数传给客户端：它每次请求时读当前值，所以换令牌不必重建客户端。
  const tokenGetter = useCallback(() => token, [token]);
  const client = useMemo(() => new AdminClient(tokenGetter), [tokenGetter]);

  const health = useLoadable(async () => {
    const response = await fetch('/health');
    return { ok: response.ok, status: response.status };
  }, []);

  if (!token) {
    return (
      <div className="main">
        <h2>seeai 运营后台</h2>
        <p className="muted">
          填入管理员令牌（部署里那个 <code>ADMIN_TOKEN</code>）。它只存在这个标签页的 sessionStorage 里，
          关掉标签页即清除，也不会写进构建产物。
        </p>
        <div className="panel">
          <div className="row">
            <input
              type="password"
              value={draft}
              placeholder="ADMIN_TOKEN"
              size={48}
              onChange={(event) => setDraft(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === 'Enter' && draft.trim()) setToken(draft.trim());
              }}
            />
            <button type="button" disabled={!draft.trim()} onClick={() => setToken(draft.trim())}>
              进入
            </button>
          </div>
          <p className={health.data?.ok ? 'muted' : 'error'}>
            API 探活：{health.loading ? '检查中…' : health.data?.ok ? '健康' : `不健康（HTTP ${health.data?.status ?? '无响应'}）`}
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className="layout">
      <aside className="sidebar">
        <h1>seeai 运营后台</h1>
        <p>管理 API 的界面；对客面见 /v1/models</p>
        <nav className="nav">
          {NAV.map((item) => (
            <button
              key={item.route}
              type="button"
              className={route === item.route ? 'active' : ''}
              onClick={() => navigate(item.route)}
            >
              {item.label}
            </button>
          ))}
        </nav>
        <p style={{ marginTop: 16 }}>
          <button type="button" onClick={() => setToken(null)}>
            清除令牌
          </button>
        </p>
      </aside>
      {route === 'models' ? <ModelsPage client={client} /> : null}
      {route === 'publish' ? <PublishPage client={client} /> : null}
      {route === 'rates' ? <RatesPage client={client} /> : null}
      {route === 'routing' ? <RoutingPage client={client} /> : null}
      {route === 'accounts' ? <AccountsPage client={client} /> : null}
      {route === 'diagnostics' ? <DiagnosticsPage client={client} /> : null}
    </div>
  );
}
