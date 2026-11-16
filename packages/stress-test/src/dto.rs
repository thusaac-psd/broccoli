//! The REST API types the harness sends and reads, from the server's own
//! definitions in [`broccoli_api_types`].

pub use broccoli_api_types::attachment::{AttachmentListResponse, AttachmentResponse};
pub use broccoli_api_types::auth::{LoginRequest, LoginResponse};
pub use broccoli_api_types::code_run::{CodeRunResponse, CustomTestCaseInput, RunCodeRequest};
pub use broccoli_api_types::contest::{
    AddContestProblemRequest, BulkAddParticipantsRequest, BulkAddParticipantsResponse,
    ContestProblemResponse, ContestResponse, CreateContestRequest, CreateUserEntry,
};
pub use broccoli_api_types::dlq::DlqStatsResponse;
pub use broccoli_api_types::error::ErrorBody;
pub use broccoli_api_types::plugin::RegistriesResponse;
pub use broccoli_api_types::problem::{
    CreateProblemRequest, CreateTestCaseRequest, ProblemResponse, TestCaseListItem,
    TestCaseResponse,
};
pub use broccoli_api_types::submission::{
    CreateSubmissionRequest, JudgeResultResponse, SubmissionFileDto, SubmissionResponse,
    TestCaseResultResponse,
};
pub use broccoli_api_types::{SubmissionStatus, Verdict};
