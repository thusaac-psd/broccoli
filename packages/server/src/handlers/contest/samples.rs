use axum::Json;
use axum::extract::State;
use sea_orm::*;
use serde::Serialize;
use tracing::instrument;

use crate::entity::test_case;
use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::AuthUser;
use crate::extractors::path::AppPath;
use crate::state::AppState;
use crate::utils::contest::{find_contest, require_contest_started};
use crate::utils::test_case_body::read_test_case_body_preview;
use crate::visibility::{Action, Resource, Subject, VisibilityKernel};

#[derive(Serialize, utoipa::ToSchema)]
pub struct ProblemSamplesResponse {
    pub samples: Vec<SampleCase>,
}
#[derive(Serialize, utoipa::ToSchema)]
pub struct SampleCase {
    pub input: String,
    pub output: String,
    /// Optional markdown note explaining this sample (author-written).
    pub description: Option<String>,
}
#[instrument(skip(state, auth_user), fields(contest_id, problem_id))]
#[utoipa::path(
    get,
    path = "/{problem_id}/samples",
    tag = "Contest Problems",
    operation_id = "getContestProblemSamples",
    summary = "Get sample test cases for a contest problem",
    description = "Returns sample test cases (input and output) for a problem within a contest. Requires the user to be a participant or have contest:manage permission. The contest must have started.",
    params(
        ("id" = i32, Path, description = "Contest ID"),
        ("problem_id" = i32, Path, description = "Problem ID"),
    ),
    responses(
        (status = 200, description = "Sample test cases", body = ProblemSamplesResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Contest or problem not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
pub async fn get_contest_problem_samples(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath((contest_id, problem_id)): AppPath<(i32, i32)>,
) -> Result<Json<serde_json::Value>, AppError> {
    // The kernel OWNS the subject: one kernel per request per subject.
    let kernel = VisibilityKernel::new(&state, Subject::from_auth_user(&auth_user));
    let resource = Resource::Sample {
        contest_id: Some(contest_id),
        problem_id,
    };

    // Fail fast, before doing any (bounded but non-trivial) blob reads below,
    // on a resource the kernel already knows is unreachable. This single
    // Sample decision folds in what the pre-kernel handler checked as three
    // separate steps: `check_contest_access`, `require_contest_started`'s
    // window predicate, and `find_contest_problem` (the problem must be
    // attached to this contest) - see `visibility::host_rules::decide_problem_or_sample`.
    if kernel.decide(Action::Read, resource.clone()).await?.is_denied() {
        return Err(AppError::NotFound("Contest not found".into()));
    }

    // NOT covered by the kernel decision above: `require_contest_started`'s
    // `now < start_time` check is a 400 business-rule rejection, not a
    // reachability outcome, so it has no representation in `Decision` and is
    // re-applied here, on top of the kernel's `Allow`.
    let contest_model = find_contest(&state.db, contest_id).await?;
    require_contest_started(&auth_user, &contest_model)?;

    let sample_test_cases = test_case::Entity::find()
        .filter(test_case::Column::ProblemId.eq(problem_id))
        .filter(test_case::Column::IsSample.eq(true))
        .order_by_asc(test_case::Column::Position)
        .all(&state.db)
        .await?;

    // Bounded reads: a setter can mark an arbitrarily large test case as a
    // sample, and this contestant-facing endpoint is polled by every
    // participant. Reading the full body (up to the ~1 GB body limit) per sample
    // per caller would OOM the server. A legitimate sample is tiny and well under
    // the preview cap, so it is still served in full; only an abusive oversized
    // sample is truncated, and peak memory stays bounded regardless of size or
    // request concurrency.
    let mut samples = Vec::with_capacity(sample_test_cases.len());
    for tc in sample_test_cases {
        let input = read_test_case_body_preview(
            &tc.input,
            tc.input_blob_hash.as_deref(),
            &*state.blob_store,
        )
        .await?;
        let output = read_test_case_body_preview(
            &tc.expected_output,
            tc.expected_output_blob_hash.as_deref(),
            &*state.blob_store,
        )
        .await?;
        samples.push(SampleCase {
            input,
            output,
            description: tc.description,
        });
    }

    // The DTO reaches the response body only through `into_masked_json`, so a
    // `Redact` decision on this Sample resource can never be forgotten at the
    // serialization step. The kernel memoizes per (Action, Resource), so this
    // re-decides the same `resource` already `Allow`ed above at no extra DB
    // cost, and the `None` arm is unreachable in practice (it was already
    // `Allow`, not `Deny`) but kept honest rather than `.unwrap()`-ed away.
    let visible = kernel
        .fetch_visible(Action::Read, resource, ProblemSamplesResponse { samples })
        .await?
        .ok_or_else(|| AppError::NotFound("Contest not found".into()))?;

    Ok(Json(visible.into_masked_json()?))
}
