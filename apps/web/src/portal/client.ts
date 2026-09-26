import { apiFetch } from '../shared/api';
import type {
  AccountEntriesResponse,
  CustomerBilling,
  CustomerKeysResponse,
  CustomerSession,
  CustomerUsageResponse,
  IssueApiKeyResponse,
  OwnAccount,
} from '../shared/types';

/// 对客 API 的客户端。**只调 `/v1/customer/*`**：管理面那批端点一条都不在这里，
/// 客户会话调它们只会被拒（那正是服务端的隔离）。
export class CustomerClient {
  constructor(private readonly token: () => string | null) {}

  private get<T>(path: string): Promise<T> {
    return apiFetch<T>(path, this.token);
  }

  private send<T>(path: string, method: string, body?: unknown, admin = true): Promise<T> {
    return apiFetch<T>(path, this.token, { method, body, admin });
  }

  /// 注册（未认证）：拿到会话与自己的账户。
  register(email: string, password: string): Promise<CustomerSession> {
    return this.send('/v1/customers', 'POST', { email, password }, false);
  }

  /// 登录（未认证）。
  login(email: string, password: string): Promise<CustomerSession> {
    return this.send('/v1/customer/sessions', 'POST', { email, password }, false);
  }

  logout(): Promise<void> {
    return this.send('/v1/customer/sessions', 'DELETE');
  }

  changePassword(currentPassword: string, newPassword: string): Promise<void> {
    return this.send('/v1/customer/password', 'PUT', {
      current_password: currentPassword,
      new_password: newPassword,
    });
  }

  apiKeys(): Promise<CustomerKeysResponse> {
    return this.get('/v1/customer/api-keys');
  }

  issueApiKey(label: string): Promise<IssueApiKeyResponse> {
    return this.send('/v1/customer/api-keys', 'POST', { label });
  }

  revokeApiKey(keyId: string): Promise<void> {
    return this.send(`/v1/customer/api-keys/${encodeURIComponent(keyId)}`, 'DELETE');
  }

  account(): Promise<OwnAccount> {
    return this.get('/v1/customer/account');
  }

  ledger(limit = 50): Promise<AccountEntriesResponse> {
    return this.get(`/v1/customer/ledger?limit=${limit}`);
  }

  usage(limit = 50): Promise<CustomerUsageResponse> {
    return this.get(`/v1/customer/usage?limit=${limit}`);
  }

  billing(): Promise<CustomerBilling> {
    return this.get('/v1/customer/billing');
  }
}
