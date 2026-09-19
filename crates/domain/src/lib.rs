use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::{Display, Formatter};
use thiserror::Error;
use uuid::Uuid;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

id_type!(AccountId);
id_type!(AssetId);
id_type!(AttemptId);
id_type!(ChannelId);
id_type!(JobId);
id_type!(OfferingId);
id_type!(PricePlanId);
id_type!(RuntimeRevisionId);
id_type!(VendorModelId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageBranch {
    PromptOnly,
    ImageConditioned,
    Masked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Accepted,
    Leased,
    Submitting,
    Succeeded,
    Failed,
    ReconciliationRequired,
    Canceled,
}

impl JobState {
    pub fn transition(self, next: Self) -> Result<Self, DomainError> {
        let allowed = matches!(
            (self, next),
            (Self::Accepted, Self::Leased)
                | (Self::Accepted, Self::Canceled)
                | (Self::Leased, Self::Submitting)
                | (Self::Leased, Self::Accepted)
                | (Self::Leased, Self::Failed)
                | (Self::Submitting, Self::Succeeded)
                | (Self::Submitting, Self::Failed)
                | (Self::Submitting, Self::ReconciliationRequired)
                | (Self::ReconciliationRequired, Self::Failed)
        );
        if allowed {
            Ok(next)
        } else {
            Err(DomainError::InvalidStateTransition {
                from: self,
                to: next,
            })
        }
    }

    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Canceled)
    }
}

