//! The S3 requests the object-storage blob store makes, signed (AWS SigV4) at
//! the storage server's time rather than this node's.
//!
//! An S3 server refuses a request whose signing time is more than 15 minutes
//! from its own clock, and contest machines often run without NTP, so a node
//! signing with its own clock could lose all blob I/O. [`ServerClock`] learns
//! the storage server's time from the `Date` header of its first response and
//! advances it with this node's monotonic clock; a `RequestTimeTooSkewed`
//! refusal re-learns it from that refusal and the request is sent once more.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use bytes::Bytes;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use reqwest::header::{DATE, HeaderMap, HeaderValue};
use reqwest::{Method, StatusCode};
use sha2::{Digest, Sha256};

/// Opening a connection to the storage server.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// The longest the storage server may stay silent within one response (a
/// header, or the next bytes of a body). A stalled or paused server fails
/// the request after this, well inside an HTTP request's deadline, however
/// large the object. Room for the seconds SeaweedFS takes to grow a volume on
/// a write.
const READ_TIMEOUT: Duration = Duration::from_secs(15);

/// A refusal dated this far from its request's signing time is taken for a
/// clock-skew refusal. S3 servers tolerate 15 minutes.
const SKEW_EVIDENCE: chrono::TimeDelta = chrono::TimeDelta::minutes(5);

/// The SHA-256 of an empty payload, which every bodiless request signs.
pub(crate) const EMPTY_PAYLOAD_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// The storage server's wall clock, as last learned from it, advanced by this
/// node's monotonic clock in between. Never this node's wall clock.
#[derive(Default)]
pub(crate) struct ServerClock {
    learned: Mutex<Option<(Instant, DateTime<Utc>)>>,
}

impl ServerClock {
    pub(crate) fn now(&self) -> Option<DateTime<Utc>> {
        let learned = *self.learned.lock().unwrap_or_else(|e| e.into_inner());
        learned.map(|(at, time)| time + elapsed(at))
    }

    /// Take the server's time from a response's `Date` header. Returns whether
    /// the header was there to learn from.
    pub(crate) fn learn(&self, headers: &HeaderMap) -> bool {
        let Some(time) = server_date(headers) else {
            return false;
        };
        *self.learned.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), time));
        true
    }

    /// Pretend this node learned a time `by` away from the server's, as a node
    /// with a skewed clock would sign.
    #[cfg(test)]
    pub(crate) fn skew(&self, by: chrono::Duration) {
        let mut learned = self.learned.lock().unwrap();
        let (at, time) = learned.expect("learned");
        *learned = Some((at, time + by));
    }
}

fn elapsed(at: Instant) -> chrono::Duration {
    chrono::Duration::from_std(at.elapsed()).unwrap_or(chrono::Duration::MAX)
}

fn server_date(headers: &HeaderMap) -> Option<DateTime<Utc>> {
    let date = headers.get(DATE)?.to_str().ok()?;
    httpdate::parse_http_date(date)
        .ok()
        .map(DateTime::<Utc>::from)
}

/// Where the bucket is and who signs for it.
pub(crate) struct S3Client {
    http: reqwest::Client,
    /// `scheme://host[:port]`, no trailing slash.
    endpoint: String,
    bucket: String,
    region: String,
    path_style: bool,
    credentials: Option<(String, String)>,
    clock: ServerClock,
}

/// A response with its body read.
pub(crate) struct S3Response {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl S3Response {
    /// The S3 error code (`<Code>` of the XML error body), if any.
    pub fn error_code(&self) -> Option<&str> {
        let body = std::str::from_utf8(&self.body).ok()?;
        let start = body.find("<Code>")? + "<Code>".len();
        let end = start + body[start..].find("</Code>")?;
        Some(&body[start..end])
    }

    pub fn content_length(&self) -> Option<u64> {
        self.headers
            .get(reqwest::header::CONTENT_LENGTH)?
            .to_str()
            .ok()?
            .parse()
            .ok()
    }

