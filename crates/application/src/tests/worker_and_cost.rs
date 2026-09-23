use super::*;

struct WorkerRepository {
    job: Mutex<Option<GenerationJob>>,
    completion: Mutex<Option<CompleteJob>>,
    failure: Mutex<Option<AttemptFailure>>,
    events: Arc<Mutex<Vec<&'static str>>>,
}

impl WorkerRepository {
    fn new(job: GenerationJob, events: Arc<Mutex<Vec<&'static str>>>) -> Self {
        Self {
            job: Mutex::new(Some(job)),
            completion: Mutex::new(None),
            failure: Mutex::new(None),
            events,
        }
    }
}

fn unused_repository<T>() -> Result<T, ApplicationError> {
    Err(ApplicationError::Persistence(
        "unused repository operation in worker test".to_owned(),
    ))
}

#[async_trait]
impl HubRepository for WorkerRepository {
    async fn publish_runtime(
        &self,
        _request: PublishRuntimeRequest,
    ) -> Result<PublishedRevision, ApplicationError> {
        unused_repository()
    }

    async fn active_offering(
        &self,
        _native_model_id: &str,
    ) -> Result<Vec<OfferingCandidate>, ApplicationError> {
        unused_repository()
    }

    async fn published_models(&self) -> Result<Vec<PublishedModel>, ApplicationError> {
        unused_repository()
    }

    async fn gateway_models(&self) -> Result<Vec<GatewayModelView>, ApplicationError> {
        unused_repository()
    }

    async fn set_gateway_model_enabled(
        &self,
        _gateway_model: &str,
        _enabled: bool,
        _actor: &str,
    ) -> Result<(), ApplicationError> {
        unused_repository()
    }

    async fn set_offering_enabled(
        &self,
        _offering_id: OfferingId,
        _enabled: bool,
        _actor: &str,
    ) -> Result<Vec<String>, ApplicationError> {
        unused_repository()
    }

    async fn set_channel_enabled(
        &self,
        _channel_id: ChannelId,
        _enabled: bool,
        _actor: &str,
    ) -> Result<Vec<String>, ApplicationError> {
        unused_repository()
    }

    async fn upsert_fx_rate(&self, _rate: NewFxRate, _actor: &str) -> Result<(), ApplicationError> {
        unused_repository()
    }

    async fn effective_fx_rate(&self, _currency: &str) -> Result<Option<FxRate>, ApplicationError> {
        unused_repository()
    }

    async fn provider_cost_gaps(
        &self,
        _limit: u32,
    ) -> Result<Vec<ProviderCostGapView>, ApplicationError> {
        unused_repository()
    }

    async fn create_account(
        &self,
        _account_id: AccountId,
        _initial_credit_microusd: u64,
        _actor: &str,
    ) -> Result<BalanceChange, ApplicationError> {
        unused_repository()
    }

    async fn credit_account(
        &self,
        _account_id: AccountId,
        _amount_microusd: u64,
        _business_key: &str,
        _actor: &str,
    ) -> Result<BalanceChange, ApplicationError> {
        unused_repository()
    }

    async fn read_account_balance(
        &self,
        _account_id: AccountId,
    ) -> Result<BalanceChange, ApplicationError> {
        unused_repository()
    }

    async fn account_tag(
        &self,
        _account_id: AccountId,
    ) -> Result<Option<String>, ApplicationError> {
        unused_repository()
    }

    async fn set_account_tag(
        &self,
        _account_id: AccountId,
        _tag: Option<&str>,
        _actor: &str,
    ) -> Result<(), ApplicationError> {
        unused_repository()
    }

    async fn route_policy(
        &self,
        _gateway_model: &str,
    ) -> Result<Option<RoutePolicy>, ApplicationError> {
        unused_repository()
    }

    async fn upsert_route_policy(
        &self,
        _policy: &RoutePolicy,
        _actor: &str,
    ) -> Result<(), ApplicationError> {
        unused_repository()
    }

