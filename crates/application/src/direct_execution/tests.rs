use super::*;
use seeai_domain::{ChannelId, OfferingId, RuntimeRevisionId, VendorModelId};
use serde_json::json;

fn price_snapshot() -> seeai_domain::PriceSnapshot {
    serde_json::from_value(json!({
        "captured_at": "2026-10-03T00:00:00Z",
        "formula": "token_rates",
        "cost_currency": "USD",
        "rates": {
            "currency": "USD",
            "text_input_microusd_per_million": 5_000_000,
            "image_input_microusd_per_million": 8_000_000,
            "text_output_microusd_per_million": 10_000_000,
            "image_output_microusd_per_million": 30_000_000
        }
    }))
    .expect("a frozen price snapshot")
}

fn offering(carrier: Value) -> PublishedOffering {
    PublishedOffering {
        runtime_revision_id: RuntimeRevisionId::new(),
        vendor_model_id: VendorModelId::new(),
        offering_id: OfferingId::new(),
        channel_id: ChannelId::new(),
        gateway_model: "gw".to_owned(),
        native_revision: "v1".to_owned(),
        capability_schema: carrier.clone(),
        carrier_schema: carrier,
        parameter_mapping: json!({}),
        restrictions: json!({}),
        adapter_key: "fake".to_owned(),
        provider_model_id: "pm".to_owned(),
        provider_kind: "Fake".to_owned(),
        base_url: "http://127.0.0.1:9".to_owned(),
        credential_env: "FAKE".to_owned(),
        price_snapshot: price_snapshot(),
    }
}

fn request(reference_images: Vec<InputImage>, mask: Option<InputImage>) -> DirectExecutionRequest {
    DirectExecutionRequest {
        account_id: AccountId::new(),
        model: "gw".to_owned(),
        endpoint: "/v1/images/edits".to_owned(),
        native_parameters: RequestParameters::try_from(json!({"prompt": "draw"}))
            .expect("the fixture parameters are within the request structure limits"),
        reference_images,
        mask,
        idempotency_key: "request-0001".to_owned(),
    }
}

#[test]
fn retry_safety_maps_to_the_single_disposition() {
    assert_eq!(
        failure_disposition_for(RetrySafety::SafeBeforeAcceptance),
        FailureDisposition::SafeRetry
    );
    assert_eq!(
        failure_disposition_for(RetrySafety::NotRetryable),
        FailureDisposition::DeterminedFailure
    );
    assert_eq!(
        failure_disposition_for(RetrySafety::AcceptanceUnknown),
        FailureDisposition::Unknown
    );
}

#[test]
fn image_sites_read_the_wire_shape_from_the_carrier_schema() {
    let carrier = json!({
        "type": "object",
        "properties": {
            "prompt": {"type": "string"},
            "image_urls": {"type": "array", "items": {"type": "string"}},
            "mask": {"type": "string"}
        }
    });
    let sites = image_sites_for(&carrier, ImageBranch::Masked);
    let reference = sites.reference.as_ref().expect("a reference site");
    assert_eq!(reference.parameter, "image_urls");
    assert_eq!(reference.shape, ImageValueShape::Array);
    let mask = sites.mask.as_ref().expect("a mask site");
    assert_eq!(mask.parameter, "mask");
    assert_eq!(mask.shape, ImageValueShape::Scalar);
    // 文生图不装载任何图：名单为空，两个位都不存在。
    let none = image_sites_for(&carrier, ImageBranch::PromptOnly);
    assert!(none.reference.is_none() && none.mask.is_none());
}

#[test]
fn gateway_input_hoists_images_out_of_native_parameters() {
    let offering = offering(json!({
        "type": "object",
        "properties": {
            "prompt": {"type": "string"},
            "image": {"type": "string"},
            "mask": {"type": "string"}
        }
    }));
    let request = request(
        vec![InputImage::url("https://img.example/a.png")],
        Some(InputImage::url("https://img.example/mask.png")),
    );
    // 这一份是承载准备后的参数面：图片位上的取值已经由 place_image_inputs 装载。
    let prepared = json!({
        "prompt": "draw",
        "image": "https://img.example/a.png",
        "mask": "https://img.example/mask.png"
    });
    let input = build_gateway_input(&offering, ImageBranch::Masked, prepared, &request)
        .expect("the gateway input");
    assert_eq!(input.native_parameters, json!({"prompt": "draw"}));
    assert_eq!(input.reference_images.len(), 1);
    assert!(input.mask.is_some());
    assert_eq!(
        input
            .image_sites
            .reference
            .as_ref()
            .map(|site| site.parameter.as_str()),
        Some("image")
    );
    assert_eq!(
        input
            .image_sites
            .mask
            .as_ref()
            .map(|site| site.parameter.as_str()),
        Some("mask")
    );
    assert_eq!(input.cost_currency, "USD");
}

#[test]
fn a_replay_projects_to_the_four_spec_four_outcomes() {
    let replay = |stage, code: Option<&str>| ExecutionReplay {
        job_id: JobId::new(),
        stage,
        error_code: code.map(str::to_owned),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let retry_after = Duration::from_secs(3);
    assert!(matches!(
        project_replay(replay(ExecutionStage::Admitted, None), retry_after),
        DirectExecutionError::RequestInProgress { .. }
    ));
    assert!(matches!(
        project_replay(replay(ExecutionStage::Executing, None), retry_after),
        DirectExecutionError::RequestInProgress { .. }
    ));
    assert!(matches!(
        project_replay(replay(ExecutionStage::Succeeded, None), retry_after),
        DirectExecutionError::ResultNotRetained
    ));
    let DirectExecutionError::OriginalFailure { code } = project_replay(
        replay(ExecutionStage::Failed, Some("content_rejected")),
        retry_after,
    ) else {
        panic!("a determined failure replays its original public code");
    };
    assert_eq!(code, PublicErrorCode::ContentRejected);
    assert!(matches!(
        project_replay(
            replay(ExecutionStage::ReconciliationRequired, None),
            retry_after
        ),
        DirectExecutionError::OutcomeUnknown
    ));
}
