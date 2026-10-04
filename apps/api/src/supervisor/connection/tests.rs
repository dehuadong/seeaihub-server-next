//! 连接驱动的关键路径用例：HTTP/1 流水线缓冲下的 FIN、HTTP/2 窗口为 0 时的发送期限、收尾顺序。
//!
//! 这些用例起真实 socket 与真实 `serve`，不用数据库：连接层的行为只能在线路上验证。

use std::sync::atomic::{AtomicBool, Ordering};

use axum::{Router, body::Body, routing::get};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::oneshot;

use super::*;

/// 单次执行预留（与 `supervisor/tests.rs` 同值）。
const EXECUTION_MEMORY_BYTES: usize = 32 * 1024 * 1024;

fn test_supervisor(execution_slots: usize, send_slots: usize) -> Arc<super::super::Supervisor> {
    super::super::Supervisor::new(super::super::SupervisorConfig {
        execution_slots,
        max_memory_bytes: EXECUTION_MEMORY_BYTES * execution_slots,
        execution_memory_bytes: EXECUTION_MEMORY_BYTES,
        send_slots,
        read_slots: 4,
        slow_read_timeout: Duration::from_secs(5),
        shutdown_grace: Duration::from_secs(2),
        total_deadline: Duration::from_secs(30),
        finalization_grace: Duration::from_secs(1),
        send_window: Duration::from_secs(30),
        ownership: None,
    })
    .expect("the supervisor")
}

fn test_config() -> TransportConfig {
    TransportConfig {
        max_connections: 16,
        max_connection_tasks: 8,
        monitor: MonitorConfig {
            max_connections: 16,
            control_queue_capacity: 128,
            event_batch: 8,
        },
        registration_timeout: Duration::from_secs(2),
        release_timeout: Duration::from_secs(2),
        shutdown_grace: Duration::from_millis(500),
        http1_max_headers: 64,
        http1_max_buf_size: 64 * 1024,
        http2_max_concurrent_streams: 16,
        http2_max_send_buf_size: 1024 * 1024,
        http2_max_header_list_size: 64 * 1024,
    }
}

type Server = (
    std::net::SocketAddr,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
);

/// 起一个真实监听端口的连接驱动。
async fn start_server(router: Router) -> Server {
    let (server, _observability) = start_server_with(router, test_config()).await;
    server
}

/// 按给定容量起服务：观测计数随连接一起交回用例。
async fn start_server_with(
    router: Router,
    config: TransportConfig,
) -> (Server, Arc<TransportObservability>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a loopback port");
    let address = listener.local_addr().expect("the bound address");
    let observability = Arc::new(TransportObservability::default());
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let served = Arc::clone(&observability);
    let server = tokio::spawn(serve(listener, router, config, served, async move {
        let _ = stop_rx.await;
    }));
    ((address, stop_tx, server), observability)
}

async fn stop_server(
    stop: oneshot::Sender<()>,
    server: tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let _ = stop.send(());
    let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
}

/// 读到一个期限之前的所有字节；返回读到的内容与是否观察到连接结束。
async fn read_to_end_within(stream: &mut TcpStream, limit: Duration) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut buffer = [0u8; 8192];
    let deadline = Instant::now() + limit;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return (out, false);
        }
        match tokio::time::timeout(remaining, stream.read(&mut buffer)).await {
            Ok(Ok(0)) => return (out, true),
            Ok(Ok(read)) => out.extend_from_slice(&buffer[..read]),
            Ok(Err(_)) => return (out, true),
            Err(_) => return (out, false),
        }
    }
}

async fn wait_until(condition: impl Fn() -> bool, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    condition()
}

fn test_observability() -> Arc<TransportObservability> {
    Arc::new(TransportObservability::default())
}

