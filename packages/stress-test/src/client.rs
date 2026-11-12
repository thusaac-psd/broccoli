use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use reqwest::{StatusCode, multipart};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::dto::{
    AddContestProblemRequest, BulkAddParticipantsRequest, BulkAddParticipantsResponse,
    ContestProblemResponse, ContestResponse, CreateContestRequest, CreateProblemRequest,
    CreateSubmissionRequest, CreateTestCaseRequest, ErrorBody, LoginRequest, LoginResponse,
    ProblemResponse, RegistriesResponse, RunCodeRequest, SubmissionResponse, TestCaseListItem,
    TestCaseResponse,
};
use crate::error::{StressError, StressResult};

#[derive(Debug, Clone)]
pub enum AuthCreds {
    Token(String),
    UsernamePassword { username: String, password: String },
}

#[derive(Debug)]
struct Inner {
    http: reqwest::Client,
    base_url: String,
    creds: AuthCreds,
    token: RwLock<String>,
    run_id: String,
    request_seq: AtomicU64,
}

#[derive(Debug, Clone)]
pub struct Client(Arc<Inner>);

impl Client {
    pub async fn new(base_url: String, creds: AuthCreds) -> StressResult<Self> {
        Self::new_with_run_id(base_url, creds, format!("stress-{}", Uuid::now_v7())).await
    }

    pub async fn new_with_run_id(
        base_url: String,
        creds: AuthCreds,
        run_id: String,
    ) -> StressResult<Self> {
        let http = reqwest::Client::builder()
            .build()
            .map_err(StressError::Network)?;

        let initial_token = match &creds {
            AuthCreds::Token(t) => t.clone(),
            AuthCreds::UsernamePassword { .. } => String::new(),
        };

        let client = Client(Arc::new(Inner {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            creds: creds.clone(),
            token: RwLock::new(initial_token),
            run_id,
            request_seq: AtomicU64::new(0),
        }));

        if matches!(client.0.creds, AuthCreds::UsernamePassword { .. }) {
            client.login().await?;
        }

        Ok(client)
    }

    pub fn run_id(&self) -> &str {
        &self.0.run_id
    }

    pub fn base_url(&self) -> &str {
        &self.0.base_url
    }

    pub fn supports_login_probe(&self) -> bool {
        matches!(self.0.creds, AuthCreds::UsernamePassword { .. })
    }

    fn next_request_id(&self) -> String {
        let next = self.0.request_seq.fetch_add(1, Ordering::Relaxed) + 1;
        format!("{}-{}", self.0.run_id, next)
    }

    fn instrument_request(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request
            .header("x-broccoli-stress-run-id", self.0.run_id.as_str())
            .header("x-request-id", self.next_request_id())
    }

    pub async fn login(&self) -> StressResult<()> {
        let (username, password) = match &self.0.creds {
            AuthCreds::UsernamePassword { username, password } => {
                (username.clone(), password.clone())
            }
            AuthCreds::Token(_) => {
                return Err(StressError::Auth(
                    "client built with a static token cannot re-login".into(),
                ));
            }
        };

        let url = format!("{}/api/v1/auth/login", self.0.base_url);
        let body = LoginRequest { username, password };

        let resp = self
            .instrument_request(self.0.http.post(&url))
            .json(&body)
            .send()
            .await
            .map_err(StressError::Network)?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(StressError::Auth(format!(
                "login returned {}: {}",
                status.as_u16(),
                text
            )));
        }

        let parsed: LoginResponse = resp
            .json()
            .await
            .map_err(|e| StressError::Decode(format!("login response: {}", e)))?;

