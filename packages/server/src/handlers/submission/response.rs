use common::db::DatabaseConnection;
use std::collections::HashMap;

use broccoli_server_sdk::permissions as perm;
use chrono::{DateTime, Utc};
use common::SubmissionStatus;
use common::storage::BlobStore;
use sea_orm::prelude::Expr;
use sea_orm::*;

// visibility-bypass-audited: these DTO-building helpers are the row fetch
// that feeds the kernel for `handlers/submission/mod.rs`'s read handlers - every caller
// pairs the DTOs built here with `VisibilityKernel::decide`/`fetch_visible`/
// `fetch_visible_batch` (see this file's own `VisibilityContext` doc comment
// below) before anything reaches a response body. Never called standalone.
use crate::entity::{
    contest, problem, submission, submission_judgement, test_case, test_case_result, user,
};
use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::models::shared::{PageWindow, Pagination};
use crate::models::submission::*;
use crate::utils::contest::compile_output_public;
use crate::utils::judging::files_from_json;
use crate::utils::test_case_body::{
    RESPONSE_BODY_PREVIEW_BYTES, read_test_case_body_preview_with_limit,
};
use common::Verdict;

pub(super) async fn build_submission_list_items(
    db: &DatabaseConnection,
    submissions: Vec<(submission::Model, Option<user::Model>)>,
) -> Result<Vec<SubmissionListItem>, AppError> {
    use std::collections::HashMap;

    if submissions.is_empty() {
        return Ok(vec![]);
    }

    let problem_ids: Vec<i32> = submissions.iter().map(|(s, _)| s.problem_id).collect();

    let problems: HashMap<i32, problem::Model> = problem::Entity::find()
        .filter(problem::Column::Id.is_in(problem_ids))
        .all(db)
        .await?
        .into_iter()
        .map(|p| (p.id, p))
        .collect();

    let mut data = Vec::with_capacity(submissions.len());
    for (sub, user_opt) in submissions {
        let user_model = user_opt.ok_or_else(|| AppError::Internal("User not found".into()))?;
        let problem_model = problems
            .get(&sub.problem_id)
            .ok_or_else(|| AppError::Internal("Problem not found".into()))?;
        let score = submission_score_for_status(&sub.status, sub.score);

        data.push(SubmissionListItem {
            id: sub.id,
            language: sub.language,
            status: sub.status,
            verdict: sub.verdict,
            user_id: sub.user_id,
            username: user_model.username,
            problem_id: sub.problem_id,
            problem_title: problem_model.title.clone(),
            contest_id: sub.contest_id,
            contest_type: sub.contest_type,
            judge_epoch: sub.judge_epoch,
            target_worker_id: sub.target_worker_id,
            created_at: sub.created_at,
            score,
            time_used: sub.time_used,
            memory_used: sub.memory_used,
        });
    }

    Ok(data)
}

#[derive(Clone, Copy)]
pub(super) struct VisibilityContext {
    pub(super) viewer_id: i32,
    pub(super) has_view_all: bool,
}

impl VisibilityContext {
    /// Derives the field-suppression context (source/compile-output/test-IO
    /// gating in [`build_submission_response`] / [`build_judgement_response`])
    /// straight from the caller's auth, with no DB round trip.
    ///
    /// Reachability belongs solely to
    /// `VisibilityKernel::decide`/`fetch_visible` (`Resource::Submission`);
    /// this constructor gives callers a `VisibilityContext` for the
    /// orthogonal field-suppression rules without deciding reachability.
    pub(super) fn from_auth_user(auth_user: &AuthUser) -> Self {
        Self {
            viewer_id: auth_user.user_id,
            has_view_all: auth_user.has_permission(perm::SUBMISSION_VIEW_ALL),
        }
    }
}
pub(super) const RESULT_PAGE_SIZE: u64 = 20;
const RESULT_PREVIEW_CHARS: usize = 240;
const RESULT_PREVIEW_LINES: usize = 5;

