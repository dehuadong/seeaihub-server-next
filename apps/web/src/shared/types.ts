/// 管理端视图的类型。字段按**真实响应**写（`crates/application` 的视图结构与
/// `apps/api/src/main.rs` 的响应结构体逐字核对过），不按想象写。
///
/// 金额一律是**微单位**（microusd）：展示层负责换算，传输与判断都用整数。

export interface ConsumerRatesCny {
  text_input_micros_per_million: number;
  image_input_micros_per_million: number;
  text_output_micros_per_million: number;
  image_output_micros_per_million: number;
}

export interface GatewayModelCandidate {
  offering_id: string;
  provider_kind: string;
  provider_model_id: string;
  adapter_key: string;
  routing_priority: number;
  weight: number;
  /// 供给与它所在渠道都启用时才为 `true`：目录里为什么没有这条候选，看它。
  enabled: boolean;
  carrier_schema: unknown;
  parameter_mapping: unknown;
  /// 按 token 计量量那一种形态的对客价载体；这条候选不带定价时为 `null`。
  consumer_rates_cny: ConsumerRatesCny | null;
  /// 渠道成本（原币种微单位），只作定价参考，不是售价的被乘数。
  reference_cost_microusd: number | null;
  cost_currency: string | null;
  cost_basis: 'computed' | 'declared' | null;
  tier_prices: unknown;
  floor_amounts: unknown;
}

export interface GatewayModel {
  gateway_model: string;
  enabled: boolean;
  vendor_id: string;
  /// 厂商原生名：只在管理端出现，对客面看不到。
  native_model_id: string;
  native_revision: string;
  runtime_revision_id: string;
  published_at: string;
  /// 加价系数（基点）：每个网关模型一个，随修订发布；没有带定价的候选时为 `null`。
  markup_bps: number | null;
  candidates: GatewayModelCandidate[];
}

export interface GatewayModelsResponse {
  gateway_models: GatewayModel[];
}

export interface ProviderCostGap {
  job_id: string;
  attempt_id: string;
  account_id: string;
  gateway_model: string;
  provider_kind: string | null;
  provider_trace_id: string | null;
  completed_at: string | null;
}

export interface ProviderCostGapsResponse {
  gaps: ProviderCostGap[];
  count: number;
  truncated: boolean;
}

export interface ProviderFailure {
  job_id: string;
  account_id: string;
  gateway_model: string;
  offering_id: string;
  provider_kind: string | null;
  kind: string;
  error_code: string;
  provider_trace_id: string | null;
  provider_error_code: string | null;
  provider_error_message: string | null;
  updated_at: string;
}

export interface ProviderFailuresResponse {
  failures: ProviderFailure[];
  count: number;
  truncated: boolean;
}

/// `GET /api/v1/reconciliation-cases` 直接回数组（没有外层包装）。
export interface ReconciliationCase {
  id: string;
  job_id: string | null;
  attempt_id: string | null;
  account_id: string;
  reason: string;
  provider_trace_id: string | null;
  created_at: string;
}

export type RouteStrategy = 'priority_failover' | 'weighted_random' | 'least_cost' | 'user_tag';

export interface RoutePolicy {
  /// `null` 是全局那条；否则是这个网关模型的覆盖。
  gateway_model: string | null;
  strategy: RouteStrategy;
  discount_rates: Record<string, number>;
  tag_channel_map: Record<string, string>;
  version: number;
}

export interface RoutePoliciesResponse {
  route_policies: RoutePolicy[];
}

/// 管理员读余额的形状（`AccountBalanceResponse`）。对客那条是另一个形状（多一个持有中）。
/// 管理端看到的一个账户（账户列表的一项）。**不含持有中**：那是与余额并列才有意义的第二个数，
/// 放进详情读。
export interface AccountSummary {
  account_id: string;
  balance_microusd: number;
  /// 运营设的标签；没设过就是 null。
  tag: string | null;
  /// 绑定的登录邮箱；这个账户还没有登录身份时为 null（那是"运营直接建的账户"，不是"取不到"）。
  email: string | null;
  created_at: string;
  updated_at: string;
}

export interface AccountsResponse {
  accounts: AccountSummary[];
}

export interface AccountBalance {
  balance_microusd: number;
  updated_at: string;
}

/// `AccountEntriesResponse`：不翻页，所以要能看出被截断。
export interface LedgerEntry {
  account_id: string;
  /// `credit` / `hold` / `capture` / `release` / `adjustment`，与落库取值同名。
  kind: string;
  /// 人民币微单位；持有与扣费为负、释放与调整为正。
  amount_microusd: number;
  job_id: string | null;
  created_at: string;
}

export interface AccountEntriesResponse {
  entries: LedgerEntry[];
  count: number;
  truncated: boolean;
}

export interface CreateAccountResponse {
  account_id: string;
}

/// 管理端看到的一条客户：邮箱身份与它指向的账户（**不含口令哈希、会话与余额**）。
export interface CustomerView {
  customer_id: string;
  email: string;
  account_id: string;
  created_at: string;
  last_login_at: string | null;
}

/// 发密钥的响应：明文只在这一次出现，标识用来事后吊销。
export interface IssueApiKeyResponse {
  api_key: string;
  key_id: string;
}

/// 对客目录的一条模型：`GET /v1/models`（公开，无需鉴权）。
export interface PublicModel {
  name: string;
  vendor_id: string;
  revision: string;
  contract: unknown;
}

export interface PublicModelsResponse {
  data: PublicModel[];
}

/// 对客账户面：余额与持有中**分开给**，不合成一个"总资产"。
export interface OwnAccount {
  balance_microusd: number;
  held_microusd: number;
  updated_at: string;
}

/// 一次对客注册/登录的结果：会话令牌与自己的账户。
export interface CustomerSession {
  token: string;
  expires_at: string;
  email: string;
  account_id: string;
}

/// 一把自己的 API Key。**没有明文**：创建那一次之后就再也拿不回来了。
export interface CustomerKey {
  key_id: string;
  label: string;
  created_at: string;
  revoked_at: string | null;
}

export interface CustomerKeysResponse {
  keys: CustomerKey[];
}

/// 用量里的一次生成请求：执行记录的**对客投影**，不含 Job 标识与内部状态。
export interface CustomerUsageRow {
  gateway_model: string;
  /// 对客状态：`succeeded` / `failed` / `pending` / `canceled`（内部 Job 状态收敛过的取值）。
  status: 'succeeded' | 'failed' | 'pending' | 'canceled';
  /// `generation` / `edit`。
  kind: 'generation' | 'edit';
  created_at: string;
  image_count: number;
  charged_microusd: number;
}

export interface CustomerUsageResponse {
  usage: CustomerUsageRow[];
  count: number;
  truncated: boolean;
}

/// 账单汇总：按区间**全量**算，不随明细条数上限变化。
export interface CustomerBilling {
  since: string | null;
  until: string | null;
  requests: number;
  images: number;
  charged_microusd: number;
}
