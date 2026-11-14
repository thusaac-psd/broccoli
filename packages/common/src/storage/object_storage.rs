use std::path::PathBuf;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use reqwest::Method;
use reqwest::header::{HeaderMap, HeaderValue, RANGE};
use sha2::{Digest, Sha256};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, warn};

use opentelemetry::KeyValue;

use crate::metrics::Metrics;

use super::error::StorageError;
use super::hash::ContentHash;
use super::s3::{
    EMPTY_PAYLOAD_SHA256, S3Client, S3Request, S3Response, S3TransportError, is_server_failure,
};
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
    s3: std::sync::Arc<S3Client>,
    max_size: u64,
    temp_dir: PathBuf,
    metrics: Option<Metrics>,
    /// The last [`BlobStore::probe`] outcome and when it was taken.
    last_probe: std::sync::Mutex<Option<(Instant, Result<(), String>)>>,
}

impl ObjectStorageBlobStore {
    pub fn new(
        config: ObjectStorageConfig,
        metrics: Option<Metrics>,
    ) -> Result<Self, StorageError> {
        let endpoint = config
            .endpoint
            .clone()
            .filter(|e| !e.trim().is_empty())
            .unwrap_or_else(|| format!("https://s3.{}.amazonaws.com", config.region));
        let credentials = match (config.access_key, config.secret_key) {
            (Some(access), Some(secret)) if !access.is_empty() => Some((access, secret)),
            _ => None,
        };
        let s3 = S3Client::new(
            endpoint,
            config.bucket,
            config.region,
            config.path_style,
            credentials,
        )
        .map_err(|e| StorageError::Backend(format!("failed to build the S3 client: {e}")))?;

        Ok(Self {
            s3: std::sync::Arc::new(s3),
            max_size: config.max_size,
            temp_dir: config.temp_dir.unwrap_or_else(std::env::temp_dir),
            metrics,
            last_probe: std::sync::Mutex::new(None),
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

/// The key [`ObjectStorageBlobStore::verify_bucket`] reads: content keys are
/// `<2 hex>/<62 hex>`, so no blob is ever stored here.
const BUCKET_CHECK_KEY: &str = "broccoli-bucket-check";

const MAX_S3_RETRIES: u32 = 3;
/// Retries start only within this long of an operation's first attempt, so
/// with the client's connect and read timeouts an unreachable or stalled
/// storage server fails the operation in well under a request's deadline.
const RETRY_WINDOW: Duration = Duration::from_secs(10);
const S3_WINDOW: u64 = 8 * 1024 * 1024;
/// How long a [`BlobStore::probe`] answer is reused.
const PROBE_TTL: Duration = Duration::from_secs(5);
/// How long a probe waits for the storage server: inside a health check's
/// budget, so a slow store is reported (and remembered) as down.
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// Log + count a retry of a transient S3 call. Centralised so every retry site
/// (get/head/put/delete) is observable identically: a WARN line (visible in
/// logs and the trace) plus the `broccoli.blob_store.retries` counter, labelled
/// by operation, for contest-day dashboards/alerts.
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
    tokio::time::sleep(Duration::from_millis(jitter)).await;
}

/// The one retry policy for every S3 call: up to [`MAX_S3_RETRIES`] retries,
/// all started within [`RETRY_WINDOW`], with jittered backoff on transport
/// errors (including timeouts) and server failures (5xx, 429). Every other
/// response is handed back for the caller to classify; what is still failing
/// when retries run out is [`StorageError::Unavailable`]. `call` builds a
/// fresh request each attempt, body included.
async fn with_retry<F, Fut>(
    op: &'static str,
    metrics: Option<&Metrics>,
    mut call: F,
) -> Result<S3Response, StorageError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<S3Response, S3TransportError>>,
{
    let started = Instant::now();
    let mut attempt = 0u32;
    loop {
        let outcome = call().await;
        let reason = match &outcome {
            Ok(resp) if is_server_failure(resp.status) => format!("status {}", resp.status),
            Ok(_) => return Ok(outcome.expect("matched Ok")),
            Err(S3TransportError::Body(e)) => {
                return Err(StorageError::Io(std::io::Error::new(
                    e.kind(),
                    e.to_string(),
                )));
            }
            Err(e) => format!("transport error: {}", error_chain(e)),
        };
        if attempt >= MAX_S3_RETRIES || started.elapsed() >= RETRY_WINDOW {
            return Err(StorageError::Unavailable(format!(
                "object storage {op} failed after {} attempts: {reason}",
                attempt + 1
            )));
        }
        attempt += 1;
        record_retry(metrics, op, attempt, &reason);
        retry_backoff(attempt).await;
    }
}

/// `e` and every error under it, for a log line that names the real cause
/// ("connection refused", "operation timed out") rather than only the top.
fn error_chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut source = e.source();
    while let Some(cause) = source {
        out.push_str(": ");
        out.push_str(&cause.to_string());
        source = cause.source();
    }
    out
}

fn unexpected(op: &str, resp: &S3Response) -> StorageError {
    StorageError::Backend(format!(
        "S3 {op} returned status {}{}",
        resp.status,
        resp.error_code()
            .map(|c| format!(" ({c})"))
            .unwrap_or_default()
    ))
}

impl ObjectStorageBlobStore {
    fn request<'a>(&self, method: Method, key: &'a str) -> S3Request<'a> {
        S3Request {
            method,
            key,
            headers: HeaderMap::new(),
            payload_sha256: EMPTY_PAYLOAD_SHA256,
        }
    }

