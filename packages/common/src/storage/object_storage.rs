use std::path::PathBuf;

use async_trait::async_trait;
use s3::creds::Credentials;
use s3::error::S3Error;
use s3::request::ResponseData;
use s3::serde_types::HeadObjectResult;
use s3::utils::PutStreamResponse;
use s3::{Bucket, Region};
use sha2::{Digest, Sha256};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, warn};

use opentelemetry::KeyValue;

use crate::metrics::Metrics;

use super::error::StorageError;
use super::hash::ContentHash;
use super::traits::{BlobStore, BoxReader};

#[derive(Debug, Clone)]
pub struct ObjectStorageConfig {
    pub bucket: String,
    pub region: String,
    pub endpoint: Option<String>,
    pub access_key: Option<String>,
    pub secret_key: Option<String>,
    pub path_style: bool,
    pub max_size: u64,
    pub temp_dir: Option<PathBuf>,
}

pub struct ObjectStorageBlobStore {
    bucket: Box<Bucket>,
    max_size: u64,
    temp_dir: PathBuf,
    metrics: Option<Metrics>,
}

impl ObjectStorageBlobStore {
    pub fn new(
        config: ObjectStorageConfig,
        metrics: Option<Metrics>,
    ) -> Result<Self, StorageError> {
        let region = match &config.endpoint {
            Some(endpoint) => Region::Custom {
                region: config.region.clone(),
                endpoint: endpoint.clone(),
            },
            None => config
                .region
                .parse::<Region>()
                .map_err(|e| StorageError::Backend(format!("invalid region: {e}")))?,
        };

        let credentials = Credentials::new(
            config.access_key.as_deref(),
            config.secret_key.as_deref(),
            None,
            None,
            None,
        )
        .map_err(|e| StorageError::Backend(format!("invalid credentials: {e}")))?;

        // Disable rust-s3's built-in retry! (default 1 retry): it covers only
        // connection/header establishment with blocking, no-jitter sleeps and -
        // with fail-on-err off - never sees 5xx. We own all retry/backoff below
        // with status-aware, jittered logic.
        s3::set_retries(0);

        let mut bucket = Bucket::new(&config.bucket, region, credentials)
            .map_err(|e| StorageError::Backend(format!("failed to create bucket: {e}")))?;

        if config.path_style {
            bucket.set_path_style();
        }

        // rust-s3 0.37's tokio backend builds its reqwest client with NO request
        // timeout (the default 60s field is honoured only by the blocking/hyper
        // backends). Without this, a half-open or stalled SeaweedFS connection
        // hangs the op's future forever, pinning a worker task and leaking a
        // pooled connection - they accumulate and stall the judge under load.
        // with_request_timeout rebuilds the reqwest client WITH a per-request
        // timeout and preserves path_style (so we set path_style first).
        let bucket = bucket
            .with_request_timeout(std::time::Duration::from_secs(S3_REQUEST_TIMEOUT_SECS))
            .map_err(|e| StorageError::Backend(format!("failed to set request timeout: {e}")))?;

        let temp_dir = config.temp_dir.unwrap_or_else(std::env::temp_dir);

        Ok(Self {
            bucket,
            max_size: config.max_size,
            temp_dir,
            metrics,
        })
    }

    fn object_key(hash: &ContentHash) -> String {
        format!("{}/{}", hash.shard_prefix(), hash.shard_suffix())
    }

    fn temp_path(&self) -> PathBuf {
        self.temp_dir
            .join(format!(".s3_upload_{}", uuid::Uuid::new_v4()))
    }
}

fn is_s3_not_found(err: &S3Error) -> bool {
    matches!(err, S3Error::HttpFailWithBody(404, _))
}

const MAX_S3_RETRIES: u32 = 3;
const S3_REQUEST_TIMEOUT_SECS: u64 = 120;
const S3_WINDOW: u64 = 8 * 1024 * 1024;

