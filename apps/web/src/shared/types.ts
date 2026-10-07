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

/// 对客计价形态：运营按候选选的收钱方式，与 Offering 的成本计价形态独立；只有两种（`0007` §1/§2）。
export type ConsumerFormula = 'token_rates' | 'upstream_declared';

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
  /// 对客计价形态（运营按候选选，与成本形态独立）；历史修订不带时为 `null`，按等于成本形态读。
  consumer_formula: ConsumerFormula | null;
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
  /// 这次生效发布的**厂商模型合同**。管理端需要它在改价时重发同一份——合同不属于"这次要改变的东西"。
  capability_schema: Record<string, unknown>;
  runtime_revision_id: string;
  published_at: string;
  /// 加价系数（基点）：每个网关模型一个，随修订发布；没有带定价的候选时为 `null`。
  markup_bps: number | null;
  candidates: GatewayModelCandidate[];
}

export interface GatewayModelsResponse {
  gateway_models: GatewayModel[];
}

/// 一条**可被运营选中**的供给：发布平台模型时那个 `offering_id` 指向的东西。
///
/// 它**不含**渠道地址与凭证变量名——那是渠道部署事实，选择用不到它们（`docs/design/0012` §2.1）。
/// 一份**四档费率**：币种 + 四档金额（每百万 token 的微单位）+ 出处。
///
/// 成本费率（`cost_rates`）与对客参考价目（`consumer_reference_rates`）同形，但含义不同：前者是
/// 渠道的成本费率、后者只作对客 token 四档的初始价来源。
export interface FourTierRates {
  currency: string;
  text_input_microusd_per_million: number;
  image_input_microusd_per_million: number;
  text_output_microusd_per_million: number;
  image_output_microusd_per_million: number;
  source_url: string;
}

export interface SelectableOffering {
  offering_id: string;
  /// 厂商（先选它，再在它的分组里选供给）。
  vendor_id: string;
  native_model_id: string;
  native_revision: string;
  /// 渠道与渠道侧模型名。
  provider_kind: string;
  provider_model_id: string;
  adapter_key: string;
  /// 这条供给按什么计价。
  formula: string;
  /// 这条通路的驱动器**会不会从上游响应里取到金额**。它决定"上游声明金额 × 倍率"这条对客形态
  /// 在这条通路上成不成立——界面据此过滤下拉（发布期同样据此拒绝）。
  declares_cost: boolean;
  /// 这条通路的驱动器**给不给四分项用量**。它决定"按 token 四档"这条对客形态在这条通路上
  /// 成不成立：给不出用量就算不出对客价（界面据此过滤下拉，发布期同样据此拒绝）。
  provides_token_usage: boolean;
  /// 渠道成本币种与四档费率（`token_rates` 才有费率）。
  cost_currency: string | null;
  cost_rates: FourTierRates | null;
  /// 这条供给声明的**对客参考价目**（渠道原币种四档）：对客 token 四档的初始价取它。
  /// 它**不是成本参数**——成本按上游声明金额的供给同样可以声明它。
  consumer_reference_rates: FourTierRates | null;
  /// 能不能选：供给自己启用、且它所属渠道启用。停用的仍列出来并标明。
  enabled: boolean;
}