/// 一个在 handler 被丢弃时置位的探针：证明连接销毁确实丢掉了在飞 service future。
#[derive(Clone)]
struct DropProbe(Arc<AtomicBool>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// HTTP/1 已经缓存了下一条流水线请求时，hyper 不会再去读 socket，FIN 因此观察不到；
/// 连接层必须靠自己注册的关闭事件把这条连接销毁。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http1_pipelined_bytes_still_observe_fin() {
    let dropped = Arc::new(AtomicBool::new(false));
    let probe = DropProbe(Arc::clone(&dropped));
    let router = Router::new().route(
        "/slow",
        get(move || {
            let probe = probe.clone();
            async move {
                let _probe = probe;
                tokio::time::sleep(Duration::from_secs(30)).await;
                "done"
            }
        }),
    );
    let (address, stop, server) = start_server(router).await;
    let mut client = TcpStream::connect(address).await.expect("connect");
    // 两条**完整**请求一次写出：第二条留在 hyper 的读缓冲里，正是 `mid_message_detect_eof`
    // 不再读 socket 的那个状态。随后只关写半边。
    client
        .write_all(
            b"GET /slow HTTP/1.1\r\nHost: test\r\n\r\nGET /slow HTTP/1.1\r\nHost: test\r\n\r\n",
        )
        .await
        .expect("write the pipelined requests");
    client.shutdown().await.expect("half-close the client");

    let started = Instant::now();
    let (_bytes, closed) = read_to_end_within(&mut client, Duration::from_secs(3)).await;
    assert!(
        closed,
        "the connection must be destroyed once the client's FIN is observed"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the FIN must be observed without waiting for the slow handler"
    );
    assert!(
        wait_until(|| dropped.load(Ordering::SeqCst), Duration::from_secs(2)).await,
        "the in-flight service future must be dropped with the connection"
    );
    stop_server(stop, server).await;
}

/// HTTP/2 流控窗口为 0：响应一个字节都发不出去、body 不再被读取，连接级最早发送期限仍须
/// 到期销毁整条连接并释放许可。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http2_zero_window_still_expires_and_releases_permits() {
    let supervisor = test_supervisor(1, 1);
    let handler_supervisor = Arc::clone(&supervisor);
    let router = Router::new().route(
        "/v1/images/generations",
        get(move || {
            let supervisor = Arc::clone(&handler_supervisor);
            async move {
                let lease = supervisor
                    .try_reserve_execution()
                    .expect("an execution lease for the test response");
                let send = supervisor
                    .try_reserve_send()
                    .expect("a send lease for the test response");
                let hold = SendHold::new(
                    Instant::now() + Duration::from_millis(400),
                    lease,
                    send,
                    256 * 1024,
                );
                let mut response = Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(vec![b'x'; 256 * 1024]))
                    .expect("the test response");
                response.extensions_mut().insert(hold);
                response
            }
        }),
    );
    let (address, stop, server) = start_server(router).await;

    let stream = TcpStream::connect(address).await.expect("connect");
    let (mut send_request, connection) = h2::client::Builder::new()
        .initial_window_size(0)
        .initial_connection_window_size(0)
        .handshake::<TcpStream, bytes::Bytes>(stream)
        .await
        .expect("the h2 handshake");
    let driver = tokio::spawn(async move {
        let _ = connection.await;
    });
    let request = http::Request::builder()
        .method("GET")
        .uri("http://test/v1/images/generations")
        .body(())
        .expect("the test request");
    let (response, _body) = send_request.send_request(request, true).expect("send");
    let response = response.await.expect("the response headers");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        supervisor.send_slots_available(),
        0,
        "the send permit is held by the connection registry"
    );
    assert_eq!(supervisor.execution_slots_available(), 0);

    // body 永远不会被读取（窗口为 0），期限仍然必须关闭整条连接。
    let closed = tokio::time::timeout(Duration::from_secs(5), driver).await;
    assert!(
        closed.is_ok(),
        "the earliest send deadline must destroy the whole h2 connection"
    );
    assert!(
        wait_until(
            || supervisor.send_slots_available() == 1
                && supervisor.execution_slots_available() == 1,
            Duration::from_secs(3)
        )
        .await,
        "the permits are released once the connection is destroyed"
    );
    stop_server(stop, server).await;
}

