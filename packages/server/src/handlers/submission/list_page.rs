//! Cursor pages of a submission list, filled only with rows the caller is
//! shown.
//!
//! A visibility plugin may hide rows the list's SQL matches. A page offset,
//! or a look-ahead row counted in SQL, would then disclose them: walking
//! one-row pages, every empty page that still reports more is a hidden row.
//! So a page is filled from the rows the kernel shows, fetched in chunks
//! after a keyset position, and `has_more` says whether one more shown row
//! exists. The cursor names the last row shown and the sort key it was shown
//! at, and is accepted only while the caller is shown that row, so its
//! position is never a hidden row's. The key travels in the cursor because a
//! row's status changes while a list is walked: a position re-read from the
//! row would move with it, and rows would skip or repeat across pages. The
//! cursor is signed, so the only positions a caller resumes from are ones the
//! server issued.
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
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sea_orm::sea_query::{Expr, SimpleExpr};
use sha2::Sha256;
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
use common::SubmissionStatus;

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

/// Bytes of HMAC-SHA256 a cursor carries.
const CURSOR_TAG_LEN: usize = 16;

/// What a cursor encodes: the order it was issued for and where it resumes.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Cursor {
    order: SortDir,
    at: Position,
}

/// The last row shown, and the key it was listed under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Position {
    /// A row of the shown group, at the sort key it had when it was shown.
    Shown { key: SortValue, id: i32 },
    /// A row of the withheld group, which is ordered by id alone.
    Withheld { id: i32 },
}

/// A row's sort key, as of the page that showed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SortValue {
    CreatedAt(DateTime<Utc>),
    Status(SubmissionStatus),
}

impl Position {
    fn group(&self) -> Group {
        match self {
            Position::Shown { .. } => Group::Shown,
            Position::Withheld { .. } => Group::Withheld,
        }
    }

    fn id(&self) -> i32 {
        match *self {
            Position::Shown { id, .. } | Position::Withheld { id } => id,
        }
    }
}

