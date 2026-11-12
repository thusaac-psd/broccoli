use anyhow::{Context, bail};
use broccoli_api_types::auth::{
    CliRefreshRequest, CliTokenResponse, DeviceCodeResponse, DeviceTokenRequest, LoginRequest,
    LoginResponse, MeResponse,
};
use broccoli_api_types::clarification::{
    ClarificationListResponse, ClarificationResponse, CreateClarificationRequest,
};
use broccoli_api_types::code_run::{CodeRunResponse, CustomTestCaseInput, RunCodeRequest};
use broccoli_api_types::contest::{
    ContestListResponse, ContestProblemResponse, ContestResponse, ContestUserContextResponse,
};
use broccoli_api_types::error::ErrorBody;
use broccoli_api_types::problem::{ProblemResponse, ProblemSamplesResponse};
use broccoli_api_types::submission::{
    CreateSubmissionRequest, SubmissionFileDto, SubmissionListResponse, SubmissionResponse,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::config::{Credentials, save_credentials, save_credentials_full};
use crate::tokenstore::{TokenStore, Tokens};

/// Shared API client; refreshes the access token and retries once on 401.
///
/// The token pair lives in a [`TokenStore`] rather than the client, so the
/// client is `Send + Sync` and several clients (or threads) can share one token
/// rotation instead of each wrapping the whole client in `Arc<Mutex<Client>>`.
pub struct Client {
    server: String,
    tokens: TokenStore,
    agent: ureq::Agent,
}

impl Client {
    /// Build a client that persists refreshed tokens back to the credentials
    /// file (the default for a stand-alone CLI process).
    pub fn new(creds: Credentials) -> Self {
        let server = creds.server;
        let persist_server = server.clone();
        let tokens = TokenStore::new(
            Tokens {
                token: creds.token,
                refresh_token: creds.refresh_token,
            },
            move |t| {
                let _ =
                    save_credentials_full(&persist_server, &t.token, t.refresh_token.as_deref());
            },
        );
        Self::with_token_store(server, tokens)
    }

    /// Build a client over an externally-owned [`TokenStore`], so multiple
    /// clients can share one token rotation (and one persistence policy).
    pub fn with_token_store(server: String, tokens: TokenStore) -> Self {
        let agent = crate::tls::build_agent(
            Some(std::time::Duration::from_secs(10)),
            Some(std::time::Duration::from_secs(30)),
        );
        Self {
            server,
            tokens,
            agent,
        }
    }

    /// The shared token store, for constructing sibling clients that share this
    /// client's token rotation.
    pub fn token_store(&self) -> TokenStore {
        self.tokens.clone()
    }

    pub fn server(&self) -> &str {
        &self.server
    }

    fn bearer(&self) -> String {
        format!("Bearer {}", self.tokens.access_token())
    }

    /// Upload a packaged plugin archive to the admin plugin-upload endpoint as
    /// `multipart/form-data`, refreshing the access token on 401 and retrying
    /// transient 5xx failures with exponential backoff. Routing this through the
    /// client (instead of a raw HTTP call) means a token that expires mid
    /// `plugin watch` session is refreshed rather than failing the upload.
    pub fn upload_plugin(&self, archive: &[u8]) -> anyhow::Result<()> {
        const MAX_RETRIES: u32 = 3;
        const INITIAL_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

        let boundary = "----broccolicli7MA4YWxkTrZu0gWupload";
        let mut body = Vec::with_capacity(archive.len() + 256);
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            b"Content-Disposition: form-data; name=\"plugin\"; filename=\"plugin.tar.gz\"\r\n",
        );
        body.extend_from_slice(b"Content-Type: application/gzip\r\n\r\n");
        body.extend_from_slice(archive);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let content_type = format!("multipart/form-data; boundary={boundary}");
        let url = format!("{}/api/v1/admin/plugins/upload", self.server);

        let send = || {
            self.agent
                .post(&url)
                .header("Authorization", &self.bearer())
                .header("Content-Type", &content_type)
                .send(&body[..])
                .with_context(|| {
                    format!(
                        "Could not reach {} — check your network connection.",
                        self.server
                    )
                })
        };

        let mut last_err: Option<String> = None;
        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                std::thread::sleep(INITIAL_DELAY * 2u32.pow(attempt - 1));
            }

            let used = self.tokens.access_token();
            let resp = match send() {
                Ok(r) => r,
                Err(e) => {
                    last_err = Some(e.to_string());
                    continue;
                }
            };
            let resp = self.with_refresh(&used, resp, send)?;

            let status = resp.status().as_u16();
            if (200..300).contains(&status) {
                return Ok(());
            }
            let text = resp.into_body().read_to_string().unwrap_or_default();
            if status == 401 {
                bail!("Authentication failed (401). Run `broccoli login` to re-authenticate.");
            }
            if status >= 500 {
                last_err = Some(format!("Upload failed ({status}): {text}"));
                continue;
            }
            bail!("Upload failed ({status}): {text}");
        }

        bail!(
            "{}. Giving up after {} attempts",
            last_err.unwrap_or_else(|| "Upload failed".into()),
            MAX_RETRIES + 1
        );
    }

    /// Refresh the access token, serialized and deduplicated across threads by
    /// the token store. `used` is the access token the failed request carried.
    fn refresh_on_401(&self, used: &str) -> anyhow::Result<()> {
        self.tokens.refresh_if_current(used, |refresh_token| {
            let resp = self
                .agent
                .post(&format!("{}/api/v1/auth/cli-refresh", self.server))
                .send_json(CliRefreshRequest {
                    refresh_token: refresh_token.to_string(),
                })
                .with_context(|| {
                    format!(
                        "Could not reach {} — check your network connection.",
                        self.server
                    )
                })?;

            if resp.status().as_u16() != 200 {
                bail!("Session expired. Run `broccoli login` to re-authenticate.");
            }

            let body: CliTokenResponse = resp
                .into_body()
                .read_json()
                .context("Failed to parse refresh response")?;

            Ok(Tokens {
                token: body.token,
                refresh_token: Some(body.refresh_token),
            })
        })
    }

    /// On 401 with a refresh token, refresh and re-run `retry`; else return `resp`.
    /// `used` is the access token `retry`'s failed attempt carried, so a racing
    /// refresh by another thread is detected and not repeated.
    fn with_refresh<F>(
        &self,
        used: &str,
        resp: http::Response<ureq::Body>,
        retry: F,
    ) -> anyhow::Result<http::Response<ureq::Body>>
    where
        F: FnOnce() -> anyhow::Result<http::Response<ureq::Body>>,
    {
        let needs_refresh = resp.status().as_u16() == 401 && self.tokens.refresh_token().is_some();
        if needs_refresh && self.refresh_on_401(used).is_ok() {
            return retry();
        }
        Ok(resp)
    }

    fn get(&self, path: &str) -> anyhow::Result<http::Response<ureq::Body>> {
        let url = format!("{}{}", self.server, path);
        let send = || {
            self.agent
                .get(&url)
                .header("Authorization", &self.bearer())
                .call()
                .with_context(|| {
                    format!(
                        "Could not reach {} — check your network connection.",
                        self.server
                    )
                })
        };
        let used = self.tokens.access_token();
        let resp = send()?;
        self.with_refresh(&used, resp, send)
    }

    fn post<B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> anyhow::Result<http::Response<ureq::Body>> {
        let url = format!("{}{}", self.server, path);
        let send = || {
            self.agent
                .post(&url)
                .header("Authorization", &self.bearer())
                .send_json(body)
                .with_context(|| {
                    format!(
                        "Could not reach {} — check your network connection.",
                        self.server
                    )
                })
        };
        let used = self.tokens.access_token();
        let resp = send()?;
        self.with_refresh(&used, resp, send)
    }

    fn delete(&self, path: &str) -> anyhow::Result<http::Response<ureq::Body>> {
        let url = format!("{}{}", self.server, path);
        let send = || {
            self.agent
                .delete(&url)
                .header("Authorization", &self.bearer())
                .call()
                .with_context(|| {
                    format!(
                        "Could not reach {} — check your network connection.",
                        self.server
                    )
                })
        };
        let used = self.tokens.access_token();
        let resp = send()?;
        self.with_refresh(&used, resp, send)
    }

    /// Bail on non-2xx with a clean message; return `resp` on success.
    fn check_status(
        resp: http::Response<ureq::Body>,
    ) -> anyhow::Result<http::Response<ureq::Body>> {
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(resp);
        }
        Err(ApiError::from_response(resp).into())
    }

    /// Check status, then parse the body.
    fn parse_response<T: DeserializeOwned>(
        &self,
        resp: http::Response<ureq::Body>,
    ) -> anyhow::Result<T> {
        let resp = Self::check_status(resp)?;
        resp.into_body()
            .read_json::<T>()
            .context("Failed to parse server response")
    }

    /// Check status for a no-body response.
    fn parse_empty(&self, resp: http::Response<ureq::Body>) -> anyhow::Result<()> {
        Self::check_status(resp)?;
        Ok(())
    }

    pub fn login(&self, username: &str, password: &str) -> anyhow::Result<LoginResponse> {
        let body = LoginRequest {
            username: username.to_string(),
            password: password.to_string(),
        };
        let resp = self
            .agent
            .post(&format!("{}/api/v1/auth/login", self.server))
            .send_json(&body)
            .with_context(|| {
                format!(
                    "Could not reach {} — check your network connection.",
                    self.server
                )
            })?;
        self.parse_response(resp)
    }

    pub fn request_device_code(&self) -> anyhow::Result<DeviceCodeResponse> {
        let resp = self
            .agent
            .post(&format!("{}/api/v1/auth/device-code", self.server))
            .send_json(serde_json::json!({}))
            .with_context(|| {
                format!(
                    "Could not reach {} — check your network connection.",
                    self.server
                )
            })?;
        self.parse_response(resp)
    }

    pub fn poll_device_token(&self, device_code: &str) -> anyhow::Result<PollResponse> {
        let body = DeviceTokenRequest {
            device_code: device_code.to_string(),
        };
        let resp = self
            .agent
            .post(&format!("{}/api/v1/auth/device-token", self.server))
            .send_json(&body)
            .with_context(|| {
                format!(
                    "Could not reach {} — check your network connection.",
                    self.server
                )
            })?;
        self.parse_response(resp)
    }

    pub fn me(&self) -> anyhow::Result<MeResponse> {
        let resp = self.get("/api/v1/auth/me")?;
        self.parse_response(resp)
    }

    /// Exchange the access token for a refresh token; called once after login.
    pub fn issue_cli_token(&self) -> anyhow::Result<CliTokenResponse> {
        let resp = self.post("/api/v1/auth/cli-token", &serde_json::json!({}))?;
        self.parse_response(resp)
    }

    pub fn list_contests(&self) -> anyhow::Result<ContestListResponse> {
        let resp = self.get("/api/v1/contests")?;
        self.parse_response(resp)
    }

    pub fn get_contest(&self, id: &str) -> anyhow::Result<ContestResponse> {
        let resp = self.get(&format!("/api/v1/contests/{}", id))?;
        self.parse_response(resp)
    }

    pub fn get_contest_my_info(&self, id: &str) -> anyhow::Result<ContestUserContextResponse> {
        let resp = self.get(&format!("/api/v1/contests/{}/me", id))?;
        self.parse_response(resp)
    }

    pub fn register_for_contest(&self, id: &str) -> anyhow::Result<()> {
        let resp = self.post(
            &format!("/api/v1/contests/{}/register", id),
            &serde_json::json!({}),
        )?;
        self.parse_empty(resp)
    }

    /// DELETE on the register path.
    pub fn unregister_from_contest(&self, id: &str) -> anyhow::Result<()> {
        let resp = self.delete(&format!("/api/v1/contests/{}/register", id))?;
        self.parse_empty(resp)
    }

    pub fn list_contest_problems(
        &self,
        contest_id: &str,
    ) -> anyhow::Result<Vec<ContestProblemResponse>> {
        let resp = self.get(&format!("/api/v1/contests/{}/problems", contest_id))?;
        self.parse_response(resp)
    }

    pub fn list_clarifications(
        &self,
        contest_id: &str,
    ) -> anyhow::Result<ClarificationListResponse> {
        let resp = self.get(&format!("/api/v1/contests/{}/clarifications", contest_id))?;
        self.parse_response(resp)
    }

    pub fn create_clarification(
        &self,
        contest_id: &str,
        content: &str,
    ) -> anyhow::Result<ClarificationResponse> {
        let body = CreateClarificationRequest {
            content: content.to_string(),
            clarification_type: "question".to_string(),
            recipient_id: None,
            is_public: None,
        };
        let resp = self.post(
            &format!("/api/v1/contests/{}/clarifications", contest_id),
            &body,
        )?;
        self.parse_response(resp)
    }

    /// A standalone problem, read outside every contest. A problem reached
    /// through a contest must use [`Client::get_contest_problem`] instead, or
    /// another contest holding the same problem can hide it.
    pub fn get_problem(&self, problem_id: &str) -> anyhow::Result<ProblemResponse> {
        let resp = self.get(&format!("/api/v1/problems/{}", problem_id))?;
        self.parse_response(resp)
    }

    /// A problem's statement, read through the contest it belongs to so the
    /// server decides visibility for that contest only.
    pub fn get_contest_problem(
        &self,
        contest_id: &str,
        problem_id: &str,
    ) -> anyhow::Result<ProblemResponse> {
        let resp = self.get(&format!(
            "/api/v1/contests/{}/problems/{}",
            contest_id, problem_id
        ))?;
        self.parse_response(resp)
    }

    pub fn get_contest_problem_samples(
        &self,
        contest_id: &str,
        problem_id: &str,
    ) -> anyhow::Result<ProblemSamplesResponse> {
        let resp = self.get(&format!(
            "/api/v1/contests/{}/problems/{}/samples",
            contest_id, problem_id
        ))?;
        self.parse_response(resp)
    }

    pub fn create_contest_submission(
        &self,
        contest_id: &str,
        problem_id: &str,
        files: Vec<SubmissionFileDto>,
        language: &str,
        contest_type: Option<&str>,
    ) -> anyhow::Result<SubmissionResponse> {
        let body = CreateSubmissionRequest {
            files,
            language: language.to_string(),
            contest_type: contest_type.map(String::from),
        };
        let resp = self.post(
            &format!(
                "/api/v1/contests/{}/problems/{}/submissions",
                contest_id, problem_id
            ),
            &body,
        )?;
        self.parse_response(resp)
    }

    /// Standalone submission, outside any contest.
    pub fn create_submission(
        &self,
        problem_id: &str,
        files: Vec<SubmissionFileDto>,
        language: &str,
        contest_type: Option<&str>,
    ) -> anyhow::Result<SubmissionResponse> {
        let body = CreateSubmissionRequest {
            files,
            language: language.to_string(),
            contest_type: contest_type.map(String::from),
        };
        let resp = self.post(
            &format!("/api/v1/problems/{}/submissions", problem_id),
            &body,
        )?;
        self.parse_response(resp)
    }

    pub fn get_submission(&self, id: &str) -> anyhow::Result<SubmissionResponse> {
        let resp = self.get(&format!("/api/v1/submissions/{}", id))?;
        self.parse_response(resp)
    }

    /// List submissions for a contest/problem with pagination; `user_id` scopes to one contestant.
    pub fn list_contest_submissions(
        &self,
        contest_id: &str,
        problem_id: Option<&str>,
        user_id: Option<&str>,
        page: Option<u32>,
        per_page: Option<u32>,
    ) -> anyhow::Result<SubmissionListResponse> {
        let mut path = format!("/api/v1/contests/{}/submissions", contest_id);
        let mut params: Vec<String> = Vec::new();
        if let Some(pid) = problem_id {
            params.push(format!("problem_id={}", pid));
        }
        if let Some(uid) = user_id {
            params.push(format!("user_id={}", uid));
        }
        if let Some(p) = page {
            params.push(format!("page={}", p));
        }
        if let Some(pp) = per_page {
            params.push(format!("per_page={}", pp));
        }
        if !params.is_empty() {
            path.push('?');
            path.push_str(&params.join("&"));
        }
        let resp = self.get(&path)?;
        self.parse_response(resp)
    }

    /// Fetch a code-run by ID (used to poll for asynchronous results).
    pub fn get_code_run(&self, id: i32) -> anyhow::Result<CodeRunResponse> {
        let resp = self.get(&format!("/api/v1/code-runs/{}", id))?;
        self.parse_response(resp)
    }

    /// Server requires 1-10 custom test cases.
    pub fn run_contest_code(
        &self,
        contest_id: &str,
        problem_id: &str,
        files: Vec<SubmissionFileDto>,
        language: &str,
        custom_test_cases: Vec<CustomTestCaseInput>,
    ) -> anyhow::Result<CodeRunResponse> {
        let body = RunCodeRequest {
            files,
            language: language.to_string(),
            custom_test_cases,
        };
        let resp = self.post(
            &format!(
                "/api/v1/contests/{}/problems/{}/code-runs",
                contest_id, problem_id
            ),
            &body,
        )?;
        self.parse_response(resp)
    }

    /// Standalone problem, outside any contest.
    pub fn run_code(
        &self,
        problem_id: &str,
        files: Vec<SubmissionFileDto>,
        language: &str,
        custom_test_cases: Vec<CustomTestCaseInput>,
    ) -> anyhow::Result<CodeRunResponse> {
        let body = RunCodeRequest {
            files,
            language: language.to_string(),
            custom_test_cases,
        };
        let resp = self.post(&format!("/api/v1/problems/{}/code-runs", problem_id), &body)?;
        self.parse_response(resp)
    }
}

