use super::*;

/// 选路：第一个候选承载不了请求用到的字段 → 落到下一条，判定记录写明为什么。
///
/// 一条都不合格时**不是**参数错：请求本身没违反合同，是平台的供给面承载不了它。
#[test]
fn routing_skips_a_candidate_that_cannot_carry_a_used_field() {
    let contract = surface(serde_json::json!({
        "model": {"const": "gpt-image-2"},
        "prompt": {"type": "string"},
        "quality": {"enum": ["low", "high"]}
    }));
    // 优先级 0 的候选承载面窄（承载不了 `quality`），优先级 1 的候选承载得了。
    let mut narrow = offering();
    narrow.capability_schema = contract.clone();
    narrow.carrier_schema = surface(serde_json::json!({
        "model": {"const": "gpt-image-2"},
        "prompt": {"type": "string"}
    }));
    let mut wide = offering();
    wide.capability_schema = contract.clone();
    wide.carrier_schema = contract.clone();
    // 两条候选是不同的供给：判定记录按 offering_id 记选中者。
    wide.offering_id = OfferingId::new();

    let request = image_request(serde_json::json!({"prompt": "hello", "quality": "high"}));
    let branch = request.branch().expect("prompt only");
    let (chosen, parameters, decision) = select_candidate(
        &request,
        branch,
        &[candidate_of(&narrow, 0), candidate_of(&wide, 1)],
        None,
    )
    .expect("the second candidate can carry quality");
    assert_eq!(chosen.offering_id, wide.offering_id);
    assert_eq!(parameters.get("quality"), Some(&serde_json::json!("high")));
    assert_eq!(
        decision.considered.len(),
        2,
        "判定记录要记全，不是记到命中为止"
    );
    assert!(!decision.considered[0].eligible);
    assert!(
        decision.considered[0]
            .skip_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("quality")),
        "落选原因必须写明承载不了哪个字段：{:?}",
        decision.considered[0].skip_reason
    );
    assert!(decision.considered[1].eligible);
    assert!(decision.considered[1].skip_reason.is_none());

    // 一条都不合格：平台侧供给问题，与"请求违反合同"分开报。
    let error = select_candidate(&request, branch, &[candidate_of(&narrow, 0)], None)
        .expect_err("no candidate can carry quality");
    assert!(
        matches!(error, ApplicationError::NoEligibleOffering(_)),
        "{error}"
    );
    // 请求本身违反合同（缺必填）：仍然是参数错。
    let missing_prompt = image_request(serde_json::json!({"quality": "high"}));
    let error = select_candidate(&missing_prompt, branch, &[candidate_of(&narrow, 0)], None)
        .expect_err("prompt is missing");
    assert!(matches!(error, ApplicationError::Validation(_)), "{error}");
    // 该型号一条 active 供给都没有：是"不存在"，不是"承载不了"。
    let error = select_candidate(&request, branch, &[], None).expect_err("no active offering");
    assert!(matches!(error, ApplicationError::NotFound(_)), "{error}");
}

