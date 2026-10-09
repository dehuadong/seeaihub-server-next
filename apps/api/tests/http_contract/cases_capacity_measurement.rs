//! 受理路径的容量测量：先定饱和资源，再定缓存。
//!
//! 它把「并发推上去之后先饱和的是哪个资源」打出来，观测与结论记在
//! `docs/verification/admission-path-capacity.md`。用例**不写阈值**，也不承诺提升百分比：阈值由
//! 部署目标定。
//!
//! 两件事必须在测量里抬开，否则量到的是闸门而不是执行路径：本机执行名额（缺省 4）与渠道全局
//! 名额（缺省 32）。抬开之后剩下的候选瓶颈是 PostgreSQL 连接池、PostgreSQL 本身与本机 CPU。
//!
//! 需要真实 PostgreSQL（夹具派生一次性库并跑迁移），因此是 ignored。

use super::*;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// 每档的样本数：与留档的网关耗时拆分同口径，够算 p50/p95/p99。
const SAMPLES: usize = 120;
/// 抬开本机执行名额用的内存预算：装得下并发 32（每次执行预留约 437 MiB）。
const MEASURE_MEMORY_BYTES: usize = 16 * 1024 * 1024 * 1024;
/// 抬开渠道全局名额：缺省 32 会在并发 32 那档正好卡住。
const MEASURE_CHANNEL_IN_FLIGHT: u64 = 256;
/// 每模型每账户的并发名额：测量要的是**执行路径**的瓶颈，不是这道闸门。
const MEASURE_MODEL_QUOTA: u64 = 64;
/// 测量账户的余额（CNY 微单位）：一轮扫描最多约 2500 次受理，每次保底额不到 0.1 元。
const MEASURE_ACCOUNT_CREDIT_MICROUSD: u64 = 1_000_000_000;
/// 后端数采样间隔：不空闲后端数按它累加，乘出来就是这一档的连接占用时间。
const SAMPLE_INTERVAL: Duration = Duration::from_millis(50);

/// 一档负载的观测。延迟分位只统计**成功**请求。
struct Cell {
    delay_ms: u64,
    concurrency: usize,
    pool: u32,
    accounts: usize,
    total: Duration,
    mean: Duration,
    p50: Duration,
    p95: Duration,
    p99: Duration,
    throughput: f64,
    failures: BTreeMap<u16, usize>,
    backends_idle: i64,
    backends_peak: i64,
    commits: i64,
    cpu_busy_permille: Option<u64>,
    read_load: usize,
    reads_done: usize,
    /// 每请求的连接占用时间（后端秒 ÷ 请求数）：不空闲后端数按采样间隔累加出来的。
    connection_seconds_per_request: f64,
    /// 这一档里每张表被访问的次数，摊到每次请求上（降序，只留非零项）。
    table_accesses: Vec<(String, f64)>,
}

