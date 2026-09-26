use std::collections::HashMap;

use broccoli_server_sdk::permissions as perm;
use chrono::Utc;
use common::SubmissionStatus;
use common::storage::BlobStore;
use sea_orm::*;

// visibility-bypass-audited: these DTO-building helpers are the pre-kernel
// row fetch for `handlers/submission/mod.rs`'s read handlers - every caller
// pairs the DTOs built here with `VisibilityKernel::decide`/`fetch_visible`/
// `fetch_visible_batch` (see this file's own `VisibilityContext` doc comment
// below) before anything reaches a response body. Never called standalone.
use crate::entity::{
    contest, problem, submission, submission_judgement, test_case, test_case_result, user,
};
use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::models::submission::*;
use crate::utils::judging::files_from_json;
use crate::utils::test_case_body::read_test_case_body_preview;

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
    /// Before this task this only ever came out of `require_submission_visible`
    /// as a side effect of it *also* deciding reachability by hand. That
    /// reachability decision now belongs solely to
    /// `VisibilityKernel::decide`/`fetch_visible` (`Resource::Submission`) -
    /// this constructor exists so callers can still get a `VisibilityContext`
    /// for the orthogonal field-suppression rules without resurrecting that
    /// hand-rolled check.
    pub(super) fn from_auth_user(auth_user: &AuthUser) -> Self {
        Self {
            viewer_id: auth_user.user_id,
            has_view_all: auth_user.has_permission(perm::SUBMISSION_VIEW_ALL),
        }
    }
}

#[derive(FromQueryResult)]
struct TestCaseMeta {
    id: i32,
    is_sample: bool,
    position: i32,
}

#[derive(FromQueryResult)]
struct TestCaseIoData {
    id: i32,
    input: String,
    expected_output: String,
    input_blob_hash: Option<String>,
    expected_output_blob_hash: Option<String>,
}

#[derive(Clone)]
struct MaterializedTestCaseIoData {
    input: String,
    expected_output: String,
}

async fn load_test_case_io_data(
    db: &DatabaseConnection,
    io_ids: Vec<i32>,
    blob_store: &dyn BlobStore,
) -> Result<HashMap<i32, MaterializedTestCaseIoData>, AppError> {
    if io_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let rows = test_case::Entity::find()
        .filter(test_case::Column::Id.is_in(io_ids))
        .select_only()
        .column(test_case::Column::Id)
        .column(test_case::Column::Input)
        .column(test_case::Column::ExpectedOutput)
        .column(test_case::Column::InputBlobHash)
        .column(test_case::Column::ExpectedOutputBlobHash)
        .into_model::<TestCaseIoData>()
        .all(db)
        .await?;

    let mut out = HashMap::with_capacity(rows.len());
    for row in rows {
        // Bounded preview, not the full blob: a status poll must never pull tens
        // of MB of test data per test case into memory (that serializes into a
        // multi-GB response and OOM-kills the server under concurrent polling).
        let input =
            read_test_case_body_preview(&row.input, row.input_blob_hash.as_deref(), blob_store)
                .await?;
        let expected_output = read_test_case_body_preview(
            &row.expected_output,
            row.expected_output_blob_hash.as_deref(),
            blob_store,
        )
        .await?;
        out.insert(
            row.id,
            MaterializedTestCaseIoData {
                input,
                expected_output,
            },
        );
    }

    Ok(out)
}