    async fn route_policies(&self) -> Result<Vec<RoutePolicy>, ApplicationError> {
        unused_repository()
    }

    async fn acceptance_probe(
        &self,
        _gateway_model: &str,
        _account_id: AccountId,
        _idempotency_key: &str,
    ) -> Result<AcceptanceProbe, ApplicationError> {
        unused_repository()
    }

    async fn accounts_updated_within(
        &self,
        _window: Duration,
    ) -> Result<Vec<BalanceChange>, ApplicationError> {
        unused_repository()
    }

    async fn insert_audit_event(
        &self,
        _actor: &str,
        _action: &str,
        _subject_type: &str,
        _subject_id: &str,
        _payload: Value,
    ) -> Result<(), ApplicationError> {
        unused_repository()
    }

    async fn create_api_key(
        &self,
        _account_id: AccountId,
        _label: &str,
        _key_hash: &str,
        _actor: &str,
    ) -> Result<(), ApplicationError> {
        unused_repository()
    }

    async fn account_for_api_key(&self, _key_hash: &str) -> Result<AccountId, ApplicationError> {
        unused_repository()
    }

    async fn create_job(
        &self,
        _command: CreateImageGeneration,
        _branch: ImageBranch,
        _offering: PublishedOffering,
        _request_hash: String,
        _routing: RoutingDecision,
    ) -> Result<(GenerationJob, BalanceChange), ApplicationError> {
        unused_repository()
    }

    async fn get_job(
        &self,
        _account_id: AccountId,
        _job_id: JobId,
    ) -> Result<JobView, ApplicationError> {
        unused_repository()
    }

    async fn claim_next_job(
        &self,
        worker_id: &str,
        lease_duration: ChronoDuration,
    ) -> Result<Option<ClaimedJob>, ApplicationError> {
        let job = self
            .job
            .lock()
            .map_err(|error| ApplicationError::Persistence(error.to_string()))?
            .take();
        Ok(job.map(|job| ClaimedJob {
            job,
            lease_owner: worker_id.to_owned(),
            lease_expires_at: Utc::now() + lease_duration,
        }))
    }

    async fn recover_expired_leases(&self) -> Result<LeaseRecovery, ApplicationError> {
        Ok(LeaseRecovery::default())
    }

    async fn begin_attempt(
        &self,
        _job_id: JobId,
        _worker_id: &str,
        _attempt_id: AttemptId,
        _request_digest: &str,
    ) -> Result<(), ApplicationError> {
        Ok(())
    }

    async fn renew_lease(
        &self,
        _job_id: JobId,
        _worker_id: &str,
        _lease_duration: ChronoDuration,
    ) -> Result<(), ApplicationError> {
        Ok(())
    }

    async fn complete_job(
        &self,
        completion: CompleteJob,
    ) -> Result<BalanceChange, ApplicationError> {
        self.events
            .lock()
            .map_err(|error| ApplicationError::Persistence(error.to_string()))?
            .push("complete");
        *self
            .completion
            .lock()
            .map_err(|error| ApplicationError::Persistence(error.to_string()))? = Some(completion);
        Ok(worker_balance_change())
    }

    async fn fail_job(
        &self,
        _job_id: JobId,
        _worker_id: &str,
        _attempt_id: Option<AttemptId>,
        failure: AttemptFailure,
    ) -> Result<BalanceChange, ApplicationError> {
        *self
            .failure
            .lock()
            .map_err(|error| ApplicationError::Persistence(error.to_string()))? = Some(failure);
        Ok(worker_balance_change())
    }

    async fn provider_failures(
        &self,
        _query: ProviderFailureQuery,
    ) -> Result<Vec<ProviderFailureView>, ApplicationError> {
        Ok(Vec::new())
    }

    async fn count_in_flight_jobs(
        &self,
        _account_id: AccountId,
        _except_idempotency_key: &str,
    ) -> Result<u64, ApplicationError> {
        Ok(0)
    }

    async fn list_open_reconciliation_cases(
        &self,
    ) -> Result<Vec<ReconciliationCaseView>, ApplicationError> {
        unused_repository()
    }

