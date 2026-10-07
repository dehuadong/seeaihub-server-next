use super::{
    AdminSeed::*, ApiError, ApplicationError, CapacityCombination,
    DEFAULT_H2_MAX_CONCURRENT_STREAMS, DEFAULT_MAX_CONNECTIONS, NO_ADMIN_ACCOUNT_WARNING,
    admin_seed_decision, sanitize_provider_text, validate_capacity_combination,
};
use std::{
    io::{self, Write},
    sync::{Arc, Mutex},
};
use tracing_subscriber::fmt::MakeWriter;

/// 图片项的对客 wire 形状：url 项是地址数组（渠道给过期时刻时带 `expires_at`、不给就没有这个键），
/// b64_json 项只有内联串。这是 Spec 0005 §8 A13 的那条判据，钉在序列化这一层。
#[test]
fn a_sync_image_item_serializes_as_a_url_array_or_inline_base64() {
    use super::SyncImageItem;
    use seeai_application::GeneratedImage;

    let with_expiry = SyncImageItem::from(GeneratedImage::from_urls(
        vec![
            "https://a.example/1.png".to_owned(),
            "https://a.example/2.png".to_owned(),
        ],
        Some(1_789_000_000),
    ));
    assert_eq!(
        serde_json::to_value(&with_expiry).expect("serializes"),
        serde_json::json!({
            "url": ["https://a.example/1.png", "https://a.example/2.png"],
            "expires_at": 1_789_000_000
        })
    );
    let without_expiry = SyncImageItem::from(GeneratedImage::from_url(
        "https://a.example/1.png".to_owned(),
    ));
    assert_eq!(
        serde_json::to_value(&without_expiry).expect("serializes"),
        serde_json::json!({"url": ["https://a.example/1.png"]})
    );
    let base64 = SyncImageItem::from(GeneratedImage::from_base64("AAAA".to_owned()));
    assert_eq!(
        serde_json::to_value(&base64).expect("serializes"),
        serde_json::json!({"b64_json": "AAAA"})
    );
}

/// 把 tracing 的输出收进内存，好断言"这条错误到底有没有留下痕迹"。
///
/// 用内存而不是子进程的 stderr：这里要验的是**这个转换有没有记日志、记的是不是带来源的那条**，
/// 那是一个纯函数行为；"日志最终写到哪"由 [`tracing_subscriber`] 在进程启动时定，与本转换无关。
#[derive(Clone, Default)]
struct CapturedLogs {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl CapturedLogs {
    /// 收上来的日志文本，**把 ANSI 转义序列剥掉**。
    ///
    /// 剥掉是必须的：`tracing_subscriber` 在**支持颜色**的输出上会给级别与字段名套 SGR 序列
    /// （`ESC[2mcategoryESC[0m="persistence"`），而在不支持的地方（本机重定向、被捕获的 writer）
    /// 不带。断言里搜的是 `category="persistence"` 这种**纯文本片段**，不剥的话这条用例会**只在有颜色
    /// 的环境上失败**——CI 就是这样红了很久，而本地一直是绿的。
    fn text(&self) -> String {
        let raw = String::from_utf8(self.bytes.lock().expect("logs lock").clone())
            .expect("logs are utf-8");
        strip_ansi(&raw)
    }
}

/// 去掉 `ESC[...m` 这类 SGR 序列。只处理 CSI 序列，够覆盖日志着色。
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\u{1b}' {
            out.push(character);
            continue;
        }
        // `ESC` 之后若是 `[`，就一直吃到终止字节（ASCII 的 `@`..=`~`）。
        if chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        }
    }
    out
}

