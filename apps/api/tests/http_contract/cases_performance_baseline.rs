//! A8 两态性能基线：直接执行关（旧 Worker 路径）与开（同步直接执行）的有界吞吐与延迟观测。
//!
//! 这不是性能断言：两态跑同一份假上游、同一并发与请求数，只把本机这一次的观测打出来，供
//! docs/verification/synchronous-gateway.md 的验收记录引用。性能阈值由整改前基线与部署目标
//! 决定，这里不虚构提升百分比。
//!
//! 需要真实 PostgreSQL（夹具派生一次性库并跑迁移），因此是 ignored。

use super::*;

/// 一组样本的均值与 p95：p95 取向上取整那一档（两态基线与耗时拆分同一口径）。
fn mean_and_p95(mut samples: Vec<Duration>) -> (Duration, Duration) {
    samples.sort();
    let count = u32::try_from(samples.len()).expect("the sample count fits a u32");
    let mean = samples.iter().sum::<Duration>() / count;
    let p95_index = (samples.len() as f64 * 0.95).ceil() as usize - 1;
    (mean, samples[p95_index.min(samples.len() - 1)])
}

/// 两态各自一次有界负载的观测结果。
///
/// `total` 是从第一个请求发出到最后一个成功请求返回的墙钟时间；延迟分布只统计成功请求——
/// 任何非 200 都会让用例直接断言失败，不会混进统计。
struct BaselineObservation {
    label: &'static str,
    total: Duration,
    mean: Duration,
    p95: Duration,
    requests_per_second: f64,
}

impl BaselineObservation {
    fn summary(&self) -> String {
        format!(
            "{}: total={:.3}s mean={:.1}ms p95={:.1}ms throughput={:.2} req/s",
            self.label,
            self.total.as_secs_f64(),
            self.mean.as_secs_f64() * 1_000.0,
            self.p95.as_secs_f64() * 1_000.0,
            self.requests_per_second,
        )
    }
}

/// A8：同一棵树、同一台机器上先后跑直接执行关与开两态，各记总耗时、平均与 p95 延迟、每秒完成数。
///
/// 关态是整改前的旧 Worker 路径：夹具起**一个**真实 Worker 进程在整个测量期间领任务，API 侧
/// 同步入口等结果；开态不启 Worker，由 API 进程内直连假上游。两态都先预热一次，让首请求冷的
/// 选路与余额缓存转热，两态才可比。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn direct_execution_two_state_throughput_baseline() {
    const CONCURRENCY: usize = 8;
    const REQUESTS: usize = 48;
    // 假上游收到生成请求后固定压这么久，两态共用：本机观测里两态的差就来自网关与执行路径，
    // 不来自上游耗时。
    const UPSTREAM_DELAY_MS: u64 = 200;

    let off = measure_baseline(
        "direct-off",
        false,
        CONCURRENCY,
        REQUESTS,
        UPSTREAM_DELAY_MS,
    )
    .await;
    println!("{}", off.summary());
    let on = measure_baseline("direct-on", true, CONCURRENCY, REQUESTS, UPSTREAM_DELAY_MS).await;
    println!("{}", on.summary());
}

/// 跑一态并返回观测：`direct` 为 true 时开直接执行且不启 Worker，为 false 时起一个真实 Worker
/// 走旧路径。
async fn measure_baseline(
    label: &'static str,
    direct: bool,
    concurrency: usize,
    requests: usize,
    upstream_delay_ms: u64,
) -> BaselineObservation {
    // 两态取同一组窗口与上游超时，只有执行路径不同。30s 足够 8 并发的 48 个请求跑完。
    const SYNC_WAIT_SECONDS: u64 = 30;
    // 夹具的账户在飞上限必须大于并发，否则两态都被容量闸门排队，测到的是闸门而不是执行路径。
    const MAX_ACCOUNT_IN_FLIGHT: u64 = 64;
    // 测量用一份单独的大余额账户，不依赖夹具发布账户的余额是否够 48 次扣费。
    const MEASURE_ACCOUNT_CREDIT_MICROUSD: u64 = 1_000_000_000;

    let draft = candidate(
        "AIHubMix",
        "aihubmix-image-v1",
        &["prompt_only", "image_conditioned", "masked"],
    );
    let behaviour = UpstreamBehaviour {
        delay_ms: upstream_delay_ms,
        ..UpstreamBehaviour::aihubmix(SyncImageShape::Url)
    };
    let harness = if direct {
        Harness::start_direct(draft, behaviour, MAX_ACCOUNT_IN_FLIGHT, SYNC_WAIT_SECONDS).await
    } else {
        Harness::start_with_draft(draft, None, behaviour, MAX_ACCOUNT_IN_FLIGHT).await
    };
    let (_, api_key) = funded_account(
        &Client::new(),
        &harness.base_url,
        &harness.admin_token,
        MEASURE_ACCOUNT_CREDIT_MICROUSD,
    )
    .await;
    // 旧路径必须有 Worker 领任务；直接执行不启 Worker，这正是要对比的那一点。一个 Worker 覆盖
    // 整段测量，避免每个请求各起一个进程把进程启动成本算进延迟。
    let worker = (!direct).then(|| harness.spawn_worker());

    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &format!("baseline-warmup-{}", Uuid::new_v4()),
        &route_request(harness.model, "warm the route and balance caches"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{label} 预热失败: {body}");

    let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency));
    let mut handles = Vec::with_capacity(requests);
    let started = std::time::Instant::now();
    for index in 0..requests {
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("the measurement semaphore stays open");
        let base_url = harness.base_url.clone();
        let api_key = api_key.clone();
        let model = harness.model;
        let key = format!("baseline-{label}-{index}-{}", Uuid::new_v4());
        let prompt = format!("baseline request {index}");
        handles.push(tokio::spawn(async move {
            let began = std::time::Instant::now();
            let (status, body) = post_json(
                &base_url,
                &api_key,
                "/v1/images/generations",
                &key,
                &route_request(model, &prompt),
            )
            .await;
            let latency = began.elapsed();
            drop(permit);
            (status, body, latency)
        }));
    }

    let mut latencies = Vec::with_capacity(requests);
    for handle in handles {
        let (status, body, latency) = handle.await.expect("a baseline request joins");
        assert_eq!(status, StatusCode::OK, "{label} 请求失败: {body}");
        latencies.push(latency);
    }
    let total = started.elapsed();
    drop(worker);
    harness.cleanup().await;

    let (mean, p95) = mean_and_p95(latencies);
    BaselineObservation {
        label,
        total,
        mean,
        p95,
        requests_per_second: requests as f64 / total.as_secs_f64(),
    }
}