/// HTTP/1 图片响应设 `Connection: close`，写完即结束连接，许可随连接销毁释放。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http1_image_response_closes_the_connection_and_releases_after_it() {
    let supervisor = test_supervisor(1, 1);
    let handler_supervisor = Arc::clone(&supervisor);
    let router = Router::new().route(
        "/v1/images/generations",
        get(move || {
            let supervisor = Arc::clone(&handler_supervisor);
            async move {
                let lease = supervisor
                    .try_reserve_execution()
                    .expect("an execution lease for the test response");
                let send = supervisor
                    .try_reserve_send()
                    .expect("a send lease for the test response");
                let hold = SendHold::new(Instant::now() + Duration::from_secs(30), lease, send, 24);
                let mut response = Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{\"created\":1,\"data\":[]}"))
                    .expect("the test response");
                response.extensions_mut().insert(hold);
                response
            }
        }),
    );
    let (address, stop, server) = start_server(router).await;
    let mut client = TcpStream::connect(address).await.expect("connect");
    client
        .write_all(b"GET /v1/images/generations HTTP/1.1\r\nHost: test\r\n\r\n")
        .await
        .expect("write the request");
    let (bytes, closed) = read_to_end_within(&mut client, Duration::from_secs(3)).await;
    assert!(closed, "the connection must close after the response");
    let text = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
    assert!(
        text.contains("connection: close"),
        "an HTTP/1 image response must ask the client to close: {text}"
    );
    assert!(text.contains("\"created\""), "the payload is delivered");
    assert!(
        wait_until(
            || supervisor.send_slots_available() == 1
                && supervisor.execution_slots_available() == 1
                && supervisor.observability() == super::super::ExecutionBudgetSnapshot::default(),
            Duration::from_secs(3)
        )
        .await,
        "the permits and their observation counts are released after the connection is destroyed"
    );
    stop_server(stop, server).await;
}

/// 收尾顺序：abort 之后仍要等任务组 join 至空，许可才归零。
///
/// 任务组里的任务是同步阻塞的，`abort_all` 不能抢占它：abort 之后许可必须还在，join 完成后才释放。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn teardown_waits_for_the_task_group_before_releasing_permits() {
    let supervisor = test_supervisor(1, 1);
    let config = test_config();
    let monitor = DisconnectMonitor::start(config.monitor).expect("the monitor");
    let registry = Arc::new(Registry::new(
        Arc::clone(&monitor),
        config,
        test_observability(),
    ));
    let socket = std::os::unix::net::UnixStream::pair().expect("a socket pair");
    let fd = Arc::new(OwnedFd::from(socket.0));
    let owner = Arc::new(ConnectionOwner::new(
        7,
        fd,
        DisconnectSignal::new(),
        config.max_connection_tasks,
        test_observability(),
    ));
    let lease = supervisor
        .try_reserve_execution()
        .expect("an execution lease");
    let send = supervisor.try_reserve_send().expect("a send lease");
    owner.register_send(SendHold::new(
        Instant::now() + Duration::from_secs(30),
        lease,
        send,
        0,
    ));
    assert_eq!(supervisor.send_slots_available(), 0);
    assert!(owner.spawn_task(async {
        // 同步阻塞：abort 只能在这里跑完之后才被观察到。
        std::thread::sleep(Duration::from_millis(300));
    }));

    let task_registry = Arc::clone(&registry);
    let task_owner = Arc::clone(&owner);
    let teardown = tokio::spawn(async move {
        let mut connection: Option<std::future::Pending<ConnectionResult>> = None;
        finish_connection(&task_registry, &task_owner, &mut connection).await;
    });
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        supervisor.send_slots_available(),
        0,
        "abort alone must not release the permits; the task group has to drain first"
    );
    assert_eq!(supervisor.execution_slots_available(), 0);
    tokio::time::timeout(Duration::from_secs(3), teardown)
        .await
        .expect("teardown finishes")
        .expect("teardown does not panic");
    assert_eq!(supervisor.send_slots_available(), 1);
    assert_eq!(supervisor.execution_slots_available(), 1);
    assert_eq!(supervisor.memory_bytes_available(), EXECUTION_MEMORY_BYTES);
    monitor.shutdown();
}