        let mut token = self.0.token.write().await;
        *token = parsed.token;
        Ok(())
    }

    pub async fn list_registries(&self) -> StressResult<RegistriesResponse> {
        let url = format!("{}/api/v1/plugins/registries", self.0.base_url);
        let resp = self.instrument_request(self.0.http.get(url)).send().await?;
        if !resp.status().is_success() {
            return Err(drain_error(resp).await);
        }
        decode_json(resp).await
    }

    pub async fn create_problem(
        &self,
        req: &CreateProblemRequest,
    ) -> StressResult<ProblemResponse> {
        self.send_json_with_retry(reqwest::Method::POST, "/api/v1/problems", Some(req))
            .await
    }

    pub async fn get_problem(&self, id: i32) -> StressResult<ProblemResponse> {
        self.send_json_with_retry::<(), _>(
            reqwest::Method::GET,
            &format!("/api/v1/problems/{}", id),
            None,
        )
        .await
    }

    pub async fn delete_problem(&self, problem_id: i32) -> StressResult<()> {
        self.send_unit_with_retry::<()>(
            reqwest::Method::DELETE,
            &format!("/api/v1/problems/{}", problem_id),
            None,
        )
        .await
    }

    pub async fn create_test_case(
        &self,
        problem_id: i32,
        req: &CreateTestCaseRequest,
    ) -> StressResult<TestCaseResponse> {
        self.send_json_with_retry(
            reqwest::Method::POST,
            &format!("/api/v1/problems/{}/test-cases", problem_id),
            Some(req),
        )
        .await
    }

    pub async fn create_submission(
        &self,
        problem_id: i32,
        req: &CreateSubmissionRequest,
    ) -> StressResult<SubmissionResponse> {
        self.send_json_with_retry(
            reqwest::Method::POST,
            &format!("/api/v1/problems/{}/submissions", problem_id),
            Some(req),
        )
        .await
    }

    pub async fn create_contest_submission(
        &self,
        contest_id: i32,
        problem_id: i32,
        req: &CreateSubmissionRequest,
    ) -> StressResult<SubmissionResponse> {
        self.send_json_with_retry(
            reqwest::Method::POST,
            &format!(
                "/api/v1/contests/{}/problems/{}/submissions",
                contest_id, problem_id,
            ),
            Some(req),
        )
        .await
    }

    pub async fn create_contest(
        &self,
        req: &CreateContestRequest,
    ) -> StressResult<ContestResponse> {
        self.send_json_with_retry(reqwest::Method::POST, "/api/v1/contests", Some(req))
            .await
    }

    pub async fn add_problem_to_contest(
        &self,
        contest_id: i32,
        req: &AddContestProblemRequest,
    ) -> StressResult<ContestProblemResponse> {
        self.send_json_with_retry(
            reqwest::Method::POST,
            &format!("/api/v1/contests/{}/problems", contest_id),
            Some(req),
        )
        .await
    }

    pub async fn delete_contest(&self, contest_id: i32) -> StressResult<()> {
        self.send_unit_with_retry::<()>(
            reqwest::Method::DELETE,
            &format!("/api/v1/contests/{}", contest_id),
            None,
        )
        .await
    }

    pub async fn bulk_add_participants(
        &self,
        contest_id: i32,
        req: &BulkAddParticipantsRequest,
    ) -> StressResult<BulkAddParticipantsResponse> {
        self.send_json_with_retry(
            reqwest::Method::POST,
            &format!("/api/v1/contests/{}/participants/bulk", contest_id),
            Some(req),
        )
        .await
    }

    pub async fn get_submission(&self, id: i32) -> StressResult<SubmissionResponse> {
        self.send_json_with_retry::<(), _>(
            reqwest::Method::GET,
            &format!("/api/v1/submissions/{}", id),
            None,
        )
        .await
    }

    pub async fn get_dlq_stats(&self) -> StressResult<crate::dto::DlqStatsResponse> {
        self.send_json_with_retry::<(), _>(reqwest::Method::GET, "/api/v1/dlq/stats", None)
            .await
    }

    pub async fn list_contest_problems(
        &self,
        contest_id: i32,
    ) -> StressResult<Vec<ContestProblemResponse>> {
        self.send_json_with_retry::<(), _>(
            reqwest::Method::GET,
            &format!("/api/v1/contests/{}/problems", contest_id),
            None,
        )
        .await
    }

    pub async fn get_contest(&self, contest_id: i32) -> StressResult<ContestResponse> {
        self.send_json_with_retry::<(), _>(
            reqwest::Method::GET,
            &format!("/api/v1/contests/{}", contest_id),
            None,
        )
        .await
    }

    pub async fn list_test_cases(&self, problem_id: i32) -> StressResult<Vec<TestCaseListItem>> {
        self.send_json_with_retry::<(), _>(
            reqwest::Method::GET,
            &format!("/api/v1/problems/{}/test-cases", problem_id),
            None,
        )
        .await
    }

    pub async fn get_test_case(
        &self,
        problem_id: i32,
        tc_id: i32,
    ) -> StressResult<TestCaseResponse> {
        self.send_json_with_retry::<(), _>(
            reqwest::Method::GET,
            &format!("/api/v1/problems/{}/test-cases/{}", problem_id, tc_id),
            None,
        )
        .await
    }

    pub async fn list_attachments(
        &self,
        problem_id: i32,
    ) -> StressResult<crate::dto::AttachmentListResponse> {
        self.send_json_with_retry::<(), _>(
            reqwest::Method::GET,
            &format!("/api/v1/problems/{}/attachments", problem_id),
            None,
        )
        .await
    }

    pub async fn download_attachment(
        &self,
        problem_id: i32,
        ref_id: &str,
    ) -> StressResult<Vec<u8>> {
        self.send_bytes_with_retry::<()>(
            reqwest::Method::GET,
            &format!("/api/v1/problems/{}/attachments/{}", problem_id, ref_id),
            None,
        )
        .await
    }

    pub async fn run_contest_code(
        &self,
        contest_id: i32,
        problem_id: i32,
        req: &RunCodeRequest,
    ) -> StressResult<crate::dto::CodeRunResponse> {
        self.send_json_with_retry(
            reqwest::Method::POST,
            &format!(
                "/api/v1/contests/{}/problems/{}/code-runs",
                contest_id, problem_id,
            ),
            Some(req),
        )
        .await
    }

    pub async fn get_code_run(&self, id: i32) -> StressResult<crate::dto::CodeRunResponse> {
        self.send_json_with_retry::<(), _>(
            reqwest::Method::GET,
            &format!("/api/v1/code-runs/{}", id),
            None,
        )
        .await
    }

    pub async fn get_json_path<R: DeserializeOwned>(&self, path: &str) -> StressResult<R> {
        self.send_json_with_retry::<(), R>(reqwest::Method::GET, path, None)
            .await
    }

    pub async fn get_public_bytes(&self, path: &str) -> StressResult<Vec<u8>> {
        let url = format!("{}{}", self.0.base_url, path);
        let resp = self
            .instrument_request(self.0.http.get(url))
            .send()
            .await
            .map_err(StressError::Network)?;
        decode_bytes(resp).await
    }

    pub async fn upload_plugin_archive(
        &self,
        _plugin_id: &str,
        tar_gz_bytes: &[u8],
    ) -> StressResult<()> {
        let url = format!("{}/api/v1/admin/plugins/upload", self.0.base_url);
        let bytes = tar_gz_bytes.to_vec();

        let send_once = |token: String| {
            let url = url.clone();
            let bytes = bytes.clone();
            let http = self.0.http.clone();
            async move {
                let part = multipart::Part::bytes(bytes)
                    .file_name("plugin.tar.gz")
                    .mime_str("application/gzip")
                    .map_err(StressError::Network)?;
                let form = multipart::Form::new().part("plugin", part);
                self.instrument_request(http.post(&url))
                    .bearer_auth(&token)
                    .multipart(form)
                    .send()
                    .await
                    .map_err(StressError::Network)
            }
        };

        let token = self.token_snapshot().await;
        let resp = send_once(token).await?;

        let resp = if resp.status() == StatusCode::UNAUTHORIZED && self.can_relogin() {
            self.login().await?;
            let new_token = self.token_snapshot().await;
            send_once(new_token).await?
        } else {
            resp
        };

        decode_unit(resp).await
    }

    pub async fn disable_plugin(&self, plugin_id: &str) -> StressResult<()> {
        self.send_unit_with_retry::<()>(
            reqwest::Method::POST,
            &format!("/api/v1/admin/plugins/{}/disable", plugin_id),
            None,
        )
        .await
    }

    fn can_relogin(&self) -> bool {
        matches!(self.0.creds, AuthCreds::UsernamePassword { .. })
    }

    async fn token_snapshot(&self) -> String {
        self.0.token.read().await.clone()
    }

    async fn send_json_with_retry<B, R>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&B>,
    ) -> StressResult<R>
    where
        B: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let resp = self.send_request_with_retry(method, path, body).await?;
        decode_json(resp).await
    }

    async fn send_unit_with_retry<B>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&B>,
    ) -> StressResult<()>
    where
        B: Serialize + ?Sized,
    {
        let resp = self.send_request_with_retry(method, path, body).await?;
        decode_unit(resp).await
    }

    async fn send_bytes_with_retry<B>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&B>,
    ) -> StressResult<Vec<u8>>
    where
        B: Serialize + ?Sized,
    {
        let resp = self.send_request_with_retry(method, path, body).await?;
        decode_bytes(resp).await
    }

    async fn send_request_with_retry<B>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&B>,
    ) -> StressResult<reqwest::Response>
    where
        B: Serialize + ?Sized,
    {
        let url = format!("{}{}", self.0.base_url, path);

        let send_once = async |token: String| -> StressResult<reqwest::Response> {
            let mut req = self
                .instrument_request(self.0.http.request(method.clone(), &url))
                .bearer_auth(token);
            if let Some(b) = body {
                req = req.json(b);
            }
            req.send().await.map_err(StressError::Network)
        };

        let token = self.token_snapshot().await;
        let resp = send_once(token).await?;

        if resp.status() == StatusCode::UNAUTHORIZED && self.can_relogin() {
            self.login().await?;
            let new_token = self.token_snapshot().await;
            send_once(new_token).await
        } else {
            Ok(resp)
        }
    }
}

