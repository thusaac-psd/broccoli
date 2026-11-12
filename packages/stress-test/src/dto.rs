use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Debug)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct LoginResponse {
    pub token: String,
    pub id: i32,
    pub username: String,
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CreateProblemRequest {
    pub title: String,
    pub content: String,
    pub time_limit: i32,
    pub memory_limit: i32,
    #[serde(default)]
    pub problem_type: String,
    pub checker_format: String,
    #[serde(default)]
    pub default_contest_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_test_details: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submission_format: Option<HashMap<String, Vec<String>>>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ProblemResponse {
    pub id: i32,
    pub title: String,
    pub time_limit: i32,
    pub memory_limit: i32,
    pub problem_type: String,
    pub checker_format: String,
    pub default_contest_type: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CreateTestCaseRequest {
    pub input: String,
    pub expected_output: String,
    pub score: i32,
    pub is_sample: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct TestCaseResponse {
    pub id: i32,
    #[serde(default)]
    pub input: String,
    #[serde(default)]
    pub expected_output: String,
    pub score: i32,
    pub label: String,
    pub is_sample: bool,
    pub position: i32,
    pub problem_id: i32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TestCaseListItem {
    pub id: i32,
    pub score: i32,
    pub label: String,
    pub is_sample: bool,
    pub position: i32,
    pub problem_id: i32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AttachmentListResponse {
    pub attachments: Vec<AttachmentResponse>,
    pub total: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AttachmentResponse {
    pub id: String,
    pub path: String,
    pub filename: String,
    pub content_type: Option<String>,
    pub size: i64,
    pub content_hash: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RegistriesResponse {
    pub problem_types: Vec<String>,
    pub checker_formats: Vec<String>,
    pub contest_types: Vec<String>,
    pub languages: Vec<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ContestProblemResponse {
    pub contest_id: i32,
    pub problem_id: i32,
    pub label: String,
    pub position: i32,
    pub problem_title: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ContestResponse {
    pub id: i32,
    pub title: String,
    pub contest_type: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct CreateContestRequest {
    pub title: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activate_time: Option<DateTime<Utc>>,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deactivate_time: Option<DateTime<Utc>>,
    pub is_public: bool,
    pub contest_type: String,
}

#[derive(Serialize, Debug)]
pub struct AddContestProblemRequest {
    pub problem_id: i32,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<i32>,
}

#[derive(Serialize, Debug)]
pub struct BulkAddParticipantsRequest {
    #[serde(default)]
    pub usernames: Vec<String>,
    #[serde(default)]
    pub create_users: Vec<CreateUserEntry>,
}

#[derive(Serialize, Debug, Clone)]
pub struct CreateUserEntry {
    pub username: String,
    pub password: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct BulkAddParticipantsResponse {
    pub added: Vec<BulkParticipantAdded>,
    pub created: Vec<BulkParticipantCreated>,
    pub already_enrolled: Vec<BulkParticipantAdded>,
    pub not_found: Vec<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct BulkParticipantAdded {
    pub user_id: i32,
    pub username: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct BulkParticipantCreated {
    pub user_id: i32,
    pub username: String,
    pub password: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SubmissionFileDto {
    pub filename: String,
    pub content: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CreateSubmissionRequest {
    pub files: Vec<SubmissionFileDto>,
    pub language: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contest_type: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CustomTestCaseInput {
    pub input: String,
    pub expected_output: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RunCodeRequest {
    pub files: Vec<SubmissionFileDto>,
    pub language: String,
    pub custom_test_cases: Vec<CustomTestCaseInput>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CodeRunResponse {
    pub id: i32,
    pub files: Vec<SubmissionFileDto>,
    pub language: String,
    pub status: SubmissionStatus,
    pub user_id: i32,
    pub username: String,
    pub problem_id: i32,
    pub problem_title: String,
    pub contest_id: Option<i32>,
    pub contest_type: String,
    pub custom_test_cases: Vec<CustomTestCaseInput>,
    pub created_at: DateTime<Utc>,
    pub result: Option<JudgeResultResponse>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SubmissionResponse {
    pub id: i32,
    pub language: String,
    pub status: SubmissionStatus,
    pub user_id: i32,
    pub username: String,
    pub problem_id: i32,
    pub problem_title: String,
    pub contest_id: Option<i32>,
    pub contest_type: String,
    pub judge_epoch: i32,
    pub created_at: DateTime<Utc>,
    pub result: Option<JudgeResultResponse>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct JudgeResultResponse {
    pub verdict: Option<Verdict>,
    pub score: Option<f64>,
    pub time_used: Option<i32>,
    pub memory_used: Option<i32>,
    pub compile_output: Option<String>,
    pub error_message: Option<String>,
    pub judged_at: Option<DateTime<Utc>>,
    pub test_case_results: Vec<TestCaseResultResponse>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct TestCaseResultResponse {
    pub id: i32,
    pub verdict: Verdict,
    pub score: f64,
    pub time_used: Option<i32>,
    pub memory_used: Option<i32>,
    pub test_case_id: Option<i32>,
}

// Verdict used to be a hand-rolled byte-for-byte copy of the canonical enum
// in `common` (including the custom wire-string serde). Re-export the real
// one instead so it can never drift behind the server contract.
pub use common::Verdict;

/// Submission lifecycle status as returned by the server.
///
/// This mirrors [`common::SubmissionStatus`] instead of re-exporting it
/// because harness hot paths pass statuses around by value and rely on
/// `Copy`, which the canonical enum does not derive. The previous local copy
/// silently drifted behind the wire contract: it was missing the `Queued`
/// durable-accept status the server persists and returns on
/// `POST /submissions`, so every create/poll that observed `Queued` failed
/// serde with an unknown-variant error. Drift is now guarded three ways:
///
/// - deserialization delegates to the canonical [`std::str::FromStr`] impl
///   in `common`, so the accepted wire strings are exactly the server's;
/// - [`From<common::SubmissionStatus>`] matches exhaustively, so adding a
///   status server-side fails this crate's build instead of breaking the
///   harness at runtime;
/// - wire strings this build does not know map to [`Self::Unknown`] instead
///   of aborting the poll loop with a serde error.
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubmissionStatus {
    /// Durable accept state: persisted by the API but not yet claimed for
    /// dispatch. Returned by `POST /submissions` before `Pending`.
    Queued,
    Pending,
    Compiling,
    Running,
    Judged,
    CompilationError,
    SystemError,
    /// Catch-all for wire statuses this build does not know about. Treated
    /// as non-terminal so pollers keep waiting (and eventually time out
    /// visibly) rather than crashing on an unrecognised status.
    Unknown,
}

impl SubmissionStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Judged | Self::CompilationError | Self::SystemError
        )
    }
}

impl From<common::SubmissionStatus> for SubmissionStatus {
    fn from(status: common::SubmissionStatus) -> Self {
        // Exhaustive on purpose: a new variant in `common` must break this
        // build so the mirror cannot silently fall behind again.
        match status {
            common::SubmissionStatus::Queued => Self::Queued,
            common::SubmissionStatus::Pending => Self::Pending,
            common::SubmissionStatus::Compiling => Self::Compiling,
            common::SubmissionStatus::Running => Self::Running,
            common::SubmissionStatus::Judged => Self::Judged,
            common::SubmissionStatus::CompilationError => Self::CompilationError,
            common::SubmissionStatus::SystemError => Self::SystemError,
        }
    }
}

impl<'de> Deserialize<'de> for SubmissionStatus {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Ok(raw
            .parse::<common::SubmissionStatus>()
            .map(Self::from)
            .unwrap_or(Self::Unknown))
    }
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct DlqStats {
    pub total_unresolved: u64,
    pub total_resolved: u64,
    #[serde(default)]
    pub unresolved_by_error_code: HashMap<String, u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submission_status_round_trip() {
        let cases = [
            (SubmissionStatus::Queued, "\"Queued\""),
            (SubmissionStatus::Pending, "\"Pending\""),
            (SubmissionStatus::Compiling, "\"Compiling\""),
            (SubmissionStatus::Running, "\"Running\""),
            (SubmissionStatus::Judged, "\"Judged\""),
            (SubmissionStatus::CompilationError, "\"CompilationError\""),
            (SubmissionStatus::SystemError, "\"SystemError\""),
        ];
        for (variant, expected) in cases {
            let serialised = serde_json::to_string(&variant).unwrap();
            assert_eq!(serialised, expected, "serialise {variant:?}");
            let parsed: SubmissionStatus = serde_json::from_str(expected).unwrap();
            assert_eq!(parsed, variant);
        }
    }

    #[test]
    fn submission_status_is_terminal_matches_server_definition() {
        assert!(!SubmissionStatus::Queued.is_terminal());
        assert!(!SubmissionStatus::Pending.is_terminal());
        assert!(!SubmissionStatus::Compiling.is_terminal());
        assert!(!SubmissionStatus::Running.is_terminal());
        assert!(SubmissionStatus::Judged.is_terminal());
        assert!(SubmissionStatus::CompilationError.is_terminal());
        assert!(SubmissionStatus::SystemError.is_terminal());
        assert!(!SubmissionStatus::Unknown.is_terminal());
    }

    #[test]
    fn submission_status_mirrors_common_wire_contract() {
        // Drift guard: every status the server can emit must round-trip
        // through the local mirror with identical wire strings and identical
        // terminality.
        for canonical in common::SubmissionStatus::ALL {
            let wire = serde_json::to_string(canonical).unwrap();
            let local: SubmissionStatus = serde_json::from_str(&wire).unwrap();
            assert_eq!(local, SubmissionStatus::from(canonical.clone()));
            assert_ne!(
                local,
                SubmissionStatus::Unknown,
                "known status {wire} must not fall back"
            );
            assert_eq!(serde_json::to_string(&local).unwrap(), wire);
            assert_eq!(
                local.is_terminal(),
                canonical.is_terminal(),
                "terminality of {wire}"
            );
        }
    }

    #[test]
    fn unknown_submission_status_falls_back_instead_of_failing() {
        let parsed: SubmissionStatus = serde_json::from_str("\"BrandNewStatus\"").unwrap();
        assert_eq!(parsed, SubmissionStatus::Unknown);
        assert!(!parsed.is_terminal());
    }

    #[test]
    fn parses_queued_submission_from_post_response() {
        // The server persists Queued and returns 201 immediately; the
        // previous local enum copy failed serde on exactly this response.
        let raw = r#"{
            "id": 2,
            "language": "cpp",
            "status": "Queued",
            "user_id": 7,
            "username": "stress-bot",
            "problem_id": 3,
            "problem_title": "Two Sum",
            "contest_id": null,
            "contest_type": "ioi",
            "judge_epoch": 0,
            "created_at": "2026-05-01T12:00:00Z",
            "result": null
        }"#;
        let parsed: SubmissionResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.status, SubmissionStatus::Queued);
        assert!(!parsed.status.is_terminal());
    }

    #[test]
    fn verdict_known_variants_round_trip() {
        let cases = [
            (Verdict::Accepted, "\"Accepted\""),
            (Verdict::WrongAnswer, "\"WrongAnswer\""),
            (Verdict::TimeLimitExceeded, "\"TimeLimitExceeded\""),
            (Verdict::MemoryLimitExceeded, "\"MemoryLimitExceeded\""),
            (Verdict::RuntimeError, "\"RuntimeError\""),
            (Verdict::SystemError, "\"SystemError\""),
            (Verdict::Skipped, "\"Skipped\""),
            (Verdict::Cancelled, "\"Cancelled\""),
        ];
        for (variant, expected) in cases {
            let serialised = serde_json::to_string(&variant).unwrap();
            assert_eq!(serialised, expected, "serialise {variant:?}");
            let parsed: Verdict = serde_json::from_str(expected).unwrap();
            assert_eq!(parsed, variant);
        }
    }

    #[test]
    fn verdict_unknown_string_becomes_other() {
        let v: Verdict = serde_json::from_str("\"PluginCustomThing\"").unwrap();
        assert_eq!(v, Verdict::Other("PluginCustomThing".into()));
    }

    #[test]
    fn verdict_other_serializes_as_inner_string() {
        let v = Verdict::Other("X".into());
        assert_eq!(serde_json::to_string(&v).unwrap(), "\"X\"");
    }

    #[test]
    fn verdict_other_round_trips_via_inner_string() {
        let original = Verdict::Other("CustomFoo".into());
        let raw = serde_json::to_string(&original).unwrap();
        let back: Verdict = serde_json::from_str(&raw).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn parses_realistic_submission_response() {
        let raw = include_str!("../tests/fixtures/submission_response.json");
        let parsed: SubmissionResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.id, 42);
        assert_eq!(parsed.language, "cpp");
        assert_eq!(parsed.status, SubmissionStatus::Judged);
        assert!(parsed.status.is_terminal());
        assert_eq!(parsed.problem_id, 3);
        assert_eq!(parsed.contest_id, None);

        let result = parsed.result.expect("judged submission has result");
        assert_eq!(result.verdict, Some(Verdict::Accepted));
        assert_eq!(result.score, Some(100.0));
        assert_eq!(result.time_used, Some(50));
        assert_eq!(result.memory_used, Some(1024));
        assert!(result.judged_at.is_some());
        assert_eq!(result.test_case_results.len(), 2);

        let tc = &result.test_case_results[0];
        assert_eq!(tc.verdict, Verdict::Accepted);
        assert_eq!(tc.score, 50.0);
        assert_eq!(tc.test_case_id, Some(1));
    }

    #[test]
    fn parses_pending_submission_without_result() {
        let raw = r#"{
            "id": 1,
            "language": "cpp",
            "status": "Pending",
            "user_id": 7,
            "username": "stress-bot",
            "problem_id": 3,
            "problem_title": "Two Sum",
            "contest_id": null,
            "contest_type": "ioi",
            "judge_epoch": 0,
            "created_at": "2026-05-01T12:00:00Z",
            "result": null
        }"#;
        let parsed: SubmissionResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.status, SubmissionStatus::Pending);
        assert!(!parsed.status.is_terminal());
        assert!(parsed.result.is_none());
    }

    #[test]
    fn parses_problem_response() {
        let raw = r#"{
            "id": 5,
            "title": "Two Sum",
            "content": "Given an array...",
            "time_limit": 1000,
            "memory_limit": 262144,
            "problem_type": "batch",
            "checker_format": "exact",
            "default_contest_type": "ioi",
            "show_test_details": false,
            "submission_format": null,
            "samples": [],
            "created_at": "2026-05-01T08:00:00Z",
            "updated_at": "2026-05-01T08:00:00Z"
        }"#;
        let parsed: ProblemResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.id, 5);
        assert_eq!(parsed.title, "Two Sum");
        assert_eq!(parsed.problem_type, "batch");
    }

    #[test]
    fn parses_test_case_response() {
        let raw = r#"{
            "id": 11,
            "input": "1\n",
            "expected_output": "1\n",
            "score": 10,
            "description": null,
            "label": "01",
            "is_sample": false,
            "position": 0,
            "problem_id": 5,
            "created_at": "2026-05-01T08:00:00Z"
        }"#;
        let parsed: TestCaseResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.id, 11);
        assert_eq!(parsed.score, 10);
        assert_eq!(parsed.label, "01");
        assert!(!parsed.is_sample);
    }

    #[test]
    fn parses_login_response() {
        let raw = r#"{
            "token": "eyJhbGciOi...",
            "id": 1,
            "username": "admin",
            "roles": ["admin"],
            "permissions": ["problem:create", "problem:edit"]
        }"#;
        let parsed: LoginResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.token, "eyJhbGciOi...");
        assert_eq!(parsed.id, 1);
        assert_eq!(parsed.username, "admin");
        assert_eq!(parsed.roles, vec!["admin".to_string()]);
    }

    #[test]
    fn parses_error_body_with_and_without_details() {
        let basic = r#"{"code":"VALIDATION_ERROR","message":"Bad input"}"#;
        let parsed: ErrorBody = serde_json::from_str(basic).unwrap();
        assert_eq!(parsed.code, "VALIDATION_ERROR");
        assert_eq!(parsed.message, "Bad input");
        assert!(parsed.details.is_none());

        let with_details =
            r#"{"code":"PLUGIN_REJECTED","message":"Cooldown","details":{"retry_in":5}}"#;
        let parsed: ErrorBody = serde_json::from_str(with_details).unwrap();
        assert_eq!(parsed.code, "PLUGIN_REJECTED");
        assert!(parsed.details.is_some());
    }

    #[test]
    fn submission_request_round_trips() {
        let req = CreateSubmissionRequest {
            files: vec![SubmissionFileDto {
                filename: "solution.cpp".into(),
                content: "int main(){}".into(),
            }],
            language: "cpp".into(),
            contest_type: Some("ioi".into()),
        };
        let raw = serde_json::to_string(&req).unwrap();
        let back: CreateSubmissionRequest = serde_json::from_str(&raw).unwrap();
        assert_eq!(back.language, "cpp");
        assert_eq!(back.files.len(), 1);
        assert_eq!(back.contest_type.as_deref(), Some("ioi"));
    }
}
