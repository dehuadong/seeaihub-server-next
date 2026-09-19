use async_trait::async_trait;
use bytes::Bytes;
use object_store::{ObjectStore, ObjectStoreExt, aws::AmazonS3Builder, local::LocalFileSystem};
use seeai_application::{ApplicationError, AssetStore};
use std::{path::Path as FilePath, sync::Arc};

#[derive(Debug, Clone)]
pub struct ObjectStoreAssetStore {
    inner: Arc<dyn ObjectStore>,
}

impl ObjectStoreAssetStore {
    pub fn from_env() -> Result<Self, ApplicationError> {
        match std::env::var("ASSET_STORE")
            .unwrap_or_else(|_| "local".to_owned())
            .as_str()
        {
            "local" => Self::local(
                std::env::var("ASSET_LOCAL_ROOT").unwrap_or_else(|_| ".data/assets".to_owned()),
            ),
            "s3" => Self::s3(S3Config {
                endpoint: required_env("S3_ENDPOINT")?,
                bucket: required_env("S3_BUCKET")?,
                region: std::env::var("S3_REGION").unwrap_or_else(|_| "us-east-1".to_owned()),
                access_key: required_env("S3_ACCESS_KEY")?,
                secret_key: required_env("S3_SECRET_KEY")?,
            }),
            value => Err(ApplicationError::Configuration(format!(
                "unsupported ASSET_STORE {value}"
            ))),
        }
    }

    pub fn local(root: impl AsRef<FilePath>) -> Result<Self, ApplicationError> {
        std::fs::create_dir_all(root.as_ref())
            .map_err(|error| ApplicationError::ObjectStorage(error.to_string()))?;
        let store = LocalFileSystem::new_with_prefix(root)
            .map_err(|error| ApplicationError::ObjectStorage(error.to_string()))?;
        Ok(Self {
            inner: Arc::new(store),
        })
    }

    pub fn s3(config: S3Config) -> Result<Self, ApplicationError> {
        let allow_http = config.endpoint.starts_with("http://");
        let store = AmazonS3Builder::new()
            .with_bucket_name(config.bucket)
            .with_region(config.region)
            .with_endpoint(config.endpoint)
            .with_access_key_id(config.access_key)
            .with_secret_access_key(config.secret_key)
            .with_allow_http(allow_http)
            .with_virtual_hosted_style_request(false)
            .build()
            .map_err(|error| ApplicationError::ObjectStorage(error.to_string()))?;
        Ok(Self {
            inner: Arc::new(store),
        })
    }
}

fn required_env(name: &str) -> Result<String, ApplicationError> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ApplicationError::Configuration(format!("missing environment {name}")))
}

#[derive(Debug, Clone)]
pub struct S3Config {
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
}

#[async_trait]
impl AssetStore for ObjectStoreAssetStore {
    async fn put(
        &self,
        object_key: &str,
        bytes: Bytes,
        _media_type: &str,
    ) -> Result<(), ApplicationError> {
        let path = object_store::path::Path::parse(object_key)
            .map_err(|error| ApplicationError::ObjectStorage(error.to_string()))?;
        self.inner
            .put(&path, bytes.into())
            .await
            .map_err(|error| ApplicationError::ObjectStorage(error.to_string()))?;
        Ok(())
    }

    async fn get(&self, object_key: &str) -> Result<Bytes, ApplicationError> {
        let path = object_store::path::Path::parse(object_key)
            .map_err(|error| ApplicationError::ObjectStorage(error.to_string()))?;
        self.inner
            .get(&path)
            .await
            .map_err(|error| ApplicationError::ObjectStorage(error.to_string()))?
            .bytes()
            .await
            .map_err(|error| ApplicationError::ObjectStorage(error.to_string()))
    }
}