    /// Whether this refuses a request signed at `signed_at` for clock skew:
    /// it says so, or (a HEAD reply has no body to say it in) it is a 403
    /// dated further from `signed_at` than any server tolerates.
    fn is_clock_skew(&self, signed_at: DateTime<Utc>) -> bool {
        if self.status != 403 {
            return false;
        }
        match self.error_code() {
            Some(code) => code == "RequestTimeTooSkewed",
            None => server_date(&self.headers)
                .is_some_and(|date| (date - signed_at).abs() > SKEW_EVIDENCE),
        }
    }
}

/// One request to send: what it does and what it carries.
#[derive(Clone)]
pub(crate) struct S3Request<'a> {
    pub method: Method,
    /// The object key; empty for the bucket itself.
    pub key: &'a str,
    pub headers: HeaderMap,
    /// The SHA-256 (hex) of the body the request sends.
    pub payload_sha256: &'a str,
}

impl S3Client {
    pub(crate) fn new(
        endpoint: String,
        bucket: String,
        region: String,
        path_style: bool,
        credentials: Option<(String, String)>,
    ) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .build()?;
        Ok(Self {
            http,
            endpoint: endpoint.trim_end_matches('/').to_string(),
            bucket,
            region,
            path_style,
            credentials,
            clock: ServerClock::default(),
        })
    }

    pub(crate) fn bucket(&self) -> &str {
        &self.bucket
    }

    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    #[cfg(test)]
    pub(crate) fn clock(&self) -> &ServerClock {
        &self.clock
    }

    /// The URL of `key` and the `Host` it is signed for.
    fn url_and_host(&self, key: &str) -> (String, String) {
        let (scheme, authority) = self
            .endpoint
            .split_once("://")
            .unwrap_or(("https", self.endpoint.as_str()));
        let path = encode_path(key);
        if self.path_style {
            let url = format!("{scheme}://{authority}/{}/{path}", self.bucket);
            (url, authority.to_string())
        } else {
            let host = format!("{}.{authority}", self.bucket);
            (format!("{scheme}://{host}/{path}"), host)
        }
    }

    fn canonical_uri(&self, key: &str) -> String {
        let path = encode_path(key);
        if self.path_style {
            format!("/{}/{path}", self.bucket)
        } else {
            format!("/{path}")
        }
    }

    /// The storage server's time, learned from it first if this client has
    /// not heard from it yet.
    async fn server_now(&self) -> Result<DateTime<Utc>, reqwest::Error> {
        if let Some(now) = self.clock.now() {
            return Ok(now);
        }
        // Any answer carries the server's `Date`; an unsigned HEAD of the
        // endpoint costs it nothing.
        let probe = self.http.head(&self.endpoint).send().await?;
        if !self.clock.learn(probe.headers()) {
            tracing::warn!(
                endpoint = %self.endpoint,
                "Object storage sent no Date header; signing with this node's clock"
            );
            #[allow(clippy::disallowed_methods)]
            let local = Utc::now();
            *self.clock.learned.lock().unwrap_or_else(|e| e.into_inner()) =
                Some((Instant::now(), local));
        }
        Ok(self.clock.now().expect("learned"))
    }

    /// Send `request` with the body `body` builds, signed at the server's
    /// time. A refusal for clock skew re-learns the time from the refusal and
    /// sends the request once more, with a fresh body.
    pub(crate) async fn send<B, F, Fut>(
        &self,
        request: &S3Request<'_>,
        mut body: F,
    ) -> Result<S3Response, S3TransportError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = std::io::Result<B>>,
        B: Into<reqwest::Body>,
    {
        let mut relearned = false;
        loop {
            let at = self.server_now().await?;
            let response = self.send_at(request, body().await?.into(), at).await?;
            if response.is_clock_skew(at) && !relearned && self.clock.learn(&response.headers) {
                tracing::warn!(
                    endpoint = %self.endpoint,
                    "Object storage refused this node's signing time; re-learned its clock"
                );
                relearned = true;
                continue;
            }
            return Ok(response);
        }
    }

    async fn send_at(
        &self,
        request: &S3Request<'_>,
        body: reqwest::Body,
        at: DateTime<Utc>,
    ) -> Result<S3Response, reqwest::Error> {
        let (url, host) = self.url_and_host(request.key);
        let amz_date = at.format("%Y%m%dT%H%M%SZ").to_string();
        let mut headers = request.headers.clone();
        headers.insert("x-amz-date", header(&amz_date));
        headers.insert("x-amz-content-sha256", header(request.payload_sha256));
        if let Some((access_key, secret_key)) = &self.credentials {
            let authorization = sign(&SigningInput {
                method: request.method.as_str(),
                canonical_uri: &self.canonical_uri(request.key),
                host: &host,
                amz_date: &amz_date,
                payload_sha256: request.payload_sha256,
                region: &self.region,
                access_key,
                secret_key,
            });
            headers.insert(reqwest::header::AUTHORIZATION, header(&authorization));
        }
        let response = self
            .http
            .request(request.method.clone(), url)
            .headers(headers)
            .body(body)
            .send()
            .await?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let body = response.bytes().await?;
        Ok(S3Response {
            status,
            headers,
            body,
        })
    }
}

