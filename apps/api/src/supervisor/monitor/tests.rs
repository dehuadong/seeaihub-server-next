//! 断开监视的注册、清理与故障路径。
//!
//! 用 `UnixStream::pair` 造一对真实 socket：关闭一端就是另一端上的 `RDHUP`，不需要 HTTP。

use std::os::unix::net::UnixStream;

use super::*;

fn test_monitor(max_connections: usize) -> Arc<DisconnectMonitor> {
    DisconnectMonitor::start(MonitorConfig {
        max_connections,
        control_queue_capacity: RELEASE_RESERVE + 8,
        event_batch: 4,
    })
    .expect("the monitor")
}

fn duplicate(stream: &UnixStream) -> Arc<OwnedFd> {
    Arc::new(rustix::io::fcntl_dupfd_cloexec(stream, 0).expect("a duplicate fd"))
}

async fn wait_for_cancel(signal: &DisconnectSignal) -> bool {
    tokio::time::timeout(Duration::from_secs(2), signal.cancelled())
        .await
        .is_ok()
}

/// 对端关闭（含只关写半边）必须取消注册的连接。
#[tokio::test]
async fn a_peer_close_cancels_the_registered_connection() {
    let monitor = test_monitor(4);
    let (left, right) = UnixStream::pair().expect("a socket pair");
    let signal = DisconnectSignal::new();
    monitor
        .register(
            duplicate(&left),
            1,
            Arc::clone(&signal),
            Duration::from_secs(2),
        )
        .await
        .expect("registration");
    drop(right);
    assert!(
        wait_for_cancel(&signal).await,
        "closing the peer must cancel the registered connection"
    );
    monitor.release(1, Duration::from_secs(2)).await;
    monitor.shutdown();
}

/// 对端在注册**之前**已经关闭时，注册仍须立刻得到关闭事件（epoll 会立即投递 RDHUP）。
#[tokio::test]
async fn a_close_before_registration_is_still_observed() {
    let monitor = test_monitor(4);
    let (left, right) = UnixStream::pair().expect("a socket pair");
    drop(right);
    let signal = DisconnectSignal::new();
    monitor
        .register(
            duplicate(&left),
            1,
            Arc::clone(&signal),
            Duration::from_secs(2),
        )
        .await
        .expect("registration");
    assert!(wait_for_cancel(&signal).await);
    monitor.release(1, Duration::from_secs(2)).await;
    monitor.shutdown();
}

/// 清理必须真的摘掉监视；旧连接 ID 的迟到事件不能取消新连接（fd 会被复用，ID 不会）。
#[tokio::test]
async fn cleanup_removes_the_registration_and_stale_ids_do_not_cancel() {
    let monitor = test_monitor(4);
    let (left, right) = UnixStream::pair().expect("a socket pair");
    let first = DisconnectSignal::new();
    monitor
        .register(
            duplicate(&left),
            1,
            Arc::clone(&first),
            Duration::from_secs(2),
        )
        .await
        .expect("the first registration");
    monitor.release(1, Duration::from_secs(2)).await;
    assert!(
        !first.is_cancelled(),
        "a released registration must not be cancelled by a later close"
    );
    // 同一个 fd 号被新连接复用；旧 ID 的事件必须被丢弃。
    let second = DisconnectSignal::new();
    monitor
        .register(
            duplicate(&left),
            2,
            Arc::clone(&second),
            Duration::from_secs(2),
        )
        .await
        .expect("the second registration");
    handle_close_event(&monitor.shared, 1);
    assert!(
        !second.is_cancelled(),
        "a stale event for a released id must not cancel the new registration"
    );
    drop(right);
    assert!(wait_for_cancel(&second).await);
    monitor.release(2, Duration::from_secs(2)).await;
    monitor.shutdown();
}

/// 监视线程致命故障：取消全部在册连接并暂停新准入。
#[tokio::test]
async fn a_fatal_monitor_failure_cancels_everything_and_pauses_admission() {
    let monitor = test_monitor(4);
    let (left, right) = UnixStream::pair().expect("a socket pair");
    let first = DisconnectSignal::new();
    monitor
        .register(
            duplicate(&left),
            1,
            Arc::clone(&first),
            Duration::from_secs(2),
        )
        .await
        .expect("the first registration");
    let (other, _other_peer) = UnixStream::pair().expect("a socket pair");
    let second = DisconnectSignal::new();
    monitor
        .register(
            duplicate(&other),
            2,
            Arc::clone(&second),
            Duration::from_secs(2),
        )
        .await
        .expect("the second registration");

    fail(&monitor.shared, &Errno::INVAL);

    assert!(monitor.is_failed());
    assert!(
        wait_for_cancel(&first).await && wait_for_cancel(&second).await,
        "a fatal failure must cancel every registered connection"
    );
    let third = DisconnectSignal::new();
    assert_eq!(
        monitor
            .register(duplicate(&left), 3, third, Duration::from_millis(200))
            .await,
        Err(MonitorError::Failed),
        "new admission is paused after a fatal failure"
    );
    drop(right);
    monitor.shutdown();
}

/// 控制队列必须为 cleanup 留容量：容量小于预留值就是配置错误，进程不该带着它起来。
#[test]
fn the_control_queue_must_reserve_room_for_cleanup() {
    assert!(
        DisconnectMonitor::start(MonitorConfig {
            max_connections: 4,
            control_queue_capacity: RELEASE_RESERVE,
            event_batch: 4,
        })
        .is_err(),
        "a queue that leaves nothing for cleanup is a configuration error"
    );
    assert!(
        DisconnectMonitor::start(MonitorConfig {
            max_connections: 0,
            control_queue_capacity: 1024,
            event_batch: 4,
        })
        .is_err()
    );
    assert!(
        DisconnectMonitor::start(MonitorConfig {
            max_connections: 4,
            control_queue_capacity: 1024,
            event_batch: 0,
        })
        .is_err()
    );
}
