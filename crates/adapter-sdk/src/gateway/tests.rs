use super::*;
use bytes::Bytes;

#[test]
fn raw_image_values_are_classified_into_the_three_shapes() {
    assert!(matches!(
        InputImage::from_raw("https://example.invalid/a.png".to_owned()),
        Ok(InputImage::Url(_))
    ));
    assert!(matches!(
        InputImage::from_raw("data:image/png;base64,AAAA".to_owned()),
        Ok(InputImage::DataUrl(_))
    ));
    assert!(InputImage::from_raw("not-an-image".to_owned()).is_err());
}

#[test]
fn a_data_url_borrows_the_original_and_bytes_are_encoded_once() {
    let url = InputImage::from_raw("https://example.invalid/a.png".to_owned()).expect("url");
    assert_eq!(
        url.to_data_url().expect("url passes through"),
        "https://example.invalid/a.png"
    );
    let data_url = InputImage::from_raw("data:image/png;base64,AAAA".to_owned()).expect("data url");
    assert_eq!(
        data_url.to_data_url().expect("data url passes through"),
        "data:image/png;base64,AAAA"
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
fn bytes_and_data_urls_decode_but_a_public_url_has_no_bytes() {
    let data_url = InputImage::from_raw("data:image/png;base64,AAAA".to_owned()).expect("data url");
    let decoded = data_url.decoded().expect("a data url decodes in place");
    assert_eq!(decoded.media_type, "image/png");
    assert_eq!(decoded.bytes.as_ref(), &[0, 0, 0]);

    let bytes = InputImage::Bytes(DecodedImage {
        media_type: "image/jpeg".to_owned(),
        bytes: Bytes::from_static(&[1, 2, 3]),
    });
    assert_eq!(
        bytes.decoded().expect("bytes borrow").bytes.as_ref(),
        &[1, 2, 3]
    );

    let url = InputImage::from_raw("https://example.invalid/a.png".to_owned()).expect("url");
    assert!(url.decoded().is_err(), "a public url carries no bytes");
}