/// 顺序直接执行下的一次观测：每次总耗时，以及扣掉固定上游延迟后的网关净耗时。
///
/// 「净耗时」= 总耗时 − D，含受理事务与写回、选路、参数映射与序列化、连接获取、响应序列化，
/// 以及顺序循环里的调度与排队；D 是假上游对每次生成请求固定压的那段。
struct GatewayOverheadObservation {
    requests: usize,
    upstream_delay: Duration,
    total_mean: Duration,
    total_p95: Duration,
    net_mean: Duration,
    net_p95: Duration,
}

impl GatewayOverheadObservation {
    fn summary(&self) -> String {
        format!(
            "sequential-direct: n={} upstream_delay={}ms total_mean={:.1}ms total_p95={:.1}ms net_mean={:.1}ms net_p95={:.1}ms",
            self.requests,
            self.upstream_delay.as_millis(),
            self.total_mean.as_secs_f64() * 1_000.0,
            self.total_p95.as_secs_f64() * 1_000.0,
            self.net_mean.as_secs_f64() * 1_000.0,
            self.net_p95.as_secs_f64() * 1_000.0,
        )
    }
}

/// A8：关闭并发（一次只发一个），顺序发 [`REQUESTS`] 个成功的直接执行请求，假上游固定延迟 D。
///
/// 逐个记录墙钟总耗时，再报告总耗时与「网关净耗时 = 总耗时 − D」的均值与 p95。净耗时里混着
/// 受理与结算的数据库往返、参数映射与序列化、连接获取、响应序列化，以及顺序循环自身的调度与
/// 排队——上游那一段 D 被减掉，剩下的就是这个网关与执行路径给一次请求加的时间。只记录，不断言。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn direct_execution_gateway_overhead_split() {
    const REQUESTS: usize = 10;
    const UPSTREAM_DELAY_MS: u64 = 200;
    // 与两态基线同一组窗口、上游超时与账户在飞上限，只有"顺序发"这一点不同。
    const SYNC_WAIT_SECONDS: u64 = 30;
    const MAX_ACCOUNT_IN_FLIGHT: u64 = 64;
    const MEASURE_ACCOUNT_CREDIT_MICROUSD: u64 = 1_000_000_000;

    let draft = candidate(
        "AIHubMix",
        "aihubmix-image-v1",
        &["prompt_only", "image_conditioned", "masked"],
    );
    let behaviour = UpstreamBehaviour {
        delay_ms: UPSTREAM_DELAY_MS,
        ..UpstreamBehaviour::aihubmix(SyncImageShape::Url)
    };
    let harness =
        Harness::start_direct(draft, behaviour, MAX_ACCOUNT_IN_FLIGHT, SYNC_WAIT_SECONDS).await;
    let (_, api_key) = funded_account(
        &Client::new(),
        &harness.base_url,
        &harness.admin_token,
        MEASURE_ACCOUNT_CREDIT_MICROUSD,
    )
    .await;

    // 预热一次：首请求冷的选路与余额缓存不进统计。
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &format!("overhead-warmup-{}", Uuid::new_v4()),
        &route_request(harness.model, "warm the route and balance caches"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "预热失败: {body}");

    let mut totals = Vec::with_capacity(REQUESTS);
    for index in 0..REQUESTS {
        let key = format!("overhead-{index}-{}", Uuid::new_v4());
        let prompt = format!("overhead request {index}");
        let began = std::time::Instant::now();
        let (status, body) = post_json(
            &harness.base_url,
            &api_key,
            "/v1/images/generations",
            &key,
            &route_request(harness.model, &prompt),
        )
        .await;
        let total = began.elapsed();
        assert_eq!(status, StatusCode::OK, "顺序请求 {index} 失败: {body}");
        println!(
            "request {index}: total={:.1}ms net={:.1}ms",
            total.as_secs_f64() * 1_000.0,
            total
                .saturating_sub(Duration::from_millis(UPSTREAM_DELAY_MS))
                .as_secs_f64()
                * 1_000.0,
        );
        totals.push(total);
    }
    harness.cleanup().await;

    let upstream_delay = Duration::from_millis(UPSTREAM_DELAY_MS);
    let (total_mean, total_p95) = mean_and_p95(totals.clone());
    let net: Vec<Duration> = totals
        .iter()
        .map(|total| total.saturating_sub(upstream_delay))
        .collect();
    let (net_mean, net_p95) = mean_and_p95(net);
    let observation = GatewayOverheadObservation {
        requests: REQUESTS,
        upstream_delay,
        total_mean,
        total_p95,
        net_mean,
        net_p95,
    };
    println!("{}", observation.summary());
}
