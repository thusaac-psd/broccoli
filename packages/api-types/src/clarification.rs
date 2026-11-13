//! Contest clarification types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ClarificationListResponse {
    pub data: Vec<ClarificationResponse>,
}
