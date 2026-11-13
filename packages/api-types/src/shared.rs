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

/// Cursor pagination of a list that a visibility plugin may narrow below what
/// its SQL matches. Every page holds only rows the caller is shown, and
/// `has_more`/`next_cursor` describe only such rows. A count of matching rows
/// would disclose rows the caller is not shown, so `total` and `total_pages`
/// are present only when the count is exact for this caller.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ListPagination {
    #[schema(example = 20)]
    pub per_page: u64,
    #[schema(example = 47)]
    pub total: Option<u64>,
    #[schema(example = 3)]
    pub total_pages: Option<u64>,
    /// Whether the caller is shown a row after this page.
    #[schema(example = true)]
    pub has_more: bool,
    /// Pass as `cursor` to fetch the next page; set exactly when `has_more`.
    #[schema(example = "eyJieSI6ImNyZWF0ZWRfYXQiLCJvcmRlciI6ImRlc2MiLCJpZCI6NDJ9")]
    pub next_cursor: Option<String>,
}
