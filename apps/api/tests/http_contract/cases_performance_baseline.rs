//! A8 单态性能基线：唯一执行路径（API 直接执行）的有界吞吐与延迟观测。
//!
//! 这不是性能断言：跑同一份假上游、同一并发与请求数，只把本机这一次的观测打出来，供
//! docs/verification/synchronous-gateway.md 的验收记录引用。性能阈值由整改前基线与部署目标
//! 决定，这里不虚构提升百分比。
//!
//! 需要真实 PostgreSQL（夹具派生一次性库并跑迁移），因此是 ignored。

use super::*;

/// 排序后的样本在 percentile（0–1）处的取值：取向上取整那一档（基线与耗时拆分同一口径）。
fn percentile(sorted: &[Duration], percentile: f64) -> Duration {
    let index = (sorted.len() as f64 * percentile).ceil() as usize - 1;
    sorted[index.min(sorted.len() - 1)]
}

/// 一组样本的均值与 p95。
fn mean_and_p95(mut samples: Vec<Duration>) -> (Duration, Duration) {
    samples.sort();
    let count = u32::try_from(samples.len()).expect("the sample count fits a u32");
    let mean = samples.iter().sum::<Duration>() / count;
    (mean, percentile(&samples, 0.95))
}

/// 一次有界负载的观测结果。
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

/// A8：同一棵树、同一台机器上量直接执行那一条路的吞吐，记总耗时、平均与 p95 延迟、每秒完成数。
///
/// 图片入口只有直接执行这一条：夹具不起 Worker，API 进程内直连假上游。先预热一次，让首请求冷的
/// 选路与余额缓存转热，测量才可比。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn direct_execution_throughput_baseline() {
    const CONCURRENCY: usize = 8;
    const REQUESTS: usize = 48;
    // 假上游收到生成请求后固定压这么久：本机观测里的差就来自网关与执行路径，不来自上游耗时。
    const UPSTREAM_DELAY_MS: u64 = 200;

    let baseline = measure_baseline("direct", CONCURRENCY, REQUESTS, UPSTREAM_DELAY_MS).await;
    println!("{}", baseline.summary());
}

/// 跑一轮并返回观测。
async fn measure_baseline(
    label: &'static str,
    concurrency: usize,
    requests: usize,
    upstream_delay_ms: u64,
) -> BaselineObservation {
    // 取同一组窗口与上游超时。30s 足够 8 并发的 48 个请求跑完。
    const SYNC_WAIT_SECONDS: u64 = 30;
    // 夹具的并发名额缺省必须大于并发，否则测量被容量闸门排队，测到的是闸门而不是执行路径。
    const MAX_CONCURRENT_JOBS_DEFAULT: u64 = 64;
    // 渠道全局名额同理：直接执行在整个上游调用期间占着它。
    const CHANNEL_IN_FLIGHT: u64 = 128;
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
    // 并发测量要同时占住那么多渠道名额：默认的渠道全局上限会先把请求挡成 503，
    // 量到的就不是执行路径了。
    let harness = Harness::start_direct_with(
        draft,
        behaviour,
        MAX_CONCURRENT_JOBS_DEFAULT,
        SYNC_WAIT_SECONDS,
        ApiProcessSettings {
            channel_max_in_flight: Some(CHANNEL_IN_FLIGHT),
            // 在飞执行的字节预算是**按 Driver 声明的上限**预留的（AIHubMix 一次约 912MiB）：
            // 默认 2GiB 只够两次并发，量到的会是容量闸门而不是执行路径。这里按并发数抬开。
            max_memory_bytes: Some(8 * 1024 * 1024 * 1024),
            ..ApiProcessSettings::default()
        },
    )
    .await;
    let (_, api_key) = funded_account(
        &Client::new(),
        &harness.base_url,
        &harness.admin_token,
        MEASURE_ACCOUNT_CREDIT_MICROUSD,
    )
    .await;
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

/// 一组耗时样本的均值与 p50/p95/p99。
struct LatencySummary {
    mean: Duration,
    p50: Duration,
    p95: Duration,
    p99: Duration,
}

impl LatencySummary {
    /// 排序后按时延分位取值；样本为空会让上游先断言失败，这里只接非空样本。
    fn of(mut samples: Vec<Duration>) -> Self {
        samples.sort();
        let count = u32::try_from(samples.len()).expect("the sample count fits a u32");
        Self {
            mean: samples.iter().sum::<Duration>() / count,
            p50: percentile(&samples, 0.50),
            p95: percentile(&samples, 0.95),
            p99: percentile(&samples, 0.99),
        }
    }

    fn render(&self) -> String {
        format!(
            "mean={:.1}ms p50={:.1}ms p95={:.1}ms p99={:.1}ms",
            self.mean.as_secs_f64() * 1_000.0,
            self.p50.as_secs_f64() * 1_000.0,
            self.p95.as_secs_f64() * 1_000.0,
            self.p99.as_secs_f64() * 1_000.0,
        )
    }
}

/// 顺序直接执行下的一次观测：每次总耗时，以及扣掉固定上游延迟后的网关净耗时。
///
/// 「净耗时」= 总耗时 − D，含受理事务与写回、选路、参数映射与序列化、连接获取、响应序列化，
/// 以及顺序循环里的调度与排队；D 是假上游对每次生成请求固定压的那段。
struct GatewayOverheadObservation {
    requests: usize,
    upstream_delay: Duration,
    total: LatencySummary,
    net: LatencySummary,
}

impl GatewayOverheadObservation {
    fn summary(&self) -> String {
        format!(
            "sequential-direct: n={} upstream_delay={}ms total[{}] net[{}]",
            self.requests,
            self.upstream_delay.as_millis(),
            self.total.render(),
            self.net.render(),
        )
    }
}

/// A8：关闭并发（一次只发一个），顺序发 [`REQUESTS`] 个成功的直接执行请求，假上游固定延迟 D。
///
/// 逐个记录墙钟总耗时，再报告总耗时与「网关净耗时 = 总耗时 − D」的均值与 p50/p95/p99。净耗时里
/// 混着受理与结算的数据库往返、参数映射与序列化、连接获取、响应序列化，以及顺序循环自身的调度
/// 与排队——上游那一段 D 被减掉，剩下的就是这个网关与执行路径给一次请求加的时间。只记录，不断言。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn direct_execution_gateway_overhead_split() {
    // 样本数按 p99 留出余量：n=120 时 p99 取第 119 个样本，不再是最大值。
    const REQUESTS: usize = 120;
    const UPSTREAM_DELAY_MS: u64 = 200;
    // 与基线同一组窗口、上游超时与并发名额缺省，只有"顺序发"这一点不同。
    const SYNC_WAIT_SECONDS: u64 = 30;
    const MAX_CONCURRENT_JOBS_DEFAULT: u64 = 64;
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
        Harness::start_direct(draft, behaviour, MAX_CONCURRENT_JOBS_DEFAULT, SYNC_WAIT_SECONDS).await;
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
    let net: Vec<Duration> = totals
        .iter()
        .map(|total| total.saturating_sub(upstream_delay))
        .collect();
    let observation = GatewayOverheadObservation {
        requests: REQUESTS,
        upstream_delay,
        total: LatencySummary::of(totals),
        net: LatencySummary::of(net),
    };
    println!("{}", observation.summary());
}