/// 按 `(账户, 幂等键)` 与权重**重算**期望落点。
///
/// 故意在测试里独立写一遍，不调用生产实现的那个辅助函数：这条用例要证明的是"分摊确实由
/// 账户、幂等键与权重共同决定"，用被验对象自己算期望就什么也证明不了。
fn expected_weight_split(account_id: AccountId, key: &str, tier: &[(Uuid, u32)]) -> Uuid {
    let mut hasher = Sha256::new();
    hasher.update(account_id.0.as_bytes());
    hasher.update(key.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    let total: u64 = tier.iter().map(|(_, weight)| u64::from(*weight)).sum();
    let draw = u64::from_be_bytes(bytes) % total;
    let mut ordered = tier.to_vec();
    ordered.sort_by_key(|(offering_id, _)| *offering_id);
    let mut cursor = 0_u64;
    for (offering_id, weight) in ordered {
        cursor += u64::from(weight);
        if draw < cursor {
            return offering_id;
        }
    }
    unreachable!("落点必然落在某条候选的区间里")
}

/// 只用**判定记录**重建选中项：合格候选、档位、权重与落点都在里面，不需要别的事实。
fn rebuild_from_decision_record(decision: &RoutingDecision) -> Uuid {
    let tier = decision
        .considered
        .iter()
        .filter(|considered| considered.eligible)
        .map(|considered| considered.routing_priority)
        .min()
        .expect("a decision always has an eligible candidate");
    let mut ordered: Vec<(Uuid, u32)> = decision
        .considered
        .iter()
        .filter(|considered| considered.eligible && considered.routing_priority == tier)
        .map(|considered| (considered.offering_id.0, considered.weight))
        .collect();
    ordered.sort_by_key(|(offering_id, _)| *offering_id);
    let draw = decision.considered[0].weight_draw;
    let mut cursor = 0_u64;
    for (offering_id, weight) in ordered {
        cursor += u64::from(weight);
        if draw < cursor {
            return offering_id;
        }
    }
    unreachable!("落点必然落在某条候选的区间里")
}

/// `least_cost` 取**折后成本估算**最小的一条：折扣把贵的那条变便宜时它就该赢。
///
/// 估算 = 参考成本 × 该候选配的折扣率，缺省不打折。这里给贵的那条配一折，让它反过来更便宜，
/// 从而把"按折后估算比较"与"按原始成本比较"区分开。
#[test]
fn least_cost_compares_discounted_estimates() {
    let plain = offering();
    let mut discounted = offering();
    discounted.offering_id = OfferingId::new();
    let mut plain_candidate = candidate_with_weight(&plain, 0, 1);
    plain_candidate.price_snapshot.reference_cost_microusd = Some(10_000);
    let mut discounted_candidate = candidate_with_weight(&discounted, 0, 1);
    discounted_candidate.price_snapshot.reference_cost_microusd = Some(20_000);
    let mut discount_rates = BTreeMap::new();
    discount_rates.insert(discounted.offering_id.0.to_string(), 1_000);
    let tag_channel_map = BTreeMap::new();
    let choice = RouteChoice {
        strategy: RouteStrategy::LeastCost,
        discount_rates: &discount_rates,
        tag_channel_map: &tag_channel_map,
        account_tag: None,
    };
    let request = image_request(serde_json::json!({"prompt": "hello"}));
    let branch = request.branch().expect("prompt only");
    let candidates = vec![plain_candidate, discounted_candidate];
    let (chosen, _, _) =
        select_candidate_with_strategy(&request, branch, &candidates, None, &choice)
            .expect("a candidate must be chosen");
    assert_eq!(
        chosen.offering_id, discounted.offering_id,
        "折扣之后估算更小的那条要赢，而不是原始成本更小的那条"
    );
}

/// `user_tag`：标签经映射指定的候选要赢；映射指向的候选**不合格**时退回默认顺序。
///
/// 退回而不是判失败：别的候选明明能承载这次请求，把它们一起判掉没有任何好处。这条断言同时
/// 钉住"映射不是绕过承载校验的入口"。
#[test]
fn user_tag_takes_the_mapped_candidate_and_falls_back_when_it_cannot_carry() {
    let mapped = offering();
    let other = offering();
    let candidates = vec![
        candidate_with_weight(&other, 0, 1),
        candidate_with_weight(&mapped, 0, 1),
    ];
    let discount_rates = BTreeMap::new();
    let mut tag_channel_map = BTreeMap::new();
    tag_channel_map.insert("vip".to_owned(), mapped.offering_id.0.to_string());
    let choice = RouteChoice {
        strategy: RouteStrategy::UserTag,
        discount_rates: &discount_rates,
        tag_channel_map: &tag_channel_map,
        account_tag: Some("vip"),
    };
    let request = image_request(serde_json::json!({"prompt": "hello"}));
    let branch = request.branch().expect("prompt only");
    let (chosen, _, _) =
        select_candidate_with_strategy(&request, branch, &candidates, None, &choice)
            .expect("a candidate must be chosen");
    assert_eq!(
        chosen.offering_id, mapped.offering_id,
        "标签映射指向的候选要赢"
    );

    // 映射指向一个不存在于本次候选里的 id：退回默认顺序，仍然选出合格候选。
    let mut unknown_map = BTreeMap::new();
    unknown_map.insert("vip".to_owned(), OfferingId::new().0.to_string());
    let choice = RouteChoice {
        strategy: RouteStrategy::UserTag,
        discount_rates: &discount_rates,
        tag_channel_map: &unknown_map,
        account_tag: Some("vip"),
    };
    let (chosen, _, _) =
        select_candidate_with_strategy(&request, branch, &candidates, None, &choice)
            .expect("退回默认顺序也必须有候选");
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.offering_id == chosen.offering_id)
    );
}