impl Write for CapturedLogs {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes
            .lock()
            .expect("logs lock")
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CapturedLogs {
    type Writer = CapturedLogs;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// 5xx 的**对客文案不泄漏细节**，所以日志是排障唯一能看到原因的地方：捕获的那个 500 必须留下
/// 一条带**类别**（哪一层）与**完整错误内容**的记录。
#[test]
fn a_server_error_is_logged_with_its_category_and_message() {
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_max_level(tracing::Level::ERROR)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let api_error: ApiError =
        ApplicationError::Persistence("sum over a malformed numeric column failed".to_owned())
            .into();

    assert_eq!(api_error.status, 500);
    assert_eq!(api_error.code, "internal_error");
    assert_eq!(
        api_error.message, "The server could not complete the request",
        "对客文案不泄漏内部细节"
    );
    let logged = logs.text();
    assert!(
        logged.contains("category=\"persistence\""),
        "日志必须点明是哪一层出的错：{logged}"
    );
    assert!(
        logged.contains("sum over a malformed numeric column failed"),
        "日志必须带上错误内容本身，否则排障还是只能猜：{logged}"
    );
}

/// 平台侧故障的两条路各自已有更具体的 warn（"一条候选都承载不了"、"成本护栏拦下"），兜底那条
/// ERROR 不能再打一遍：同一个错误两条日志，其中一条还说不清是哪一类。
#[test]
fn the_platform_side_failures_keep_their_own_warning() {
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_max_level(tracing::Level::ERROR)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let no_offering: ApiError =
        ApplicationError::NoEligibleOffering("no candidate can carry this request".to_owned())
            .into();
    let ceiling: ApiError = ApplicationError::RequestCostCeilingExceeded(
        "this request could cost 12000000 microusd".to_owned(),
    )
    .into();

    assert_eq!(no_offering.status, 503);
    assert_eq!(ceiling.status, 503);
    assert_eq!(no_offering.code, "platform_unavailable");
    assert_eq!(
        logs.text(),
        "",
        "这两条已有自己的 warn，不该再被 ERROR 记一遍"
    );
}

#[test]
fn ansi_colouring_does_not_break_log_assertions() {
    // 这条是给 CI 的回归护栏：`tracing_subscriber` 在支持颜色的输出上会给级别与字段名套 SGR 序列，
    // 而在本机（重定向、被捕获的 writer）通常不带。断言搜的是纯文本片段，所以**不剥 ANSI 的写法只在
    // 有颜色的环境上失败**——本仓库的 CI 就是这样红了很久而本地一直绿。
    //
    // 这里显式开颜色造出 CI 那个条件，验证 `text()` 剥掉之后断言仍然成立。
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_ansi(true)
        .with_max_level(tracing::Level::ERROR)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let api_error: ApiError =
        ApplicationError::Persistence("sum over a malformed numeric column failed".to_owned())
            .into();
    assert_eq!(api_error.status, 500);

    let logged = logs.text();
    assert!(
        !logged.contains('\u{1b}'),
        "剥完之后不该还有转义字符：{logged:?}"
    );
    assert!(
        logged.contains("category=\"persistence\""),
        "带颜色的输出剥掉之后也必须能断言：{logged}"
    );
}

#[test]
fn the_admin_bootstrap_branches_are_distinguishable() {
    // 两个都给：建号（口令与邮箱原样传下去）。
    assert_eq!(
        admin_seed_decision(
            Some("ops@example.com".to_owned()),
            Some("secret".to_owned())
        ),
        Create {
            email: "ops@example.com".to_owned(),
            password: "secret".to_owned(),
        }
    );

    // 两个都不给：**警告、不建号、进程照起**。后台登不进去这件事要能被发现，但不该让 API 起不来。
    assert_eq!(admin_seed_decision(None, None), WarnNoAccount);
    // 空串与没配是一回事（部署里很常见）。
    assert_eq!(
        admin_seed_decision(Some("   ".to_owned()), Some(String::new())),
        WarnNoAccount
    );

    // 只给一个：点名"谁在、谁缺"，信息要够运维直接改对。
    assert_eq!(
        admin_seed_decision(Some("ops@example.com".to_owned()), None),
        Reject {
            present: "ADMIN_EMAIL",
            missing: "ADMIN_PASSWORD",
        }
    );
    assert_eq!(
        admin_seed_decision(None, Some("secret".to_owned())),
        Reject {
            present: "ADMIN_PASSWORD",
            missing: "ADMIN_EMAIL",
        }
    );
}

#[test]
fn the_missing_admin_account_warning_says_the_console_cannot_be_used() {
    // V-A8 要的"日志里看得出没有管理员账号"：文案必须点明**后果**（后台登不进去）与**原因**
    // （没有这两个变量），否则运维只会看到"什么都没发生"。
    //
    // 这里打的是**生产那句**（`seed_admin_account` 用的同一个常量），不是在测试里再抄一遍字面量：
    // 抄一遍的话，把生产那句删掉或改写之后这条仍然通过，等于没验（复核抓到的正是这一点）。
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_max_level(tracing::Level::WARN)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    tracing::warn!("{}", NO_ADMIN_ACCOUNT_WARNING);

    let logged = logs.text();
    for needle in ["ADMIN_EMAIL", "ADMIN_PASSWORD", "cannot be logged into"] {
        assert!(logged.contains(needle), "警告里必须出现 {needle}：{logged}");
    }
}

#[test]
fn credential_fragments_are_removed_from_provider_text() {
    assert_eq!(
        sanitize_provider_text("Forbidden – key(ab12cd) allowed only from approved IP ranges."),
        "Forbidden –  allowed only from approved IP ranges."
    );
    // 一条消息里出现多次也要全去掉。
    assert_eq!(
        sanitize_provider_text("key(aaa) then key(bbb) done"),
        " then  done"
    );
}

#[test]
fn text_without_credential_fragments_is_untouched() {
    assert_eq!(
        sanitize_provider_text("insufficient_user_quota: quota exhausted"),
        "insufficient_user_quota: quota exhausted"
    );
}

#[test]
fn an_unclosed_fragment_drops_the_rest_of_the_message() {
    // 宁可丢掉后半句，也不能把括号里的内容放出去。
    assert_eq!(
        sanitize_provider_text("Forbidden – key(ab12cd nothing closes this"),
        "Forbidden – "
    );
}

// ── RFC 0018 §2.1 预留常数的实测 ──────────────────────────────────────────────
//
// 这些用例测的是**真实代码路径**在峰值时让进程常驻内存多涨了多少：用 `/proc/self/status` 的
// `VmHWM`（峰值 RSS，单调不减）取操作前后的差值。因此它给出的是"实际持有过的内存"，不是按倍数
// 推算的估计值；分配器的保留内存也计入其中。
//
// **一个用例一个进程**：`VmHWM` 是进程级的单调水位，同进程里跑第二个用例时"操作前"的水位已经
// 包含了第一个用例的峰值，差值就不再是这一段的峰值。所以按名字逐个跑（六条）：
//
//   cargo test -p seeai-api --bin seeai-api -- --ignored --nocapture --test-threads=1 \
//     memory_request_parse_peak_image
//
// 其余五个名字：memory_request_parse_peak_scalar_nodes、memory_request_parse_peak_small_object_nodes、
// memory_mapped_parameter_peak、memory_provider_response_and_parsed_image_peak、
// memory_encoded_client_body_peak。这里测出来的数字写在 `crates/adapter-sdk` 的 `GATEWAY_*`
// 常量注释里，改夹具就得连那些常数一起改。
//
// 夹具（16 MiB 请求正文、128 MiB 上游正文）也**直接拼字节**，不先造 `json!` 中间对象：那种中间
// 对象会在测量窗口之前就把水位抬起来、随后被释放，被测阶段的分配复用这些页，增量会被抹平成 0。
//
// 只有 Linux 有 `VmHWM`：这些用例因此是 Linux 专属（直接执行本身也只在 Linux 上启动）。
#[cfg(target_os = "linux")]
mod memory_measurement {
    use crate::{
        SyncImageData, SyncImageItem, SyncImageResponse, SyncImageResult,
        take_contract_image_inputs,
    };
    use seeai_adapter_aihubmix::{ADAPTER_KEY as AIHUBMIX_ADAPTER_KEY, AihubmixAdapterFactory};
    use seeai_adapter_sdk::{
        AcceptanceError, AcceptedHandle, Deadline, ExecutionContext, ExternalActionRefused,
        GATEWAY_REQUEST_WIRE_BYTES, GatewayInput, ImageSites, ProviderCredential,
    };
    use seeai_application::{AdapterFactory, CustomerUsageStatus, GeneratedImage};
    use seeai_domain::{
        ImageBranch, JobId, RequestJsonError, RequestParameters, RequestStructureViolation,
    };
    use serde_json::json;
    use std::sync::Arc;
    use std::time::Duration;

    /// 进程峰值常驻内存（`VmHWM`，KiB）。单调不减：操作前后的差值就是这段操作抬高的峰值。
    fn process_peak_rss_kib() -> u64 {
        let status =
            std::fs::read_to_string("/proc/self/status").expect("/proc/self/status is readable");
        let line = status
            .lines()
            .find(|line| line.starts_with("VmHWM:"))
            .expect("VmHWM is reported for a live process");
        line.split_whitespace()
            .nth(1)
            .expect("VmHWM has a numeric value")
            .parse()
            .expect("VmHWM is in KiB")
    }

    fn kib(bytes: usize) -> u64 {
        u64::try_from(bytes / 1024).unwrap_or(u64::MAX)
    }

    /// 接近入口上限的请求正文：一个 data URL 参考图吃掉 16 MiB 里的绝大部分。
    ///
    /// 直接拼字节而不是先造 `json!` 值再序列化：中间大对象会在测量窗口之前就把进程峰值抬起来，
    /// 那些页随后被解析复用，`VmHWM` 的增量就会被抹平（第一次实测就踩到，量出来是 0）。
    fn max_wire_request_body() -> Vec<u8> {
        let budget = GATEWAY_REQUEST_WIRE_BYTES;
        let head = b"{\"model\":\"measure-model\",\"prompt\":\"";
        let tail = b"\",\"n\":1}";
        let payload = budget - head.len() - tail.len();
        let mut body = Vec::with_capacity(budget);
        body.extend_from_slice(head);
        body.resize(body.len() + payload, b'A');
        body.extend_from_slice(tail);
        assert_eq!(body.len(), budget, "夹具必须正好贴住声明的入口上限");
        body
    }

    /// 同样是贴着上限的 16 MiB 正文，但几乎全是一个大数组里的标量节点（2 字节一个节点）。
    ///
    /// 解析结构的大小由**节点数**决定，与 wire 字节数不成正比（RFC 0018 §2.2 的节点/字段计数就是
    /// 要收这一支）。每种形状单独测、取其中最大的进公式才是覆盖。
    fn max_wire_scalar_nodes_body() -> Vec<u8> {
        array_body(b"0")
    }

    /// 一个节点一个单字段对象（`{"a":0}`）：每个对象自己一次堆分配，是节点密集里更贵的一档。
    fn max_wire_small_object_nodes_body() -> Vec<u8> {
        array_body(b"{\"a\":0}")
    }

    fn array_body(element: &[u8]) -> Vec<u8> {
        let budget = GATEWAY_REQUEST_WIRE_BYTES;
        let head = b"{\"model\":\"measure-model\",\"x\":[";
        let tail = b"]}";
        // 元素之间一个逗号。
        let elements = (budget - head.len() - tail.len() + 1) / (element.len() + 1);
        let mut body = Vec::with_capacity(budget);
        body.extend_from_slice(head);
        for _ in 0..elements {
            if body.last() != Some(&b'[') {
                body.push(b',');
            }
            body.extend_from_slice(element);
        }
        body.extend_from_slice(tail);
        assert!(body.len() <= budget, "夹具必须留在声明的入口上限内");
        body
    }

    /// 入口 wire → 解析结构（RFC 0018 §2.1 的 `A`），大图输入。
    ///
    /// 解析走线上那条入口（[`RequestParameters::parse`]，有界 visitor），不是另写一个等价的 `Value`
    /// 解析：测的必须是线上那条路径。解析结果活着离开测量窗口——峰值里同时有 wire 与解析结构，
    /// 正是公式里 `I + A` 的形态。
    #[test]
    #[ignore = "内存实测：Linux + 单线程跑，见本模块注释"]
    fn memory_request_parse_peak_image() {
        let body = max_wire_request_body();
        let wire = body.len();
        let before = process_peak_rss_kib();
        let parsed = RequestParameters::parse(&body).expect("the max-wire request parses");
        let after = process_peak_rss_kib();
        println!(
            "请求解析（大图）：wire {wire} 字节（{} KiB），解析结构额外峰值 {} KiB",
            kib(wire),
            after - before
        );
        assert!(!parsed.as_object().is_empty());
    }

    /// 入口 wire → 解析结构，标量节点密集。
    ///
    /// 有界解析在节点数上限处失败：测出来的增量是"走到上限为止已经构造出来的那些节点"，不是整份
    /// 16 MiB 正文能撑出的结构。它必须留在 `GATEWAY_REQUEST_PARSE_BYTES` 之内。
    #[test]
    #[ignore = "内存实测：Linux + 单线程跑，见本模块注释"]
    fn memory_request_parse_peak_scalar_nodes() {
        let body = max_wire_scalar_nodes_body();
        let wire = body.len();
        let before = process_peak_rss_kib();
        let error =
            RequestParameters::parse(&body).expect_err("the node-heavy request is rejected");
        let after = process_peak_rss_kib();
        println!(
            "请求解析（标量节点密集，超限拒绝：{error}）：wire {wire} 字节（{} KiB），解析结构额外峰值 {} KiB",
            kib(wire),
            after - before
        );
        assert!(matches!(
            error,
            RequestJsonError::Limit(RequestStructureViolation::Nodes { .. })
        ));
    }

    /// 入口 wire → 解析结构，单字段对象密集（每节点一次 `Map` 分配，是最贵的一档）。
    #[test]
    #[ignore = "内存实测：Linux + 单线程跑，见本模块注释"]
    fn memory_request_parse_peak_small_object_nodes() {
        let body = max_wire_small_object_nodes_body();
        let wire = body.len();
        let before = process_peak_rss_kib();
        let error =
            RequestParameters::parse(&body).expect_err("the object-heavy request is rejected");
        let after = process_peak_rss_kib();
        println!(
            "请求解析（小对象密集，超限拒绝：{error}）：wire {wire} 字节（{} KiB），解析结构额外峰值 {} KiB",
            kib(wire),
            after - before
        );
        assert!(matches!(
            error,
            RequestJsonError::Limit(RequestStructureViolation::Nodes { .. })
        ));
    }

    /// 解析结构 → API 侧映射（图片提升为强类型输入；RFC 0018 §2.1 的 `X` 可测到的部分）。
    #[test]
    #[ignore = "内存实测：Linux + 单线程跑，见本模块注释"]
    fn memory_mapped_parameter_peak() {
        let body = max_wire_request_body();
        let mut parsed = RequestParameters::parse(&body).expect("the max-wire request parses");
        let before = process_peak_rss_kib();
        let inputs = take_contract_image_inputs(&mut parsed).expect("image fields");
        // 生成入口只收公网 URL：贴着上限的正文里没有图片字段，映射这一档不再搬运图片字节。
        let reference_images = inputs
            .reference_images
            .into_iter()
            .map(crate::public_image_url)
            .collect::<Result<Vec<_>, _>>()
            .expect("no inline image is left to map");
        let mask = inputs
            .mask
            .map(crate::public_image_url)
            .transpose()
            .expect("no inline mask is left to map");
        let after = process_peak_rss_kib();
        println!(
            "映射后参数：参考图 {} 张、遮罩 {}，额外峰值 {} KiB",
            reference_images.len(),
            mask.is_some() as usize,
            after - before
        );
        assert!(reference_images.is_empty());
        assert!(mask.is_none());
    }

    /// 上游响应原缓冲 → 解析出的图片字符串（RFC 0018 §2.1 的 `U` + `P`）。
    ///
    /// 走真实 Driver（AIHubMix）打本地假上游：响应正文在测量窗口**之前**就分配好并由假上游持有，
    /// 因此测量窗口里的增量是客户端侧的读缓冲与解析结果，而不是假上游那份正文。
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "内存实测：Linux + 单线程跑，见本模块注释"]
    async fn memory_provider_response_and_parsed_image_peak() {
        let target = seeai_adapter_aihubmix::MAX_PROVIDER_RESPONSE_BYTES - 4096;
        let response_body = aihubmix_response_body(target);
        let body_bytes = response_body.len();
        let upstream = start_fake_upstream(response_body).await;
        let adapter = AihubmixAdapterFactory
            .create_gateway(
                AIHUBMIX_ADAPTER_KEY,
                &upstream.base_url,
                Duration::from_secs(60),
            )
            .expect("the aihubmix gateway adapter is assembled");
        let input = Arc::new(GatewayInput {
            provider_model_id: "measure-model".to_owned(),
            branch: ImageBranch::PromptOnly,
            native_parameters: json!({"prompt": "measure"}),
            reference_images: Vec::new(),
            mask: None,
            image_sites: ImageSites::default(),
            cost_currency: "USD".to_owned(),
        });
        let credential = ProviderCredential::new("measure-credential".to_owned())
            .expect("a non-empty credential");
        let context = MeasuringContext;
        let before = process_peak_rss_kib();
        let output = adapter
            .execute(input, &context, &credential)
            .await
            .expect("the fake upstream answers with a readable response");
        let after = process_peak_rss_kib();
        let parsed_image_bytes: usize = output
            .response_payload
            .images
            .iter()
            .map(|image| match image {
                GeneratedImage::B64Json(value) => value.len(),
                GeneratedImage::Url { urls, .. } => urls.iter().map(String::len).sum(),
            })
            .sum();
        println!(
            "上游响应：正文 {} 字节（{} MiB），解析出图片 {} 字节（{} MiB），额外峰值 {} KiB（{} MiB）",
            body_bytes,
            body_bytes / (1024 * 1024),
            parsed_image_bytes,
            parsed_image_bytes / (1024 * 1024),
            after - before,
            (after - before) / 1024,
        );
        assert_eq!(output.response_payload.images.len(), 1);
        upstream.task.abort();
    }

    /// 解析出的图片字符串 → 编码后的对客正文（RFC 0018 §2.1 的 `C`）。
    ///
    /// 编码用的就是成功响应那行 `serde_json::to_vec(&SyncImageResponse { .. })`：图片字符串在
    /// 测量窗口之前就分配好，窗口里的增量是编码正文本身（含它扩容时的瞬时峰值）。
    #[test]
    #[ignore = "内存实测：Linux + 单线程跑，见本模块注释"]
    fn memory_encoded_client_body_peak() {
        let image_bytes = seeai_adapter_aihubmix::MAX_PROVIDER_RESPONSE_BYTES - 4096;
        let body = SyncImageResponse {
            code: 200,
            data: SyncImageData {
                id: JobId::new(),
                status: CustomerUsageStatus::Completed,
                cost: 0,
                result: SyncImageResult {
                    images: vec![SyncImageItem::from(GeneratedImage::from_base64(
                        "A".repeat(image_bytes),
                    ))],
                },
            },
        };
        let before = process_peak_rss_kib();
        let payload = serde_json::to_vec(&body).expect("the client body serializes");
        let after = process_peak_rss_kib();
        // 信封：合同允许的产出张数上限（`NO_CONTRACT_MAX_OUTPUT_IMAGES = 10`）下、图片字符串之外的
        // 固定开销。空字符串把这一项单独量出来，不带任何图片字节。
        let envelope = SyncImageResponse {
            code: 200,
            data: SyncImageData {
                id: JobId::new(),
                status: CustomerUsageStatus::Completed,
                cost: 0,
                result: SyncImageResult {
                    images: (0..10)
                        .map(|_| SyncImageItem::from(GeneratedImage::from_base64(String::new())))
                        .collect(),
                },
            },
        };
        let envelope_bytes = serde_json::to_vec(&envelope)
            .expect("the envelope serializes")
            .len();
        println!(
            "客户端编码：图片字符串 {} 字节，编码后正文 {} 字节（比例 {:.4}），额外峰值 {} KiB",
            image_bytes,
            payload.len(),
            payload.len() as f64 / image_bytes as f64,
            after - before,
        );
        println!("客户端编码信封：10 张图的固定开销 {envelope_bytes} 字节");
        assert!(payload.len() >= image_bytes);
    }

    /// 一次成功的 AIHubMix 同步响应：`b64_json` 占满给定字节数。
    ///
    /// 同样是直接拼字节：`json!` + 序列化会产生第二份 128 MiB 中间对象，它先抬高峰值又被释放，
    /// 客户端读缓冲随后复用这些页，增量就测不出来了。
    fn aihubmix_response_body(target_bytes: usize) -> Vec<u8> {
        let head = b"{\"created\":1790000000,\"data\":[{\"b64_json\":\"";
        let tail = b"\"}],\"usage\":{\"input_tokens\":14,\"input_tokens_details\":{\"cached_tokens\":0,\"image_tokens\":0,\"text_tokens\":14},\"output_tokens\":196,\"output_tokens_details\":{\"image_tokens\":196,\"text_tokens\":0},\"total_tokens\":210}}";
        let payload = target_bytes - head.len() - tail.len();
        let mut body = Vec::with_capacity(target_bytes);
        body.extend_from_slice(head);
        body.resize(body.len() + payload, b'A');
        body.extend_from_slice(tail);
        assert_eq!(body.len(), target_bytes);
        body
    }

    /// 只回一份**预置正文**的本地假上游；正文由调用方在测量窗口之前分配好。
    struct FakeUpstream {
        base_url: String,
        task: tokio::task::JoinHandle<()>,
    }

    async fn start_fake_upstream(body: Vec<u8>) -> FakeUpstream {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the fake upstream binds");
        let port = listener.local_addr().expect("addr").port();
        let body = Arc::new(body);
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let body = Arc::clone(&body);
                tokio::spawn(async move {
                    let _ = serve_fake_response(&mut socket, &body).await;
                });
            }
        });
        FakeUpstream {
            base_url: format!("http://127.0.0.1:{port}/"),
            task,
        }
    }

    async fn serve_fake_response(
        socket: &mut tokio::net::TcpStream,
        body: &[u8],
    ) -> std::io::Result<()> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // 读满请求（生成入口是 JSON，带 Content-Length）：不读完就回会导致客户端重置。
        let mut received = Vec::new();
        let mut chunk = [0_u8; 8192];
        loop {
            let read = socket.read(&mut chunk).await?;
            if read == 0 {
                break;
            }
            received.extend_from_slice(&chunk[..read]);
            if let Some(head_end) = received.windows(4).position(|window| window == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&received[..head_end]).to_ascii_lowercase();
                let length = head.lines().find_map(|line| {
                    line.strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                });
                match length {
                    Some(length) if received.len() >= head_end + 4 + length => break,
                    None => break,
                    _ => {}
                }
            }
        }
        let head = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        socket.write_all(head.as_bytes()).await?;
        socket.write_all(body).await?;
        socket.flush().await
    }

    /// 只满足 Driver 资格检查的执行上下文：不取消、不落库。
    struct MeasuringContext;

    #[async_trait::async_trait]
    impl ExecutionContext for MeasuringContext {
        fn deadline(&self) -> Deadline {
            Deadline::after(Duration::from_secs(60))
        }

        fn client_gone(&self) -> bool {
            false
        }

        fn ownership_lost(&self) -> bool {
            false
        }

        fn try_begin_external_action(&self) -> Result<(), ExternalActionRefused> {
            Ok(())
        }

        async fn accepted(&self, _handle: AcceptedHandle) -> Result<(), AcceptanceError> {
            Ok(())
        }
    }
}

