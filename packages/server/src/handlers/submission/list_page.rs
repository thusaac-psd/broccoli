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
//!
//! A row the kernel shows may still have fields hidden from the caller. A
//! list filtered or sorted by such a field would disclose its value through
//! which rows match or where a row lands, so a row is shown in a list only
//! when every field the list filters on is visible to the caller. A list
//! sorted by status shows the rows whose status the caller cannot see as
//! their own group after every other row, ordered by id alone: their place
//! says nothing of their status.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sea_orm::sea_query::{Expr, SimpleExpr};
use std::collections::HashSet;

use sea_orm::{ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder, QuerySelect, Select};
use serde::{Deserialize, Serialize};

use super::filter::apply_filter_to_list;
use super::response::build_submission_list_items;
use crate::utils::diagnostics::Diagnostics;
// visibility-bypass-audited: `visible_page` decides every row it fetches with
// `VisibilityKernel::decide_batch` and keeps only the ones not denied, and
// decides the cursor row in the same batch, so no row reaches a page, or
// positions one, without the kernel.
use crate::entity::{submission, user};
use crate::error::AppError;
use crate::models::shared::ListPagination;
use crate::models::submission::SubmissionListQuery;
use crate::services::submission_feedback::public_status_sql;
use crate::state::AppState;
use crate::visibility::{Action, Decision, Resource, VisibilityKernel};
use broccoli_api_types::submission::SubmissionListItem;

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

/// Which rows of a list a pass over it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Group {
    /// Rows whose sort key the caller sees, in the list's order.
    Shown,
    /// Rows of a status sort whose status is withheld from the caller, after
    /// every shown row, by id in the list's direction.
    Withheld,
}

/// The total order of a submission list: its sort key, then `id`, both in
/// the requested direction.
#[derive(Clone, Copy, Debug)]
pub(super) struct ListOrder {
    key: SortKey,
    dir: SortDir,
    sees_withheld: bool,
}

/// What a cursor encodes: the order it was issued for and the last row shown.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Cursor {
    by: SortKey,
    order: SortDir,
    group: Group,
    id: i32,
}

impl ListOrder {
    /// The order a list query asks for (already validated by
    /// `validate_sorting_params`). `sees_withheld` picks which status a
    /// status sort compares: the one this viewer sees.
    pub fn from_query(
        sort_by: Option<&str>,
        sort_order: Option<&str>,
        sees_withheld: bool,
    ) -> Self {
        Self {
            key: match sort_by {
                Some("status") => SortKey::Status,
                _ => SortKey::CreatedAt,
            },
            dir: match sort_order {
                Some("asc") => SortDir::Asc,
                _ => SortDir::Desc,
            },
            sees_withheld,
        }
    }

    fn order(&self) -> Order {
        match self.dir {
            SortDir::Asc => Order::Asc,
            SortDir::Desc => Order::Desc,
        }
    }

    /// The list item field the sort key orders by.
    fn key_field(&self) -> &'static str {
        match self.key {
            SortKey::CreatedAt => "created_at",
            SortKey::Status => "status",
        }
    }

    /// The sort key as SQL over the `submission` row aliased `row`.
    fn key_sql(&self, row: &str) -> String {
        match self.key {
            SortKey::CreatedAt => format!("{row}.created_at"),
            SortKey::Status => public_status_sql(self.sees_withheld, row),
        }
    }

    /// The groups a list in this order is shown in. Only a status sort for a
    /// caller some outcome may be withheld from has a withheld group.
    fn groups(&self) -> &'static [Group] {
        match self.key {
            SortKey::Status if !self.sees_withheld => &[Group::Shown, Group::Withheld],
            SortKey::Status | SortKey::CreatedAt => &[Group::Shown],
        }
    }

    /// The sort key as SQL for `group`: the withheld group is ordered by id
    /// alone.
    fn group_key_sql(&self, group: Group, row: &str) -> Option<String> {
        match group {
            Group::Shown => Some(self.key_sql(row)),
            Group::Withheld => None,
        }
    }

    /// Rows strictly after submission `id` of `group` in this order. The
    /// position is read from that row in the same statement.
    fn after(&self, group: Group, id: i32) -> SimpleExpr {
        let cmp = match self.dir {
            SortDir::Asc => ">",
            SortDir::Desc => "<",
        };
        match self.group_key_sql(group, "submission") {
            Some(key) => Expr::cust_with_values(
                format!(
                    "({key}, submission.id) {cmp} ((SELECT {cursor_key} FROM submission c WHERE c.id = $1), $1)",
                    cursor_key = self.key_sql("c"),
                ),
                [id],
            ),
            None => Expr::cust_with_values(format!("submission.id {cmp} $1"), [id]),
        }
    }

    fn encode(&self, group: Group, id: i32) -> String {
        let cursor = Cursor {
            by: self.key,
            order: self.dir,
            group,
            id,
        };
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&cursor).expect("a cursor serializes"))
    }

    /// The group and row id `raw` resumes after, if it was issued for this
    /// order.
    fn decode(&self, raw: &str) -> Result<(Group, i32), AppError> {
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
        if !self.groups().contains(&cursor.group) {
            return Err(invalid_cursor());
        }
        Ok((cursor.group, cursor.id))
    }
}

