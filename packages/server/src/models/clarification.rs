use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::AppError;

#[derive(Deserialize, utoipa::ToSchema)]
pub struct CreateClarificationRequest {
    #[schema(example = "Is the input guaranteed to be sorted?")]
    pub content: String,
    #[schema(example = "question")]
    pub clarification_type: String,
    #[schema(example = 7)]
    pub recipient_id: Option<i32>,
    #[schema(example = false)]
    pub is_public: Option<bool>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ReplyClarificationRequest {
    #[schema(example = "Yes, the input is always sorted in ascending order.")]
    pub content: String,
    #[schema(example = true)]
    pub is_public: bool,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ResolveClarificationRequest {
    #[schema(example = true)]
    pub resolved: bool,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ToggleReplyPublicQuery {
    pub include_question: Option<bool>,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ClarificationListQuery {
    #[param(example = "question")]
    pub r#type: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ClarificationReplyResponse {
    #[schema(example = 1)]
    pub id: i32,
    #[schema(example = 42)]
    pub author_id: i32,
    #[schema(example = "admin")]
    pub author_name: String,
    pub content: String,
    #[schema(example = true)]
    pub is_public: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ClarificationResponse {
    #[schema(example = 1)]
    pub id: i32,
    #[schema(example = 1)]
    pub contest_id: i32,
    #[schema(example = 101)]
    pub author_id: i32,
    #[schema(example = "alice")]
    pub author_name: String,
    pub content: String,
    #[schema(example = "question")]
    pub clarification_type: String,
    pub recipient_id: Option<i32>,
    pub recipient_name: Option<String>,
    #[schema(example = false)]
    pub is_public: bool,

    pub replies: Vec<ClarificationReplyResponse>,

    pub resolved: bool,
    pub resolved_at: Option<DateTime<Utc>>,
    pub resolved_by: Option<i32>,
    pub resolved_by_name: Option<String>,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ClarificationListResponse {
    pub data: Vec<ClarificationResponse>,
}

const VALID_TYPES: &[&str] = &["announcement", "question", "direct_message"];

pub fn validate_create_clarification(req: &CreateClarificationRequest) -> Result<(), AppError> {
    let content = req.content.trim();
    if content.is_empty() || content.chars().count() > 10_000 {
        return Err(AppError::Validation(
            "Content must be 1 – 10 000 characters".into(),
        ));
    }
    if !VALID_TYPES.contains(&req.clarification_type.as_str()) {
        return Err(AppError::Validation(format!(
            "Invalid clarification_type '{}'. Must be one of: {}",
            req.clarification_type,
            VALID_TYPES.join(", ")
        )));
    }
    if req.clarification_type == "direct_message" && req.recipient_id.is_none() {
        return Err(AppError::Validation(
            "recipient_id is required for direct_message".into(),
        ));
    }
    Ok(())
}

pub fn validate_reply_clarification(req: &ReplyClarificationRequest) -> Result<(), AppError> {
    let content = req.content.trim();
    if content.is_empty() || content.chars().count() > 10_000 {
        return Err(AppError::Validation(
            "Reply content must be 1 – 10 000 characters".into(),
        ));
    }
    Ok(())
}
