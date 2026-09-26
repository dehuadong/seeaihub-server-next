import { createContext, useCallback, useContext, useMemo, useState, type ReactNode } from 'react';

/// 管理端会话的存放位置。
///
/// 服务端的 `identity.*_sessions` 每次都读库判有效性（吊销与过期立刻生效），所以这里的令牌对客户端
/// 来说是**不透明**的：拿到就用，过期了会被拒，届时清掉它回到登录页。
///
/// 键名带 `console` 前缀：客户控制台将来用同一个浏览器时各存各的，不会串。
const TOKEN_KEY = 'seeai.console.session';

interface AdminSession {
  /// 会话令牌；`null` 表示未登录。
  token: string | null;
  /// 当前登录的管理员邮箱（登录响应给的，用于界面显示）。
  email: string | null;
  signIn: (token: string, email: string) => void;
  signOut: () => void;
}

const AdminSessionContext = createContext<AdminSession | null>(null);

export function AdminSessionProvider({ children }: { children: ReactNode }) {
  const [token, setToken] = useState<string | null>(() => sessionStorage.getItem(TOKEN_KEY));
  const [email, setEmail] = useState<string | null>(() =>
    sessionStorage.getItem(`${TOKEN_KEY}.email`),
  );

  const signIn = useCallback((next: string, nextEmail: string) => {
    sessionStorage.setItem(TOKEN_KEY, next);
    sessionStorage.setItem(`${TOKEN_KEY}.email`, nextEmail);
    setToken(next);
    setEmail(nextEmail);
  }, []);

  const signOut = useCallback(() => {
    sessionStorage.removeItem(TOKEN_KEY);
    sessionStorage.removeItem(`${TOKEN_KEY}.email`);
    setToken(null);
    setEmail(null);
  }, []);

  const value = useMemo(() => ({ token, email, signIn, signOut }), [token, email, signIn, signOut]);
  return <AdminSessionContext.Provider value={value}>{children}</AdminSessionContext.Provider>;
}

export function useAdminSession(): AdminSession {
  const session = useContext(AdminSessionContext);
  if (!session) throw new Error('useAdminSession 必须在 AdminSessionProvider 内使用');
  return session;
}
