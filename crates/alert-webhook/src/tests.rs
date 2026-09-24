use super::*;
use chrono::Utc;
use seeai_application::ProviderFailureKind;
use seeai_domain::JobId;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

/// 一个只收告警的本地接收器：记下每条请求的正文，并按给定状态码应答。
///
/// 用裸 TCP 而不是一个测试框架的假服务：这里要验的正是"线上到底发了什么、发了几次"，
/// 多一层框架只会把这两件事藏起来。
struct Receiver {
    url: String,
    bodies: Arc<Mutex<Vec<String>>>,
    _task: tokio::task::JoinHandle<()>,
}

impl Receiver {
    async fn start(status: u16) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the receiver binds a local port");
        let port = listener.local_addr().expect("the receiver address").port();
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let recorded = bodies.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let recorded = recorded.clone();
                tokio::spawn(async move {
                    let _ = serve(&mut socket, recorded, status).await;
                });
            }
        });
        Self {
            url: format!("http://127.0.0.1:{port}/alerts"),
            bodies,
            _task: task,
        }
    }

    fn bodies(&self) -> Vec<String> {
        self.bodies.lock().expect("bodies lock").clone()
    }

    /// 等到收到 `count` 条为止（有上限地等）：发送是异步的，断言要等它到。
    async fn wait_for(&self, count: usize) -> Vec<String> {
        for _ in 0..200 {
            let bodies = self.bodies();
            if bodies.len() >= count {
                return bodies;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        self.bodies()
    }
}

async fn serve(
    socket: &mut tokio::net::TcpStream,
    bodies: Arc<Mutex<Vec<String>>>,
    status: u16,
) -> std::io::Result<()> {
    let mut reader = tokio::io::BufReader::new(&mut *socket);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;
    let mut content_length = 0_usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).await? == 0 {
            break;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0_u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).await?;
    }
    if let Ok(mut bodies) = bodies.lock() {
        bodies.push(String::from_utf8_lossy(&body).into_owned());
    }
    let reason = if status < 400 { "OK" } else { "Error" };
    let head =
        format!("HTTP/1.1 {status} {reason}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
    socket.write_all(head.as_bytes()).await?;
    socket.flush().await
}

fn alert() -> PlatformAlert {
    PlatformAlert {
        job_id: JobId::new(),
        provider_kind: "APIMart".to_owned(),
        failure_kind: ProviderFailureKind::PlatformFunding,
        occurred_at: Utc::now(),
    }
}

/// 线上发的就是那条告警：POST 一个 JSON 对象，键与值都能逐字对上。
#[tokio::test]
async fn an_alert_is_posted_as_json() {
    let receiver = Receiver::start(200).await;
    let sink = WebhookAlertSink::new(&receiver.url, Duration::from_secs(5), 0)
        .expect("the receiver address is usable");
    let sent = alert();

    sink.send(&sent)
        .await
        .expect("the receiver accepts the alert");

    let bodies = receiver.wait_for(1).await;
    assert_eq!(bodies.len(), 1, "一条告警就是一次请求");
    let payload: serde_json::Value = serde_json::from_str(&bodies[0]).expect("the body is JSON");
    assert_eq!(
        payload,
        serde_json::json!({
            "job_id": sent.job_id.0.to_string(),
            "provider_kind": "APIMart",
            "failure_kind": "platform_funding",
            "occurred_at": sent.occurred_at,
        }),
        "线上必须只有这四个字段，且取值与那条告警一模一样"
    );
}

/// 对端报错就重试，但**次数有界**：重试次数是配置项，试满就认输，不无限发。
#[tokio::test]
async fn a_refused_delivery_is_retried_up_to_the_configured_bound() {
    let receiver = Receiver::start(500).await;
    let sink = WebhookAlertSink::new(&receiver.url, Duration::from_secs(5), 2)
        .expect("the receiver address is usable");

    let error = sink
        .send(&alert())
        .await
        .expect_err("an error answer must not be taken as delivered");
    assert!(
        error.to_string().contains("after 3 attempt(s)"),
        "试满 1 + 2 次就认输：{error}"
    );
    assert_eq!(receiver.wait_for(3).await.len(), 3);
}

/// 对端根本不在（连接被拒）时**不 panic、不挂住**：这是一种发送失败，由出口收口。
#[tokio::test]
async fn an_unreachable_webhook_fails_without_retrying_forever() {
    // 端口 1 上没有服务：连接会被立刻拒绝，用例因此不必等超时。
    let sink = WebhookAlertSink::new("http://127.0.0.1:1/alerts", Duration::from_secs(2), 0)
        .expect("even an unreachable address is a usable address");
    assert!(sink.send(&alert()).await.is_err());
}

/// 地址写错了是**配置错误**，不是"这次没发出去"：构造时就失败，进程起不来比默默发不出去好。
#[test]
fn a_misconfigured_address_is_refused_at_construction() {
    assert!(WebhookAlertSink::new("not a url", Duration::from_secs(1), 0).is_err());
    assert!(
        WebhookAlertSink::new("ftp://example.invalid/alerts", Duration::from_secs(1), 0).is_err()
    );
}