fn header(value: &str) -> HeaderValue {
    HeaderValue::from_str(value).expect("signing headers are ASCII")
}

/// The request never got an answer.
#[derive(Debug, thiserror::Error)]
pub(crate) enum S3TransportError {
    #[error("{0}")]
    Http(#[from] reqwest::Error),
    #[error("reading the upload: {0}")]
    Body(#[from] std::io::Error),
}

/// Whether a response status is the server failing rather than refusing.
pub(crate) fn is_server_failure(status: u16) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS.as_u16() || (500..600).contains(&status)
}

struct SigningInput<'a> {
    method: &'a str,
    canonical_uri: &'a str,
    host: &'a str,
    amz_date: &'a str,
    payload_sha256: &'a str,
    region: &'a str,
    access_key: &'a str,
    secret_key: &'a str,
}

const SIGNED_HEADERS: &str = "host;x-amz-content-sha256;x-amz-date";

/// The `Authorization` header of an AWS SigV4 request with no query string,
/// signing `Host`, `x-amz-content-sha256` and `x-amz-date`.
fn sign(input: &SigningInput<'_>) -> String {
    let canonical_request = format!(
        "{method}\n{uri}\n\nhost:{host}\nx-amz-content-sha256:{payload}\nx-amz-date:{date}\n\n{SIGNED_HEADERS}\n{payload}",
        method = input.method,
        uri = input.canonical_uri,
        host = input.host,
        payload = input.payload_sha256,
        date = input.amz_date,
    );
    let day = &input.amz_date[..8];
    let scope = format!("{day}/{}/s3/aws4_request", input.region);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{scope}\n{}",
        input.amz_date,
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );
    let key = [day, input.region, "s3", "aws4_request"].iter().fold(
        format!("AWS4{}", input.secret_key).into_bytes(),
        |key, part| hmac_sha256(&key, part.as_bytes()),
    );
    let signature = hex::encode(hmac_sha256(&key, string_to_sign.as_bytes()));
    format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={SIGNED_HEADERS}, Signature={signature}",
        input.access_key
    )
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// `key` percent-encoded for a URI path as SigV4 canonicalizes it: every byte
/// but the unreserved ones and `/`.
fn encode_path(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for byte in key.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn reads_the_error_code_of_an_s3_error_body() {
        let response = S3Response {
            status: 403,
            headers: HeaderMap::new(),
            body: Bytes::from_static(
                b"<?xml version=\"1.0\"?><Error><Code>RequestTimeTooSkewed</Code>\
                  <Message>skewed</Message></Error>",
            ),
        };
        assert_eq!(response.error_code(), Some("RequestTimeTooSkewed"));
        assert!(response.is_clock_skew(Utc::now()));
    }

    /// A HEAD refusal has no body: a 403 dated far from the signing time is
    /// skew, one dated near it is a plain refusal.
    #[test]
    fn a_bodiless_refusal_is_skew_when_dated_far_from_the_signature() {
        let signed_at = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let refusal = |server: DateTime<Utc>| {
            let mut headers = HeaderMap::new();
            headers.insert(DATE, header(&httpdate::fmt_http_date(server.into())));
            S3Response {
                status: 403,
                headers,
                body: Bytes::new(),
            }
        };
        assert!(refusal(signed_at + chrono::Duration::hours(2)).is_clock_skew(signed_at));
        assert!(!refusal(signed_at + chrono::Duration::seconds(30)).is_clock_skew(signed_at));
    }

    #[test]
    fn encodes_keys_as_sigv4_canonicalizes_them() {
        assert_eq!(encode_path("ab/cd-ef_1.~"), "ab/cd-ef_1.~");
        assert_eq!(encode_path("a b+c"), "a%20b%2Bc");
    }
}
