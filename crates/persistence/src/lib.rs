use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use seeai_application::{
    AcceptanceProbe, AccountSummary, ActiveOfferingChannel, ApiKeyView, ApplicationError,
    AttemptFailure, BalanceChange, ClaimedJob, CompleteJob, CustomerBillingQuery,
    CustomerBillingSummary, CustomerUsageKind, CustomerUsageView, CustomerView,
    GatewayModelCandidateView, GatewayModelView, HoldDisposition, HubRepository, JobView,
    LeaseRecovery, LedgerBalanceMismatch, NewFxRate, NormalizedOffering, OpenLedgerCaseCommand,
    PricePlanRates, ProviderCostGapView, ProviderFailureKind, ProviderFailureQuery,
    ProviderFailureView, PublicErrorCode, PublishRuntimeRequest, ReconciliationCaseView,
    ReferencedOffering, RefundReconciliationCommand, RoutingDecision, SelectableOfferingView,
    UnacceptedAttempt, customer_usage_status, declared_output_images,
};
use seeai_domain::{
    AccountId, AttemptId, ChannelId, ConsumerRatesCny, CostBasis, CreateImageGeneration, FxRate,
    GenerationJob, HitCandidate, ImageBranch, JobId, LedgerEntry, LedgerEntryKind,
    OfferingCandidate, OfferingId, PricePlanId, PriceRates, PriceSnapshot, PricingFormula,
    PublishedModel, PublishedOffering, PublishedRevision, RoutePolicy, RouteStrategy,
    RuntimeRevisionId, VendorModelId,
};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use sqlx::{AssertSqlSafe, PgPool, Row, postgres::PgPoolOptions};
use std::collections::HashSet;
use std::time::Duration as StdDuration;
use uuid::Uuid;

pub mod material_import;

/// **唯一的账本区间谓词**：`$2` 是下界（不含），`$3` 是上界（不含）——即半开区间 `[since, until)`。
///
/// 抽取它是为了"明细与计数口径一致"这件事**在结构上**成立：两处各写一遍 SQL 时，一旦有人只改一处，
/// "还有没有下一页"就会在边界上错位，而那种错位只在条数恰好落在边界上才显形。
///
/// 上界不含与对客账单汇总同一条口径（Spec §4.3）：同一区间下明细与汇总必须对得上，否则边界上那一笔
/// 会被一边算进去、另一边不算。
const LEDGER_RANGE_PREDICATE: &str =
    "($2::timestamptz IS NULL OR created_at > $2) AND ($3::timestamptz IS NULL OR created_at < $3)";

/// **唯一的候选可用性判据**：这条候选现在真的能走吗——它自己启用，且它所在的渠道也启用。
///
/// 四处都判它：对客目录（列哪些模型）、受理（取哪些候选）、管理员视图（每个候选的 `enabled`
/// 字段），以及受理路径对 route 缓存给出的候选集的**开关复核**（缓存只按修订标识比对新旧，
/// 启停看不见，见 [`HubRepository::enabled_offerings`]）。只写一遍是因为分散之后，将来加一条
/// 闸门（比如渠道维护窗口）必然漏掉其中一处，而漏掉的那一处会让"目录里列着、提交时却取不到
/// 候选"重新出现——那种模型对调用方是 404，比不列更糟。
///
/// 列名 `o`/`c` 是这四条查询里供给与渠道的固定别名。管理员视图不把它放进 `WHERE`（它要连
/// **停用**的候选一起列出来，运营才看得出"为什么它调不动"），而是放进 `SELECT` 当一列读。
///
/// 用它拼查询要经过 `AssertSqlSafe`：`sqlx::query` 默认只收字面量，为的是逼动态 SQL 先被审
/// 一遍。这里拼进去的只有这个编译期常量（列名与一个布尔与），不含任何外部输入或用户数据，
/// 所以那个断言是"审过了"，不是把检查绕过去——别的动态 SQL 不要走这条路。
const CANDIDATE_AVAILABLE_SQL: &str = "o.enabled AND c.enabled";

/// 读候选定价必须**一起**选出来的列。
///
/// [`row_candidate_pricing`] 按名字逐列读它们，少一列就是运行期的 `no column found for name: …`
/// ——曾经漏掉 `consumer_formula`，让"省略渠道的增量发布"整条 500。三处读候选定价的查询共用这一份，
/// 新增列只改这里。列名不带表别名：这几列只有 `publication.runtime_revisions` 有，不会歧义。
const CANDIDATE_PRICING_COLUMNS: &str = "consumer_rates_cny, consumer_formula, cost_basis, \
     reference_cost_microusd, cost_currency, tier_prices, floor_amounts";

/// 读**所有在效合同**里声明的输出张数上限，取最大的那份。
///
/// 超时链上"按最大输出张数算出来的上限"要有个来源，这就是它：合同自己给 `n` 声明的取值面
/// （`capability_schema.properties.n.maximum`），不是代码里写死的数、也不是输入参考图上限
/// （`restrictions.max_reference_images` 说的是能带几张图进去）。一次发布的合同对全平台生效，所以这里扫的
/// 是全部 active 条目，不是某一条候选的承载面。
///
/// 提交之后才查：运行中的进程不会看到未提交的发布；发布是原子的（一次修订全量替换），所以扫出来
/// 的一定是某一个完整修订的合同集合。
///
/// 三种情形都落回 `fallback`：还没发布过任何东西、在效合同一条都没声明 `n`、以及合同解码不出来的
/// 行。解码失败不报错——它不该让整个进程起不来，那时取兜底值更保守；解不出来的行数随结果一起返回，
/// 由调用方记日志。返回的是最大值以及声明它的那条合同，启动日志因此说得出"这个数哪来的"。
pub async fn max_declared_output_images(
    pool: &PgPool,
    fallback: u64,
) -> Result<(u64, Option<String>, usize), ApplicationError> {
    let rows = sqlx::query(
        r#"
        SELECT DISTINCT re.gateway_model, vm.capability_schema
        FROM publication.runtime_entries re
        JOIN catalog.vendor_models vm ON vm.id = re.vendor_model_id
        WHERE re.active
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(database_error)?;
    let mut declared: Vec<(String, u64)> = Vec::with_capacity(rows.len());
    let mut undecodable = 0_usize;
    for row in &rows {
        let gateway_model: String = row.try_get("gateway_model").map_err(database_error)?;
        let capability_schema: Value = row.try_get("capability_schema").map_err(database_error)?;
        if capability_schema.as_object().is_none() {
            // 合同不是一份 JSON 对象：这一行的内容读不出来，得报给调用方（合同没声明 `n` 是另一回事，
            // 那种模型本来就只生成一张，不算异常）。
            undecodable += 1;
            continue;
        }
        if let Some(maximum) = declared_output_images(&gateway_model, &capability_schema) {
            declared.push((gateway_model, maximum.maximum));
        }
    }
    match declared.into_iter().max_by_key(|(_, maximum)| *maximum) {
        Some((gateway_model, maximum)) => Ok((maximum, Some(gateway_model), undecodable)),
        None => Ok((fallback, None, undecodable)),
    }
}

#[derive(Debug, Clone)]
pub struct PgHubRepository {
    pool: PgPool,
}

impl PgHubRepository {
    pub async fn connect(
        database_url: &str,
        max_connections: u32,
    ) -> Result<Self, ApplicationError> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(database_url)
            .await
            .map_err(database_error)?;
        Ok(Self { pool })
    }

    pub async fn migrate(&self) -> Result<(), ApplicationError> {
        sqlx::migrate!("../../migrations")
            .run(&self.pool)
            .await
            .map_err(|error| ApplicationError::Persistence(error.to_string()))
    }

    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// 读一个账户**当前**的余额与写入时刻。
    ///
    /// 用在"这次没有改动余额、但调用方仍要刷新缓存"的路径上（幂等重放、保留预授权的失败收尾）：
    /// 返回数据库的值总不会错，而"重放后缓存还留着旧数"会让下一次预检拿着过时的数去判。
    async fn account_balance(
        &self,
        account_id: AccountId,
    ) -> Result<BalanceChange, ApplicationError> {
        let row =
            sqlx::query("SELECT balance_microusd, updated_at FROM ledger.accounts WHERE id = $1")
                .bind(account_id.0)
                .fetch_optional(&self.pool)
                .await
                .map_err(database_error)?
                .ok_or_else(|| ApplicationError::NotFound(format!("account {account_id}")))?;
        balance_change(&row, account_id)
    }

    /// 一次索引探测：账户这一行在不在。
    ///
    /// 读流水与读持有额都要先把"账户不存在"与"这个账户什么都没有"分开——前者是 404，后者是
    /// 空结果。判据只能是 `ledger.accounts`：账户事实只有这一处。
    async fn account_exists(&self, account_id: AccountId) -> Result<bool, ApplicationError> {
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM ledger.accounts WHERE id = $1)")
            .bind(account_id.0)
            .fetch_one(&self.pool)
            .await
            .map_err(database_error)
    }

    async fn load_generation_job(&self, job_id: JobId) -> Result<GenerationJob, ApplicationError> {
        // 入口地址与凭证名从 **Job 自己那两列**读：它们与适配器、渠道模型一样是受理时冻结的
        // 执行事实，现场 JOIN 渠道行会让一次直接改库把已受理的 Job 打到别处去。渠道 JOIN 因此
        // 只剩 `provider_kind` 这一个只在发布侧用到的值。
        let row = sqlx::query(
            r#"
            SELECT
                j.id, j.account_id, j.state, j.branch, j.gateway_model,
                j.native_parameters, j.idempotency_key,
                j.request_hash, j.max_cost_microusd, j.created_at, j.updated_at,
                vm.id AS vendor_model_id, vm.native_revision, vm.capability_schema,
                j.carrier_schema, j.parameter_mapping,
                j.adapter_key, j.provider_model_id, j.base_url, j.credential_env,
                o.id AS offering_id, o.restrictions,
                c.id AS channel_id, c.provider_kind,
                j.runtime_revision_id, j.price_snapshot
            FROM generation.jobs j
            JOIN catalog.vendor_models vm ON vm.id = j.vendor_model_id
            JOIN supply.offerings o ON o.id = j.offering_id
            JOIN supply.channels c ON c.id = j.channel_id
            WHERE j.id = $1
            "#,
        )
        .bind(job_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)?
        .ok_or_else(|| ApplicationError::NotFound(format!("job {job_id}")))?;
        row_to_generation_job(&row)
    }
}