#[derive(FromQueryResult)]
struct ResultRow {
    id: i32,
    test_case_id: Option<i32>,
    verdict: Verdict,
    score: f64,
    time_used: Option<i32>,
    memory_used: Option<i32>,
    stdout: Option<String>,
    stderr: Option<String>,
    checker_output: Option<String>,
    is_sample: Option<bool>,
    /// The testcase's input and answer previews, only where they are shown.
    input: Option<String>,
    expected_output: Option<String>,
    input_blob_hash: Option<String>,
    expected_output_blob_hash: Option<String>,
    /// Rows matching the filter across all pages (`COUNT(*) OVER ()`).
    total: i64,
}

fn output_preview(text: String, full: bool) -> String {
    let char_limit = if full {
        RESPONSE_BODY_PREVIEW_BYTES
    } else {
        RESULT_PREVIEW_CHARS
    };
    let line_limit = if full {
        usize::MAX
    } else {
        RESULT_PREVIEW_LINES
    };
    let mut end = 0;
    let mut lines = 1;
    for (count, (index, ch)) in text.char_indices().enumerate() {
        if count >= char_limit
            || (ch == '\n' && lines >= line_limit)
            || index + ch.len_utf8() > RESPONSE_BODY_PREVIEW_BYTES
        {
            break;
        }
        if ch == '\n' {
            lines += 1;
        }
        end = index + ch.len_utf8();
    }
    let mut preview = text[..end].to_owned();
    if end < text.len() {
        preview.push_str("\n… (truncated)");
    }
    preview
}

fn result_selection(query: &SubmissionResultQuery) -> Result<(PageWindow, Vec<i32>), AppError> {
    if query.full_output && query.result_id.is_none() {
        return Err(AppError::Validation(
            "full_output requires result_id".into(),
        ));
    }
    // A single selected result is always a one-row window.
    let per_page = if query.result_id.is_some() {
        Some(1)
    } else {
        query.per_page
    };
    let window = PageWindow::with_bounds(query.page, per_page, RESULT_PAGE_SIZE, RESULT_PAGE_SIZE)?;
    let ids = match query.test_case_ids.as_deref() {
        None => vec![],
        Some(raw) => {
            // Bound parsing as well as the SQL IN list.
            if raw.len() > 240 {
                return Err(AppError::Validation("Too many testcase IDs".into()));
            }
            let ids = raw
                .split(',')
                .map(|id| {
                    id.parse::<i32>()
                        .ok()
                        .filter(|n| *n > 0)
                        .ok_or_else(|| AppError::Validation("Invalid testcase ID".into()))
                })
                .collect::<Result<Vec<_>, _>>()?;
            if ids.len() > RESULT_PAGE_SIZE as usize {
                return Err(AppError::Validation(
                    "At most 20 testcase IDs are allowed".into(),
                ));
            }
            ids
        }
    };
    Ok((window, ids))
}

/// Only results written under their judgement's current epoch: a row a
/// requeued or superseded dispatch left behind is never shown.
fn results_of_their_judgements_epoch() -> sea_orm::sea_query::SimpleExpr {
    sea_orm::sea_query::Expr::cust(
        "test_case_result.judge_epoch = (SELECT j.judge_epoch FROM submission_judgement j \
         WHERE j.id = test_case_result.judgement_id)",
    )
}