/// `weighted_random` 不看档位：在**全部合格候选**里按权重分摊，且同一请求重放落同一条。
///
/// 与"档内分摊"的差别只在这一处：候选集合是全部合格候选，而不是最小档位那一档。落点仍由
/// `(账户, 幂等键)` 与权重决定，所以档 1 的候选也会分到请求——这正是它与 `priority_failover`
/// 的可观察差别。
#[test]
fn weighted_random_ignores_tiers_and_replays_to_the_same_candidate() {
    let account_id = AccountId(Uuid::from_u128(0x5eea_0000_0000_0000_0000_0000_0000_0009));
    let tier_zero = offering();
    let mut tier_one = offering();
    tier_one.offering_id = OfferingId::new();
    let candidates = vec![
        candidate_with_weight(&tier_zero, 0, 1),
        candidate_with_weight(&tier_one, 1, 3),
    ];

    let mut tier_one_hits = 0;
    let choice = RouteChoice {
        strategy: RouteStrategy::WeightedRandom,
        discount_rates: &BTreeMap::new(),
        tag_channel_map: &BTreeMap::new(),
        account_tag: None,
    };
    for index in 0..64 {
        let mut request = image_request(serde_json::json!({"prompt": "hello"}));
        request.account_id = account_id;
        request.idempotency_key = format!("weighted-key-{index}");
        let branch = request.branch().expect("prompt only");
        let (chosen, _, decision) =
            select_candidate_with_strategy(&request, branch, &candidates, None, &choice)
                .expect("a candidate must be chosen");
        let hit = decision
            .considered
            .iter()
            .find(|considered| considered.offering_id == chosen.offering_id)
            .expect("命中项必须在判定记录里");
        assert!(hit.eligible, "策略不得选中不合格候选：{hit:?}");
        // 重放：同一请求再来一次，必须落同一条候选。
        let (again, _, _) =
            select_candidate_with_strategy(&request, branch, &candidates, None, &choice)
                .expect("a replay must be chosen");
        assert_eq!(
            again.offering_id, chosen.offering_id,
            "同一 (账户, 幂等键) 必须落同一条候选"
        );
        if chosen.offering_id == tier_one.offering_id {
            tier_one_hits += 1;
        }
    }
    assert!(
        tier_one_hits > 0,
        "weighted_random 不看档位：档 1 的候选也应当分到请求"
    );
}

