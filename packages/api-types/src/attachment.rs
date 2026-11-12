//! Problem attachment responses.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AttachmentResponse {
    #[schema(example = "01936f0e-1234-7abc-8000-000000000001")]
    pub id: String,
    #[schema(example = "images/figure1.png")]
    pub path: String,
    #[schema(example = "figure1.png")]
    pub filename: String,
    #[schema(example = "image/png")]
    pub content_type: Option<String>,
    #[schema(example = 142857)]
    pub size: i64,
    #[schema(example = "a1b2c3d4e5f6...")]
    pub content_hash: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AttachmentListResponse {
    pub attachments: Vec<AttachmentResponse>,
    pub total: u64,
}
