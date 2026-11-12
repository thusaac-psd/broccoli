//! Cursor pages of a submission list, filled only with rows the caller is
//! shown.
//!
//! A visibility plugin may hide rows the list's SQL matches. A page offset,
//! or a look-ahead row counted in SQL, would then disclose them: walking
//! one-row pages, every empty page that still reports more is a hidden row.
//! So a page is filled from the rows the kernel shows, fetched in chunks
//! after a keyset position, and `has_more` says whether one more shown row
//! exists. The cursor names the last row shown, and is accepted only while
//! the caller is shown that row, so its position is never a hidden row's.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sea_orm::sea_query::{Expr, SimpleExpr};
use sea_orm::{Order, QueryFilter, QueryOrder, QuerySelect, Select};
use serde::{Deserialize, Serialize};

use super::filter::apply_filter_to_list;
use super::response::build_submission_list_items;
use crate::entity::{submission, user};
use crate::error::AppError;
use crate::models::shared::ListPagination;
use crate::services::submission_feedback::public_status_sql;
use crate::state::AppState;
use crate::visibility::{Action, Resource, VisibilityKernel};

/// Most rows decided in one kernel batch. The first chunk is one page plus
/// the look-ahead row, so a list nothing hides costs one query and one
/// batch, as an offset page did; each further chunk doubles, up to this.
const MAX_CHUNK: u64 = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SortKey {
    CreatedAt,
    Status,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SortDir {
    Asc,
    Desc,
}

/// The total order of a submission list: its sort key, then `id`, both in
/// the requested direction.
#[derive(Clone, Copy, Debug)]
pub(super) struct ListOrder {
    key: SortKey,
    dir: SortDir,
    can_view_all: bool,
}

/// What a cursor encodes: the order it was issued for and the last row shown.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Cursor {
    by: SortKey,
    order: SortDir,
    id: i32,
}

impl ListOrder {
    /// The order a list query asks for (already validated by
    /// `validate_sorting_params`). `can_view_all` picks which status a
    /// status sort compares: the one this viewer sees.
    pub fn from_query(sort_by: Option<&str>, sort_order: Option<&str>, can_view_all: bool) -> Self {
        Self {
            key: match sort_by {
                Some("status") => SortKey::Status,
                _ => SortKey::CreatedAt,
            },
            dir: match sort_order {
                Some("asc") => SortDir::Asc,
                _ => SortDir::Desc,
            },
            can_view_all,
        }
    }

    fn order(&self) -> Order {
        match self.dir {
            SortDir::Asc => Order::Asc,
            SortDir::Desc => Order::Desc,
        }
    }

    /// The sort key as SQL over the `submission` row aliased `row`.
    fn key_sql(&self, row: &str) -> String {
        match self.key {
            SortKey::CreatedAt => format!("{row}.created_at"),
            SortKey::Status => public_status_sql(self.can_view_all, row),
        }
    }

    /// Rows strictly after submission `id` in this order. The position is
    /// read from that row in the same statement.
    fn after(&self, id: i32) -> SimpleExpr {
        let cmp = match self.dir {
            SortDir::Asc => ">",
            SortDir::Desc => "<",
        };
        Expr::cust_with_values(
            format!(
                "({key}, submission.id) {cmp} ((SELECT {cursor_key} FROM submission c WHERE c.id = $1), $1)",
                key = self.key_sql("submission"),
                cursor_key = self.key_sql("c"),
            ),
            [id],
        )
    }

    fn encode(&self, id: i32) -> String {
        let cursor = Cursor {
            by: self.key,
            order: self.dir,
            id,
        };
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&cursor).expect("a cursor serializes"))
    }

    /// The row id `raw` resumes after, if it was issued for this order.
    fn decode(&self, raw: &str) -> Result<i32, AppError> {
        let cursor: Cursor = URL_SAFE_NO_PAD
            .decode(raw)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or_else(invalid_cursor)?;
        if (cursor.by, cursor.order) != (self.key, self.dir) {
            return Err(AppError::Validation(
                "cursor does not match sort_by/sort_order".into(),
            ));
        }
        Ok(cursor.id)
    }
}

fn invalid_cursor() -> AppError {
    AppError::Validation("Invalid cursor".into())
}

