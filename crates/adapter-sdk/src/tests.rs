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
