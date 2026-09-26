import { useState } from 'react';
import { apiFetch } from '../../shared/api';
import { useAdminSession } from '../session';
import { useLoadable } from '../../shared/ui';

/// 登录页。管理员用**邮箱 + 口令**换一条会话；拿不到就不进后台。
///
/// 这是 Spec M7 的落点：**未认证时一个管理 API 请求都不发**。所以这里只调 `/health`（公开）与
/// 登录端点，六个页面的取数在登录成功之前根本不会挂载。
export function LoginPage() {
  const { signIn } = useAdminSession();
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
      // 登录端点不需要凭据：用 `admin: false` 明确说明这一点。
      const body = await apiFetch<{ token: string; email: string }>(
        '/api/v1/admin/sessions',
        () => null,
        { method: 'POST', body: { email: email.trim(), password }, admin: false },
      );
      signIn(body.token, body.email);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="main">
      <h2>seeai 运营后台</h2>
      <p className="muted">
        用管理员邮箱与口令登录。口令只在这一次请求里用，服务端存的是 argon2 哈希；会话令牌只存在这个
        标签页的 sessionStorage 里，关掉标签页即失效。
      </p>
      <div className="panel">
        <div className="row">
          <label className="field">
            <span>邮箱</span>
            <input
              value={email}
              onChange={(event) => setEmail(event.target.value)}
              placeholder="ops@example.com"
              size={26}
              autoComplete="username"
            />
          </label>
          <label className="field">
            <span>口令</span>
            <input
              type="password"
              value={password}
              onChange={(event) => setPassword(event.target.value)}
              size={26}
              autoComplete="current-password"
              onKeyDown={(event) => {
                if (event.key === 'Enter' && email.trim() && password) void submit();
              }}
            />
          </label>
          <button
            type="button"
            disabled={busy || !email.trim() || !password}
            onClick={() => void submit()}
          >
            {busy ? '登录中…' : '登录'}
          </button>
        </div>
        {error ? <p className="error">{error}</p> : null}
        <p className={health.data?.ok ? 'muted' : 'error'}>
          API 探活：
          {health.loading ? '检查中…' : health.data?.ok ? '健康' : `不健康（HTTP ${health.data?.status ?? '无响应'}）`}
        </p>
        <p className="muted">
          还没有账号？运维在部署时用环境变量 <code>ADMIN_EMAIL</code> / <code>ADMIN_PASSWORD</code>{' '}
          引导出第一个管理员；口令只在账号不存在时写入，之后改口令请在这里登录后用「改口令」。
        </p>
      </div>
    </div>
  );
}

/// 改自己的口令。成功后**该管理员的全部会话都失效**（含当前这条），所以这里只能回登录页。
export function ChangePasswordPanel() {
  const { email, signOut } = useAdminSession();
  const [current, setCurrent] = useState('');
  const [next, setNext] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);

  async function submit() {
    setBusy(true);
    setError(null);
    try {
      await apiFetch('/api/v1/admin/password', () => sessionStorage.getItem('seeai.console.session'), {
        method: 'PUT',
        body: { current_password: current, new_password: next },
      });
      setDone(true);
      setCurrent('');
      setNext('');
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="panel">
      <h3>改口令</h3>
      <p className="muted">当前登录：{email}</p>
      <div className="row">
        <label className="field">
          <span>当前口令</span>
          <input
            type="password"
            value={current}
            onChange={(event) => setCurrent(event.target.value)}
            size={22}
            autoComplete="current-password"
          />
        </label>
        <label className="field">
          <span>新口令（至少 8 个字符）</span>
          <input
            type="password"
            value={next}
            onChange={(event) => setNext(event.target.value)}
            size={22}
            autoComplete="new-password"
          />
        </label>
        <button type="button" disabled={busy || !current || !next} onClick={() => void submit()}>
          {busy ? '提交中…' : '改口令'}
        </button>
      </div>
      {error ? <p className="error">{error}</p> : null}
      {done ? (
        <p className="muted">
          口令已改。**该管理员的全部会话都已失效**，请用新口令重新登录。
          <button type="button" style={{ marginLeft: 8 }} onClick={signOut}>
            回登录页
          </button>
        </p>
      ) : null}
    </div>
  );
}
