use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::entity::{additional_file, problem_attachment};
pub use broccoli_api_types::attachment::{AttachmentListResponse, AttachmentResponse};

impl From<problem_attachment::Model> for AttachmentResponse {
    fn from(model: problem_attachment::Model) -> Self {
        Self {
            id: model.id.to_string(),
            path: model.path,
            filename: model.filename,
            content_type: model.content_type,
            size: model.size,
            content_hash: model.content_hash,
            created_at: model.created_at,
        }
    }
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct AdditionalFileResponse {
    #[schema(example = "01936f0e-1234-7abc-8000-000000000002")]
    pub id: String,
    #[schema(example = "cpp")]
    pub language: String,
    #[schema(example = "include/grader.h")]
    pub path: String,
    #[schema(example = "grader.h")]
    pub filename: String,
    #[schema(example = "text/x-c")]
    pub content_type: Option<String>,
    #[schema(example = 2048)]
    pub size: i64,
    #[schema(example = "a1b2c3d4e5f6...")]
    pub content_hash: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct AdditionalFileListResponse {
    pub files: Vec<AdditionalFileResponse>,
    pub total: u64,
}

impl From<additional_file::Model> for AdditionalFileResponse {
    fn from(model: additional_file::Model) -> Self {
        Self {
            id: model.id.to_string(),
            language: model.language,
            path: model.path,
            filename: model.filename,
            content_type: model.content_type,
            size: model.size,
            content_hash: model.content_hash,
            created_at: model.created_at,
        }
    }
}