    async fn refund_reconciliation(
        &self,
        _command: RefundReconciliationCommand,
    ) -> Result<BalanceChange, ApplicationError> {
        unused_repository()
    }
}

/// 假仓库给出的余额变更：这组用例只验 Worker 的编排，加速层是关着的，数值没有意义。
fn worker_balance_change() -> BalanceChange {
    BalanceChange {
        account_id: AccountId::new(),
        balance_microusd: 0,
        updated_at: Utc::now(),
    }
}

struct WorkerAdapter {
    succeeds: bool,
    /// 上游确认生成、却**没交付任何图**：结果交付失败那条路径的夹具。
    empty_result: bool,
    /// 失败时附在错误上的成本事实：扮 Driver 在终态之后判定失败、把已经读到的金额带回来。
    failure_cost: Option<ProviderCost>,
    calls: AtomicUsize,
}

impl WorkerAdapter {
    fn new(succeeds: bool, empty_result: bool) -> Self {
        Self {
            succeeds,
            empty_result,
            failure_cost: None,
            calls: AtomicUsize::new(0),
        }
    }

    /// 让失败分支带上成本事实（负试用例里扮"终态给了金额、这次却没出图"）。
    fn reporting_failure_cost(mut self, provider_cost: ProviderCost) -> Self {
        self.failure_cost = Some(provider_cost);
        self
    }
}

#[async_trait]
impl ImageAdapter for WorkerAdapter {
    fn key(&self) -> &'static str {
        "aihubmix-image-v1"
    }

    async fn execute(
        &self,
        _request: PreparedImageRequest,
        _credential: &ProviderCredential,
    ) -> Result<ProviderSuccess, AdapterError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.succeeds {
            return Err(ProviderCallError {
                code: "provider_response_invalid".to_owned(),
                message: "missing verified usage".to_owned(),
                trace_id: None,
                retry_safety: RetrySafety::AcceptanceUnknown,
                kind: ProviderFailureKind::PlatformInternal,
                provider_cost: self.failure_cost.clone(),
            }
            .into());
        }
        Ok(ProviderSuccess {
            images: if self.empty_result {
                Vec::new()
            } else {
                vec![GeneratedImage::from_base64("iVBORw0KGgo=".to_owned())]
            },
            usage: TokenUsage {
                input_tokens: 9,
                input_text_tokens: 9,
                input_image_tokens: 0,
                output_tokens: 196,
                output_text_tokens: 0,
                output_image_tokens: 196,
                total_tokens: 205,
            },
            response_digest: "provider-response-digest".to_owned(),
            provider_trace_id: Some("provider-request-1".to_owned()),
            // 这条假 Driver 扮的是"不给金额字段"的渠道：成本由平台按实际用量自算。
            provider_cost: ProviderCost::Computed,
        })
    }
}

struct WorkerAdapterFactory {
    adapter: Arc<WorkerAdapter>,
}

impl AdapterFactory for WorkerAdapterFactory {
    fn descriptor(&self, _adapter_key: &str) -> Option<AdapterDescriptor> {
        None
    }

    fn validate_publication(
        &self,
        _adapter_key: &str,
        _capability_schema: &Value,
        _restrictions: &Value,
    ) -> Result<(), String> {
        Ok(())
    }

    fn create(
        &self,
        _adapter_key: &str,
        _base_url: &str,
        _timeout: Duration,
    ) -> Result<Arc<dyn ImageAdapter>, ApplicationError> {
        Ok(self.adapter.clone())
    }
}

struct WorkerCredentialProvider;

impl CredentialProvider for WorkerCredentialProvider {
    fn resolve(&self, _reference: &str) -> Result<ProviderCredential, ApplicationError> {
        ProviderCredential::new("test-credential".to_owned())
            .map_err(|error| ApplicationError::Configuration(error.to_string()))
    }
}

