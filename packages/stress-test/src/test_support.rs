//! Mock-server fixtures shared by the unit tests of the phase runners.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};

use serde_json::{Value, json};
use tokio::sync::mpsc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use crate::client::{AuthCreds, Client};
use crate::dto::{SubmissionStatus, Verdict};
use crate::events::Event;

pub fn login_body(token: &str) -> Value {
    json!({
        "token": token,
        "id": 1,
        "username": "admin",
        "display_name": "admin",
        "roles": ["admin"],
        "permissions": []
    })
}

/// A client logged in as `admin` against `server`, which answers the login.
pub async fn client_with_login(server: &MockServer, token: &str) -> Client {
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(login_body(token)))
        .mount(server)
        .await;
    Client::new(
        server.uri(),
        AuthCreds::UsernamePassword {
            username: "admin".into(),
            password: "secret".into(),
        },
    )
    .await
    .expect("client builds")
}

/// A `GET /submissions/{id}` body; `verdict` adds a judge result.
pub fn submission_json(id: i32, status: SubmissionStatus, verdict: Option<Verdict>) -> Value {
    let result = verdict.map(|v| {
        json!({
            "verdict": v,
            "score": 100.0,
            "time_used": 10,
            "memory_used": 1024,
            "compile_output": null,
            "error_message": null,
            "judged_at": "2026-05-01T00:00:00Z",
            "test_case_results": []
        })
    });
    json!({
        "id": id,
        "files": [],
        "language": "cpp",
        "status": status,
        "user_id": 1,
        "username": "admin",
        "problem_id": 100,
        "problem_title": "stress-test",
        "contest_id": null,
        "contest_type": "icpc",
        "judge_epoch": 0,
        "created_at": "2026-05-01T00:00:00Z",
        "result": result,
    })
}

/// What a mock submission reports once it is polled.
#[derive(Clone)]
pub struct MockFinalSubmission {
    pub status: SubmissionStatus,
    pub verdict: Option<Verdict>,
    pub fails_on_get: bool,
}

/// Hands out distinct submission ids and records what was submitted and polled.
pub struct MockSubmissionStore {
    next_id: AtomicI32,
    final_by_id: Mutex<HashMap<i32, MockFinalSubmission>>,
    submitted_ids: Mutex<Vec<i32>>,
    polled_ids: Mutex<Vec<i32>>,
}

impl MockSubmissionStore {
    pub fn new(first_id: i32) -> Self {
        Self {
            next_id: AtomicI32::new(first_id),
            final_by_id: Mutex::new(HashMap::new()),
            submitted_ids: Mutex::new(Vec::new()),
            polled_ids: Mutex::new(Vec::new()),
        }
    }

    /// Register a new submission that will poll as `final_submission`.
    pub fn submit(&self, final_submission: MockFinalSubmission) -> i32 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.final_by_id
            .lock()
            .unwrap()
            .insert(id, final_submission);
        self.submitted_ids.lock().unwrap().push(id);
        id
    }

    /// Record a poll of `request`'s submission and return its final state.
    pub fn poll(&self, request: &Request) -> Option<(i32, MockFinalSubmission)> {
        let id = submission_id_from_request(request)?;
        self.polled_ids.lock().unwrap().push(id);
        let final_submission = self.final_by_id.lock().unwrap().get(&id).cloned()?;
        Some((id, final_submission))
    }

    pub fn submitted_ids(&self) -> Vec<i32> {
        self.submitted_ids.lock().unwrap().clone()
    }

    pub fn polled_ids(&self) -> Vec<i32> {
        self.polled_ids.lock().unwrap().clone()
    }
}

fn submission_id_from_request(request: &Request) -> Option<i32> {
    request.url.path().rsplit('/').next()?.parse().ok()
}

pub fn assert_distinct_submission_ids(ids: &[i32]) {
    let unique: HashSet<_> = ids.iter().copied().collect();
    assert_eq!(unique.len(), ids.len(), "submission ids must be distinct");
}

pub fn drain(rx: &mut mpsc::UnboundedReceiver<Event>) -> Vec<Event> {
    let mut out = Vec::new();
    while let Ok(e) = rx.try_recv() {
        out.push(e);
    }
    out
}

/// A `GET /contests/{id}` body.
pub fn contest_json(id: i32, contest_type: Option<&str>) -> Value {
    json!({
        "id": id,
        "title": "Test Contest",
        "description": "",
        "activate_time": null,
        "start_time": "2026-05-01T00:00:00Z",
        "end_time": "2026-05-01T03:00:00Z",
        "deactivate_time": null,
        "is_public": true,
        "submissions_visible": true,
        "show_compile_output": true,
        "show_participants_list": true,
        "contest_type": contest_type,
        "created_at": "2026-05-01T00:00:00Z",
        "updated_at": "2026-05-01T00:00:00Z",
    })
}

/// A `GET /problems/{id}` body.
pub fn problem_json(
    id: i32,
    problem_type: &str,
    checker_format: &str,
    contest_type: &str,
) -> Value {
    json!({
        "id": id,
        "title": "stress-test:x",
        "content": "...",
        "time_limit": 1000,
        "memory_limit": 65536,
        "problem_type": problem_type,
        "checker_format": checker_format,
        "default_contest_type": contest_type,
        "show_test_details": false,
        "is_public": false,
        "submission_format": null,
        "samples": [],
        "created_at": "2026-05-01T00:00:00Z",
        "updated_at": "2026-05-01T00:00:00Z"
    })
}

/// A `GET`/`POST /problems/{id}/test-cases` body.
pub fn test_case_json(id: i32, problem_id: i32, expected_output: &str) -> Value {
    json!({
        "id": id,
        "input": "1\n",
        "expected_output": expected_output,
        "input_size": 2,
        "output_size": expected_output.len(),
        "input_preview": "1\n",
        "output_preview": expected_output,
        "score": 100,
        "description": null,
        "label": "01",
        "is_sample": true,
        "position": 0,
        "problem_id": problem_id,
        "created_at": "2026-05-01T00:00:00Z"
    })
}
