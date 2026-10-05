use super::*;
use bytes::Bytes;

#[test]
fn a_public_url_borrows_the_original_and_bytes_are_encoded_once() {
    let url = InputImage::Url("https://example.invalid/a.png".to_owned());
    assert_eq!(
        url.to_data_url().expect("url passes through"),
        "https://example.invalid/a.png"
    );
    let bytes = InputImage::Bytes(DecodedImage {
        media_type: "image/png".to_owned(),
        bytes: Bytes::from_static(&[0x89]),
    });
    assert_eq!(
        bytes.to_data_url().expect("bytes encode once"),
        "data:image/png;base64,iQ=="
    );
}

#[test]
fn bytes_decode_but_a_public_url_has_no_bytes() {
    let bytes = InputImage::Bytes(DecodedImage {
        media_type: "image/jpeg".to_owned(),
        bytes: Bytes::from_static(&[1, 2, 3]),
    });
    assert_eq!(
        bytes.decoded().expect("bytes borrow").bytes.as_ref(),
        &[1, 2, 3]
    );

    let url = InputImage::Url("https://example.invalid/a.png".to_owned());
    assert!(url.decoded().is_err(), "a public url carries no bytes");
}