/// HTTP statuses worth retrying. With fail-on-err off these arrive as
/// `Ok(ResponseData)` (not `Err`), so callers must inspect status, not just the
/// `Err` arm. 429 = throttling (SeaweedFS SlowDown), 5xx = server-side transient.
fn is_retryable_status(code: u16) -> bool {
    code == 429 || (500..600).contains(&code)
}

/// Log + count a retry of a transient S3 call. Centralised so every retry site
/// (get/head/put/delete) is observable identically: a WARN line (visible in
/// logs and the trace) plus the `broccoli.blob_store.retries` counter, labelled
/// by operation, for contest-day dashboards/alerts. Transport errors include
/// request timeouts (reqwest surfaces them as transport errors), so a stalled
/// backend shows up here as a retry with a "transport error: ... timed out"
/// reason rather than silently.
fn record_retry(metrics: Option<&Metrics>, op: &str, attempt: u32, reason: &str) {
    warn!(
        operation = op,
        attempt, reason, "retrying transient blob store S3 call"
    );
    if let Some(m) = metrics {
        m.blob_store_retries_total
            .add(1, &[KeyValue::new("operation", op.to_string())]);
    }
}

/// Full-jitter exponential backoff for attempt `n` (1-based): ~base 200ms,
/// capped at 2s, randomised across `[0, cap]` to avoid thundering herds when
/// many concurrent ops retry against a throttling backend.
async fn retry_backoff(attempt: u32) {
    let cap = (100u64.saturating_mul(1u64 << attempt.min(5))).min(2000);
    let jitter = rand::random::<u64>() % (cap + 1);
    tokio::time::sleep(std::time::Duration::from_millis(jitter)).await;
}

/// The HTTP status of a successful-at-transport-level S3 response. With
/// fail-on-err off, error statuses arrive inside `Ok`, so retry and
/// classification must look at this, not only at `Err`.
trait S3Status {
    fn status(&self) -> u16;
}

impl S3Status for ResponseData {
    fn status(&self) -> u16 {
        self.status_code()
    }
}

impl S3Status for PutStreamResponse {
    fn status(&self) -> u16 {
        self.status_code()
    }
}

impl S3Status for (HeadObjectResult, u16) {
    fn status(&self) -> u16 {
        self.1
    }
}

/// The one retry policy for every S3 call: up to [`MAX_S3_RETRIES`] retries
/// with jittered backoff on transport errors (incl. timeouts) and retryable
/// statuses (5xx/429). Terminal outcomes (2xx, 404, 416, other 4xx) are handed
/// back for the caller to classify. `call` builds a fresh request each attempt,
/// so the whole call (connection AND body) is inside the retry, unlike
/// rust-s3's built-in retry, which only wraps connection setup.
async fn with_retry<T, F, Fut>(
    op: &'static str,
    metrics: Option<&Metrics>,
    mut call: F,
) -> Result<T, S3Error>
where
    T: S3Status,
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, S3Error>>,
{
    let mut attempt = 0u32;
    loop {
        let reason = match call().await {
            Ok(resp) if is_retryable_status(resp.status()) && attempt < MAX_S3_RETRIES => {
                format!("status {}", resp.status())
            }
            Err(e) if !is_s3_not_found(&e) && attempt < MAX_S3_RETRIES => {
                format!("transport error: {e}")
            }
            done => return done,
        };
        attempt += 1;
        record_retry(metrics, op, attempt, &reason);
        retry_backoff(attempt).await;
    }
}

impl ObjectStorageBlobStore {
    /// HEAD an object with retry; `Ok(None)` when it does not exist.
    async fn head(&self, key: &str) -> Result<Option<HeadObjectResult>, StorageError> {
        match with_retry("head", self.metrics.as_ref(), || {
            self.bucket.head_object(key)
        })
        .await
        {
            Ok((_, 404)) => Ok(None),
            Ok((head, status)) if (200..300).contains(&status) => Ok(Some(head)),
            Ok((_, status)) => Err(StorageError::Backend(format!(
                "S3 head returned status {status}"
            ))),
            Err(e) if is_s3_not_found(&e) => Ok(None),
            Err(e) => Err(StorageError::Backend(format!(
                "head_object failed after retries: {e}"
            ))),
        }
    }

