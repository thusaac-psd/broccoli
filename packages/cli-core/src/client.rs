use anyhow::Context;
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

use crate::credentials::{CredentialsFile, StoredTokens};
use crate::session::{Exchange, Session, SessionError};

/// Why a request failed, in a form callers can act on.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The session is rejected (log in again) or temporarily unavailable.
    #[error(transparent)]
    Session(#[from] SessionError),
    /// The server could not be reached.
    #[error("Could not reach {server}: {cause}. Check your network connection.")]
    Network { server: String, cause: String },
    /// The server answered with an error.
    #[error(transparent)]
    Api(#[from] ApiError),
    /// The server answered 2xx with a body this client cannot read.
    #[error("Failed to parse server response: {0}")]
    Decode(String),
}

pub type ClientResult<T> = Result<T, ClientError>;

type Response = http::Response<ureq::Body>;

/// API client over one [`Session`]. On a 401 it refreshes the session once and
/// resends the request. Clones of the session share its token rotation, so the
/// client is `Send + Sync` and can be shared across threads.
pub struct Client {
    session: Session,
    agent: ureq::Agent,
}

impl Client {
    pub fn new(session: Session) -> Self {
        let agent = crate::tls::build_agent(
            Some(std::time::Duration::from_secs(10)),
            Some(std::time::Duration::from_secs(30)),
        );
        Self { session, agent }
    }

    /// A client for the unauthenticated endpoints (login, device flow).
    pub fn anonymous(server: &str) -> Self {
        Self::new(Session::explicit(server.to_string(), String::new()))
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn server(&self) -> &str {
        self.session.server()
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.server(), path)
    }

    fn network(&self, e: ureq::Error) -> ClientError {
        ClientError::Network {
            server: self.server().to_string(),
            cause: e.to_string(),
        }
    }

    /// Send an authenticated request built by `send` (given the bearer
    /// header). On 401 the session is refreshed and the request resent once.
    fn authorized(
        &self,
        send: impl Fn(&str) -> Result<Response, ureq::Error>,
    ) -> ClientResult<Response> {
        let token = self.session.bearer()?;
        let resp = send(&format!("Bearer {token}")).map_err(|e| self.network(e))?;
        if resp.status().as_u16() != 401 {
            return Ok(resp);
        }
        self.session.refresh(&token, |refresh| self.exchange(refresh))?;
        let token = self.session.bearer()?;
        send(&format!("Bearer {token}")).map_err(|e| self.network(e))
    }

    /// Present a refresh token to `POST /auth/cli-refresh`.
    fn exchange(&self, refresh_token: &str) -> Exchange {
        let sent = self
            .agent
            .post(&self.url("/api/v1/auth/cli-refresh"))
            .send_json(CliRefreshRequest {
                refresh_token: refresh_token.to_string(),
            });
        let resp = match sent {
            Ok(resp) => resp,
            Err(e) => return Exchange::Unreachable(e.to_string()),
        };
        match resp.status().as_u16() {
            200 => match resp.into_body().read_json::<CliTokenResponse>() {
                Ok(body) => Exchange::Rotated(StoredTokens {
                    token: body.token,
                    refresh_token: body.refresh_token,
                }),
                Err(e) => Exchange::Unreachable(format!("unreadable refresh response: {e}")),
            },
            // Too many requests, a timeout or a server failure says nothing
            // about the refresh token; any other client error refuses it.
            408 | 429 => Exchange::Unreachable(ApiError::from_response(resp).to_string()),
            status if status >= 500 => {
                Exchange::Unreachable(ApiError::from_response(resp).to_string())
            }
            _ => Exchange::Refused,
        }
    }

    fn get(&self, path: &str) -> ClientResult<Response> {
        let url = self.url(path);
        self.authorized(|bearer| self.agent.get(&url).header("Authorization", bearer).call())
    }

    fn post<B: Serialize>(&self, path: &str, body: &B) -> ClientResult<Response> {
        let url = self.url(path);
        self.authorized(|bearer| {
            self.agent
                .post(&url)
                .header("Authorization", bearer)
                .send_json(body)
        })
    }

    fn delete(&self, path: &str) -> ClientResult<Response> {
        let url = self.url(path);
        self.authorized(|bearer| {
            self.agent
                .delete(&url)
                .header("Authorization", bearer)
                .call()
        })
    }

    /// POST to an endpoint that needs no session.
    fn post_anonymous<B: Serialize>(&self, path: &str, body: &B) -> ClientResult<Response> {
        self.agent
            .post(&self.url(path))
            .send_json(body)
            .map_err(|e| self.network(e))
    }

    fn check_status(resp: Response) -> ClientResult<Response> {
        if resp.status().is_success() {
            Ok(resp)
        } else {
            Err(ApiError::from_response(resp).into())
        }
    }

    fn parse_response<T: DeserializeOwned>(&self, resp: Response) -> ClientResult<T> {
        Self::check_status(resp)?
            .into_body()
            .read_json::<T>()
            .map_err(|e| ClientError::Decode(e.to_string()))
    }

    fn parse_empty(&self, resp: Response) -> ClientResult<()> {
        Self::check_status(resp).map(drop)
    }

    /// Upload a packaged plugin archive to the admin plugin-upload endpoint as
    /// `multipart/form-data`, retrying transient failures with exponential
    /// backoff. A token that expires mid `plugin watch` is refreshed like any
    /// other request.
    pub fn upload_plugin(&self, archive: &[u8]) -> ClientResult<()> {
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
        let url = self.url("/api/v1/admin/plugins/upload");

        let mut attempt = 0;
        loop {
            let result = self
                .authorized(|bearer| {
                    self.agent
                        .post(&url)
                        .header("Authorization", bearer)
                        .header("Content-Type", &content_type)
                        .send(&body[..])
                })
                .and_then(|resp| self.parse_empty(resp));
            let transient = match &result {
                Err(ClientError::Network { .. }) => true,
                Err(ClientError::Api(e)) => e.status >= 500,
                _ => false,
            };
            if !transient || attempt == MAX_RETRIES {
                return result;
            }
            std::thread::sleep(INITIAL_DELAY * 2u32.pow(attempt));
            attempt += 1;
        }
    }

    pub fn login(&self, username: &str, password: &str) -> ClientResult<LoginResponse> {
        let body = LoginRequest {
            username: username.to_string(),
            password: password.to_string(),
        };
        let resp = self.post_anonymous("/api/v1/auth/login", &body)?;
        self.parse_response(resp)
    }

    pub fn request_device_code(&self) -> ClientResult<DeviceCodeResponse> {
        let resp = self.post_anonymous("/api/v1/auth/device-code", &serde_json::json!({}))?;
        self.parse_response(resp)
    }

    /// Poll the device flow. The server answers a pending, slowed-down or
    /// expired code with 400 and an RFC 8628 `error`, which is returned as a
    /// [`PollResponse`] rather than an error.
    pub fn poll_device_token(&self, device_code: &str) -> ClientResult<PollResponse> {
        let body = DeviceTokenRequest {
            device_code: device_code.to_string(),
        };
        let resp = self.post_anonymous("/api/v1/auth/device-token", &body)?;
        let resp = match resp.status().as_u16() {
            200 | 400 => resp,
            _ => return Err(ApiError::from_response(resp).into()),
        };
        resp.into_body()
            .read_json()
            .map_err(|e| ClientError::Decode(e.to_string()))
    }

    pub fn me(&self) -> ClientResult<MeResponse> {
        let resp = self.get("/api/v1/auth/me")?;
        self.parse_response(resp)
    }

    /// Exchange the access token for a refresh token; called once after login.
    pub fn issue_cli_token(&self) -> ClientResult<CliTokenResponse> {
        let resp = self.post("/api/v1/auth/cli-token", &serde_json::json!({}))?;
        self.parse_response(resp)
    }

    pub fn list_contests(&self) -> ClientResult<ContestListResponse> {
        let resp = self.get("/api/v1/contests")?;
        self.parse_response(resp)
    }

    pub fn get_contest(&self, id: &str) -> ClientResult<ContestResponse> {
        let resp = self.get(&format!("/api/v1/contests/{}", id))?;
        self.parse_response(resp)
    }

    pub fn get_contest_my_info(&self, id: &str) -> ClientResult<ContestUserContextResponse> {
        let resp = self.get(&format!("/api/v1/contests/{}/me", id))?;
        self.parse_response(resp)
    }

    pub fn register_for_contest(&self, id: &str) -> ClientResult<()> {
        let resp = self.post(
            &format!("/api/v1/contests/{}/register", id),
            &serde_json::json!({}),
        )?;
        self.parse_empty(resp)
    }

    /// DELETE on the register path.
    pub fn unregister_from_contest(&self, id: &str) -> ClientResult<()> {
        let resp = self.delete(&format!("/api/v1/contests/{}/register", id))?;
        self.parse_empty(resp)
    }

    pub fn list_contest_problems(
        &self,
        contest_id: &str,
    ) -> ClientResult<Vec<ContestProblemResponse>> {
        let resp = self.get(&format!("/api/v1/contests/{}/problems", contest_id))?;
        self.parse_response(resp)
    }

    pub fn list_clarifications(
        &self,
        contest_id: &str,
    ) -> ClientResult<ClarificationListResponse> {
        let resp = self.get(&format!("/api/v1/contests/{}/clarifications", contest_id))?;
        self.parse_response(resp)
    }

    pub fn create_clarification(
        &self,
        contest_id: &str,
        content: &str,
    ) -> ClientResult<ClarificationResponse> {
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
    pub fn get_problem(&self, problem_id: &str) -> ClientResult<ProblemResponse> {
        let resp = self.get(&format!("/api/v1/problems/{}", problem_id))?;
        self.parse_response(resp)
    }

    /// A problem's statement, read through the contest it belongs to so the
    /// server decides visibility for that contest only.
    pub fn get_contest_problem(
        &self,
        contest_id: &str,
        problem_id: &str,
    ) -> ClientResult<ProblemResponse> {
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
    ) -> ClientResult<ProblemSamplesResponse> {
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
    ) -> ClientResult<SubmissionResponse> {
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
    ) -> ClientResult<SubmissionResponse> {
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

    pub fn get_submission(&self, id: &str) -> ClientResult<SubmissionResponse> {
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
    ) -> ClientResult<SubmissionListResponse> {
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
    pub fn get_code_run(&self, id: i32) -> ClientResult<CodeRunResponse> {
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
    ) -> ClientResult<CodeRunResponse> {
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
    ) -> ClientResult<CodeRunResponse> {
        let body = RunCodeRequest {
            files,
            language: language.to_string(),
            custom_test_cases,
        };
        let resp = self.post(&format!("/api/v1/problems/{}/code-runs", problem_id), &body)?;
        self.parse_response(resp)
    }
}

/// Exchange a freshly obtained access token (device flow, browser callback or
/// password login) for a new access token plus a refresh token, and save the
/// pair to `credentials.json`. Nothing is saved unless both tokens were issued.
pub fn persist_session(server: &str, access_token: &str) -> anyhow::Result<()> {
    let client = Client::new(Session::explicit(
        server.to_string(),
        access_token.to_string(),
    ));
    let issued = client
        .issue_cli_token()
        .context("Failed to obtain a refresh token")?;
    CredentialsFile::default_location()
        .lock()?
        .write(
            server,
            &StoredTokens {
                token: issued.token,
                refresh_token: issued.refresh_token,
            },
        )
        .context("Failed to save credentials")
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