/// The list item fields `query` filters rows on.
pub(super) fn filtered_fields(query: &SubmissionListQuery) -> Vec<&'static str> {
    let mut fields = Vec::new();
    if query.problem_id.is_some() {
        fields.push("problem_id");
    }
    if query.user_id.is_some() {
        fields.push("user_id");
    }
    if query.language.is_some() {
        fields.push("language");
    }
    if query.status.is_some() {
        fields.push("status");
    }
    if query.q.is_some() {
        fields.extend(["username", "problem_title", "contest_id"]);
    }
    fields
}

fn invalid_cursor() -> AppError {
    AppError::Validation("Invalid cursor".into())
}

/// One page of `select` in `order`, after `cursor`, holding up to `per_page`
/// rows the caller is shown. `filtered_on` names the fields `select` filters
/// on (see [`filtered_fields`]). `total` is the exact count for this caller,
/// or `None` when it is withheld. `diagnostics` is what the caller sees of
/// the platform's details.
// The page is one request's parameters; a struct would only rename them.
#[allow(clippy::too_many_arguments)]
pub(super) async fn visible_page(
    state: &AppState,
    kernel: &VisibilityKernel<'_>,
    select: Select<submission::Entity>,
    filtered_on: &[&'static str],
    order: ListOrder,
    cursor: Option<&str>,
    per_page: u64,
    total: Option<u64>,
    diagnostics: Diagnostics,
) -> Result<(Vec<serde_json::Value>, ListPagination), AppError> {
    let wanted = per_page as usize + 1;
    let mut shown: Vec<(Group, (submission::Model, Option<user::Model>))> =
        Vec::with_capacity(wanted);
    // A status filter matches no withheld status, so only an unfiltered
    // status sort has rows in the withheld group.
    let groups: Vec<Group> = order
        .groups()
        .iter()
        .copied()
        .filter(|g| *g == Group::Shown || !filtered_on.contains(&"status"))
        .collect();
    let (start, mut cursor_row) = match cursor.map(|raw| order.decode(raw)).transpose()? {
        Some((group, id)) => (group, Some(id)),
        None => (Group::Shown, None),
    };
    if !groups.contains(&start) {
        return Err(invalid_cursor());
    }

    for &group in groups.iter().skip_while(|g| **g != start) {
        if shown.len() >= wanted {
            break;
        }
        // The cursor row is decided with the first chunk of its group: one
        // hidden from the caller (or gone) is rejected like any malformed
        // cursor.
        let mut after = cursor_row;
        let mut chunk = per_page + 1;
        loop {
            let mut rows = select.clone();
            if group == Group::Shown && order.groups().contains(&Group::Withheld) {
                rows = rows.filter(Expr::cust(format!(
                    "({}) IS NOT NULL",
                    order.key_sql("submission")
                )));
            }
            if let Some(id) = after {
                rows = rows.filter(order.after(group, id));
            }
            let mut rows = rows.find_also_related(user::Entity);
            if let Some(key) = order.group_key_sql(group, "submission") {
                rows = rows.order_by(Expr::cust(key), order.order());
            }
            let rows = rows
                .order_by(submission::Column::Id, order.order())
                .limit(chunk)
                .all(&state.db)
                .await?;
            let exhausted = (rows.len() as u64) < chunk;

            let mut ids: Vec<i32> = rows.iter().map(|(row, _)| row.id).collect();
            let cursor_id = cursor_row.take();
            ids.extend(cursor_id);
            let resources: Vec<Resource> = ids.iter().copied().map(Resource::Submission).collect();
            let decisions = kernel.decide_batch(Action::Read, &resources).await?;
            let held = match group {
                Group::Shown => HashSet::new(),
                Group::Withheld => withheld_ids(state, &order, &ids).await?,
            };
            let listed = |id: i32, decision: &Decision| {
                let filters_shown = !filtered_on
                    .iter()
                    .any(|f| decision.hides::<SubmissionListItem>(f));
                let key_hidden =
                    decision.hides::<SubmissionListItem>(order.key_field()) || held.contains(&id);
                filters_shown
                    && match group {
                        Group::Shown => !key_hidden,
                        Group::Withheld => key_hidden,
                    }
            };
            if let Some(id) = cursor_id
                && !listed(id, &decisions[rows.len()])
            {
                return Err(invalid_cursor());
            }

            after = rows.last().map(|(row, _)| row.id).or(after);
            for (row, decision) in rows.into_iter().zip(&decisions) {
                if listed(row.0.id, decision) {
                    shown.push((group, row));
                }
            }
            if shown.len() >= wanted || exhausted {
                break;
            }
            chunk = (chunk * 2).min(MAX_CHUNK);
        }
    }

    let has_more = shown.len() > per_page as usize;
    shown.truncate(per_page as usize);
    let next_cursor = if has_more {
        shown
            .last()
            .map(|(group, (row, _))| order.encode(*group, row.id))
    } else {
        None
    };

    let rows = shown.into_iter().map(|(_, row)| row).collect();
    let items = build_submission_list_items(state, rows, diagnostics).await?;
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

/// Which of `ids` the caller sees no status of in SQL: the plugin withholds
/// their feedback (see `submission_feedback::public_status`).
async fn withheld_ids(
    state: &AppState,
    order: &ListOrder,
    ids: &[i32],
) -> Result<HashSet<i32>, AppError> {
    if ids.is_empty() {
        return Ok(HashSet::new());
    }
    let rows = submission::Entity::find()
        .select_only()
        .column(submission::Column::Id)
        .filter(submission::Column::Id.is_in(ids.to_vec()))
        .filter(Expr::cust(format!(
            "({}) IS NULL",
            public_status_sql(order.sees_withheld, "submission")
        )))
        .into_tuple::<i32>()
        .all(&state.db)
        .await?;
    Ok(rows.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DbBackend, EntityTrait, QueryTrait};

    fn order(sort_by: &str, sort_order: &str, sees_withheld: bool) -> ListOrder {
        ListOrder::from_query(Some(sort_by), Some(sort_order), sees_withheld)
    }

    #[test]
    fn a_cursor_round_trips_for_its_own_order() {
        let order = order("created_at", "desc", true);
        assert_eq!(
            order.decode(&order.encode(Group::Shown, 42)).unwrap(),
            (Group::Shown, 42)
        );
    }

    #[test]
    fn a_cursor_is_rejected_for_another_order() {
        let issued = order("created_at", "desc", true).encode(Group::Shown, 42);
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
                .filter(order.after(Group::Shown, 7))
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

    #[test]
    fn the_withheld_group_follows_by_id_alone_for_a_caller_outcomes_are_withheld_from() {
        assert_eq!(
            order("status", "asc", false).groups(),
            &[Group::Shown, Group::Withheld]
        );
        assert_eq!(order("status", "asc", true).groups(), &[Group::Shown]);
        assert_eq!(order("created_at", "asc", false).groups(), &[Group::Shown]);
        let sql = submission::Entity::find()
            .filter(order("status", "desc", false).after(Group::Withheld, 7))
            .build(DbBackend::Postgres)
            .to_string();
        assert!(sql.contains("submission.id < 7"), "{sql}");
        assert!(!sql.contains("CASE"), "{sql}");
    }

    #[test]
    fn a_withheld_group_cursor_is_rejected_where_there_is_no_such_group() {
        let issued = order("status", "asc", false).encode(Group::Withheld, 3);
        assert_eq!(
            order("status", "asc", false).decode(&issued).unwrap(),
            (Group::Withheld, 3)
        );
        assert!(matches!(
            order("status", "asc", true).decode(&issued),
            Err(AppError::Validation(_))
        ));
    }
}