/// Exchange the short-lived device-flow/callback access token for a fresh
/// access token plus a long-lived refresh token, and store both so later
/// commands can keep refreshing instead of dying when the access token
/// expires. Older servers without `/auth/cli-token` fall back to storing the
/// access token alone.
pub fn persist_session(server: &str, access_token: &str) -> anyhow::Result<()> {
    let client = Client::new(Credentials {
        server: server.to_string(),
        token: access_token.to_string(),
        refresh_token: None,
    });
    match client.issue_cli_token() {
        Ok(cli) => save_credentials_full(server, &cli.token, Some(&cli.refresh_token))
            .context("Failed to save credentials")?,
        Err(_) => save_credentials(server, access_token).context("Failed to save credentials")?,
    }
    Ok(())
}

/// A request the server answered with a non-2xx status. `code` is the
/// machine-readable [`ErrorBody::code`] (an API contract); callers match on it
/// instead of on the human message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub status: u16,
    pub code: String,
    pub message: String,
}

impl ApiError {
    /// The `code` of a body that is not an [`ErrorBody`] (a proxy page, an
    /// empty body).
    pub const UNSTRUCTURED: &'static str = "UNSTRUCTURED";

    fn from_response(resp: http::Response<ureq::Body>) -> Self {
        let status = resp.status().as_u16();
        let text = resp.into_body().read_to_string().unwrap_or_default();
        match serde_json::from_str::<ErrorBody>(&text) {
            Ok(body) => Self {
                status,
                code: body.code,
                message: body.message,
            },
            Err(_) => Self {
                status,
                code: Self::UNSTRUCTURED.to_string(),
                message: text,
            },
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.code.as_str() {
            "TOKEN_MISSING" | "TOKEN_INVALID" => write!(
                f,
                "{} Run `broccoli login` to re-authenticate.",
                self.message
            ),
            Self::UNSTRUCTURED if self.message.is_empty() => {
                write!(f, "Server returned {}", self.status)
            }
            Self::UNSTRUCTURED => write!(f, "Server returned {}: {}", self.status, self.message),
            _ => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for ApiError {}

/// Body of `POST /auth/device-token`: `token` once the device is authorized,
/// otherwise an RFC 8628 `error`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PollResponse {
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
}
