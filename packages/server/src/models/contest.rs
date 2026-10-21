use chrono::{DateTime, Utc};
use sea_orm::FromQueryResult;
use serde::{Deserialize, Serialize};

use super::shared::{
    Pagination, double_option, validate_bulk_ids, validate_optional_position, validate_reorder_ids,
    validate_title,
};
use crate::error::AppError;

#[derive(Deserialize, utoipa::ToSchema)]
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

#[derive(Deserialize, Default, PartialEq, utoipa::ToSchema)]
pub struct UpdateContestRequest {
    #[schema(example = "Weekly Contest #42 (Extended)")]
    pub title: Option<String>,
    #[schema(example = "Updated description...")]
    pub description: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    #[schema(example = "2025-09-30T12:00:00Z")]
    pub activate_time: Option<Option<DateTime<Utc>>>,
    #[schema(example = "2025-10-01T13:00:00Z")]
    pub start_time: Option<DateTime<Utc>>,
    #[schema(example = "2025-10-01T18:00:00Z")]
    pub end_time: Option<DateTime<Utc>>,
    #[serde(default, deserialize_with = "double_option")]
    #[schema(example = "2025-10-02T12:00:00Z")]
    pub deactivate_time: Option<Option<DateTime<Utc>>>,
    #[schema(example = false)]
    pub is_public: Option<bool>,
    #[schema(example = true)]
    pub submissions_visible: Option<bool>,
    #[schema(example = true)]
    pub show_compile_output: Option<bool>,
    #[schema(example = true)]
    pub show_participants_list: Option<bool>,
    /// Omit to keep the current type. A contest type cannot be removed.
    #[schema(example = "icpc")]
    pub contest_type: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct AddContestProblemRequest {
    #[schema(example = 1)]
    pub problem_id: i32,
    #[schema(example = "A")]
    pub label: String,
    #[schema(example = 0)]
    pub position: Option<i32>,
}

#[derive(Deserialize, Default, PartialEq, utoipa::ToSchema)]
pub struct UpdateContestProblemRequest {
    #[schema(example = "B")]
    pub label: Option<String>,
    #[schema(example = 1)]
    pub position: Option<i32>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct AddParticipantRequest {
    #[schema(example = 7)]
    pub user_id: i32,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ReorderContestProblemsRequest {
    #[schema(example = json!([3, 1, 2]))]
    pub problem_ids: Vec<i32>,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ContestListQuery {
    #[param(example = 1)]
    pub page: Option<u64>,
    #[param(example = 20)]
    pub per_page: Option<u64>,
    #[param(example = "weekly")]
    pub search: Option<String>,
    #[param(example = "start_time")]
    pub sort_by: Option<String>,
    #[param(example = "asc")]
    pub sort_order: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
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
}

#[derive(Serialize, FromQueryResult, utoipa::ToSchema)]
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
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ContestListResponse {
    pub data: Vec<ContestListItem>,
    pub pagination: Pagination,
}

#[derive(Serialize, utoipa::ToSchema)]
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

#[derive(Serialize, utoipa::ToSchema)]
pub struct ContestParticipantResponse {
    #[schema(example = 1)]
    pub contest_id: i32,
    #[schema(example = 7)]
    pub user_id: i32,
    #[schema(example = "alice_wonder")]
    pub username: String,
    pub display_name: String,
    pub is_deleted: bool,
    #[schema(example = "2025-09-30T12:00:00Z")]
    pub registered_at: DateTime<Utc>,
}

#[derive(Serialize, utoipa::ToSchema)]
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

impl From<crate::entity::contest::Model> for ContestResponse {
    fn from(m: crate::entity::contest::Model) -> Self {
        Self {
            id: m.id,
            title: m.title,
            description: m.description,
            activate_time: m.activate_time,
            start_time: m.start_time,
            end_time: m.end_time,
            deactivate_time: m.deactivate_time,
            is_public: m.is_public,
            submissions_visible: m.submissions_visible,
            show_compile_output: m.show_compile_output,
            show_participants_list: m.show_participants_list,
            contest_type: m.contest_type,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

pub fn validate_contest_timeline(
    activate_time: Option<DateTime<Utc>>,
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
    deactivate_time: Option<DateTime<Utc>>,
) -> Result<(), AppError> {
    if let Some(activate) = activate_time
        && activate > start_time
    {
        return Err(AppError::Validation(
            "activate_time must be before or equal to start_time".into(),
        ));
    }
    if end_time <= start_time {
        return Err(AppError::Validation(
            "end_time must be after start_time".into(),
        ));
    }
    if let Some(deactivate) = deactivate_time
        && deactivate < end_time
    {
        return Err(AppError::Validation(
            "deactivate_time must be after or equal to end_time".into(),
        ));
    }
    Ok(())
}

pub fn validate_create_contest(req: &CreateContestRequest) -> Result<(), AppError> {
    validate_title(&req.title)?;
    if req.description.trim().is_empty() || req.description.len() > 1_000_000 {
        return Err(AppError::Validation(
            "Description must be non-empty and at most 1MB".into(),
        ));
    }
    validate_contest_timeline(
        req.activate_time.unwrap_or(None),
        req.start_time,
        req.end_time,
        req.deactivate_time.unwrap_or(None),
    )?;
    Ok(())
}

pub fn validate_update_contest(req: &UpdateContestRequest) -> Result<(), AppError> {
    if let Some(ref title) = req.title {
        validate_title(title)?;
    }
    if let Some(ref description) = req.description
        && (description.trim().is_empty() || description.len() > 1_000_000)
    {
        return Err(AppError::Validation(
            "Description must be non-empty and at most 1MB".into(),
        ));
    }
    Ok(())
}

pub fn validate_add_contest_problem(req: &AddContestProblemRequest) -> Result<(), AppError> {
    let label = req.label.trim();
    if label.is_empty() || label.chars().count() > 10 {
        return Err(AppError::Validation("Label must be 1-10 characters".into()));
    }
    validate_optional_position(req.position)
}

pub fn validate_reorder_contest_problems(
    req: &ReorderContestProblemsRequest,
) -> Result<(), AppError> {
    validate_reorder_ids(&req.problem_ids, "problem_id")
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct BulkDeleteContestProblemsRequest {
    #[schema(example = json!([5, 7, 9]))]
    pub problem_ids: Vec<i32>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct BulkDeleteContestProblemsResponse {
    #[schema(example = 3)]
    pub removed: usize,
}

pub fn validate_bulk_delete_contest_problems(
    req: &BulkDeleteContestProblemsRequest,
) -> Result<(), AppError> {
    validate_bulk_ids(&req.problem_ids, "problem_ids", 1000)
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct CreateUserEntry {
    #[schema(example = "charlie")]
    pub username: String,
    pub display_name: Option<String>,
    #[schema(example = "custom_pass123")]
    pub password: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct BulkAddParticipantsRequest {
    #[serde(default)]
    pub usernames: Vec<String>,
    #[serde(default)]
    pub create_users: Vec<CreateUserEntry>,
}

#[derive(Clone, Serialize, utoipa::ToSchema)]
pub struct BulkParticipantAdded {
    #[schema(example = 5)]
    pub user_id: i32,
    #[schema(example = "alice")]
    pub username: String,
    pub display_name: String,
}

impl From<crate::entity::user::Model> for BulkParticipantAdded {
    fn from(user: crate::entity::user::Model) -> Self {
        Self {
            user_id: user.id,
            username: user.username,
            display_name: user.display_name,
        }
    }
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct BulkParticipantCreated {
    #[schema(example = 12)]
    pub user_id: i32,
    #[schema(example = "charlie")]
    pub username: String,
    pub display_name: String,
    #[schema(example = "aB3$kLm9xQ2z")]
    pub password: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct BulkAddParticipantsResponse {
    pub added: Vec<BulkParticipantAdded>,
    pub created: Vec<BulkParticipantCreated>,
    pub already_enrolled: Vec<BulkParticipantAdded>,
    pub not_found: Vec<String>,
}

pub fn validate_bulk_add_participants(req: &BulkAddParticipantsRequest) -> Result<(), AppError> {
    if req.usernames.is_empty() && req.create_users.is_empty() {
        return Err(AppError::Validation(
            "At least one of 'usernames' or 'create_users' must be provided".into(),
        ));
    }

    let total = req.usernames.len() + req.create_users.len();
    if total > 1000 {
        return Err(AppError::Validation(
            "Combined usernames and create_users cannot exceed 1,000 entries".into(),
        ));
    }

    let mut seen = std::collections::HashSet::new();
    for name in &req.usernames {
        let trimmed = name.trim().to_lowercase();
        if trimmed.is_empty() {
            return Err(AppError::Validation("Username must not be empty".into()));
        }
        if !seen.insert(trimmed.clone()) {
            return Err(AppError::Validation(format!(
                "Duplicate username: '{}'",
                name.trim()
            )));
        }
    }
    for entry in &req.create_users {
        let trimmed = entry.username.trim().to_lowercase();
        if trimmed.is_empty() {
            return Err(AppError::Validation("Username must not be empty".into()));
        }
        if !seen.insert(trimmed) {
            return Err(AppError::Validation(format!(
                "Duplicate username: '{}'",
                entry.username.trim()
            )));
        }
        super::user::validate_account_credentials(&entry.username, entry.password.as_deref())?;
    }

    Ok(())
}

pub fn validate_update_contest_problem(req: &UpdateContestProblemRequest) -> Result<(), AppError> {
    if let Some(ref label) = req.label {
        let label = label.trim();
        if label.is_empty() || label.chars().count() > 10 {
            return Err(AppError::Validation("Label must be 1-10 characters".into()));
        }
    }
    validate_optional_position(req.position)
}
