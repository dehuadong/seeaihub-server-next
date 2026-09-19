use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use seeai_adapter_sdk::{
    AdapterDescriptor, AdapterError, ImageAdapter, PreparedImageRequest, ProviderCredential,
    ProviderSuccess, ResolvedAsset, RetrySafety,
};
use seeai_domain::{
    AccountId, AssetBinding, AssetId, AttemptId, CreateImageGeneration, GenerationJob, ImageBranch,
    JobId, JobState, MeteringEvidence, PriceRates, PublishedOffering,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishRuntimeCommand {
    pub vendor_id: String,
    pub native_model_id: String,
    pub native_revision: String,
    pub capability_schema: Value,
    #[serde(default = "empty_object")]
    pub restrictions: Value,
    pub provider_kind: String,
    pub adapter_key: String,
    pub provider_model_id: String,
    pub base_url: String,
    pub credential_env: String,
    pub rates: PriceRates,
    pub price_source_url: String,
    pub actor: String,
}

fn empty_object() -> Value {
    Value::Object(Map::new())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetRecord {
    pub id: AssetId,
    pub account_id: AccountId,
    pub role: String,
    pub object_key: String,
    pub media_type: String,
    pub byte_count: u64,
    pub width: u32,
    pub height: u32,
    pub sha256: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputAsset {
    pub record: AssetRecord,
    #[serde(skip)]
    pub bytes: Bytes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobView {
    pub id: JobId,
    pub account_id: AccountId,
    pub state: String,
    pub branch: ImageBranch,
    pub native_model_id: String,
    pub result_asset_ids: Vec<AssetId>,
    pub error_code: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ClaimedJob {
    pub job: GenerationJob,
    pub lease_owner: String,
    pub lease_expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct AttemptFailure {
    pub code: String,
    pub message: String,
    pub trace_id: Option<String>,
    pub target_state: JobState,
    pub hold_disposition: HoldDisposition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldDisposition {
    Release,
    RetainForReconciliation,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LeaseRecovery {
    pub returned_to_queue: u64,
    pub sent_to_reconciliation: u64,
}

#[derive(Debug, Clone)]
pub struct CompleteJob {
    pub job_id: JobId,
    pub worker_id: String,
    pub attempt_id: AttemptId,
    pub outputs: Vec<AssetRecord>,
    pub evidence: MeteringEvidence,
    pub charge_microusd: u64,
}

#[derive(Debug, Clone)]
pub struct RefundReconciliationCommand {
    pub job_id: JobId,
    pub note: String,
    pub business_key: String,
    pub actor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationCaseView {
    pub id: Uuid,
    pub job_id: JobId,
    pub attempt_id: AttemptId,
    pub account_id: AccountId,
    pub reason: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Error)]
pub enum ApplicationError {
    #[error("validation failed: {0}")]
    Validation(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("insufficient balance")]
    InsufficientBalance,
    #[error("configuration error: {0}")]
    Configuration(String),
    #[error("persistence error: {0}")]
    Persistence(String),
    #[error("object storage error: {0}")]
    ObjectStorage(String),
    #[error("provider result requires reconciliation: {0}")]
    Reconciliation(String),
}

#[async_trait]
pub trait HubRepository: Send + Sync {
    async fn publish_runtime(
        &self,
        command: PublishRuntimeCommand,
    ) -> Result<PublishedOffering, ApplicationError>;

    async fn active_offering(
        &self,
        native_model_id: &str,
    ) -> Result<PublishedOffering, ApplicationError>;

    async fn create_account(
        &self,
        account_id: AccountId,
        initial_credit_microusd: u64,
        actor: &str,
    ) -> Result<(), ApplicationError>;

    async fn credit_account(
        &self,
        account_id: AccountId,
        amount_microusd: u64,
        business_key: &str,
        actor: &str,
    ) -> Result<(), ApplicationError>;

    async fn create_api_key(
        &self,
        account_id: AccountId,
        label: &str,
        key_hash: &str,
        actor: &str,
    ) -> Result<(), ApplicationError>;

    async fn account_for_api_key(&self, key_hash: &str) -> Result<AccountId, ApplicationError>;

    async fn insert_asset(&self, asset: AssetRecord) -> Result<(), ApplicationError>;

    async fn get_asset(
        &self,
        account_id: AccountId,
        asset_id: AssetId,
    ) -> Result<AssetRecord, ApplicationError>;

    async fn create_job(
        &self,
        command: CreateImageGeneration,
        branch: ImageBranch,
        offering: PublishedOffering,
        request_hash: String,
    ) -> Result<GenerationJob, ApplicationError>;

    async fn get_job(
        &self,
        account_id: AccountId,
        job_id: JobId,
    ) -> Result<JobView, ApplicationError>;

    async fn claim_next_job(
        &self,
        worker_id: &str,
        lease_duration: ChronoDuration,
    ) -> Result<Option<ClaimedJob>, ApplicationError>;

    async fn recover_expired_leases(&self) -> Result<LeaseRecovery, ApplicationError>;

    async fn begin_attempt(
        &self,
        job_id: JobId,
        worker_id: &str,
        attempt_id: AttemptId,
        request_digest: &str,
    ) -> Result<(), ApplicationError>;

    async fn renew_lease(
        &self,
        job_id: JobId,
        worker_id: &str,
        lease_duration: ChronoDuration,
    ) -> Result<(), ApplicationError>;

    async fn complete_job(&self, completion: CompleteJob) -> Result<(), ApplicationError>;

    async fn fail_job(
        &self,
        job_id: JobId,
        worker_id: &str,
        attempt_id: Option<AttemptId>,
        failure: AttemptFailure,
    ) -> Result<(), ApplicationError>;

    async fn list_open_reconciliation_cases(
        &self,
    ) -> Result<Vec<ReconciliationCaseView>, ApplicationError>;

    async fn refund_reconciliation(
        &self,
        command: RefundReconciliationCommand,
    ) -> Result<(), ApplicationError>;
}

#[derive(Clone)]
pub struct ReconciliationService {
    repository: Arc<dyn HubRepository>,
}

impl ReconciliationService {
    #[must_use]
    pub fn new(repository: Arc<dyn HubRepository>) -> Self {
        Self { repository }
    }

    pub async fn list_open(&self) -> Result<Vec<ReconciliationCaseView>, ApplicationError> {
        self.repository.list_open_reconciliation_cases().await
    }

    pub async fn refund(
        &self,
        command: RefundReconciliationCommand,
    ) -> Result<(), ApplicationError> {
        if command.note.trim().is_empty() || command.business_key.trim().is_empty() {
            return Err(ApplicationError::Validation(
                "reconciliation note and business_key are required".to_owned(),
            ));
        }
        self.repository.refund_reconciliation(command).await
    }
}

#[derive(Clone)]
pub struct IdentityService {
    repository: Arc<dyn HubRepository>,
}

impl IdentityService {
    #[must_use]
    pub fn new(repository: Arc<dyn HubRepository>) -> Self {
        Self { repository }
    }

    pub async fn issue_api_key(
        &self,
        account_id: AccountId,
        label: &str,
        actor: &str,
    ) -> Result<String, ApplicationError> {
        if label.trim().is_empty() {
            return Err(ApplicationError::Validation(
                "api key label must not be empty".to_owned(),
            ));
        }
        let first = Uuid::new_v4().simple();
        let second = Uuid::new_v4().simple();
        let plaintext = format!("sk_seeai_{first}{second}");
        let key_hash = sha256_hex(plaintext.as_bytes());
        self.repository
            .create_api_key(account_id, label, &key_hash, actor)
            .await?;
        Ok(plaintext)
    }

    pub async fn authenticate(&self, plaintext: &str) -> Result<AccountId, ApplicationError> {
        if !plaintext.starts_with("sk_seeai_") {
            return Err(ApplicationError::NotFound("api key".to_owned()));
        }
        self.repository
            .account_for_api_key(&sha256_hex(plaintext.as_bytes()))
            .await
    }
}

#[async_trait]
pub trait AssetStore: Send + Sync {
    async fn put(
        &self,
        object_key: &str,
        bytes: Bytes,
        media_type: &str,
    ) -> Result<(), ApplicationError>;

    async fn get(&self, object_key: &str) -> Result<Bytes, ApplicationError>;
}

pub trait AdapterFactory: Send + Sync {
    fn descriptor(&self, adapter_key: &str) -> Option<AdapterDescriptor>;

    fn validate_publication(
        &self,
        adapter_key: &str,
        capability_schema: &Value,
        restrictions: &Value,
    ) -> Result<(), String>;

    fn create(
        &self,
        adapter_key: &str,
        base_url: &str,
        timeout: Duration,
    ) -> Result<Arc<dyn ImageAdapter>, ApplicationError>;
}

pub trait CredentialProvider: Send + Sync {
    fn resolve(&self, reference: &str) -> Result<ProviderCredential, ApplicationError>;
}

#[derive(Clone)]
pub struct RuntimeService {
    repository: Arc<dyn HubRepository>,
    adapters: Arc<dyn AdapterFactory>,
}

impl RuntimeService {
    #[must_use]
    pub fn new(repository: Arc<dyn HubRepository>, adapters: Arc<dyn AdapterFactory>) -> Self {
        Self {
            repository,
            adapters,
        }
    }

    pub async fn publish(
        &self,
        mut command: PublishRuntimeCommand,
    ) -> Result<PublishedOffering, ApplicationError> {
        for (name, value) in [
            ("vendor_id", command.vendor_id.as_str()),
            ("native_model_id", command.native_model_id.as_str()),
            ("native_revision", command.native_revision.as_str()),
            ("provider_kind", command.provider_kind.as_str()),
            ("adapter_key", command.adapter_key.as_str()),
            ("provider_model_id", command.provider_model_id.as_str()),
            ("actor", command.actor.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(ApplicationError::Validation(format!(
                    "{name} must not be empty"
                )));
            }
        }
        jsonschema::validator_for(&command.capability_schema)
            .map_err(|error| ApplicationError::Validation(error.to_string()))?;
        if command
            .capability_schema
            .pointer("/properties/model/const")
            .and_then(Value::as_str)
            != Some(command.native_model_id.as_str())
        {
            return Err(ApplicationError::Validation(
                "capability_schema model.const must equal native_model_id".to_owned(),
            ));
        }
        if command
            .capability_schema
            .get("type")
            .and_then(Value::as_str)
            != Some("object")
            || command
                .capability_schema
                .get("additionalProperties")
                .and_then(Value::as_bool)
                != Some(false)
        {
            return Err(ApplicationError::Validation(
                "capability_schema must be a closed object schema".to_owned(),
            ));
        }
        let mut base_url = command.base_url.trim().trim_end_matches('/').to_owned();
        if base_url.is_empty() {
            return Err(ApplicationError::Validation(
                "base_url must not be empty".to_owned(),
            ));
        }
        let parsed = url::Url::parse(&base_url)
            .map_err(|error| ApplicationError::Validation(error.to_string()))?;
        if parsed.scheme() != "https" && parsed.host_str() != Some("127.0.0.1") {
            return Err(ApplicationError::Validation(
                "provider base_url must use https outside local development".to_owned(),
            ));
        }
        if !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(ApplicationError::Validation(
                "provider base_url must not contain credentials, query, or fragment".to_owned(),
            ));
        }
        base_url.truncate(base_url.trim_end_matches('/').len());
        command.base_url = base_url;
        if command.credential_env.trim().is_empty() {
            return Err(ApplicationError::Validation(
                "credential_env must not be empty".to_owned(),
            ));
        }
        if command.rates.currency != "USD" {
            return Err(ApplicationError::Validation(
                "price currency must be USD for microUSD rates".to_owned(),
            ));
        }
        let price_source = url::Url::parse(&command.price_source_url)
            .map_err(|error| ApplicationError::Validation(error.to_string()))?;
        if price_source.scheme() != "https" {
            return Err(ApplicationError::Validation(
                "price_source_url must use https".to_owned(),
            ));
        }
        let descriptor = self
            .adapters
            .descriptor(&command.adapter_key)
            .ok_or_else(|| {
                ApplicationError::Validation(format!("unknown adapter {}", command.adapter_key))
            })?;
        validate_adapter_compatibility(&command, &descriptor)?;
        self.adapters
            .validate_publication(
                &command.adapter_key,
                &command.capability_schema,
                &command.restrictions,
            )
            .map_err(ApplicationError::Validation)?;
        self.adapters.create(
            &command.adapter_key,
            &command.base_url,
            Duration::from_secs(1),
        )?;
        self.repository.publish_runtime(command).await
    }
}

fn validate_adapter_compatibility(
    command: &PublishRuntimeCommand,
    descriptor: &AdapterDescriptor,
) -> Result<(), ApplicationError> {
    let properties = command
        .capability_schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            ApplicationError::Validation("capability_schema.properties is required".to_owned())
        })?;
    for parameter in properties.keys() {
        if !descriptor
            .supported_top_level_parameters
            .contains(&parameter.as_str())
        {
            return Err(ApplicationError::Validation(format!(
                "adapter {} does not support native parameter {parameter}",
                descriptor.key
            )));
        }
    }
    if let Some(extra) = properties
        .get("extra")
        .and_then(|value| value.get("properties"))
        .and_then(Value::as_object)
    {
        for parameter in extra.keys() {
            if !descriptor
                .supported_extra_parameters
                .contains(&parameter.as_str())
            {
                return Err(ApplicationError::Validation(format!(
                    "adapter {} does not support native parameter extra.{parameter}",
                    descriptor.key
                )));
            }
        }
    }
    let max_images = command
        .restrictions
        .get("max_images")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if max_images > descriptor.max_images {
        return Err(ApplicationError::Validation(format!(
            "adapter {} supports at most {} image inputs",
            descriptor.key, descriptor.max_images
        )));
    }
    if let Some(branches) = command
        .restrictions
        .get("allowed_branches")
        .and_then(Value::as_array)
    {
        for branch in branches {
            let parsed = match branch.as_str() {
                Some("prompt_only") => ImageBranch::PromptOnly,
                Some("image_conditioned") => ImageBranch::ImageConditioned,
                Some("masked") => ImageBranch::Masked,
                _ => {
                    return Err(ApplicationError::Validation(
                        "restriction contains an unknown image branch".to_owned(),
                    ));
                }
            };
            if !descriptor.supported_branches.contains(&parsed) {
                return Err(ApplicationError::Validation(format!(
                    "adapter {} does not support branch {parsed:?}",
                    descriptor.key
                )));
            }
        }
    }
    Ok(())
}

#[derive(Clone)]
pub struct AssetService {
    repository: Arc<dyn HubRepository>,
    store: Arc<dyn AssetStore>,
}

impl AssetService {
    #[must_use]
    pub fn new(repository: Arc<dyn HubRepository>, store: Arc<dyn AssetStore>) -> Self {
        Self { repository, store }
    }

    pub async fn upload(
        &self,
        account_id: AccountId,
        role: &str,
        media_type: &str,
        bytes: Bytes,
    ) -> Result<AssetRecord, ApplicationError> {
        if !matches!(role, "image" | "mask") {
            return Err(ApplicationError::Validation(
                "asset role must be image or mask".to_owned(),
            ));
        }
        validate_input_media(media_type, &bytes)?;
        if role == "mask" && (media_type != "image/png" || !matches!(bytes.get(25), Some(4 | 6))) {
            return Err(ApplicationError::Validation(
                "mask must be a PNG with an alpha channel".to_owned(),
            ));
        }
        let dimensions = imagesize::blob_size(&bytes)
            .map_err(|error| ApplicationError::Validation(error.to_string()))?;
        let asset_id = AssetId::new();
        let digest = sha256_hex(&bytes);
        let extension = match media_type {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/webp" => "webp",
            _ => {
                return Err(ApplicationError::Validation(format!(
                    "unsupported media type {media_type}"
                )));
            }
        };
        let object_key = format!("inputs/{account_id}/{asset_id}.{extension}");
        self.store
            .put(&object_key, bytes.clone(), media_type)
            .await?;
        let record = AssetRecord {
            id: asset_id,
            account_id,
            role: role.to_owned(),
            object_key,
            media_type: media_type.to_owned(),
            byte_count: u64::try_from(bytes.len())
                .map_err(|_| ApplicationError::Validation("asset is too large".to_owned()))?,
            width: u32::try_from(dimensions.width)
                .map_err(|_| ApplicationError::Validation("asset width is too large".to_owned()))?,
            height: u32::try_from(dimensions.height).map_err(|_| {
                ApplicationError::Validation("asset height is too large".to_owned())
            })?,
            sha256: digest,
            created_at: Utc::now(),
        };
        self.repository.insert_asset(record.clone()).await?;
        Ok(record)
    }

    pub async fn download(
        &self,
        account_id: AccountId,
        asset_id: AssetId,
    ) -> Result<(AssetRecord, Bytes), ApplicationError> {
        let record = self.repository.get_asset(account_id, asset_id).await?;
        let bytes = self.store.get(&record.object_key).await?;
        if sha256_hex(&bytes) != record.sha256 {
            return Err(ApplicationError::ObjectStorage(format!(
                "asset {asset_id} digest mismatch"
            )));
        }
        Ok((record, bytes))
    }
}

#[derive(Clone)]
pub struct GenerationService {
    repository: Arc<dyn HubRepository>,
}

impl GenerationService {
    #[must_use]
    pub fn new(repository: Arc<dyn HubRepository>) -> Self {
        Self { repository }
    }

    pub async fn create(
        &self,
        command: CreateImageGeneration,
    ) -> Result<GenerationJob, ApplicationError> {
        validate_idempotency_key(&command.idempotency_key)?;
        if command.max_cost_microusd == 0 {
            return Err(ApplicationError::Validation(
                "max_cost_microusd must be positive".to_owned(),
            ));
        }
        let branch = command
            .branch()
            .map_err(|error| ApplicationError::Validation(error.to_string()))?;
        let offering = self
            .repository
            .active_offering(&command.native_model_id)
            .await?;
        validate_restrictions(branch, &command.asset_bindings, &offering.restrictions)?;
        let mut image_dimensions = None;
        let mut mask_dimensions = None;
        for binding in &command.asset_bindings {
            let asset = self
                .repository
                .get_asset(command.account_id, binding.asset_id)
                .await?;
            match binding.native_parameter_path.as_str() {
                path if path == "/image" || path.starts_with("/images/") => {
                    if !matches!(asset.role.as_str(), "image" | "output") {
                        return Err(ApplicationError::Validation(format!(
                            "asset {} cannot be used as an image",
                            asset.id
                        )));
                    }
                    image_dimensions.get_or_insert((asset.width, asset.height));
                }
                "/mask" => {
                    if asset.role != "mask" {
                        return Err(ApplicationError::Validation(format!(
                            "asset {} is not a mask",
                            asset.id
                        )));
                    }
                    mask_dimensions = Some((asset.width, asset.height));
                }
                _ => {}
            }
        }
        if let Some(mask_dimensions) = mask_dimensions
            && Some(mask_dimensions) != image_dimensions
        {
            return Err(ApplicationError::Validation(
                "mask dimensions must match the input image".to_owned(),
            ));
        }
        validate_native_request(&command, &offering)?;
        let request_hash = request_hash(&command)?;
        self.repository
            .create_job(command, branch, offering, request_hash)
            .await
    }

    pub async fn get(
        &self,
        account_id: AccountId,
        job_id: JobId,
    ) -> Result<JobView, ApplicationError> {
        self.repository.get_job(account_id, job_id).await
    }
}

pub struct WorkerService {
    repository: Arc<dyn HubRepository>,
    store: Arc<dyn AssetStore>,
    adapters: Arc<dyn AdapterFactory>,
    credentials: Arc<dyn CredentialProvider>,
    worker_id: String,
    lease_duration: ChronoDuration,
    provider_timeout: Duration,
}

impl WorkerService {
    pub fn new(
        repository: Arc<dyn HubRepository>,
        store: Arc<dyn AssetStore>,
        adapters: Arc<dyn AdapterFactory>,
        credentials: Arc<dyn CredentialProvider>,
        worker_id: String,
        lease_duration: ChronoDuration,
        provider_timeout: Duration,
    ) -> Result<Self, ApplicationError> {
        if worker_id.trim().is_empty() {
            return Err(ApplicationError::Configuration(
                "worker_id must not be empty".to_owned(),
            ));
        }
        Ok(Self {
            repository,
            store,
            adapters,
            credentials,
            worker_id,
            lease_duration,
            provider_timeout,
        })
    }

    pub async fn run_once(&self) -> Result<bool, ApplicationError> {
        self.repository.recover_expired_leases().await?;
        let Some(claimed) = self
            .repository
            .claim_next_job(&self.worker_id, self.lease_duration)
            .await?
        else {
            return Ok(false);
        };
        self.execute(claimed).await?;
        Ok(true)
    }

    async fn execute(&self, claimed: ClaimedJob) -> Result<(), ApplicationError> {
        let attempt_id = AttemptId::new();
        let prepared = match self.prepare(&claimed.job).await {
            Ok(prepared) => prepared,
            Err(error) => {
                self.repository
                    .fail_job(
                        claimed.job.id,
                        &self.worker_id,
                        None,
                        AttemptFailure {
                            code: "worker_prepare_failed".to_owned(),
                            message: error.to_string(),
                            trace_id: None,
                            target_state: JobState::Failed,
                            hold_disposition: HoldDisposition::Release,
                        },
                    )
                    .await?;
                return Ok(());
            }
        };
        let request_digest = request_digest(&prepared)?;
        self.repository
            .begin_attempt(claimed.job.id, &self.worker_id, attempt_id, &request_digest)
            .await?;
        let credential = match self
            .credentials
            .resolve(&claimed.job.offering.credential_env)
        {
            Ok(credential) => credential,
            Err(error) => {
                self.repository
                    .fail_job(
                        claimed.job.id,
                        &self.worker_id,
                        Some(attempt_id),
                        AttemptFailure {
                            code: "credential_unavailable".to_owned(),
                            message: error.to_string(),
                            trace_id: None,
                            target_state: JobState::Failed,
                            hold_disposition: HoldDisposition::Release,
                        },
                    )
                    .await?;
                return Ok(());
            }
        };
        let adapter = match self.adapters.create(
            &claimed.job.offering.adapter_key,
            &claimed.job.offering.base_url,
            self.provider_timeout,
        ) {
            Ok(adapter) => adapter,
            Err(error) => {
                self.repository
                    .fail_job(
                        claimed.job.id,
                        &self.worker_id,
                        Some(attempt_id),
                        AttemptFailure {
                            code: "adapter_configuration_failed".to_owned(),
                            message: error.to_string(),
                            trace_id: None,
                            target_state: JobState::Failed,
                            hold_disposition: HoldDisposition::Release,
                        },
                    )
                    .await?;
                return Ok(());
            }
        };
        match self
            .execute_with_heartbeat(claimed.job.id, adapter, prepared, &credential)
            .await
        {
            Ok(success) => {
                if let Err(error) = self
                    .complete_success(&claimed.job, attempt_id, success)
                    .await
                {
                    self.repository
                        .fail_job(
                            claimed.job.id,
                            &self.worker_id,
                            Some(attempt_id),
                            AttemptFailure {
                                code: "result_delivery_failed".to_owned(),
                                message: error.to_string(),
                                trace_id: None,
                                target_state: JobState::ReconciliationRequired,
                                hold_disposition: HoldDisposition::RetainForReconciliation,
                            },
                        )
                        .await?;
                }
            }
            Err(error) => {
                let failure = failure_from_adapter(error);
                self.repository
                    .fail_job(claimed.job.id, &self.worker_id, Some(attempt_id), failure)
                    .await?;
            }
        }
        Ok(())
    }

    async fn execute_with_heartbeat(
        &self,
        job_id: JobId,
        adapter: Arc<dyn ImageAdapter>,
        prepared: PreparedImageRequest,
        credential: &ProviderCredential,
    ) -> Result<ProviderSuccess, AdapterError> {
        let heartbeat_seconds = (self.lease_duration.num_seconds() / 3).max(1);
        let mut interval = tokio::time::interval(Duration::from_secs(
            u64::try_from(heartbeat_seconds).unwrap_or(1),
        ));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let call = adapter.execute(prepared, credential);
        tokio::pin!(call);
        let mut heartbeat_failure = None;
        loop {
            tokio::select! {
                result = &mut call => {
                    if let Some(message) = heartbeat_failure {
                        return Err(seeai_adapter_sdk::ProviderCallError {
                            code: "lease_heartbeat_failed".to_owned(),
                            message,
                            trace_id: None,
                            retry_safety: RetrySafety::AcceptanceUnknown,
                        }.into());
                    }
                    return result;
                },
                _ = interval.tick() => {
                    if let Err(error) = self.repository
                        .renew_lease(job_id, &self.worker_id, self.lease_duration)
                        .await
                    {
                        heartbeat_failure.get_or_insert_with(|| error.to_string());
                    }
                }
            }
        }
    }

    async fn prepare(&self, job: &GenerationJob) -> Result<PreparedImageRequest, ApplicationError> {
        let mut assets = Vec::with_capacity(job.asset_bindings.len());
        for binding in &job.asset_bindings {
            let record = self
                .repository
                .get_asset(job.account_id, binding.asset_id)
                .await?;
            let bytes = self.store.get(&record.object_key).await?;
            let digest = sha256_hex(&bytes);
            if digest != record.sha256 {
                return Err(ApplicationError::ObjectStorage(format!(
                    "asset {} digest mismatch",
                    record.id
                )));
            }
            assets.push(ResolvedAsset {
                native_parameter_path: binding.native_parameter_path.clone(),
                position: binding.position,
                media_type: record.media_type,
                sha256: record.sha256,
                bytes,
            });
        }
        Ok(PreparedImageRequest {
            provider_model_id: job.offering.provider_model_id.clone(),
            branch: job.branch,
            native_parameters: job.native_parameters.clone(),
            assets,
        })
    }

    async fn complete_success(
        &self,
        job: &GenerationJob,
        attempt_id: AttemptId,
        success: ProviderSuccess,
    ) -> Result<(), ApplicationError> {
        let charge = job
            .offering
            .price_snapshot
            .charge_microusd(&success.usage)
            .map_err(|error| ApplicationError::Reconciliation(error.to_string()))?;
        if charge > job.max_cost_microusd {
            return Err(ApplicationError::Reconciliation(format!(
                "actual charge {charge} exceeds authorization {}",
                job.max_cost_microusd
            )));
        }
        let mut outputs = Vec::with_capacity(success.images.len());
        for (index, image) in success.images.into_iter().enumerate() {
            let asset_id = AssetId::new();
            let dimensions = imagesize::blob_size(&image.bytes)
                .map_err(|error| ApplicationError::ObjectStorage(error.to_string()))?;
            let extension = match image.media_type.as_str() {
                "image/jpeg" => "jpg",
                "image/webp" => "webp",
                _ => "png",
            };
            let object_key = format!("outputs/{}/{index}-{asset_id}.{extension}", job.id);
            self.store
                .put(&object_key, image.bytes.clone(), &image.media_type)
                .await?;
            outputs.push(AssetRecord {
                id: asset_id,
                account_id: job.account_id,
                role: "output".to_owned(),
                object_key,
                media_type: image.media_type,
                byte_count: u64::try_from(image.bytes.len()).map_err(|_| {
                    ApplicationError::ObjectStorage("output is too large".to_owned())
                })?,
                width: u32::try_from(dimensions.width).map_err(|_| {
                    ApplicationError::ObjectStorage("output width is too large".to_owned())
                })?,
                height: u32::try_from(dimensions.height).map_err(|_| {
                    ApplicationError::ObjectStorage("output height is too large".to_owned())
                })?,
                sha256: image.sha256,
                created_at: Utc::now(),
            });
        }
        self.repository
            .complete_job(CompleteJob {
                job_id: job.id,
                worker_id: self.worker_id.clone(),
                attempt_id,
                outputs,
                evidence: MeteringEvidence {
                    attempt_id,
                    provider_response_digest: success.response_digest,
                    usage: success.usage,
                },
                charge_microusd: charge,
            })
            .await
    }
}

fn validate_idempotency_key(value: &str) -> Result<(), ApplicationError> {
    let length = value.len();
    if !(8..=128).contains(&length)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(ApplicationError::Validation(
            "idempotency_key must be 8-128 ASCII letters, digits, '.', '_' or '-'".to_owned(),
        ));
    }
    Ok(())
}

fn validate_restrictions(
    branch: ImageBranch,
    bindings: &[AssetBinding],
    restrictions: &Value,
) -> Result<(), ApplicationError> {
    if let Some(allowed) = restrictions
        .get("allowed_branches")
        .and_then(Value::as_array)
    {
        let name = match branch {
            ImageBranch::PromptOnly => "prompt_only",
            ImageBranch::ImageConditioned => "image_conditioned",
            ImageBranch::Masked => "masked",
        };
        if !allowed.iter().any(|value| value.as_str() == Some(name)) {
            return Err(ApplicationError::Validation(format!(
                "offering does not allow branch {name}"
            )));
        }
    }
    let image_count = bindings
        .iter()
        .filter(|binding| {
            binding.native_parameter_path == "/image"
                || binding.native_parameter_path.starts_with("/images/")
        })
        .count();
    let max_images = restrictions
        .get("max_images")
        .and_then(Value::as_u64)
        .unwrap_or(1);
    if u64::try_from(image_count).unwrap_or(u64::MAX) > max_images {
        return Err(ApplicationError::Validation(format!(
            "offering accepts at most {max_images} image(s)"
        )));
    }
    Ok(())
}

fn validate_native_request(
    command: &CreateImageGeneration,
    offering: &PublishedOffering,
) -> Result<(), ApplicationError> {
    let mut instance = command.native_parameters.clone();
    let object = instance.as_object_mut().ok_or_else(|| {
        ApplicationError::Validation("native_parameters must be an object".to_owned())
    })?;
    object.insert(
        "model".to_owned(),
        Value::String(command.native_model_id.clone()),
    );
    for binding in &command.asset_bindings {
        inject_asset_placeholder(object, binding)?;
    }
    let validator = jsonschema::validator_for(&offering.capability_schema)
        .map_err(|error| ApplicationError::Configuration(error.to_string()))?;
    let errors = validator
        .iter_errors(&instance)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ApplicationError::Validation(errors.join("; ")))
    }
}

fn inject_asset_placeholder(
    object: &mut Map<String, Value>,
    binding: &AssetBinding,
) -> Result<(), ApplicationError> {
    match binding.native_parameter_path.as_str() {
        "/image" => {
            object.insert(
                "image".to_owned(),
                Value::String(format!("asset://{}", binding.asset_id)),
            );
        }
        "/mask" => {
            object.insert(
                "mask".to_owned(),
                Value::String(format!("asset://{}", binding.asset_id)),
            );
        }
        path if path.starts_with("/images/") => {
            let mut images = object
                .remove("images")
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default();
            images.push(Value::String(format!("asset://{}", binding.asset_id)));
            object.insert("images".to_owned(), Value::Array(images));
        }
        path => {
            return Err(ApplicationError::Validation(format!(
                "unsupported asset binding path {path}"
            )));
        }
    }
    Ok(())
}

fn request_hash(command: &CreateImageGeneration) -> Result<String, ApplicationError> {
    let mut value = serde_json::to_value(command)
        .map_err(|error| ApplicationError::Validation(error.to_string()))?;
    canonicalize_json(&mut value);
    let encoded = serde_json::to_vec(&value)
        .map_err(|error| ApplicationError::Validation(error.to_string()))?;
    Ok(sha256_hex(&encoded))
}

fn request_digest(request: &PreparedImageRequest) -> Result<String, ApplicationError> {
    let assets = request
        .assets
        .iter()
        .map(|asset| {
            serde_json::json!({
                "path": asset.native_parameter_path,
                "position": asset.position,
                "media_type": asset.media_type,
                "sha256": asset.sha256,
            })
        })
        .collect::<Vec<_>>();
    let mut value = serde_json::json!({
        "provider_model_id": request.provider_model_id,
        "branch": request.branch,
        "native_parameters": request.native_parameters,
        "assets": assets,
    });
    canonicalize_json(&mut value);
    let bytes = serde_json::to_vec(&value)
        .map_err(|error| ApplicationError::Validation(error.to_string()))?;
    Ok(sha256_hex(&bytes))
}

fn canonicalize_json(value: &mut Value) {
    match value {
        Value::Object(object) => {
            let original = std::mem::take(object);
            let mut sorted = BTreeMap::new();
            for (key, mut child) in original {
                canonicalize_json(&mut child);
                sorted.insert(key, child);
            }
            object.extend(sorted);
        }
        Value::Array(items) => items.iter_mut().for_each(canonicalize_json),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn failure_from_adapter(error: AdapterError) -> AttemptFailure {
    match error {
        AdapterError::Provider(provider) => AttemptFailure {
            code: provider.code,
            message: provider.message,
            trace_id: provider.trace_id,
            target_state: if provider.retry_safety == RetrySafety::AcceptanceUnknown {
                JobState::ReconciliationRequired
            } else {
                JobState::Failed
            },
            hold_disposition: if provider.retry_safety == RetrySafety::AcceptanceUnknown {
                HoldDisposition::RetainForReconciliation
            } else {
                HoldDisposition::Release
            },
        },
        AdapterError::Configuration(message) | AdapterError::UnsupportedInput(message) => {
            AttemptFailure {
                code: "adapter_rejected".to_owned(),
                message,
                trace_id: None,
                target_state: JobState::Failed,
                hold_disposition: HoldDisposition::Release,
            }
        }
    }
}

fn validate_input_media(media_type: &str, bytes: &[u8]) -> Result<(), ApplicationError> {
    let valid = match media_type {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/webp" => bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        _ => false,
    };
    if !valid {
        return Err(ApplicationError::Validation(
            "media type does not match file signature".to_owned(),
        ));
    }
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(ApplicationError::Validation(
            "asset exceeds 16 MiB".to_owned(),
        ));
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use seeai_adapter_sdk::{GeneratedImage, ProviderCallError};
    use seeai_domain::{
        ChannelId, OfferingId, PricePlanId, PriceSnapshot, RuntimeRevisionId, TokenUsage,
        VendorModelId,
    };
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    fn offering() -> PublishedOffering {
        PublishedOffering {
            runtime_revision_id: RuntimeRevisionId::new(),
            vendor_model_id: VendorModelId::new(),
            offering_id: OfferingId::new(),
            channel_id: ChannelId::new(),
            native_model_id: "gpt-image-2".to_owned(),
            native_revision: "2026-04-21".to_owned(),
            capability_schema: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["model", "prompt"],
                "properties": {
                    "model": {"const": "gpt-image-2"},
                    "prompt": {"type": "string", "minLength": 1},
                    "image": {"type": "string"},
                    "mask": {"type": "string"}
                }
            }),
            restrictions: serde_json::json!({
                "allowed_branches": ["prompt_only", "image_conditioned", "masked"],
                "max_images": 1
            }),
            adapter_key: "aihubmix-image-v1".to_owned(),
            provider_model_id: "gpt-image-2".to_owned(),
            provider_kind: "AIHubMix".to_owned(),
            base_url: "https://api.inferera.com".to_owned(),
            credential_env: "AIHUBMIX_API_KEY".to_owned(),
            price_snapshot: PriceSnapshot {
                price_plan_id: PricePlanId::new(),
                rates: PriceRates {
                    currency: "USD".to_owned(),
                    text_input_microusd_per_million: 5_000_000,
                    image_input_microusd_per_million: 8_000_000,
                    text_output_microusd_per_million: 10_000_000,
                    image_output_microusd_per_million: 30_000_000,
                },
                captured_at: Utc::now(),
            },
        }
    }

    #[test]
    fn validates_prompt_only_native_request() {
        let command = CreateImageGeneration {
            account_id: AccountId::new(),
            native_model_id: "gpt-image-2".to_owned(),
            native_parameters: serde_json::json!({"prompt": "hello"}),
            asset_bindings: Vec::new(),
            idempotency_key: "request-0001".to_owned(),
            max_cost_microusd: 20_000,
        };
        assert!(validate_native_request(&command, &offering()).is_ok());
    }

    #[test]
    fn injects_image_binding_before_schema_validation() {
        let command = CreateImageGeneration {
            account_id: AccountId::new(),
            native_model_id: "gpt-image-2".to_owned(),
            native_parameters: serde_json::json!({"prompt": "hello"}),
            asset_bindings: vec![AssetBinding {
                native_parameter_path: "/image".to_owned(),
                asset_id: AssetId::new(),
                position: 0,
            }],
            idempotency_key: "request-0002".to_owned(),
            max_cost_microusd: 20_000,
        };
        assert!(validate_native_request(&command, &offering()).is_ok());
    }

    #[test]
    fn canonical_request_hash_ignores_object_key_order() {
        let first = CreateImageGeneration {
            account_id: AccountId::new(),
            native_model_id: "gpt-image-2".to_owned(),
            native_parameters: serde_json::json!({"prompt":"x", "n":1}),
            asset_bindings: Vec::new(),
            idempotency_key: "request-0003".to_owned(),
            max_cost_microusd: 20_000,
        };
        let mut second = first.clone();
        second.native_parameters =
            serde_json::from_str(r#"{"n":1,"prompt":"x"}"#).expect("fixture should parse");
        assert_eq!(
            request_hash(&first).expect("hash"),
            request_hash(&second).expect("hash")
        );
    }

    struct WorkerRepository {
        job: Mutex<Option<GenerationJob>>,
        completion: Mutex<Option<CompleteJob>>,
        failure: Mutex<Option<AttemptFailure>>,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl WorkerRepository {
        fn new(job: GenerationJob, events: Arc<Mutex<Vec<&'static str>>>) -> Self {
            Self {
                job: Mutex::new(Some(job)),
                completion: Mutex::new(None),
                failure: Mutex::new(None),
                events,
            }
        }
    }

    fn unused_repository<T>() -> Result<T, ApplicationError> {
        Err(ApplicationError::Persistence(
            "unused repository operation in worker test".to_owned(),
        ))
    }

    #[async_trait]
    impl HubRepository for WorkerRepository {
        async fn publish_runtime(
            &self,
            _command: PublishRuntimeCommand,
        ) -> Result<PublishedOffering, ApplicationError> {
            unused_repository()
        }

        async fn active_offering(
            &self,
            _native_model_id: &str,
        ) -> Result<PublishedOffering, ApplicationError> {
            unused_repository()
        }

        async fn create_account(
            &self,
            _account_id: AccountId,
            _initial_credit_microusd: u64,
            _actor: &str,
        ) -> Result<(), ApplicationError> {
            unused_repository()
        }

        async fn credit_account(
            &self,
            _account_id: AccountId,
            _amount_microusd: u64,
            _business_key: &str,
            _actor: &str,
        ) -> Result<(), ApplicationError> {
            unused_repository()
        }

        async fn create_api_key(
            &self,
            _account_id: AccountId,
            _label: &str,
            _key_hash: &str,
            _actor: &str,
        ) -> Result<(), ApplicationError> {
            unused_repository()
        }

        async fn account_for_api_key(
            &self,
            _key_hash: &str,
        ) -> Result<AccountId, ApplicationError> {
            unused_repository()
        }

        async fn insert_asset(&self, _asset: AssetRecord) -> Result<(), ApplicationError> {
            unused_repository()
        }

        async fn get_asset(
            &self,
            _account_id: AccountId,
            _asset_id: AssetId,
        ) -> Result<AssetRecord, ApplicationError> {
            unused_repository()
        }

        async fn create_job(
            &self,
            _command: CreateImageGeneration,
            _branch: ImageBranch,
            _offering: PublishedOffering,
            _request_hash: String,
        ) -> Result<GenerationJob, ApplicationError> {
            unused_repository()
        }

        async fn get_job(
            &self,
            _account_id: AccountId,
            _job_id: JobId,
        ) -> Result<JobView, ApplicationError> {
            unused_repository()
        }

        async fn claim_next_job(
            &self,
            worker_id: &str,
            lease_duration: ChronoDuration,
        ) -> Result<Option<ClaimedJob>, ApplicationError> {
            let job = self
                .job
                .lock()
                .map_err(|error| ApplicationError::Persistence(error.to_string()))?
                .take();
            Ok(job.map(|job| ClaimedJob {
                job,
                lease_owner: worker_id.to_owned(),
                lease_expires_at: Utc::now() + lease_duration,
            }))
        }

        async fn recover_expired_leases(&self) -> Result<LeaseRecovery, ApplicationError> {
            Ok(LeaseRecovery::default())
        }

        async fn begin_attempt(
            &self,
            _job_id: JobId,
            _worker_id: &str,
            _attempt_id: AttemptId,
            _request_digest: &str,
        ) -> Result<(), ApplicationError> {
            Ok(())
        }

        async fn renew_lease(
            &self,
            _job_id: JobId,
            _worker_id: &str,
            _lease_duration: ChronoDuration,
        ) -> Result<(), ApplicationError> {
            Ok(())
        }

        async fn complete_job(&self, completion: CompleteJob) -> Result<(), ApplicationError> {
            self.events
                .lock()
                .map_err(|error| ApplicationError::Persistence(error.to_string()))?
                .push("complete");
            *self
                .completion
                .lock()
                .map_err(|error| ApplicationError::Persistence(error.to_string()))? =
                Some(completion);
            Ok(())
        }

        async fn fail_job(
            &self,
            _job_id: JobId,
            _worker_id: &str,
            _attempt_id: Option<AttemptId>,
            failure: AttemptFailure,
        ) -> Result<(), ApplicationError> {
            *self
                .failure
                .lock()
                .map_err(|error| ApplicationError::Persistence(error.to_string()))? = Some(failure);
            Ok(())
        }

        async fn list_open_reconciliation_cases(
            &self,
        ) -> Result<Vec<ReconciliationCaseView>, ApplicationError> {
            unused_repository()
        }

        async fn refund_reconciliation(
            &self,
            _command: RefundReconciliationCommand,
        ) -> Result<(), ApplicationError> {
            unused_repository()
        }
    }

    struct WorkerStore {
        events: Arc<Mutex<Vec<&'static str>>>,
        keys: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl AssetStore for WorkerStore {
        async fn put(
            &self,
            object_key: &str,
            _bytes: Bytes,
            _media_type: &str,
        ) -> Result<(), ApplicationError> {
            self.events
                .lock()
                .map_err(|error| ApplicationError::ObjectStorage(error.to_string()))?
                .push("store");
            self.keys
                .lock()
                .map_err(|error| ApplicationError::ObjectStorage(error.to_string()))?
                .push(object_key.to_owned());
            Ok(())
        }

        async fn get(&self, _object_key: &str) -> Result<Bytes, ApplicationError> {
            Err(ApplicationError::ObjectStorage(
                "unused object read in worker test".to_owned(),
            ))
        }
    }

    struct WorkerAdapter {
        succeeds: bool,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ImageAdapter for WorkerAdapter {
        fn key(&self) -> &'static str {
            "aihubmix-image-v1"
        }

        async fn execute(
            &self,
            _request: PreparedImageRequest,
            _credential: &ProviderCredential,
        ) -> Result<ProviderSuccess, AdapterError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if !self.succeeds {
                return Err(ProviderCallError {
                    code: "provider_response_invalid".to_owned(),
                    message: "missing verified usage".to_owned(),
                    trace_id: None,
                    retry_safety: RetrySafety::AcceptanceUnknown,
                }
                .into());
            }
            let bytes = Bytes::from(
                hex::decode("89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d4944415478da6364f8cff01f0005fe02fe5dc638590000000049454e44ae426082")
                    .expect("PNG fixture must decode"),
            );
            Ok(ProviderSuccess {
                images: vec![GeneratedImage {
                    media_type: "image/png".to_owned(),
                    sha256: sha256_hex(&bytes),
                    bytes,
                }],
                usage: TokenUsage {
                    input_tokens: 9,
                    input_text_tokens: 9,
                    input_image_tokens: 0,
                    output_tokens: 196,
                    output_text_tokens: 0,
                    output_image_tokens: 196,
                    total_tokens: 205,
                },
                response_digest: "provider-response-digest".to_owned(),
            })
        }
    }

    struct WorkerAdapterFactory {
        adapter: Arc<WorkerAdapter>,
    }

    impl AdapterFactory for WorkerAdapterFactory {
        fn descriptor(&self, _adapter_key: &str) -> Option<AdapterDescriptor> {
            None
        }

        fn validate_publication(
            &self,
            _adapter_key: &str,
            _capability_schema: &Value,
            _restrictions: &Value,
        ) -> Result<(), String> {
            Ok(())
        }

        fn create(
            &self,
            _adapter_key: &str,
            _base_url: &str,
            _timeout: Duration,
        ) -> Result<Arc<dyn ImageAdapter>, ApplicationError> {
            Ok(self.adapter.clone())
        }
    }

    struct WorkerCredentialProvider;

    impl CredentialProvider for WorkerCredentialProvider {
        fn resolve(&self, _reference: &str) -> Result<ProviderCredential, ApplicationError> {
            ProviderCredential::new("test-credential".to_owned())
                .map_err(|error| ApplicationError::Configuration(error.to_string()))
        }
    }

    fn worker_job() -> GenerationJob {
        GenerationJob {
            id: JobId::new(),
            account_id: AccountId::new(),
            state: JobState::Leased,
            branch: ImageBranch::PromptOnly,
            native_model_id: "gpt-image-2".to_owned(),
            native_parameters: serde_json::json!({"prompt": "worker contract"}),
            asset_bindings: Vec::new(),
            offering: offering(),
            idempotency_key: "worker-contract-1".to_owned(),
            request_hash: "request-hash".to_owned(),
            max_cost_microusd: 20_000,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn worker(
        repository: Arc<WorkerRepository>,
        store: Arc<WorkerStore>,
        adapter: Arc<WorkerAdapter>,
    ) -> WorkerService {
        WorkerService::new(
            repository,
            store,
            Arc::new(WorkerAdapterFactory { adapter }),
            Arc::new(WorkerCredentialProvider),
            "worker-test".to_owned(),
            ChronoDuration::seconds(30),
            Duration::from_secs(1),
        )
        .expect("worker fixture must be valid")
    }

    #[tokio::test]
    async fn worker_archives_output_before_evidence_settlement() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let repository = Arc::new(WorkerRepository::new(worker_job(), events.clone()));
        let store = Arc::new(WorkerStore {
            events: events.clone(),
            keys: Mutex::new(Vec::new()),
        });
        let adapter = Arc::new(WorkerAdapter {
            succeeds: true,
            calls: AtomicUsize::new(0),
        });

        assert!(
            worker(repository.clone(), store, adapter.clone())
                .run_once()
                .await
                .expect("worker run must succeed")
        );
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            *events.lock().expect("event lock"),
            vec!["store", "complete"]
        );
        let completion = repository
            .completion
            .lock()
            .expect("completion lock")
            .take()
            .expect("job must complete");
        assert_eq!(completion.evidence.usage.total_tokens, 205);
        assert_eq!(completion.charge_microusd, 5_925);
        assert_eq!(completion.outputs.len(), 1);
        assert!(repository.failure.lock().expect("failure lock").is_none());
    }

    #[tokio::test]
    async fn worker_sends_ambiguous_provider_response_to_reconciliation_once() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let repository = Arc::new(WorkerRepository::new(worker_job(), events.clone()));
        let store = Arc::new(WorkerStore {
            events,
            keys: Mutex::new(Vec::new()),
        });
        let adapter = Arc::new(WorkerAdapter {
            succeeds: false,
            calls: AtomicUsize::new(0),
        });

        assert!(
            worker(repository.clone(), store, adapter.clone())
                .run_once()
                .await
                .expect("worker run must converge")
        );
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
        let failure = repository
            .failure
            .lock()
            .expect("failure lock")
            .take()
            .expect("job must fail to reconciliation");
        assert_eq!(failure.target_state, JobState::ReconciliationRequired);
        assert_eq!(
            failure.hold_disposition,
            HoldDisposition::RetainForReconciliation
        );
        assert!(
            repository
                .completion
                .lock()
                .expect("completion lock")
                .is_none()
        );
    }
}
