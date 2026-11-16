//! Contest, contest problem and participant types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub use broccoli_types::types::ContestPhase;

use crate::shared::Pagination;

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CreateContestRequest {
    #[schema(example = "Weekly Contest #42")]
    pub title: String,
    #[schema(example = "Welcome to this week's programming contest.")]
    pub description: String,
    #[schema(example = "2025-09-30T12:00:00Z")]
    pub activate_time: Option<Option<DateTime<Utc>>>,
    #[schema(example = "2025-10-01T14:00:00Z")]
    pub start_time: DateTime<Utc>,
    #[schema(example = "2025-10-01T17:00:00Z")]
    pub end_time: DateTime<Utc>,
    #[schema(example = "2025-10-02T12:00:00Z")]
    pub deactivate_time: Option<Option<DateTime<Utc>>>,
    #[schema(example = true)]
    pub is_public: bool,
    #[schema(example = false)]
    pub submissions_visible: Option<bool>,
    #[schema(example = true)]
    pub show_compile_output: Option<bool>,
    #[schema(example = true)]
    pub show_participants_list: Option<bool>,
    /// The contest type (format) that judges this contest's submissions.
    /// Required: the server never picks one, because any "first registered"
    /// choice depends on plugin names.
    #[schema(example = "ioi")]
    pub contest_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AddContestProblemRequest {
    #[schema(example = 1)]
    pub problem_id: i32,
    #[schema(example = "A")]
    pub label: String,
    #[schema(example = 0)]
    pub position: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ContestResponse {
    #[schema(example = 1)]
    pub id: i32,
    #[schema(example = "Weekly Contest #42")]
    pub title: String,
    #[schema(example = "Welcome to this week's contest.")]
    pub description: String,
    #[schema(example = "2025-09-30T12:00:00Z")]
    pub activate_time: Option<DateTime<Utc>>,
    #[schema(example = "2025-10-01T14:00:00Z")]
    pub start_time: DateTime<Utc>,
    #[schema(example = "2025-10-01T17:00:00Z")]
    pub end_time: DateTime<Utc>,
    #[schema(example = "2025-10-02T12:00:00Z")]
    pub deactivate_time: Option<DateTime<Utc>>,
    #[schema(example = true)]
    pub is_public: bool,
    #[schema(example = false)]
    pub submissions_visible: bool,
    #[schema(example = true)]
    pub show_compile_output: bool,
    #[schema(example = true)]
    pub show_participants_list: bool,
    #[schema(example = "ioi")]
    pub contest_type: Option<String>,
    #[schema(example = "2025-09-25T10:00:00Z")]
    pub created_at: DateTime<Utc>,
    #[schema(example = "2025-09-25T10:30:00Z")]
    pub updated_at: DateTime<Utc>,
    /// Where the contest is now, on the server's clock. Clients render this
    /// rather than comparing the timeline with their own clock.
    pub phase: ContestPhase,
}

/// One contest in a contest list.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ContestListItem {
    #[schema(example = 1)]
    pub id: i32,
    #[schema(example = "Weekly Contest #42")]
    pub title: String,
    #[schema(example = "2025-09-30T12:00:00Z")]
    pub activate_time: Option<DateTime<Utc>>,
    #[schema(example = "2025-10-01T14:00:00Z")]
    pub start_time: DateTime<Utc>,
    #[schema(example = "2025-10-01T17:00:00Z")]
    pub end_time: DateTime<Utc>,
    #[schema(example = "2025-10-02T12:00:00Z")]
    pub deactivate_time: Option<DateTime<Utc>>,
    #[schema(example = true)]
    pub is_public: bool,
    #[schema(example = false)]
    pub submissions_visible: bool,
    #[schema(example = true)]
    pub show_compile_output: bool,
    #[schema(example = true)]
    pub show_participants_list: bool,
    #[schema(example = "ioi")]
    pub contest_type: Option<String>,
    #[schema(example = "2025-09-25T10:00:00Z")]
    pub created_at: DateTime<Utc>,
    #[schema(example = "2025-09-25T10:30:00Z")]
    pub updated_at: DateTime<Utc>,
    /// Where the contest is now, on the server's clock.
    pub phase: ContestPhase,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ContestListResponse {
    pub data: Vec<ContestListItem>,
    pub pagination: Pagination,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ContestProblemResponse {
    #[schema(example = 1)]
    pub contest_id: i32,
    #[schema(example = 5)]
    pub problem_id: i32,
    #[schema(example = "A")]
    pub label: String,
    #[schema(example = 0)]
    pub position: i32,
    #[schema(example = "Two Sum")]
    pub problem_title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ContestUserContextResponse {
    #[schema(example = 1)]
    pub contest_id: i32,
    #[schema(example = 7)]
    pub user_id: i32,
    #[schema(example = false)]
    pub is_registered: bool,
    #[schema(example = "2025-09-30T12:00:00Z")]
    pub registered_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CreateUserEntry {
    #[schema(example = "charlie")]
    pub username: String,
    pub display_name: Option<String>,
    #[schema(example = "custom_pass123")]
    pub password: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BulkAddParticipantsRequest {
    #[serde(default)]
    pub usernames: Vec<String>,
    #[serde(default)]
    pub create_users: Vec<CreateUserEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BulkParticipantAdded {
    #[schema(example = 5)]
    pub user_id: i32,
    #[schema(example = "alice")]
    pub username: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BulkParticipantCreated {
    #[schema(example = 12)]
    pub user_id: i32,
    #[schema(example = "charlie")]
    pub username: String,
    pub display_name: String,
    #[schema(example = "aB3$kLm9xQ2z")]
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BulkAddParticipantsResponse {
    pub added: Vec<BulkParticipantAdded>,
    pub created: Vec<BulkParticipantCreated>,
    pub already_enrolled: Vec<BulkParticipantAdded>,
    pub not_found: Vec<String>,
}
