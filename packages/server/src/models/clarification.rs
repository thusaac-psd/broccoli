use serde::Deserialize;

use crate::error::AppError;
pub use broccoli_api_types::clarification::{
    ClarificationListResponse, ClarificationReplyResponse, ClarificationResponse,
    CreateClarificationRequest,
};

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
