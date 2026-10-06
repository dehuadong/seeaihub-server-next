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

/// 线上名字带点表示落在容器里：同一容器多次写入合并，容器位置已是普通值时明确失败。
#[test]
fn a_dotted_wire_name_lands_in_its_container() {
    let mut body = Map::new();
    insert_wire_parameter(&mut body, "model", serde_json::json!("m")).expect("top level");
    insert_wire_parameter(&mut body, "extra.quality", serde_json::json!("high"))
        .expect("container");
    insert_wire_parameter(
        &mut body,
        "extra.background",
        serde_json::json!("transparent"),
    )
    .expect("merged");
    assert_eq!(body["model"], serde_json::json!("m"));
    assert_eq!(
        body["extra"],
        serde_json::json!({"quality": "high", "background": "transparent"})
    );
    let mut conflicted = Map::new();
    insert_wire_parameter(&mut conflicted, "extra", serde_json::json!("plain"))
        .expect("plain value");
    assert!(
        insert_wire_parameter(&mut conflicted, "extra.quality", serde_json::json!("high")).is_err(),
        "容器位置上已经有普通取值时必须明确失败"
    );
}