/// 当前集群范围的 WAL 插入位置（文本形式，供 `pg_wal_lsn_diff` 再解析）。
async fn wal_insert_lsn(harness: &Harness) -> String {
    sqlx::query_scalar("SELECT pg_current_wal_insert_lsn()::text")
        .fetch_one(&harness.pool)
        .await
        .expect("the current WAL insert position")
}

/// 从 `start` 到现在的 WAL 插入字节数。
///
/// `pg_current_wal_insert_lsn()` 是**集群范围**的插入位置，所以差值含这段时间里同集群其他后端
/// （其他库、autovacuum、checkpoint）写的 WAL；它是本机单次观测的一部分，不当作请求的净写入。
async fn wal_bytes_since(harness: &Harness, start: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT pg_wal_lsn_diff(pg_current_wal_insert_lsn(), $1::text::pg_lsn)::bigint",
    )
    .bind(start)
    .fetch_one(&harness.pool)
    .await
    .expect("the WAL bytes written since the given position")
}

/// 发一次成功的直接执行请求，返回它前后测到的 WAL 字节增量。
async fn wal_bytes_of_one_request(harness: &Harness, api_key: &str, label: &str) -> i64 {
    let start = wal_insert_lsn(harness).await;
    let (status, body) = post_json(
        &harness.base_url,
        api_key,
        "/v1/images/generations",
        &format!("wal-{label}-{}", Uuid::new_v4()),
        &route_request(harness.model, label),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "WAL 测量请求失败: {body}");
    wal_bytes_since(harness, &start).await
}

/// A8：一次成功直接执行请求前后读 `pg_current_wal_insert_lsn()`，报告请求窗口里的 WAL 字节。
///
/// 先测一次请求的差值，再连测 [`BATCH`] 次取平均以弱化单窗口噪声。差值口径是集群范围的插入位置，
/// 不是这条请求独占的写入；本机单次观测，只作记录，不设阈值。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn direct_execution_wal_bytes_per_request() {
    const BATCH: usize = 5;
    // 与顺序耗时拆分同一组窗口与并发名额缺省，只有测量对象换成 WAL 字节。
    const SYNC_WAIT_SECONDS: u64 = 30;
    const MAX_CONCURRENT_JOBS_DEFAULT: u64 = 64;
    const MEASURE_ACCOUNT_CREDIT_MICROUSD: u64 = 1_000_000_000;

    let draft = candidate(
        "AIHubMix",
        "aihubmix-image-v1",
        &["prompt_only", "image_conditioned", "masked"],
    );
    let harness = Harness::start_direct(
        draft,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        MAX_CONCURRENT_JOBS_DEFAULT,
        SYNC_WAIT_SECONDS,
    )
    .await;
    let (_, api_key) = funded_account(
        &Client::new(),
        &harness.base_url,
        &harness.admin_token,
        MEASURE_ACCOUNT_CREDIT_MICROUSD,
    )
    .await;

    // 预热一次，让受理事务的首次页面/索引分配不混进被测窗口。
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &format!("wal-warmup-{}", Uuid::new_v4()),
        &route_request(harness.model, "warm the route and balance caches"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "预热失败: {body}");

    let single = wal_bytes_of_one_request(&harness, &api_key, "single").await;
    let mut batch_total: i64 = 0;
    for index in 0..BATCH {
        batch_total +=
            wal_bytes_of_one_request(&harness, &api_key, &format!("batch-{index}")).await;
    }
    println!(
        "direct-execution-wal: single={single}B batch_n={BATCH} batch_total={batch_total}B batch_mean={:.0}B/request",
        batch_total as f64 / BATCH as f64,
    );
    harness.cleanup().await;
}
