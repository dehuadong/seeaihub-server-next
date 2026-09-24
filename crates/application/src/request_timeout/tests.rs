use super::*;

/// 一套自洽的夹具：合同声明的输出张数上限 `n = 10` ⇒ 上限 180 + 6 × 30 = 360s。
fn policy() -> RequestTimeoutPolicy {
    RequestTimeoutPolicy {
        base: Duration::from_secs(180),
        included_images: 4,
        per_image: Duration::from_secs(30),
        provider_timeout: Duration::from_secs(360),
        worker_lease: Duration::from_secs(432),
        sync_wait: Duration::from_secs(390),
        max_output_images: 10,
    }
}

/// 逐位断言公式：基础含前 4 张，所以 1–4 张同值；第 5 张起每张加 30 秒。
#[test]
fn the_timeout_is_the_base_for_four_images_and_adds_a_budget_per_extra_image() {
    let base = Duration::from_secs(180);
    let per_image = Duration::from_secs(30);
    let expected = [
        (1_u64, 180_u64),
        (2, 180),
        (3, 180),
        (4, 180),
        (5, 210),
        (6, 240),
        (7, 270),
        (8, 300),
        (9, 330),
        (10, 360),
        (16, 540),
    ];
    for (images, seconds) in expected {
        assert_eq!(
            upstream_timeout(base, 4, per_image, images),
            Duration::from_secs(seconds),
            "n = {images} must be 180s + max(0, n - 4) x 30s"
        );
    }
}

#[test]
fn a_larger_n_never_gets_a_smaller_timeout() {
    let base = Duration::from_secs(180);
    let per_image = Duration::from_secs(30);
    let mut previous = Duration::ZERO;
    for images in 1..=16 {
        let timeout = upstream_timeout(base, 4, per_image, images);
        assert!(
            timeout >= previous,
            "n = {images} must not get less than n - 1"
        );
        previous = timeout;
    }
}

/// 基础含张数是 0 时退化成纯线性，公式仍然只写一遍：`基础 + n × 每张预算`。
#[test]
fn a_zero_included_images_makes_the_formula_linear() {
    let base = Duration::from_secs(180);
    let per_image = Duration::from_secs(30);
    assert_eq!(
        upstream_timeout(base, 0, per_image, 5),
        Duration::from_secs(330)
    );
}

/// 缺省 `n` 这条回归：没有 `n`、`n = 1` 与 `n` 不是整数都只生成一张，超时按一张算。
#[test]
fn a_missing_or_unusable_n_counts_as_one_image() {
    for parameters in [
        serde_json::json!({"prompt": "x"}),
        serde_json::json!({"prompt": "x", "n": 1}),
        serde_json::json!({"prompt": "x", "n": 0}),
        serde_json::json!({"prompt": "x", "n": "2"}),
        serde_json::json!({"prompt": "x", "n": 1.5}),
        serde_json::json!(null),
    ] {
        assert_eq!(
            requested_image_count(&parameters),
            1,
            "{parameters} must count as one image"
        );
    }
    assert_eq!(
        requested_image_count(&serde_json::json!({"prompt": "x", "n": 7})),
        7
    );
}

/// 逐位断言"按请求算"：4 张与 1 张同值（都在基础里），第 7 张比第 1 张大 90 秒。
#[test]
fn the_timeout_grows_with_the_requested_images() {
    let policy = policy();
    assert_eq!(policy.upstream_timeout_for(1), Duration::from_secs(180));
    assert_eq!(policy.upstream_timeout_for(4), Duration::from_secs(180));
    assert_eq!(policy.upstream_timeout_for(7), Duration::from_secs(270));
}

#[test]
fn the_per_request_timeout_is_capped_at_the_configured_ceiling() {
    let capped = RequestTimeoutPolicy {
        provider_timeout: Duration::from_secs(200),
        ..policy()
    };
    assert_eq!(
        capped.upstream_timeout_for(1),
        Duration::from_secs(180),
        "below the ceiling the formula decides"
    );
    assert_eq!(
        capped.upstream_timeout_for(10),
        Duration::from_secs(200),
        "above the ceiling the config decides"
    );
}

#[test]
fn the_sync_window_default_covers_the_ceiling_and_a_little_more() {
    let provider_timeout = Duration::from_secs(360);
    assert_eq!(
        RequestTimeoutPolicy::default_sync_wait(provider_timeout),
        Duration::from_secs(360 + SYNC_WAIT_OVERHEAD_SECONDS)
    );
}

/// 链的下半条第一段：上游超时小于"最大 `n` 下的按请求上限"就拒绝，且点名这条链与当前值。
#[test]
fn a_ceiling_below_the_largest_n_is_rejected_with_both_values_named() {
    let broken = RequestTimeoutPolicy {
        // 合同最大 n = 10：180 + 6 × 30 = 360，比 300 大。
        provider_timeout: Duration::from_secs(300),
        ..policy()
    };
    let error = broken.validate().expect_err("the chain must be rejected");
    let message = error.to_string();
    assert!(
        message.contains("PROVIDER_TIMEOUT_SECONDS (300s)"),
        "the error must name the current ceiling: {message}"
    );
    assert!(
        message.contains("n = 10") && message.contains("= 360s"),
        "the error must name the largest n and its bound: {message}"
    );
}

/// 链的下半条第二段：租约短于上游超时就拒绝——租约过期会让同一个 Job 再调一次上游。
#[test]
fn a_lease_below_the_upstream_timeout_is_rejected_with_both_values_named() {
    let broken = RequestTimeoutPolicy {
        worker_lease: Duration::from_secs(300),
        ..policy()
    };
    let error = broken.validate().expect_err("the chain must be rejected");
    let message = error.to_string();
    assert!(
        message.contains("WORKER_LEASE_SECONDS (300s)")
            && message.contains("PROVIDER_TIMEOUT_SECONDS (360s)"),
        "the error must name both ends of the lease chain: {message}"
    );
}

/// 链的上半条：对客窗口短于上游超时就拒绝——窗口先到期等于把还在生成、照样计费的那一次丢掉。
#[test]
fn a_sync_window_below_the_upstream_timeout_is_rejected_with_both_values_named() {
    let broken = RequestTimeoutPolicy {
        sync_wait: Duration::from_secs(120),
        ..policy()
    };
    let error = broken.validate().expect_err("the chain must be rejected");
    let message = error.to_string();
    assert!(
        message.contains("GENERATION_SYNC_WAIT_SECONDS (120s)")
            && message.contains("PROVIDER_TIMEOUT_SECONDS (360s)"),
        "the error must name both ends of the sync chain: {message}"
    );
}

#[test]
fn a_consistent_chain_is_accepted_at_its_boundaries() {
    assert!(policy().validate().is_ok());
    let boundary = RequestTimeoutPolicy {
        provider_timeout: Duration::from_secs(360),
        worker_lease: Duration::from_secs(360),
        sync_wait: Duration::from_secs(360),
        ..policy()
    };
    assert!(
        boundary.validate().is_ok(),
        "equal values satisfy \"at least as long as\", only strictly shorter is a break"
    );
}

/// 边界：上限刚好等于最大 `n` 的按请求上限时通过（`≥` 不是 `>`）。
#[test]
fn a_ceiling_equal_to_the_largest_n_bound_is_accepted() {
    let exact = RequestTimeoutPolicy {
        provider_timeout: Duration::from_secs(360),
        ..policy()
    };
    assert_eq!(exact.max_upstream_timeout(), Duration::from_secs(360));
    assert!(exact.validate().is_ok());
}