#[async_trait]
impl HubRepository for PgHubRepository {
    async fn publish_runtime(
        &self,
        request: PublishRuntimeRequest,
    ) -> Result<PublishedRevision, ApplicationError> {
        let revision_id = RuntimeRevisionId::new();
        let now = Utc::now();
        let PublishRuntimeRequest {
            vendor_id,
            native_model_id,
            gateway_model,
            native_revision,
            actor,
            capability_schema,
            markup_bps,
            definitions_from_offerings,
            offerings,
        } = request;
        if offerings.is_empty() {
            return Err(ApplicationError::Validation(
                "publish requires at least one offering".to_owned(),
            ));
        }
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        // 同一个网关模型名的发布**串行化**：本函数靠"先 `UPDATE ... SET active = false
        // WHERE active AND gateway_model = $1`、再插入新条目"做原子替换，而这个替换只有在
        // 同一名字的两次发布不交错时才成立。交错时两边都可能先看到"还没有自己的条目"，
        // 于是两份修订的 active 条目同时存在——那是**读时**才会暴露的问题（仓库层对
        // "active 候选跨修订并存"报错），表现为这个型号的所有请求一起失败，直到有人重新发布一次。
        // 唯一索引挡不住这件事：每次发布都给候选新建一条供给行，索引上不会撞。
        // 按名字取一把事务级咨询锁（随事务结束自动释放），让"替换"真的是一次替换。
        // 锁键的写法与 `create_job` 里那把幂等锁一致（`hashtextextended`，bigint 键）。
        // 不同的名字各有各的锁；哈希撞键只会让两个名字的发布多等一会儿，不影响正确性。
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(&gateway_model)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        // 发布期校验：每个候选声明的成本币种必须有一行**已生效**的折算率，否则整份发布回滚。
        // 受理时要按该币种取"受理时刻生效的那一行"并快照；发布期不拦，问题会在受理时才暴露
        // ——那时拒的是消费者的请求，而错的是管理员的一次录入遗漏。
        //
        // 判据按**候选声明的币种**（不是"这条候选带不带定价"）：币种是这条供给的成本口径，
        // 有没有定价不该让同一个币种在"能不能发布"这件事上前后不一致。
        for offering in &offerings {
            // 发布期校验的判据是**这条供给声明的成本币种**：受理时按它取折算率、Driver 按它给
            // 上游金额标注币种。币种是成本口径，与"这条供给按什么计价、带不带价目表"无关。
            let cost_currency = offering.cost_currency().ok_or_else(|| {
                ApplicationError::Validation(
                    "every offering must declare the currency its cost is kept in".to_owned(),
                )
            })?;
            let effective: bool = sqlx::query_scalar(
                r#"
                SELECT EXISTS (
                    SELECT 1 FROM pricing.fx_rates
                    WHERE currency = $1 AND effective_at <= now()
                )
                "#,
            )
            .bind(cost_currency)
            .fetch_one(&mut *transaction)
            .await
            .map_err(database_error)?;
            if !effective {
                transaction.rollback().await.map_err(database_error)?;
                return Err(ApplicationError::Validation(format!(
                    "no effective fx rate for {cost_currency}; record one before publishing a \
                     candidate whose cost is kept in that currency"
                )));
            }
        }
        // 合同行**不可变**：同一个 (vendor, model, revision) 只落一行，已有行一律复用，
        // 绝不就地改写。这样"Job 固定受理时版本"才成立——旧 Job 事后读到的合同与它受理时
        // 逐字相同。要改合同就发新修订（新修订是新行）。
        //
        // 同一修订重发是幂等的：内容相同就复用那一行（不产生第二行）；内容不同则明确拒绝，
        // 而不是把旧合同悄悄改掉——那会让已受理的 Job 与它对不上。
        let vendor_model_id = match sqlx::query_scalar::<_, Uuid>(
            r#"
            INSERT INTO catalog.vendor_models
                (id, vendor_id, native_model_id, native_revision, capability_schema)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (vendor_id, native_model_id, native_revision) DO NOTHING
            RETURNING id
            "#,
        )
        .bind(VendorModelId::new().0)
        .bind(&vendor_id)
        .bind(&native_model_id)
        .bind(&native_revision)
        .bind(&capability_schema)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        {
            Some(id) => VendorModelId(id),
            None => {
                let existing = sqlx::query(
                    r#"
                    SELECT id, capability_schema FROM catalog.vendor_models
                    WHERE vendor_id = $1 AND native_model_id = $2 AND native_revision = $3
                    "#,
                )
                .bind(&vendor_id)
                .bind(&native_model_id)
                .bind(&native_revision)
                .fetch_one(&mut *transaction)
                .await
                .map_err(database_error)?;
                let stored: Value = existing
                    .try_get("capability_schema")
                    .map_err(database_error)?;
                if stored != capability_schema {
                    transaction.rollback().await.map_err(database_error)?;
                    return Err(ApplicationError::Validation(format!(
                        "vendor model {native_model_id} already has a contract for revision {native_revision}; \
                         the contract is immutable, so publish a new revision"
                    )));
                }
                VendorModelId(existing.try_get("id").map_err(database_error)?)
            }
        };
        let mut candidates = Vec::with_capacity(offerings.len());
        let mut snapshot_entries = Vec::with_capacity(offerings.len());
        // 定价列是**按候选键**的映射（同一个网关模型的不同候选价格不同），所以在循环里逐条
        // 收集，最后整列写在修订上。全部为空时写 NULL：那是"这份修订没有定价"的明确信号，
        // 与"定价表是空对象"分得开。
        let mut reference_cost_microusd = Map::new();
        let mut cost_currency = Map::new();
        let mut consumer_rates_cny = Map::new();
        let mut consumer_formula = Map::new();
        let mut cost_basis = Map::new();
        let mut tier_prices = Map::new();
        let mut floor_amounts = Map::new();
        for offering in &offerings {
            // 这次发布把哪份技术定义冻结进条目有两条来源（见 `PublishRuntimeRequest::definitions_from_offerings`）：
            // 内联式发布用请求里那份（顺带按身份 upsert 供给行）；引用式发布用被引用 Offering 行与它所属
            // 渠道行的**当前值**，一个字都不取自请求。两条来源经同一个类型返回，后面的写入因此只有一个形状。
            let frozen = if definitions_from_offerings {
                // 引用式发布必须带"选中的是哪一行"。缺了就是一次说不清指向的发布——**按身份去猜**
                // 会在同一渠道下多行供给时挑错，所以这里当场拒，不替调用方补一个默认。
                let selected = offering.offering_id.ok_or_else(|| {
                    ApplicationError::Validation(format!(
                        "referenced offering {}/{} carries no offering_id: a referenced publish \
                         must name the row it points at",
                        offering.provider_kind, offering.provider_model_id
                    ))
                })?;
                referenced_definition(&mut transaction, selected).await?
            } else {
                inline_definition(&mut transaction, vendor_model_id, offering, &actor).await?
            };
            candidates.push(OfferingCandidate {
                runtime_revision_id: revision_id,
                vendor_model_id,
                offering_id: frozen.offering_id,
                channel_id: frozen.channel_id,
                gateway_model: gateway_model.clone(),
                native_revision: native_revision.clone(),
                capability_schema: capability_schema.clone(),
                // 八列技术定义取 `frozen`：引用式发布下它是被引用 Offering 行与渠道行的当前值，
                // 内联式发布下它就是请求里那份（由 `inline_definition` 落库）。
                carrier_schema: frozen.carrier_schema.clone(),
                parameter_mapping: frozen.parameter_mapping.clone(),
                restrictions: frozen.restrictions.clone(),
                adapter_key: frozen.adapter_key.clone(),
                provider_model_id: frozen.provider_model_id.clone(),
                provider_kind: frozen.provider_kind.clone(),
                base_url: frozen.base_url.clone(),
                credential_env: frozen.credential_env.clone(),
                price_snapshot: PriceSnapshot {
                    price_plan_id: frozen.price_plan_id,
                    rates: offering.rates.clone(),
                    formula: offering.formula,
                    cost_unit_price_microusd: offering.cost_unit_price_microusd,
                    consumer_formula: Some(offering.consumer_formula),
                    captured_at: now,
                    // 命中的候选就是这条候选本身：快照是**按候选**带下来的，选中哪条就把哪条
                    // 的快照固化进 Job，所以"这一笔的售价按谁算的"在快照里读得出来。
                    hit_candidate: Some(HitCandidate {
                        offering_id: frozen.offering_id,
                        channel_id: frozen.channel_id,
                        provider_kind: frozen.provider_kind.clone(),
                    }),
                    // 随修订发布的定价。保底额与汇率依赖这次请求（`(size, quality)` 与受理时刻），
                    // 发布侧算不出来，由受理用例算定后填。
                    // 对客费率向量是这条供给的**对客计费基准**（发布期已保证有它、或有旧口径那份
                    // Price Plan 费率），与"参考成本 / 保底表"那组定价参考各归各：渠道按张 / 按次
                    // 计价或直接由上游给金额时，参考成本与保底表都没有着落，但这条供给照样要能卖。
                    consumer_rates_cny: offering.consumer_rates_cny.clone(),
                    tier_prices: offering
                        .pricing
                        .as_ref()
                        .map(|pricing| pricing.tier_prices.clone()),
                    floor_amounts: offering
                        .pricing
                        .as_ref()
                        .map(|pricing| pricing.floor_amounts.clone()),
                    hold_microusd: None,
                    hold_source: None,
                    cost_basis: offering.pricing.as_ref().map(|pricing| pricing.cost_basis),
                    reference_cost_microusd: offering
                        .pricing
                        .as_ref()
                        .map(|pricing| pricing.reference_cost_microusd),
                    // 成本币种是**这条供给声明的事实**（带不带定价都有）：没有 Price Plan 时
                    // 它是唯一的来源，那份声明也必须随快照冻结（Driver 拿它给上游金额标注币种、
                    // 受理时按它取折算率）。
                    cost_currency: offering.cost_currency().map(str::to_owned),
                    markup_bps,
                    fx_rate: None,
                },
                routing_priority: offering.routing_priority,
                weight: offering.weight,
            });
            // 成本币种按候选键记进修订：它是成本平面的币种，与有没有定价无关。
            if let Some(currency) = offering.cost_currency() {
                cost_currency.insert(
                    frozen.offering_id.to_string(),
                    Value::String(currency.to_owned()),
                );
            }
            // 对客费率向量按候选键记进修订：它是这条供给的售价依据，与定价参考那组无关。
            if let Some(rates) = &offering.consumer_rates_cny {
                consumer_rates_cny.insert(
                    frozen.offering_id.to_string(),
                    serde_json::to_value(rates)
                        .map_err(|error| ApplicationError::Persistence(error.to_string()))?,
                );
            }
            // 对客计价形态同样按候选键记：它记的是运营这次选了什么，与成本形态（条目快照里的
            // `formula`）分处两个字段。
            {
                let key = frozen.offering_id.to_string();
                consumer_formula.insert(
                    key.clone(),
                    Value::String(offering.consumer_formula.as_str().to_owned()),
                );
            }
            if let Some(pricing) = &offering.pricing {
                let key = frozen.offering_id.to_string();
                reference_cost_microusd
                    .insert(key.clone(), Value::from(pricing.reference_cost_microusd));
                cost_basis.insert(
                    key.clone(),
                    Value::String(pricing.cost_basis.as_str().to_owned()),
                );
                tier_prices.insert(key.clone(), pricing.tier_prices.clone());
                floor_amounts.insert(key, pricing.floor_amounts.clone());
            }
            snapshot_entries.push(serde_json::json!({
                "offering_id": frozen.offering_id,
                "routing_priority": offering.routing_priority,
                "weight": offering.weight,
                "provider_kind": frozen.provider_kind,
                "adapter_key": frozen.adapter_key,
                "provider_model_id": frozen.provider_model_id,
                "base_url": frozen.base_url,
                "credential_env": frozen.credential_env,
                "restrictions": frozen.restrictions,
                "contract_carrier_hash": contract_carrier_hash(&capability_schema, &frozen.carrier_schema)?,
                "formula": offering.formula.as_str(),
                "cost_unit_price_microusd": offering.cost_unit_price_microusd,
                "cost_currency": offering.cost_currency(),
                // 四档费率与价目出处只在有 Price Plan 时才有：它们属于 token 计量量这一种形态。
                "rates": offering.rates,
                "price_source_url": offering.price_source_url,
                "consumer_formula": offering.consumer_formula.as_str(),
            }));
        }
        // 发布即原子替换该模型的全部 active 条目：候选集与顺序
        // 始终属于同一个 Revision，不存在跨 Revision 并存。
        //
        // 替换的对象是**平台对客名**，不是厂商原生名：同一份供给可以包成两个网关模型
        // （各自一个名字、各自一份定义），重发一个名字只动它自己。
        //
        // 守卫：一次发布只定义一个网关模型，写下的候选必须同值。它守的是"同一次发布里的
        // `gateway_model` 不得出现第二个值"这条验收要求——"发布即原子替换**这个名字**的
        // 候选集"依赖它，两个名字的候选混在一次发布里会互相顶掉，"替换的到底是谁"就说不清了。
        //
        // 为什么现在走不到这里：每个候选的 `gateway_model` 都是从这个请求**唯一那个**名字字段
        // 抄下来的（上面写快照时逐条用的就是它），因此类型上不可能出现第二个值。留着它是为了
        // 让将来形状变化时（例如允许候选各自报名）先在这里被拦住，而不是先写进去再发现。
        if candidates
            .iter()
            .any(|candidate| candidate.gateway_model != gateway_model)
        {
            transaction.rollback().await.map_err(database_error)?;
            return Err(ApplicationError::Validation(
                "a publication defines exactly one gateway model, but its candidates disagree \
                 on the name"
                    .to_owned(),
            ));
        }
        sqlx::query(
            "UPDATE publication.runtime_entries SET active = false WHERE active AND gateway_model = $1",
        )
        .bind(&gateway_model)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        let snapshot = serde_json::json!({
            "vendor_id": vendor_id,
            "gateway_model": gateway_model,
            "native_revision": native_revision,
            "candidates": snapshot_entries,
        });
        sqlx::query(
            r#"
            INSERT INTO publication.runtime_revisions
                (id, snapshot, published_by, gateway_model, vendor_model_id,
                 markup_bps, reference_cost_microusd, cost_currency, consumer_rates_cny,
                 consumer_formula, cost_basis, tier_prices, floor_amounts)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
            "#,
        )
        .bind(revision_id.0)
        .bind(&snapshot)
        .bind(&actor)
        .bind(&gateway_model)
        .bind(vendor_model_id.0)
        .bind(markup_bps)
        .bind(optional_pricing_map(reference_cost_microusd))
        .bind(optional_pricing_map(cost_currency))
        .bind(optional_pricing_map(consumer_rates_cny))
        .bind(optional_pricing_map(consumer_formula))
        .bind(optional_pricing_map(cost_basis))
        .bind(optional_pricing_map(tier_prices))
        .bind(optional_pricing_map(floor_amounts))
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        for candidate in &candidates {
            // 技术定义八列是**这次发布冻结下来的那一份**（驱动器、供应商模型名、承载面、参数映射、
            // 限制与渠道三要素），受理装配候选只读条目、不再现场 JOIN 活表：否则工程师事后改一条
            // 供给的承载面、或改一条渠道的地址，**已发布修订**的候选会跟着变，"这次发布定义了什么"
            // 就不由这次发布决定了。供给与渠道上那两个 `enabled` 开关**不进快照**——它们是运行状态，
            // 停用要立刻对之后的受理生效，不能被某次发布钉住，所以受理仍按活表这两个开关复核。
            sqlx::query(
                r#"
                INSERT INTO publication.runtime_entries
                    (runtime_revision_id, vendor_model_id, offering_id, price_plan_id,
                     gateway_model, active, routing_priority, weight,
                     adapter_key, provider_model_id, carrier_schema, parameter_mapping,
                     restrictions, provider_kind, base_url, credential_env)
                VALUES ($1, $2, $3, $4, $5, true, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
                "#,
            )
            .bind(revision_id.0)
            .bind(candidate.vendor_model_id.0)
            .bind(candidate.offering_id.0)
            // 这条供给的 Price Plan；它按 token 计量量计价时才有。
            .bind(candidate.price_snapshot.price_plan_id.map(|plan| plan.0))
            .bind(&gateway_model)
            .bind(candidate.routing_priority)
            .bind(i32::try_from(candidate.weight).map_err(|_| {
                ApplicationError::Validation("offering weight is out of range".to_owned())
            })?)
            .bind(&candidate.adapter_key)
            .bind(&candidate.provider_model_id)
            .bind(&candidate.carrier_schema)
            .bind(&candidate.parameter_mapping)
            .bind(&candidate.restrictions)
            .bind(&candidate.provider_kind)
            .bind(&candidate.base_url)
            .bind(&candidate.credential_env)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        }
        // 该名字**首次发布成功**时落一行运维开关（`enabled` 默认 true），此后只由 PATCH 改它。
        // 定义（合同、候选集、定价）不进这张表：那些是修订的内容，存第二份就等于造第二个权威。
        sqlx::query(
            r#"
            INSERT INTO publication.gateway_models (gateway_model, updated_by)
            VALUES ($1, $2)
            ON CONFLICT (gateway_model) DO NOTHING
            "#,
        )
        .bind(&gateway_model)
        .bind(&actor)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        insert_audit(
            &mut transaction,
            &actor,
            "runtime.publish",
            "runtime_revision",
            &revision_id.to_string(),
            &snapshot,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(PublishedRevision {
            runtime_revision_id: revision_id,
            gateway_model,
            candidates,
        })
    }

    async fn active_offering(
        &self,
        gateway_model: &str,
    ) -> Result<Vec<OfferingCandidate>, ApplicationError> {
        // 按 routing_priority 升序取全部 active 候选。每个候选 JOIN 到它所属的那一行
        // vendor_models 取**合同**（模型级唯一一份），技术定义（承载面、参数映射、限制、驱动器、
        // 供应商模型名与渠道三要素）读**条目自己那八列**——它是这次发布冻结下来的那一份，供给与
        // 渠道事后的改动不该改到已发布修订的受理口径。
        //
        // `supply.offerings` 与 `supply.channels` 的 JOIN 仍然要留：它们的两个 `enabled` 开关不在
        // 快照里（[`CANDIDATE_AVAILABLE_SQL`]），停用必须立刻对之后的受理生效。`formula` 与
        // `cost_unit_price_microusd` 也不在快照里：它们是**渠道怎么结算**的事实，继续读活表。
        //
        // 参数名是**平台对客名**（网关模型名），不是厂商原生名：调用方提交的 `model` 就是它。
        // 判据与对客目录**逐条一致**，其中多一条"网关模型开着"——关掉的模型必须真的调不动，
        // 否则"关闭"只影响目录、不影响受理，等于没关。
        //
        // 排序的第二项是 `o.id`（定序，不是业务顺序）：同一档允许多条候选，档内按权重分摊要
        // 划分区间，而区间划分必须只有一个答案——行序不保证稳定，落点因此必须配一个稳定序。
        // 分摊本身在用例层做（那里才知道账户与幂等键），这里只保证取回来的顺序是确定的。
        let rows = sqlx::query(AssertSqlSafe(format!(
            r#"
            SELECT
                rr.id AS runtime_revision_id,
                vm.id AS vendor_model_id, re.gateway_model, vm.native_revision,
                vm.capability_schema, re.carrier_schema, re.parameter_mapping,
                o.id AS offering_id, re.adapter_key, re.provider_model_id, re.restrictions,
                o.formula, o.cost_unit_price_microusd,
                c.id AS channel_id, re.provider_kind, re.base_url, re.credential_env,
                p.id AS price_plan_id, p.currency,
                p.text_input_microusd_per_million,
                p.image_input_microusd_per_million,
                p.text_output_microusd_per_million,
                p.image_output_microusd_per_million,
                rr.created_at AS captured_at,
                rr.markup_bps,
                {CANDIDATE_PRICING_COLUMNS},
                re.routing_priority,
                re.weight
            FROM publication.runtime_entries re
            JOIN publication.runtime_revisions rr ON rr.id = re.runtime_revision_id
            JOIN publication.gateway_models gm ON gm.gateway_model = re.gateway_model AND gm.enabled
            JOIN catalog.vendor_models vm ON vm.id = re.vendor_model_id
            JOIN supply.offerings o ON o.id = re.offering_id
            JOIN supply.channels c ON c.id = o.channel_id
            --  LEFT JOIN：渠道不按 token 计量量计价的供给没有 Price Plan。
            LEFT JOIN pricing.price_plans p ON p.id = re.price_plan_id
            WHERE re.active AND re.gateway_model = $1 AND {CANDIDATE_AVAILABLE_SQL}
            ORDER BY re.routing_priority ASC, o.id ASC
            "#
        )))
        .bind(gateway_model)
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        // 无 active 候选不是错误：由调用方判定「无合格候选」。返回空集合。
        let mut candidates = Vec::with_capacity(rows.len());
        for row in &rows {
            candidates.push(row_to_candidate(row)?);
        }
        // 防御：同一模型的 active 候选集必须**永远来自同一个 Revision**（发布即原子替换）。
        // 若出现跨 Revision 并存，说明发布语义被绕过——宁可在这里失败，也不要在路由时
        // 悄悄用一半旧候选挑供给。
        if let Some(first) = candidates.first() {
            let first_revision = first.runtime_revision_id;
            if candidates
                .iter()
                .any(|candidate| candidate.runtime_revision_id != first_revision)
            {
                return Err(ApplicationError::Persistence(format!(
                    "active candidates for model {gateway_model} span multiple runtime revisions"
                )));
            }
        }
        Ok(candidates)
    }

    /// 该型号当前生效修订里可被沿用的候选：供应商、渠道模型名与渠道三要素。
    ///
    /// 与 [`Self::active_offering`] 的差别只有一处：**不过滤 `enabled`**。停用的候选也要能被沿用——
    /// "改价之后重新启用"是常见动作，按启用状态过滤会让它在改价时突然找不到，而那种失败看起来像
    /// "这个候选不存在"。其余判据（取当前生效修订、按网关模型名）与那条读一致。
    async fn active_offering_channels(
        &self,
        gateway_model: &str,
    ) -> Result<Vec<ActiveOfferingChannel>, ApplicationError> {
        let rows = sqlx::query(AssertSqlSafe(format!(
            r#"
            SELECT o.provider_model_id, o.adapter_key, c.provider_kind, c.base_url, c.credential_env,
                   o.id AS offering_id,
                   o.formula, o.cost_unit_price_microusd,
                   p.currency AS plan_currency,
                   p.text_input_microusd_per_million,
                   p.image_input_microusd_per_million,
                   p.text_output_microusd_per_million,
                   p.image_output_microusd_per_million,
                   p.source_url AS plan_source_url,
                   {CANDIDATE_PRICING_COLUMNS}
            FROM publication.runtime_entries re
            JOIN supply.offerings o ON o.id = re.offering_id
            JOIN supply.channels c ON c.id = o.channel_id
            JOIN publication.runtime_revisions r ON r.id = re.runtime_revision_id
            LEFT JOIN pricing.price_plans p ON p.id = re.price_plan_id
            WHERE re.active AND re.gateway_model = $1
            ORDER BY c.provider_kind, o.provider_model_id, o.id
            "#,
        )))
        .bind(gateway_model)
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        rows.iter()
            .map(|row| {
                use sqlx::Row as _;
                let currency: Option<String> =
                    row.try_get("plan_currency").map_err(database_error)?;
                // 没有 Price Plan 的形态（按张 / 按次 / 上游给金额）不该造一份空费率出来：
                // 造了会被"形态与参数不配套"的校验拒掉。
                let plan = match currency {
                    Some(currency) => Some(PricePlanRates {
                        currency,
                        text_input_microusd_per_million: to_u64(
                            row.try_get("text_input_microusd_per_million")
                                .map_err(database_error)?,
                        )?,
                        image_input_microusd_per_million: to_u64(
                            row.try_get("image_input_microusd_per_million")
                                .map_err(database_error)?,
                        )?,
                        text_output_microusd_per_million: to_u64(
                            row.try_get("text_output_microusd_per_million")
                                .map_err(database_error)?,
                        )?,
                        image_output_microusd_per_million: to_u64(
                            row.try_get("image_output_microusd_per_million")
                                .map_err(database_error)?,
                        )?,
                        source_url: row
                            .try_get::<Option<String>, _>("plan_source_url")
                            .map_err(database_error)?
                            .unwrap_or_default(),
                    }),
                    None => None,
                };
                // 修订上那几列是**按候选键的 jsonb 映射**，不是文本列——用 `row_candidate_pricing`
                // 的同一套读法（`candidate_pricing_entry` 按 offering 取那一格）。
                let offering_id: Uuid = row.try_get("offering_id").map_err(database_error)?;
                let pricing = row_candidate_pricing(row, offering_id)?;
                Ok(ActiveOfferingChannel {
                    provider_model_id: row.try_get("provider_model_id").map_err(database_error)?,
                    adapter_key: row.try_get("adapter_key").map_err(database_error)?,
                    provider_kind: row.try_get("provider_kind").map_err(database_error)?,
                    base_url: row.try_get("base_url").map_err(database_error)?,
                    credential_env: row.try_get("credential_env").map_err(database_error)?,
                    formula: row.try_get("formula").map_err(database_error)?,
                    plan,
                    cost_unit_price_microusd: row
                        .try_get::<Option<i64>, _>("cost_unit_price_microusd")
                        .map_err(database_error)?
                        .map(to_u64)
                        .transpose()?,
                    cost_currency: pricing.cost_currency,
                    reference_cost_microusd: pricing.reference_cost_microusd,
                    cost_basis: pricing.cost_basis.map(|basis| basis.as_str().to_owned()),
                    tier_prices: pricing.tier_prices,
                    floor_amounts: pricing.floor_amounts,
                })
            })
            .collect()
    }

    async fn offerings_by_id(
        &self,
        offering_ids: &[OfferingId],
    ) -> Result<Vec<ReferencedOffering>, ApplicationError> {
        // 一次点读（`= ANY`），不逐条查：引用式发布一次要解析整组候选。
        //
        // **不过滤 `enabled`**（供给的与渠道的都不滤）：停用由逐候选校验按活表判、并给出可读的
        // 理由；在这里按启用状态丢掉，会让"我选的那条停用了"表现为"这次发布少了一条候选"——
        // 而调用方是按传入标识逐个核对的，少一条正是它要报出来的那件事。
        //
        // 渠道费率取该 Offering **当前那行**（按 `created_at DESC, id DESC` 取第一条，同一批写入
        // 撞上同一时刻时用主键定序——与 `active_offering` 里"行序不保证稳定就得配一个稳定序"
        // 同一条纪律）。`pricing.price_plans` 与 `supply.offerings` 之间没有 `price_plan_id` 列，
        // 关联键是 `offering_id`（`0001` 的表形），且费率是**追加**形态：改费率写新行、旧行留着
        // 给已在那个时刻发布过的修订引用。
        //
        // 成本币种**不在 `supply.offerings` 里**（`0013` 只加了 `formula` 与单价）：它是发布期按
        // 候选声明的东西，落点是 Price Plan 的币种或修订的 `cost_currency` 映射。所以这里按
        // "实际生效的那个"取：有 Price Plan 就是它的币种，否则取这条供给最近一次发布声明的币种。
        // 按张 / 按次计价的供给两条都不能少——缺了它，引用式发布填不出草稿的成本币种，会被
        // "必须显式声明成本币种"拒掉。
        if offering_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<Uuid> = offering_ids
            .iter()
            .map(|offering_id| offering_id.0)
            .collect();
        let rows = sqlx::query(
            r#"
            SELECT
                o.id AS offering_id, o.adapter_key, o.provider_model_id, o.restrictions,
                o.carrier_schema, o.parameter_mapping, o.formula, o.cost_unit_price_microusd,
                vm.vendor_id, vm.native_model_id, vm.native_revision, vm.capability_schema,
                c.provider_kind, c.base_url, c.credential_env,
                p.currency AS plan_currency,
                p.text_input_microusd_per_million,
                p.image_input_microusd_per_million,
                p.text_output_microusd_per_million,
                p.image_output_microusd_per_million,
                p.source_url AS plan_source_url,
                d.declared_currency
            FROM supply.offerings o
            JOIN catalog.vendor_models vm ON vm.id = o.vendor_model_id
            JOIN supply.channels c ON c.id = o.channel_id
            --  LEFT JOIN：渠道不按 token 计量量计价的供给没有 Price Plan。
            LEFT JOIN pricing.price_plans p ON p.id = (
                SELECT id FROM pricing.price_plans
                WHERE offering_id = o.id
                ORDER BY created_at DESC, id DESC
                LIMIT 1
            )
            LEFT JOIN LATERAL (
                SELECT rr.cost_currency ->> o.id::text AS declared_currency
                FROM publication.runtime_entries re
                JOIN publication.runtime_revisions rr ON rr.id = re.runtime_revision_id
                WHERE re.offering_id = o.id
                  AND rr.cost_currency IS NOT NULL
                  AND jsonb_exists(rr.cost_currency, o.id::text)
                ORDER BY rr.created_at DESC, rr.id DESC
                LIMIT 1
            ) d ON true
            WHERE o.id = ANY($1)
            "#,
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        rows.iter()
            .map(|row| {
                let currency: Option<String> =
                    row.try_get("plan_currency").map_err(database_error)?;
                // 与其他读侧同一条口径：没有 Price Plan 的形态不造一份空费率出来——造了会被
                // "形态与参数不配套"的校验拒掉（见 `active_offering_channels`）。
                let plan = match currency {
                    Some(currency) => Some(PricePlanRates {
                        currency,
                        text_input_microusd_per_million: to_u64(
                            row.try_get("text_input_microusd_per_million")
                                .map_err(database_error)?,
                        )?,
                        image_input_microusd_per_million: to_u64(
                            row.try_get("image_input_microusd_per_million")
                                .map_err(database_error)?,
                        )?,
                        text_output_microusd_per_million: to_u64(
                            row.try_get("text_output_microusd_per_million")
                                .map_err(database_error)?,
                        )?,
                        image_output_microusd_per_million: to_u64(
                            row.try_get("image_output_microusd_per_million")
                                .map_err(database_error)?,
                        )?,
                        source_url: row
                            .try_get::<Option<String>, _>("plan_source_url")
                            .map_err(database_error)?
                            .unwrap_or_default(),
                    }),
                    None => None,
                };
                let cost_currency = plan.as_ref().map(|plan| plan.currency.clone()).or(row
                    .try_get::<Option<String>, _>("declared_currency")
                    .map_err(database_error)?);
                Ok(ReferencedOffering {
                    offering_id: OfferingId(row.try_get("offering_id").map_err(database_error)?),
                    vendor_id: row.try_get("vendor_id").map_err(database_error)?,
                    native_model_id: row.try_get("native_model_id").map_err(database_error)?,
                    native_revision: row.try_get("native_revision").map_err(database_error)?,
                    capability_schema: row.try_get("capability_schema").map_err(database_error)?,
                    provider_kind: row.try_get("provider_kind").map_err(database_error)?,
                    base_url: row.try_get("base_url").map_err(database_error)?,
                    credential_env: row.try_get("credential_env").map_err(database_error)?,
                    adapter_key: row.try_get("adapter_key").map_err(database_error)?,
                    provider_model_id: row.try_get("provider_model_id").map_err(database_error)?,
                    carrier_schema: row.try_get("carrier_schema").map_err(database_error)?,
                    parameter_mapping: row.try_get("parameter_mapping").map_err(database_error)?,
                    restrictions: row.try_get("restrictions").map_err(database_error)?,
                    formula: row.try_get("formula").map_err(database_error)?,
                    plan,
                    cost_currency,
                    cost_unit_price_microusd: row
                        .try_get::<Option<i64>, _>("cost_unit_price_microusd")
                        .map_err(database_error)?
                        .map(to_u64)
                        .transpose()?,
                })
            })
            .collect()
    }

    async fn selectable_offerings(&self) -> Result<Vec<SelectableOfferingView>, ApplicationError> {
        // 一条供给一项，排序键就是调用方的分组口径：`vendor_id` → `native_model_id` →
        // `provider_kind`，最后拿主键定序——行序不保证稳定，清单每刷一次就跳的话分组也跟着跳。
        //
        // 渠道费率取该 Offering **当前那行** Price Plan，关联方式与 [`HubRepository::offerings_by_id`]
        // 逐字相同：按 `created_at DESC, id DESC` 取第一条，费率是追加形态、旧行留给引用过它的修订。
        // 成本币种同样按"实际生效的那个"取：有 Price Plan 就是它的币种，否则取这条供给最近一次发布
        // 声明的币种——按张 / 按次的供给没有 Price Plan，缺了它清单上就只有一片空。
        let rows = sqlx::query(AssertSqlSafe(format!(
            r#"
            SELECT
                o.id AS offering_id, o.adapter_key, o.provider_model_id, o.formula,
                vm.vendor_id, vm.native_model_id, vm.native_revision,
                c.provider_kind,
                ({CANDIDATE_AVAILABLE_SQL}) AS enabled,
                p.currency AS plan_currency,
                p.text_input_microusd_per_million,
                p.image_input_microusd_per_million,
                p.text_output_microusd_per_million,
                p.image_output_microusd_per_million,
                p.source_url AS plan_source_url,
                d.declared_currency
            FROM supply.offerings o
            JOIN catalog.vendor_models vm ON vm.id = o.vendor_model_id
            JOIN supply.channels c ON c.id = o.channel_id
            --  LEFT JOIN：渠道不按 token 计量量计价的供给没有 Price Plan。
            LEFT JOIN pricing.price_plans p ON p.id = (
                SELECT id FROM pricing.price_plans
                WHERE offering_id = o.id
                ORDER BY created_at DESC, id DESC
                LIMIT 1
            )
            LEFT JOIN LATERAL (
                SELECT rr.cost_currency ->> o.id::text AS declared_currency
                FROM publication.runtime_entries re
                JOIN publication.runtime_revisions rr ON rr.id = re.runtime_revision_id
                WHERE re.offering_id = o.id
                  AND rr.cost_currency IS NOT NULL
                  AND jsonb_exists(rr.cost_currency, o.id::text)
                ORDER BY rr.created_at DESC, rr.id DESC
                LIMIT 1
            ) d ON true
            ORDER BY vm.vendor_id ASC, vm.native_model_id ASC, c.provider_kind ASC, o.id ASC
            "#
        )))
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        rows.iter()
            .map(|row| {
                let currency: Option<String> =
                    row.try_get("plan_currency").map_err(database_error)?;
                // 与其他读侧同一条口径：没有 Price Plan 的形态不造一份空费率出来——造了会被
                // "形态与参数不配套"的校验拒掉（见 `active_offering_channels`）。
                let cost_rates = match currency {
                    Some(currency) => Some(PricePlanRates {
                        currency,
                        text_input_microusd_per_million: to_u64(
                            row.try_get("text_input_microusd_per_million")
                                .map_err(database_error)?,
                        )?,
                        image_input_microusd_per_million: to_u64(
                            row.try_get("image_input_microusd_per_million")
                                .map_err(database_error)?,
                        )?,
                        text_output_microusd_per_million: to_u64(
                            row.try_get("text_output_microusd_per_million")
                                .map_err(database_error)?,
                        )?,
                        image_output_microusd_per_million: to_u64(
                            row.try_get("image_output_microusd_per_million")
                                .map_err(database_error)?,
                        )?,
                        source_url: row
                            .try_get::<Option<String>, _>("plan_source_url")
                            .map_err(database_error)?
                            .unwrap_or_default(),
                    }),
                    None => None,
                };
                let cost_currency = cost_rates
                    .as_ref()
                    .map(|rates| rates.currency.clone())
                    .or(row
                        .try_get::<Option<String>, _>("declared_currency")
                        .map_err(database_error)?);
                Ok(SelectableOfferingView {
                    offering_id: OfferingId(row.try_get("offering_id").map_err(database_error)?),
                    vendor_id: row.try_get("vendor_id").map_err(database_error)?,
                    native_model_id: row.try_get("native_model_id").map_err(database_error)?,
                    native_revision: row.try_get("native_revision").map_err(database_error)?,
                    provider_kind: row.try_get("provider_kind").map_err(database_error)?,
                    provider_model_id: row.try_get("provider_model_id").map_err(database_error)?,
                    adapter_key: row.try_get("adapter_key").map_err(database_error)?,
                    formula: row.try_get("formula").map_err(database_error)?,
                    cost_currency,
                    cost_rates,
                    enabled: row.try_get("enabled").map_err(database_error)?,
                })
            })
            .collect()
    }

    async fn enabled_offerings(
        &self,
        offering_ids: &[OfferingId],
    ) -> Result<HashSet<OfferingId>, ApplicationError> {
        // 按主键点读：一次一批（`= ANY`），不 JOIN 发布条目、不比修订标识。判据就是
        // `CANDIDATE_AVAILABLE_SQL` 那一条——供给自己启用，且它所在的渠道也启用。取不到行
        // （供给被删）与停用一样，都不在这个集合里。
        if offering_ids.is_empty() {
            return Ok(HashSet::new());
        }
        let ids: Vec<Uuid> = offering_ids
            .iter()
            .map(|offering_id| offering_id.0)
            .collect();
        let rows = sqlx::query_scalar::<_, Uuid>(AssertSqlSafe(format!(
            r#"
            SELECT o.id
            FROM supply.offerings o
            JOIN supply.channels c ON c.id = o.channel_id
            WHERE o.id = ANY($1) AND {CANDIDATE_AVAILABLE_SQL}
            "#
        )))
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        Ok(rows.into_iter().map(OfferingId).collect())
    }

    async fn published_models(&self) -> Result<Vec<PublishedModel>, ApplicationError> {
        // 判据与 `active_offering` **逐条对齐**（生效的发布条目 + 网关模型开着 + 启用的供给 +
        // 启用的渠道）：目录里列出的型号必须真的受理得起来。少判一条，就会出现"目录里有、
        // 提交时取不到候选"的型号——那种型号对调用方是 404，比不列更糟。
        //
        // 一个型号一条：正常情形下同一个型号的 active 条目来自同一次发布（发布即原子替换），
        // 只有发布语义被绕过才会跨修订并存；即便如此也按确定的顺序取一条，不把同一个名字列两遍。
        //
        // 这里取的是**发布的合同原文**：合同行不可变，替换对客名是投射那一步的事
        // （存的那份不动），否则旧 Job 事后读到的合同就与它受理时不一样了。
        let rows = sqlx::query(AssertSqlSafe(format!(
            r#"
            SELECT DISTINCT ON (re.gateway_model)
                re.gateway_model, vm.vendor_id, vm.native_revision, vm.capability_schema
            FROM publication.runtime_entries re
            JOIN publication.gateway_models gm ON gm.gateway_model = re.gateway_model AND gm.enabled
            JOIN catalog.vendor_models vm ON vm.id = re.vendor_model_id
            JOIN supply.offerings o ON o.id = re.offering_id
            JOIN supply.channels c ON c.id = o.channel_id
            WHERE re.active AND {CANDIDATE_AVAILABLE_SQL}
            ORDER BY re.gateway_model ASC, re.routing_priority ASC, vm.created_at DESC, vm.id ASC
            "#
        )))
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        rows.iter()
            .map(|row| {
                Ok(PublishedModel {
                    gateway_model: row.try_get("gateway_model").map_err(database_error)?,
                    vendor_id: row.try_get("vendor_id").map_err(database_error)?,
                    native_revision: row.try_get("native_revision").map_err(database_error)?,
                    capability_schema: row.try_get("capability_schema").map_err(database_error)?,
                })
            })
            .collect()
    }

    async fn gateway_models(&self) -> Result<Vec<GatewayModelView>, ApplicationError> {
        // 只读投影：生效条目（含**已停用**的候选，运营要能看出"为什么它调不动"）+
        // 修订（哪一次发布、什么时候发的）+ 厂商模型合同（厂商与原生名）+ 运维开关。
        //
        // 候选按 `routing_priority` 升序取；一条网关模型一项。开关行缺失的名字不会出现在这里
        // （它同时也在受理与目录里取不到），因此"列得出来"就等于"能停用、能受理"。
        //
        // 候选能不能走由 `CANDIDATE_AVAILABLE_SQL` 判一次、当列读回来——不在这里用 Rust 重算
        // 那几个开关的与：重算就是第三份判据，将来加一条闸门必然漏掉一处。
        let rows = sqlx::query(AssertSqlSafe(format!(
            r#"
            SELECT
                re.gateway_model, gm.enabled,
                vm.vendor_id, vm.native_model_id, vm.native_revision, vm.capability_schema,
                rr.id AS runtime_revision_id, rr.created_at AS published_at,
                o.id AS offering_id, re.adapter_key, re.provider_model_id,
                re.carrier_schema, re.parameter_mapping,
                re.provider_kind,
                ({CANDIDATE_AVAILABLE_SQL}) AS candidate_available,
                rr.markup_bps,
                {CANDIDATE_PRICING_COLUMNS},
                re.routing_priority,
                re.weight
            FROM publication.runtime_entries re
            JOIN publication.runtime_revisions rr ON rr.id = re.runtime_revision_id
            JOIN publication.gateway_models gm ON gm.gateway_model = re.gateway_model
            JOIN catalog.vendor_models vm ON vm.id = re.vendor_model_id
            JOIN supply.offerings o ON o.id = re.offering_id
            JOIN supply.channels c ON c.id = o.channel_id
            WHERE re.active
            ORDER BY re.gateway_model ASC, re.routing_priority ASC, o.id ASC
            "#
        )))
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        let mut views: Vec<GatewayModelView> = Vec::new();
        for row in &rows {
            // 行按名字排好序，所以同一个名字的候选连续出现：遇到新名字就起一条，否则挂上去。
            let name: String = row.try_get("gateway_model").map_err(database_error)?;
            let candidate = row_to_gateway_model_candidate(row)?;
            match views.last_mut() {
                Some(view) if view.gateway_model == name => view.candidates.push(candidate),
                _ => views.push(row_to_gateway_model(row, vec![candidate])?),
            }
        }
        Ok(views)
    }

    async fn set_gateway_model_enabled(
        &self,
        gateway_model: &str,
        enabled: bool,
        actor: &str,
    ) -> Result<(), ApplicationError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        // 只改开关这一列。定义（合同、候选集）不在这里，改了它等于绕过发布——那会让
        // "Job 固定受理时版本"失去依据。
        let updated = sqlx::query(
            r#"
            UPDATE publication.gateway_models
            SET enabled = $2, updated_at = now(), updated_by = $3
            WHERE gateway_model = $1
            "#,
        )
        .bind(gateway_model)
        .bind(enabled)
        .bind(actor)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        // 没发布过的名字不是"待创建的资源"：定义只能由发布产生，因此这里是"不存在"，
        // 而不是先建一行再让人以为它已经在售。
        if updated.rows_affected() == 0 {
            transaction.rollback().await.map_err(database_error)?;
            return Err(ApplicationError::NotFound(format!(
                "gateway model {gateway_model}"
            )));
        }
        insert_audit(
            &mut transaction,
            actor,
            "gateway_model.set_enabled",
            "gateway_model",
            gateway_model,
            &serde_json::json!({"enabled": enabled}),
        )
        .await?;
        transaction.commit().await.map_err(database_error)
    }

    async fn set_offering_enabled(
        &self,
        offering_id: OfferingId,
        enabled: bool,
        actor: &str,
    ) -> Result<Vec<String>, ApplicationError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        // 只改开关这一列：这条供给的定义（承载面、映射、计价）只在发布里，就地改它等于绕过发布。
        let updated = sqlx::query("UPDATE supply.offerings SET enabled = $2 WHERE id = $1")
            .bind(offering_id.0)
            .bind(enabled)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        // 不存在的供给不是"待创建的资源"：定义只能由发布产生，这里只改已经发布出来的行。
        if updated.rows_affected() == 0 {
            transaction.rollback().await.map_err(database_error)?;
            return Err(ApplicationError::NotFound(format!(
                "offering {offering_id}"
            )));
        }
        // 这次改动影响哪些网关模型的候选集：调用方拿它失效 route 缓存。候选集的修订标识不因
        // 启停而变，缓存里那份因此仍然"看起来是新的"——失效只是让命中率回来，停用本身的生效
        // 由受理路径命中缓存后按主键复核这两列兜住（判据与候选查询同一条），不依赖这次失效。
        let gateway_models =
            affected_gateway_models(&mut transaction, "re.offering_id = $1", offering_id.0).await?;
        insert_audit(
            &mut transaction,
            actor,
            "offering.set_enabled",
            "offering",
            &offering_id.to_string(),
            &serde_json::json!({"enabled": enabled}),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(gateway_models)
    }

    async fn set_channel_enabled(
        &self,
        channel_id: ChannelId,
        enabled: bool,
        actor: &str,
    ) -> Result<Vec<String>, ApplicationError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let updated = sqlx::query("UPDATE supply.channels SET enabled = $2 WHERE id = $1")
            .bind(channel_id.0)
            .bind(enabled)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        if updated.rows_affected() == 0 {
            transaction.rollback().await.map_err(database_error)?;
            return Err(ApplicationError::NotFound(format!("channel {channel_id}")));
        }
        // 渠道经它名下的供给影响候选集，判据与 `set_offering_enabled` 同一条。
        let gateway_models =
            affected_gateway_models(&mut transaction, "o.channel_id = $1", channel_id.0).await?;
        insert_audit(
            &mut transaction,
            actor,
            "channel.set_enabled",
            "channel",
            &channel_id.to_string(),
            &serde_json::json!({"enabled": enabled}),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(gateway_models)
    }

    async fn upsert_fx_rate(&self, rate: NewFxRate, actor: &str) -> Result<(), ApplicationError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        // 同一币种同一生效时刻只有一行：取值规则是"受理时刻生效的那一行"，两行同时刻就没有
        // 唯一答案。重录同一时刻是**改**那一行（录错了要能改回来），不是再添一行。
        //
        // 生效时刻缺省时**由数据库盖章**（`coalesce($4, now())`），不让调用方替它读一个进程
        // 时钟：发布期校验与受理取值都拿库的 `now()` 去比，盖章的时钟若不是同一个，"录完立刻
        // 发布"就会在宿主与容器时钟漂移的不利方向上被判成"该币种还没有生效的折算率"。钱与
        // 生效时刻只能认一个时钟。
        //
        // `RETURNING` 取回**库里最终那一行的时刻**：审计要记的是真正落库的时刻，而不是请求里
        // 那个可能为空的入参。
        let effective_at: DateTime<Utc> = sqlx::query_scalar(
            r#"
            INSERT INTO pricing.fx_rates (id, currency, rate_micros, effective_at, created_by)
            VALUES ($1, $2, $3, coalesce($4, now()), $5)
            ON CONFLICT (currency, effective_at)
            DO UPDATE SET rate_micros = EXCLUDED.rate_micros, created_by = EXCLUDED.created_by
            RETURNING effective_at
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(&rate.currency)
        .bind(to_i64(rate.rate_micros)?)
        .bind(rate.effective_at)
        .bind(actor)
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        insert_audit(
            &mut transaction,
            actor,
            "fx_rate.upsert",
            "fx_rate",
            &rate.currency,
            &serde_json::json!({
                "rate_micros": rate.rate_micros,
                "effective_at": effective_at,
            }),
        )
        .await?;
        transaction.commit().await.map_err(database_error)
    }

    async fn effective_fx_rate(&self, currency: &str) -> Result<Option<FxRate>, ApplicationError> {
        // 取值规则：**受理时刻之前已生效、其中最新的一行**。不用"最新一行"是因为后录入的行可以
        // 生效时间在未来（调价预告）——那样受理时该用的仍是旧那一行。
        let row = sqlx::query(
            r#"
            SELECT currency, rate_micros, effective_at
            FROM pricing.fx_rates
            WHERE currency = $1 AND effective_at <= now()
            ORDER BY effective_at DESC
            LIMIT 1
            "#,
        )
        .bind(currency)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)?;
        row.map(|row| {
            Ok(FxRate {
                currency: row.try_get("currency").map_err(database_error)?,
                rate_micros: to_u64(row.try_get("rate_micros").map_err(database_error)?)?,
                effective_at: row.try_get("effective_at").map_err(database_error)?,
            })
        })
        .transpose()
    }

    async fn current_fx_rates(
        &self,
    ) -> Result<Vec<(String, u64, DateTime<Utc>)>, ApplicationError> {
        // 每个币种取**当前生效**的那一行：与 `effective_fx_rate` 同一条取值规则（此刻之前已生效、
        // 其中最新的一行），只是对全部币种各取一条。`DISTINCT ON` 让这件事一次查询做完，页面看到
        // 的就是平台真正在用的数。
        let rows = sqlx::query(
            r#"
            SELECT DISTINCT ON (currency) currency, rate_micros, effective_at
            FROM pricing.fx_rates
            WHERE effective_at <= now()
            ORDER BY currency, effective_at DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        rows.into_iter()
            .map(|row| {
                Ok((
                    row.try_get("currency").map_err(database_error)?,
                    to_u64(row.try_get("rate_micros").map_err(database_error)?)?,
                    row.try_get("effective_at").map_err(database_error)?,
                ))
            })
            .collect()
    }

    async fn customer_usage(
        &self,
        account_id: AccountId,
        query: CustomerBillingQuery,
    ) -> Result<Vec<CustomerUsageView>, ApplicationError> {
        let rows = sqlx::query(
            r#"
            SELECT
                j.gateway_model,
                j.state,
                j.branch,
                j.created_at,
                COALESCE(jsonb_array_length(j.result_images), 0)::bigint AS image_count,
                COALESCE(charged.total, 0)::bigint AS charged_microusd
            FROM generation.jobs j
            LEFT JOIN (
                SELECT job_id, SUM(amount_microusd)::bigint AS total
                FROM ledger.entries
                WHERE account_id = $1 AND kind = 'capture' AND job_id IS NOT NULL
                  AND ($2::timestamptz IS NULL OR created_at >= $2)
                  AND ($3::timestamptz IS NULL OR created_at < $3)
                GROUP BY job_id
            ) charged ON charged.job_id = j.id
            WHERE j.account_id = $1
              AND ($2::timestamptz IS NULL OR j.created_at >= $2)
              AND ($3::timestamptz IS NULL OR j.created_at < $3)
            ORDER BY j.created_at DESC
            LIMIT $4
            "#,
        )
        .bind(account_id.0)
        .bind(query.since)
        .bind(query.until)
        .bind(i64::from(query.limit))
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        rows.into_iter()
            .map(|row| {
                let state: String = row.try_get("state").map_err(database_error)?;
                let branch: String = row.try_get("branch").map_err(database_error)?;
                let image_count: i64 = row.try_get("image_count").map_err(database_error)?;
                Ok(CustomerUsageView {
                    gateway_model: row.try_get("gateway_model").map_err(database_error)?,
                    status: customer_usage_status(parse_state(&state)?),
                    kind: match branch.as_str() {
                        "edit" => CustomerUsageKind::Edit,
                        _ => CustomerUsageKind::Generation,
                    },
                    created_at: row.try_get("created_at").map_err(database_error)?,
                    image_count: u32::try_from(image_count).unwrap_or(0),
                    charged_microusd: row.try_get("charged_microusd").map_err(database_error)?,
                })
            })
            .collect()
    }

    async fn customer_billing(
        &self,
        account_id: AccountId,
        query: CustomerBillingQuery,
    ) -> Result<CustomerBillingSummary, ApplicationError> {
        // 请求数与产出张数按**执行记录**数，扣费总额按账本条目：与逐笔明细说的是同一批事实。
        let row = sqlx::query(
            r#"
            SELECT
                (SELECT COUNT(*) FROM generation.jobs j
                 WHERE j.account_id = $1
                   AND ($2::timestamptz IS NULL OR j.created_at >= $2)
                   AND ($3::timestamptz IS NULL OR j.created_at < $3)) AS requests,
                (SELECT COALESCE(SUM(jsonb_array_length(j.result_images)), 0)::bigint
                 FROM generation.jobs j
                 WHERE j.account_id = $1
                   AND ($2::timestamptz IS NULL OR j.created_at >= $2)
                   AND ($3::timestamptz IS NULL OR j.created_at < $3)) AS images,
                (SELECT COALESCE(SUM(e.amount_microusd), 0)::bigint FROM ledger.entries e
                 WHERE e.account_id = $1 AND e.kind IN ('capture', 'adjustment')
                   AND ($2::timestamptz IS NULL OR e.created_at >= $2)
                   AND ($3::timestamptz IS NULL OR e.created_at < $3)) AS charged_microusd
            "#,
        )
        .bind(account_id.0)
        .bind(query.since)
        .bind(query.until)
        .fetch_one(&self.pool)
        .await
        .map_err(database_error)?;
        Ok(CustomerBillingSummary {
            requests: row.try_get("requests").map_err(database_error)?,
            images: row.try_get("images").map_err(database_error)?,
            charged_microusd: row.try_get("charged_microusd").map_err(database_error)?,
        })
    }

    async fn list_api_keys(
        &self,
        account_id: AccountId,
    ) -> Result<Vec<ApiKeyView>, ApplicationError> {
        let rows = sqlx::query(
            r#"
            SELECT id, label, created_at, revoked_at
            FROM identity.api_keys
            WHERE account_id = $1
            ORDER BY created_at DESC
            "#,
        )
        .bind(account_id.0)
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        rows.into_iter()
            .map(|row| {
                Ok(ApiKeyView {
                    key_id: row.try_get("id").map_err(database_error)?,
                    label: row.try_get("label").map_err(database_error)?,
                    created_at: row.try_get("created_at").map_err(database_error)?,
                    revoked_at: row.try_get("revoked_at").map_err(database_error)?,
                })
            })
            .collect()
    }

    async fn revoke_api_key_of_account(
        &self,
        account_id: AccountId,
        key_id: Uuid,
        actor: &str,
    ) -> Result<bool, ApplicationError> {
        // 判据是"账户 + 密钥标识"一起收窄：不属于这个账户的那把改不到行，调用方据此回 404，
        // 而不是先查存在再判归属（那样 403/404 的差异会泄露"别人的密钥存在吗"）。
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let revoked: Option<Uuid> = sqlx::query_scalar(
            r#"
            UPDATE identity.api_keys
            SET revoked_at = now()
            WHERE id = $1 AND account_id = $2 AND revoked_at IS NULL
            RETURNING id
            "#,
        )
        .bind(key_id)
        .bind(account_id.0)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        if revoked.is_none() {
            // 还可能本来就是这个账户的、且**已经**吊销过：那是幂等成功，只写审计没必要，
            // 直接告诉调用方"它现在不可用"。只有确实不属于这个账户才回 false。
            let belongs: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM identity.api_keys WHERE id = $1 AND account_id = $2",
            )
            .bind(key_id)
            .bind(account_id.0)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(database_error)?;
            transaction.commit().await.map_err(database_error)?;
            return Ok(belongs.is_some());
        }
        insert_audit(
            &mut transaction,
            actor,
            "api_key.revoke",
            "account",
            &account_id.to_string(),
            &serde_json::json!({"key_id": key_id}),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(true)
    }

    async fn provider_cost_gaps(
        &self,
        limit: u32,
    ) -> Result<Vec<ProviderCostGapView>, ApplicationError> {
        // 判据只有一条：**来源是 `unavailable`**（本该有金额却拿不到）。请求根本没交到渠道的
        // 失败执行四列全空、来源也是空，那不是"缺口"而是"没有成本事实"——两者处置不同，
        // 不能混在一个清单里。
        let rows = sqlx::query(
            r#"
            SELECT a.id AS attempt_id, a.job_id, j.account_id, j.gateway_model,
                   c.provider_kind, a.provider_trace_id, a.completed_at
            FROM generation.attempts a
            JOIN generation.jobs j ON j.id = a.job_id
            JOIN supply.channels c ON c.id = j.channel_id
            WHERE a.provider_cost_source = 'unavailable'
            ORDER BY a.completed_at DESC NULLS LAST, a.id ASC
            LIMIT $1
            "#,
        )
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        rows.iter()
            .map(|row| {
                Ok(ProviderCostGapView {
                    job_id: JobId(row.try_get("job_id").map_err(database_error)?),
                    attempt_id: AttemptId(row.try_get("attempt_id").map_err(database_error)?),
                    account_id: AccountId(row.try_get("account_id").map_err(database_error)?),
                    gateway_model: row.try_get("gateway_model").map_err(database_error)?,
                    provider_kind: row.try_get("provider_kind").map_err(database_error)?,
                    provider_trace_id: row.try_get("provider_trace_id").map_err(database_error)?,
                    completed_at: row.try_get("completed_at").map_err(database_error)?,
                })
            })
            .collect()
    }

    /// 读账户余额（权威在数据库那一行，**不读缓存**）。
    ///
    /// 这条读服务于运营查看与对账：缓存的值可能滞后、也可能来自对账覆盖，用它当答案会把
    /// 账实不符读成账实相符。账户不存在返回 [`ApplicationError::NotFound`]。
    async fn read_account_balance(
        &self,
        account_id: AccountId,
    ) -> Result<BalanceChange, ApplicationError> {
        self.account_balance(account_id).await
    }

    /// 列出账户：按创建时间倒序，可按邮箱（走 `identity.customers` 的绑定）或标签（走账户那一列）
    /// 精确收窄，两个条件同时给时是**与**。
    ///
    /// 用 `LEFT JOIN` 而不是 `JOIN`：没有登录身份的账户（运营直接建的、还没绑邮箱的那些）也必须
    /// 出现在列表里——按邮箱筛时它们自然落选，但不筛时必须看得见。邮箱比对大小写不敏感，与
    /// "登录用的那个邮箱"一致。
    async fn list_accounts(
        &self,
        email: Option<&str>,
        tag: Option<&str>,
        limit: u32,
    ) -> Result<Vec<AccountSummary>, ApplicationError> {
        let rows = sqlx::query(
            r#"
            SELECT a.id, a.balance_microusd, a.tag, c.email, a.created_at, a.updated_at
            FROM ledger.accounts a
            LEFT JOIN identity.customers c ON c.account_id = a.id
            WHERE ($1::text IS NULL OR lower(c.email) = lower($1))
              AND ($2::text IS NULL OR a.tag = $2)
            ORDER BY a.created_at DESC, a.id DESC
            LIMIT $3
            "#,
        )
        .bind(email)
        .bind(tag)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        rows.iter()
            .map(|row| {
                use sqlx::Row as _;
                Ok(AccountSummary {
                    account_id: AccountId(row.try_get("id").map_err(database_error)?),
                    balance_microusd: row.try_get("balance_microusd").map_err(database_error)?,
                    tag: row.try_get("tag").map_err(database_error)?,
                    // 一个账户最多一个登录身份，所以 LEFT JOIN 至多带出一行、不会重复账户。
                    email: row.try_get("email").map_err(database_error)?,
                    created_at: row.try_get("created_at").map_err(database_error)?,
                    updated_at: row.try_get("updated_at").map_err(database_error)?,
                })
            })
            .collect()
    }

    /// 按账户读账本流水：时间**倒序**、`[since, until)` 半开区间、`offset` 翻页、`limit` 截断。
    ///
    /// 先判账户在不在，再取分录：一条不存在的账户与"这个账户还没有任何流水"必须分得开，否则
    /// 管理员面会把 404 说成"没有账目"。判据是 `ledger.accounts` 那一行——账本的账户事实只有
    /// 这一处。排序带 `id` 作次级键：`ORDER BY` 不完全定序时分页会漏条或重条，同一事务里写的
    /// 多条尤其会并列（`created_at` 取的是事务时间）。
    async fn read_ledger_entries(
        &self,
        account_id: AccountId,
        since: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<LedgerEntry>, ApplicationError> {
        if !self.account_exists(account_id).await? {
            return Err(ApplicationError::NotFound(format!("account {account_id}")));
        }
        let rows = sqlx::query(AssertSqlSafe(format!(
            r#"
            SELECT account_id, job_id, kind, amount_microusd, created_at
            FROM ledger.entries
            WHERE account_id = $1 AND {LEDGER_RANGE_PREDICATE}
            ORDER BY created_at DESC, id DESC
            OFFSET $4 LIMIT $5
            "#
        )))
        .bind(account_id.0)
        .bind(since)
        .bind(until)
        .bind(i64::from(offset))
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        rows.iter().map(ledger_entry_from_row).collect()
    }

    /// 该账户在 `[since, until)` 内的流水条数。
    ///
    /// 区间谓词与 `read_ledger_entries` **共用一处常量**：两处口径不一致的话，"还有没有下一页"的判断
    /// 就会错位，而那种错位在条数恰好落在边界上时才显形。
    async fn count_ledger_entries(
        &self,
        account_id: AccountId,
        since: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
    ) -> Result<u64, ApplicationError> {
        if !self.account_exists(account_id).await? {
            return Err(ApplicationError::NotFound(format!("account {account_id}")));
        }
        let count: i64 = sqlx::query_scalar(AssertSqlSafe(format!(
            r#"
            SELECT count(*)::bigint
            FROM ledger.entries
            WHERE account_id = $1 AND {LEDGER_RANGE_PREDICATE}
            "#
        )))
        .bind(account_id.0)
        .bind(since)
        .bind(until)
        .fetch_one(&self.pool)
        .await
        .map_err(database_error)?;
        Ok(count.max(0) as u64)
    }

    /// 该账户当前持有中的金额：`ledger.holds` 里还没结算的预授权之和。
    ///
    /// `COALESCE` 让"没有任何持有"读成 0，而不是"读不出来"：持有额是**合计**，空集合的合计就是
    /// 0。账户不存在仍然是 404——先判那一行在不在。
    async fn held_microusd(&self, account_id: AccountId) -> Result<i64, ApplicationError> {
        if !self.account_exists(account_id).await? {
            return Err(ApplicationError::NotFound(format!("account {account_id}")));
        }
        let held: i64 = sqlx::query_scalar(
            r#"
            SELECT COALESCE(sum(amount_microusd), 0)::bigint
            FROM ledger.holds
            WHERE account_id = $1 AND status = 'active'
            "#,
        )
        .bind(account_id.0)
        .fetch_one(&self.pool)
        .await
        .map_err(database_error)?;
        Ok(held)
    }

    /// 只探连接能不能用：`SELECT 1`，不碰任何业务表。
    async fn probe(&self) -> Result<(), ApplicationError> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(database_error)?;
        Ok(())
    }

    /// 生效的策略：按模型覆盖优先，其次全局那条。
    ///
    /// `ORDER BY gateway_model IS NULL` 把非空（覆盖）排在前面：策略的作用域是"有覆盖用覆盖、
    /// 没有用全局"，一条 SQL 就能定下来，不必让调用方分两次查再自己判优先级。
    async fn route_policy(
        &self,
        gateway_model: &str,
    ) -> Result<Option<RoutePolicy>, ApplicationError> {
        let row = sqlx::query(
            r#"
            SELECT gateway_model, strategy, discount_rates, tag_channel_map, version
            FROM routing.route_policies
            WHERE gateway_model = $1 OR gateway_model IS NULL
            ORDER BY gateway_model IS NULL
            LIMIT 1
            "#,
        )
        .bind(gateway_model)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)?;
        row.as_ref().map(route_policy_from_row).transpose()
    }

    /// 账户标签：只有 `user_tag` 策略消费它，所以它是一条单独的读，不与受理探针绑在一起。
    async fn account_tag(&self, account_id: AccountId) -> Result<Option<String>, ApplicationError> {
        let row = sqlx::query("SELECT tag FROM ledger.accounts WHERE id = $1")
            .bind(account_id.0)
            .fetch_optional(&self.pool)
            .await
            .map_err(database_error)?
            .ok_or_else(|| ApplicationError::NotFound(format!("account {account_id}")))?;
        row.try_get("tag").map_err(database_error)
    }

    /// 设账户标签。
    ///
    /// **不动 `updated_at`**：那一列是"余额最后一次变动"的时刻，对账与缓存新鲜度都按它判断；
    /// 改标签不是余额变动，动它会让对账以为这个账户刚有过一笔钱变动。
    async fn set_account_tag(
        &self,
        account_id: AccountId,
        tag: Option<&str>,
        actor: &str,
    ) -> Result<(), ApplicationError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let updated = sqlx::query("UPDATE ledger.accounts SET tag = $2 WHERE id = $1")
            .bind(account_id.0)
            .bind(tag)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        if updated.rows_affected() == 0 {
            return Err(ApplicationError::NotFound(format!("account {account_id}")));
        }
        insert_audit(
            &mut transaction,
            actor,
            "account.tag_set",
            "account",
            &account_id.to_string(),
            &json!({"tag": tag}),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(())
    }

    async fn upsert_route_policy(
        &self,
        policy: &RoutePolicy,
        actor: &str,
    ) -> Result<(), ApplicationError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        sqlx::query(
            r#"
            INSERT INTO routing.route_policies
                (id, gateway_model, strategy, discount_rates, tag_channel_map, version, updated_by)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (coalesce(gateway_model, ''))
            DO UPDATE SET strategy = EXCLUDED.strategy,
                          discount_rates = EXCLUDED.discount_rates,
                          tag_channel_map = EXCLUDED.tag_channel_map,
                          version = EXCLUDED.version,
                          updated_at = now(),
                          updated_by = EXCLUDED.updated_by
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(policy.gateway_model.as_deref())
        .bind(policy.strategy.as_str())
        .bind(serde_json::to_value(&policy.discount_rates).map_err(json_error)?)
        .bind(serde_json::to_value(&policy.tag_channel_map).map_err(json_error)?)
        .bind(&policy.version)
        .bind(actor)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        insert_audit(
            &mut transaction,
            actor,
            "route_policy.upsert",
            "route_policy",
            policy.gateway_model.as_deref().unwrap_or("global"),
            &json!({
                "strategy": policy.strategy.as_str(),
                "version": policy.version,
                "discount_rates": policy.discount_rates,
                "tag_channel_map": policy.tag_channel_map,
            }),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(())
    }

    async fn route_policies(&self) -> Result<Vec<RoutePolicy>, ApplicationError> {
        let rows = sqlx::query(
            r#"
            SELECT gateway_model, strategy, discount_rates, tag_channel_map, version
            FROM routing.route_policies
            ORDER BY gateway_model IS NULL DESC, gateway_model
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        rows.iter().map(route_policy_from_row).collect()
    }

    async fn create_account(
        &self,
        account_id: AccountId,
        initial_credit_microusd: u64,
        actor: &str,
    ) -> Result<BalanceChange, ApplicationError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let inserted = sqlx::query(
            r#"
            INSERT INTO ledger.accounts (id, balance_microusd) VALUES ($1, $2)
            RETURNING balance_microusd, updated_at
            "#,
        )
        .bind(account_id.0)
        .bind(to_i64(initial_credit_microusd)?)
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        if initial_credit_microusd > 0 {
            sqlx::query(
                r#"
                INSERT INTO ledger.entries
                    (id, account_id, kind, amount_microusd, business_key)
                VALUES ($1, $2, 'credit', $3, $4)
                "#,
            )
            .bind(Uuid::new_v4())
            .bind(account_id.0)
            .bind(to_i64(initial_credit_microusd)?)
            .bind(format!("account:{account_id}:initial"))
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        }
        insert_audit(
            &mut transaction,
            actor,
            "account.create",
            "account",
            &account_id.to_string(),
            &serde_json::json!({"initial_credit_microusd": initial_credit_microusd}),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        balance_change(&inserted, account_id)
    }

    async fn credit_account(
        &self,
        account_id: AccountId,
        amount_microusd: u64,
        business_key: &str,
        actor: &str,
    ) -> Result<BalanceChange, ApplicationError> {
        if amount_microusd == 0 {
            return Err(ApplicationError::Validation(
                "credit amount must be positive".to_owned(),
            ));
        }
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let inserted = sqlx::query(
            r#"
            INSERT INTO ledger.entries
                (id, account_id, kind, amount_microusd, business_key)
            VALUES ($1, $2, 'credit', $3, $4)
            ON CONFLICT (business_key) DO NOTHING
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(account_id.0)
        .bind(to_i64(amount_microusd)?)
        .bind(business_key)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        let change = if inserted.rows_affected() == 1 {
            let updated = sqlx::query(
                r#"
                UPDATE ledger.accounts
                SET balance_microusd = balance_microusd + $2, updated_at = now()
                WHERE id = $1
                RETURNING balance_microusd, updated_at
                "#,
            )
            .bind(account_id.0)
            .bind(to_i64(amount_microusd)?)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(database_error)?
            .ok_or_else(|| ApplicationError::NotFound(format!("account {account_id}")))?;
            insert_audit(
                &mut transaction,
                actor,
                "account.credit",
                "account",
                &account_id.to_string(),
                &serde_json::json!({"amount_microusd": amount_microusd, "business_key": business_key}),
            )
                .await?;
            balance_change(&updated, account_id)?
        } else {
            let existing = sqlx::query(
                "SELECT account_id, kind, amount_microusd FROM ledger.entries WHERE business_key = $1",
            )
            .bind(business_key)
            .fetch_one(&mut *transaction)
            .await
            .map_err(database_error)?;
            let existing_account: Uuid = existing.try_get("account_id").map_err(database_error)?;
            let existing_kind: String = existing.try_get("kind").map_err(database_error)?;
            let existing_amount: i64 = existing
                .try_get("amount_microusd")
                .map_err(database_error)?;
            if existing_account != account_id.0
                || existing_kind != "credit"
                || existing_amount != to_i64(amount_microusd)?
            {
                return Err(ApplicationError::Conflict(
                    "credit business_key was already used with different input".to_owned(),
                ));
            }
            // 幂等重放：这次没有改动余额，但返回**当前**余额——把缓存刷成数据库的值不会有坏处，
            // 而"重放后缓存还是旧的"会让下一次预检拿着一个过时的数去判。
            let current = sqlx::query(
                "SELECT balance_microusd, updated_at FROM ledger.accounts WHERE id = $1",
            )
            .bind(account_id.0)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(database_error)?
            .ok_or_else(|| ApplicationError::NotFound(format!("account {account_id}")))?;
            balance_change(&current, account_id)?
        };
        transaction.commit().await.map_err(database_error)?;
        Ok(change)
    }

    async fn acceptance_probe(
        &self,
        gateway_model: &str,
        account_id: AccountId,
        idempotency_key: &str,
    ) -> Result<AcceptanceProbe, ApplicationError> {
        // 一条查询读完四件事，且**永远返回一行**：没有这个网关模型时开关为 false、修订为空，
        // 受理侧对它的处置与"取不到任何候选"一样（对客是"模型不存在"）。
        //
        // 开关必须单独读：`PATCH enabled` 改的是可变表、不改变修订标识，只比对修订标识的话，
        // 关掉的模型会在 route 缓存的有效期内继续被受理。时钟也一起取回来，"缓存值新不新鲜"
        // 因此用的是数据库的时钟，不受进程与库之间漂移的影响。
        //
        // 重放这一项按 `(account_id, idempotency_key)` 的唯一索引判，是一次索引探测：它只决定
        // 余额预检该不该拦这一次请求，不参与任何金额判定。
        let row = sqlx::query(
            r#"
            SELECT
                now() AS database_now,
                COALESCE(
                    (SELECT gm.enabled FROM publication.gateway_models gm
                     WHERE gm.gateway_model = $1),
                    false
                ) AS enabled,
                (SELECT re.runtime_revision_id FROM publication.runtime_entries re
                 WHERE re.active AND re.gateway_model = $1
                 ORDER BY re.routing_priority ASC, re.offering_id ASC
                 LIMIT 1) AS runtime_revision_id,
                EXISTS (
                    SELECT 1 FROM generation.jobs j
                    WHERE j.account_id = $2 AND j.idempotency_key = $3
                ) AS replay
            "#,
        )
        .bind(gateway_model)
        .bind(account_id.0)
        .bind(idempotency_key)
        .fetch_one(&self.pool)
        .await
        .map_err(database_error)?;
        Ok(AcceptanceProbe {
            enabled: row.try_get("enabled").map_err(database_error)?,
            effective_revision_id: row
                .try_get::<Option<Uuid>, _>("runtime_revision_id")
                .map_err(database_error)?
                .map(RuntimeRevisionId),
            database_now: row.try_get("database_now").map_err(database_error)?,
            replay: row.try_get("replay").map_err(database_error)?,
        })
    }

    async fn accounts_updated_within(
        &self,
        window: StdDuration,
    ) -> Result<Vec<BalanceChange>, ApplicationError> {
        // 窗口在**库侧**算：`now() - $1` 用的是数据库的时钟，与写穿缓存时盖章的 `updated_at`
        // 同一个来源；换成进程时钟就会因为漂移漏掉刚变过的账户。
        //
        // 只要消费者账户：这条增量喂的是**余额缓存**，而平台账户不对客、没有缓存（它只在库里有
        // 一行，运营查成本流水时直接读库）。把平台账户也算进来，每记一笔成本就多一轮没有缓存
        // 条目可比的空检查。
        let rows = sqlx::query(
            r#"
            SELECT id, balance_microusd, updated_at FROM ledger.accounts
            WHERE kind = 'consumer' AND updated_at >= now() - make_interval(secs => $1)
            ORDER BY updated_at ASC, id ASC
            "#,
        )
        .bind(window.as_secs_f64())
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        rows.iter().map(balance_change_with_account).collect()
    }

    async fn insert_audit_event(
        &self,
        actor: &str,
        action: &str,
        subject_type: &str,
        subject_id: &str,
        payload: Value,
    ) -> Result<(), ApplicationError> {
        // 单独一个事务：调用点都是"业务已经定局、现在要留痕"，不该被业务事务回滚带走。
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        insert_audit(
            &mut transaction,
            actor,
            action,
            subject_type,
            subject_id,
            &payload,
        )
        .await?;
        transaction.commit().await.map_err(database_error)
    }

    /// 建一行密钥，返回它的 id。id 在这里生成，所以也只能由这里交回去——发密钥的响应要靠它定位
    /// "刚才发的是哪一把"，否则吊销就退化成"回库捞 id"。
    async fn create_api_key(
        &self,
        account_id: AccountId,
        label: &str,
        key_hash: &str,
        actor: &str,
    ) -> Result<Uuid, ApplicationError> {
        let key_id = Uuid::new_v4();
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        sqlx::query(
            r#"
            INSERT INTO identity.api_keys (id, account_id, label, key_hash)
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(key_id)
        .bind(account_id.0)
        .bind(label)
        .bind(key_hash)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        insert_audit(
            &mut transaction,
            actor,
            "api_key.create",
            "account",
            &account_id.to_string(),
            &serde_json::json!({"label": label}),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(key_id)
    }

    /// 按密钥标识取账户，**吊销判定就在这条语句里**（`revoked_at IS NULL`）。
    ///
    /// 认证链路上没有、也不能有"这把密钥还有效吗"的缓存：吊销的语义是"从这一刻起停止使用"，把它
    /// 缓存住就等于把吊销推迟到缓存过期之后——正是吊销要阻止的事。代价只是每个请求一次唯一索引
    /// 点查，换来吊销即生效。
    async fn api_key_identity(
        &self,
        key_hash: &str,
    ) -> Result<(Uuid, AccountId), ApplicationError> {
        let identity: (Uuid, Uuid) = sqlx::query_as(
            r#"
            SELECT id, account_id FROM identity.api_keys
            WHERE key_hash = $1 AND revoked_at IS NULL
            "#,
        )
        .bind(key_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)?
        .ok_or_else(|| ApplicationError::NotFound("api key".to_owned()))?;
        Ok((identity.0, AccountId(identity.1)))
    }

    async fn revoke_api_key(&self, key_id: Uuid, actor: &str) -> Result<(), ApplicationError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        // 只改**还没吊销**的那一行：一条语句就把"第一次吊销"与"重复吊销"分开了——重复调用影响 0 行。
        // 0 行既可能是"早就吊销过"（幂等成功）也可能是"这把键根本不存在"（404），下面再查一次区分。
        let revoked_account: Option<Uuid> = sqlx::query_scalar(
            r#"
            UPDATE identity.api_keys
            SET revoked_at = now()
            WHERE id = $1 AND revoked_at IS NULL
            RETURNING account_id
            "#,
        )
        .bind(key_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        let Some(account_id) = revoked_account else {
            let existing: Option<Uuid> =
                sqlx::query_scalar("SELECT account_id FROM identity.api_keys WHERE id = $1")
                    .bind(key_id)
                    .fetch_optional(&mut *transaction)
                    .await
                    .map_err(database_error)?;
            // 已经吊销过：幂等成功，也不再写一条"又吊销了一次"的审计——吊销是一件事、只发生一次，
            // 重复调用说明的是调用方不知道它已经生效，不是新的事实。
            return match existing {
                Some(_) => Ok(()),
                None => Err(ApplicationError::NotFound("api key".to_owned())),
            };
        };
        // 不删行：创建与吊销都是要留痕的历史事实，排障要看这把密钥什么时候被谁停掉。
        insert_audit(
            &mut transaction,
            actor,
            "api_key.revoke",
            "api_key",
            &key_id.to_string(),
            &serde_json::json!({"account_id": account_id.to_string()}),
        )
        .await?;
        transaction.commit().await.map_err(database_error)
    }

    async fn create_job(
        &self,
        command: CreateImageGeneration,
        branch: ImageBranch,
        offering: PublishedOffering,
        request_hash: String,
        routing: RoutingDecision,
    ) -> Result<(GenerationJob, BalanceChange), ApplicationError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!(
                "{}:{}",
                command.account_id, command.idempotency_key
            ))
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        if let Some(existing) = sqlx::query(
            "SELECT id, request_hash FROM generation.jobs WHERE account_id = $1 AND idempotency_key = $2",
        )
        .bind(command.account_id.0)
        .bind(&command.idempotency_key)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        {
            let existing_hash: String = existing.try_get("request_hash").map_err(database_error)?;
            let existing_id: Uuid = existing.try_get("id").map_err(database_error)?;
            transaction.rollback().await.map_err(database_error)?;
            if existing_hash != request_hash {
                return Err(ApplicationError::Conflict(
                    "idempotency key was already used with different input".to_owned(),
                ));
            }
            // 重放：这次没有扣减，但返回**当前**余额——缓存跟着刷成数据库的值不会有坏处，
            // 而"重放后缓存还留着旧数"会让下一次预检拿着过时的数去判。
            let job = self.load_generation_job(JobId(existing_id)).await?;
            let balance = self.account_balance(command.account_id).await?;
            return Ok((job, balance));
        }
        let max_cost = to_i64(command.max_cost_microusd)?;
        // 预授权扣减：`RETURNING` 把**扣减之后**的余额带出来，调用方据此写穿缓存。
        // 判据一字不动（`rows_affected != 1` ⇒ 余额不足），缓存从不参与这个判定。
        let reserved = sqlx::query(
            r#"
            UPDATE ledger.accounts
            SET balance_microusd = balance_microusd - $2, updated_at = now()
            WHERE id = $1 AND balance_microusd >= $2
            RETURNING balance_microusd, updated_at
            "#,
        )
        .bind(command.account_id.0)
        .bind(max_cost)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or(ApplicationError::InsufficientBalance)?;
        let job_id = JobId::new();
        let hold_id = Uuid::new_v4();
        let price_snapshot = serde_json::to_value(&offering.price_snapshot)
            .map_err(|error| ApplicationError::Persistence(error.to_string()))?;
        sqlx::query(
            r#"
            INSERT INTO generation.jobs (
                id, account_id, idempotency_key, request_hash, state, branch,
                gateway_model, native_parameters,
                runtime_revision_id, vendor_model_id, offering_id, channel_id,
                carrier_schema, parameter_mapping, adapter_key, provider_model_id,
                base_url, credential_env,
                price_snapshot, max_cost_microusd
            ) VALUES ($1,$2,$3,$4,'accepted',$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19)
            "#,
        )
        .bind(job_id.0)
        .bind(command.account_id.0)
        .bind(&command.idempotency_key)
        .bind(&request_hash)
        .bind(branch_name(branch))
        .bind(&command.gateway_model)
        .bind(&command.native_parameters)
        .bind(offering.runtime_revision_id.0)
        .bind(offering.vendor_model_id.0)
        .bind(offering.offering_id.0)
        .bind(offering.channel_id.0)
        .bind(&offering.carrier_schema)
        .bind(&offering.parameter_mapping)
        .bind(&offering.adapter_key)
        .bind(&offering.provider_model_id)
        // 入口与凭证名一并冻结：它们与被选中的这条候选同时定下，执行时不再回渠道行取。
        .bind(&offering.base_url)
        .bind(&offering.credential_env)
        .bind(&price_snapshot)
        .bind(max_cost)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            "INSERT INTO ledger.holds (id, account_id, job_id, amount_microusd, status) VALUES ($1,$2,$3,$4,'active')",
        )
        .bind(hold_id)
        .bind(command.account_id.0)
        .bind(job_id.0)
        .bind(max_cost)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            r#"
            INSERT INTO ledger.entries (id, account_id, job_id, kind, amount_microusd, business_key)
            VALUES ($1,$2,$3,'hold',$4,$5)
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(command.account_id.0)
        .bind(job_id.0)
        .bind(-max_cost)
        .bind(format!("job:{job_id}:hold"))
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        // 路由判定与 Job **同事务**写入。构造 Job 失败时不留下只写其一的中间态；
        // 幂等重放分支在上方已 rollback 并返回，不写本表。
        let considered = serde_json::to_value(&routing.considered)
            .map_err(|error| ApplicationError::Persistence(error.to_string()))?;
        sqlx::query(
            r#"
            INSERT INTO generation.routing_decisions
                (job_id, runtime_revision_id, chosen_offering_id, considered)
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(job_id.0)
        .bind(routing.runtime_revision_id.0)
        .bind(routing.chosen_offering_id.0)
        .bind(&considered)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        let balance = balance_change(&reserved, command.account_id)?;
        let now = Utc::now();
        Ok((
            GenerationJob {
                id: job_id,
                account_id: command.account_id,
                state: seeai_domain::JobState::Accepted,
                branch,
                gateway_model: command.gateway_model,
                native_parameters: command.native_parameters,
                offering,
                idempotency_key: command.idempotency_key,
                request_hash,
                max_cost_microusd: command.max_cost_microusd,
                created_at: now,
                updated_at: now,
            },
            balance,
        ))
    }

    async fn get_job(
        &self,
        account_id: AccountId,
        job_id: JobId,
    ) -> Result<JobView, ApplicationError> {
        let row = sqlx::query(
            r#"
            SELECT id, state, branch, gateway_model, result_images,
                   error_code, created_at, updated_at
            FROM generation.jobs WHERE id = $1 AND account_id = $2
            "#,
        )
        .bind(job_id.0)
        .bind(account_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)?
        .ok_or_else(|| ApplicationError::NotFound(format!("job {job_id}")))?;
        let images: Option<Value> = row.try_get("result_images").map_err(database_error)?;
        Ok(JobView {
            job_id: JobId(row.try_get("id").map_err(database_error)?),
            state: row.try_get("state").map_err(database_error)?,
            branch: parse_branch(row.try_get("branch").map_err(database_error)?)?,
            // 平台上就叫 `gateway_model`，对外接口叫 `model`；厂商原生名在 vendor_models 上。
            model: row.try_get("gateway_model").map_err(database_error)?,
            created_at: row.try_get("created_at").map_err(database_error)?,
            updated_at: row.try_get("updated_at").map_err(database_error)?,
            error_code: row.try_get("error_code").map_err(database_error)?,
            // 结果信封只在成功时写入；没写就是没有结果，不是空数组。
            data: images
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| ApplicationError::Persistence(error.to_string()))?,
        })
    }

    async fn claim_next_job(
        &self,
        worker_id: &str,
        lease_duration: ChronoDuration,
    ) -> Result<Option<ClaimedJob>, ApplicationError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let row = sqlx::query(
            r#"
            SELECT id FROM generation.jobs
            WHERE state = 'accepted' AND next_attempt_at <= now()
            ORDER BY created_at
            FOR UPDATE SKIP LOCKED
            LIMIT 1
            "#,
        )
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        let Some(row) = row else {
            transaction.commit().await.map_err(database_error)?;
            return Ok(None);
        };
        let job_id = JobId(row.try_get("id").map_err(database_error)?);
        let lease_expires_at = Utc::now() + lease_duration;
        sqlx::query(
            r#"
            UPDATE generation.jobs
            SET state = 'leased', lease_owner = $2, lease_expires_at = $3,
                version = version + 1, updated_at = now()
            WHERE id = $1 AND state = 'accepted'
            "#,
        )
        .bind(job_id.0)
        .bind(worker_id)
        .bind(lease_expires_at)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        let mut job = self.load_generation_job(job_id).await?;
        job.state = seeai_domain::JobState::Leased;
        Ok(Some(ClaimedJob {
            job,
            lease_owner: worker_id.to_owned(),
            lease_expires_at,
        }))
    }

    async fn recover_expired_leases(&self) -> Result<LeaseRecovery, ApplicationError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let returned_to_queue = sqlx::query(
            r#"
            UPDATE generation.jobs
            SET state = 'accepted', lease_owner = NULL, lease_expires_at = NULL,
                version = version + 1, updated_at = now()
            WHERE state = 'leased' AND lease_expires_at <= now()
            "#,
        )
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();

        let expired_submissions = sqlx::query(
            r#"
            SELECT j.id AS job_id, j.account_id, a.id AS attempt_id
            FROM generation.jobs j
            JOIN generation.attempts a ON a.job_id = j.id
            WHERE j.state = 'submitting' AND j.lease_expires_at <= now()
            FOR UPDATE OF j, a SKIP LOCKED
            "#,
        )
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;

        for row in &expired_submissions {
            let job_id: Uuid = row.try_get("job_id").map_err(database_error)?;
            let account_id: Uuid = row.try_get("account_id").map_err(database_error)?;
            let attempt_id: Uuid = row.try_get("attempt_id").map_err(database_error)?;
            sqlx::query(
                r#"
                UPDATE generation.attempts
                SET state = 'reconciliation_required',
                    provider_error_code = 'worker_lease_expired',
                    provider_error_message = 'worker lease expired after provider submission began',
                    completed_at = now()
                WHERE id = $1 AND state = 'submitting'
                "#,
            )
            .bind(attempt_id)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
            sqlx::query(
                r#"
                UPDATE generation.jobs
                SET state = 'reconciliation_required',
                    error_code = 'outcome_unknown',
                    error_message = 'the request outcome is unknown; see reconciliation',
                    failure_kind = 'platform_internal',
                    lease_owner = NULL, lease_expires_at = NULL,
                    version = version + 1, updated_at = now()
                WHERE id = $1 AND state = 'submitting'
                "#,
            )
            .bind(job_id)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
            sqlx::query(
                r#"
                INSERT INTO operations.reconciliation_cases
                    (id, job_id, attempt_id, account_id, reason)
                VALUES ($1,$2,$3,$4,'worker lease expired after provider submission began')
                ON CONFLICT (job_id) DO NOTHING
                "#,
            )
            .bind(Uuid::new_v4())
            .bind(job_id)
            .bind(attempt_id)
            .bind(account_id)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        }

        transaction.commit().await.map_err(database_error)?;
        Ok(LeaseRecovery {
            returned_to_queue,
            sent_to_reconciliation: u64::try_from(expired_submissions.len()).map_err(|_| {
                ApplicationError::Persistence("lease recovery count overflow".to_owned())
            })?,
        })
    }

    async fn begin_attempt(
        &self,
        job_id: JobId,
        worker_id: &str,
        attempt_id: AttemptId,
        request_digest: &str,
    ) -> Result<u32, ApplicationError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let updated = sqlx::query(
            r#"
            UPDATE generation.jobs
            SET state = 'submitting', version = version + 1, updated_at = now()
            WHERE id = $1 AND state = 'leased' AND lease_owner = $2 AND lease_expires_at > now()
            "#,
        )
        .bind(job_id.0)
        .bind(worker_id)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        if updated.rows_affected() != 1 {
            return Err(ApplicationError::Conflict(format!(
                "job {job_id} lease is not valid"
            )));
        }
        // 号在同一台 Job 的锁里算：这次 `UPDATE` 已经把 Job 那一行锁住（同事务、未提交），
        // 同一台 Job 的第二次执行拿不到锁，所以"现有行数 + 1"在并发下也是唯一的。
        let attempt_no: i32 = sqlx::query_scalar(
            r#"
            SELECT coalesce(max(attempt_no), 0) + 1 FROM generation.attempts WHERE job_id = $1
            "#,
        )
        .bind(job_id.0)
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        let attempt_no = u32::try_from(attempt_no).map_err(|_| {
            ApplicationError::Persistence(
                "attempt number overflows the 32-bit range used by the worker".to_owned(),
            )
        })?;
        sqlx::query(
            r#"
            INSERT INTO generation.attempts (id, job_id, state, request_digest, attempt_no)
            VALUES ($1,$2,'submitting',$3,$4)
            "#,
        )
        .bind(attempt_id.0)
        .bind(job_id.0)
        .bind(request_digest)
        .bind(i32::try_from(attempt_no).map_err(|_| {
            ApplicationError::Persistence("attempt number is out of range".to_owned())
        })?)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(attempt_no)
    }