fn worker_job(max_cost_microusd: u64) -> GenerationJob {
    GenerationJob {
        id: JobId::new(),
        account_id: AccountId::new(),
        state: JobState::Leased,
        branch: ImageBranch::PromptOnly,
        gateway_model: "gpt-image-2".to_owned(),
        native_parameters: serde_json::json!({"prompt": "worker contract"}),
        offering: offering(),
        idempotency_key: "worker-contract-1".to_owned(),
        request_hash: "request-hash".to_owned(),
        max_cost_microusd,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn worker(repository: Arc<WorkerRepository>, adapter: Arc<WorkerAdapter>) -> WorkerService {
    WorkerService::new(
        repository,
        Arc::new(WorkerAdapterFactory { adapter }),
        Arc::new(WorkerCredentialProvider),
        "worker-test".to_owned(),
        ChronoDuration::seconds(30),
        Duration::from_secs(1),
    )
    .expect("worker fixture must be valid")
}

#[tokio::test]
async fn worker_settles_the_provider_image_envelope_with_the_evidence() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let job = worker_job(20_000);
    // 成本侧要算的两个数（费率与币种）先从 Job 快照里取出来：断言得按**成本那一侧**的
    // 入口独立算一遍，而不是照抄实现里那个数——照抄的话，把对客金额接回来也照样过。
    let snapshot = job.offering.price_snapshot.clone();
    let repository = Arc::new(WorkerRepository::new(job, events.clone()));
    let adapter = Arc::new(WorkerAdapter::new(true, false));

    assert!(
        worker(repository.clone(), adapter.clone())
            .run_once()
            .await
            .expect("worker run must succeed")
    );
    assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
    assert_eq!(*events.lock().expect("event lock"), vec!["complete"]);
    let completion = repository
        .completion
        .lock()
        .expect("completion lock")
        .take()
        .expect("job must complete");
    assert_eq!(completion.evidence.usage.total_tokens, 205);
    let expected_cost = snapshot
        .cost_rates()
        .expect("这条快照带 Price Plan")
        .amount_microusd(&completion.evidence.usage)
        .expect("the channel cost rates must price the actual usage");
    // 9 文本输入 × 5 + 196 图像输出 × 30（每 1M）。
    assert_eq!(expected_cost, 5_925);
    assert_eq!(
        completion.provider_cost,
        ProviderCostFact {
            source: ProviderCostSource::Computed,
            amount_microusd: Some(expected_cost),
            currency: Some(
                snapshot
                    .cost_currency()
                    .expect("这条快照带成本币种")
                    .to_owned(),
            ),
            cny_microusd: None,
        },
        "渠道不报金额时，成本按**本次实际用量 × 该渠道成本费率**自算，币种按渠道声明"
    );
    // 对客扣费走的是另一条入口（对客费率）。今天价格计划表暂时兼作成本费率，两个数恰好
    // 相同；它们仍是两个量——对客费率拆出去之后，成本不能跟着售价走。
    assert_eq!(completion.charge_microusd, 5_925);
    assert_eq!(
        completion.images,
        vec![GeneratedImage::from_base64("iVBORw0KGgo=".to_owned())],
        "结果信封必须原样落到 Job：渠道给 base64 就留 base64"
    );
    assert!(repository.failure.lock().expect("failure lock").is_none());
}

/// 成本来源的判定：上游给金额就**直接取**，渠道不报就按实际用量自算，拿不到就留空（不猜）。
///
/// 同一个用量与同一份快照下走三态，钉住"判据是成本从哪来，不是金额对不对"。
#[test]
fn provider_cost_source_follows_where_the_cost_came_from() {
    let snapshot = offering().price_snapshot;
    let usage = TokenUsage {
        input_tokens: 9,
        input_text_tokens: 9,
        input_image_tokens: 0,
        output_tokens: 196,
        output_text_tokens: 0,
        output_image_tokens: 196,
        total_tokens: 205,
    };
    // 声明分支：金额与币种**都取上游随金额报回的那一份**，自算那份费率不参与这一态
    // ——不是"传了却忘了用"，是这条来源本就不认它。
    let declared = provider_cost_fact(
        &snapshot,
        &ProviderCost::Declared(seeai_adapter_sdk::DeclaredCost {
            amount_microusd: 11_354,
            currency: "CNY".to_owned(),
        }),
        CostInputs::Succeeded {
            usage: &usage,
            images: 1,
        },
    );
    assert_eq!(declared.source, ProviderCostSource::Declared);
    assert_eq!(
        declared.amount_microusd,
        Some(11_354),
        "上游给了金额就直接取它，不许用自算值顶替"
    );
    assert_eq!(
        declared.currency.as_deref(),
        Some("CNY"),
        "币种按上游报的那一份，不取渠道声明的成本币种、也不假定 USD"
    );

    let computed = provider_cost_fact(
        &snapshot,
        &ProviderCost::Computed,
        CostInputs::Succeeded {
            usage: &usage,
            images: 1,
        },
    );
    assert_eq!(computed.source, ProviderCostSource::Computed);
    // 9 文本输入 × 5 + 196 图像输出 × 30（每 1M）。
    assert_eq!(computed.amount_microusd, Some(5_925));
    assert_eq!(computed.currency.as_deref(), Some("USD"));

    let unavailable = provider_cost_fact(
        &snapshot,
        &ProviderCost::Unavailable,
        CostInputs::Succeeded {
            usage: &usage,
            images: 1,
        },
    );
    assert_eq!(unavailable.source, ProviderCostSource::Unavailable);
    assert_eq!(
        unavailable.amount_microusd, None,
        "拿不到金额就留空，不许写 0"
    );
    assert_eq!(unavailable.currency, None);
    // 这份快照没有定价（没有冻结的汇率）：折算值留空，是"没有折算值"，不是"折算成了 0"。
    for fact in [&declared, &computed, &unavailable] {
        assert_eq!(fact.cny_microusd, None);
    }

    // 用量不在手里（失败件没有本次用量）：自算那一态算不出金额，按缺口落，不猜。
    let without_usage = provider_cost_fact(&snapshot, &ProviderCost::Computed, CostInputs::Failed);
    assert_eq!(without_usage.source, ProviderCostSource::Unavailable);
    assert_eq!(without_usage.amount_microusd, None);
    assert_eq!(without_usage.currency, None);
}

/// 按张 / 按次计费的渠道：成本 = **数量 × 单价**，形态与它的参数随快照冻结。
///
/// 逐位断言三种情形：按张看**产出的张数**、按次永远是**一次的钱**（不随张数变）、
/// 上游直接给金额的形态平台没有可算的东西（上游没给就是缺口）。这两种形态的快照里
/// **没有四档费率**——Price Plan 是 token 计量量那一种形态的参数，不是成本自算的前提。
#[test]
fn a_supply_priced_per_image_or_per_call_computes_from_its_unit_price() {
    let usage = TokenUsage {
        input_tokens: 9,
        input_text_tokens: 9,
        input_image_tokens: 0,
        output_tokens: 196,
        output_text_tokens: 0,
        output_image_tokens: 196,
        total_tokens: 205,
    };
    let mut snapshot = offering().price_snapshot;
    snapshot.price_plan_id = None;
    snapshot.rates = None;
    snapshot.cost_currency = Some("USD".to_owned());
    snapshot.fx_rate = Some(FxRate {
        currency: "USD".to_owned(),
        rate_micros: 7_100_000,
        effective_at: Utc::now(),
    });

    // 按张：3 张 × 11_354 = 34_062（微单位），折算 34_062 × 7.1 = 241840.2 ⇒ 241841。
    let mut per_image = snapshot.clone();
    per_image.formula = PricingFormula::PerImage;
    per_image.cost_unit_price_microusd = Some(11_354);
    let fact = provider_cost_fact(
        &per_image,
        &ProviderCost::Computed,
        CostInputs::Succeeded {
            usage: &usage,
            images: 3,
        },
    );
    assert_eq!(fact.source, ProviderCostSource::Computed);
    assert_eq!(fact.amount_microusd, Some(34_062), "按张 = 张数 × 单价");
    assert_eq!(fact.currency.as_deref(), Some("USD"));
    assert_eq!(
        fact.cny_microusd,
        Some(241_841),
        "按冻结的汇率折成人民币算毛利"
    );

    // 按次：一次的钱，产出 7 张也一样。
    let mut per_call = snapshot.clone();
    per_call.formula = PricingFormula::PerCall;
    per_call.cost_unit_price_microusd = Some(20_000);
    let fact = provider_cost_fact(
        &per_call,
        &ProviderCost::Computed,
        CostInputs::Succeeded {
            usage: &usage,
            images: 7,
        },
    );
    assert_eq!(fact.amount_microusd, Some(20_000), "按次 = 1 × 单价");

    // 上游直接给金额的形态：平台没有可算的东西，上游没给就是缺口（不编一个数）。
    let mut declared_by_upstream = snapshot.clone();
    declared_by_upstream.formula = PricingFormula::UpstreamDeclared;
    let fact = provider_cost_fact(
        &declared_by_upstream,
        &ProviderCost::Computed,
        CostInputs::Succeeded {
            usage: &usage,
            images: 3,
        },
    );
    assert_eq!(fact.source, ProviderCostSource::Unavailable);
    assert_eq!(fact.amount_microusd, None);
    assert_eq!(fact.currency, None);

    // 失败件手里没有产出张数：按张算不出来 ⇒ 缺口，不用别的数顶替。
    let failed = provider_cost_fact(&per_image, &ProviderCost::Computed, CostInputs::Failed);
    assert_eq!(failed.source, ProviderCostSource::Unavailable);
    assert_eq!(failed.amount_microusd, None);

    // 上游给了金额时形态不参与：直接取它，失败件也一样（那是执行事实，不是自算）。
    let declared = provider_cost_fact(
        &per_image,
        &ProviderCost::Declared(seeai_adapter_sdk::DeclaredCost {
            amount_microusd: 11_354,
            currency: "USD".to_owned(),
        }),
        CostInputs::Failed,
    );
    assert_eq!(declared.source, ProviderCostSource::Declared);
    assert_eq!(declared.amount_microusd, Some(11_354));
    assert_eq!(declared.cny_microusd, Some(80_614));
}

/// 折算只在**成本币种与冻结的汇率对得上**时才做，且按定点整数算。
#[test]
fn the_cost_is_converted_with_the_frozen_rate_of_its_own_currency() {
    let mut snapshot = offering().price_snapshot;
    snapshot.fx_rate = Some(FxRate {
        currency: "USD".to_owned(),
        rate_micros: 7_100_000,
        effective_at: Utc::now(),
    });
    let usage = TokenUsage {
        input_tokens: 0,
        input_text_tokens: 0,
        input_image_tokens: 0,
        output_tokens: 0,
        output_text_tokens: 0,
        output_image_tokens: 0,
        total_tokens: 0,
    };
    let declared = provider_cost_fact(
        &snapshot,
        &ProviderCost::Declared(seeai_adapter_sdk::DeclaredCost {
            amount_microusd: 11_354,
            currency: "USD".to_owned(),
        }),
        CostInputs::Succeeded {
            usage: &usage,
            images: 1,
        },
    );
    // 11354 微美元 × 7.1 = 80613.4 微元 ⇒ 向上取整。
    assert_eq!(declared.cny_microusd, Some(80_614));

    // 上游报的币种与冻结的汇率不是一回事：不折（留空），不拿另一个币种的汇率去乘。
    let foreign = provider_cost_fact(
        &snapshot,
        &ProviderCost::Declared(seeai_adapter_sdk::DeclaredCost {
            amount_microusd: 11_354,
            currency: "CNY".to_owned(),
        }),
        CostInputs::Succeeded {
            usage: &usage,
            images: 1,
        },
    );
    assert_eq!(foreign.amount_microusd, Some(11_354));
    assert_eq!(foreign.cny_microusd, None);
}

/// 记进成本列的是**按渠道成本费率自算的成本**，不是对客扣费。
///
/// 把两个口径**人为设成不同的值**（成本读渠道成本费率、对客读自己的 CNY 向量）：这条断言
/// 防的是"又把对客金额接回来当成本"——那会让成本跟着售价漂移，而两者本来是两个量。
#[test]
fn the_recorded_computed_cost_is_not_the_consumer_charge() {
    let mut snapshot = offering().price_snapshot;
    snapshot.consumer_rates_cny = Some(ConsumerRatesCny {
        text_input_micros_per_million: 7_000_000,
        image_input_micros_per_million: 9_000_000,
        text_output_micros_per_million: 11_000_000,
        image_output_micros_per_million: 40_000_000,
    });
    let usage = TokenUsage {
        input_tokens: 9,
        input_text_tokens: 9,
        input_image_tokens: 0,
        output_tokens: 196,
        output_text_tokens: 0,
        output_image_tokens: 196,
        total_tokens: 205,
    };
    let cost = snapshot
        .cost_rates()
        .expect("这条快照带 Price Plan")
        .amount_microusd(&usage)
        .expect("cost rates price the usage");
    let charge = snapshot
        .charge_microusd(ChargeFacts {
            usage: &usage,
            images: 1,
            declared_cost_microusd: None,
        })
        .expect("consumer rates price the usage");
    assert_ne!(cost, charge, "用例得先让两个口径真的不同");

    let fact = provider_cost_fact(
        &snapshot,
        &ProviderCost::Computed,
        CostInputs::Succeeded {
            usage: &usage,
            images: 1,
        },
    );
    assert_eq!(
        fact.amount_microusd,
        Some(cost),
        "成本列记的是渠道成本费率算出来的钱"
    );
    assert_ne!(fact.amount_microusd, Some(charge), "成本列不许记对客扣费");
}

#[tokio::test]
async fn worker_sends_ambiguous_provider_response_to_reconciliation_once() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let repository = Arc::new(WorkerRepository::new(worker_job(20_000), events.clone()));
    let adapter = Arc::new(WorkerAdapter::new(false, false));

    assert!(
        worker(repository.clone(), adapter.clone())
            .run_once()
            .await
            .expect("worker run must converge")
    );
    assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
    let failure = repository
        .failure
        .lock()
        .expect("failure lock")
        .take()
        .expect("job must fail to reconciliation");
    assert_eq!(failure.target_state, JobState::ReconciliationRequired);
    assert_eq!(
        failure.hold_disposition,
        HoldDisposition::RetainForReconciliation
    );
    // 适配器没在错误上带回成本事实：按 `unavailable` 落（来源可辨、进缺口清单），
    // 而不是留 NULL 让这笔成本在账上与缺口两头都看不见。
    assert_eq!(
        failure.provider_cost,
        Some(ProviderCostFact {
            source: ProviderCostSource::Unavailable,
            amount_microusd: None,
            currency: None,
            cny_microusd: None,
        })
    );
    assert!(
        repository
            .completion
            .lock()
            .expect("completion lock")
            .is_none()
    );
}