export interface SelectableOfferingsResponse {
  offerings: SelectableOffering[];
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

/// 管理端看到的一个账户（账户列表的一项）。**不含持有中**：那是与余额并列才有意义的第二个数，
/// 放进详情读。
export interface AccountSummary {
  account_id: string;
  /// 账户名称：每个账户始终有一个（建账户时给的或服务端生成的）。
  name: string;
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

/// 管理员读余额的形状（`AccountBalanceResponse`）：已结算余额、持有中与可用额分开给，同一时点满足
/// `available = balance − held`（账户资金 Spec `0002` §4）。管理员可以分别显示三个金额。
export interface AccountBalance {
  /// 已结算余额（可以为负）。
  balance_microusd: number;
  /// 持有中：active 预授权之和。
  held_microusd: number;
  /// 可用额 = 已结算余额 − 持有中。
  available_microusd: number;
  /// 账户金额版本。
  version: number;
  updated_at: string;
}

/// `AccountEntriesResponse`：不翻页，所以要能看出被截断。
export interface LedgerEntry {
  account_id: string;
  /// `credit` / `capture` / `adjustment` / `cost`，与落库取值同名。预授权只留在 `ledger.holds`，
  /// 不作为资金流水。
  kind: string;
  /// 人民币微单位；入账为正，实收与平台成本为负，正式调整按资金增减带符号。
  amount_microusd: number;
  job_id: string | null;
  created_at: string;
}

/// 管理员看到的**一条调用明细**：逐笔生成请求（对客那条读**不回** `job_id`，这条回——运营要回答
/// "哪一笔扣费对应哪次调用"）。
export interface AdminUsageRow {
  job_id: string;
  gateway_model: string;
  status: 'succeeded' | 'failed' | 'pending' | 'canceled';
  kind: 'generation' | 'edit';
  created_at: string;
  /// 模型类型：受理时引用的 Vendor Model 的类型。
  type: ModelType;
  /// 本次执行按类型给出的量。
  usage: UsageAmounts;
  charged_microusd: number;
}

export interface AdminUsageResponse {
  usage: AdminUsageRow[];
  count: number;
  truncated: boolean;
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
  /// 关联账户的当前名称（开户、改名之后都从同一处读）。
  account_name: string;
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
  /// 模型类型：客户端据此判断用量单位与表单参数面。
  type: ModelType;
  contract: unknown;
}

export interface PublicModelsResponse {
  data: PublicModel[];
}

/// 对客账户面：已结算余额、持有中与可用额分开给，三者在同一时点满足
/// `available = balance − held`。客户控制台只把这个数字显示成一个「余额」——取
/// `available_microusd`（客户现在能用的钱），不分别展示已结算余额、持有中或可用额，也不显示单笔
/// 预授权额（账户资金 Spec `0002` §4；控制台 Spec `0001` C7、V-D5）。
export interface OwnAccount {
  /// 自己的账户名称：客户可在账户设置里改（规则见账户名称 Spec）。
  name: string;
  balance_microusd: number;
  /// 已受理未结清的占用合计。接口返回，但客户页面不作为单独的数展示。
  held_microusd: number;
  /// 可用额 = 已结算余额 − 持有中。客户页面那个「余额」取的就是它。
  available_microusd: number;
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
  /// 终态时刻；处理中（结果未定）时为 `null`。已结束历史按它归属日期区间。
  terminal_at: string | null;
  /// 模型类型：受理时引用的 Vendor Model 的类型。
  type: ModelType;
  /// 本次执行按类型给出的量。
  usage: UsageAmounts;
  charged_microusd: number;
}

export interface CustomerUsageResponse {
  usage: CustomerUsageRow[];
  count: number;
  truncated: boolean;
  /// 下一页的不透明定位；没有下一页时为 `null`。
  next_cursor: string | null;
}

/// 对客资金流水响应：与管理员那条同形，外加翻页定位与同一条件下的总数。
export interface CustomerLedgerResponse {
  entries: LedgerEntry[];
  count: number;
  /// 同一套区间与类别条件下的总条数（与 `count` 不同：本页条数）。
  total: number;
  truncated: boolean;
  /// 下一页的不透明定位；没有下一页时为 `null`。
  next_cursor: string | null;
}

/// 模型类型：与 `/v1/models`、用量记录与账单里的 `type` 同值。
export type ModelType = 'image' | 'video' | 'chat';

/// 一次执行（或一段区间）按模型类型给出的用量；只有有值的键出现。
export interface UsageAmounts {
  images?: number;
  seconds?: number;
  input_tokens?: number;
  output_tokens?: number;
}

/// 账单汇总：按区间**全量**算，不随明细条数上限变化。
export interface CustomerBilling {
  since: string | null;
  until: string | null;
  requests: number;
  /// 区间内按类型分别合计的量；某类型没有值时对应的键缺省。
  usage: UsageAmounts;
  charged_microusd: number;
}
