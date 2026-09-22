use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use seeai_application::{
    ApplicationError, AttemptFailure, ClaimedJob, CompleteJob, GatewayModelCandidateView,
    GatewayModelView, HoldDisposition, HubRepository, JobView, LeaseRecovery, NewFxRate,
    ProviderCostGapView, ProviderFailureKind, ProviderFailureQuery, ProviderFailureView,
    PublicErrorCode, PublishRuntimeRequest, ReconciliationCaseView, RefundReconciliationCommand,
    RoutingDecision,
};
use seeai_domain::{
    AccountId, AttemptId, ChannelId, ConsumerRatesCny, CostBasis, CreateImageGeneration, FxRate,
    GenerationJob, HitCandidate, ImageBranch, JobId, OfferingCandidate, OfferingId, PricePlanId,
    PriceRates, PriceSnapshot, PublishedModel, PublishedOffering, PublishedRevision,
    RuntimeRevisionId, VendorModelId,
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use sqlx::{AssertSqlSafe, PgPool, Row, postgres::PgPoolOptions};
use uuid::Uuid;

/// **唯一的候选可用性判据**：这条候选现在真的能走吗——它自己启用，且它所在的渠道也启用。
///
/// 三处都判它：对客目录（列哪些模型）、受理（取哪些候选）、管理员视图（每个候选的 `enabled`
/// 字段）。只写一遍是因为分散之后，将来加一条闸门（比如渠道维护窗口）必然漏掉其中一处，
/// 而漏掉的那一处会让"目录里列着、提交时却取不到候选"重新出现——那种模型对调用方是 404，
/// 比不列更糟。
///
/// 列名 `o`/`c` 是这三条查询里供给与渠道的固定别名。管理员视图不把它放进 `WHERE`（它要连
/// **停用**的候选一起列出来，运营才看得出"为什么它调不动"），而是放进 `SELECT` 当一列读。
///
/// 用它拼查询要经过 `AssertSqlSafe`：`sqlx::query` 默认只收字面量，为的是逼动态 SQL 先被审
/// 一遍。这里拼进去的只有这个编译期常量（列名与一个布尔与），不含任何外部输入或用户数据，
/// 所以那个断言是"审过了"，不是把检查绕过去——别的动态 SQL 不要走这条路。
const CANDIDATE_AVAILABLE_SQL: &str = "o.enabled AND c.enabled";

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

    async fn load_generation_job(&self, job_id: JobId) -> Result<GenerationJob, ApplicationError> {
        let row = sqlx::query(
            r#"
            SELECT
                j.id, j.account_id, j.state, j.branch, j.gateway_model,
                j.native_parameters, j.idempotency_key,
                j.request_hash, j.max_cost_microusd, j.created_at, j.updated_at,
                vm.id AS vendor_model_id, vm.native_revision, vm.capability_schema,
                j.carrier_schema, j.parameter_mapping,
                o.id AS offering_id, o.adapter_key, o.provider_model_id, o.restrictions,
                c.id AS channel_id, c.provider_kind, c.base_url, c.credential_env,
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
            let effective: bool = sqlx::query_scalar(
                r#"
                SELECT EXISTS (
                    SELECT 1 FROM pricing.fx_rates
                    WHERE currency = $1 AND effective_at <= now()
                )
                "#,
            )
            .bind(&offering.rates.currency)
            .fetch_one(&mut *transaction)
            .await
            .map_err(database_error)?;
            if !effective {
                transaction.rollback().await.map_err(database_error)?;
                return Err(ApplicationError::Validation(format!(
                    "no effective fx rate for {}; record one before publishing a candidate \
                     whose cost is kept in that currency",
                    offering.rates.currency
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
        let mut cost_basis = Map::new();
        let mut tier_prices = Map::new();
        let mut floor_amounts = Map::new();
        for offering in &offerings {
            let channel_id = ChannelId::new();
            sqlx::query(
                r#"
                INSERT INTO supply.channels
                    (id, provider_kind, base_url, credential_env, enabled)
                VALUES ($1, $2, $3, $4, true)
                "#,
            )
            .bind(channel_id.0)
            .bind(&offering.provider_kind)
            .bind(&offering.base_url)
            .bind(&offering.credential_env)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
            let offering_id = OfferingId::new();
            sqlx::query(
                r#"
                INSERT INTO supply.offerings
                    (id, vendor_model_id, channel_id, adapter_key, provider_model_id,
                     restrictions, carrier_schema, parameter_mapping, enabled)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, true)
                "#,
            )
            .bind(offering_id.0)
            .bind(vendor_model_id.0)
            .bind(channel_id.0)
            .bind(&offering.adapter_key)
            .bind(&offering.provider_model_id)
            .bind(&offering.restrictions)
            .bind(&offering.carrier_schema)
            .bind(&offering.parameter_mapping)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
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
            .bind(&offering.rates.currency)
            .bind(to_i64(offering.rates.text_input_microusd_per_million)?)
            .bind(to_i64(offering.rates.image_input_microusd_per_million)?)
            .bind(to_i64(offering.rates.text_output_microusd_per_million)?)
            .bind(to_i64(offering.rates.image_output_microusd_per_million)?)
            .bind(&offering.price_source_url)
            .bind(&actor)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
            candidates.push(OfferingCandidate {
                runtime_revision_id: revision_id,
                vendor_model_id,
                offering_id,
                channel_id,
                gateway_model: gateway_model.clone(),
                native_revision: native_revision.clone(),
                capability_schema: capability_schema.clone(),
                carrier_schema: offering.carrier_schema.clone(),
                parameter_mapping: offering.parameter_mapping.clone(),
                restrictions: offering.restrictions.clone(),
                adapter_key: offering.adapter_key.clone(),
                provider_model_id: offering.provider_model_id.clone(),
                provider_kind: offering.provider_kind.clone(),
                base_url: offering.base_url.clone(),
                credential_env: offering.credential_env.clone(),
                price_snapshot: PriceSnapshot {
                    price_plan_id,
                    rates: offering.rates.clone(),
                    captured_at: now,
                    // 命中的候选就是这条候选本身：快照是**按候选**带下来的，选中哪条就把哪条
                    // 的快照固化进 Job，所以"这一笔的售价按谁算的"在快照里读得出来。
                    hit_candidate: Some(HitCandidate {
                        offering_id,
                        channel_id,
                        provider_kind: offering.provider_kind.clone(),
                    }),
                    // 随修订发布的定价。保底额与汇率依赖这次请求（`(size, quality)` 与受理时刻），
                    // 发布侧算不出来，由受理用例算定后填。
                    consumer_rates_cny: offering
                        .pricing
                        .as_ref()
                        .map(|pricing| pricing.consumer_rates_cny.clone()),
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
                    cost_currency: offering
                        .pricing
                        .as_ref()
                        .map(|pricing| pricing.cost_currency.clone()),
                    markup_bps,
                    fx_rate: None,
                },
                routing_priority: offering.routing_priority,
                weight: offering.weight,
            });
            if let Some(pricing) = &offering.pricing {
                let key = offering_id.to_string();
                reference_cost_microusd
                    .insert(key.clone(), Value::from(pricing.reference_cost_microusd));
                cost_currency.insert(key.clone(), Value::String(pricing.cost_currency.clone()));
                consumer_rates_cny.insert(
                    key.clone(),
                    serde_json::to_value(&pricing.consumer_rates_cny)
                        .map_err(|error| ApplicationError::Persistence(error.to_string()))?,
                );
                cost_basis.insert(
                    key.clone(),
                    Value::String(pricing.cost_basis.as_str().to_owned()),
                );
                tier_prices.insert(key.clone(), pricing.tier_prices.clone());
                floor_amounts.insert(key, pricing.floor_amounts.clone());
            }
            snapshot_entries.push(serde_json::json!({
                "offering_id": offering_id,
                "routing_priority": offering.routing_priority,
                "weight": offering.weight,
                "provider_kind": offering.provider_kind,
                "adapter_key": offering.adapter_key,
                "provider_model_id": offering.provider_model_id,
                "base_url": offering.base_url,
                "credential_env": offering.credential_env,
                "restrictions": offering.restrictions,
                "contract_carrier_hash": contract_carrier_hash(&capability_schema, &offering.carrier_schema)?,
                "currency": offering.rates.currency,
                "text_input_microusd_per_million": offering.rates.text_input_microusd_per_million,
                "image_input_microusd_per_million": offering.rates.image_input_microusd_per_million,
                "text_output_microusd_per_million": offering.rates.text_output_microusd_per_million,
                "image_output_microusd_per_million": offering.rates.image_output_microusd_per_million,
                "price_source_url": offering.price_source_url,
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
                 cost_basis, tier_prices, floor_amounts)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
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
        .bind(optional_pricing_map(cost_basis))
        .bind(optional_pricing_map(tier_prices))
        .bind(optional_pricing_map(floor_amounts))
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        for candidate in &candidates {
            sqlx::query(
                r#"
                INSERT INTO publication.runtime_entries
                    (runtime_revision_id, vendor_model_id, offering_id, price_plan_id,
                     gateway_model, active, routing_priority, weight)
                VALUES ($1, $2, $3, $4, $5, true, $6, $7)
                "#,
            )
            .bind(revision_id.0)
            .bind(candidate.vendor_model_id.0)
            .bind(candidate.offering_id.0)
            .bind(candidate.price_snapshot.price_plan_id.0)
            .bind(&gateway_model)
            .bind(candidate.routing_priority)
            .bind(i32::try_from(candidate.weight).map_err(|_| {
                ApplicationError::Validation("offering weight is out of range".to_owned())
            })?)
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
        // vendor_models 取**合同**（模型级唯一一份），并从它自己的 offering 行取**承载面**——
        // 同一型号的候选共享一份合同，各自带自己的承载面。
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
                vm.capability_schema, o.carrier_schema, o.parameter_mapping,
                o.id AS offering_id, o.adapter_key, o.provider_model_id, o.restrictions,
                c.id AS channel_id, c.provider_kind, c.base_url, c.credential_env,
                p.id AS price_plan_id, p.currency,
                p.text_input_microusd_per_million,
                p.image_input_microusd_per_million,
                p.text_output_microusd_per_million,
                p.image_output_microusd_per_million,
                rr.created_at AS captured_at,
                rr.markup_bps,
                rr.reference_cost_microusd,
                rr.cost_currency,
                rr.consumer_rates_cny,
                rr.cost_basis,
                rr.tier_prices,
                rr.floor_amounts,
                re.routing_priority,
                re.weight
            FROM publication.runtime_entries re
            JOIN publication.runtime_revisions rr ON rr.id = re.runtime_revision_id
            JOIN publication.gateway_models gm ON gm.gateway_model = re.gateway_model AND gm.enabled
            JOIN catalog.vendor_models vm ON vm.id = re.vendor_model_id
            JOIN supply.offerings o ON o.id = re.offering_id
            JOIN supply.channels c ON c.id = o.channel_id
            JOIN pricing.price_plans p ON p.id = re.price_plan_id
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
                vm.vendor_id, vm.native_model_id, vm.native_revision,
                rr.id AS runtime_revision_id, rr.created_at AS published_at,
                o.id AS offering_id, o.adapter_key, o.provider_model_id,
                o.carrier_schema, o.parameter_mapping,
                c.provider_kind,
                ({CANDIDATE_AVAILABLE_SQL}) AS candidate_available,
                rr.markup_bps,
                rr.reference_cost_microusd,
                rr.cost_currency,
                rr.consumer_rates_cny,
                rr.cost_basis,
                rr.tier_prices,
                rr.floor_amounts,
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

    async fn provider_cost_gaps(
        &self,
        limit: u32,
    ) -> Result<Vec<ProviderCostGapView>, ApplicationError> {
        // 判据只有一条：**来源是 `unavailable`**（本该有金额却拿不到）。失败的执行四列全空、
        // 来源也是空，那不是"缺口"而是"没有成本事实"——两者处置不同，不能混在一个清单里。
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

    async fn create_account(
        &self,
        account_id: AccountId,
        initial_credit_microusd: u64,
        actor: &str,
    ) -> Result<(), ApplicationError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        sqlx::query("INSERT INTO ledger.accounts (id, balance_microusd) VALUES ($1, $2)")
            .bind(account_id.0)
            .bind(to_i64(initial_credit_microusd)?)
            .execute(&mut *transaction)
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
        transaction.commit().await.map_err(database_error)
    }

    async fn credit_account(
        &self,
        account_id: AccountId,
        amount_microusd: u64,
        business_key: &str,
        actor: &str,
    ) -> Result<(), ApplicationError> {
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
        if inserted.rows_affected() == 1 {
            let updated = sqlx::query(
                r#"
                UPDATE ledger.accounts
                SET balance_microusd = balance_microusd + $2, updated_at = now()
                WHERE id = $1
                "#,
            )
            .bind(account_id.0)
            .bind(to_i64(amount_microusd)?)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
            if updated.rows_affected() != 1 {
                return Err(ApplicationError::NotFound(format!("account {account_id}")));
            }
            insert_audit(
                &mut transaction,
                actor,
                "account.credit",
                "account",
                &account_id.to_string(),
                &serde_json::json!({"amount_microusd": amount_microusd, "business_key": business_key}),
            )
                .await?;
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
        }
        transaction.commit().await.map_err(database_error)
    }

    async fn create_api_key(
        &self,
        account_id: AccountId,
        label: &str,
        key_hash: &str,
        actor: &str,
    ) -> Result<(), ApplicationError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        sqlx::query(
            r#"
            INSERT INTO identity.api_keys (id, account_id, label, key_hash)
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(Uuid::new_v4())
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
        transaction.commit().await.map_err(database_error)
    }

    async fn account_for_api_key(&self, key_hash: &str) -> Result<AccountId, ApplicationError> {
        let account_id: Uuid = sqlx::query_scalar(
            r#"
            SELECT account_id FROM identity.api_keys
            WHERE key_hash = $1 AND revoked_at IS NULL
            "#,
        )
        .bind(key_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)?
        .ok_or_else(|| ApplicationError::NotFound("api key".to_owned()))?;
        Ok(AccountId(account_id))
    }

    async fn create_job(
        &self,
        command: CreateImageGeneration,
        branch: ImageBranch,
        offering: PublishedOffering,
        request_hash: String,
        routing: RoutingDecision,
    ) -> Result<GenerationJob, ApplicationError> {
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
            return self.load_generation_job(JobId(existing_id)).await;
        }
        let max_cost = to_i64(command.max_cost_microusd)?;
        let reserved = sqlx::query(
            r#"
            UPDATE ledger.accounts
            SET balance_microusd = balance_microusd - $2, updated_at = now()
            WHERE id = $1 AND balance_microusd >= $2
            "#,
        )
        .bind(command.account_id.0)
        .bind(max_cost)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        if reserved.rows_affected() != 1 {
            return Err(ApplicationError::InsufficientBalance);
        }
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
                carrier_schema, parameter_mapping,
                price_snapshot, max_cost_microusd
            ) VALUES ($1,$2,$3,$4,'accepted',$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)
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
        let now = Utc::now();
        Ok(GenerationJob {
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
        })
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
            SELECT j.id AS job_id, a.id AS attempt_id
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
                    (id, job_id, attempt_id, reason)
                VALUES ($1,$2,$3,'worker lease expired after provider submission began')
                ON CONFLICT (job_id) DO NOTHING
                "#,
            )
            .bind(Uuid::new_v4())
            .bind(job_id)
            .bind(attempt_id)
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
    ) -> Result<(), ApplicationError> {
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
        sqlx::query(
            r#"
            INSERT INTO generation.attempts (id, job_id, state, request_digest)
            VALUES ($1,$2,'submitting',$3)
            "#,
        )
        .bind(attempt_id.0)
        .bind(job_id.0)
        .bind(request_digest)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
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

    async fn complete_job(&self, completion: CompleteJob) -> Result<(), ApplicationError> {
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
        sqlx::query(
            "UPDATE ledger.accounts SET balance_microusd = balance_microusd + $2, updated_at = now() WHERE id = $1",
        )
        .bind(account_id.0)
        .bind(refund)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
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
        transaction.commit().await.map_err(database_error)
    }

    async fn fail_job(
        &self,
        job_id: JobId,
        worker_id: &str,
        attempt_id: Option<AttemptId>,
        failure: AttemptFailure,
    ) -> Result<(), ApplicationError> {
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
            // 没有成本事实（连用量都算不出）时四列留 NULL——那是"这次没有成本事实可落"，
            // 不是"成本是 0"。
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
                    (id, job_id, attempt_id, reason)
                VALUES ($1,$2,$3,$4)
                ON CONFLICT (job_id) DO NOTHING
                "#,
            )
            .bind(Uuid::new_v4())
            .bind(job_id.0)
            .bind(attempt_id.0)
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
            sqlx::query(
                "UPDATE ledger.accounts SET balance_microusd = balance_microusd + $2, updated_at = now() WHERE id = $1",
            )
            .bind(account_id.0)
            .bind(held)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
            insert_ledger_entry(
                &mut transaction,
                account_id,
                Some(job_id),
                "release",
                held,
                &format!("job:{job_id}:failure-release"),
            )
            .await?;
        }
        transaction.commit().await.map_err(database_error)
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

    async fn list_open_reconciliation_cases(
        &self,
    ) -> Result<Vec<ReconciliationCaseView>, ApplicationError> {
        let rows = sqlx::query(
            r#"
            SELECT rc.id, rc.job_id, rc.attempt_id, j.account_id, rc.reason, rc.created_at,
                   a.provider_trace_id
            FROM operations.reconciliation_cases rc
            JOIN generation.jobs j ON j.id = rc.job_id
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
                    job_id: JobId(row.try_get("job_id").map_err(database_error)?),
                    attempt_id: AttemptId(row.try_get("attempt_id").map_err(database_error)?),
                    account_id: AccountId(row.try_get("account_id").map_err(database_error)?),
                    reason: row.try_get("reason").map_err(database_error)?,
                    provider_trace_id: row.try_get("provider_trace_id").map_err(database_error)?,
                    created_at: row.try_get("created_at").map_err(database_error)?,
                })
            })
            .collect()
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
            LEFT JOIN generation.attempts a ON a.job_id = j.id
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

    async fn refund_reconciliation(
        &self,
        command: RefundReconciliationCommand,
    ) -> Result<(), ApplicationError> {
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
        if status == "resolved" {
            let existing_key: Option<String> =
                row.try_get("refund_business_key").map_err(database_error)?;
            if existing_key.as_deref() == Some(command.business_key.as_str()) {
                transaction.commit().await.map_err(database_error)?;
                return Ok(());
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
        let account_id = AccountId(row.try_get("account_id").map_err(database_error)?);
        let held: i64 = row.try_get("held_microusd").map_err(database_error)?;
        sqlx::query(
            "UPDATE ledger.accounts SET balance_microusd = balance_microusd + $2, updated_at = now() WHERE id = $1",
        )
        .bind(account_id.0)
        .bind(held)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
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
        transaction.commit().await.map_err(database_error)
    }
}

async fn insert_audit(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    actor: &str,
    action: &str,
    subject_type: &str,
    subject_id: &str,
    payload: &Value,
) -> Result<(), ApplicationError> {
    sqlx::query(
        r#"
        INSERT INTO operations.audit_events
            (id, actor, action, subject_type, subject_id, payload)
        VALUES ($1,$2,$3,$4,$5,$6)
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(actor)
    .bind(action)
    .bind(subject_type)
    .bind(subject_id)
    .bind(payload)
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

fn row_to_candidate(row: &sqlx::postgres::PgRow) -> Result<OfferingCandidate, ApplicationError> {
    let offering_id = OfferingId(row.try_get("offering_id").map_err(database_error)?);
    let channel_id = ChannelId(row.try_get("channel_id").map_err(database_error)?);
    let provider_kind: String = row.try_get("provider_kind").map_err(database_error)?;
    let pricing = row_candidate_pricing(row, offering_id.0)?;
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
            price_plan_id: PricePlanId(row.try_get("price_plan_id").map_err(database_error)?),
            rates: PriceRates {
                currency: row.try_get("currency").map_err(database_error)?,
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
            },
            captured_at: row.try_get("captured_at").map_err(database_error)?,
            // 命中候选就是这条候选：受理时选中哪条，就把它这份快照固化进 Job。
            hit_candidate: Some(HitCandidate {
                offering_id,
                channel_id,
                provider_kind,
            }),
            consumer_rates_cny: pricing.consumer_rates_cny,
            tier_prices: pricing.tier_prices,
            floor_amounts: pricing.floor_amounts,
            // 保底额与汇率**依赖这次请求**，由受理用例算定后填（发布侧算不出来）。
            hold_microusd: None,
            hold_source: None,
            cost_basis: pricing.cost_basis,
            reference_cost_microusd: pricing.reference_cost_microusd,
            cost_currency: pricing.cost_currency,
            markup_bps: row.try_get("markup_bps").map_err(database_error)?,
            fx_rate: None,
        },
        routing_priority: row.try_get("routing_priority").map_err(database_error)?,
        weight: row_weight(row)?,
    })
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

/// 一条候选在修订上的**定价**（按候选键的六个映射 + 修订级加价系数）。
///
/// 六个映射都缺席（`NULL`，或映射里没有这条候选）时全部为 `None`：这条候选不带定价，
/// 受理与结算走旧口径。这是"迁移前的旧修订"与"发布了定价但表为空"分得开的关键。
struct CandidatePricingRow {
    consumer_rates_cny: Option<ConsumerRatesCny>,
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
    // 承载面与映射从 **Job 自己那两列**取——它们是受理时随 Job 冻结的快照，
    // 不跟着发布物走，所以改发布之后旧 Job 读到的仍是旧承载面。
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
