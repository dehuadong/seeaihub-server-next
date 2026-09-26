import { useState } from 'react';
import { apiFetch } from '../shared/api';
import { CustomerClient } from './client';
import { useCustomerSession } from './session';
import { useLoadable } from '../shared/ui';

/// 登录 / 注册页。客户用邮箱 + 口令进来；没有账号就注册一个（Spec C1–C3）。
///
/// 未登录时只调公开端点：`/health`、注册、登录。账户数据在登录成功之前**一个请求都不发**。
export function AuthPage() {
  const { signIn } = useCustomerSession();
  const [mode, setMode] = useState<'login' | 'register'>('login');
  const [email, setEmail] = useState('');
  const [password, setPassword] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const health = useLoadable(async () => {
    const response = await fetch('/health');
    return { ok: response.ok, status: response.status };
  }, []);

  async function submit() {
    setBusy(true);
    setError(null);
    try {
      const path = mode === 'register' ? '/v1/customers' : '/v1/customer/sessions';
      const session = await apiFetch<{ token: string; email: string; account_id: string }>(
        path,
        () => null,
        { method: 'POST', body: { email: email.trim(), password }, admin: false },
      );
      signIn(session);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  }

  const canSubmit = email.trim().length > 0 && password.length > 0;

  return (
    <div className="main">
      <h2>seeai 控制台</h2>
      <p className="muted">
        用邮箱与口令{mode === 'register' ? '注册' : '登录'}。口令只在这一次请求里用，服务端存的是
        argon2 哈希；会话令牌只存在这个标签页的 sessionStorage 里。
      </p>
      <div className="panel">
        <div className="row">
          <button
            data-testid="portal-mode-login"
            type="button"
            className={mode === 'login' ? 'active' : ''}
            onClick={() => setMode('login')}
          >
            登录
          </button>
          <button
            data-testid="portal-mode-register"
            type="button"
            className={mode === 'register' ? 'active' : ''}
            onClick={() => setMode('register')}
          >
            注册
          </button>
        </div>
        <div className="row" style={{ marginTop: 10 }}>
          <label className="field">
            <span>邮箱</span>
            <input
              data-testid="portal-email"
              value={email}
              onChange={(event) => setEmail(event.target.value)}
              placeholder="you@example.com"
              size={26}
              autoComplete="username"
            />
          </label>
          <label className="field">
            <span>口令（至少 8 个字符）</span>
            <input
              data-testid="portal-password"
              type="password"
              value={password}
              onChange={(event) => setPassword(event.target.value)}
              size={26}
              autoComplete={mode === 'register' ? 'new-password' : 'current-password'}
              onKeyDown={(event) => {
                if (event.key === 'Enter' && canSubmit) void submit();
              }}
            />
          </label>
          <button
            data-testid="portal-submit"
            type="button"
            disabled={busy || !canSubmit}
            onClick={() => void submit()}
          >
            {busy ? '提交中…' : mode === 'register' ? '注册并进入' : '登录'}
          </button>
        </div>
        {error ? <p className="error">{error}</p> : null}
        <p className={health.data?.ok ? 'muted' : 'error'}>
          服务探活：
          {health.loading ? '检查中…' : health.data?.ok ? '正常' : `不健康（HTTP ${health.data?.status ?? '无响应'}）`}
        </p>
      </div>
      <div className="panel">
        <h3>关于充值</h3>
        <p className="muted">
          平台目前**没有**在线支付：充值由运营在后台完成，这里只**展示**充值记录与余额。
          忘了口令也**不能自助重置**——请找运营，由他们签发一枚一次性重置令牌给你，你再用它设置新口令。
        </p>
      </div>
      <ResetPanel />
    </div>
  );
}

/// 凭运营转交的令牌设置新口令（Spec C12 的对客一侧）。
///
/// 平台上没有"提交邮箱就收到重置链接"这条路——不发邮件、不做邮箱验证，那种入口等于"知道邮箱就能
/// 接管账户"。所以这里只收**运营转交过来的令牌**。
function ResetPanel() {
  const client = new CustomerClient(() => null);
  const [token, setToken] = useState('');
  const [password, setPassword] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);

  return (
    <div className="panel">
      <h3>用重置令牌设置新口令</h3>
      <p className="muted">
        口令忘了就找运营要一枚**一次性重置令牌**（平台不发邮件）。拿到之后填在这里，设置新口令即可登录。
        重置成功会使此前所有登录会话失效。
      </p>
      <div className="row">
        <label className="field">
          <span>重置令牌</span>
          <input
            data-testid="portal-reset-token"
            value={token}
            onChange={(event) => setToken(event.target.value)}
            size={44}
          />
        </label>
        <label className="field">
          <span>新口令（至少 8 个字符）</span>
          <input
            data-testid="portal-reset-password"
            type="password"
            value={password}
            onChange={(event) => setPassword(event.target.value)}
            size={22}
            autoComplete="new-password"
          />
        </label>
        <button
          data-testid="portal-reset-submit"
          type="button"
          disabled={busy || !token.trim() || !password}
          onClick={async () => {
            setBusy(true);
            setError(null);
            setDone(false);
            try {
              await client.redeemPasswordReset(token.trim(), password);
              setDone(true);
              setToken('');
              setPassword('');
            } catch (failure) {
              setError(failure instanceof Error ? failure.message : String(failure));
            } finally {
              setBusy(false);
            }
          }}
        >
          {busy ? '提交中…' : '设置新口令'}
        </button>
      </div>
      {error ? <p className="error">{error}</p> : null}
      {done ? <p style={{ color: 'var(--ok)' }}>口令已重置，回到上面用新口令登录。</p> : null}
    </div>
  );
}