/// 同一档按权重分摊：逐条等于重算的期望，且判定记录自己就能重建结论。
///
/// 三条性质一起验：① 分摊由 `(账户, 幂等键)` 与权重决定（不是随机数、也不看行序）；
/// ② 同一批输入重放结果逐条相同；③ 权重 1:3 下权重大的那条确实分到更多。
#[test]
fn weight_splits_within_one_tier_deterministically() {
    let account_id = AccountId(Uuid::from_u128(0x5eea_0000_0000_0000_0000_0000_0000_0001));
    let light = offering();
    let mut heavy = offering();
    heavy.offering_id = OfferingId::new();
    let candidates = vec![
        candidate_with_weight(&light, 0, 1),
        candidate_with_weight(&heavy, 0, 3),
    ];
    let tier = vec![(light.offering_id.0, 1_u32), (heavy.offering_id.0, 3_u32)];

    let mut first_pass = Vec::new();
    let mut heavy_count = 0_u32;
    for index in 0..64 {
        let mut request = image_request(serde_json::json!({"prompt": "hello"}));
        request.account_id = account_id;
        request.idempotency_key = format!("weight-key-{index}");
        let branch = request.branch().expect("prompt only");
        let (chosen, _, decision) = select_candidate(&request, branch, &candidates, None)
            .expect("a candidate must be chosen");
        assert_eq!(
            chosen.offering_id.0,
            expected_weight_split(account_id, &request.idempotency_key, &tier),
            "落点必须由账户、幂等键与权重共同决定"
        );
        assert_eq!(
            rebuild_from_decision_record(&decision),
            chosen.offering_id.0,
            "判定记录必须足以重建选中项"
        );
        let draw = decision.considered[0].weight_draw;
        assert!(
            decision
                .considered
                .iter()
                .all(|considered| considered.weight_draw == draw),
            "分流落点是本次判定一个数，逐项同值：{:?}",
            decision.considered
        );
        assert!(draw < 4, "落点必须落在该档权重之和以内：{draw}");
        if chosen.offering_id == heavy.offering_id {
            heavy_count += 1;
        }
        first_pass.push(chosen.offering_id);
    }

    // 同一批输入重放：逐条相同。
    let mut second_pass = Vec::new();
    for index in 0..64 {
        let mut request = image_request(serde_json::json!({"prompt": "hello"}));
        request.account_id = account_id;
        request.idempotency_key = format!("weight-key-{index}");
        let branch = request.branch().expect("prompt only");
        let (chosen, _, _) = select_candidate(&request, branch, &candidates, None)
            .expect("a candidate must be chosen");
        second_pass.push(chosen.offering_id);
    }
    assert_eq!(first_pass, second_pass, "同一批输入必须逐条可复现");

    let distinct: std::collections::BTreeSet<Uuid> =
        first_pass.iter().map(|offering_id| offering_id.0).collect();
    assert_eq!(distinct.len(), 2, "权重 1:3 下两条候选都该被分到过");
    assert!(
        heavy_count > 32,
        "权重大的那条应当分到更多：{heavy_count}/64"
    );
}

/// 跨档：权重**不改变**档位顺序——档 0 有合格候选时，档 1 的权重再大也轮不到。
#[test]
fn weight_never_outranks_a_tier_that_has_an_eligible_candidate() {
    let first = offering();
    let mut second = offering();
    second.offering_id = OfferingId::new();
    let candidates = vec![
        candidate_with_weight(&first, 0, 1),
        candidate_with_weight(&second, 1, 1_000),
    ];
    for index in 0..16 {
        let mut request = image_request(serde_json::json!({"prompt": "hello"}));
        request.idempotency_key = format!("tier-key-{index}");
        let branch = request.branch().expect("prompt only");
        let (chosen, _, _) = select_candidate(&request, branch, &candidates, None)
            .expect("a candidate must be chosen");
        assert_eq!(
            chosen.offering_id, first.offering_id,
            "档 0 有合格候选时权重不该把它让给后面的档"
        );
    }
}

/// 输出张数超过某条候选承载面的上界**不影响它合格**：两条候选都进合格集合，选路结果与"谁会被夹"
/// 无关；被选中那条发出去的是它自己的上限。
///
/// 判据是"合格性先于策略"落在这里的形态：夹取发生在组装参数面时，而合格与否是同一趟算出来的——
/// 若把超上界当成不合格，这条权重策略就会永远跳过它，而调用方什么错都没犯。
#[test]
fn a_count_above_the_carriers_maximum_does_not_disqualify_it() {
    let contract = surface(serde_json::json!({
        "model": {"const": "gpt-image-2"},
        "prompt": {"type": "string"},
        "n": {"type": "integer", "minimum": 1, "maximum": 10}
    }));
    // 同档两条：窄承载面（`n` 最多 4）权重 1000，宽承载面（跟合同一样 10）权重 1。
    let mut narrow = offering();
    narrow.capability_schema = contract.clone();
    narrow.carrier_schema = surface(serde_json::json!({
        "model": {"const": "gpt-image-2"},
        "prompt": {"type": "string"},
        "n": {"type": "integer", "minimum": 1, "maximum": 4}
    }));
    let mut wide = offering();
    wide.capability_schema = contract.clone();
    wide.carrier_schema = contract;
    wide.offering_id = OfferingId::new();
    let candidates = vec![
        candidate_with_weight(&narrow, 0, 1_000),
        candidate_with_weight(&wide, 0, 1),
    ];
    let choice = RouteChoice {
        strategy: RouteStrategy::WeightedRandom,
        discount_rates: &BTreeMap::new(),
        tag_channel_map: &BTreeMap::new(),
        account_tag: None,
    };
    for index in 0..16 {
        let mut request = image_request(serde_json::json!({"prompt": "hello", "n": 6}));
        request.idempotency_key = format!("capped-key-{index}");
        let branch = request.branch().expect("prompt only");
        let (chosen, parameters, decision) =
            select_candidate_with_strategy(&request, branch, &candidates, None, &choice)
                .expect("both carriers take a request for 6 images");
        assert!(
            decision
                .considered
                .iter()
                .all(|considered| considered.eligible),
            "两条候选都合格：超承载面上界不是落选理由：{:?}",
            decision.considered
        );
        assert_eq!(
            chosen.offering_id, narrow.offering_id,
            "权重 1000 的那条照常按权重赢下分流"
        );
        assert_eq!(
            parameters.get("n"),
            Some(&serde_json::json!(4)),
            "选中那条发出去的是它自己声明的上限：{parameters}"
        );
    }
}

