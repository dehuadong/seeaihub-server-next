//! 夹具自身的检查：探活必须认得**我们起的那个** API 进程。
//!
//! 判据 [`probe_api`] 只存在于这个夹具里，没有别处够得着，所以这几条与夹具同处；它们不启平台
//! 进程、不用数据库，因此不进 `#[ignore]`，由 `cargo test --workspace` 那一步跑。

use super::*;

/// 一个最小的"平台 API"替身：`/health` 回那一份响应体，其余路径按 `admin_ok` 回 200 或 403。
///
/// 真夹具做不了这件事——假上游对任何 GET 都回图片，验不了"令牌认不认"。返回它的 `base_url`。
async fn fake_platform_api(admin_ok: bool) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("probe listener binds");
    let port = listener.local_addr().expect("probe address").port();
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            // 读到请求头结束为止：只读一次会在请求被分包时判错。
            let mut request = Vec::new();
            let mut chunk = [0_u8; 512];
            while !request.windows(4).any(|w| w == b"\r\n\r\n".as_slice()) {
                match socket.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) => request.extend_from_slice(&chunk[..read]),
                }
            }
            let head = String::from_utf8_lossy(&request);
            let _ = if head.starts_with("GET /health") {
                write_response(
                    &mut socket,
                    200,
                    "OK",
                    "application/json",
                    br#"{"status":"ok"}"#,
                )
                .await
            } else if admin_ok {
                write_response(&mut socket, 200, "OK", "application/json", b"{}").await
            } else {
                write_response(&mut socket, 403, "Forbidden", "application/json", b"{}").await
            };
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// 假上游的闸门必须**先记到达、再停住**，放行后才继续。
///
/// 强杀矩阵的判据全靠这条顺序：用例先看到到达信号，才敢说"API 到了这一格、杀进程时状态是确定的"。
/// 这里不启平台进程、不用数据库，所以不进 `#[ignore]`。
#[tokio::test]
async fn the_upstream_gate_signals_arrival_before_releasing() {
    let gate = Arc::new(UpstreamGate::default());
    assert_eq!(gate.arrivals(), 0);
    let held = {
        let gate = gate.clone();
        tokio::spawn(async move {
            // 假上游收到命中请求时做的两件事：记到达、停住。
            let ordinal = gate.arrivals.fetch_add(1, Ordering::SeqCst) + 1;
            gate.arrival_notify.notify_waiters();
            gate.arrive_and_hold(ordinal).await;
        })
    };
    gate.wait_for_arrival(1).await;
    assert_eq!(gate.arrivals(), 1, "the arrival is visible before release");
    assert!(
        !held.is_finished(),
        "the gate must still be holding the request"
    );
    gate.release_all();
    gate.wait_for_resume(1).await;
    held.await.expect("the held request finishes after release");
}

/// 对任何 GET 都回 200 的应答者不是平台 API：假上游就是这样（它给参考图取字节的那条兜底）。
#[tokio::test]
async fn probe_rejects_a_blanket_200_responder() {
    let upstream = start_fake_upstream_with(
        Arc::new(Mutex::new(Vec::new())),
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
    )
    .await;
    let client = Client::new();
    assert!(
        probe_api(&client, &upstream.base_url, "any-admin-token")
            .await
            .is_err(),
        "a responder that answers 200 to any GET must not pass as the platform API"
    );
}

/// 回得出 `/health`、却不认这份令牌的应答者是**别人**：另一条用例的 API 进程就是这样。
#[tokio::test]
async fn probe_rejects_an_api_that_is_not_ours() {
    let base_url = fake_platform_api(false).await;
    let client = Client::new();
    assert!(
        probe_api(&client, &base_url, "this-test-admin-token")
            .await
            .is_err(),
        "an API that rejects this test's admin token is not the one we started"
    );
}

/// 无缓存用例必须**显式**把 `REDIS_URL` 置空：API 与 Worker 进程用 `dotenvy` 加载仓库
/// `.env`，它不覆盖已设置的变量；只"不设"时本机那份 `.env` 的 `REDIS_URL` 会替用例补上一个
/// 真实 Redis，跑的不是 CI（无 `.env`）那条路径。空值在 `RedisCache::from_env` 里判为未配置。
#[test]
fn no_cache_is_an_explicitly_empty_redis_url() {
    let mut command = Command::new("unused");
    apply_cache_env(&mut command, None);
    let redis_url = command
        .get_envs()
        .find(|(name, _)| *name == std::ffi::OsStr::new("REDIS_URL"))
        .and_then(|(_, value)| value);
    assert_eq!(
        redis_url,
        Some(std::ffi::OsStr::new("")),
        "a no-cache case must set REDIS_URL to the empty value, not leave it unset"
    );
}

/// 反过来钉住判据本身：`/health` 与令牌都对就要被认下来。
///
/// 这里回的 `{"status":"ok"}` 是**生产 `/health` 的线协议字面量**（`apps/api/src/main.rs` 的
/// `health`）——判据要钉的正是它。
#[tokio::test]
async fn probe_accepts_an_api_that_answers_our_token() {
    let base_url = fake_platform_api(true).await;
    let client = Client::new();
    assert!(
        probe_api(&client, &base_url, "this-test-admin-token")
            .await
            .is_ok(),
        "an API that answers the health payload and this test's token must be accepted"
    );
}