/// 图片路由拒绝协议升级；CONNECT 一律拒绝。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn image_routes_reject_upgrades_and_connect_is_refused() {
    let router = Router::new().route("/v1/images/generations", get(|| async { "ok" }));
    let (address, stop, server) = start_server(router).await;

    let mut upgrade = TcpStream::connect(address).await.expect("connect");
    upgrade
        .write_all(
            b"GET /v1/images/generations HTTP/1.1\r\nHost: test\r\nConnection: upgrade\r\nUpgrade: websocket\r\n\r\n",
        )
        .await
        .expect("write the upgrade request");
    let (bytes, _closed) = read_to_end_within(&mut upgrade, Duration::from_secs(3)).await;
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.starts_with("HTTP/1.1 400") && text.contains("upgrade_not_supported"),
        "image routes must refuse upgrades: {text}"
    );

    let mut connect = TcpStream::connect(address).await.expect("connect");
    connect
        .write_all(b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com\r\n\r\n")
        .await
        .expect("write the CONNECT request");
    let (bytes, _closed) = read_to_end_within(&mut connect, Duration::from_secs(3)).await;
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.starts_with("HTTP/1.1 405") && text.contains("connect_not_supported"),
        "CONNECT must be refused: {text}"
    );
    stop_server(stop, server).await;
}

/// 连接级期限取所有在册响应的**最早**值，并且只前移不延长。
#[tokio::test]
async fn the_connection_takes_the_earliest_send_deadline_and_never_extends_it() {
    let supervisor = test_supervisor(4, 4);
    let config = test_config();
    let monitor = DisconnectMonitor::start(config.monitor).expect("the monitor");
    let socket = std::os::unix::net::UnixStream::pair().expect("a socket pair");
    let fd = Arc::new(OwnedFd::from(socket.0));
    let owner = ConnectionOwner::new(1, fd, DisconnectSignal::new(), 4, test_observability());

    let reserve = |supervisor: &Arc<super::super::Supervisor>, deadline| {
        let lease = supervisor
            .try_reserve_execution()
            .expect("an execution lease");
        let send = supervisor.try_reserve_send().expect("a send lease");
        SendHold::new(deadline, lease, send, 0)
    };
    let now = Instant::now();
    let late = now + Duration::from_secs(10);
    let early = now + Duration::from_secs(2);
    owner.register_send(reserve(&supervisor, late));
    assert_eq!(owner.effective_deadline(), Some(late));
    owner.register_send(reserve(&supervisor, early));
    assert_eq!(
        owner.effective_deadline(),
        Some(early),
        "a sooner deadline moves the connection deadline forward"
    );
    owner.register_send(reserve(&supervisor, now + Duration::from_secs(30)));
    assert_eq!(
        owner.effective_deadline(),
        Some(early),
        "a later deadline must never extend the connection deadline"
    );
    // 连接已经在收尾时新到的响应不再收编，许可当场释放。
    owner.release_holds();
    assert_eq!(supervisor.send_slots_available(), 4);
    owner.register_send(reserve(&supervisor, now + Duration::from_secs(1)));
    assert_eq!(supervisor.send_slots_available(), 4);
    monitor.shutdown();
}

/// 任务组满时拒绝新 task 并关闭整条连接，不排队；拒绝计入观测。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_task_group_refuses_and_closes_instead_of_queueing() {
    use hyper::rt::Executor as _;

    let config = test_config();
    let monitor = DisconnectMonitor::start(config.monitor).expect("the monitor");
    let socket = std::os::unix::net::UnixStream::pair().expect("a socket pair");
    let fd = Arc::new(OwnedFd::from(socket.0));
    let signal = DisconnectSignal::new();
    let observability = test_observability();
    let owner = Arc::new(ConnectionOwner::new(
        1,
        fd,
        Arc::clone(&signal),
        /* capacity */ 1,
        Arc::clone(&observability),
    ));
    assert!(owner.spawn_task(async {
        std::thread::sleep(Duration::from_millis(150));
    }));
    let exec = ConnectionExec {
        owner: Arc::clone(&owner),
    };
    exec.execute(async {});
    assert!(
        signal.is_cancelled(),
        "a refused task must close the connection instead of queueing"
    );
    assert_eq!(observability.snapshot().task_rejections, 1);
    monitor.shutdown();
}