async fn load_result_page(
    db: &DatabaseConnection,
    blob_store: &dyn BlobStore,
    submission_id: i32,
    judgement_id: Option<i32>,
    show_test_details: bool,
    query: Option<&SubmissionResultQuery>,
) -> Result<(Vec<TestCaseResultResponse>, Pagination), AppError> {
    let default_query = SubmissionResultQuery::default();
    let (window, ids) = result_selection(query.unwrap_or(&default_query))?;
    let mut select = test_case_result::Entity::find()
        .filter(test_case_result::Column::SubmissionId.eq(submission_id))
        .filter(results_of_their_judgements_epoch());
    if let Some(id) = judgement_id {
        select = select.filter(test_case_result::Column::JudgementId.eq(Some(id)));
    }
    if !ids.is_empty() {
        select = select.filter(test_case_result::Column::TestCaseId.is_in(ids));
    }
    if let Some(id) = query.and_then(|q| q.result_id) {
        select = select.filter(test_case_result::Column::Id.eq(id));
    }
    // Version history loads summaries only, without reading any testcase text.
    let Some(query) = query else {
        let total = select.count(db).await?;
        return Ok((vec![], window.pagination(total)));
    };
    let full = query.full_output;
    let cap = if full {
        RESPONSE_BODY_PREVIEW_BYTES
    } else {
        RESULT_PREVIEW_CHARS
    };
    // Rows, their testcase previews and the total in one statement.
    let shown = format!("{show_test_details} OR COALESCE(test_case.is_sample, false)");
    let mut page = select
        .clone()
        .join(
            JoinType::LeftJoin,
            test_case_result::Relation::TestCase.def(),
        )
        .select_only()
        .columns([
            test_case_result::Column::Id,
            test_case_result::Column::TestCaseId,
            test_case_result::Column::Verdict,
            test_case_result::Column::Score,
            test_case_result::Column::TimeUsed,
            test_case_result::Column::MemoryUsed,
        ])
        .column_as(test_case::Column::IsSample, "is_sample")
        .column_as(Expr::cust("COUNT(*) OVER ()"), "total");
    for (column, alias) in [
        ("test_case_result.stdout", "stdout"),
        ("test_case_result.stderr", "stderr"),
        ("test_case_result.checker_output", "checker_output"),
        ("test_case.input", "input"),
        ("test_case.expected_output", "expected_output"),
    ] {
        page = page.column_as(
            Expr::cust(format!(
                "CASE WHEN {shown} THEN LEFT({column}, {}) ELSE NULL END",
                cap + 1
            )),
            alias,
        );
    }
    for column in ["input_blob_hash", "expected_output_blob_hash"] {
        page = page.column_as(
            Expr::cust(format!(
                "CASE WHEN {shown} THEN test_case.{column} ELSE NULL END"
            )),
            column,
        );
    }
    let rows = page
        .order_by_asc(test_case::Column::Position)
        .order_by_asc(test_case_result::Column::Id)
        .offset(window.offset())
        .limit(window.limit())
        .into_model::<ResultRow>()
        .all(db)
        .await?;
    let total = match rows.first() {
        Some(row) => u64::try_from(row.total).unwrap_or(0),
        // Past the last page the window counts nothing; count separately.
        None if window.offset() > 0 => select.count(db).await?,
        None => 0,
    };
    let pagination = window.pagination(total);
    let mut io = HashMap::new();
    for row in &rows {
        let (Some(id), Some(input), Some(expected)) =
            (row.test_case_id, &row.input, &row.expected_output)
        else {
            continue;
        };
        let input = read_test_case_body_preview_with_limit(
            input,
            row.input_blob_hash.as_deref(),
            blob_store,
            cap,
        )
        .await?;
        let expected = read_test_case_body_preview_with_limit(
            expected,
            row.expected_output_blob_hash.as_deref(),
            blob_store,
            cap,
        )
        .await?;
        io.insert(
            id,
            (output_preview(input, full), output_preview(expected, full)),
        );
    }
    let results = rows
        .into_iter()
        .map(|r| {
            let show_io = show_test_details || r.is_sample == Some(true);
            let (input, expected_output) = r
                .test_case_id
                .and_then(|id| io.get(&id))
                .map(|(a, b)| (Some(a.clone()), Some(b.clone())))
                .unwrap_or_default();
            TestCaseResultResponse {
                id: r.id,
                test_case_id: r.test_case_id,
                verdict: Some(r.verdict),
                score: Some(r.score),
                time_used: r.time_used,
                memory_used: r.memory_used,
                input,
                expected_output,
                stdout: if show_io {
                    r.stdout.map(|s| output_preview(s, full))
                } else {
                    None
                },
                stderr: if show_io {
                    r.stderr.map(|s| output_preview(s, full))
                } else {
                    None
                },
                checker_output: if show_io {
                    r.checker_output.map(|s| output_preview(s, full))
                } else {
                    None
                },
            }
        })
        .collect();
    Ok((results, pagination))
}

/// What a submission's detail response shows besides the submission row:
/// its author, problem, contest and current judgement, in one statement.
struct SubmissionContext {
    username: String,
    problem_title: String,
    show_test_details: bool,
    contest: Option<contest::Model>,
    current_judgement_id: Option<i32>,
}

