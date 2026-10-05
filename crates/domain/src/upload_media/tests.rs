use super::*;
use chrono::TimeZone;

fn png() -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x0D]);
    bytes
}

fn jpeg() -> Vec<u8> {
    vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]
}

fn webp() -> Vec<u8> {
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&[0x1A, 0x00, 0x00, 0x00]);
    bytes.extend_from_slice(b"WEBP");
    bytes.extend_from_slice(&[0x56, 0x50, 0x38, 0x20]);
    bytes
}

#[test]
fn magic_bytes_identify_the_three_accepted_types() {
    assert_eq!(
        UploadMediaType::from_magic_bytes(&png()),
        Some(UploadMediaType::Png)
    );
    assert_eq!(
        UploadMediaType::from_magic_bytes(&jpeg()),
        Some(UploadMediaType::Jpeg)
    );
    assert_eq!(
        UploadMediaType::from_magic_bytes(&webp()),
        Some(UploadMediaType::Webp)
    );
}

#[test]
fn magic_bytes_reject_unknown_and_truncated_content() {
    // 0 字节文件没有魔数，按不受理处理。
    assert_eq!(UploadMediaType::from_magic_bytes(&[]), None);
    // 短到装不下完整魔数：半个 PNG 头、两个字节的 JPEG 头都不是一张图。
    assert_eq!(UploadMediaType::from_magic_bytes(&[0x89, b'P', b'N']), None);
    assert_eq!(UploadMediaType::from_magic_bytes(&[0xFF, 0xD8]), None);
    // RIFF 容器但不是 WEBP：视频（AVI）也以 RIFF 开头，不能按图片受理。
    let mut avi = b"RIFF".to_vec();
    avi.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
    avi.extend_from_slice(b"AVI ");
    assert_eq!(UploadMediaType::from_magic_bytes(&avi), None);
    // RIFF 头不足 12 字节：长度读不全，不能凭前四字节判成 WebP。
    assert_eq!(UploadMediaType::from_magic_bytes(b"RIFFWEBP"), None);
    // 纯文本与声明成图片的假字节。
    assert_eq!(UploadMediaType::from_magic_bytes(b"not an image"), None);
}

#[test]
fn canonical_mime_and_extension_are_reverse_mapped() {
    assert_eq!(UploadMediaType::Jpeg.canonical_mime(), "image/jpeg");
    assert_eq!(UploadMediaType::Png.canonical_mime(), "image/png");
    assert_eq!(UploadMediaType::Webp.canonical_mime(), "image/webp");
    assert_eq!(UploadMediaType::Jpeg.object_key_extension(), "jpg");
    assert_eq!(UploadMediaType::Png.object_key_extension(), "png");
    assert_eq!(UploadMediaType::Webp.object_key_extension(), "webp");
    assert_eq!(UploadMediaType::ALL.len(), 3);
}

#[test]
fn single_file_limit_is_strictly_less() {
    assert!(within_single_file_limit(0));
    assert!(within_single_file_limit(MAX_UPLOAD_BYTES - 1));
    assert!(!within_single_file_limit(MAX_UPLOAD_BYTES));
    assert!(!within_single_file_limit(MAX_UPLOAD_BYTES + 1));
}

#[test]
fn object_key_carries_utc_date_identifier_and_extension() {
    let written_at = Utc
        .with_ymd_and_hms(2026, 8, 12, 10, 30, 0)
        .single()
        .expect("a valid UTC instant");
    let identifier = Uuid::parse_str("6f1d0f0e-0000-4000-8000-000000000001").expect("a uuid");
    assert_eq!(
        object_key(written_at, identifier, UploadMediaType::Jpeg),
        "reference-media/2026-08-12/6f1d0f0e-0000-4000-8000-000000000001.jpg"
    );
    // 日期取 UTC，不看调用方本地时区：同一时刻换一种表示仍是同一个键。
    let offset = DateTime::parse_from_rfc3339("2026-08-12T18:30:00+08:00").expect("a timestamp");
    assert_eq!(
        object_key(
            offset.with_timezone(&Utc),
            identifier,
            UploadMediaType::Jpeg
        ),
        "reference-media/2026-08-12/6f1d0f0e-0000-4000-8000-000000000001.jpg"
    );
}

#[test]
fn new_object_key_is_unique_and_keeps_prefix_and_extension() {
    let first = new_object_key(UploadMediaType::Webp);
    let second = new_object_key(UploadMediaType::Webp);
    assert!(first.starts_with("reference-media/"));
    assert!(first.ends_with(".webp"));
    assert_ne!(first, second, "each upload gets its own random object key");
}
