use super::*;
use serde_json::json;

fn key(byte: u8) -> Vec<u8> {
    vec![byte; FINGERPRINT_KEY_LEN]
}

fn input<'a>(
    parameters: &'a Value,
    images: &'a [String],
    mask: Option<&'a str>,
) -> RequestFingerprintInput<'a> {
    RequestFingerprintInput {
        endpoint: "/v1/images/generations",
        gateway_model: "gpt-image-2.5-flare",
        parameters,
        reference_images: images,
        mask,
        n: 1,
    }
}

#[test]
fn the_same_key_and_input_yield_the_same_fingerprint() {
    let parameters = json!({"prompt": "a cat", "size": "1024x1024"});
    let images: Vec<String> = Vec::new();
    let first = input(&parameters, &images, None)
        .fingerprint(&key(7))
        .expect("digest");
    let second = input(&parameters, &images, None)
        .fingerprint(&key(7))
        .expect("digest");
    assert_eq!(first, second);
    assert_eq!(first.len(), 64, "HMAC-SHA256 renders as 64 hex characters");
}

#[test]
fn a_different_key_changes_the_fingerprint() {
    let parameters = json!({"prompt": "a cat"});
    let images: Vec<String> = Vec::new();
    let first = input(&parameters, &images, None)
        .fingerprint(&key(7))
        .expect("digest");
    let other = input(&parameters, &images, None)
        .fingerprint(&key(8))
        .expect("digest");
    assert_ne!(first, other);
}

#[test]
fn object_key_order_does_not_change_the_fingerprint_but_array_order_does() {
    let first = json!({"prompt": "a cat", "size": "1024x1024"});
    let reordered = json!({"size": "1024x1024", "prompt": "a cat"});
    let images: Vec<String> = Vec::new();
    assert_eq!(
        input(&first, &images, None)
            .fingerprint(&key(9))
            .expect("digest"),
        input(&reordered, &images, None)
            .fingerprint(&key(9))
            .expect("digest"),
    );
    let ordered = json!({"tags": ["a", "b"]});
    let reversed = json!({"tags": ["b", "a"]});
    assert_ne!(
        input(&ordered, &images, None)
            .fingerprint(&key(9))
            .expect("digest"),
        input(&reversed, &images, None)
            .fingerprint(&key(9))
            .expect("digest"),
    );
}

#[test]
fn each_recognized_field_changes_the_fingerprint() {
    let base = json!({"prompt": "a cat"});
    let images: Vec<String> = Vec::new();
    let baseline = input(&base, &images, None)
        .fingerprint(&key(3))
        .expect("digest");

    let other_parameters = json!({"prompt": "a dog"});
    assert_ne!(
        baseline,
        input(&other_parameters, &images, None)
            .fingerprint(&key(3))
            .expect("digest")
    );

    let with_image = vec!["https://example.invalid/a.png".to_owned()];
    assert_ne!(
        baseline,
        input(&base, &with_image, None)
            .fingerprint(&key(3))
            .expect("digest")
    );

    assert_ne!(
        baseline,
        input(&base, &images, Some("data:image/png;base64,AAAA"))
            .fingerprint(&key(3))
            .expect("digest")
    );

    let mut with_n = input(&base, &images, None);
    with_n.n = 2;
    assert_ne!(baseline, with_n.fingerprint(&key(3)).expect("digest"));

    let mut other_model = input(&base, &images, None);
    other_model.gateway_model = "gpt-image-2.5-sunburst";
    assert_ne!(baseline, other_model.fingerprint(&key(3)).expect("digest"));

    let mut other_endpoint = input(&base, &images, None);
    other_endpoint.endpoint = "/v1/images/edits";
    assert_ne!(
        baseline,
        other_endpoint.fingerprint(&key(3)).expect("digest")
    );
}

#[test]
fn the_idempotency_digest_is_keyless_and_stable() {
    let digest = idempotency_key_digest("order-42");
    assert_eq!(
        digest,
        idempotency_key_digest("order-42"),
        "同一幂等键必须得到同一摘要"
    );
    assert_eq!(digest.len(), 64, "SHA-256 renders as 64 hex characters");
    assert_ne!(digest, idempotency_key_digest("order-43"));
}

#[test]
fn an_unknown_request_key_version_reports_no_fingerprint() {
    let mut request_keys = BTreeMap::new();
    request_keys.insert(1, key(2));
    let keys = RequestFingerprintKeys::new(request_keys, 1).expect("keys");
    let parameters = json!({"prompt": "a cat"});
    let images: Vec<String> = Vec::new();
    assert!(
        keys.request_fingerprint(2, &input(&parameters, &images, None))
            .expect("lookup")
            .is_none()
    );
    assert!(
        keys.request_fingerprint(1, &input(&parameters, &images, None))
            .expect("lookup")
            .is_some()
    );
}

#[test]
fn short_keys_are_rejected() {
    let mut request_keys = BTreeMap::new();
    request_keys.insert(1, vec![0u8; 4]);
    assert!(matches!(
        RequestFingerprintKeys::new(request_keys, 1),
        Err(ApplicationError::Configuration(_))
    ));
}

#[test]
fn a_long_key_is_hashed_before_use_and_image_content_is_compared() {
    let parameters = json!({"prompt": "a cat"});
    let images: Vec<String> = Vec::new();
    let long = vec![0xABu8; 100];
    let digest = input(&parameters, &images, None)
        .fingerprint(&long)
        .expect("a key longer than the HMAC block size must still digest");
    assert_eq!(digest.len(), 64);

    let first = vec!["https://example.invalid/a.png".to_owned()];
    let second = vec!["https://example.invalid/b.png".to_owned()];
    assert_ne!(
        input(&parameters, &first, None)
            .fingerprint(&key(4))
            .expect("digest"),
        input(&parameters, &second, None)
            .fingerprint(&key(4))
            .expect("digest"),
        "different reference image content must change the fingerprint"
    );
}