impl SubmissionContext {
    async fn load(db: &DatabaseConnection, sub: &submission::Model) -> Result<Self, AppError> {
        let row = db
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                r#"SELECT u.username, p.title, p.show_test_details,
                          CASE WHEN c.id IS NULL THEN NULL ELSE to_jsonb(c) END AS contest,
                          (SELECT j.id FROM submission_judgement j
                           WHERE j.submission_id = $3 AND j.is_current) AS current_judgement_id
                   FROM "user" u
                   JOIN problem p ON p.id = $2
                   LEFT JOIN contest c ON c.id = $4
                   WHERE u.id = $1"#,
                [
                    sub.user_id.into(),
                    sub.problem_id.into(),
                    sub.id.into(),
                    sub.contest_id.into(),
                ],
            ))
            .await?
            .ok_or_else(|| AppError::Internal("Submission user or problem not found".into()))?;
        let contest: Option<serde_json::Value> = row.try_get("", "contest")?;
        let contest = contest
            .map(serde_json::from_value::<contest::Model>)
            .transpose()
            .map_err(|e| AppError::Internal(format!("malformed contest row: {e}")))?;
        if sub.contest_id.is_some() && contest.is_none() {
            return Err(AppError::Internal("Contest not found".into()));
        }
        Ok(Self {
            username: row.try_get("", "username")?,
            problem_title: row.try_get("", "title")?,
            show_test_details: row.try_get("", "show_test_details")?,
            contest,
            current_judgement_id: row.try_get("", "current_judgement_id")?,
        })
    }
}

pub(super) async fn build_submission_response(
    db: &DatabaseConnection,
    blob_store: &dyn BlobStore,
    now: DateTime<Utc>,
    sub: submission::Model,
    visibility: Option<VisibilityContext>,
) -> Result<SubmissionResponse, AppError> {
    build_submission_response_with_cases(
        db,
        blob_store,
        now,
        sub,
        visibility,
        &SubmissionResultQuery::default(),
    )
    .await
}

pub(super) async fn build_submission_response_with_cases(
    db: &DatabaseConnection,
    blob_store: &dyn BlobStore,
    now: DateTime<Utc>,
    sub: submission::Model,
    visibility: Option<VisibilityContext>,
    query: &SubmissionResultQuery,
) -> Result<SubmissionResponse, AppError> {
    let context = SubmissionContext::load(db, &sub).await?;
    let contest_model = context.contest;

    let is_owner = visibility
        .as_ref()
        .is_some_and(|ctx| ctx.viewer_id == sub.user_id);
    let has_view_all = visibility.as_ref().is_some_and(|ctx| ctx.has_view_all);
    let show_source_code = has_view_all || is_owner;

    let show_compile_output =
        has_view_all || is_owner || compile_output_public(contest_model.as_ref(), now);

    let is_running = sub.status == SubmissionStatus::Running;
    let show_results = sub.status.is_terminal() || is_running;

    let result_response = if show_results {
        let current_judgement_id = context.current_judgement_id;
        let (test_case_results, pagination) = load_result_page(
            db,
            blob_store,
            sub.id,
            current_judgement_id,
            has_view_all || context.show_test_details,
            Some(query),
        )
        .await?;

        if is_running {
            Some(JudgeResultResponse {
                judgement_id: current_judgement_id,
                test_case_pagination: Some(pagination),
                verdict: None,
                score: None,
                time_used: None,
                memory_used: None,
                compile_output: None,
                error_message: None,
                judged_at: None,
                test_case_results,
            })
        } else {
            Some(JudgeResultResponse {
                judgement_id: current_judgement_id,
                test_case_pagination: Some(pagination),
                verdict: sub.verdict,
                score: submission_score_for_status(&sub.status, sub.score),
                time_used: sub.time_used,
                memory_used: sub.memory_used,
                compile_output: if show_compile_output {
                    sub.compile_output.clone()
                } else {
                    None
                },
                error_message: if show_compile_output {
                    sub.error_message.clone()
                } else {
                    None
                },
                judged_at: sub.judged_at,
                test_case_results,
            })
        }
    } else {
        None
    };

    let files = if show_source_code {
        files_from_json(&sub.files)
    } else {
        vec![]
    };

    Ok(SubmissionResponse {
        id: sub.id,
        files,
        language: sub.language,
        status: sub.status,
        user_id: sub.user_id,
        username: context.username,
        problem_id: sub.problem_id,
        problem_title: context.problem_title,
        contest_id: sub.contest_id,
        contest_type: sub.contest_type.clone(),
        judge_epoch: sub.judge_epoch,
        target_worker_id: sub.target_worker_id,
        created_at: sub.created_at,
        result: result_response,
    })
}