impl Cell {
    fn summary(&self) -> String {
        let failures = if self.failures.is_empty() {
            "none".to_owned()
        } else {
            self.failures
                .iter()
                .map(|(status, count)| format!("{status}x{count}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        let cpu = match self.cpu_busy_permille {
            Some(value) => format!("{}.{}%", value / 10, value % 10),
            None => "n/a".to_owned(),
        };
        format!(
            "delay={:>5}ms pool={:>2} accounts={:>2} concurrency={:>2} read_load={:>2} | \
             total={:>6.2}s throughput={:>6.2}/s mean={:>6.1}ms p50={:>6.1}ms p95={:>6.1}ms \
             p99={:>6.1}ms | backends={}/{} conn={:>5.1}ms commits={:>6} reads={:>5} cpu={} | \
             failures={}",
            self.delay_ms,
            self.pool,
            self.accounts,
            self.concurrency,
            self.read_load,
            self.total.as_secs_f64(),
            self.throughput,
            self.mean.as_secs_f64() * 1_000.0,
            self.p50.as_secs_f64() * 1_000.0,
            self.p95.as_secs_f64() * 1_000.0,
            self.p99.as_secs_f64() * 1_000.0,
            self.backends_idle,
            self.backends_peak,
            self.connection_seconds_per_request * 1_000.0,
            self.commits,
            self.reads_done,
            cpu,
            failures,
        )
    }

    /// 表访问那一行：`表名×每请求次数`，只列前六张。
    fn table_access_line(&self) -> String {
        self.table_accesses
            .iter()
            .take(6)
            .map(|(table, per_request)| format!("{table}x{per_request:.1}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// 跑一档：`concurrency` 个并发、`SAMPLES` 次尝试，返回这一档的观测。
///
/// 密钥按 `index` 轮流取：给多把密钥就放开了"同一账户"这一维，而渠道仍是同一条。
///
/// `read_load` 大于零时另起这么多路**非生成流量**（`GET /v1/models`，与生成共用同一个连接池），
/// 跑满这一档为止，用来回答"两类流量能不能在同一个池上互相排队"。
///
/// 采样与请求并发跑：后台每 50 毫秒读一次 `pg_stat_activity` 记后端数峰值，请求全部返回后停。
/// 峰值含夹具自己的池，所以同时报**空闲基线**，两者的差才是这次负载真的用掉了多少连接。
async fn measure_cell(
    harness: &Harness,
    api_keys: &[String],
    delay_ms: u64,
    concurrency: usize,
    pool: u32,
    read_load: usize,
) -> Cell {
    let backends_idle = case_backends(&harness.admin_pool, &harness.database_name).await;
    let commits_before = committed(&harness.admin_pool, &harness.database_name).await;
    let scans_before = table_scans(&harness.pool).await;
    let cpu_before = cpu_snapshot();

    let stop = Arc::new(AtomicBool::new(false));
    let peak = Arc::new(AtomicUsize::new(0));
    let busy_samples = Arc::new(AtomicUsize::new(0));
    let sampler = tokio::spawn(sample_backends(
        harness.admin_pool.clone(),
        harness.database_name.clone(),
        stop.clone(),
        peak.clone(),
        busy_samples.clone(),
    ));
    let reads_done = Arc::new(AtomicUsize::new(0));
    let readers = (read_load > 0).then(|| {
        spawn_read_load(
            harness.base_url.clone(),
            read_load,
            stop.clone(),
            reads_done.clone(),
        )
    });

    let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency));
    let started = std::time::Instant::now();
    let mut handles = Vec::with_capacity(SAMPLES);
    for index in 0..SAMPLES {
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("the load generator's semaphore stays open");
        let base_url = harness.base_url.clone();
        let api_key = api_keys[index % api_keys.len()].clone();
        let model = harness.model;
        handles.push(tokio::spawn(async move {
            let key = format!(
                "capacity-{delay_ms}-{concurrency}-{index}-{}",
                Uuid::new_v4()
            );
            let began = std::time::Instant::now();
            let (status, _body) = post_json(
                &base_url,
                &api_key,
                "/v1/images/generations",
                &key,
                &route_request(model, "measure the admission path"),
            )
            .await;
            let elapsed = began.elapsed();
            drop(permit);
            (status, elapsed)
        }));
    }

    let mut latencies = Vec::with_capacity(SAMPLES);
    let mut failures: BTreeMap<u16, usize> = BTreeMap::new();
    for handle in handles {
        let (status, elapsed) = handle.await.expect("a load task should not panic");
        if status == StatusCode::OK {
            latencies.push(elapsed);
        } else {
            *failures.entry(status.as_u16()).or_default() += 1;
        }
    }
    let total = started.elapsed();

    stop.store(true, Ordering::Relaxed);
    let _ = sampler.await;
    if let Some(readers) = readers {
        for reader in readers {
            let _ = reader.await;
        }
    }
    // 表扫描计数按约 1 秒的粒度成批可见：等一拍再读，否则最后一秒的访问会算到下一档头上。
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    let after_scans = table_scans(&harness.pool).await;
    let commits = committed(&harness.admin_pool, &harness.database_name).await - commits_before;
    let cpu_busy_permille = cpu_busy_permille(cpu_before, cpu_snapshot());

    latencies.sort();
    let count = u32::try_from(latencies.len()).unwrap_or(u32::MAX).max(1);
    let mean = latencies.iter().sum::<Duration>() / count;
    let table_accesses = table_access_deltas(&scans_before, &after_scans, count);
    Cell {
        delay_ms,
        concurrency,
        pool,
        accounts: api_keys.len(),
        total,
        mean,
        p50: percentile(&latencies, 0.50),
        p95: percentile(&latencies, 0.95),
        p99: percentile(&latencies, 0.99),
        throughput: latencies.len() as f64 / total.as_secs_f64(),
        failures,
        backends_idle,
        backends_peak: peak.load(Ordering::Relaxed) as i64,
        commits,
        cpu_busy_permille,
        read_load,
        reads_done: reads_done.load(Ordering::Relaxed),
        connection_seconds_per_request: busy_samples.load(Ordering::Relaxed) as f64
            * SAMPLE_INTERVAL.as_secs_f64()
            / f64::from(count),
        table_accesses,
    }
}

/// 这个一次性库每张用户表的扫描次数（顺序 + 索引）。
async fn table_scans(pool: &sqlx::PgPool) -> BTreeMap<String, i64> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT relname, seq_scan + idx_scan FROM pg_stat_user_tables ORDER BY relname",
    )
    .fetch_all(pool)
    .await
    .expect("read the table scan counters");
    rows.into_iter().collect()
}

/// 一档里每张表被访问了多少次，摊到**每次请求**上；只留非零项，按次数降序。
///
/// 这是「每请求 SQL 条数与分类」能拿到的最近似的一层：`pg_stat_statements` 在本机没加载，语句级
/// 计数缺席，表级扫描计数是公开可读的替代。
fn table_access_deltas(
    before: &BTreeMap<String, i64>,
    after: &BTreeMap<String, i64>,
    requests: u32,
) -> Vec<(String, f64)> {
    let mut deltas: Vec<(String, f64)> = after
        .iter()
        .filter_map(|(table, scans)| {
            let delta = scans - before.get(table).copied().unwrap_or(0);
            (delta > 0).then(|| (table.clone(), delta as f64 / f64::from(requests)))
        })
        .collect();
    deltas.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    deltas
}

/// 非生成流量：`read_load` 路并发反复读目录，直到 `stop` 置位。返回完成次数。
///
/// 读的是 `GET /v1/models`——公开面、不需要凭证、每次都读目录，与生成请求共用同一个连接池。
fn spawn_read_load(
    base_url: String,
    read_load: usize,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicUsize>,
) -> Vec<tokio::task::JoinHandle<()>> {
    (0..read_load)
        .map(|_| {
            let base_url = base_url.clone();
            let stop = stop.clone();
            let done = done.clone();
            tokio::spawn(async move {
                let client = Client::new();
                let url = format!("{base_url}/v1/models");
                while !stop.load(Ordering::Relaxed) {
                    if client.get(&url).send().await.is_ok() {
                        done.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
        })
        .collect()
}

/// 后台采样：每 50 毫秒记一次这个库的后端数峰值，直到 `stop` 置位。
async fn sample_backends(
    admin: sqlx::PgPool,
    database: String,
    stop: Arc<AtomicBool>,
    peak: Arc<AtomicUsize>,
    busy_samples: Arc<AtomicUsize>,
) {
    while !stop.load(Ordering::Relaxed) {
        let count = case_backends(&admin, &database).await;
        peak.fetch_max(usize::try_from(count).unwrap_or(0), Ordering::Relaxed);
        // 不空闲的后端数按采样累加：乘上采样间隔就是这一档的连接占用时间（后端秒）。
        let busy = case_busy_backends(&admin, &database).await;
        busy_samples.fetch_add(usize::try_from(busy).unwrap_or(0), Ordering::Relaxed);
        tokio::time::sleep(SAMPLE_INTERVAL).await;
    }
}

/// 这个一次性库上**不空闲**的后端连接数。
async fn case_busy_backends(admin: &sqlx::PgPool, database: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM pg_stat_activity WHERE datname = $1 AND state <> 'idle'",
    )
    .bind(database)
    .fetch_one(admin)
    .await
    .expect("read the busy backend count")
}

/// 这个一次性库上现在的后端连接数（含夹具自己的池）。
async fn case_backends(admin: &sqlx::PgPool, database: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname = $1")
        .bind(database)
        .fetch_one(admin)
        .await
        .expect("read the backend count")
}

/// 这个一次性库已提交的事务数。
async fn committed(admin: &sqlx::PgPool, database: &str) -> i64 {
    sqlx::query_scalar("SELECT xact_commit FROM pg_stat_database WHERE datname = $1")
        .bind(database)
        .fetch_one(admin)
        .await
        .expect("read the database transaction counter")
}

/// 整机 CPU 的（总、空闲）累计节拍。读不到 `/proc/stat` 时返回 `None`。
fn cpu_snapshot() -> Option<(u64, u64)> {
    let text = std::fs::read_to_string("/proc/stat").ok()?;
    let values: Vec<u64> = text
        .lines()
        .next()?
        .split_whitespace()
        .skip(1)
        .filter_map(|value| value.parse().ok())
        .collect();
    if values.len() < 5 {
        return None;
    }
    Some((values.iter().sum(), values[3] + values[4]))
}

/// 两个快照之间的整机忙碌率（千分比）。
fn cpu_busy_permille(before: Option<(u64, u64)>, after: Option<(u64, u64)>) -> Option<u64> {
    let (total_before, idle_before) = before?;
    let (total_after, idle_after) = after?;
    let total = total_after.checked_sub(total_before)?;
    let idle = idle_after.checked_sub(idle_before)?;
    if total == 0 {
        return None;
    }
    Some((total - idle).saturating_mul(1000) / total)
}

/// 起一台直接执行夹具：抬开执行与渠道名额，池按参数给；另开 `accounts` 个**大余额**账户并返回
/// 它们的密钥。
///
/// 不能用夹具自己那把密钥：它绑的账户余额不够几百次受理，量到的是 402 而不是容量。
async fn start(delay_ms: u64, pool: u32, accounts: usize) -> (Harness, Vec<String>) {
    let harness = Harness::start_direct_with(
        candidate(
            "AIHubMix",
            "aihubmix-image-v1",
            &["prompt_only", "image_conditioned", "masked"],
        ),
        UpstreamBehaviour {
            delay_ms,
            ..UpstreamBehaviour::aihubmix(SyncImageShape::Url)
        },
        MEASURE_MODEL_QUOTA,
        30,
        ApiProcessSettings {
            max_memory_bytes: Some(MEASURE_MEMORY_BYTES),
            channel_max_in_flight: Some(MEASURE_CHANNEL_IN_FLIGHT),
            database_max_connections: Some(pool),
            ..ApiProcessSettings::default()
        },
    )
    .await;
    let client = Client::new();
    let mut keys = Vec::with_capacity(accounts);
    for _ in 0..accounts {
        let (_, api_key) = funded_account(
            &client,
            &harness.base_url,
            &harness.admin_token,
            MEASURE_ACCOUNT_CREDIT_MICROUSD,
        )
        .await;
        keys.push(api_key);
    }
    (harness, keys)
}

/// 先走通一次：选路与配置读取是首请求冷、后续热，热了的请求才各档可比。
async fn warm_up(harness: &Harness, api_key: &str) {
    let key = format!("capacity-warmup-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        api_key,
        "/v1/images/generations",
        &key,
        &route_request(harness.model, "warm the admission path"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "预热失败: {body}");
}

/// 一组参数下扫一遍并发。`read_load` 大于零时每档都另压同样多路非生成读。
async fn sweep(
    delay_ms: u64,
    pool: u32,
    accounts: usize,
    concurrencies: &[usize],
    read_load: usize,
) {
    let (harness, keys) = start(delay_ms, pool, accounts).await;
    warm_up(&harness, &keys[0]).await;
    for &concurrency in concurrencies {
        let cell = measure_cell(&harness, &keys, delay_ms, concurrency, pool, read_load).await;
        println!("{}", cell.summary());
        println!("  table accesses per request: {}", cell.table_access_line());
    }
    harness.cleanup().await;
}

/// R0：上游延迟、连接池、账户数三轴下扫并发，找出先饱和的资源；另压一组非生成流量。
///
/// 三轴都要跑：只扫并发看不出「池是不是限住了」，只改池看不出「改池有没有用」，只改账户数分不开
/// 「同一账户的行锁」与「同一渠道的咨询锁」——夹具只有一条渠道，多账户只放掉前者。
///
/// 上游延迟取 0 与 200 毫秒：0 毫秒把网关自身暴露成瓶颈，200 毫秒与既有留档可比。秒级延迟下上游
/// 占绝对大头，各档都落在执行名额之内，扫不出拐点，所以不进这一轮。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_admission_path_saturation_sweep() {
    for delay_ms in [0_u64, 200] {
        for pool in [10_u32, 64] {
            sweep(delay_ms, pool, 1, &[1, 4, 8, 16, 32], 0).await;
        }
    }
    // 账户这一轴只在暴露瓶颈的档位上跑：0 毫秒延迟、缺省池。
    sweep(0, 10, 8, &[8, 16, 32], 0).await;
    // 非生成流量这一轴：同一档各跑一次，只差后台压不压目录读。
    sweep(0, 10, 1, &[8], 0).await;
    sweep(0, 10, 1, &[8], 8).await;
}