    /// Send a bodiless request with the retry policy.
    async fn call(
        &self,
        op: &'static str,
        request: S3Request<'_>,
    ) -> Result<S3Response, StorageError> {
        with_retry(op, self.metrics.as_ref(), || {
            self.s3
                .send(&request, || async { Ok(reqwest::Body::from(Vec::new())) })
        })
        .await
    }

    /// The object's length, or `None` when it does not exist.
    async fn head(&self, key: &str) -> Result<Option<u64>, StorageError> {
        let resp = self.call("head", self.request(Method::HEAD, key)).await?;
        match resp.status {
            404 => Ok(None),
            200..300 => resp.content_length().map(Some).ok_or_else(|| {
                StorageError::Backend("HEAD response missing content-length".into())
            }),
            _ => Err(unexpected("head", &resp)),
        }
    }

    /// The object's length from a HEAD, or `NotFound`.
    async fn content_length(&self, hash: &ContentHash) -> Result<u64, StorageError> {
        self.head(&Self::object_key(hash))
            .await?
            .ok_or_else(|| StorageError::NotFound(hash.to_hex()))
    }

    /// Check at boot that the configured bucket exists and these credentials
    /// can reach it (S3 HeadBucket, with the usual retry), naming the cause
    /// when they cannot. Without this, a missing bucket (e.g. an infra init job
    /// that failed quietly) lets servers and workers boot "healthy" and fail on
    /// the first upload or fetch. Clock skew is never the cause: requests are
    /// signed at the storage server's time.
    ///
    /// The bucket is deliberately not created here: it is shared by every
    /// server and worker, so auto-creating it would turn a typo in one node's
    /// config into a private, empty bucket for that node alone.
    pub async fn verify_bucket(&self) -> Result<(), StorageError> {
        let bucket = self.s3.bucket();
        let endpoint = self.s3.endpoint();
        // A HEAD carries no error body to tell a missing bucket from bad
        // credentials, so the check GETs a key that never exists: `NoSuchKey`
        // means the bucket is there and these credentials may read it.
        let resp = match self
            .call("bucket_check", self.request(Method::GET, BUCKET_CHECK_KEY))
            .await
        {
            Ok(resp) => resp,
            Err(StorageError::Unavailable(e)) => {
                return Err(StorageError::Unavailable(format!(
                    "object storage at {endpoint} is unreachable: {e}"
                )));
            }
            Err(e) => return Err(e),
        };
        match (resp.status, resp.error_code()) {
            (200..300, _) | (404, Some("NoSuchKey")) => Ok(()),
            (404, _) => Err(StorageError::Backend(format!(
                "object storage bucket `{bucket}` does not exist at {endpoint}. Create it (the \
                 infra init job normally does) or fix storage.object_storage.bucket"
            ))),
            (_, Some("InvalidAccessKeyId")) => Err(StorageError::Backend(format!(
                "object storage at {endpoint} does not know the access key: check \
                 storage.object_storage.access_key"
            ))),
            (_, Some("SignatureDoesNotMatch")) => Err(StorageError::Backend(format!(
                "object storage at {endpoint} rejected the request signature: check \
                 storage.object_storage.secret_key"
            ))),
            (401 | 403, code) => Err(StorageError::Backend(format!(
                "object storage refused access to bucket `{bucket}` (HTTP {}{}): grant these \
                 credentials read and write on it",
                resp.status,
                code.map(|c| format!(", {c}")).unwrap_or_default()
            ))),
            _ => Err(unexpected("bucket check", &resp)),
        }
    }