pub(super) async fn build_judgement_response(
    db: &DatabaseConnection,
    blob_store: &dyn BlobStore,
    judgement: submission_judgement::Model,
    show_compile_output: bool,
    show_test_details: bool,
    query: Option<&SubmissionResultQuery>,
) -> Result<SubmissionJudgementResponse, AppError> {
    let (test_case_results, pagination) = load_result_page(
        db,
        blob_store,
        judgement.submission_id,
        Some(judgement.id),
        show_test_details,
        query,
    )
    .await?;
    let score = submission_score_for_status(&judgement.status, judgement.score);

    Ok(SubmissionJudgementResponse {
        case_changes: None,
        id: judgement.id,
        submission_id: judgement.submission_id,
        version: judgement.version,
        is_current: judgement.is_current,
        is_finalized: judgement.is_finalized,
        status: judgement.status,
        verdict: judgement.verdict,
        score,
        time_used: judgement.time_used,
        memory_used: judgement.memory_used,
        compile_output: if show_compile_output {
            judgement.compile_output
        } else {
            None
        },
        error_code: if show_compile_output {
            judgement.error_code
        } else {
            None
        },
        error_message: if show_compile_output {
            judgement.error_message
        } else {
            None
        },
        judge_epoch: judgement.judge_epoch,
        target_worker_id: judgement.target_worker_id,
        created_at: judgement.created_at,
        finalized_at: judgement.finalized_at,
        test_case_pagination: Some(pagination),
        test_case_results,
    })
}

pub(super) fn submission_score_for_status(
    status: &SubmissionStatus,
    score: Option<f64>,
) -> Option<f64> {
    if status == &SubmissionStatus::Judged {
        score
    } else {
        None
    }
}

#[cfg(test)]
mod result_page_tests {
    use super::*;

    #[test]
    fn short_lines_and_unicode_are_bounded() {
        let preview = output_preview("x\n".repeat(10_000), false);
        assert_eq!(preview, "x\nx\nx\nx\nx\n… (truncated)");
        let preview = output_preview("界".repeat(10_000), false);
        assert_eq!(
            preview.chars().take_while(|c| *c != '\n').count(),
            RESULT_PREVIEW_CHARS
        );
        let expanded = output_preview("界".repeat(100_000), true);
        assert!(expanded.len() <= RESPONSE_BODY_PREVIEW_BYTES + 20);
        assert!(expanded.ends_with("… (truncated)"));
        assert_eq!(output_preview("short\ntext".into(), false), "short\ntext");
    }

    #[test]
    fn result_queries_cannot_remove_the_bounds() {
        let query = SubmissionResultQuery {
            per_page: Some(u64::MAX),
            ..Default::default()
        };
        assert_eq!(
            result_selection(&query).unwrap().0.limit(),
            RESULT_PAGE_SIZE
        );
        let query = SubmissionResultQuery {
            page: Some(u64::MAX),
            ..Default::default()
        };
        assert!(result_selection(&query).is_err());
        let query = SubmissionResultQuery {
            full_output: true,
            ..Default::default()
        };
        assert!(result_selection(&query).is_err());
        let query = SubmissionResultQuery {
            full_output: true,
            result_id: Some(1),
            per_page: Some(1000),
            ..Default::default()
        };
        assert_eq!(result_selection(&query).unwrap().0.limit(), 1);
        let query = SubmissionResultQuery {
            test_case_ids: Some(
                (1..=21)
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
            ),
            ..Default::default()
        };
        assert!(result_selection(&query).is_err());
    }
}