pub(super) async fn build_submission_response(
    db: &DatabaseConnection,
    blob_store: &dyn BlobStore,
    sub: submission::Model,
    visibility: Option<VisibilityContext>,
) -> Result<SubmissionResponse, AppError> {
    let user_model = user::Entity::find_by_id(sub.user_id)
        .one(db)
        .await?
        .ok_or_else(|| AppError::Internal("Submission user not found".into()))?;

    let problem_model = problem::Entity::find_by_id(sub.problem_id)
        .one(db)
        .await?
        .ok_or_else(|| AppError::Internal("Submission problem not found".into()))?;

    let contest_model = if let Some(contest_id) = sub.contest_id {
        Some(
            contest::Entity::find_by_id(contest_id)
                .one(db)
                .await?
                .ok_or_else(|| AppError::Internal("Contest not found".into()))?,
        )
    } else {
        None
    };

    let is_owner = visibility
        .as_ref()
        .is_some_and(|ctx| ctx.viewer_id == sub.user_id);
    let has_view_all = visibility.as_ref().is_some_and(|ctx| ctx.has_view_all);
    let contest_ended = contest_model
        .as_ref()
        .is_none_or(|c| Utc::now() > c.end_time);

    let show_source_code = has_view_all || is_owner;

    let show_compile_output = has_view_all
        || is_owner
        || contest_ended
        || contest_model
            .as_ref()
            .is_some_and(|c| c.show_compile_output);

    let is_running = sub.status == SubmissionStatus::Running;
    let show_results = sub.status.is_terminal() || is_running;

    let result_response = if show_results {
        let current_judgement_id = submission_judgement::Entity::find()
            .filter(submission_judgement::Column::SubmissionId.eq(sub.id))
            .filter(submission_judgement::Column::IsCurrent.eq(true))
            .one(db)
            .await?
            .map(|j| j.id);
        let mut results_query = test_case_result::Entity::find()
            .filter(test_case_result::Column::SubmissionId.eq(sub.id));
        if let Some(judgement_id) = current_judgement_id {
            results_query =
                results_query.filter(test_case_result::Column::JudgementId.eq(Some(judgement_id)));
        }
        let results = results_query.all(db).await?;

        let tc_ids: Vec<i32> = results.iter().filter_map(|r| r.test_case_id).collect();
        let tc_meta: HashMap<i32, TestCaseMeta> = if tc_ids.is_empty() {
            HashMap::new()
        } else {
            test_case::Entity::find()
                .filter(test_case::Column::Id.is_in(tc_ids.clone()))
                .select_only()
                .column(test_case::Column::Id)
                .column(test_case::Column::IsSample)
                .column(test_case::Column::Position)
                .into_model::<TestCaseMeta>()
                .all(db)
                .await?
                .into_iter()
                .map(|tc| (tc.id, tc))
                .collect()
        };

        let mut results_with_pos: Vec<_> = results
            .into_iter()
            .map(|r| {
                let pos = r
                    .test_case_id
                    .and_then(|tc_id| tc_meta.get(&tc_id))
                    .map_or(i32::MAX, |m| m.position);
                (r, pos)
            })
            .collect();
        results_with_pos.sort_by_key(|(_, pos)| *pos);

        let io_ids: Vec<i32> = if has_view_all || problem_model.show_test_details {
            tc_ids
        } else {
            tc_meta
                .values()
                .filter(|m| m.is_sample)
                .map(|m| m.id)
                .collect()
        };
        let io_data = load_test_case_io_data(db, io_ids, blob_store).await?;

        let test_case_results = results_with_pos
            .into_iter()
            .map(|(result, _)| {
                let is_sample = result
                    .test_case_id
                    .and_then(|tc_id| tc_meta.get(&tc_id))
                    .is_some_and(|m| m.is_sample);
                let show_io = has_view_all || problem_model.show_test_details || is_sample;

                let (tc_input, tc_expected) = if show_io {
                    let io = result.test_case_id.and_then(|tc_id| io_data.get(&tc_id));
                    (
                        io.map(|d| d.input.clone()),
                        io.map(|d| d.expected_output.clone()),
                    )
                } else {
                    (None, None)
                };

                TestCaseResultResponse {
                    id: result.id,
                    verdict: Some(result.verdict),
                    score: Some(result.score),
                    time_used: result.time_used,
                    memory_used: result.memory_used,
                    test_case_id: result.test_case_id,
                    input: tc_input,
                    expected_output: tc_expected,
                    stdout: if show_io { result.stdout } else { None },
                    stderr: if show_io { result.stderr } else { None },
                    checker_output: if show_io { result.checker_output } else { None },
                }
            })
            .collect();

        if is_running {
            Some(JudgeResultResponse {
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
        username: user_model.username,
        problem_id: sub.problem_id,
        problem_title: problem_model.title,
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
) -> Result<SubmissionJudgementResponse, AppError> {
    let results = test_case_result::Entity::find()
        .filter(test_case_result::Column::JudgementId.eq(Some(judgement.id)))
        .all(db)
        .await?;

    let tc_ids: Vec<i32> = results.iter().filter_map(|r| r.test_case_id).collect();
    let tc_meta: HashMap<i32, TestCaseMeta> = if tc_ids.is_empty() {
        HashMap::new()
    } else {
        test_case::Entity::find()
            .filter(test_case::Column::Id.is_in(tc_ids.clone()))
            .select_only()
            .column(test_case::Column::Id)
            .column(test_case::Column::IsSample)
            .column(test_case::Column::Position)
            .into_model::<TestCaseMeta>()
            .all(db)
            .await?
            .into_iter()
            .map(|tc| (tc.id, tc))
            .collect()
    };

    let mut results_with_pos: Vec<_> = results
        .into_iter()
        .map(|r| {
            let pos = r
                .test_case_id
                .and_then(|tc_id| tc_meta.get(&tc_id))
                .map_or(i32::MAX, |m| m.position);
            (r, pos)
        })
        .collect();
    results_with_pos.sort_by_key(|(_, pos)| *pos);

    let io_ids: Vec<i32> = if show_test_details {
        tc_ids
    } else {
        tc_meta
            .values()
            .filter(|m| m.is_sample)
            .map(|m| m.id)
            .collect()
    };
    let io_data = load_test_case_io_data(db, io_ids, blob_store).await?;

    let test_case_results = results_with_pos
        .into_iter()
        .map(|(result, _)| {
            let is_sample = result
                .test_case_id
                .and_then(|tc_id| tc_meta.get(&tc_id))
                .is_some_and(|m| m.is_sample);
            let show_io = show_test_details || is_sample;

            let (tc_input, tc_expected) = if show_io {
                let io = result.test_case_id.and_then(|tc_id| io_data.get(&tc_id));
                (
                    io.map(|d| d.input.clone()),
                    io.map(|d| d.expected_output.clone()),
                )
            } else {
                (None, None)
            };

            TestCaseResultResponse {
                id: result.id,
                verdict: Some(result.verdict),
                score: Some(result.score),
                time_used: result.time_used,
                memory_used: result.memory_used,
                test_case_id: result.test_case_id,
                input: tc_input,
                expected_output: tc_expected,
                stdout: if show_io { result.stdout } else { None },
                stderr: if show_io { result.stderr } else { None },
                checker_output: if show_io { result.checker_output } else { None },
            }
        })
        .collect();
    let score = submission_score_for_status(&judgement.status, judgement.score);

    Ok(SubmissionJudgementResponse {
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
