import { createContext, useCallback, useContext, useMemo, useState, type ReactNode } from 'react';

/// 客户会话的存放位置。
///
/// 键名带 `portal` 前缀，与管理端各存各的：同一个浏览器同时开两个控制台也不会串。
/// 令牌对客户端是不透明的——服务端每次都读库判有效性，过期了会被拒，届时回到登录/注册页。
const TOKEN_KEY = 'seeai.portal.session';

interface CustomerSessionState {
  token: string | null;
  email: string | null;
  accountId: string | null;
  signIn: (session: { token: string; email: string; account_id: string }) => void;
  signOut: () => void;
}

const CustomerSessionContext = createContext<CustomerSessionState | null>(null);

export function CustomerSessionProvider({ children }: { children: ReactNode }) {
  const [token, setToken] = useState<string | null>(() => sessionStorage.getItem(TOKEN_KEY));
  const [email, setEmail] = useState<string | null>(() =>
    sessionStorage.getItem(`${TOKEN_KEY}.email`),
  );
  const [accountId, setAccountId] = useState<string | null>(() =>
    sessionStorage.getItem(`${TOKEN_KEY}.account`),
  );

  const signIn = useCallback(
    (session: { token: string; email: string; account_id: string }) => {
      sessionStorage.setItem(TOKEN_KEY, session.token);
      sessionStorage.setItem(`${TOKEN_KEY}.email`, session.email);
      sessionStorage.setItem(`${TOKEN_KEY}.account`, session.account_id);
      setToken(session.token);
      setEmail(session.email);
      setAccountId(session.account_id);
    },
    [],
  );

  const signOut = useCallback(() => {
    sessionStorage.removeItem(TOKEN_KEY);
    sessionStorage.removeItem(`${TOKEN_KEY}.email`);
    sessionStorage.removeItem(`${TOKEN_KEY}.account`);
    setToken(null);
    setEmail(null);
    setAccountId(null);
  }, []);

  const value = useMemo(
    () => ({ token, email, accountId, signIn, signOut }),
    [token, email, accountId, signIn, signOut],
  );
  return <CustomerSessionContext.Provider value={value}>{children}</CustomerSessionContext.Provider>;
}

export function useCustomerSession(): CustomerSessionState {
  const session = useContext(CustomerSessionContext);
  if (!session) throw new Error('useCustomerSession 必须在 CustomerSessionProvider 内使用');
  return session;
}
