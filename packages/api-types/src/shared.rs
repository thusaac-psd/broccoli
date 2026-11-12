//! Types shared by several endpoints.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Pagination {
    #[schema(example = 1)]
    pub page: u64,
    #[schema(example = 20)]
    pub per_page: u64,
    #[schema(example = 47)]
    pub total: u64,
    #[schema(example = 3)]
    pub total_pages: u64,
}