/// The MAC that binds a cursor's payload to this installation.
fn cursor_mac(secret: &[u8], payload: &[u8]) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC takes any key length");
    mac.update(b"submission-list-cursor\0");
    mac.update(payload);
    mac
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

    /// Where `row`, shown in `group`, puts a cursor. A row of the shown group
    /// has its own status as its key: the group holds only rows whose status
    /// the caller sees.
    fn position(&self, group: Group, row: &submission::Model) -> Position {
        match group {
            Group::Shown => Position::Shown {
                key: match self.key {
                    SortKey::CreatedAt => SortValue::CreatedAt(row.created_at),
                    SortKey::Status => SortValue::Status(row.status),
                },
                id: row.id,
            },
            Group::Withheld => Position::Withheld { id: row.id },
        }
    }

    /// Rows strictly after `at` in this order.
    fn after(&self, at: Position) -> SimpleExpr {
        let cmp = match self.dir {
            SortDir::Asc => ">",
            SortDir::Desc => "<",
        };
        match at {
            Position::Shown { key, id } => {
                let key_value: sea_orm::Value = match key {
                    SortValue::CreatedAt(at) => at.into(),
                    SortValue::Status(status) => status.into(),
                };
                Expr::cust_with_values(
                    format!(
                        "({key}, submission.id) {cmp} ($1, $2)",
                        key = self.key_sql("submission"),
                    ),
                    [key_value, id.into()],
                )
            }
            Position::Withheld { id } => {
                Expr::cust_with_values(format!("submission.id {cmp} $1"), [id])
            }
        }
    }

    /// A cursor resuming after `at`, signed with `secret`.
    fn encode(&self, secret: &[u8], at: Position) -> String {
        let payload = serde_json::to_vec(&Cursor {
            order: self.dir,
            at,
        })
        .expect("a cursor serializes");
        let tag = cursor_mac(secret, &payload).finalize().into_bytes();
        format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(&payload),
            URL_SAFE_NO_PAD.encode(&tag[..CURSOR_TAG_LEN])
        )
    }

    /// Where `raw` resumes, if this server signed it for this order.
    fn decode(&self, secret: &[u8], raw: &str) -> Result<Position, AppError> {
        let (payload, tag) = raw.split_once('.').ok_or_else(invalid_cursor)?;
        let payload = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| invalid_cursor())?;
        let tag = URL_SAFE_NO_PAD.decode(tag).map_err(|_| invalid_cursor())?;
        if tag.len() != CURSOR_TAG_LEN {
            return Err(invalid_cursor());
        }
        cursor_mac(secret, &payload)
            .verify_truncated_left(&tag)
            .map_err(|_| invalid_cursor())?;
        let cursor: Cursor = serde_json::from_slice(&payload).map_err(|_| invalid_cursor())?;
        let key_matches = match cursor.at {
            Position::Shown {
                key: SortValue::CreatedAt(_),
                ..
            } => self.key == SortKey::CreatedAt,
            Position::Shown {
                key: SortValue::Status(_),
                ..
            }
            | Position::Withheld { .. } => self.key == SortKey::Status,
        };
        if !key_matches || cursor.order != self.dir {
            return Err(AppError::Validation(
                "cursor does not match sort_by/sort_order".into(),
            ));
        }
        if !self.groups().contains(&cursor.at.group()) {
            return Err(invalid_cursor());
        }
        Ok(cursor.at)
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
    let secret = state.config.auth.jwt_secret.as_bytes();
    let resume = cursor.map(|raw| order.decode(secret, raw)).transpose()?;
    let start = resume.map_or(Group::Shown, |at| at.group());
    let mut cursor_row = resume.map(|at| at.id());
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
        let mut after = resume.filter(|at| at.group() == group);
        let mut chunk = per_page + 1;
        loop {
            let mut rows = select.clone();
            if group == Group::Shown && order.groups().contains(&Group::Withheld) {
                rows = rows.filter(Expr::cust(format!(
                    "({}) IS NOT NULL",
                    order.key_sql("submission")
                )));
            }
            if let Some(at) = after {
                rows = rows.filter(order.after(at));
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

            after = rows
                .last()
                .map(|(row, _)| order.position(group, row))
                .or(after);
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
            .map(|(group, (row, _))| order.encode(secret, order.position(*group, row)))
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
    use chrono::TimeZone;
    use sea_orm::{DbBackend, EntityTrait, QueryTrait};

    const SECRET: &[u8] = b"a-test-secret-of-at-least-32-bytes";

    fn order(sort_by: &str, sort_order: &str, sees_withheld: bool) -> ListOrder {
        ListOrder::from_query(Some(sort_by), Some(sort_order), sees_withheld)
    }

    fn shown_at(minute: u32, id: i32) -> Position {
        Position::Shown {
            key: SortValue::CreatedAt(Utc.with_ymd_and_hms(2026, 1, 1, 0, minute, 0).unwrap()),
            id,
        }
    }

    #[test]
    fn a_cursor_round_trips_for_its_own_order() {
        let order = order("created_at", "desc", true);
        let at = shown_at(5, 42);
        assert_eq!(order.decode(SECRET, &order.encode(SECRET, at)).unwrap(), at);
    }

    #[test]
    fn a_cursor_carries_the_status_its_row_was_shown_at() {
        let order = order("status", "asc", true);
        let at = Position::Shown {
            key: SortValue::Status(SubmissionStatus::Judged),
            id: 7,
        };
        assert_eq!(order.decode(SECRET, &order.encode(SECRET, at)).unwrap(), at);
    }

    #[test]
    fn a_cursor_is_rejected_for_another_order() {
        let issued = order("created_at", "desc", true).encode(SECRET, shown_at(5, 42));
        for other in [
            order("created_at", "asc", true),
            order("status", "desc", true),
        ] {
            let err = other.decode(SECRET, &issued).unwrap_err();
            assert!(matches!(err, AppError::Validation(_)), "{err:?}");
        }
    }

    #[test]
    fn a_cursor_another_secret_signed_or_anyone_edited_is_rejected() {
        let order = order("created_at", "desc", true);
        let issued = order.encode(SECRET, shown_at(5, 42));
        assert!(matches!(
            order.decode(b"another-secret-of-at-least-32-bytes", &issued),
            Err(AppError::Validation(_))
        ));
        let (_, tag) = issued.split_once('.').unwrap();
        let forged = serde_json::to_vec(&Cursor {
            order: SortDir::Desc,
            at: shown_at(59, 42),
        })
        .unwrap();
        let forged = format!("{}.{tag}", URL_SAFE_NO_PAD.encode(forged));
        assert!(matches!(
            order.decode(SECRET, &forged),
            Err(AppError::Validation(_))
        ));
    }

    #[test]
    fn a_malformed_cursor_is_a_validation_error() {
        let order = order("created_at", "desc", true);
        for raw in ["", "not base64!", &URL_SAFE_NO_PAD.encode(b"{}"), "e30.e30"] {
            assert!(matches!(
                order.decode(SECRET, raw),
                Err(AppError::Validation(_))
            ));
        }
    }

    #[test]
    fn the_keyset_compares_the_sort_key_then_id_with_the_cursors_own_key() {
        let sql = |order: ListOrder, at: Position| {
            submission::Entity::find()
                .filter(order.after(at))
                .build(DbBackend::Postgres)
                .to_string()
        };
        let desc = sql(order("created_at", "desc", true), shown_at(5, 7));
        assert!(
            desc.contains("(submission.created_at, submission.id) < ('2026-01-01 00:05:00"),
            "{desc}"
        );
        assert!(!desc.contains("submission c"), "{desc}");
        let asc = sql(
            order("status", "asc", false),
            Position::Shown {
                key: SortValue::Status(SubmissionStatus::Judged),
                id: 7,
            },
        );
        assert!(asc.contains(", submission.id) > ('Judged', 7)"), "{asc}");
        assert!(asc.contains("f.submission_id = submission.id"), "{asc}");
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
            .filter(order("status", "desc", false).after(Position::Withheld { id: 7 }))
            .build(DbBackend::Postgres)
            .to_string();
        assert!(sql.contains("submission.id < 7"), "{sql}");
        assert!(!sql.contains("CASE"), "{sql}");
    }

    #[test]
    fn a_withheld_group_cursor_is_rejected_where_there_is_no_such_group() {
        let at = Position::Withheld { id: 3 };
        let issued = order("status", "asc", false).encode(SECRET, at);
        assert_eq!(
            order("status", "asc", false)
                .decode(SECRET, &issued)
                .unwrap(),
            at
        );
        assert!(matches!(
            order("status", "asc", true).decode(SECRET, &issued),
            Err(AppError::Validation(_))
        ));
    }
}
