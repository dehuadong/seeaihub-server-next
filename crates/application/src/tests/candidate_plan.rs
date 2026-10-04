use super::*;

/// 判定只出计划：候选数从 1 到 8 到 32，都不会为候选复制图片，也不会为候选构造映射后的参数对象。
///
/// 判据都是可判定的：① 逐条判定**不动**物化计数——物化正是构造映射后参数对象、也是唯一装载
/// 图片取值的那一步，所以它不随候选数增长，图片分配也就不可能随候选数线性增长；② 判定函数只看得到
/// [`RequestFeatures`]（普通参数借用 + 图片张数），图片取值根本不在它的作用域里；③ 计划里图片位
/// 只是一个在场标记，图片取值与普通参数取值都不出现在计划里；④ 选中之后物化计数只加一次。
///
/// 这条用例不用分配计数：计数要自定义 `GlobalAlloc`，而工作区禁 `unsafe`（见 workspace lints）。
#[test]
fn planning_never_materializes_and_the_chosen_candidate_is_materialized_once() {
    let prompt = "planning-test-prompt-6b1f".repeat(64);
    let image = format!(
        "data:image/png;base64,{}",
        "planning-test-image-9a2c".repeat(64)
    );
    let mut request = image_request(serde_json::json!({"prompt": prompt.clone()}));
    request.reference_images = vec![image.clone()];
    let branch = request.branch().expect("image conditioned");

    let first = offering();
    let face = contract_face(&request, &first);
    let shared = SharedInput {
        parameters: &face,
        reference_images: &request.reference_images,
        mask: request.mask.as_deref(),
    };
    let features = shared.features(branch);
    let mut candidates = vec![candidate_of(&first, 0)];
    let mut parameters = None;
    for count in [1_usize, 8, 32] {
        while candidates.len() < count {
            let mut other = offering();
            other.offering_id = OfferingId::new();
            candidates.push(candidate_of(&other, candidates.len() as i32));
        }
        let before = materializations_probe::count();
        let plans: Vec<CandidatePlan<'_>> = candidates
            .iter()
            .map(|candidate| plan_candidate(candidate, &features, None))
            .collect();
        assert_eq!(
            materializations_probe::count(),
            before,
            "{count} 条候选的判定一条都不物化"
        );
        assert!(plans.iter().all(CandidatePlan::eligible));
        assert_eq!(plans.len(), count);
        for plan in &plans {
            let image_fields: Vec<&PlannedField> = plan
                .fields
                .iter()
                .filter(|field| matches!(field.value, PlannedValue::PlacedImage))
                .collect();
            assert_eq!(image_fields.len(), 1, "图片位在计划里只有一个在场标记");
            assert_eq!(image_fields[0].wire, "image");
            // 计划里既没有图片取值也没有普通参数取值：它们只在物化那一步从共享输入取。
            let rendered = format!("{plan:?}");
            assert!(!rendered.contains(&image), "计划不得持有图片取值");
            assert!(!rendered.contains(&prompt), "计划不得持有普通参数取值");
        }

        let before_selection = materializations_probe::count();
        let (_, prepared, _) = select_candidate(&request, branch, &candidates, None)
            .expect("every candidate carries the request");
        assert_eq!(
            materializations_probe::count(),
            before_selection + 1,
            "{count} 条候选里只有选中那条物化一次"
        );
        assert_eq!(prepared.get("image"), Some(&Value::String(image.clone())));
        match &parameters {
            None => parameters = Some(prepared),
            Some(first) => assert_eq!(first, &prepared, "候选数不改变物化结果"),
        }
    }
}

/// 物化时才发现实际承载不了：按既有机制在**受理前**排除这条候选并重选。
///
/// 实验用的是判定阶段看不到的取值：把参考图字段挂进取值映射表，表里没有这次真实图片取值，
/// 于是只有物化那一步会失败。判定记录据此把这条候选记成不合格、命中另一条；一条都不剩时走
/// "无可用供给"，不建 Job、也没有 Provider 副作用。
#[test]
fn a_materialization_failure_excludes_the_candidate_and_reselects() {
    let contract = surface(serde_json::json!({
        "model": {"const": "gpt-image-2"},
        "prompt": {"type": "string"},
        "image": {"type": "string"}
    }));
    // 优先级 0 的候选把 `image` 挂进取值映射，表里没有这次真实图片取值。
    let mut mapped = offering();
    mapped.capability_schema = contract.clone();
    mapped.carrier_schema = contract.clone();
    mapped.parameter_mapping = serde_json::json!({
        "enum_map": {"image": {"https://example.invalid/other.png": "x"}}
    });
    let mut wide = offering();
    wide.capability_schema = contract.clone();
    wide.carrier_schema = contract.clone();
    wide.offering_id = OfferingId::new();

    let mut request = image_request(serde_json::json!({"prompt": "hello"}));
    request.reference_images = vec!["https://example.invalid/a.png".to_owned()];
    let branch = request.branch().expect("image conditioned");

    // 判定只按张数判：这条候选合格，落选只能发生在物化那一步。
    let face = contract_face(&request, &mapped);
    let shared = SharedInput {
        parameters: &face,
        reference_images: &request.reference_images,
        mask: request.mask.as_deref(),
    };
    let features = shared.features(branch);
    let mapped_candidate = candidate_of(&mapped, 0);
    let plan = plan_candidate(&mapped_candidate, &features, None);
    assert!(plan.eligible());
    let reason = materialize_selected(&plan, &shared)
        .expect_err("the value table has no entry for this image");
    assert!(reason.contains("image"), "{reason}");

    // 选路：物化失败的那条在受理前被排除、命中下一条（物化两次：先失败一次，再成功一次）。
    let candidates = vec![candidate_of(&mapped, 0), candidate_of(&wide, 1)];
    let before = materializations_probe::count();
    let (chosen, parameters, decision) = select_candidate(&request, branch, &candidates, None)
        .expect("the second candidate carries the image as it is");
    assert_eq!(chosen.offering_id, wide.offering_id);
    assert_eq!(
        materializations_probe::count(),
        before + 2,
        "物化失败一次、重选后成功一次"
    );
    assert!(!decision.considered[0].eligible);
    assert!(
        decision.considered[0]
            .skip_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("image")),
        "落选原因要写明是图片位上的取值映射不了：{:?}",
        decision.considered[0].skip_reason
    );
    assert!(decision.considered[1].eligible);
    assert_eq!(
        parameters.get("image"),
        Some(&Value::String("https://example.invalid/a.png".to_owned()))
    );

    // 一条都不剩：平台侧供给问题，不是参数错。
    let error = select_candidate(&request, branch, &candidates[..1], None)
        .expect_err("no candidate can map this image");
    assert!(
        matches!(error, ApplicationError::NoEligibleOffering(_)),
        "{error}"
    );
}
