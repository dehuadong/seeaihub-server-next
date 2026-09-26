import { createContext, useCallback, useContext, useMemo, useState, type ReactNode } from 'react';

/// 管理员凭证的存放位置。
///
/// 它是**运维自己填的一个共享令牌**（服务端只有 `ADMIN_TOKEN` 这一种管理员身份，见
/// `apps/api/src/main.rs` 的 `require_admin`）。控制台不引入登录态：令牌只存在这个标签页的
/// `sessionStorage` 里，关掉标签页就没了，也**不写进任何请求日志或构建产物**。
const TOKEN_KEY = 'seeai.adminToken';

interface AdminSession {
  token: string | null;
  setToken: (value: string | null) => void;
}

const AdminSessionContext = createContext<AdminSession | null>(null);

export function AdminSessionProvider({ children }: { children: ReactNode }) {
  const [token, setTokenState] = useState<string | null>(() => sessionStorage.getItem(TOKEN_KEY));

  const setToken = useCallback((value: string | null) => {
    if (value) sessionStorage.setItem(TOKEN_KEY, value);
    else sessionStorage.removeItem(TOKEN_KEY);
    setTokenState(value);
  }, []);

  const value = useMemo(() => ({ token, setToken }), [token, setToken]);
  return <AdminSessionContext.Provider value={value}>{children}</AdminSessionContext.Provider>;
}

export function useAdminSession(): AdminSession {
  const session = useContext(AdminSessionContext);
  if (!session) throw new Error('useAdminSession 必须在 AdminSessionProvider 内使用');
  return session;
}
