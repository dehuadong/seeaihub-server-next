use super::*;

#[test]
fn decodes_inline_data_urls_into_bytes() {
    let encoded = STANDARD.encode([0x89_u8, b'P', b'N', b'G']);
    let decoded =
        decode_data_url(&format!("data:image/png;base64,{encoded}")).expect("data url decodes");
    assert_eq!(decoded.media_type, "image/png");
    assert_eq!(&decoded.bytes[..], &[0x89, b'P', b'N', b'G']);
    // 不带媒体类型时保持中性，不假装知道格式。
    let decoded = decode_data_url("data:;base64,AAAA").expect("decodes");
    assert_eq!(decoded.media_type, "application/octet-stream");
    for bad in [
        "https://example.invalid/a.png",
        "data:image/png,notbase64",
        "data:image/png;base64",
    ] {
        assert!(decode_data_url(bad).is_err(), "`{bad}` is not a data url");
    }
    assert!(is_http_url("https://example.invalid/a.png"));
    assert!(!is_http_url("data:image/png;base64,AAAA"));
}

#[test]
fn generated_images_keep_only_the_shape_the_provider_gave() {
    let url = GeneratedImage::from_url("https://example.invalid/a.png".to_owned());
    assert_eq!(
        serde_json::to_value(&url).expect("serializes"),
        serde_json::json!({"url": "https://example.invalid/a.png"})
    );
    let base64 = GeneratedImage::from_base64("AAAA".to_owned());
    assert_eq!(
        serde_json::to_value(&base64).expect("serializes"),
        serde_json::json!({"b64_json": "AAAA"})
    );
    // 读回来还是同一种形态（结果信封落库、再读出来）。
    assert_eq!(
        serde_json::from_value::<GeneratedImage>(serde_json::json!({"url": "u"}))
            .expect("a url reads back"),
        GeneratedImage::from_url("u".to_owned())
    );
    // 形状本身就是"恰好其一"：两项都在、一项都没有都不是合法的图。
    for impossible in [
        serde_json::json!({}),
        serde_json::json!({"url": "u", "b64_json": "b"}),
    ] {
        assert!(
            serde_json::from_value::<GeneratedImage>(impossible.clone()).is_err(),
            "{impossible} 不是恰好一项"
        );
    }
}

#[test]
fn a_cancel_before_the_generation_gate_blocks_the_send() {
    let gate = DispatchGate::new();
    gate.cancel();
    assert!(!gate.try_begin_generation());
    assert!(!gate.generation_started());
    assert!(gate.is_cancelled());
}

#[test]
fn a_generation_that_started_first_is_recorded_even_if_cancelled_after() {
    let gate = DispatchGate::new();
    assert!(gate.try_begin_generation());
    gate.cancel();
    assert!(
        gate.generation_started(),
        "已经开始发送的事实不能被后来的取消抹掉"
    );
    assert!(gate.is_cancelled(), "之后的取消仍然生效，用于停止后续动作");
}

/// 取消与发送竞争同一个原子字：并发下结论必须与状态一致，且不会两者都"赢"。
#[test]
fn concurrent_cancel_and_generation_have_one_consistent_winner() {
    use std::sync::{Arc, Barrier};
    use std::thread;

    for _ in 0..500 {
        let gate = Arc::new(DispatchGate::new());
        let cancelling = gate.clone();
        let barrier = Arc::new(Barrier::new(2));
        let other = barrier.clone();
        let handle = thread::spawn(move || {
            other.wait();
            cancelling.cancel();
        });
        barrier.wait();
        let started = gate.try_begin_generation();
        handle.join().expect("the cancelling thread");

        assert_eq!(
            started,
            gate.generation_started(),
            "闸口的结论必须与已记录的状态一致"
        );
        if !started {
            assert!(
                gate.is_cancelled(),
                "闸口拒绝意味着取消先赢，取消必须是可观察的"
            );
        }
    }
}

/// 一次执行的内存预留必须同时覆盖入口、上游原始响应与编码后的正文，不能只算一份。
#[test]
fn the_per_execution_envelope_covers_input_response_and_encoding() {
    let limits = GatewayByteLimits {
        request_wire_bytes: 16 * 1024 * 1024,
        provider_response_bytes: 128 * 1024 * 1024,
    };
    let expected = (16 + 128 + 128 * ENCODED_RESPONSE_EXPANSION) * 1024 * 1024;
    assert_eq!(limits.max_bytes_per_execution(), expected);
    assert!(
        limits.max_bytes_per_execution() > 32 * 1024 * 1024,
        "旧的固定 32 MiB 预留装不下最大上游响应与副本"
    );
}

#[test]
fn an_extreme_envelope_saturates_instead_of_wrapping() {
    let huge = GatewayByteLimits {
        request_wire_bytes: usize::MAX,
        provider_response_bytes: usize::MAX,
    };
    assert_eq!(huge.max_bytes_per_execution(), usize::MAX);
}

/// 对账读取上限是独立的一条，并且**不大于**该 Adapter 声明的生成响应上限。
#[test]
fn the_reconciliation_read_limit_stays_within_the_declared_response_limit() {
    let configured =
        reconciliation_read_bytes_from_env().unwrap_or(GATEWAY_RECONCILIATION_READ_BYTES);
    let wide = GatewayByteLimits {
        request_wire_bytes: 16 * 1024 * 1024,
        provider_response_bytes: 128 * 1024 * 1024,
    };
    assert_eq!(
        wide.reconciliation_read_bytes(),
        configured.min(128 * 1024 * 1024),
        "对账读取取配置值，并以声明的生成响应上限收口"
    );
    // 声明面比缺省值还小时，对账读取跟着它收窄，绝不反过来放大。
    let narrow = GatewayByteLimits {
        request_wire_bytes: 16 * 1024 * 1024,
        provider_response_bytes: 1,
    };
    assert_eq!(narrow.reconciliation_read_bytes(), 1);
}

/// 任务句柄只能是有界标识：URL、data URL、超长值与正文在构造处就被拒绝，且不保留原值。
#[test]
fn a_task_handle_can_only_be_parsed_from_a_bounded_identifier() {
    let handle = ProviderTaskHandle::parse("task_abc-1.2".to_owned()).expect("a valid identifier");
    assert_eq!(handle.as_str(), "task_abc-1.2");
    assert_eq!(handle.into_string(), "task_abc-1.2");

    for bad in [
        "https://example.invalid/a.png",
        "data:image/png;base64,AAAA",
        "task id",
        "",
    ] {
        assert!(
            ProviderTaskHandle::parse(bad.to_owned()).is_err(),
            "{bad:?} must not become a task handle"
        );
    }
    assert!(ProviderTaskHandle::parse("a".repeat(129)).is_err());
}