impl Display for JobState {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Accepted => "accepted",
            Self::Leased => "leased",
            Self::Submitting => "submitting",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::ReconciliationRequired => "reconciliation_required",
            Self::Canceled => "canceled",
        };
        formatter.write_str(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetBinding {
    pub native_parameter_path: String,
    pub asset_id: AssetId,
    pub position: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateImageGeneration {
    pub account_id: AccountId,
    pub native_model_id: String,
    pub native_parameters: Value,
    #[serde(default)]
    pub asset_bindings: Vec<AssetBinding>,
    pub idempotency_key: String,
    pub max_cost_microusd: u64,
}

impl CreateImageGeneration {
    pub fn branch(&self) -> Result<ImageBranch, DomainError> {
        let has_image = self.asset_bindings.iter().any(|binding| {
            binding.native_parameter_path == "/image"
                || binding.native_parameter_path.starts_with("/images/")
        });
        let has_mask = self
            .asset_bindings
            .iter()
            .any(|binding| binding.native_parameter_path == "/mask");
        match (has_image, has_mask) {
            (false, false) => Ok(ImageBranch::PromptOnly),
            (true, false) => Ok(ImageBranch::ImageConditioned),
            (true, true) => Ok(ImageBranch::Masked),
            (false, true) => Err(DomainError::MaskRequiresImage),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub input_text_tokens: u64,
    pub input_image_tokens: u64,
    pub output_tokens: u64,
    pub output_text_tokens: u64,
    pub output_image_tokens: u64,
    pub total_tokens: u64,
}

impl TokenUsage {
    pub fn validate(&self) -> Result<(), DomainError> {
        let input = self
            .input_text_tokens
            .checked_add(self.input_image_tokens)
            .ok_or(DomainError::ArithmeticOverflow)?;
        let output = self
            .output_text_tokens
            .checked_add(self.output_image_tokens)
            .ok_or(DomainError::ArithmeticOverflow)?;
        let total = self
            .input_tokens
            .checked_add(self.output_tokens)
            .ok_or(DomainError::ArithmeticOverflow)?;
        if input != self.input_tokens || output != self.output_tokens || total != self.total_tokens
        {
            return Err(DomainError::InconsistentUsage);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeteringEvidence {
    pub attempt_id: AttemptId,
    pub provider_response_digest: String,
    pub usage: TokenUsage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceRates {
    pub currency: String,
    pub text_input_microusd_per_million: u64,
    pub image_input_microusd_per_million: u64,
    pub text_output_microusd_per_million: u64,
    pub image_output_microusd_per_million: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceSnapshot {
    pub price_plan_id: PricePlanId,
    pub rates: PriceRates,
    pub captured_at: DateTime<Utc>,
}

impl PriceSnapshot {
    pub fn charge_microusd(&self, usage: &TokenUsage) -> Result<u64, DomainError> {
        usage.validate()?;
        let terms = [
            (
                usage.input_text_tokens,
                self.rates.text_input_microusd_per_million,
            ),
            (
                usage.input_image_tokens,
                self.rates.image_input_microusd_per_million,
            ),
            (
                usage.output_text_tokens,
                self.rates.text_output_microusd_per_million,
            ),
            (
                usage.output_image_tokens,
                self.rates.image_output_microusd_per_million,
            ),
        ];
        let numerator = terms.into_iter().try_fold(0_u128, |sum, (tokens, rate)| {
            let term = u128::from(tokens)
                .checked_mul(u128::from(rate))
                .ok_or(DomainError::ArithmeticOverflow)?;
            sum.checked_add(term).ok_or(DomainError::ArithmeticOverflow)
        })?;
        let rounded_up = numerator.div_ceil(1_000_000);
        u64::try_from(rounded_up).map_err(|_| DomainError::ArithmeticOverflow)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublishedOffering {
    pub runtime_revision_id: RuntimeRevisionId,
    pub vendor_model_id: VendorModelId,
    pub offering_id: OfferingId,
    pub channel_id: ChannelId,
    pub native_model_id: String,
    pub native_revision: String,
    pub capability_schema: Value,
    pub restrictions: Value,
    pub adapter_key: String,
    pub provider_model_id: String,
    pub provider_kind: String,
    pub base_url: String,
    pub credential_env: String,
    pub price_snapshot: PriceSnapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerationJob {
    pub id: JobId,
    pub account_id: AccountId,
    pub state: JobState,
    pub branch: ImageBranch,
    pub native_model_id: String,
    pub native_parameters: Value,
    pub asset_bindings: Vec<AssetBinding>,
    pub offering: PublishedOffering,
    pub idempotency_key: String,
    pub request_hash: String,
    pub max_cost_microusd: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DomainError {
    #[error("mask requires an image asset")]
    MaskRequiresImage,
    #[error("invalid job transition from {from} to {to}")]
    InvalidStateTransition { from: JobState, to: JobState },
    #[error("provider usage fields are inconsistent")]
    InconsistentUsage,
    #[error("arithmetic overflow")]
    ArithmeticOverflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage() -> TokenUsage {
        TokenUsage {
            input_tokens: 1051,
            input_text_tokens: 27,
            input_image_tokens: 1024,
            output_tokens: 196,
            output_text_tokens: 0,
            output_image_tokens: 196,
            total_tokens: 1247,
        }
    }

    #[test]
    fn derives_masked_branch_from_asset_bindings() {
        let command = CreateImageGeneration {
            account_id: AccountId::new(),
            native_model_id: "gpt-image-2".to_owned(),
            native_parameters: serde_json::json!({"prompt": "test"}),
            asset_bindings: vec![
                AssetBinding {
                    native_parameter_path: "/image".to_owned(),
                    asset_id: AssetId::new(),
                    position: 0,
                },
                AssetBinding {
                    native_parameter_path: "/mask".to_owned(),
                    asset_id: AssetId::new(),
                    position: 0,
                },
            ],
            idempotency_key: "test-key".to_owned(),
            max_cost_microusd: 20_000,
        };
        assert_eq!(command.branch(), Ok(ImageBranch::Masked));
    }

    #[test]
    fn refuses_mask_without_image() {
        let command = CreateImageGeneration {
            account_id: AccountId::new(),
            native_model_id: "gpt-image-2".to_owned(),
            native_parameters: serde_json::json!({"prompt": "test"}),
            asset_bindings: vec![AssetBinding {
                native_parameter_path: "/mask".to_owned(),
                asset_id: AssetId::new(),
                position: 0,
            }],
            idempotency_key: "test-key".to_owned(),
            max_cost_microusd: 20_000,
        };
        assert_eq!(command.branch(), Err(DomainError::MaskRequiresImage));
    }

    #[test]
    fn calculates_edit_charge_from_verified_usage() {
        let snapshot = PriceSnapshot {
            price_plan_id: PricePlanId::new(),
            rates: PriceRates {
                currency: "USD".to_owned(),
                text_input_microusd_per_million: 5_000_000,
                image_input_microusd_per_million: 8_000_000,
                text_output_microusd_per_million: 10_000_000,
                image_output_microusd_per_million: 30_000_000,
            },
            captured_at: Utc::now(),
        };
        assert_eq!(snapshot.charge_microusd(&usage()), Ok(14_207));
    }

    #[test]
    fn rejects_inconsistent_usage() {
        let mut invalid = usage();
        invalid.total_tokens = 1;
        assert_eq!(invalid.validate(), Err(DomainError::InconsistentUsage));
    }

    #[test]
    fn rejects_unsafe_state_jump() {
        assert!(matches!(
            JobState::Accepted.transition(JobState::Succeeded),
            Err(DomainError::InvalidStateTransition { .. })
        ));
    }

    #[test]
    fn reconciliation_cannot_be_promoted_to_success_without_evidence() {
        assert!(matches!(
            JobState::ReconciliationRequired.transition(JobState::Succeeded),
            Err(DomainError::InvalidStateTransition { .. })
        ));
        assert_eq!(
            JobState::ReconciliationRequired.transition(JobState::Failed),
            Ok(JobState::Failed)
        );
    }

    #[test]
    fn allows_lease_failure_before_provider_submission() {
        assert_eq!(
            JobState::Leased.transition(JobState::Failed),
            Ok(JobState::Failed)
        );
    }
}