/// 不合格的候选**不进分摊**：权重写得再大也换不来一次选中。
///
/// 这就是"候选合格性优先于策略"在本层的落点——合格集合先算出来，权重只在集合内部起作用。
#[test]
fn an_ineligible_candidate_never_wins_the_split() {
    let contract = surface(serde_json::json!({
        "model": {"const": "gpt-image-2"},
        "prompt": {"type": "string"},
        "quality": {"enum": ["low", "high"]}
    }));
    // 同档两条：窄承载面（承载不了 `quality`）权重 1000，宽承载面权重 1。
    let mut narrow = offering();
    narrow.capability_schema = contract.clone();
    narrow.carrier_schema = surface(serde_json::json!({
        "model": {"const": "gpt-image-2"},
        "prompt": {"type": "string"}
    }));
    let mut wide = offering();
    wide.capability_schema = contract.clone();
    wide.carrier_schema = contract;
    wide.offering_id = OfferingId::new();
    let candidates = vec![
        candidate_with_weight(&narrow, 0, 1_000),
        candidate_with_weight(&wide, 0, 1),
    ];
    for index in 0..16 {
        let mut request = image_request(serde_json::json!({"prompt": "hello", "quality": "high"}));
        request.idempotency_key = format!("eligible-key-{index}");
        let branch = request.branch().expect("prompt only");
        let (chosen, _, decision) =
            select_candidate(&request, branch, &candidates, None).expect("the wide candidate fits");
        assert_eq!(
            chosen.offering_id, wide.offering_id,
            "不合格的候选不得因为权重大而被选中"
        );
        let skipped = decision
            .considered
            .iter()
            .find(|considered| considered.offering_id == narrow.offering_id)
            .expect("the narrow candidate must be considered");
        assert!(!skipped.eligible);
        assert!(
            skipped
                .skip_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("quality")),
            "落选原因要写明承载不了哪个字段：{:?}",
            skipped.skip_reason
        );
    }
}