/// 适配器在失败上带回的成本事实必须跟着失败件落库：执行已经发生、金额也读到了，
/// 把结果丢掉的同时把成本一起丢掉，等于这一笔既不在账上也不在缺口里。
#[tokio::test]
async fn worker_records_the_cost_fact_the_adapter_reported_on_failure() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let snapshot = offering().price_snapshot;
    let repository = Arc::new(WorkerRepository::new(worker_job(20_000), events.clone()));
    let adapter = Arc::new(
        WorkerAdapter::new(false, false).reporting_failure_cost(ProviderCost::Declared(
            seeai_adapter_sdk::DeclaredCost {
                amount_microusd: 11_354,
                currency: snapshot
                    .cost_currency()
                    .expect("这条快照带成本币种")
                    .to_owned(),
            },
        )),
    );

    assert!(
        worker(repository.clone(), adapter.clone())
            .run_once()
            .await
            .expect("worker run must converge")
    );
    let failure = repository
        .failure
        .lock()
        .expect("failure lock")
        .take()
        .expect("job must fail");
    assert_eq!(
        failure.provider_cost,
        Some(ProviderCostFact {
            source: ProviderCostSource::Declared,
            amount_microusd: Some(11_354),
            currency: Some(
                snapshot
                    .cost_currency()
                    .expect("这条快照带成本币种")
                    .to_owned(),
            ),
            cny_microusd: None,
        }),
        "失败件走与成功件同一套成本映射：来源、金额、币种都按适配器报的那一份"
    );
}