/// One page of `select` in `order`, after `cursor`, holding up to `per_page`
/// rows the caller is shown. `total` is the exact count for this caller, or
/// `None` when it is withheld.
pub(super) async fn visible_page(
    state: &AppState,
    kernel: &VisibilityKernel<'_>,
    select: Select<submission::Entity>,
    order: ListOrder,
    cursor: Option<&str>,
    per_page: u64,
    total: Option<u64>,
) -> Result<(Vec<serde_json::Value>, ListPagination), AppError> {
    // The cursor row is decided with the first chunk: one hidden from the
    // caller (or gone) is rejected like any malformed cursor.
    let mut cursor_row = cursor.map(|raw| order.decode(raw)).transpose()?;
    let mut after = cursor_row;
    let wanted = per_page as usize + 1;
    let mut chunk = per_page + 1;
    let mut shown = Vec::with_capacity(wanted);

    loop {
        let mut rows = select.clone();
        if let Some(id) = after {
            rows = rows.filter(order.after(id));
        }
        let rows = rows
            .find_also_related(user::Entity)
            .order_by(Expr::cust(order.key_sql("submission")), order.order())
            .order_by(submission::Column::Id, order.order())
            .limit(chunk)
            .all(&state.db)
            .await?;
        let exhausted = (rows.len() as u64) < chunk;

        let mut resources: Vec<Resource> = rows
            .iter()
            .map(|(row, _)| Resource::Submission(row.id))
            .collect();
        if let Some(id) = cursor_row.take() {
            resources.push(Resource::Submission(id));
        }
        let mut decisions = kernel.decide_batch(Action::Read, &resources).await?;
        if resources.len() > rows.len() && decisions.pop().is_none_or(|d| d.is_denied()) {
            return Err(invalid_cursor());
        }

        after = rows.last().map(|(row, _)| row.id).or(after);
        for (row, decision) in rows.into_iter().zip(decisions) {
            if !decision.is_denied() {
                shown.push(row);
            }
        }
        if shown.len() >= wanted || exhausted {
            break;
        }
        chunk = (chunk * 2).min(MAX_CHUNK);
    }

    let has_more = shown.len() > per_page as usize;
    shown.truncate(per_page as usize);
    let next_cursor = if has_more {
        shown.last().map(|(row, _)| order.encode(row.id))
    } else {
        None
    };

    let items = build_submission_list_items(&state.db, shown).await?;
    let data = apply_filter_to_list(kernel, items).await?;
    Ok((
        data,
        ListPagination {
            per_page,
            total,
            total_pages: total.map(|t| t.div_ceil(per_page)),
            has_more,
            next_cursor,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DbBackend, EntityTrait, QueryTrait};

    fn order(sort_by: &str, sort_order: &str, can_view_all: bool) -> ListOrder {
        ListOrder::from_query(Some(sort_by), Some(sort_order), can_view_all)
    }

    #[test]
    fn a_cursor_round_trips_for_its_own_order() {
        let order = order("created_at", "desc", true);
        assert_eq!(order.decode(&order.encode(42)).unwrap(), 42);
    }

    #[test]
    fn a_cursor_is_rejected_for_another_order() {
        let issued = order("created_at", "desc", true).encode(42);
        for other in [
            order("created_at", "asc", true),
            order("status", "desc", true),
        ] {
            let err = other.decode(&issued).unwrap_err();
            assert!(matches!(err, AppError::Validation(_)), "{err:?}");
        }
    }

    #[test]
    fn a_malformed_cursor_is_a_validation_error() {
        let order = order("created_at", "desc", true);
        for raw in ["", "not base64!", &URL_SAFE_NO_PAD.encode(b"{}")] {
            assert!(matches!(order.decode(raw), Err(AppError::Validation(_))));
        }
    }

    #[test]
    fn the_keyset_compares_the_sort_key_then_id_in_the_sort_direction() {
        let sql = |order: ListOrder| {
            submission::Entity::find()
                .filter(order.after(7))
                .build(DbBackend::Postgres)
                .to_string()
        };
        let desc = sql(order("created_at", "desc", true));
        assert!(
            desc.contains(
                "(submission.created_at, submission.id) < ((SELECT c.created_at FROM submission c WHERE c.id = 7), 7)"
            ),
            "{desc}"
        );
        let asc = sql(order("status", "asc", false));
        assert!(asc.contains(", submission.id) > ((SELECT CASE"), "{asc}");
        assert!(asc.contains("f.submission_id = c.id"), "{asc}");
    }
}