// ── 启动容量组合校验（RFC 0018 §2.3）───────────────────────────────────────────
//
// 判据只有一条主线："配置出来的名额，上层资源盖得住吗"。不自洽时拒绝启动，并把相关的配置名与
// 数字都点出来——运维要能照着报错改，而不是收到一句"容量不足"。

/// 一份刚好自洽的组合：一次执行的预留，预算正好够一个执行名额，连接容量够读/发送名额。
fn self_consistent_combination() -> CapacityCombination {
    const RESERVATION: usize = 96 * 1024 * 1024;
    CapacityCombination {
        execution_slots: 4,
        execution_memory_bytes: RESERVATION,
        max_memory_bytes: RESERVATION * 4,
        read_slots: 64,
        send_slots: 64,
        max_connections: DEFAULT_MAX_CONNECTIONS,
        h2_max_concurrent_streams: DEFAULT_H2_MAX_CONCURRENT_STREAMS,
    }
}

/// 刚好的那一档必须通过：预算正好等于 名额 × 单次预留（不多一个字节）不算不自洽。
#[test]
fn the_smallest_self_consistent_combination_starts() {
    let capacity = self_consistent_combination();
    assert_eq!(
        capacity.max_memory_bytes,
        capacity.execution_slots * capacity.execution_memory_bytes,
        "夹具要卡在等号上，才验得到边界"
    );
    assert!(validate_capacity_combination(&capacity).is_ok());
}