    /// The object's length from a HEAD, or `NotFound`.
    async fn content_length(&self, hash: &ContentHash) -> Result<u64, StorageError> {
        let head = self
            .head(&Self::object_key(hash))
            .await?
            .ok_or_else(|| StorageError::NotFound(hash.to_hex()))?;
        head.content_length
            .map(|len| len as u64)
            .ok_or_else(|| StorageError::Backend("HEAD response missing content-length".into()))
    }

    /// Check at boot that the configured bucket exists and these credentials
    /// can reach it (S3 HeadBucket, with the usual retry). Without this, a
    /// missing bucket (e.g. an infra init job that failed quietly) lets servers
    /// and workers boot "healthy" and fail on the first upload or fetch.
    ///
    /// The bucket is deliberately not created here: it is shared by every
    /// server and worker, so auto-creating it would turn a typo in one node's
    /// config into a private, empty bucket for that node alone.
    pub async fn verify_bucket(&self) -> Result<(), StorageError> {
        let bucket = &self.bucket.name;
        // An empty key addresses the bucket itself: `HEAD /<bucket>/`.
        match with_retry("head_bucket", self.metrics.as_ref(), || {
            self.bucket.head_object("")
        })
        .await
        {
            Ok((_, status)) if (200..300).contains(&status) => Ok(()),
            Ok((_, 404)) => Err(StorageError::Backend(format!(
                "object storage bucket `{bucket}` does not exist at {}. Create it (the infra \
                 init job normally does) or fix storage.object_storage.bucket",
                self.bucket.url()
            ))),
            Ok((_, status @ (401 | 403))) => Err(StorageError::Backend(format!(
                "object storage refused access to bucket `{bucket}` (HTTP {status}): check \
                 storage.object_storage.access_key / secret_key"
            ))),
            Ok((_, status)) => Err(StorageError::Backend(format!(
                "object storage bucket check for `{bucket}` returned HTTP {status}"
            ))),
            Err(e) => Err(StorageError::Backend(format!(
                "object storage at {} is unreachable: {e}",
                self.bucket.url()
            ))),
        }
    }
}

#[async_trait]
impl BlobStore for ObjectStorageBlobStore {
    async fn put_stream(&self, mut reader: BoxReader) -> Result<ContentHash, StorageError> {
        let temp_path = self.temp_path();
        let mut hasher = Sha256::new();
        let mut total_bytes: u64 = 0;
        let mut chunk = vec![0u8; 64 * 1024];

        let mut temp_file = fs::File::create(&temp_path).await.map_err(|e| {
            StorageError::Io(std::io::Error::new(
                e.kind(),
                format!("failed to create temp file {}: {e}", temp_path.display()),
            ))
        })?;

        let result: Result<ContentHash, StorageError> = async {
            loop {
                let n = reader.read(&mut chunk).await?;
                if n == 0 {
                    break;
                }

                total_bytes += n as u64;
                if total_bytes > self.max_size {
                    return Err(StorageError::SizeLimitExceeded {
                        actual: total_bytes,
                        limit: self.max_size,
                    });
                }

                hasher.update(&chunk[..n]);
                temp_file.write_all(&chunk[..n]).await?;
            }

            temp_file.flush().await?;
            drop(temp_file);

            let hash = ContentHash::from_bytes(hasher.finalize().into());
            let key = Self::object_key(&hash);

            debug!(key = %key, size = total_bytes, "streaming temp file to S3");

            // Retry on transient failures, RE-OPENING the temp file every
            // attempt. put_object_stream CONSUMES the reader (and for >=8 MiB
            // bodies drains it via multipart); reusing a spent handle on retry
            // would upload 0 bytes under the correct content hash - silent,
            // permanent content-addressed corruption. A fresh handle each
            // attempt always streams from byte 0.
            let status_code = with_retry("put", self.metrics.as_ref(), || async {
                let mut upload_file = fs::File::open(&temp_path).await?;
                self.bucket.put_object_stream(&mut upload_file, &key).await
            })
            .await
            .map_err(|e| {
                StorageError::Backend(format!("put_object_stream failed after retries: {e}"))
            })?
            .status();

            if !(200..300).contains(&status_code) {
                return Err(StorageError::Backend(format!(
                    "S3 put returned status {status_code}"
                )));
            }

            Ok(hash)
        }
        .await;

        let _ = fs::remove_file(&temp_path).await;

        result
    }

