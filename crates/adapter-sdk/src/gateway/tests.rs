use super::*;

#[test]
fn an_input_image_carries_the_public_url() {
    let url = InputImage::url("https://example.invalid/a.png");
    assert_eq!(url.as_str(), "https://example.invalid/a.png");
    // 图片值不进日志或 Debug（RFC 0017 §4）：只打印长度。
    assert_eq!(
        format!("{url:?}"),
        format!("InputImage({} chars)", url.as_str().len())
    );
}
