use std::collections::HashSet;

use serde::{Deserialize, Deserializer, Serialize};

use crate::error::AppError;

#[derive(Clone, Serialize, Deserialize, utoipa::ToSchema)]
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

/// A validated page request: the only place an SQL `OFFSET` is derived from
/// caller-supplied `page`/`per_page`.
///
/// `(page - 1) * per_page` on a raw `u64` overflows (panic in debug, a wrapped
/// offset in release) and anything above `i64::MAX` is rejected by Postgres'
/// `bigint` OFFSET with a 500. Construction checks both and maps them to
/// `400 VALIDATION_ERROR`, so list handlers cannot get this wrong by hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageWindow {
    page: u64,
    per_page: u64,
    offset: u64,
}

impl PageWindow {
    /// Page size used by list endpoints when `per_page` is omitted.
    pub const DEFAULT_PER_PAGE: u64 = 20;
    /// Largest page size list endpoints accept; larger values are clamped.
    pub const MAX_PER_PAGE: u64 = 100;

    /// The page size of a cursor-paged list: `per_page` with the standard
    /// bounds (default 20, clamped to `1..=100`).
    pub fn page_size(per_page: Option<u64>) -> u64 {
        per_page
            .unwrap_or(Self::DEFAULT_PER_PAGE)
            .clamp(1, Self::MAX_PER_PAGE)
    }

    /// Window with the standard list-endpoint bounds (default 20, max 100).
    pub fn new(page: Option<u64>, per_page: Option<u64>) -> Result<Self, AppError> {
        Self::with_bounds(page, per_page, Self::DEFAULT_PER_PAGE, Self::MAX_PER_PAGE)
    }

    /// Window with endpoint-specific page-size bounds. `page` defaults to and
    /// is floored at 1; `per_page` defaults to `default_per_page` and is
    /// clamped to `1..=max_per_page`.
    pub fn with_bounds(
        page: Option<u64>,
        per_page: Option<u64>,
        default_per_page: u64,
        max_per_page: u64,
    ) -> Result<Self, AppError> {
        let page = page.unwrap_or(1).max(1);
        let per_page = per_page
            .unwrap_or(default_per_page)
            .clamp(1, max_per_page.max(1));
        let offset = (page - 1)
            .checked_mul(per_page)
            .filter(|n| i64::try_from(*n).is_ok())
            .ok_or_else(|| AppError::Validation("page is too large".into()))?;
        Ok(Self {
            page,
            per_page,
            offset,
        })
    }

    pub fn page(&self) -> u64 {
        self.page
    }

    /// Rows per page; use as the SQL `LIMIT`.
    pub fn limit(&self) -> u64 {
        self.per_page
    }

    /// Rows to skip; always fits in a Postgres `bigint`.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Response metadata for this window given the total row count.
    pub fn pagination(&self, total: u64) -> Pagination {
        Pagination {
            page: self.page,
            per_page: self.per_page,
            total,
            total_pages: total.div_ceil(self.per_page),
        }
    }
}

pub fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

pub fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(Option::deserialize(deserializer)?))
}

pub fn validate_title(title: &str) -> Result<(), AppError> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 256 {
        return Err(AppError::Validation(
            "Title must be 1-256 characters".into(),
        ));
    }
    Ok(())
}

pub fn validate_optional_position(pos: Option<i32>) -> Result<(), AppError> {
    if let Some(pos) = pos
        && pos < 0
    {
        return Err(AppError::Validation("Position must be >= 0".into()));
    }
    Ok(())
}

pub fn validate_reorder_ids(ids: &[i32], name: &str) -> Result<(), AppError> {
    if ids.is_empty() {
        return Err(AppError::Validation(format!("{name}s must not be empty")));
    }
    let mut seen = HashSet::new();
    for &id in ids {
        if !seen.insert(id) {
            return Err(AppError::Validation(format!(
                "Duplicate {name} {id} in reorder list"
            )));
        }
    }
    Ok(())
}

pub fn validate_bulk_ids(ids: &[i32], name: &str, max: usize) -> Result<(), AppError> {
    if ids.is_empty() {
        return Err(AppError::Validation(format!("{name} must not be empty")));
    }
    if ids.len() > max {
        return Err(AppError::Validation(format!("Too many {name}: max {max}")));
    }
    let mut seen = HashSet::new();
    for &id in ids {
        if !seen.insert(id) {
            return Err(AppError::Validation(format!("Duplicate {name} ID: {id}")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_window_defaults_and_clamps() {
        let w = PageWindow::new(None, None).unwrap();
        assert_eq!((w.page(), w.limit(), w.offset()), (1, 20, 0));
        let w = PageWindow::new(Some(0), Some(0)).unwrap();
        assert_eq!((w.page(), w.limit(), w.offset()), (1, 1, 0));
        let w = PageWindow::new(Some(3), Some(1000)).unwrap();
        assert_eq!((w.page(), w.limit(), w.offset()), (3, 100, 200));
        assert_eq!(w.pagination(201).total_pages, 3);
    }

    #[test]
    fn page_window_rejects_offsets_outside_bigint() {
        // (page - 1) * per_page overflows u64.
        assert!(PageWindow::new(Some(u64::MAX), Some(20)).is_err());
        // Fits in u64 but is exactly one past i64::MAX.
        assert!(PageWindow::new(Some((1 << 62) + 1), Some(2)).is_err());
        // The largest offset Postgres accepts is still allowed.
        let w = PageWindow::new(Some(i64::MAX as u64 + 1), Some(1)).unwrap();
        assert_eq!(w.offset(), i64::MAX as u64);
    }
}