async fn drain_error(resp: reqwest::Response) -> StressError {
    let status = resp.status().as_u16();
    let request_id = resp
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(ToOwned::to_owned);
    let body = resp.text().await.unwrap_or_default();

    match serde_json::from_str::<ErrorBody>(&body) {
        Ok(parsed) => StressError::Api {
            status,
            code: parsed.code,
            message: append_request_id(parsed.message, request_id),
        },
        Err(_) => StressError::Api {
            status,
            code: "UNKNOWN".to_string(),
            message: append_request_id(body, request_id),
        },
    }
}

fn append_request_id(message: String, request_id: Option<String>) -> String {
    match request_id {
        Some(id) if !id.is_empty() => format!("{message} (x-request-id: {id})"),
        _ => message,
    }
}

async fn decode_json<R: DeserializeOwned>(resp: reqwest::Response) -> StressResult<R> {
    if !resp.status().is_success() {
        return Err(drain_error(resp).await);
    }
    let body = resp.text().await.map_err(StressError::Network)?;
    serde_json::from_str::<R>(&body).map_err(|e| StressError::Decode(e.to_string()))
}

async fn decode_unit(resp: reqwest::Response) -> StressResult<()> {
    if !resp.status().is_success() {
        return Err(drain_error(resp).await);
    }
    Ok(())
}