    async fn requeue_after_unaccepted(
        &self,
        command: UnacceptedAttempt,
    ) -> Result<(), ApplicationError> {
        let UnacceptedAttempt {
            job_id,
            worker_id,
            attempt_id,
            attempt_no,
            failure,
            next_attempt_at,
        } = command;
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        // 这次执行的收尾：状态、渠道码、原文与**它自己那一行的成本四列**，与终态那次同一套口径
        // （逐次归）。所以重投产生的每一行都有本次执行的用量与成本，不合并、不覆盖。
        let provider_cost = failure.provider_cost.clone();
        let (cost_amount, cost_currency, cost_source, cost_cny) = match &provider_cost {
            Some(cost) => (
                cost.amount_microusd.map(to_i64).transpose()?,
                cost.currency.clone(),
                Some(cost.source.as_str()),
                cost.cny_microusd.map(to_i64).transpose()?,
            ),
            None => (None, None, None, None),
        };
        let completed = sqlx::query(
            r#"
            UPDATE generation.attempts
            SET state = 'failed', provider_trace_id = $4, provider_error_code = $5,
                provider_error_message = $6, provider_cost_microusd = $7,
                provider_cost_currency = $8, provider_cost_source = $9,
                provider_cost_cny_microusd = $10, completed_at = now()
            WHERE id = $1 AND job_id = $2 AND attempt_no = $3 AND state = 'submitting'
            "#,
        )
        .bind(attempt_id.0)
        .bind(job_id.0)
        .bind(i32::try_from(attempt_no).map_err(|_| {
            ApplicationError::Persistence("attempt number is out of range".to_owned())
        })?)
        .bind(&failure.trace_id)
        .bind(&failure.provider_code)
        .bind(&failure.message)
        .bind(cost_amount)
        .bind(&cost_currency)
        .bind(cost_source)
        .bind(cost_cny)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        if completed.rows_affected() != 1 {
            return Err(ApplicationError::Conflict(format!(
                "attempt {attempt_id} of job {job_id} is not submitting"
            )));
        }
        // Job 回到可领取：**预授权一动不动**（`ledger.holds` 与余额都不碰）。这次失败上游没开始
        // 计费，重投的不是一笔新业务；重新预授权等于把同一笔钱扣两遍，而释放再扣一遍也一样。
        // 结算与释放只发生在最后那次成功或用尽额度失败时；账本上也**不该**出现成本条目——上游
        // 没受理就没计费，这次没有任何成本事实可记。
        let requeued = sqlx::query(
            r#"
            UPDATE generation.jobs
            SET state = 'accepted', lease_owner = NULL, lease_expires_at = NULL,
                next_attempt_at = $3, version = version + 1, updated_at = now()
            WHERE id = $1 AND lease_owner = $2
            "#,
        )
        .bind(job_id.0)
        .bind(&worker_id)
        .bind(next_attempt_at)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        if requeued.rows_affected() != 1 {
            return Err(ApplicationError::Conflict(format!(
                "job {job_id} is not leased by {worker_id}"
            )));
        }
        transaction.commit().await.map_err(database_error)
    }