/// 执行名额比内存预算盖得住的还多：拒绝启动，并同时点名名额与预算两个配置。
#[test]
fn execution_slots_the_memory_budget_cannot_cover_are_refused_by_name() {
    let capacity = CapacityCombination {
        execution_slots: 64,
        ..self_consistent_combination()
    };
    let error = validate_capacity_combination(&capacity).expect_err("64 个名额装不进 4 份预算");
    assert!(
        error.contains("GENERATION_EXECUTION_SLOTS"),
        "报错要点名名额配置，got {error}"
    );
    assert!(
        error.contains("GENERATION_MAX_MEMORY_BYTES"),
        "报错要点名预算配置，got {error}"
    );
    assert!(error.contains("64"), "报错要带上实际取值，got {error}");
}

/// 预算连一次执行都盖不住：同样是拒绝，而不是把单次预留改小。
#[test]
fn a_memory_budget_below_one_execution_is_refused_by_name() {
    let capacity = CapacityCombination {
        execution_slots: 1,
        max_memory_bytes: 96 * 1024 * 1024 - 1,
        ..self_consistent_combination()
    };
    let error = validate_capacity_combination(&capacity).expect_err("预算不足一次执行");
    assert!(
        error.contains("GENERATION_MAX_MEMORY_BYTES"),
        "报错要点名预算配置，got {error}"
    );
}

