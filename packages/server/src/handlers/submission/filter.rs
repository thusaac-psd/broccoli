use common::SubmissionStatus;

// visibility-bypass-audited: this module fetches nothing - `problem`,
// `submission`, and `user` are only used as parameter/field types for rows
// its callers already fetched. Every function here is the per-row masking
// step applied *after* `VisibilityKernel::decide`/`fetch_visible_batch`
// (see the module doc comment below and `handlers/submission/mod.rs`'s
// `list_contest_submissions`), never a substitute for it.
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
    response: SubmissionJudgementResponse,
    visibility: &VisibilityContext,
) -> Result<serde_json::Value, AppError> {
    let result_response =
        if response.status.is_terminal() || response.status == SubmissionStatus::Running {
            Some(JudgeResultResponse {
                verdict: response.verdict.clone(),
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

    // DO NOT re-deserialize `filtered_value` into a typed `SubmissionResponse`
    // here, even though that used to be exactly what this function did (and
    // is still what `filter_submission_via_plugin`'s pre-task caller did). A
    // `FieldMask` can only ever blank a value to JSON `null`, never author a
    // replacement - so `per_test_case_mask_fields()` (used by
    // `subtask_scores`/`total_only`) nulls individual
    // `result.test_case_results[i].verdict`/`.score` leaves while leaving
    // their surrounding array elements in place. `TestCaseResultResponse`
    // declares `verdict: Verdict` and `score: f64` as non-`Option`, so a
    // masked `null` inside a populated array fails `serde_json::from_value`
    // outright, turning a legitimate redaction into a 500
    // (`AppError::Internal`) instead of the null the client is supposed to
    // see. The `none` level's own e2e coverage never caught this because it
    // masks the whole array key to `[]`, not individual leaves within it -
    // see `ioi_feedback_filter_subtask_scores_redacts_per_test_case_verdict`.
    //
    // The fix mirrors what `apply_filter_to_response` / `get_submission` and
    // `Visible::into_masked_json` already do: serialize-then-mask, and once
    // masked, stay in `serde_json::Value` land all the way to the wire -
    // never re-type masked JSON. Concretely: serialize the still-UNMASKED
    // `response` (safe - masking hasn't touched it) to a `Value`, then splice
    // the already-masked `result.*` leaves from `filtered_value` onto it
    // directly as JSON, reading them with `Value` indexing rather than
    // through a struct. If you're tempted to "simplify" this back into a
    // typed round trip, don't - that reintroduces this exact crash.
    // Task 17 / Step 2z: the `obj.insert(...)` calls below assume the eight
    // leaves named here are the WHOLE of `JudgeResultResponse` - i.e. that
    // splicing them one by one out of `masked_result` is equivalent to
    // splicing the whole `result` object. That is true today, but it is a
    // whitelist, not a projection: if `JudgeResultResponse` ever grows a
    // ninth field, this splice silently keeps shipping the field from the
    // UNMASKED `response` serialization below (`obj` already has it, and
    // nothing here overwrites it) even if a future plugin's `FieldMask`
    // targets it. That is a leak, not a crash, so nothing here would fail
    // loudly - see `_assert_judge_result_response_fields_are_exhaustively_spliced`
    // just below, which turns that silent leak into a compile error by
    // naming every field of `JudgeResultResponse` with no `..` catch-all:
    // adding a field there without touching this function no longer compiles.
    let masked_result = filtered_value
        .get("result")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    let mut value = serde_json::to_value(&response)
        .map_err(|e| AppError::Internal(format!("Failed to serialize judgement response: {e}")))?;
    let obj = value.as_object_mut().ok_or_else(|| {
        AppError::Internal("Judgement response did not serialize to a JSON object".into())
    })?;

    if masked_result.is_null() {
        obj.insert("verdict".into(), serde_json::Value::Null);
        obj.insert("score".into(), serde_json::Value::Null);
        obj.insert("time_used".into(), serde_json::Value::Null);
        obj.insert("memory_used".into(), serde_json::Value::Null);
        obj.insert("compile_output".into(), serde_json::Value::Null);
        obj.insert("error_code".into(), serde_json::Value::Null);
        obj.insert("error_message".into(), serde_json::Value::Null);
        obj.insert(
            "test_case_results".into(),
            serde_json::Value::Array(Vec::new()),
        );
    } else {
        obj.insert("verdict".into(), masked_result["verdict"].clone());
        obj.insert("score".into(), masked_result["score"].clone());
        obj.insert("time_used".into(), masked_result["time_used"].clone());
        obj.insert("memory_used".into(), masked_result["memory_used"].clone());
        obj.insert(
            "compile_output".into(),
            masked_result["compile_output"].clone(),
        );
        obj.insert(
            "error_message".into(),
            masked_result["error_message"].clone(),
        );
        obj.insert("finalized_at".into(), masked_result["judged_at"].clone());
        obj.insert(
            "test_case_results".into(),
            masked_result["test_case_results"].clone(),
        );
        if masked_result["compile_output"].is_null() && masked_result["error_message"].is_null() {
            obj.insert("error_code".into(), serde_json::Value::Null);
        }
    }

    Ok(value)
}

/// Compile-time exhaustiveness guard for the splice in
/// [`apply_filter_to_judgement_response`]. This function is never called -
/// its only purpose is the destructuring pattern below, which names every
/// field of [`JudgeResultResponse`] explicitly with no `..` catch-all. If a
/// field is ever added to (or removed from) `JudgeResultResponse` without
/// this pattern being updated to match, `rustc` rejects the mismatch with
/// "pattern does not mention field `<name>`" (E0027) rather than letting the
/// splice above silently keep whitelisting only the original eight leaves.
/// Keep this pattern in exact 1:1 correspondence with the `obj.insert(...)`
/// calls in both branches of `apply_filter_to_judgement_response` - when you
/// add a field here, add the matching `obj.insert` (both the `is_null()` and
/// the populated branch) at the same time.
#[allow(dead_code)]
fn _assert_judge_result_response_fields_are_exhaustively_spliced(r: JudgeResultResponse) {
    let JudgeResultResponse {
        verdict: _,
        score: _,
        time_used: _,
        memory_used: _,
        compile_output: _,
        error_message: _,
        judged_at: _,
        test_case_results: _,
    } = r;
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