    async fn renew_lease(
        &self,
        job_id: JobId,
        worker_id: &str,
        lease_duration: ChronoDuration,
    ) -> Result<(), ApplicationError> {
        let lease_expires_at = Utc::now() + lease_duration;
        let updated = sqlx::query(
            r#"
            UPDATE generation.jobs
            SET lease_expires_at = $3, updated_at = now()
            WHERE id = $1 AND lease_owner = $2
              AND state IN ('leased', 'submitting')
            "#,
        )
        .bind(job_id.0)
        .bind(worker_id)
        .bind(lease_expires_at)
        .execute(&self.pool)
        .await
        .map_err(database_error)?;
        if updated.rows_affected() != 1 {
            return Err(ApplicationError::Conflict(format!(
                "job {job_id} lease cannot be renewed"
            )));
        }
        Ok(())
    }

    async fn complete_job(
        &self,
        completion: CompleteJob,
    ) -> Result<BalanceChange, ApplicationError> {
        let CompleteJob {
            job_id,
            worker_id,
            attempt_id,
            images,
            evidence,
            charge_microusd,
            provider_trace_id,
            provider_cost,
        } = completion;
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let row = sqlx::query(
            r#"
            SELECT account_id, max_cost_microusd FROM generation.jobs
            WHERE id = $1 AND state = 'submitting' AND lease_owner = $2
              AND lease_expires_at > now()
            FOR UPDATE
            "#,
        )
        .bind(job_id.0)
        .bind(&worker_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| ApplicationError::Conflict(format!("job {job_id} is not submitting")))?;
        let account_id = AccountId(row.try_get("account_id").map_err(database_error)?);
        let authorized: i64 = row.try_get("max_cost_microusd").map_err(database_error)?;
        let charge = to_i64(charge_microusd)?;
        // **不封顶在预授权额**：预授权只是保底，实收按实际用量算。实收超过保底额时差额把余额
        // 扣成负数（透支发生在结算，不在受理）——这是允许的结果，不是错误；下一次受理按当时的
        // 余额判（可能已为负）⇒ 402。把这里改成"超过就进对账"会把一笔正常完成的生成扣在对账里。
        if evidence.attempt_id != attempt_id {
            return Err(ApplicationError::Persistence(
                "metering evidence attempt does not match completion".to_owned(),
            ));
        }
        let evidence_json = serde_json::to_value(&evidence)
            .map_err(|error| ApplicationError::Persistence(error.to_string()))?;
        // 成本事实与计量证据**分开落**：计量事实是上游给的分项 token（在 `metering_evidence`
        // 里），成本是渠道报的钱或平台按实际用量自算的钱，只进毛利口径，不改对客金额。
        // 折算后 CNY 这一项由用例用**受理时冻结的汇率**算好——币种与那份汇率对不上时留 NULL
        // （"没有折算值"，不是 0）。
        let provider_cost_amount = provider_cost.amount_microusd.map(to_i64).transpose()?;
        let provider_cost_cny = provider_cost.cny_microusd.map(to_i64).transpose()?;
        sqlx::query(
            r#"
            UPDATE generation.attempts
            SET state = 'succeeded', response_digest = $3, metering_evidence = $4,
                provider_trace_id = $5, provider_cost_microusd = $6,
                provider_cost_currency = $7, provider_cost_source = $8,
                provider_cost_cny_microusd = $9, completed_at = now()
            WHERE id = $1 AND job_id = $2 AND state = 'submitting'
            "#,
        )
        .bind(attempt_id.0)
        .bind(job_id.0)
        .bind(&evidence.provider_response_digest)
        .bind(&evidence_json)
        .bind(&provider_trace_id)
        .bind(provider_cost_amount)
        .bind(&provider_cost.currency)
        .bind(provider_cost.source.as_str())
        .bind(provider_cost_cny)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        // 结果只是当次信封：渠道给什么就存什么，平台不下载、不归档。
        let result_images = serde_json::to_value(&images)
            .map_err(|error| ApplicationError::Persistence(error.to_string()))?;
        sqlx::query(
            r#"
            UPDATE generation.jobs
            SET state = 'succeeded', result_images = $3, lease_owner = NULL,
                lease_expires_at = NULL, version = version + 1, updated_at = now()
            WHERE id = $1 AND lease_owner = $2
            "#,
        )
        .bind(job_id.0)
        .bind(&worker_id)
        .bind(&result_images)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            "UPDATE ledger.holds SET status = 'captured', updated_at = now() WHERE job_id = $1 AND status = 'active'",
        )
        .bind(job_id.0)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        // 释放差额 = 预授权额 − 实收：估大了退回余额，**估小了这里就是负数**，余额被扣成负的
        // （透支在结算吸收）。库层的非负约束已放宽，所以这一步不再需要绕开。
        let refund = authorized - charge;
        // `RETURNING` 把结算之后的余额带出来：调用方据此写穿缓存（用户要求：扣减成功后立即同步）。
        let settled = sqlx::query(
            r#"
            UPDATE ledger.accounts
            SET balance_microusd = balance_microusd + $2, updated_at = now()
            WHERE id = $1
            RETURNING balance_microusd, updated_at
            "#,
        )
        .bind(account_id.0)
        .bind(refund)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| ApplicationError::NotFound(format!("account {account_id}")))?;
        insert_ledger_entry(
            &mut transaction,
            account_id,
            Some(job_id),
            "release",
            authorized,
            &format!("job:{job_id}:hold-release"),
        )
        .await?;
        insert_ledger_entry(
            &mut transaction,
            account_id,
            Some(job_id),
            "capture",
            -charge,
            &format!("job:{job_id}:capture"),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        balance_change(&settled, account_id)
    }

    async fn fail_job(
        &self,
        job_id: JobId,
        worker_id: &str,
        attempt_id: Option<AttemptId>,
        failure: AttemptFailure,
    ) -> Result<BalanceChange, ApplicationError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let row = sqlx::query(
            r#"
            SELECT account_id, max_cost_microusd, state
            FROM generation.jobs WHERE id = $1 AND lease_owner = $2 FOR UPDATE
            "#,
        )
        .bind(job_id.0)
        .bind(worker_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| ApplicationError::Conflict(format!("job {job_id} is not leased")))?;
        let account_id = AccountId(row.try_get("account_id").map_err(database_error)?);
        let held: i64 = row.try_get("max_cost_microusd").map_err(database_error)?;
        let next_state = match failure.target_state {
            seeai_domain::JobState::Failed => "failed",
            seeai_domain::JobState::ReconciliationRequired => "reconciliation_required",
            state => {
                return Err(ApplicationError::Persistence(format!(
                    "invalid failure target state {state}"
                )));
            }
        };
        if let Some(attempt_id) = attempt_id {
            // 成本事实与失败事实**一起写**：进对账那条路径上执行已经发生、上游成本也拿得到，
            // 只有成功路径才落成本，等于把"这一笔到底花了多少钱"丢在一条已经付过钱的路径上。
            // 端口接受"没有成本事实"（`None`）：那时四列留 NULL——那是"这次没有成本事实可落"，
            // 不是"成本是 0"。调用方那一侧拿不到成本时按 `unavailable` 落（来源可辨、进缺口
            // 清单），把 NULL 留给"根本没采"的执行。
            let provider_cost = failure.provider_cost.clone();
            let (cost_amount, cost_currency, cost_source, cost_cny) = match &provider_cost {
                Some(cost) => (
                    cost.amount_microusd.map(to_i64).transpose()?,
                    cost.currency.clone(),
                    Some(cost.source.as_str()),
                    cost.cny_microusd.map(to_i64).transpose()?,
                ),
                None => (None, None, None, None),
            };
            sqlx::query(
                r#"
                UPDATE generation.attempts
                SET state = $3, provider_trace_id = $4, provider_error_code = $5,
                    provider_error_message = $6, provider_cost_microusd = $7,
                    provider_cost_currency = $8, provider_cost_source = $9,
                    provider_cost_cny_microusd = $10, completed_at = now()
                WHERE id = $1 AND job_id = $2
                "#,
            )
            .bind(attempt_id.0)
            .bind(job_id.0)
            .bind(next_state)
            .bind(&failure.trace_id)
            .bind(&failure.provider_code)
            .bind(&failure.message)
            .bind(cost_amount)
            .bind(&cost_currency)
            .bind(cost_source)
            .bind(cost_cny)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
            // **平台自担的成本这一刻进账本**：执行收尾与成本事实落到它自己那行是同一件事，
            // 所以这里就是账上该看见这笔钱的时刻——与预授权怎么处置无关。金额为 0 或没有折算值
            // 时不写：那不是"花掉 0 元"，是"这次没有可记账的成本事实"（来源可辨，见成本缺口清单）。
            // 进对账那条路**不在这里之外再补一笔**：退款只是释放消费者的预授权，成本已经在它
            // 上面那次事务里记过，补记会被业务键的唯一约束挡下（同一笔执行只记一次）。
            if let Some(cny_microusd) = cost_cny.filter(|amount| *amount > 0) {
                insert_platform_cost(&mut transaction, job_id, attempt_id, cny_microusd).await?;
            }
        }
        sqlx::query(
            r#"
            UPDATE generation.jobs
            SET state = $3, error_code = $4, error_message = $5, failure_kind = $6,
                lease_owner = NULL, lease_expires_at = NULL,
                version = version + 1, updated_at = now()
            WHERE id = $1 AND lease_owner = $2
            "#,
        )
        .bind(job_id.0)
        .bind(worker_id)
        .bind(next_state)
        .bind(failure.public_code.as_str())
        .bind(failure.public_code.default_message())
        .bind(failure.kind.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        // 释放预授权那条分支会改动余额，把变更后的值留到提交之后带回去；保留预授权的那条
        // 不动余额，提交后读当前值。
        let mut released_balance = None;
        if failure.hold_disposition == HoldDisposition::RetainForReconciliation {
            if failure.target_state != seeai_domain::JobState::ReconciliationRequired {
                return Err(ApplicationError::Persistence(
                    "retained failure hold requires reconciliation state".to_owned(),
                ));
            }
            let attempt_id = attempt_id.ok_or_else(|| {
                ApplicationError::Persistence(
                    "reconciliation requires a provider attempt".to_owned(),
                )
            })?;
            sqlx::query(
                r#"
                INSERT INTO operations.reconciliation_cases
                    (id, job_id, attempt_id, account_id, reason)
                VALUES ($1,$2,$3,$4,$5)
                ON CONFLICT (job_id) DO NOTHING
                "#,
            )
            .bind(Uuid::new_v4())
            .bind(job_id.0)
            .bind(attempt_id.0)
            // 案例说到底问的是"哪个账户的钱出了问题"：执行类的案例也把账户写上，运营看清单时
            // 不必再回 Job 表捞一次。
            .bind(account_id.0)
            .bind(&failure.message)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        } else {
            if failure.target_state != seeai_domain::JobState::Failed {
                return Err(ApplicationError::Persistence(
                    "released failure hold requires failed state".to_owned(),
                ));
            }
            sqlx::query(
                "UPDATE ledger.holds SET status = 'released', updated_at = now() WHERE job_id = $1 AND status = 'active'",
            )
            .bind(job_id.0)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
            // 释放预授权：余额变了，所以这里要把变更后的值带回去（调用方要写穿缓存）。
            let released = sqlx::query(
                r#"
                UPDATE ledger.accounts
                SET balance_microusd = balance_microusd + $2, updated_at = now()
                WHERE id = $1
                RETURNING balance_microusd, updated_at
                "#,
            )
            .bind(account_id.0)
            .bind(held)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(database_error)?
            .ok_or_else(|| ApplicationError::NotFound(format!("account {account_id}")))?;
            insert_ledger_entry(
                &mut transaction,
                account_id,
                Some(job_id),
                "release",
                held,
                &format!("job:{job_id}:failure-release"),
            )
            .await?;
            released_balance = Some(balance_change(&released, account_id)?);
        }
        transaction.commit().await.map_err(database_error)?;
        // 保留预授权（进对账）的那条路径没有改动余额：返回**当前**余额，让缓存刷成数据库的值。
        match released_balance {
            Some(change) => Ok(change),
            None => self.account_balance(account_id).await,
        }
    }

    async fn count_in_flight_jobs(
        &self,
        account_id: AccountId,
        except_idempotency_key: &str,
    ) -> Result<u64, ApplicationError> {
        let count: i64 = sqlx::query_scalar(
            r#"
            SELECT count(*) FROM generation.jobs
            WHERE account_id = $1 AND state IN ($2, $3, $4) AND idempotency_key <> $5
            -- 在跑的三态：等执行、持有租约、正在调上游；终态与对账态不算在飞。
            -- 同一个幂等键的那个不算：重发要拿回原来那个 Job，不该被并发上限拒掉。
            "#,
        )
        .bind(account_id.0)
        .bind("accepted")
        .bind("leased")
        .bind("submitting")
        .bind(except_idempotency_key)
        .fetch_one(&self.pool)
        .await
        .map_err(database_error)?;
        Ok(u64::try_from(count).unwrap_or(u64::MAX))
    }

    async fn daily_spend_microusd(&self, account_id: AccountId) -> Result<u64, ApplicationError> {
        // **从账本读事实**：今天已经扣掉多少，只有这条路径能回答。按需聚合、不写缓存、
        // 也不读缓存——配额判的是"钱花到哪了"，拿一份可能过时的计数去判会放出不该放的请求。
        //
        // 判据是 `today`（数据库的 `now()` 落在哪个 UTC 自然日）：一天的边界应当由**事实的
        // 书写者**（数据库）划，而不是由受理进程的本地时区划；否则同一份账本在两个时区的
        // 进程眼里是两天的花销。
        //
        // `capture` 在结算时是**负数**（见结算那段：预授权释放一笔正数、实收一笔负数），
        // 所以这里取负数的相反数——"花掉多少"是正着说的。持有与释放都不是花费：预授权只是
        // 占位，它已经由余额那条路挡着；释放是把没花的退回去。既然余额不足时受理会拒，这个
        // 和式就不会为负；真出现负数（对账退款之类）也只归到 0，绝不折成一个巨大的 `u64`。
        let spent: i64 = sqlx::query_scalar(
            r#"
            SELECT COALESCE(sum(-e.amount_microusd), 0)::bigint
            FROM ledger.entries e
            WHERE e.account_id = $1
              AND e.kind = 'capture'
              AND e.amount_microusd < 0
              AND e.created_at >= date_trunc('day', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'
            "#,
        )
        .bind(account_id.0)
        .fetch_one(&self.pool)
        .await
        .map_err(database_error)?;
        Ok(u64::try_from(spent).unwrap_or(0))
    }

    async fn list_open_reconciliation_cases(
        &self,
    ) -> Result<Vec<ReconciliationCaseView>, ApplicationError> {
        // LEFT JOIN 而不是 INNER：账户级的账实案例没有 Job（也就没有 Attempt），用 INNER 会让它
        // 从这个清单里消失——而那正是最需要人看见的一类。账户取案例自己那一列：两种来源的案例都
        // 写了它（执行类案例建案时由它那个 Job 给出）。
        let rows = sqlx::query(
            r#"
            SELECT rc.id, rc.job_id, rc.attempt_id, rc.account_id, rc.reason, rc.created_at,
                   a.provider_trace_id
            FROM operations.reconciliation_cases rc
            LEFT JOIN generation.attempts a ON a.id = rc.attempt_id
            WHERE rc.status = 'open'
            ORDER BY rc.created_at
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        rows.into_iter()
            .map(|row| {
                Ok(ReconciliationCaseView {
                    id: row.try_get("id").map_err(database_error)?,
                    job_id: row
                        .try_get::<Option<Uuid>, _>("job_id")
                        .map_err(database_error)?
                        .map(JobId),
                    attempt_id: row
                        .try_get::<Option<Uuid>, _>("attempt_id")
                        .map_err(database_error)?
                        .map(AttemptId),
                    account_id: AccountId(row.try_get("account_id").map_err(database_error)?),
                    reason: row.try_get("reason").map_err(database_error)?,
                    provider_trace_id: row.try_get("provider_trace_id").map_err(database_error)?,
                    created_at: row.try_get("created_at").map_err(database_error)?,
                })
            })
            .collect()
    }

    async fn accounts_with_ledger_mismatch(
        &self,
    ) -> Result<Vec<LedgerBalanceMismatch>, ApplicationError> {
        // 一次全表比对：账户行上的余额 vs 它自己账本条目的符号和。两个数都来自库，缓存不参与，
        // 这条 SQL 也不写任何一行——"发现"是它的全部职责（改账是人的决定）。
        //
        // `HAVING` 里重算一次和而不是引用别名：余额与账本两边都取自 GROUP BY 的同一批行，
        // 这样写不依赖任何 SELECT 别名的解析顺序。`COALESCE` 让"一条条目都没有"的账户按 0 比，
        // 于是"余额非 0 却没有任何条目"这种形态也报得出来。
        let rows = sqlx::query(
            r#"
            SELECT a.id AS account_id,
                   a.balance_microusd,
                   COALESCE(sum(e.amount_microusd), 0)::bigint AS ledger_total_microusd
            FROM ledger.accounts a
            LEFT JOIN ledger.entries e ON e.account_id = a.id
            GROUP BY a.id, a.balance_microusd
            HAVING a.balance_microusd <> COALESCE(sum(e.amount_microusd), 0)
            ORDER BY a.id
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        rows.iter()
            .map(|row| {
                Ok(LedgerBalanceMismatch {
                    account_id: AccountId(row.try_get("account_id").map_err(database_error)?),
                    ledger_total_microusd: row
                        .try_get("ledger_total_microusd")
                        .map_err(database_error)?,
                    balance_microusd: row.try_get("balance_microusd").map_err(database_error)?,
                })
            })
            .collect()
    }

    async fn open_ledger_reconciliation_case(
        &self,
        command: OpenLedgerCaseCommand,
    ) -> Result<bool, ApplicationError> {
        // `ON CONFLICT DO NOTHING` 落在那条"一个账户同时只留一条未结案账实案例"的部分唯一索引
        // 上：核对是周期跑的，没有它每跑一轮就多一条同样的案例。插入被挡下时返回 `false`，
        // 调用方据此**不重复外发告警**（见 `LedgerAuditor::audit_once`）。
        //
        // 这条案例**不写** `job_id` / `attempt_id`（两列留空）：它指向的是一条账户的余额与账本，
        // 不是某一次执行。
        let reason = format!(
            "ledger entries total {} microusd does not match the account balance {} microusd",
            command.ledger_total_microusd, command.balance_microusd
        );
        let inserted = sqlx::query(
            r#"
            INSERT INTO operations.reconciliation_cases (id, account_id, reason)
            VALUES ($1, $2, $3)
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(command.account_id.0)
        .bind(&reason)
        .execute(&self.pool)
        .await
        .map_err(database_error)?;
        Ok(inserted.rows_affected() == 1)
    }

    async fn provider_failures(
        &self,
        query: ProviderFailureQuery,
    ) -> Result<Vec<ProviderFailureView>, ApplicationError> {
        let kinds: Option<Vec<String>> = if query.kinds.is_empty() {
            None
        } else {
            Some(
                query
                    .kinds
                    .iter()
                    .map(|kind| kind.as_str().to_owned())
                    .collect(),
            )
        };
        let rows = sqlx::query(
            r#"
            SELECT j.id AS job_id, j.account_id, j.gateway_model, j.offering_id,
                   c.provider_kind, j.failure_kind, j.error_code, j.updated_at,
                   a.provider_trace_id, a.provider_error_code, a.provider_error_message
            FROM generation.jobs j
            -- 一个 Job 现在可以有多行 Attempt（只有"可证明未受理"才重投）。运营面列的是**失败
            -- 的 Job**，所以取**最后一次**执行的记录：一次重投之后还挂着第一次的渠道码，运营
            -- 看到的会是已经被重投消解掉的那次失败的原因。`LATERAL` 而不是普通 JOIN 加分组，
            -- 是为了让"一条 Job 一行"由查询本身保证——分成多行会让下面的条数与 limit 一起失真。
            LEFT JOIN LATERAL (
                SELECT provider_trace_id, provider_error_code, provider_error_message
                FROM generation.attempts
                WHERE job_id = j.id
                ORDER BY attempt_no DESC
                LIMIT 1
            ) a ON true
            LEFT JOIN supply.channels c ON c.id = j.channel_id
            WHERE j.failure_kind IS NOT NULL
              AND ($1::text[] IS NULL OR j.failure_kind = ANY($1))
              AND ($2::timestamptz IS NULL OR j.updated_at >= $2)
            ORDER BY j.updated_at DESC, j.id
            LIMIT $3
            "#,
        )
        .bind(kinds)
        .bind(query.since)
        .bind(i64::from(query.limit))
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        rows.into_iter()
            .map(|row| {
                let stored_kind: String = row.try_get("failure_kind").map_err(database_error)?;
                let kind = ProviderFailureKind::parse(&stored_kind).ok_or_else(|| {
                    ApplicationError::Persistence(format!(
                        "unknown platform failure kind in storage: {stored_kind}"
                    ))
                })?;
                let stored_code: String = row.try_get("error_code").map_err(database_error)?;
                let error_code = PublicErrorCode::parse(&stored_code).ok_or_else(|| {
                    ApplicationError::Persistence(format!(
                        "unknown public error code in storage: {stored_code}"
                    ))
                })?;
                Ok(ProviderFailureView {
                    job_id: JobId(row.try_get("job_id").map_err(database_error)?),
                    account_id: AccountId(row.try_get("account_id").map_err(database_error)?),
                    gateway_model: row.try_get("gateway_model").map_err(database_error)?,
                    offering_id: OfferingId(row.try_get("offering_id").map_err(database_error)?),
                    provider_kind: row.try_get("provider_kind").map_err(database_error)?,
                    kind,
                    error_code,
                    provider_trace_id: row.try_get("provider_trace_id").map_err(database_error)?,
                    provider_error_code: row
                        .try_get("provider_error_code")
                        .map_err(database_error)?,
                    provider_error_message: row
                        .try_get("provider_error_message")
                        .map_err(database_error)?,
                    updated_at: row.try_get("updated_at").map_err(database_error)?,
                })
            })
            .collect()
    }

    /// 某条候选最近若干次终态执行里的连续失败次数。
    ///
    /// 只取终态（`succeeded` / `failed` / `reconciliation_required`）并按 `updated_at` 倒序，
    /// 于是"开头有几个不是成功"就是连续失败次数；在队或在跑的那些没有结论，不参与。
    /// `LIMIT` 用调用方给的窗口：要判"够不够 N 次"就不必再往回读。
    async fn consecutive_offering_failures(
        &self,
        offering_id: OfferingId,
        window: u32,
    ) -> Result<u64, ApplicationError> {
        let states: Vec<String> = sqlx::query_scalar(
            r#"
            SELECT state FROM generation.jobs
            WHERE offering_id = $1
              AND state IN ('succeeded', 'failed', 'reconciliation_required')
            ORDER BY updated_at DESC, id DESC
            LIMIT $2
            "#,
        )
        .bind(offering_id.0)
        .bind(i64::from(window))
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        let streak = states
            .iter()
            .take_while(|state| *state != "succeeded")
            .count();
        Ok(u64::try_from(streak).unwrap_or(u64::MAX))
    }

    async fn refund_reconciliation(
        &self,
        command: RefundReconciliationCommand,
    ) -> Result<BalanceChange, ApplicationError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let row = sqlx::query(
            r#"
            SELECT rc.id, rc.attempt_id, rc.status, rc.refund_business_key,
                   j.account_id, j.state, h.amount_microusd AS held_microusd,
                   h.status AS hold_status
            FROM operations.reconciliation_cases rc
            JOIN generation.jobs j ON j.id = rc.job_id
            JOIN ledger.holds h ON h.job_id = j.id
            WHERE rc.job_id = $1
            FOR UPDATE OF rc, j, h
            "#,
        )
        .bind(command.job_id.0)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| {
            ApplicationError::NotFound(format!("reconciliation case for job {}", command.job_id))
        })?;
        let status: String = row.try_get("status").map_err(database_error)?;
        let account_id = AccountId(row.try_get("account_id").map_err(database_error)?);
        if status == "resolved" {
            let existing_key: Option<String> =
                row.try_get("refund_business_key").map_err(database_error)?;
            if existing_key.as_deref() == Some(command.business_key.as_str()) {
                transaction.commit().await.map_err(database_error)?;
                // 重放：这次没有退款，但返回**当前**余额，让缓存刷成数据库的值。
                return self.account_balance(account_id).await;
            }
            return Err(ApplicationError::Conflict(
                "reconciliation case was already resolved differently".to_owned(),
            ));
        }
        let job_state: String = row.try_get("state").map_err(database_error)?;
        let hold_status: String = row.try_get("hold_status").map_err(database_error)?;
        if job_state != "reconciliation_required" || hold_status != "active" {
            return Err(ApplicationError::Conflict(
                "reconciliation job or hold is not open".to_owned(),
            ));
        }
        let held: i64 = row.try_get("held_microusd").map_err(database_error)?;
        let released = sqlx::query(
            r#"
            UPDATE ledger.accounts
            SET balance_microusd = balance_microusd + $2, updated_at = now()
            WHERE id = $1
            RETURNING balance_microusd, updated_at
            "#,
        )
        .bind(account_id.0)
        .bind(held)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| ApplicationError::NotFound(format!("account {account_id}")))?;
        sqlx::query(
            "UPDATE ledger.holds SET status = 'released', updated_at = now() WHERE job_id = $1 AND status = 'active'",
        )
        .bind(command.job_id.0)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        insert_ledger_entry(
            &mut transaction,
            account_id,
            Some(command.job_id),
            "release",
            held,
            &format!(
                "reconciliation:{}:{}:release",
                command.job_id, command.business_key
            ),
        )
        .await?;
        let attempt_id: Uuid = row.try_get("attempt_id").map_err(database_error)?;
        sqlx::query(
            "UPDATE generation.attempts SET state = 'failed', completed_at = now() WHERE id = $1",
        )
        .bind(attempt_id)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            r#"
            UPDATE generation.jobs
            SET state = 'failed', error_code = 'platform_unavailable', error_message = $2,
                failure_kind = 'platform_internal',
                version = version + 1, updated_at = now()
            WHERE id = $1 AND state = 'reconciliation_required'
            "#,
        )
        .bind(command.job_id.0)
        .bind(&command.note)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            r#"
            UPDATE operations.reconciliation_cases
            SET status = 'resolved', refund_note = $2,
                refund_business_key = $3, resolved_at = now()
            WHERE job_id = $1 AND status = 'open'
            "#,
        )
        .bind(command.job_id.0)
        .bind(&command.note)
        .bind(&command.business_key)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        insert_audit(
            &mut transaction,
            &command.actor,
            "reconciliation.refund",
            "job",
            &command.job_id.to_string(),
            &serde_json::json!({
                "released_microusd": held,
                "business_key": command.business_key,
            }),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        balance_change(&released, account_id)
    }

    async fn find_admin_by_email(
        &self,
        email: &str,
    ) -> Result<Option<(Uuid, String)>, ApplicationError> {
        // 邮箱判据大小写不敏感：库里存小写，表达式索引也建在 `lower(email)` 上，所以这里直接比。
        sqlx::query_as::<_, (Uuid, String)>(
            r#"SELECT id, password_hash FROM identity.admin_users WHERE lower(email) = lower($1)"#,
        )
        .bind(email)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)
    }

    async fn ensure_admin_account(
        &self,
        email: &str,
        password_hash: &str,
    ) -> Result<(Uuid, bool), ApplicationError> {
        // `DO NOTHING` 是这条的关键：账号已存在时**连口令都不动**。运维改过口令之后，
        // 每次重启再把环境变量里的值写回去等于把口令打回初始值。
        //
        // 插进去的那次影响 1 行（新建）；撞上已有账号时影响 0 行，此时再按邮箱把它读出来。
        let inserted: Option<Uuid> = sqlx::query_scalar(
            r#"
            INSERT INTO identity.admin_users (id, email, password_hash)
            VALUES ($1, $2, $3)
            ON CONFLICT (lower(email)) DO NOTHING
            RETURNING id
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(email)
        .bind(password_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)?;
        if let Some(id) = inserted {
            return Ok((id, true));
        }
        let existing: Uuid = sqlx::query_scalar(
            "SELECT id FROM identity.admin_users WHERE lower(email) = lower($1)",
        )
        .bind(email)
        .fetch_one(&self.pool)
        .await
        .map_err(database_error)?;
        Ok((existing, false))
    }

    async fn upsert_admin_password(
        &self,
        email: &str,
        password_hash: &str,
    ) -> Result<Uuid, ApplicationError> {
        // 按邮箱 upsert：**覆盖**口令，用于运维按邮箱改口令（引导不走这条，见 `ensure_admin_account`）。
        let id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO identity.admin_users (id, email, password_hash)
            VALUES ($1, $2, $3)
            ON CONFLICT (lower(email)) DO UPDATE
                SET password_hash = EXCLUDED.password_hash, updated_at = now()
            RETURNING id
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(email)
        .bind(password_hash)
        .fetch_one(&self.pool)
        .await
        .map_err(database_error)?;
        Ok(id)
    }

    async fn record_admin_login(&self, admin_id: Uuid) -> Result<(), ApplicationError> {
        // 登录成功留痕：谁在什么时候登进来了。写 `last_login_at` 与审计在同一个事务。
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        sqlx::query("UPDATE identity.admin_users SET last_login_at = now() WHERE id = $1")
            .bind(admin_id)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        insert_audit(
            &mut transaction,
            "admin-self",
            "admin.login",
            "admin_user",
            &admin_id.to_string(),
            &serde_json::json!({}),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(())
    }

    async fn create_admin_session(
        &self,
        admin_id: Uuid,
        token_hash: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<Uuid, ApplicationError> {
        let id = Uuid::new_v4();
        sqlx::query(
            r#"
            INSERT INTO identity.admin_sessions (id, admin_id, token_hash, expires_at)
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(id)
        .bind(admin_id)
        .bind(token_hash)
        .bind(expires_at)
        .execute(&self.pool)
        .await
        .map_err(database_error)?;
        Ok(id)
    }

    async fn find_admin_session(
        &self,
        token_hash: &str,
    ) -> Result<Option<(Uuid, String, DateTime<Utc>)>, ApplicationError> {
        sqlx::query_as::<_, (Uuid, String, DateTime<Utc>)>(
            r#"
            SELECT s.admin_id, u.email, s.expires_at
            FROM identity.admin_sessions s
            JOIN identity.admin_users u ON u.id = s.admin_id
            WHERE s.token_hash = $1
            "#,
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)
    }

    async fn delete_admin_session(&self, token_hash: &str) -> Result<(), ApplicationError> {
        sqlx::query("DELETE FROM identity.admin_sessions WHERE token_hash = $1")
            .bind(token_hash)
            .execute(&self.pool)
            .await
            .map_err(database_error)?;
        Ok(())
    }

    async fn create_customer(
        &self,
        email: &str,
        password_hash: &str,
    ) -> Result<(Uuid, Uuid), ApplicationError> {
        let customer_id = Uuid::new_v4();
        let account_id = Uuid::new_v4();
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        // 撞邮箱是调用方能自己改的事：回 Conflict，让对客那一层说"这个邮箱已经注册过了"。
        let existing: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM identity.customers WHERE lower(email) = lower($1)")
                .bind(email)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(database_error)?;
        if existing.is_some() {
            return Err(ApplicationError::Conflict(format!(
                "email {email} is already registered"
            )));
        }
        // 账户与身份同一个事务：注册出来的账户必须能立刻用，不能出现"有账户没身份"的半截状态。
        sqlx::query("INSERT INTO ledger.accounts (id, balance_microusd) VALUES ($1, 0)")
            .bind(account_id)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        sqlx::query(
            r#"
            INSERT INTO identity.customers (id, email, password_hash, account_id)
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(customer_id)
        .bind(email)
        .bind(password_hash)
        .bind(account_id)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        insert_audit(
            &mut transaction,
            "self-service",
            "customer.register",
            "account",
            &account_id.to_string(),
            &serde_json::json!({"email": email}),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok((customer_id, account_id))
    }

    async fn find_customer_by_email(
        &self,
        email: &str,
    ) -> Result<Option<(Uuid, Uuid, String)>, ApplicationError> {
        sqlx::query_as::<_, (Uuid, Uuid, String)>(
            r#"
            SELECT id, account_id, password_hash FROM identity.customers
            WHERE lower(email) = lower($1)
            "#,
        )
        .bind(email)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)
    }

    async fn find_customer_account(
        &self,
        customer_id: Uuid,
    ) -> Result<Option<Uuid>, ApplicationError> {
        sqlx::query_scalar::<_, Uuid>("SELECT account_id FROM identity.customers WHERE id = $1")
            .bind(customer_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(database_error)
    }

    async fn record_customer_login(&self, customer_id: Uuid) -> Result<(), ApplicationError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        sqlx::query("UPDATE identity.customers SET last_login_at = now() WHERE id = $1")
            .bind(customer_id)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        insert_audit(
            &mut transaction,
            "customer-self",
            "customer.login",
            "customer",
            &customer_id.to_string(),
            &serde_json::json!({}),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(())
    }

    async fn create_customer_session(
        &self,
        customer_id: Uuid,
        token_hash: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<Uuid, ApplicationError> {
        let id = Uuid::new_v4();
        sqlx::query(
            r#"
            INSERT INTO identity.customer_sessions (id, customer_id, token_hash, expires_at)
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(id)
        .bind(customer_id)
        .bind(token_hash)
        .bind(expires_at)
        .execute(&self.pool)
        .await
        .map_err(database_error)?;
        Ok(id)
    }

    async fn find_customer_session(
        &self,
        token_hash: &str,
    ) -> Result<Option<(Uuid, Uuid, DateTime<Utc>)>, ApplicationError> {
        sqlx::query_as::<_, (Uuid, Uuid, DateTime<Utc>)>(
            r#"
            SELECT c.id, c.account_id, s.expires_at
            FROM identity.customer_sessions s
            JOIN identity.customers c ON c.id = s.customer_id
            WHERE s.token_hash = $1
            "#,
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)
    }

    async fn delete_customer_session(&self, token_hash: &str) -> Result<(), ApplicationError> {
        sqlx::query("DELETE FROM identity.customer_sessions WHERE token_hash = $1")
            .bind(token_hash)
            .execute(&self.pool)
            .await
            .map_err(database_error)?;
        Ok(())
    }

    async fn find_admin_password(
        &self,
        admin_id: Uuid,
    ) -> Result<Option<String>, ApplicationError> {
        sqlx::query_scalar::<_, String>(
            "SELECT password_hash FROM identity.admin_users WHERE id = $1",
        )
        .bind(admin_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)
    }

    async fn record_audit(
        &self,
        actor: &str,
        action: &str,
        subject_type: &str,
        subject_id: &str,
    ) -> Result<(), ApplicationError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        insert_audit(
            &mut transaction,
            actor,
            action,
            subject_type,
            subject_id,
            &serde_json::json!({}),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(())
    }

    async fn set_admin_password(
        &self,
        admin_id: Uuid,
        password_hash: &str,
        actor: &str,
    ) -> Result<bool, ApplicationError> {
        // 写哈希、吊销该管理员全部会话、写审计在**同一个事务**：只成一件会留下"新口令生效、
        // 旧会话还能用"这类半截状态，而 Spec 要求改完旧凭据立刻不能再用。
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let written = sqlx::query(
            "UPDATE identity.admin_users SET password_hash = $2, updated_at = now() WHERE id = $1",
        )
        .bind(admin_id)
        .bind(password_hash)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected()
            == 1;
        if !written {
            transaction.rollback().await.map_err(database_error)?;
            return Ok(false);
        }
        sqlx::query("DELETE FROM identity.admin_sessions WHERE admin_id = $1")
            .bind(admin_id)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        insert_audit(
            &mut transaction,
            actor,
            "admin.password_change",
            "admin_user",
            &admin_id.to_string(),
            &serde_json::json!({}),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(true)
    }

    async fn create_password_reset(
        &self,
        subject_kind: &str,
        subject_id: Uuid,
        token_hash: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<Uuid, ApplicationError> {
        let id = Uuid::new_v4();
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        // 同一身份只留一条未兑换令牌（迁移 0020 的部分唯一索引）：签新的就作废旧的那些，
        // 免得旧令牌在运营看不见的地方继续可用。
        sqlx::query(
            r#"
            UPDATE identity.password_resets
            SET redeemed_at = now()
            WHERE subject_kind = $1 AND subject_id = $2 AND redeemed_at IS NULL
            "#,
        )
        .bind(subject_kind)
        .bind(subject_id)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            r#"
            INSERT INTO identity.password_resets (id, subject_kind, subject_id, token_hash, expires_at)
            VALUES ($1, $2, $3, $4, $5)
            "#,
        )
        .bind(id)
        .bind(subject_kind)
        .bind(subject_id)
        .bind(token_hash)
        .bind(expires_at)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(id)
    }

    async fn find_password_reset(
        &self,
        token_hash: &str,
    ) -> Result<Option<(String, Uuid, DateTime<Utc>, Option<DateTime<Utc>>)>, ApplicationError>
    {
        sqlx::query_as::<_, (String, Uuid, DateTime<Utc>, Option<DateTime<Utc>>)>(
            r#"
            SELECT subject_kind, subject_id, expires_at, redeemed_at
            FROM identity.password_resets
            WHERE token_hash = $1
            "#,
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)
    }

    async fn redeem_password_reset(&self, token_hash: &str) -> Result<bool, ApplicationError> {
        // `redeemed_at IS NULL` 就在这一条语句里：并发的两次兑换只有一次能改到行，
        // 另一次影响 0 行——"一次性"由数据库判定，不靠用例层的先读后写。
        let updated = sqlx::query(
            r#"
            UPDATE identity.password_resets
            SET redeemed_at = now()
            WHERE token_hash = $1 AND redeemed_at IS NULL
            "#,
        )
        .bind(token_hash)
        .execute(&self.pool)
        .await
        .map_err(database_error)?;
        Ok(updated.rows_affected() == 1)
    }

    async fn find_customer_password(
        &self,
        customer_id: Uuid,
    ) -> Result<Option<String>, ApplicationError> {
        sqlx::query_scalar::<_, String>(
            "SELECT password_hash FROM identity.customers WHERE id = $1",
        )
        .bind(customer_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)
    }

    async fn set_customer_password(
        &self,
        customer_id: Uuid,
        password_hash: &str,
        actor: &str,
    ) -> Result<bool, ApplicationError> {
        // 与管理员那条同一个理由：写哈希、吊销全部会话、写审计必须一起成功。
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let written = sqlx::query("UPDATE identity.customers SET password_hash = $2 WHERE id = $1")
            .bind(customer_id)
            .bind(password_hash)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?
            .rows_affected()
            == 1;
        if !written {
            transaction.rollback().await.map_err(database_error)?;
            return Ok(false);
        }
        sqlx::query("DELETE FROM identity.customer_sessions WHERE customer_id = $1")
            .bind(customer_id)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        insert_audit(
            &mut transaction,
            actor,
            "customer.password_change",
            "customer",
            &customer_id.to_string(),
            &serde_json::json!({}),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(true)
    }

    /// 按**账户**找一个客户的 id（运营按账户签重置令牌时用）。
    async fn find_customer_by_account(
        &self,
        account_id: Uuid,
    ) -> Result<Option<Uuid>, ApplicationError> {
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM identity.customers WHERE account_id = $1")
            .bind(account_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(database_error)
    }

    async fn open_customer_account(
        &self,
        email: &str,
        password_hash: &str,
        account_id: Option<Uuid>,
    ) -> Result<(Uuid, Uuid), ApplicationError> {
        let customer_id = Uuid::new_v4();
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        // 邮箱与账户各自唯一（迁移 0020 的两条唯一索引）：先查一次好给出说得清的冲突错误，
        // 真正的兜底仍由那两条索引承担。
        let taken: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM identity.customers WHERE lower(email) = lower($1)")
                .bind(email)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(database_error)?;
        if taken.is_some() {
            return Err(ApplicationError::Conflict(format!(
                "email {email} already has a login identity"
            )));
        }
        let account_id = match account_id {
            Some(existing) => {
                let bound: Option<Uuid> =
                    sqlx::query_scalar("SELECT id FROM identity.customers WHERE account_id = $1")
                        .bind(existing)
                        .fetch_optional(&mut *transaction)
                        .await
                        .map_err(database_error)?;
                if bound.is_some() {
                    return Err(ApplicationError::Conflict(format!(
                        "account {existing} already has a login identity"
                    )));
                }
                // 账户行必须真的存在：配身份不创建账户，指一个不存在的账户是调用方搞错了。
                let exists: Option<Uuid> =
                    sqlx::query_scalar("SELECT id FROM ledger.accounts WHERE id = $1")
                        .bind(existing)
                        .fetch_optional(&mut *transaction)
                        .await
                        .map_err(database_error)?;
                if exists.is_none() {
                    return Err(ApplicationError::NotFound(format!("account {existing}")));
                }
                existing
            }
            None => {
                let fresh = Uuid::new_v4();
                sqlx::query("INSERT INTO ledger.accounts (id, balance_microusd) VALUES ($1, 0)")
                    .bind(fresh)
                    .execute(&mut *transaction)
                    .await
                    .map_err(database_error)?;
                fresh
            }
        };
        sqlx::query(
            r#"
            INSERT INTO identity.customers (id, email, password_hash, account_id)
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(customer_id)
        .bind(email)
        .bind(password_hash)
        .bind(account_id)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        insert_audit(
            &mut transaction,
            "admin-api",
            "customer.open",
            "account",
            &account_id.to_string(),
            &serde_json::json!({"email": email}),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok((customer_id, account_id))
    }

    async fn find_customer_view(
        &self,
        email: &str,
    ) -> Result<Option<CustomerView>, ApplicationError> {
        let row = sqlx::query_as::<_, (Uuid, String, Uuid, DateTime<Utc>, Option<DateTime<Utc>>)>(
            r#"
            SELECT id, email, account_id, created_at, last_login_at
            FROM identity.customers
            WHERE lower(email) = lower($1)
            "#,
        )
        .bind(email)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)?;
        Ok(row.map(
            |(customer_id, email, account_id, created_at, last_login_at)| CustomerView {
                customer_id,
                email,
                account_id: AccountId(account_id),
                created_at,
                last_login_at,
            },
        ))
    }

    async fn list_customers(&self, limit: u32) -> Result<Vec<CustomerView>, ApplicationError> {
        let rows = sqlx::query_as::<_, (Uuid, String, Uuid, DateTime<Utc>, Option<DateTime<Utc>>)>(
            r#"
            SELECT id, email, account_id, created_at, last_login_at
            FROM identity.customers
            ORDER BY created_at DESC
            LIMIT $1
            "#,
        )
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        Ok(rows
            .into_iter()
            .map(
                |(customer_id, email, account_id, created_at, last_login_at)| CustomerView {
                    customer_id,
                    email,
                    account_id: AccountId(account_id),
                    created_at,
                    last_login_at,
                },
            )
            .collect())
    }
}

/// 一次发布**冻结进条目**的那份技术定义：`runtime_entries` 的八列，加上它们指向的供给、渠道与
/// Price Plan。
///
/// 两条来源（请求内联的值、被引用 Offering 行的当前值）收在同一个类型里，写入路径因此只有一个
/// 形状；另外它保证发布响应里的候选与修订 `snapshot` 的候选用的是同一份值，不出现"条目记的是
/// 行上的值、快照记的是请求里的值"这种同一份事实两个答案。
struct FrozenDefinition {
    offering_id: OfferingId,
    channel_id: ChannelId,
    adapter_key: String,
    provider_model_id: String,
    carrier_schema: Value,
    parameter_mapping: Value,
    restrictions: Value,
    provider_kind: String,
    base_url: String,
    credential_env: String,
    price_plan_id: Option<PricePlanId>,
}

/// **内联式**发布（老形状）的供给落库：请求自带技术定义，供给与渠道按身份 upsert，返回冻结的那份。
///
/// 渠道按 `provider_kind` + `base_url` + `credential_env` 复用：撞上既有行就回读它——渠道除了身份与
/// `enabled` 没有可变量，而 `enabled` 是**运营设的停用状态**，发布不是它的写入方（写回 `true` 会把
/// 手工停用无声顶掉）。因此这里不做 `DO UPDATE`：没有可更新的东西。
///
/// 供给按**它所属的 vendor model + channel** 复用。它的可变量（驱动、渠道侧模型名、限制、承载面、
/// 映射、计价形态与单价）随这次发布更新，`enabled` **不在更新之列**：那是运营设的停用状态，重发一次
/// 不该把它顶回启用。
async fn inline_definition(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    vendor_model_id: VendorModelId,
    offering: &NormalizedOffering,
    actor: &str,
) -> Result<FrozenDefinition, ApplicationError> {
    let channel_id = match sqlx::query_scalar::<_, Uuid>(
        r#"
        INSERT INTO supply.channels
            (id, provider_kind, base_url, credential_env, enabled)
        VALUES ($1, $2, $3, $4, true)
        ON CONFLICT (provider_kind, base_url, credential_env) DO NOTHING
        RETURNING id
        "#,
    )
    .bind(ChannelId::new().0)
    .bind(&offering.provider_kind)
    .bind(&offering.base_url)
    .bind(&offering.credential_env)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    {
        Some(id) => ChannelId(id),
        None => ChannelId(
            sqlx::query_scalar(
                r#"
                SELECT id FROM supply.channels
                WHERE provider_kind = $1 AND base_url = $2 AND credential_env = $3
                "#,
            )
            .bind(&offering.provider_kind)
            .bind(&offering.base_url)
            .bind(&offering.credential_env)
            .fetch_one(&mut **transaction)
            .await
            .map_err(database_error)?,
        ),
    };
    let offering_id = OfferingId(
        sqlx::query_scalar::<_, Uuid>(
            r#"
            INSERT INTO supply.offerings
                (id, vendor_model_id, channel_id, adapter_key, provider_model_id,
                 restrictions, carrier_schema, parameter_mapping, enabled,
                 formula, cost_unit_price_microusd)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, true, $9, $10)
            ON CONFLICT (vendor_model_id, channel_id) DO UPDATE SET
                adapter_key = EXCLUDED.adapter_key,
                provider_model_id = EXCLUDED.provider_model_id,
                restrictions = EXCLUDED.restrictions,
                carrier_schema = EXCLUDED.carrier_schema,
                parameter_mapping = EXCLUDED.parameter_mapping,
                formula = EXCLUDED.formula,
                cost_unit_price_microusd = EXCLUDED.cost_unit_price_microusd
            RETURNING id
            "#,
        )
        .bind(OfferingId::new().0)
        .bind(vendor_model_id.0)
        .bind(channel_id.0)
        .bind(&offering.adapter_key)
        .bind(&offering.provider_model_id)
        .bind(&offering.restrictions)
        .bind(&offering.carrier_schema)
        .bind(&offering.parameter_mapping)
        .bind(offering.formula.as_str())
        .bind(offering.cost_unit_price_microusd.map(to_i64).transpose()?)
        .fetch_one(&mut **transaction)
        .await
        .map_err(database_error)?,
    );
    // 渠道费率是**追加**形态：改费率写新的一行，旧行留着给已在那个时刻发布过的修订引用。
    let price_plan_id = match &offering.rates {
        Some(rates) => {
            let price_plan_id = PricePlanId::new();
            sqlx::query(
                r#"
                INSERT INTO pricing.price_plans (
                    id, offering_id, currency,
                    text_input_microusd_per_million, image_input_microusd_per_million,
                    text_output_microusd_per_million, image_output_microusd_per_million,
                    source_url, approved_by
                ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
                "#,
            )
            .bind(price_plan_id.0)
            .bind(offering_id.0)
            .bind(&rates.currency)
            .bind(to_i64(rates.text_input_microusd_per_million)?)
            .bind(to_i64(rates.image_input_microusd_per_million)?)
            .bind(to_i64(rates.text_output_microusd_per_million)?)
            .bind(to_i64(rates.image_output_microusd_per_million)?)
            .bind(offering.price_source_url.as_deref().unwrap_or_default())
            .bind(actor)
            .execute(&mut **transaction)
            .await
            .map_err(database_error)?;
            Some(price_plan_id)
        }
        None => None,
    };
    Ok(FrozenDefinition {
        offering_id,
        channel_id,
        adapter_key: offering.adapter_key.clone(),
        provider_model_id: offering.provider_model_id.clone(),
        carrier_schema: offering.carrier_schema.clone(),
        parameter_mapping: offering.parameter_mapping.clone(),
        restrictions: offering.restrictions.clone(),
        provider_kind: offering.provider_kind.clone(),
        base_url: offering.base_url.clone(),
        credential_env: offering.credential_env.clone(),
        price_plan_id,
    })
}

/// **引用式**发布的技术定义来源：被引用 Offering 行与它所属渠道行的**当前值**。
///
/// 为什么不继续走 upsert：`supply.offerings` 是**工程师配好的资产**，运营的发布只引用它
/// （`docs/design/0012-platform-model-publishing.md` §3）。继续 upsert 就等于"运营每发一次货就把
/// 工程师的技术定义按请求重写一遍"——那正是这次改动要收掉的那件事，而请求里根本没有这些字段。
///
/// 定位用的是供给的**身份键** `(vendor_model_id, channel_id)`（`offerings_identity` 索引，`0014`）：
/// 草稿带着从被引用行取回的渠道三要素，因此能唯一定位到那条既有行，不必新建、也不改动它。
/// **渠道行不新建**（引用式发布不引入新的调用入口）。
///
/// 查不到行说明"运营选中的那条资产在这次发布落库之前变了或没了"（例如工程师把它挪到了另一个
/// 渠道）：这是发布期错误，点名是哪条候选，不静默少一条候选、也不落一条空壳供给。
///
/// 费率取该 Offering 当前那行并**复用它**，不为这次发布插新行：费率是渠道事实（导入写新行），
/// 新插一行会把运营记成 `approved_by`，还让同一份费率多出一个 id 供不同修订指向。
async fn referenced_definition(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    offering_id: OfferingId,
) -> Result<FrozenDefinition, ApplicationError> {
    // **按选中的那一行直查**，不按身份四元组反查：同一个厂商模型下、同一条渠道上可能有多行供给，
    // 反查会挑错行，或挑不到——于是把一次正当的发布报成"选中的供给不存在"。运营选的是**哪一条**，
    // 这件事只有 `offering_id` 知道。
    let row = sqlx::query(
        r#"
        SELECT o.id AS offering_id, o.channel_id, o.adapter_key, o.provider_model_id,
               o.carrier_schema, o.parameter_mapping, o.restrictions,
               c.provider_kind, c.base_url, c.credential_env
        FROM supply.offerings o
        JOIN supply.channels c ON c.id = o.channel_id
        WHERE o.id = $1
        "#,
    )
    .bind(offering_id.0)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .ok_or_else(|| {
        ApplicationError::Validation(format!(
            "the referenced offering {} no longer exists: pick it again from the offering list",
            offering_id.0
        ))
    })?;
    let offering_id: Uuid = row.try_get("offering_id").map_err(database_error)?;
    let price_plan_id = sqlx::query_scalar::<_, Uuid>(
        r#"
        SELECT id FROM pricing.price_plans
        WHERE offering_id = $1
        ORDER BY created_at DESC, id DESC
        LIMIT 1
        "#,
    )
    .bind(offering_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .map(PricePlanId);
    Ok(FrozenDefinition {
        offering_id: OfferingId(offering_id),
        channel_id: ChannelId(row.try_get("channel_id").map_err(database_error)?),
        adapter_key: row.try_get("adapter_key").map_err(database_error)?,
        provider_model_id: row.try_get("provider_model_id").map_err(database_error)?,
        carrier_schema: row.try_get("carrier_schema").map_err(database_error)?,
        parameter_mapping: row.try_get("parameter_mapping").map_err(database_error)?,
        restrictions: row.try_get("restrictions").map_err(database_error)?,
        provider_kind: row.try_get("provider_kind").map_err(database_error)?,
        base_url: row.try_get("base_url").map_err(database_error)?,
        credential_env: row.try_get("credential_env").map_err(database_error)?,
        price_plan_id,
    })
}

/// 读一行的余额与写入时刻，配上调用方手上的账户。
///
/// `balance_microusd` 是 `bigint` 且**可以为负**（透支发生在结算）：这里不做非负校验，
/// 负数必须原样读出来，否则"缓存里那个数"与账本就不一致了。
fn balance_change(
    row: &sqlx::postgres::PgRow,
    account_id: AccountId,
) -> Result<BalanceChange, ApplicationError> {
    Ok(BalanceChange {
        account_id,
        balance_microusd: row.try_get("balance_microusd").map_err(database_error)?,
        updated_at: row.try_get("updated_at").map_err(database_error)?,
    })
}

/// 读一行的账户、余额与写入时刻（对账取数用：查询里带 `id`）。
fn balance_change_with_account(
    row: &sqlx::postgres::PgRow,
) -> Result<BalanceChange, ApplicationError> {
    balance_change(row, AccountId(row.try_get("id").map_err(database_error)?))
}

/// 从一行分录还原一条流水。
///
/// 类别是受控取值（库里有 `CHECK`）：解析不到说明存储被绕过，按错误处理而不是猜一个方向——
/// 猜错的表现是把一笔占位读成扣款，而读流水的人正是拿它核对的。
fn ledger_entry_from_row(row: &sqlx::postgres::PgRow) -> Result<LedgerEntry, ApplicationError> {
    let stored_kind: String = row.try_get("kind").map_err(database_error)?;
    let kind = LedgerEntryKind::parse(&stored_kind).ok_or_else(|| {
        ApplicationError::Persistence(format!(
            "unknown ledger entry kind in storage: {stored_kind}"
        ))
    })?;
    Ok(LedgerEntry {
        account_id: AccountId(row.try_get("account_id").map_err(database_error)?),
        kind,
        amount_microusd: row.try_get("amount_microusd").map_err(database_error)?,
        job_id: row
            .try_get::<Option<Uuid>, _>("job_id")
            .map_err(database_error)?
            .map(JobId),
        created_at: row.try_get("created_at").map_err(database_error)?,
    })
}

/// 从一行策略还原。
///
/// 策略类型是受控取值：落库值解析不到说明存储被绕过（或写进了本层还不支持的策略），按错误处理
/// 而不是悄悄退回默认——退回默认会把"配置没生效"伪装成"配置生效了"，而选路正是靠它决定走哪家。
fn route_policy_from_row(row: &sqlx::postgres::PgRow) -> Result<RoutePolicy, ApplicationError> {
    let strategy: String = row.try_get("strategy").map_err(database_error)?;
    let strategy = RouteStrategy::parse(&strategy).ok_or_else(|| {
        ApplicationError::InvalidParameter(format!("unknown route strategy {strategy}"))
    })?;
    Ok(RoutePolicy {
        gateway_model: row.try_get("gateway_model").map_err(database_error)?,
        strategy,
        discount_rates: serde_json::from_value(
            row.try_get("discount_rates").map_err(database_error)?,
        )
        .map_err(json_error)?,
        tag_channel_map: serde_json::from_value(
            row.try_get("tag_channel_map").map_err(database_error)?,
        )
        .map_err(json_error)?,
        version: row.try_get("version").map_err(database_error)?,
    })
}

/// 序列化/反序列化失败在语义上都是"这次的值不成形状"，按参数错误报出去。
///
/// jsonb 列的形状不对（不是对象、或值的类型不符）说明存储被绕过：按错误处理而不是当成空表——
/// 空表意味着"这条策略没有输入"，而"读不出来"是另一回事，把它当空表会让 `least_cost` 悄悄退回
/// 默认顺序，运营却以为折扣率生效了。
fn json_error(error: serde_json::Error) -> ApplicationError {
    ApplicationError::InvalidParameter(error.to_string())
}

/// 某条供给 / 渠道的启停会影响哪些网关模型的候选集：它们的**生效**条目里含这条供给的那些名字。
///
/// 只取 active 条目：停用的历史条目不在任何受理路径上，失效它们的缓存没有意义。`predicate`
/// 是调用点写死的片段（`re.offering_id = $1` 或 `o.channel_id = $1`），不是调用方给的输入。
async fn affected_gateway_models(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    predicate: &str,
    id: Uuid,
) -> Result<Vec<String>, ApplicationError> {
    let rows = sqlx::query(AssertSqlSafe(format!(
        r#"
        SELECT DISTINCT re.gateway_model
        FROM publication.runtime_entries re
        JOIN supply.offerings o ON o.id = re.offering_id
        WHERE re.active AND {predicate}
        ORDER BY re.gateway_model
        "#
    )))
    .bind(id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(database_error)?;
    rows.iter()
        .map(|row| row.try_get("gateway_model").map_err(database_error))
        .collect()
}

async fn insert_audit(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    actor: &str,
    action: &str,
    subject_type: &str,
    subject_id: &str,
    payload: &Value,
) -> Result<(), ApplicationError> {
    // `admin_id` 取自**当前请求的会话身份**（见 `seeai_application::with_admin_id`）：共享令牌与
    // 机器自我操作没有具体的人，那一列留空——`actor` 仍然说明"经哪条路径做的"。
    let admin_id = seeai_application::current_admin_id();
    sqlx::query(
        r#"
        INSERT INTO operations.audit_events
            (id, actor, action, subject_type, subject_id, payload, admin_id)
        VALUES ($1,$2,$3,$4,$5,$6,$7)
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(actor)
    .bind(action)
    .bind(subject_type)
    .bind(subject_id)
    .bind(payload)
    .bind(admin_id)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}

async fn insert_ledger_entry(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: AccountId,
    job_id: Option<JobId>,
    kind: &str,
    amount_microusd: i64,
    business_key: &str,
) -> Result<(), ApplicationError> {
    sqlx::query(
        r#"
        INSERT INTO ledger.entries
            (id, account_id, job_id, kind, amount_microusd, business_key)
        VALUES ($1,$2,$3,$4,$5,$6)
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(account_id.0)
    .bind(job_id.map(|value| value.0))
    .bind(kind)
    .bind(amount_microusd)
    .bind(business_key)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}

/// 把平台自己承担的一笔上游成本记进账本：平台账户余额减、`cost` 分录记负。
///
/// 平台账户按 `kind` 找，不把 id 写进代码——账户是数据（`migrations/0019_ledger_platform_cost.sql`
/// 种下那一行）。找不到它说明库没迁到位：这里报错回滚整笔事务，绝不静默丢掉一笔真花掉的钱。
///
/// 业务键按**执行**唯一：同一笔执行的成本只记一次，重放或将来多一个写入方都会被库挡下。
async fn insert_platform_cost(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    job_id: JobId,
    attempt_id: AttemptId,
    cny_microusd: i64,
) -> Result<(), ApplicationError> {
    let platform: Option<Uuid> = sqlx::query_scalar(
        r#"
        UPDATE ledger.accounts
        SET balance_microusd = balance_microusd - $1, updated_at = now()
        WHERE kind = 'platform'
        RETURNING id
        "#,
    )
    .bind(cny_microusd)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?;
    let platform = platform.ok_or_else(|| {
        ApplicationError::Persistence(
            "no platform account is seeded; see migrations/0019_ledger_platform_cost.sql"
                .to_owned(),
        )
    })?;
    insert_ledger_entry(
        transaction,
        AccountId(platform),
        Some(job_id),
        LedgerEntryKind::Cost.as_str(),
        -cny_microusd,
        &format!("job:{job_id}:attempt:{attempt_id}:cost"),
    )
    .await
}

fn row_to_candidate(row: &sqlx::postgres::PgRow) -> Result<OfferingCandidate, ApplicationError> {
    let offering_id = OfferingId(row.try_get("offering_id").map_err(database_error)?);
    let channel_id = ChannelId(row.try_get("channel_id").map_err(database_error)?);
    let provider_kind: String = row.try_get("provider_kind").map_err(database_error)?;
    let pricing = row_candidate_pricing(row, offering_id.0)?;
    let formula: String = row.try_get("formula").map_err(database_error)?;
    let formula = PricingFormula::parse(&formula).ok_or_else(|| {
        ApplicationError::Persistence(format!(
            "the published pricing formula {formula} is not one this build knows"
        ))
    })?;
    // 四档费率与价目行一起有、一起没有（LEFT JOIN 的两侧）：Price Plan 是 token 计量量那一种
    // 计价形态的参数，另外几种形态的供给没有它。
    let currency: Option<String> = row.try_get("currency").map_err(database_error)?;
    let rates = match currency {
        Some(currency) => Some(PriceRates {
            currency,
            text_input_microusd_per_million: read_rate_amount(
                row,
                "text_input_microusd_per_million",
            )?,
            image_input_microusd_per_million: read_rate_amount(
                row,
                "image_input_microusd_per_million",
            )?,
            text_output_microusd_per_million: read_rate_amount(
                row,
                "text_output_microusd_per_million",
            )?,
            image_output_microusd_per_million: read_rate_amount(
                row,
                "image_output_microusd_per_million",
            )?,
        }),
        None => None,
    };
    let cost_currency = pricing.cost_currency.clone();
    Ok(OfferingCandidate {
        runtime_revision_id: RuntimeRevisionId(
            row.try_get("runtime_revision_id").map_err(database_error)?,
        ),
        vendor_model_id: VendorModelId(row.try_get("vendor_model_id").map_err(database_error)?),
        offering_id,
        channel_id,
        gateway_model: row.try_get("gateway_model").map_err(database_error)?,
        native_revision: row.try_get("native_revision").map_err(database_error)?,
        capability_schema: row.try_get("capability_schema").map_err(database_error)?,
        carrier_schema: row.try_get("carrier_schema").map_err(database_error)?,
        parameter_mapping: row.try_get("parameter_mapping").map_err(database_error)?,
        restrictions: row.try_get("restrictions").map_err(database_error)?,
        adapter_key: row.try_get("adapter_key").map_err(database_error)?,
        provider_model_id: row.try_get("provider_model_id").map_err(database_error)?,
        provider_kind: provider_kind.clone(),
        base_url: row.try_get("base_url").map_err(database_error)?,
        credential_env: row.try_get("credential_env").map_err(database_error)?,
        price_snapshot: PriceSnapshot {
            price_plan_id: row
                .try_get::<Option<Uuid>, _>("price_plan_id")
                .map_err(database_error)?
                .map(PricePlanId),
            rates,
            formula,
            cost_unit_price_microusd: row
                .try_get::<Option<i64>, _>("cost_unit_price_microusd")
                .map_err(database_error)?
                .map(to_u64)
                .transpose()?,
            captured_at: row.try_get("captured_at").map_err(database_error)?,
            // 命中候选就是这条候选：受理时选中哪条，就把它这份快照固化进 Job。
            hit_candidate: Some(HitCandidate {
                offering_id,
                channel_id,
                provider_kind,
            }),
            consumer_rates_cny: pricing.consumer_rates_cny,
            consumer_formula: pricing.consumer_formula,
            tier_prices: pricing.tier_prices,
            floor_amounts: pricing.floor_amounts,
            // 保底额与汇率**依赖这次请求**，由受理用例算定后填（发布侧算不出来）。
            hold_microusd: None,
            hold_source: None,
            cost_basis: pricing.cost_basis,
            reference_cost_microusd: pricing.reference_cost_microusd,
            cost_currency,
            markup_bps: row.try_get("markup_bps").map_err(database_error)?,
            fx_rate: None,
        },
        routing_priority: row.try_get("routing_priority").map_err(database_error)?,
        weight: row_weight(row)?,
    })
}

/// 读四档费率里的一个金额。价目行在场时四列都是 NOT NULL（库层约束），读到 NULL 说明存储被
/// 绕过——按错误处理，不把缺的那一档当 0（0 费率会把成本算成免费）。
fn read_rate_amount(row: &sqlx::postgres::PgRow, column: &str) -> Result<u64, ApplicationError> {
    let value: Option<i64> = row.try_get(column).map_err(database_error)?;
    match value {
        Some(amount) => to_u64(amount),
        None => Err(ApplicationError::Persistence(format!(
            "the published price plan has no {column}"
        ))),
    }
}

/// 读回一条候选的权重。
///
/// 库层有 `CHECK (weight > 0)`，所以读到的值正常都 ≥ 1；这里仍做一次防御性校验：库约束是
/// 别人也能绕过的（直接写 SQL、手工改数据），而"权重为 0 的候选"会让分摊区间少一段，
/// 表现为"某些请求分不到任何候选"——那种故障从结果上看不出来，只能在读回来时就拒。
fn row_weight(row: &sqlx::postgres::PgRow) -> Result<u32, ApplicationError> {
    let weight: i32 = row.try_get("weight").map_err(database_error)?;
    u32::try_from(weight).map_err(|_| {
        ApplicationError::Persistence(format!(
            "offering weight {weight} is not a positive integer"
        ))
    })
}

/// 一条候选在修订上的**定价**（按候选键的映射 + 修订级加价系数）。
///
/// 这些映射都缺席（`NULL`，或映射里没有这条候选）时全部为 `None`：这条候选不带定价，
/// 受理与结算走旧口径。这是"迁移前的旧修订"与"发布了定价但表为空"分得开的关键。
struct CandidatePricingRow {
    consumer_rates_cny: Option<ConsumerRatesCny>,
    consumer_formula: Option<PricingFormula>,
    tier_prices: Option<Value>,
    floor_amounts: Option<Value>,
    cost_basis: Option<CostBasis>,
    reference_cost_microusd: Option<u64>,
    cost_currency: Option<String>,
}

fn row_candidate_pricing(
    row: &sqlx::postgres::PgRow,
    offering_id: Uuid,
) -> Result<CandidatePricingRow, ApplicationError> {
    let entry = |column: &str| -> Result<Option<Value>, ApplicationError> {
        let column: Option<Value> = row.try_get(column).map_err(database_error)?;
        Ok(candidate_pricing_entry(column.as_ref(), offering_id).cloned())
    };
    let consumer_rates_cny = entry("consumer_rates_cny")?
        .map(serde_json::from_value::<ConsumerRatesCny>)
        .transpose()
        .map_err(|error| ApplicationError::Persistence(error.to_string()))?;
    let consumer_formula = entry("consumer_formula")?
        .map(|value| {
            value
                .as_str()
                .and_then(PricingFormula::parse)
                .ok_or_else(|| {
                    ApplicationError::Persistence(
                        "the published consumer form is not a known pricing formula".to_owned(),
                    )
                })
        })
        .transpose()?;
    let cost_basis = match entry("cost_basis")? {
        Some(value) => Some(value.as_str().and_then(CostBasis::parse).ok_or_else(|| {
            ApplicationError::Persistence(
                "the published cost basis is not computed or declared".to_owned(),
            )
        })?),
        None => None,
    };
    let reference_cost_microusd = entry("reference_cost_microusd")?
        .map(|value| {
            value.as_u64().ok_or_else(|| {
                ApplicationError::Persistence(
                    "the published reference cost is not an amount".to_owned(),
                )
            })
        })
        .transpose()?;
    let cost_currency = entry("cost_currency")?
        .map(|value| {
            value.as_str().map(str::to_owned).ok_or_else(|| {
                ApplicationError::Persistence(
                    "the published cost currency is not a currency name".to_owned(),
                )
            })
        })
        .transpose()?;
    Ok(CandidatePricingRow {
        consumer_rates_cny,
        consumer_formula,
        tier_prices: entry("tier_prices")?,
        floor_amounts: entry("floor_amounts")?,
        cost_basis,
        reference_cost_microusd,
        cost_currency,
    })
}

/// 从修订上**按候选键**的定价映射里取这条候选的那一份；没有这个键（或整列为空）时为 `None`。
fn candidate_pricing_entry(column: Option<&Value>, offering_id: Uuid) -> Option<&Value> {
    column?.as_object()?.get(&offering_id.to_string())
}

/// 按候选键的定价映射：一条候选都没带定价时写 `NULL`。
///
/// `NULL` 与空对象不是一回事：`NULL` 是"这份修订没有定价"（受理与结算走旧口径），空对象是
/// "定价表是空的"。混起来之后，"迁移前的旧修订"与"发布了定价但表为空"就分不开了。
fn optional_pricing_map(map: Map<String, Value>) -> Option<Value> {
    if map.is_empty() {
        None
    } else {
        Some(Value::Object(map))
    }
}

/// 管理员视图里的一条候选：只读投影的一行。
fn row_to_gateway_model_candidate(
    row: &sqlx::postgres::PgRow,
) -> Result<GatewayModelCandidateView, ApplicationError> {
    let offering_id = OfferingId(row.try_get("offering_id").map_err(database_error)?);
    let pricing = row_candidate_pricing(row, offering_id.0)?;
    Ok(GatewayModelCandidateView {
        offering_id,
        provider_kind: row.try_get("provider_kind").map_err(database_error)?,
        provider_model_id: row.try_get("provider_model_id").map_err(database_error)?,
        adapter_key: row.try_get("adapter_key").map_err(database_error)?,
        routing_priority: row.try_get("routing_priority").map_err(database_error)?,
        weight: row_weight(row)?,
        // 判据在 SQL 里判过了（`CANDIDATE_AVAILABLE_SQL`），这里只读结果，不重算。
        enabled: row.try_get("candidate_available").map_err(database_error)?,
        carrier_schema: row.try_get("carrier_schema").map_err(database_error)?,
        parameter_mapping: row.try_get("parameter_mapping").map_err(database_error)?,
        consumer_rates_cny: pricing.consumer_rates_cny,
        consumer_formula: pricing.consumer_formula,
        reference_cost_microusd: pricing.reference_cost_microusd,
        cost_currency: pricing.cost_currency,
        cost_basis: pricing.cost_basis,
        tier_prices: pricing.tier_prices,
        floor_amounts: pricing.floor_amounts,
    })
}

/// 管理员视图里的一个网关模型：只读投影的一行；候选按行序由调用方拼好后传进来。
fn row_to_gateway_model(
    row: &sqlx::postgres::PgRow,
    candidates: Vec<GatewayModelCandidateView>,
) -> Result<GatewayModelView, ApplicationError> {
    Ok(GatewayModelView {
        gateway_model: row.try_get("gateway_model").map_err(database_error)?,
        enabled: row.try_get("enabled").map_err(database_error)?,
        vendor_id: row.try_get("vendor_id").map_err(database_error)?,
        native_model_id: row.try_get("native_model_id").map_err(database_error)?,
        native_revision: row.try_get("native_revision").map_err(database_error)?,
        capability_schema: row.try_get("capability_schema").map_err(database_error)?,
        runtime_revision_id: RuntimeRevisionId(
            row.try_get("runtime_revision_id").map_err(database_error)?,
        ),
        published_at: row.try_get("published_at").map_err(database_error)?,
        // 加价系数是**修订级**的（每个网关模型一个），所以从行上直接读，不按候选取。
        markup_bps: row.try_get("markup_bps").map_err(database_error)?,
        candidates,
    })
}

fn row_to_generation_job(row: &sqlx::postgres::PgRow) -> Result<GenerationJob, ApplicationError> {
    let price_snapshot: Value = row.try_get("price_snapshot").map_err(database_error)?;
    // 合同从 vendor_model 行取（它落库后不再改，因此读到的永远是受理当时那一份）；
    // 承载面、映射、适配器、Provider Model 与**执行入口**（入口地址、凭证名）都从 **Job 自己那几列**
    // 取——它们是受理时随 Job 冻结的执行事实，不跟着发布物或渠道行走，所以重发把供给行改成另一套、
    // 或者有人直接改库换掉渠道行的入口，旧 Job 读到的仍是受理时那一套。
    // 这不是顺手的偏好：供给行按身份复用、重发就地改写它，读那一行等于让已受理的 Job 用上
    // 后来改的适配器与 Provider Model。
    let offering = PublishedOffering {
        runtime_revision_id: RuntimeRevisionId(
            row.try_get("runtime_revision_id").map_err(database_error)?,
        ),
        vendor_model_id: VendorModelId(row.try_get("vendor_model_id").map_err(database_error)?),
        offering_id: OfferingId(row.try_get("offering_id").map_err(database_error)?),
        channel_id: ChannelId(row.try_get("channel_id").map_err(database_error)?),
        gateway_model: row.try_get("gateway_model").map_err(database_error)?,
        native_revision: row.try_get("native_revision").map_err(database_error)?,
        capability_schema: row.try_get("capability_schema").map_err(database_error)?,
        carrier_schema: row.try_get("carrier_schema").map_err(database_error)?,
        parameter_mapping: row.try_get("parameter_mapping").map_err(database_error)?,
        restrictions: row.try_get("restrictions").map_err(database_error)?,
        adapter_key: row.try_get("adapter_key").map_err(database_error)?,
        provider_model_id: row.try_get("provider_model_id").map_err(database_error)?,
        provider_kind: row.try_get("provider_kind").map_err(database_error)?,
        base_url: row.try_get("base_url").map_err(database_error)?,
        credential_env: row.try_get("credential_env").map_err(database_error)?,
        price_snapshot: serde_json::from_value(price_snapshot)
            .map_err(|error| ApplicationError::Persistence(error.to_string()))?,
    };
    let state: String = row.try_get("state").map_err(database_error)?;
    Ok(GenerationJob {
        id: JobId(row.try_get("id").map_err(database_error)?),
        account_id: AccountId(row.try_get("account_id").map_err(database_error)?),
        state: parse_state(&state)?,
        branch: parse_branch(row.try_get("branch").map_err(database_error)?)?,
        gateway_model: row.try_get("gateway_model").map_err(database_error)?,
        native_parameters: row.try_get("native_parameters").map_err(database_error)?,
        offering,
        idempotency_key: row.try_get("idempotency_key").map_err(database_error)?,
        request_hash: row.try_get("request_hash").map_err(database_error)?,
        max_cost_microusd: to_u64(row.try_get("max_cost_microusd").map_err(database_error)?)?,
        created_at: row.try_get("created_at").map_err(database_error)?,
        updated_at: row.try_get("updated_at").map_err(database_error)?,
    })
}

fn parse_state(value: &str) -> Result<seeai_domain::JobState, ApplicationError> {
    match value {
        "accepted" => Ok(seeai_domain::JobState::Accepted),
        "leased" => Ok(seeai_domain::JobState::Leased),
        "submitting" => Ok(seeai_domain::JobState::Submitting),
        "succeeded" => Ok(seeai_domain::JobState::Succeeded),
        "failed" => Ok(seeai_domain::JobState::Failed),
        "reconciliation_required" => Ok(seeai_domain::JobState::ReconciliationRequired),
        "canceled" => Ok(seeai_domain::JobState::Canceled),
        _ => Err(ApplicationError::Persistence(format!(
            "unknown job state {value}"
        ))),
    }
}

fn branch_name(value: ImageBranch) -> &'static str {
    match value {
        ImageBranch::PromptOnly => "prompt_only",
        ImageBranch::ImageConditioned => "image_conditioned",
        ImageBranch::Masked => "masked",
    }
}

fn parse_branch(value: String) -> Result<ImageBranch, ApplicationError> {
    match value.as_str() {
        "prompt_only" => Ok(ImageBranch::PromptOnly),
        "image_conditioned" => Ok(ImageBranch::ImageConditioned),
        "masked" => Ok(ImageBranch::Masked),
        _ => Err(ApplicationError::Persistence(format!(
            "unknown image branch {value}"
        ))),
    }
}

fn to_i64(value: u64) -> Result<i64, ApplicationError> {
    i64::try_from(value).map_err(|_| {
        ApplicationError::Validation("numeric value exceeds database range".to_owned())
    })
}

fn to_u64(value: i64) -> Result<u64, ApplicationError> {
    u64::try_from(value)
        .map_err(|_| ApplicationError::Persistence("negative monetary value".to_owned()))
}

fn database_error(error: impl std::fmt::Display) -> ApplicationError {
    ApplicationError::Persistence(error.to_string())
}

/// 发布物的内容指纹：把**合同**与这条供给的**承载面**一起哈希。
///
/// 路由判定记的是"受理时考虑了哪些候选、谁被选中"，事后要能核对当时那份合同与承载面是否
/// 还是现在这两份——因此快照里留的是两者的合体指纹，而不是只有其中一份。
/// 字段顺序不影响结果：`serde_json` 的对象按键排序，同一份内容无论怎么写都得到同一个哈希。
fn contract_carrier_hash(contract: &Value, carrier: &Value) -> Result<String, ApplicationError> {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "contract": contract,
        "carrier": carrier,
    }))
    .map_err(|error| ApplicationError::Validation(error.to_string()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}