    async fn get_stream(&self, hash: &ContentHash) -> Result<BoxReader, StorageError> {
        let key = Self::object_key(hash);

        // NOTE: rust-s3 0.37's streaming `get_object_stream` returns an
        // `AsyncRead` adapter that TRUNCATES chunked-transfer responses from
        // SeaweedFS - it signals EOF after the first chunk, so a 17 KB object
        // is read back as ~12 KB of garbage-hashing bytes. Instead we stream
        // the object ourselves via explicit byte ranges: each ranged GET has a
        // definite Content-Length and reassembles losslessly, while memory
        // stays bounded to a single window regardless of blob size.
        //
        // A HEAD up front gives us the AUTHORITATIVE total length (so the loop
        // terminates against a real total, not a fragile "short read == EOF"
        // heuristic) and surfaces a missing blob as `NotFound` before we hand
        // back a reader. The HEAD reuses the pooled connection and is cheap
        // relative to the body transfer. Both HEAD and every window carry the
        // bounded transient retry.
        let total = self.content_length(hash).await?;

        let bucket = (*self.bucket).clone();
        let hash_hex = hash.to_hex();
        let metrics = self.metrics.clone();

        let stream = futures::stream::try_unfold(0u64, move |offset| {
            let bucket = bucket.clone();
            let key = key.clone();
            let hash_hex = hash_hex.clone();
            let metrics = metrics.clone();
            async move {
                if offset >= total {
                    return Ok::<_, std::io::Error>(None);
                }
                let end = (offset + S3_WINDOW).min(total) - 1;
                let expected = end - offset + 1;
                let resp = with_retry("get", metrics.as_ref(), || {
                    bucket.get_object_range(&key, offset, Some(end))
                })
                .await
                .map_err(|e| {
                    std::io::Error::other(format!("get_object_range failed after retries: {e}"))
                })?;

                let code = resp.status_code();
                if !(200..300).contains(&code) {
                    return Err(std::io::Error::other(format!(
                        "S3 range get returned status {code} for {hash_hex} at offset {offset}/{total}"
                    )));
                }

                let chunk = resp.bytes().clone();
                // The authoritative `total` says exactly `expected` bytes remain
                // in this window. Anything else - empty body, short read, or an
                // over-read - is truncation/corruption from a misbehaving
                // backend, NOT EOF. Fail loud so io::copy errors and the fetch
                // is retried, never silently cached under the correct hash.
                if chunk.len() as u64 != expected {
                    return Err(std::io::Error::other(format!(
                        "short range read for {hash_hex}: got {} bytes, expected {expected} at offset {offset}/{total}",
                        chunk.len()
                    )));
                }
                let next = offset + chunk.len() as u64;
                Ok(Some((chunk, next)))
            }
        });

        // `Box::pin` makes the stream `Unpin` (try_unfold's future is not),
        // which `StreamReader`/`BoxReader` require.
        Ok(Box::new(tokio_util::io::StreamReader::new(Box::pin(
            stream,
        ))))
    }