/// 连接数上限的拒绝也计入观测：在册连接数与拒绝次数都与真实连接一致。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_connection_limit_refusal_is_counted() {
    let router = Router::new().route("/ping", get(|| async { "pong" }));
    let mut config = test_config();
    config.max_connections = 1;
    config.monitor.max_connections = 1;
    let ((address, stop, server), observability) = start_server_with(router, config).await;

    let mut first = TcpStream::connect(address).await.expect("connect");
    first
        .write_all(b"GET /ping HTTP/1.1\r\nHost: test\r\n\r\n")
        .await
        .expect("write the request");
    let (bytes, _closed) = read_to_end_within(&mut first, Duration::from_secs(2)).await;
    assert!(String::from_utf8_lossy(&bytes).contains("pong"));
    assert_eq!(
        observability.snapshot().connections,
        1,
        "the first connection is on the books"
    );

    // 第二条连接达到上限：直接关闭，不进队列。
    let mut second = TcpStream::connect(address).await.expect("connect");
    let (bytes, closed) = read_to_end_within(&mut second, Duration::from_secs(2)).await;
    assert!(
        closed && bytes.is_empty(),
        "a refused connection is closed without a response"
    );
    assert!(
        wait_until(
            || observability.snapshot().connection_rejections == 1,
            Duration::from_secs(2)
        )
        .await,
        "the refusal is counted"
    );
    assert_eq!(observability.snapshot().connections, 1);

    drop(first);
    drop(second);
    stop_server(stop, server).await;
    assert!(
        wait_until(
            || observability.snapshot().connections == 0,
            Duration::from_secs(2)
        )
        .await,
        "every connection owner had to subtract its own count"
    );
}

/// 停机：空闲的 keep-alive 连接必须被关闭，`serve` 在宽限期内返回。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_closes_idle_keep_alive_connections_within_the_grace() {
    let router = Router::new().route("/ping", get(|| async { "pong" }));
    let (address, stop, server) = start_server(router).await;
    let mut client = TcpStream::connect(address).await.expect("connect");
    client
        .write_all(b"GET /ping HTTP/1.1\r\nHost: test\r\n\r\n")
        .await
        .expect("write the request");
    let (bytes, closed) = read_to_end_within(&mut client, Duration::from_secs(2)).await;
    assert!(String::from_utf8_lossy(&bytes).contains("pong"));
    assert!(
        !closed,
        "a non-image route keeps the connection alive for pipelining"
    );

    let stop_at = Instant::now();
    let _ = stop.send(());
    let served = tokio::time::timeout(Duration::from_secs(5), server).await;
    assert!(served.is_ok(), "the accept loop must stop on shutdown");
    assert!(
        stop_at.elapsed() < Duration::from_secs(4),
        "an idle keep-alive connection must not hold shutdown for the whole grace"
    );
    let (_bytes, closed) = read_to_end_within(&mut client, Duration::from_secs(3)).await;
    assert!(closed, "the idle connection is closed by the shutdown");
}

/// 慢读客户端：HTTP/1 的 body 卡在内核缓冲里、Hyper 不再 poll 它，发送期限仍须销毁连接。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http1_slow_reader_still_expires_at_the_send_deadline() {
    let supervisor = test_supervisor(1, 1);
    let handler_supervisor = Arc::clone(&supervisor);
    let router = Router::new().route(
        "/v1/images/generations",
        get(move || {
            let supervisor = Arc::clone(&handler_supervisor);
            async move {
                let lease = supervisor
                    .try_reserve_execution()
                    .expect("an execution lease for the test response");
                let send = supervisor
                    .try_reserve_send()
                    .expect("a send lease for the test response");
                let hold = SendHold::new(
                    Instant::now() + Duration::from_millis(500),
                    lease,
                    send,
                    8 * 1024 * 1024,
                );
                let mut response = Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(vec![b'x'; 8 * 1024 * 1024]))
                    .expect("the test response");
                response.extensions_mut().insert(hold);
                response
            }
        }),
    );
    let (address, stop, server) = start_server(router).await;
    let mut client = TcpStream::connect(address).await.expect("connect");
    client
        .write_all(b"GET /v1/images/generations HTTP/1.1\r\nHost: test\r\n\r\n")
        .await
        .expect("write the request");
    // 从不读：服务端的写很快就阻塞，期限而不是"body 是否继续被读取"必须收口。
    let started = Instant::now();
    assert!(
        wait_until(
            || supervisor.send_slots_available() == 1
                && supervisor.execution_slots_available() == 1,
            Duration::from_secs(5)
        )
        .await,
        "the send deadline must release the permits without the body being polled"
    );
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "the deadline must not wait for the client to read"
    );
    drop(client);
    stop_server(stop, server).await;
}
