use std::path::PathBuf;
use std::sync::Arc;

use crate::db::DatabaseConnection;
use serde::{Deserialize, Serialize};

use super::StorageError;
use super::database::DatabaseBlobStore;
use super::filesystem::FilesystemBlobStore;
use super::instrumented::InstrumentedBlobStore;
use super::traits::BlobStore;

pub const DEFAULT_MAX_BLOB_SIZE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[cfg(feature = "object-storage")]
use super::object_storage::{ObjectStorageBlobStore, ObjectStorageConfig};

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ObjectStorageConfigToml {
    pub bucket: String,
    #[serde(default = "default_os_region")]
    pub region: String,
    pub endpoint: Option<String>,
    pub access_key: Option<String>,
    pub secret_key: Option<String>,
    #[serde(default)]
    pub path_style: bool,
    pub temp_dir: Option<String>,
}

fn default_os_region() -> String {
    "us-east-1".into()
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct BlobStoreConfig {
    #[serde(default = "default_backend")]
    pub backend: String,
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
    #[serde(default = "default_max_blob_size")]
    pub max_blob_size: u64,
    pub object_storage: Option<ObjectStorageConfigToml>,
}

fn default_backend() -> String {
    "database".into()
}

fn default_data_dir() -> String {
    "./data".into()
}

fn default_max_blob_size() -> u64 {
    DEFAULT_MAX_BLOB_SIZE_BYTES
}

impl Default for BlobStoreConfig {
    fn default() -> Self {
        Self {
            backend: default_backend(),
            data_dir: default_data_dir(),
            max_blob_size: default_max_blob_size(),
            object_storage: None,
        }
    }
}

pub async fn create_blob_store(
    config: &BlobStoreConfig,
    db: DatabaseConnection,
    metrics: Option<crate::metrics::Metrics>,
) -> Result<Arc<dyn BlobStore>, StorageError> {
    let backend = config.backend.clone();
    let store: Arc<dyn BlobStore> = match config.backend.as_str() {
        "filesystem" => {
            let blob_path = PathBuf::from(&config.data_dir).join("blobs");
            let store = FilesystemBlobStore::new(blob_path, config.max_blob_size).await?;
            Arc::new(store)
        }
        #[cfg(feature = "object-storage")]
        "object_storage" => {
            let os = config.object_storage.as_ref().ok_or_else(|| {
                StorageError::Backend(
                    "storage.backend is 'object_storage' but [storage.object_storage] section is missing".into(),
                )
            })?;
            let store = ObjectStorageBlobStore::new(ObjectStorageConfig {
                bucket: os.bucket.clone(),
                region: os.region.clone(),
                endpoint: os.endpoint.clone(),
                access_key: os.access_key.clone(),
                secret_key: os.secret_key.clone(),
                path_style: os.path_style,
                max_size: config.max_blob_size,
                temp_dir: os
                    .temp_dir
                    .as_ref()
                    .filter(|s| !s.is_empty())
                .map(PathBuf::from),
            }, metrics.clone())?;
            // Refuse to start against a missing or inaccessible bucket rather
            // than failing on the first upload or fetch; wait for storage that
            // is not answering yet, as for the database and Redis.
            crate::boot::wait_for("object storage", &os.endpoint.clone().unwrap_or_default(), || async {
                store.verify_bucket().await.map_err(|e| match e {
                    StorageError::Unavailable(_) => crate::boot::Unreachable::Transient(e),
                    e => crate::boot::Unreachable::Fatal(e),
                })
            })
            .await
            .map_err(|e| StorageError::Backend(e.to_string()))?;
            Arc::new(store)
        }
        #[cfg(not(feature = "object-storage"))]
        "object_storage" => return Err(StorageError::Backend(
            "storage.backend is 'object_storage' but the binary was compiled without the 'object-storage' feature".into(),
        )),
        "database" => Arc::new(DatabaseBlobStore::new(db, config.max_blob_size)),
        other => return Err(StorageError::Backend(format!(
            "Unknown storage backend '{other}'. Valid values: database, filesystem, object_storage"
        )))
    };

    if let Some(metrics) = metrics {
        Ok(Arc::new(InstrumentedBlobStore::new(
            store, backend, metrics,
        )))
    } else {
        Ok(store)
    }
}