    async fn get_range(
        &self,
        hash: &ContentHash,
        offset: u64,
        len: usize,
    ) -> Result<(Vec<u8>, bool), StorageError> {
        if len == 0 {
            return Ok((Vec::new(), false));
        }

        let key = Self::object_key(hash);
        let end = offset.saturating_add(len as u64).saturating_sub(1);
        let response = with_retry("get", self.metrics.as_ref(), || {
            self.bucket.get_object_range(&key, offset, Some(end))
        })
        .await
        .map_err(|e| {
            StorageError::Backend(format!("get_object_range failed after retries: {e}"))
        })?;

        if response.status_code() == 404 {
            return Err(StorageError::NotFound(hash.to_hex()));
        }
        if response.status_code() == 416 {
            return Ok((Vec::new(), true));
        }
        if !(200..300).contains(&response.status_code()) {
            return Err(StorageError::Backend(format!(
                "S3 range get returned status {}",
                response.status_code()
            )));
        }

        let bytes = response.to_vec();
        let eof = bytes.len() < len;
        Ok((bytes, eof))
    }

    async fn exists(&self, hash: &ContentHash) -> Result<bool, StorageError> {
        Ok(self.head(&Self::object_key(hash)).await?.is_some())
    }

    async fn delete(&self, hash: &ContentHash) -> Result<bool, StorageError> {
        let key = Self::object_key(hash);

        let status = with_retry("delete", self.metrics.as_ref(), || {
            self.bucket.delete_object(&key)
        })
        .await
        .map_err(|e| StorageError::Backend(format!("delete_object failed after retries: {e}")))?
        .status();

        // Distinguish the three terminal outcomes instead of collapsing
        // everything-but-204 to `Ok(false)`. SeaweedFS returns 204 (or 200) for a
        // successful delete and 404 when the key was already gone - both are a
        // definite "no longer present", reported as Ok(true)/Ok(false). A 5xx
        // that survives the retry loop is a genuine backend failure: surfacing it
        // as Err prevents a caller from recording a still-live blob as deleted.
        match status {
            200 | 204 => Ok(true),
            404 => Ok(false),
            other => Err(StorageError::Backend(format!(
                "S3 delete returned status {other}"
            ))),
        }
    }

    async fn size(&self, hash: &ContentHash) -> Result<u64, StorageError> {
        self.content_length(hash).await
    }
}

/// A throwaway SeaweedFS S3 gateway for tests that exercise real object
/// storage. Needs Docker.
#[cfg(any(test, feature = "testing"))]
pub mod testing {
    use s3::creds::Credentials;
    use s3::{Bucket, Region};
    use testcontainers::core::{IntoContainerPort, WaitFor};
    use testcontainers::runners::AsyncRunner;
    use testcontainers::{ContainerAsync, GenericImage, ImageExt};

    use super::{ObjectStorageBlobStore, ObjectStorageConfig};

    const ACCESS_KEY: &str = "ak";
    const SECRET_KEY: &str = "sk";
    const REGION: &str = "us-east-1";
    const S3_CONFIG: &str = r#"{"identities":[{"name":"t","credentials":[{"accessKey":"ak","secretKey":"sk"}],"actions":["Admin","Read","Write","List"]}]}"#;

    /// A running SeaweedFS container; it stops when dropped.
    pub struct Seaweedfs {
        _container: ContainerAsync<GenericImage>,
        endpoint: String,
    }

    impl Seaweedfs {
        pub async fn start() -> Self {
            let container = GenericImage::new("chrislusf/seaweedfs", "4.15")
                .with_exposed_port(8333.tcp())
                .with_wait_for(WaitFor::message_on_stderr("Start Seaweed S3 API Server"))
                .with_copy_to("/etc/s3.json", S3_CONFIG.as_bytes().to_vec())
                .with_cmd(["server", "-s3", "-s3.config=/etc/s3.json", "-dir=/data"])
                .start()
                .await
                .expect("start seaweedfs");
            let port = container.get_host_port_ipv4(8333).await.expect("port");
            Self {
                _container: container,
                endpoint: format!("http://127.0.0.1:{port}"),
            }
        }