async fn decode_bytes(resp: reqwest::Response) -> StressResult<Vec<u8>> {
    if !resp.status().is_success() {
        return Err(drain_error(resp).await);
    }
    resp.bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(StressError::Network)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{client_with_login, contest_json, login_body};
    use serde_json::json;
    use wiremock::matchers::{body_partial_json, header, header_exists, method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    #[tokio::test]
    async fn login_uses_post_v1_auth_login_and_stores_token() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/v1/auth/login"))
            .and(body_partial_json(json!({
                "username": "admin",
                "password": "secret"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(login_body("first-token")))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new(
            server.uri(),
            AuthCreds::UsernamePassword {
                username: "admin".into(),
                password: "secret".into(),
            },
        )
        .await
        .expect("login succeeds");

        assert_eq!(client.0.token.read().await.as_str(), "first-token");
    }

    #[tokio::test]
    async fn get_submission_attaches_bearer_token() {
        let server = MockServer::start().await;
        let client = client_with_login(&server, "first-token").await;

        Mock::given(method("GET"))
            .and(path("/api/v1/submissions/42"))
            .and(header("authorization", "Bearer first-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 42,
                "files": [],
                "language": "cpp",
                "status": "Pending",
                "user_id": 1,
                "username": "admin",
                "problem_id": 3,
                "problem_title": "P",
                "contest_id": null,
                "contest_type": "ioi",
                "judge_epoch": 0,
                "created_at": "2026-05-01T00:00:00Z",
                "result": null
            })))
            .expect(1)
            .mount(&server)
            .await;

        let s = client.get_submission(42).await.expect("ok");
        assert_eq!(s.id, 42);
    }

    #[tokio::test]
    async fn requests_include_run_and_request_ids() {
        let server = MockServer::start().await;
        let client = Client::new_with_run_id(
            server.uri(),
            AuthCreds::Token("static-token".into()),
            "profile-run-1".into(),
        )
        .await
        .expect("client builds");

        Mock::given(method("GET"))
            .and(path("/api/v1/submissions/42"))
            .and(header("authorization", "Bearer static-token"))
            .and(header("x-broccoli-stress-run-id", "profile-run-1"))
            .and(header("x-request-id", "profile-run-1-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 42,
                "files": [],
                "language": "cpp",
                "status": "Pending",
                "user_id": 1,
                "username": "admin",
                "problem_id": 3,
                "problem_title": "P",
                "contest_id": null,
                "contest_type": "ioi",
                "judge_epoch": 0,
                "created_at": "2026-05-01T00:00:00Z",
                "result": null
            })))
            .expect(1)
            .mount(&server)
            .await;

        let s = client.get_submission(42).await.expect("ok");
        assert_eq!(s.id, 42);
    }

    #[tokio::test]
    async fn re_logs_in_once_on_401_when_creds_available() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/v1/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(login_body("old-token")))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/v1/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(login_body("new-token")))
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/api/v1/submissions/7"))
            .and(header("authorization", "Bearer old-token"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_json(json!({ "code": "TOKEN_INVALID", "message": "expired" })),
            )
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/api/v1/submissions/7"))
            .and(header("authorization", "Bearer new-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 7,
                "files": [],
                "language": "cpp",
                "status": "Pending",
                "user_id": 1,
                "username": "admin",
                "problem_id": 3,
                "problem_title": "P",
                "contest_id": null,
                "contest_type": "ioi",
                "judge_epoch": 0,
                "created_at": "2026-05-01T00:00:00Z",
                "result": null
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new(
            server.uri(),
            AuthCreds::UsernamePassword {
                username: "admin".into(),
                password: "secret".into(),
            },
        )
        .await
        .unwrap();

        assert_eq!(client.0.token.read().await.as_str(), "old-token");

        let s = client.get_submission(7).await.expect("retried ok");
        assert_eq!(s.id, 7);
        assert_eq!(client.0.token.read().await.as_str(), "new-token");
    }

    #[tokio::test]
    async fn token_only_creds_propagate_401_without_relogin() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/v1/submissions/9"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_json(json!({ "code": "TOKEN_INVALID", "message": "no" })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new(server.uri(), AuthCreds::Token("static-token".into()))
            .await
            .unwrap();

        let err = client.get_submission(9).await.expect_err("must fail");
        match err {
            StressError::Api {
                status,
                code,
                message: _,
            } => {
                assert_eq!(status, 401);
                assert_eq!(code, "TOKEN_INVALID");
            }
            other => panic!("wrong error variant: {:?}", other),
        }
    }

    #[tokio::test]
    async fn upload_plugin_sends_multipart_with_plugin_field() {
        let server = MockServer::start().await;
        let client = client_with_login(&server, "first-token").await;

        let archive = b"fake-tar-gz-bytes-payload-12345";
        let archive_vec = archive.to_vec();

        Mock::given(method("POST"))
            .and(path("/api/v1/admin/plugins/upload"))
            .and(header_exists("content-type"))
            .and(header("authorization", "Bearer first-token"))
            .and(MultipartFieldMatcher {
                field: "plugin",
                expected_bytes: archive_vec.clone(),
                expected_mime: "application/gzip",
            })
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "status": "loaded" })))
            .expect(1)
            .mount(&server)
            .await;

        client
            .upload_plugin_archive("ioi", archive)
            .await
            .expect("upload ok");
    }

    #[tokio::test]
    async fn api_errors_decode_into_stress_error_api() {
        let server = MockServer::start().await;
        let client = client_with_login(&server, "first-token").await;

        Mock::given(method("GET"))
            .and(path("/api/v1/problems/404"))
            .respond_with(
                ResponseTemplate::new(404)
                    .set_body_json(json!({ "code": "NOT_FOUND", "message": "no such problem" })),
            )
            .mount(&server)
            .await;

        let err = client.get_problem(404).await.expect_err("must fail");
        match err {
            StressError::Api {
                status,
                code,
                message,
            } => {
                assert_eq!(status, 404);
                assert_eq!(code, "NOT_FOUND");
                assert_eq!(message, "no such problem");
            }
            other => panic!("expected Api error, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn non_json_error_body_falls_back_to_unknown_code() {
        let server = MockServer::start().await;
        let client = client_with_login(&server, "first-token").await;

        Mock::given(method("GET"))
            .and(path("/api/v1/problems/500"))
            .respond_with(ResponseTemplate::new(500).set_body_string("plain text doom"))
            .mount(&server)
            .await;

        let err = client.get_problem(500).await.expect_err("must fail");
        match err {
            StressError::Api {
                status,
                code,
                message,
            } => {
                assert_eq!(status, 500);
                assert_eq!(code, "UNKNOWN");
                assert!(message.contains("plain text doom"));
            }
            other => panic!("expected Api fallback, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn delete_problem_treats_204_as_success() {
        let server = MockServer::start().await;
        let client = client_with_login(&server, "first-token").await;

        Mock::given(method("DELETE"))
            .and(path("/api/v1/problems/12"))
            .and(header("authorization", "Bearer first-token"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        client.delete_problem(12).await.expect("delete ok");
    }

    #[tokio::test]
    async fn create_contest_posts_v1_contests_and_returns_response() {
        let server = MockServer::start().await;
        let client = client_with_login(&server, "tok").await;

        Mock::given(method("POST"))
            .and(path("/api/v1/contests"))
            .and(header("authorization", "Bearer tok"))
            .and(body_partial_json(json!({
                "title": "stress-test scratch",
                "is_public": false,
                "contest_type": "icpc"
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(contest_json(7, Some("icpc"))))
            .expect(1)
            .mount(&server)
            .await;

        let req = CreateContestRequest {
            title: "stress-test scratch".into(),
            description: "auto".into(),
            activate_time: None,
            start_time: chrono::Utc::now(),
            end_time: chrono::Utc::now() + chrono::Duration::hours(24),
            deactivate_time: None,
            is_public: false,
            submissions_visible: None,
            show_compile_output: None,
            show_participants_list: None,
            contest_type: "icpc".into(),
        };
        let resp = client.create_contest(&req).await.expect("ok");
        assert_eq!(resp.id, 7);
    }

    #[tokio::test]
    async fn add_problem_to_contest_posts_v1_contests_id_problems() {
        let server = MockServer::start().await;
        let client = client_with_login(&server, "tok").await;

        Mock::given(method("POST"))
            .and(path("/api/v1/contests/7/problems"))
            .and(header("authorization", "Bearer tok"))
            .and(body_partial_json(json!({ "problem_id": 11, "label": "A" })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "contest_id": 7,
                "problem_id": 11,
                "label": "A",
                "position": 0,
                "problem_title": "stress-test:ab-cpp-ac"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let req = AddContestProblemRequest {
            problem_id: 11,
            label: "A".into(),
            position: None,
        };
        let resp = client.add_problem_to_contest(7, &req).await.expect("ok");
        assert_eq!(resp.problem_id, 11);
        assert_eq!(resp.label, "A");
    }

    #[tokio::test]
    async fn delete_contest_treats_204_as_success() {
        let server = MockServer::start().await;
        let client = client_with_login(&server, "tok").await;

        Mock::given(method("DELETE"))
            .and(path("/api/v1/contests/7"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        client.delete_contest(7).await.expect("ok");
    }

    #[tokio::test]
    async fn create_contest_submission_targets_contest_problem_path() {
        let server = MockServer::start().await;
        let client = client_with_login(&server, "tok").await;

        Mock::given(method("POST"))
            .and(path("/api/v1/contests/7/problems/11/submissions"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "id": 99,
                "files": [],
                "language": "cpp",
                "status": "Pending",
                "user_id": 1,
                "username": "admin",
                "problem_id": 11,
                "problem_title": "stress-test:ab-cpp-ac",
                "contest_id": 7,
                "contest_type": "icpc",
                "judge_epoch": 0,
                "created_at": "2026-05-01T00:00:00Z",
                "result": null
            })))
            .expect(1)
            .mount(&server)
            .await;

        let req = CreateSubmissionRequest {
            files: vec![],
            language: "cpp".into(),
            contest_type: None,
        };
        let s = client
            .create_contest_submission(7, 11, &req)
            .await
            .expect("ok");
        assert_eq!(s.id, 99);
        assert_eq!(s.contest_id, Some(7));
    }

    #[tokio::test]
    async fn list_registries_uses_public_endpoint_without_auth() {
        let server = MockServer::start().await;
        let client = client_with_login(&server, "ignored-token").await;

        Mock::given(method("GET"))
            .and(path("/api/v1/plugins/registries"))
            .and(NoBearerHeader)
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "problem_types": ["batch", "interactive"],
                "checker_formats": ["exact", "tokens"],
                "contest_types": ["icpc", "ioi"],
                "languages": [],
                "evaluators": [],
                "contest_type_handlers": [],
                "hooks": []
            })))
            .expect(1)
            .mount(&server)
            .await;

        let r = client.list_registries().await.expect("registries ok");
        assert_eq!(r.problem_types, vec!["batch", "interactive"]);
        assert_eq!(r.contest_types, vec!["icpc", "ioi"]);
        assert_eq!(r.checker_formats, vec!["exact", "tokens"]);
    }

    struct NoBearerHeader;

    impl wiremock::Match for NoBearerHeader {
        fn matches(&self, req: &Request) -> bool {
            !req.headers.iter().any(|(name, value)| {
                name.as_str().eq_ignore_ascii_case("authorization")
                    && value.as_bytes().starts_with(b"Bearer ")
            })
        }
    }

    struct MultipartFieldMatcher {
        field: &'static str,
        expected_bytes: Vec<u8>,
        expected_mime: &'static str,
    }

    impl wiremock::Match for MultipartFieldMatcher {
        fn matches(&self, req: &Request) -> bool {
            let ct = match req.headers.get("content-type") {
                Some(v) => v.to_str().unwrap_or(""),
                None => return false,
            };
            let boundary = match ct.split("boundary=").nth(1) {
                Some(b) => b.trim_matches('"'),
                None => return false,
            };

            let body = &req.body;
            let delimiter = format!("--{}", boundary);
            let parts: Vec<&[u8]> = split_bytes(body, delimiter.as_bytes());

            for part in parts {
                if part.is_empty() || part.starts_with(b"--") {
                    continue;
                }
                let part = part.strip_prefix(b"\r\n").unwrap_or(part);
                let split_at = match find_subsequence(part, b"\r\n\r\n") {
                    Some(idx) => idx,
                    None => continue,
                };
                let (headers_raw, rest) = part.split_at(split_at);
                let body_part = &rest[4..];
                let body_part = body_part.strip_suffix(b"\r\n").unwrap_or(body_part);

                let headers = std::str::from_utf8(headers_raw).unwrap_or("");
                let name_token = format!("name=\"{}\"", self.field);
                if !headers.contains(&name_token) {
                    continue;
                }

                let mime_ok = headers.lines().any(|line| {
                    line.to_ascii_lowercase().starts_with("content-type:")
                        && line.to_ascii_lowercase().contains(self.expected_mime)
                });

                let bytes_ok = body_part == self.expected_bytes.as_slice();
                return mime_ok && bytes_ok;
            }
            false
        }
    }

    fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    fn split_bytes<'a>(haystack: &'a [u8], delimiter: &[u8]) -> Vec<&'a [u8]> {
        let mut out = Vec::new();
        let mut start = 0;
        while start < haystack.len() {
            match find_subsequence(&haystack[start..], delimiter) {
                Some(idx) => {
                    out.push(&haystack[start..start + idx]);
                    start += idx + delimiter.len();
                }
                None => {
                    out.push(&haystack[start..]);
                    break;
                }
            }
        }
        out
    }
}
