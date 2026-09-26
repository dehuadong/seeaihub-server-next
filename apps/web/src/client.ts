import { apiFetch, type AdminToken } from './api';
import type {
  AccountBalance,
  AccountEntriesResponse,
  CreateAccountResponse,
  GatewayModelsResponse,
  IssueApiKeyResponse,
  ProviderCostGapsResponse,
  ProviderFailuresResponse,
  ReconciliationCase,
  RoutePoliciesResponse,
  RoutePolicy,
  RouteStrategy,
} from './types';

/// 管理 API 的每一个端点在这里各有一个函数：**页面不拼路径、不拼查询串**。
///
/// 与 `apps/api/src/main.rs` 的路由表一一对应；域名前缀由 Vite 代理（开发）或同源部署（生产）提供。
export class AdminClient {
  constructor(private readonly token: AdminToken) {}

  private get<T>(path: string): Promise<T> {
    return apiFetch<T>(path, this.token);
  }

  private send<T>(path: string, method: string, body?: unknown): Promise<T> {
    return apiFetch<T>(path, this.token, { method, body });
  }

  gatewayModels(): Promise<GatewayModelsResponse> {
    return this.get('/api/v1/gateway-models');
  }

  setGatewayModelEnabled(gatewayModel: string, enabled: boolean): Promise<void> {
    return this.send(`/api/v1/gateway-models/${encodeURIComponent(gatewayModel)}`, 'PATCH', { enabled });
  }

  setOfferingEnabled(offeringId: string, enabled: boolean): Promise<void> {
    return this.send(`/api/v1/offerings/${offeringId}`, 'PATCH', { enabled });
  }

  publishRevision(command: unknown): Promise<{ runtime_revision_id: string; gateway_model: string }> {
    return this.send('/api/v1/runtime-revisions', 'POST', command);
  }

  upsertFxRate(currency: string, rateMicros: number, effectiveAt?: string): Promise<void> {
    const body: Record<string, unknown> = { currency, rate_micros: rateMicros };
    if (effectiveAt) body.effective_at = effectiveAt;
    return this.send('/api/v1/fx-rates', 'PUT', body);
  }

  routePolicies(): Promise<RoutePoliciesResponse> {
    return this.get('/api/v1/route-policies');
  }

  upsertRoutePolicy(
    gatewayModel: string | null,
    strategy: RouteStrategy,
    discountRates: Record<string, number> = {},
    tagChannelMap: Record<string, string> = {},
  ): Promise<RoutePolicy> {
    return this.send('/api/v1/route-policies', 'PUT', {
      gateway_model: gatewayModel,
      strategy,
      discount_rates: discountRates,
      tag_channel_map: tagChannelMap,
    });
  }

  createAccount(initialCreditMicrousd: number): Promise<CreateAccountResponse> {
    return this.send('/api/v1/accounts', 'POST', { initial_credit_microusd: initialCreditMicrousd });
  }

  accountBalance(accountId: string): Promise<AccountBalance> {
    return this.get(`/api/v1/accounts/${encodeURIComponent(accountId)}`);
  }

  accountEntries(accountId: string, limit = 50): Promise<AccountEntriesResponse> {
    return this.get(`/api/v1/accounts/${encodeURIComponent(accountId)}/entries?limit=${limit}`);
  }

  creditAccount(accountId: string, amountMicrousd: number, businessKey: string): Promise<void> {
    return this.send(`/api/v1/accounts/${encodeURIComponent(accountId)}/credits`, 'POST', {
      amount_microusd: amountMicrousd,
      business_key: businessKey,
    });
  }

  setAccountTag(accountId: string, tag: string | null): Promise<void> {
    return this.send(`/api/v1/accounts/${encodeURIComponent(accountId)}/tag`, 'PUT', { tag });
  }

  issueApiKey(accountId: string, label: string): Promise<IssueApiKeyResponse> {
    return this.send(`/api/v1/accounts/${encodeURIComponent(accountId)}/api-keys`, 'POST', { label });
  }

  revokeApiKey(keyId: string): Promise<void> {
    return this.send(`/api/v1/api-keys/${encodeURIComponent(keyId)}`, 'DELETE');
  }

  providerFailures(kinds: string[] = [], limit = 50): Promise<ProviderFailuresResponse> {
    const query = new URLSearchParams();
    if (kinds.length > 0) query.set('kind', kinds.join(','));
    query.set('limit', String(limit));
    return this.get(`/api/v1/provider-failures?${query}`);
  }

  providerCostGaps(limit = 50): Promise<ProviderCostGapsResponse> {
    return this.get(`/api/v1/provider-cost-gaps?limit=${limit}`);
  }

  reconciliationCases(): Promise<ReconciliationCase[]> {
    return this.get('/api/v1/reconciliation-cases');
  }

  refundReconciliation(jobId: string, note: string, businessKey: string): Promise<void> {
    return this.send(`/api/v1/reconciliation-cases/${encodeURIComponent(jobId)}/refund`, 'POST', {
      note,
      business_key: businessKey,
    });
  }
}