        /// Creates `bucket`. The filer behind the S3 gateway can lag the
        /// gateway's "started" log line, so creation is retried until it
        /// succeeds.
        pub async fn create_bucket(&self, bucket: &str) {
            let region = Region::Custom {
                region: REGION.into(),
                endpoint: self.endpoint.clone(),
            };
            let creds =
                Credentials::new(Some(ACCESS_KEY), Some(SECRET_KEY), None, None, None).unwrap();
            for _ in 0..20 {
                if let Ok(resp) = Bucket::create_with_path_style(
                    bucket,
                    region.clone(),
                    creds.clone(),
                    s3::BucketConfiguration::default(),
                )
                .await
                    && resp.success()
                {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            panic!("could not create bucket {bucket}");
        }

        /// A blob store on `bucket`, which need not exist.
        pub fn store(&self, bucket: &str) -> ObjectStorageBlobStore {
            ObjectStorageBlobStore::new(
                ObjectStorageConfig {
                    bucket: bucket.to_string(),
                    region: REGION.into(),
                    endpoint: Some(self.endpoint.clone()),
                    access_key: Some(ACCESS_KEY.into()),
                    secret_key: Some(SECRET_KEY.into()),
                    path_style: true,
                    max_size: 1 << 20,
                    temp_dir: None,
                },
                None,
            )
            .expect("store")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct Status(u16);
    impl S3Status for Status {
        fn status(&self) -> u16 {
            self.0
        }
    }

    /// Feed `with_retry` a scripted sequence of outcomes; returns the final
    /// outcome's status (or `None` for an error) and how many calls were made.
    async fn script(outcomes: Vec<Result<u16, S3Error>>) -> (Option<u16>, u32) {
        let calls = AtomicU32::new(0);
        let outcomes = std::sync::Mutex::new(outcomes.into_iter());
        let out = with_retry("test", None, || {
            calls.fetch_add(1, Ordering::SeqCst);
            let next = outcomes.lock().unwrap().next().expect("scripted outcome");
            async move { next.map(Status) }
        })
        .await;
        (out.ok().map(|s| s.0), calls.load(Ordering::SeqCst))
    }

    #[tokio::test]
    async fn retries_transient_failures_until_success() {
        let (status, calls) = script(vec![Err(S3Error::HttpFail), Ok(503), Ok(200)]).await;
        assert_eq!((status, calls), (Some(200), 3));
    }

    #[tokio::test]
    async fn gives_up_after_the_retry_budget() {
        let (status, calls) = script(vec![Ok(500), Ok(502), Ok(503), Ok(504)]).await;
        assert_eq!((status, calls), (Some(504), MAX_S3_RETRIES + 1));
    }

    #[tokio::test]
    async fn terminal_outcomes_are_not_retried() {
        for terminal in [200, 404, 416, 403] {
            assert_eq!(script(vec![Ok(terminal)]).await, (Some(terminal), 1));
        }
        let (status, calls) =
            script(vec![Err(S3Error::HttpFailWithBody(404, String::new()))]).await;
        assert_eq!((status, calls), (None, 1));
    }

    /// Against a real SeaweedFS: a missing bucket refuses boot with a clear
    /// error, an existing one passes, and the read paths classify a missing
    /// object as absent.
    #[tokio::test]
    async fn verify_bucket_against_seaweedfs() {
        let seaweedfs = testing::Seaweedfs::start().await;

        let missing = seaweedfs.store("no-such-bucket");
        let err = missing.verify_bucket().await.expect_err("missing bucket");
        assert!(err.to_string().contains("does not exist"), "{err}");

        seaweedfs.create_bucket("broccoli-blobs").await;
        let present = seaweedfs.store("broccoli-blobs");
        present.verify_bucket().await.expect("existing bucket");

        let absent = ContentHash::from_bytes([7u8; 32]);
        assert!(!present.exists(&absent).await.expect("exists"));
        assert!(matches!(
            present.size(&absent).await,
            Err(StorageError::NotFound(_))
        ));
        let hash = present.put(b"hello").await.expect("put");
        assert!(present.exists(&hash).await.expect("exists"));
        assert_eq!(present.size(&hash).await.expect("size"), 5);
        assert_eq!(
            present.get_range(&hash, 1, 3).await.expect("range").0,
            b"ell".to_vec()
        );
    }
}