/// 第二种对账：**上游已确认生成、但平台无法交付结果**（上游没给任何图）。
///
/// 与第一种（创建阶段失联）的区别在这条路径上体现为**错误码不同**：
/// 这里是 `result_delivery_failed`，而创建阶段失联用 adapter 报的错误码。
/// 两者都进对账并保留预授权，但性质可分。
///
/// 这条路径同时钉住一件事：**执行已经发生、成本也拿得到**，所以成本事实必须跟着落下来
/// ——只有成功路径才落成本，等于把"这一笔到底花了多少钱"丢在一条已经付过钱的路径上。
/// 结算按实际、可透支之后，"实际费用超过预授权"不再是进对账的理由（那是旧口径），
/// 所以这里用"上游没给图"来构造这条路径。
#[tokio::test]
async fn worker_sends_settlement_failure_to_reconciliation_with_its_own_code() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let job = worker_job(1);
    let snapshot = job.offering.price_snapshot.clone();
    let repository = Arc::new(WorkerRepository::new(job, events.clone()));
    let adapter = Arc::new(WorkerAdapter::new(true, true));

    assert!(
        worker(repository.clone(), adapter.clone())
            .run_once()
            .await
            .expect("worker run must converge")
    );
    assert_eq!(
        adapter.calls.load(Ordering::SeqCst),
        1,
        "the provider was called exactly once; a settlement failure must not retry it"
    );
    let failure = repository
        .failure
        .lock()
        .expect("failure lock")
        .take()
        .expect("settlement failure must be recorded");
    assert_eq!(
        failure.provider_code, "result_delivery_failed",
        "the settlement failure must carry its own code, distinct from acceptance-unknown"
    );
    assert_eq!(
        failure.public_code,
        PublicErrorCode::OutcomeUnknown,
        "the result already exists, so the consumer must wait for the reconciliation outcome"
    );
    assert_eq!(failure.kind, ProviderFailureKind::PlatformInternal);
    assert_eq!(failure.target_state, JobState::ReconciliationRequired);
    assert_eq!(
        failure.hold_disposition,
        HoldDisposition::RetainForReconciliation,
        "a generated result must keep the hold for reconciliation"
    );
    // 执行已经发生：成本事实跟着这条路径一起落，且按**成本那一侧**的费率独立算出来。
    let expected = snapshot
        .cost_rates()
        .expect("这条快照带 Price Plan")
        .amount_microusd(&TokenUsage {
            input_tokens: 9,
            input_text_tokens: 9,
            input_image_tokens: 0,
            output_tokens: 196,
            output_text_tokens: 0,
            output_image_tokens: 196,
            total_tokens: 205,
        })
        .expect("the channel cost rates price the usage");
    assert_eq!(
        failure.provider_cost,
        Some(ProviderCostFact {
            source: ProviderCostSource::Computed,
            amount_microusd: Some(expected),
            currency: Some(
                snapshot
                    .cost_currency()
                    .expect("这条快照带成本币种")
                    .to_owned(),
            ),
            cny_microusd: None,
        }),
        "进对账这条路径上的成本事实必须有去处：执行已经发生、成本也拿得到"
    );
    assert!(
        repository
            .completion
            .lock()
            .expect("completion lock")
            .is_none(),
        "no settlement may happen when the provider delivered no result"
    );
}