    /// GET bytes `[start, end]` of `key`.
    async fn get_range_raw(
        s3: &S3Client,
        metrics: Option<&Metrics>,
        key: &str,
        start: u64,
        end: u64,
    ) -> Result<S3Response, StorageError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            RANGE,
            HeaderValue::from_str(&format!("bytes={start}-{end}")).expect("ASCII range"),
        );
        let request = S3Request {
            method: Method::GET,
            key,
            headers,
            payload_sha256: EMPTY_PAYLOAD_SHA256,
        };
        with_retry("get", metrics, || {
            s3.send(&request, || async { Ok(reqwest::Body::from(Vec::new())) })
        })
        .await
    }

    #[cfg(test)]
    pub(crate) fn s3(&self) -> &S3Client {
        &self.s3
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
            let payload_sha256 = hash.to_hex();
            let mut headers = HeaderMap::new();
            headers.insert(
                reqwest::header::CONTENT_LENGTH,
                HeaderValue::from(total_bytes),
            );
            let request = S3Request {
                method: Method::PUT,
                key: &key,
                headers,
                payload_sha256: &payload_sha256,
            };
            // Every attempt RE-OPENS the temp file and streams it from byte 0:
            // a body that is resent from a spent reader would store a short
            // object under the full content's hash.
            let upload = &temp_path;
            let resp = with_retry("put", self.metrics.as_ref(), || {
                self.s3.send(&request, || async move {
                    let file = fs::File::open(upload).await?;
                    Ok(reqwest::Body::wrap_stream(
                        tokio_util::io::ReaderStream::new(file),
                    ))
                })
            })
            .await?;
            if !(200..300).contains(&resp.status) {
                return Err(unexpected("put", &resp));
            }

            Ok(hash)
        }
        .await;

        let _ = fs::remove_file(&temp_path).await;

        result
    }

    async fn get_stream(&self, hash: &ContentHash) -> Result<BoxReader, StorageError> {
        let key = Self::object_key(hash);

        // The object streams as explicit byte ranges rather than through
        // rust-s3's `get_object_stream`, whose `AsyncRead` adapter can end a
        // chunked-transfer response from SeaweedFS after its first chunk. Each
        // ranged GET has a definite Content-Length and reassembles losslessly,
        // while memory stays bounded to a single window regardless of blob
        // size.
        //
        // A HEAD up front gives us the AUTHORITATIVE total length (so the loop
        // terminates against a real total, not a fragile "short read == EOF"
        // heuristic) and surfaces a missing blob as `NotFound` before we hand
        // back a reader. The HEAD reuses the pooled connection and is cheap
        // relative to the body transfer. Both HEAD and every window carry the
        // bounded transient retry.
        let total = self.content_length(hash).await?;

        let s3 = self.s3.clone();
        let hash_hex = hash.to_hex();
        let metrics = self.metrics.clone();

        let stream = futures::stream::try_unfold(0u64, move |offset| {
            let s3 = s3.clone();
            let key = key.clone();
            let hash_hex = hash_hex.clone();
            let metrics = metrics.clone();
            async move {
                if offset >= total {
                    return Ok::<_, std::io::Error>(None);
                }
                let end = (offset + S3_WINDOW).min(total) - 1;
                let expected = end - offset + 1;
                let resp = Self::get_range_raw(&s3, metrics.as_ref(), &key, offset, end)
                    .await
                    .map_err(std::io::Error::other)?;
                if !(200..300).contains(&resp.status) {
                    return Err(std::io::Error::other(format!(
                        "S3 range get returned status {} for {hash_hex} at offset {offset}/{total}",
                        resp.status
                    )));
                }
                let chunk = resp.body;
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
        let response =
            Self::get_range_raw(&self.s3, self.metrics.as_ref(), &key, offset, end).await?;
        match response.status {
            404 => Err(StorageError::NotFound(hash.to_hex())),
            416 => Ok((Vec::new(), true)),
            200..300 => {
                let bytes = response.body.to_vec();
                let eof = bytes.len() < len;
                Ok((bytes, eof))
            }
            _ => Err(unexpected("range get", &response)),
        }
    }

    async fn exists(&self, hash: &ContentHash) -> Result<bool, StorageError> {
        Ok(self.head(&Self::object_key(hash)).await?.is_some())
    }

    /// One bucket check, with no retry, answered from the last check for
    /// [`PROBE_TTL`] so a health endpoint polled often costs the storage
    /// server next to nothing.
    async fn probe(&self) -> Result<(), StorageError> {
        if let Some((at, outcome)) = &*self.last_probe.lock().unwrap_or_else(|e| e.into_inner())
            && at.elapsed() < PROBE_TTL
        {
            return outcome.clone().map_err(StorageError::Unavailable);
        }
        let request = self.request(Method::GET, BUCKET_CHECK_KEY);
        let check = self
            .s3
            .send(&request, || async { Ok(reqwest::Body::from(Vec::new())) });
        let outcome = match tokio::time::timeout(PROBE_TIMEOUT, check)
            .await
            .unwrap_or_else(|_| Err(S3TransportError::Body(std::io::ErrorKind::TimedOut.into())))
        {
            Ok(resp)
                if (200..300).contains(&resp.status) || resp.error_code() == Some("NoSuchKey") =>
            {
                Ok(())
            }
            Ok(resp) => Err(format!("bucket check returned status {}", resp.status)),
            Err(e) => Err(e.to_string()),
        };
        *self.last_probe.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((Instant::now(), outcome.clone()));
        outcome.map_err(StorageError::Unavailable)
    }

    async fn delete(&self, hash: &ContentHash) -> Result<bool, StorageError> {
        let key = Self::object_key(hash);

        let status = self
            .call("delete", self.request(Method::DELETE, &key))
            .await?
            .status;

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
    use testcontainers::core::{IntoContainerPort, WaitFor};
    use testcontainers::runners::AsyncRunner;
    use testcontainers::{ContainerAsync, GenericImage, ImageExt};

    use super::{ObjectStorageBlobStore, ObjectStorageConfig};
    use crate::storage::s3::{EMPTY_PAYLOAD_SHA256, S3Client, S3Request};

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

        pub fn endpoint(&self) -> &str {
            &self.endpoint
        }

        /// Creates `bucket`. The filer behind the S3 gateway can lag the
        /// gateway's "started" log line, so creation is retried until it
        /// succeeds.
        pub async fn create_bucket(&self, bucket: &str) {
            let s3 = S3Client::new(
                self.endpoint.clone(),
                bucket.into(),
                REGION.into(),
                true,
                Some((ACCESS_KEY.into(), SECRET_KEY.into())),
            )
            .expect("client");
            let request = S3Request {
                method: reqwest::Method::PUT,
                key: "",
                headers: Default::default(),
                payload_sha256: EMPTY_PAYLOAD_SHA256,
            };
            // Then a first write, which waits while SeaweedFS grows its first
            // volume, so tests do not spend their own retries on that. An
            // empty object needs no volume, so the write carries a byte.
            let byte_sha256 = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(b"w"));
            let warmup = S3Request {
                key: "warmup",
                payload_sha256: &byte_sha256,
                ..request.clone()
            };
            for (request, body) in [(&request, &b""[..]), (&warmup, &b"w"[..])] {
                let mut done = false;
                for _ in 0..60 {
                    if let Ok(resp) = s3
                        .send(request, || async { Ok(reqwest::Body::from(body.to_vec())) })
                        .await
                        && (200..300).contains(&resp.status)
                    {
                        done = true;
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                assert!(done, "could not create and write bucket {bucket}");
            }
        }

        /// A blob store on `bucket`, which need not exist.
        pub fn store(&self, bucket: &str) -> ObjectStorageBlobStore {
            self.store_with(bucket, ACCESS_KEY, SECRET_KEY)
        }

        /// A blob store on `bucket` signing with these credentials.
        pub fn store_with(
            &self,
            bucket: &str,
            access_key: &str,
            secret_key: &str,
        ) -> ObjectStorageBlobStore {
            ObjectStorageBlobStore::new(
                ObjectStorageConfig {
                    bucket: bucket.to_string(),
                    region: REGION.into(),
                    endpoint: Some(self.endpoint.clone()),
                    access_key: Some(access_key.into()),
                    secret_key: Some(secret_key.into()),
                    path_style: true,
                    max_size: 64 << 20,
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

    fn response(status: u16) -> S3Response {
        S3Response {
            status,
            headers: HeaderMap::new(),
            body: Default::default(),
        }
    }

    fn unreadable_body() -> S3TransportError {
        S3TransportError::Body(std::io::Error::other("unused"))
    }

    /// Feed `with_retry` a scripted sequence of outcomes (`None` for a
    /// transport failure); returns the final status (or `None` when the store
    /// is unavailable) and how many calls were made.
    async fn script(outcomes: Vec<Option<u16>>) -> (Option<u16>, u32) {
        let calls = AtomicU32::new(0);
        let outcomes = std::sync::Mutex::new(outcomes.into_iter());
        let out = with_retry("test", None, || {
            calls.fetch_add(1, Ordering::SeqCst);
            let next = outcomes.lock().unwrap().next().expect("scripted outcome");
            async move {
                match next {
                    Some(status) => Ok(response(status)),
                    // A connection that cannot be made, as an HTTP error.
                    None => Err(S3TransportError::Http(
                        reqwest::Client::new()
                            .get("http://127.0.0.1:1/")
                            .send()
                            .await
                            .expect_err("nothing listens on port 1"),
                    )),
                }
            }
        })
        .await;
        let status = match out {
            Ok(resp) => Some(resp.status),
            Err(StorageError::Unavailable(_)) => None,
            Err(e) => panic!("{e}"),
        };
        (status, calls.load(Ordering::SeqCst))
    }

    #[tokio::test(start_paused = true)]
    async fn retries_transient_failures_until_success() {
        assert_eq!(
            script(vec![None, Some(503), Some(200)]).await,
            (Some(200), 3)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn what_still_fails_after_the_retry_budget_is_unavailable() {
        let (status, calls) = script(vec![Some(500), Some(502), None, Some(504)]).await;
        assert_eq!((status, calls), (None, MAX_S3_RETRIES + 1));
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_outcomes_are_not_retried() {
        for terminal in [200, 404, 416, 403] {
            assert_eq!(script(vec![Some(terminal)]).await, (Some(terminal), 1));
        }
    }

    #[tokio::test]
    async fn an_unreadable_upload_body_is_an_io_error_not_an_outage() {
        let out = with_retry("put", None, || async { Err(unreadable_body()) }).await;
        assert!(matches!(out, Err(StorageError::Io(_))));
    }

    /// Storage that does not answer is an outage, reported as such by reads,
    /// writes, the boot check and the health probe alike.
    #[tokio::test(start_paused = true)]
    async fn unreachable_storage_is_unavailable() {
        let store = ObjectStorageBlobStore::new(
            ObjectStorageConfig {
                bucket: "b".into(),
                region: "us-east-1".into(),
                endpoint: Some("http://127.0.0.1:1".into()),
                access_key: Some("ak".into()),
                secret_key: Some("sk".into()),
                path_style: true,
                max_size: 1 << 20,
                temp_dir: None,
            },
            None,
        )
        .unwrap();
        let hash = ContentHash::from_bytes([1u8; 32]);
        assert!(matches!(
            store.put(b"x").await,
            Err(StorageError::Unavailable(_))
        ));
        assert!(matches!(
            store.exists(&hash).await,
            Err(StorageError::Unavailable(_))
        ));
        assert!(matches!(
            store.verify_bucket().await,
            Err(StorageError::Unavailable(_))
        ));
        assert!(matches!(
            store.probe().await,
            Err(StorageError::Unavailable(_))
        ));
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
        present.probe().await.expect("probe");

        let err = seaweedfs
            .store_with("broccoli-blobs", "ak", "wrong-secret")
            .verify_bucket()
            .await
            .expect_err("wrong secret");
        assert!(err.to_string().contains("secret_key"), "{err}");
        let err = seaweedfs
            .store_with("broccoli-blobs", "nobody", "sk")
            .verify_bucket()
            .await
            .expect_err("unknown access key");
        assert!(err.to_string().contains("access_key"), "{err}");

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
        let big: Vec<u8> = (0..(S3_WINDOW as usize * 2 + 17))
            .map(|i| i as u8)
            .collect();
        let big_hash = present.put(&big).await.expect("put big");
        assert_eq!(present.get(&big_hash).await.expect("get big"), big);
        assert!(present.delete(&big_hash).await.expect("delete"));
        assert!(!present.exists(&big_hash).await.expect("exists"));
    }

    /// A node whose clock is hours off the storage server's still reads and
    /// writes: the server refuses its first signature as skewed, the store
    /// re-learns the server's time from that refusal and the request goes
    /// through. Driven by skewing the learned clock, not the host's.
    #[tokio::test]
    async fn a_node_with_a_skewed_clock_keeps_its_blob_io() {
        let seaweedfs = testing::Seaweedfs::start().await;
        seaweedfs.create_bucket("skewed").await;
        let store = seaweedfs.store("skewed");
        store.verify_bucket().await.expect("bucket");
        let hash = store.put(b"before").await.expect("put");

        for skew in [chrono::Duration::hours(3), chrono::Duration::hours(-5)] {
            store.s3().clock().skew(skew);
            assert_eq!(store.get(&hash).await.expect("get while skewed"), b"before");
            let learned = store.s3().clock().now().unwrap();
            store.s3().clock().skew(skew);
            store.put(b"while skewed").await.expect("put while skewed");
            let relearned = store.s3().clock().now().unwrap();
            assert!(
                (relearned - learned).num_minutes().abs() < 15,
                "re-learned {relearned} vs {learned}"
            );
        }
    }
}
