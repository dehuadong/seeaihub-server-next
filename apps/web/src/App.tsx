import { useCallback, useMemo, useState } from 'react';
import { AdminClient } from './client';
import { useAdminSession } from './session';
import { useHashRoute, type Route } from './routes';
import { AccountsPage } from './pages/Accounts';
import { DiagnosticsPage } from './pages/Diagnostics';
import { ChangePasswordPanel, LoginPage } from './pages/Login';
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
  const { token, email, signOut } = useAdminSession();
  const [route, navigate] = useHashRoute();
  const [showPassword, setShowPassword] = useState(false);

  /// 令牌的取值函数传给客户端：它每次请求时读当前值，所以换会话不必重建客户端。
  const tokenGetter = useCallback(() => token, [token]);
  const client = useMemo(() => new AdminClient(tokenGetter), [tokenGetter]);

  // **路由守卫**：没登录就只渲染登录页。六个页面的组件在登录之前根本不挂载，因此也不会发任何
  // 管理 API 取数请求（Spec M7）——会话过期或被吊销时，返回来的 403 会把我们带回这里。
  if (!token) return <LoginPage />;

  return (
    <div className="layout">
      <aside className="sidebar">
        <h1>seeai 运营后台</h1>
        <p>{email}</p>
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
          <button type="button" onClick={() => setShowPassword((value) => !value)}>
            {showPassword ? '收起改口令' : '改口令'}
          </button>{' '}
          <button type="button" onClick={signOut}>
            退出登录
          </button>
        </p>
      </aside>
      <div>
        {showPassword ? <ChangePasswordPanel /> : null}
        {route === 'models' ? <ModelsPage client={client} /> : null}
        {route === 'publish' ? <PublishPage client={client} /> : null}
        {route === 'rates' ? <RatesPage client={client} /> : null}
        {route === 'routing' ? <RoutingPage client={client} /> : null}
        {route === 'accounts' ? <AccountsPage client={client} /> : null}
        {route === 'diagnostics' ? <DiagnosticsPage client={client} /> : null}
      </div>
    </div>
  );
}
