use common::SubmissionStatus;

use crate::entity::{problem, submission, user};
use crate::error::AppError;
use crate::models::submission::*;
use crate::utils::judging::files_from_json;
use crate::visibility::{Action, Resource, VisibilityKernel};

use super::response::{VisibilityContext, submission_score_for_status};

/// Applies the kernel's `Resource::Submission` decision - host reachability
/// `meet`-ed with any registered visibility plugin's answer, host always
/// wins on `Deny` - to an already-built `SubmissionResponse`, returning the
/// masked wire JSON.
///
/// Before this task, this file's `filter_submission_via_plugin` adopted a
/// plugin-authored submission JSON wholesale after only a shape check -
/// `Ok(out) => Ok(out.submission)`. A plugin registered against the
/// submission's contest type could therefore alter a verdict, a score, or a
/// displayed user id outright. Routing through `VisibilityKernel::fetch_visible`
/// closes that structurally rather than by convention: `Visible::new` is
/// `pub(super)` to `crate::visibility`, so the only way anything under
/// `handlers` can turn a DTO into a response body is `Visible::into_masked_json`,
/// which can only ever blank fields named in a `FieldMask` - never substitute a
/// value. `None` (the kernel's `Deny`) maps to the same 404 this submission
/// surface has always returned for an unreachable submission; the caller (not
/// this function) also fails fast on that same decision before doing the
/// (multi-table) work of building `response` in the first place - see
/// `get_submission`.
///
/// Returns raw JSON, not a re-parsed `SubmissionResponse`: a plugin (or the
/// host) can name any field path, including ones like `username` that aren't
/// `Option` on the DTO, so a masked value must be allowed to ship as JSON
/// `null` without first surviving a round trip back through `serde`.
pub(super) async fn apply_filter_to_response(
    kernel: &VisibilityKernel<'_>,
    response: SubmissionResponse,
) -> Result<serde_json::Value, AppError> {
    let resource = Resource::Submission(response.id);
    kernel
        .fetch_visible(Action::Read, resource, response)
        .await?
        .ok_or_else(|| AppError::NotFound("Submission not found".into()))?
        .into_masked_json()
}

/// Judgement-history masking.
///
/// `SubmissionJudgementResponse` is a flat shape (`verdict`, `score`, ...
/// directly on the object), not the nested `result.verdict` shape a
/// `FieldMask` targets (see `Decision`'s own doc examples and
/// `apply_mask`'s tests). Exactly as before this task, a synthetic
/// `SubmissionResponse` wrapper stands in for the judgement so the masking
/// (now kernel-driven instead of plugin-driven) lands on the paths a caller
/// actually names, and the masked fields are copied back onto the real
/// judgement response afterwards. The kernel memoizes per `(Action,
/// Resource)`, so this doesn't re-decide anything already decided for this
/// submission earlier in the request (e.g. by `list_submission_judgements`'s
/// own upfront reachability check) - it's a second lookup into the same
/// memo, not a second host/plugin round trip.
pub(super) async fn apply_filter_to_judgement_response(
    kernel: &VisibilityKernel<'_>,
    sub: &submission::Model,
    user_model: &user::Model,
    problem_model: &problem::Model,
    mut response: SubmissionJudgementResponse,
    visibility: &VisibilityContext,
) -> Result<SubmissionJudgementResponse, AppError> {
    let result_response =
        if response.status.is_terminal() || response.status == SubmissionStatus::Running {
            Some(JudgeResultResponse {
                verdict: response.verdict,
                score: submission_score_for_status(&response.status, response.score),
                time_used: response.time_used,
                memory_used: response.memory_used,
                compile_output: response.compile_output.clone(),
                error_message: response.error_message.clone(),
                judged_at: response.finalized_at,
                test_case_results: response.test_case_results.clone(),
            })
        } else {
            None
        };

    let synthetic_submission = SubmissionResponse {
        id: sub.id,
        files: if visibility.has_view_all || visibility.viewer_id == sub.user_id {
            files_from_json(&sub.files)
        } else {
            vec![]
        },
        language: sub.language.clone(),
        status: response.status.clone(),
        user_id: sub.user_id,
        username: user_model.username.clone(),
        problem_id: sub.problem_id,
        problem_title: problem_model.title.clone(),
        contest_id: sub.contest_id,
        contest_type: sub.contest_type.clone(),
        judge_epoch: response.judge_epoch,
        target_worker_id: response.target_worker_id.clone(),
        created_at: sub.created_at,
        result: result_response,
    };

    let filtered_value = apply_filter_to_response(kernel, synthetic_submission).await?;
    // Unlike `get_submission` (which ships `apply_filter_to_response`'s Value
    // straight out as the body), this needs the masked result fields back as
    // typed data to copy onto `response` below - same round trip
    // `filter_submission_via_plugin`'s caller did before this task, same
    // pre-existing caveat that a mask naming a non-`Option` field here (there
    // are none among `result.*`) would fail this deserialize.
    let filtered_submission: SubmissionResponse =
        serde_json::from_value(filtered_value).map_err(|e| {
            AppError::Internal(format!("Failed to deserialize masked submission: {e}"))
        })?;

    match filtered_submission.result {
        Some(result) => {
            response.verdict = result.verdict;
            response.score = submission_score_for_status(&response.status, result.score);
            response.time_used = result.time_used;
            response.memory_used = result.memory_used;
            response.compile_output = result.compile_output;
            response.error_message = result.error_message;
            response.finalized_at = result.judged_at;
            response.test_case_results = result.test_case_results;
            if response.compile_output.is_none() && response.error_message.is_none() {
                response.error_code = None;
            }
        }
        None => {
            response.verdict = None;
            response.score = None;
            response.time_used = None;
            response.memory_used = None;
            response.compile_output = None;
            response.error_code = None;
            response.error_message = None;
            response.test_case_results.clear();
        }
    }

    Ok(response)
}

/// List masking: one `decide_batch` (via `fetch_visible_batch`) over every
/// listed submission's own `Resource::Submission(id)`. `decide_batch` dedupes
/// and resolves each submission's own `contest_id` internally via
/// `host_decide`'s `submission_contest_ids` out-param, so a single global
/// page spanning several contests (and contest-less submissions) still gets a
/// per-submission-correct decision with no extra plumbing here - see
/// `visibility::resource_contest_id`.
///
/// A denied row is omitted outright: `fetch_visible_batch` returns `None` for
/// it, and `.flatten()` below drops it - never rendered as a placeholder,
/// which would itself leak that the row exists. This is a real (and
/// intended) behavioural change from the plugin-based mechanism it replaces:
/// `FilterSubmissionOutput` had no way to say "omit this item", only to
/// rewrite one - a list page can now legitimately come back shorter than
/// `per_page` even though `total`/`total_pages` are computed from the
/// pre-decision SQL count, exactly like every other kernel-backed list
/// endpoint (e.g. `handlers::attachment::list_attachments`).
pub(super) async fn apply_filter_to_list(
    kernel: &VisibilityKernel<'_>,
    items: Vec<SubmissionListItem>,
) -> Result<Vec<serde_json::Value>, AppError> {
    let pairs: Vec<(Resource, SubmissionListItem)> = items
        .into_iter()
        .map(|item| (Resource::Submission(item.id), item))
        .collect();

    let visible = kernel.fetch_visible_batch(Action::Read, pairs).await?;
    visible
        .into_iter()
        .flatten()
        .map(|v| v.into_masked_json())
        .collect()
}