/// 复核说某条供给已经停用 ⇒ 它与"承载面表达不了"同一条路：不合格，权重再大也换不来一次选中。
///
/// 复核结果只由**缓存给出的**候选集带来（`None` 表示这批候选刚回源读来），所以 `None` 那一支
/// 必须与复核引入之前逐位相同。
#[test]
fn a_reviewed_disabled_offering_is_ineligible_and_never_wins_the_split() {
    let disabled = offering();
    let mut still_on = offering();
    still_on.offering_id = OfferingId::new();
    let candidates = vec![
        candidate_with_weight(&disabled, 0, 1_000),
        candidate_with_weight(&still_on, 1, 1),
    ];
    let request = image_request(serde_json::json!({"prompt": "hello"}));
    let branch = request.branch().expect("prompt only");

    // 只有优先级 1 那条还在启用里：停用那条连档位都不占，落点因此下移。
    let enabled: HashSet<OfferingId> = [still_on.offering_id].into_iter().collect();
    let (chosen, _, decision) = select_candidate(&request, branch, &candidates, Some(&enabled))
        .expect("the second candidate is still enabled");
    assert_eq!(chosen.offering_id, still_on.offering_id);
    assert!(!decision.considered[0].eligible);
    assert_eq!(
        decision.considered[0].skip_reason.as_deref(),
        Some(DISABLED_OFFERING_REASON),
        "落选原因要写明是停用，运营才解释得清为什么没走这条"
    );
    assert!(decision.considered[1].eligible);

    // 一条都不在启用里：全不合格是平台侧供给问题，不是"模型不存在"（候选本身是有的）。
    let error = select_candidate(&request, branch, &candidates, Some(&HashSet::new()))
        .expect_err("every candidate was disabled");
    assert!(
        matches!(error, ApplicationError::NoEligibleOffering(_)),
        "{error}"
    );

    // 不复核（候选刚回源读来）：仍按档位选优先级 0 那条，逐位不变。
    let (chosen, _, _) = select_candidate(&request, branch, &candidates, None)
        .expect("an unreviewed candidate set routes as before");
    assert_eq!(chosen.offering_id, disabled.offering_id);
}

/// 显式档位与权重按候选归一；**缺省仍是"下标即档位、权重 1"**（老素材行为逐位不变）。
#[test]
fn explicit_priority_and_weight_are_normalized_for_shared_tiers() {
    let mut first = draft("m");
    first.routing_priority = Some(0);
    first.weight = Some(1);
    let mut second = draft("m");
    second.routing_priority = Some(0);
    second.weight = Some(3);
    let normalized = PublishRuntimeCommand {
        offerings: Some(vec![first, second]),
        ..base_command()
    }
    .normalize()
    .expect("two candidates may share one tier");
    assert_eq!(normalized.offerings[0].routing_priority, 0);
    assert_eq!(normalized.offerings[1].routing_priority, 0);
    assert_eq!(normalized.offerings[0].weight, 1);
    assert_eq!(normalized.offerings[1].weight, 3);

    let normalized = PublishRuntimeCommand {
        offerings: Some(vec![draft("m"), draft("m")]),
        ..base_command()
    }
    .normalize()
    .expect("the legacy shape still publishes");
    assert_eq!(normalized.offerings[0].routing_priority, 0);
    assert_eq!(normalized.offerings[1].routing_priority, 1);
    assert!(
        normalized
            .offerings
            .iter()
            .all(|offering| offering.weight == 1),
        "没给权重时一律是 1"
    );
}

/// 权重 0 与负档位在**发布期**就拒：它们是配置错误，不该等到分摊时表现为"分不到"。
#[test]
fn a_zero_weight_or_negative_priority_is_rejected() {
    let mut zero_weight = draft("m");
    zero_weight.weight = Some(0);
    let error = PublishRuntimeCommand {
        offerings: Some(vec![zero_weight]),
        ..base_command()
    }
    .normalize()
    .expect_err("a zero weight must be rejected");
    assert!(
        error
            .to_string()
            .contains("weight must be a positive integer"),
        "{error}"
    );

    let mut negative = draft("m");
    negative.routing_priority = Some(-1);
    let error = PublishRuntimeCommand {
        offerings: Some(vec![negative]),
        ..base_command()
    }
    .normalize()
    .expect_err("a negative priority must be rejected");
    assert!(
        error.to_string().contains("must not be negative"),
        "{error}"
    );
}

#[test]
fn branch_follows_the_images_the_caller_sent() {
    let mut request = image_request(serde_json::json!({"prompt": "hello"}));
    assert_eq!(
        request.branch().expect("prompt only"),
        ImageBranch::PromptOnly
    );
    request.reference_images = vec!["https://example.invalid/a.png".to_owned()];
    assert_eq!(
        request.branch().expect("image conditioned"),
        ImageBranch::ImageConditioned
    );
    request.mask = Some("data:image/png;base64,BBBB".to_owned());
    assert_eq!(request.branch().expect("masked"), ImageBranch::Masked);
    // 只有遮罩没有参考图：结构性规则，直接拒绝。
    request.reference_images.clear();
    assert!(request.branch().is_err());
}