/// 读取/发送名额超过连接容量能承载的：拒绝启动并点名连接配置。
///
/// 连接容量按 `API_MAX_CONNECTIONS × API_H2_MAX_CONCURRENT_STREAMS` 算：HTTP/1 是每连接 1，
/// HTTP/2 是每连接多条流，取大者是**保守可达**下界。
#[test]
fn read_and_send_slots_above_the_connection_capacity_are_refused_by_name() {
    let narrow = CapacityCombination {
        max_connections: 1,
        h2_max_concurrent_streams: 1,
        read_slots: 2,
        send_slots: 1,
        execution_slots: 1,
        ..self_consistent_combination()
    };
    let error = validate_capacity_combination(&narrow).expect_err("两条流装不下两个读取名额");
    assert!(
        error.contains("GENERATION_READ_SLOTS") && error.contains("API_MAX_CONNECTIONS"),
        "报错要点名读取名额与连接上限，got {error}"
    );

    let narrow_send = CapacityCombination {
        max_connections: 1,
        h2_max_concurrent_streams: 1,
        read_slots: 1,
        send_slots: 2,
        execution_slots: 1,
        ..self_consistent_combination()
    };
    let error = validate_capacity_combination(&narrow_send).expect_err("一条流装不下两个发送名额");
    assert!(
        error.contains("GENERATION_SEND_SLOTS") && error.contains("API_H2_MAX_CONCURRENT_STREAMS"),
        "报错要点名发送名额与流上限，got {error}"
    );
}

/// 名额为 0 也要点名对应的配置，而不是静默取一个缺省。
#[test]
fn zero_slot_counts_are_refused_by_name() {
    for (capacity, expected) in [
        (
            CapacityCombination {
                execution_slots: 0,
                ..self_consistent_combination()
            },
            "GENERATION_EXECUTION_SLOTS",
        ),
        (
            CapacityCombination {
                read_slots: 0,
                ..self_consistent_combination()
            },
            "GENERATION_READ_SLOTS",
        ),
        (
            CapacityCombination {
                send_slots: 0,
                ..self_consistent_combination()
            },
            "GENERATION_SEND_SLOTS",
        ),
    ] {
        let error = validate_capacity_combination(&capacity).expect_err("0 个名额不成组合");
        assert!(error.contains(expected), "got {error}");
    }
}
