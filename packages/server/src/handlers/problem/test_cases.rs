use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use broccoli_server_sdk::permissions as perm;
use sea_orm::prelude::Expr;
use sea_orm::*;
use tracing::instrument;

// visibility-bypass-audited: every write here (create/update/delete/reorder/
// bulk_delete) requires perm::PROBLEM_EDIT, asserted at its handler's entry
// and pinned by tests/integration/problem.rs's `contestant_cannot_reorder_test_cases`
// and `contestant_cannot_bulk_delete_test_cases`. The viewer-facing reads,
// `get_test_case` and `get_contest_problem_sample`, gate reachability through
// `VisibilityKernel`'s `Resource::Problem` decision (see `get_test_case_in`),
// pinned by `contestant_can_access_sample_test_case_via_active_contest`
// and `contestant_cannot_access_non_sample_test_case_via_active_contest`.
use crate::entity::{problem, test_case, test_case_result};
use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::{AuthUser, Authorized, FreshAuthUser, ProblemEdit};
use crate::extractors::json::AppJson;
use crate::extractors::path::AppPath;
use crate::models::problem::*;
use crate::state::AppState;
use crate::upload_limits::LARGE_UPLOAD_LIMIT_BYTES;
use crate::utils::problem::find_problem;
use crate::utils::soft_delete::find_active_child;
use crate::utils::test_case_body::{
    prepare_test_case_body, read_test_case_body, test_case_body_preview, test_case_body_size,
};
use crate::visibility::{Action, ProblemScope, Subject, VisibilityKernel};

use super::{find_problem_for_update, require_problem_part_reachable};

#[utoipa::path(
    post,
    path = "/",
    tag = "Test Cases",
    operation_id = "createTestCase",
    summary = "Create a test case for a problem",
    description = "Creates a new test case under the specified problem. Requires `problem:edit` permission. Position is auto-assigned if omitted. Input and expected_output may be empty for output-only or custom-checker problems. Body limit: 1 GB.",
    params(("id" = i32, Path, description = "Problem ID")),
    request_body = CreateTestCaseRequest,
    responses(
        (status = 201, description = "Test case created", body = TestCaseResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 413, description = "Request body over the endpoint's limit (PAYLOAD_TOO_LARGE)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Problem not found (NOT_FOUND)", body = ErrorBody),
        (status = 409, description = "Duplicate label in problem (CONFLICT)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(_auth, state, payload), fields(problem_id))]
pub async fn create_test_case(
    _auth: Authorized<ProblemEdit>,
    State(state): State<AppState>,
    AppPath(problem_id): AppPath<i32>,
    AppJson(payload): AppJson<CreateTestCaseRequest>,
) -> Result<impl IntoResponse, AppError> {
    let txn = state.db.begin().await?;
    find_problem_for_update(&txn, problem_id).await?;
    validate_create_test_case(&payload)?;

    let position = match payload.position {
        Some(p) => p,
        None => next_test_case_position(&txn, problem_id).await?,
    };

    let label = match payload.label.as_deref() {
        Some(label) => {
            let label = label.trim().to_string();
            ensure_test_case_label_available(&txn, problem_id, &label, None).await?;
            label
        }
        None => first_free_test_case_label(&txn, problem_id, position).await?,
    };
    let input_body = prepare_test_case_body(payload.input, state.blob_store.clone()).await?;
    let output_body =
        prepare_test_case_body(payload.expected_output, state.blob_store.clone()).await?;
    let new_tc = test_case::ActiveModel {
        input: Set(input_body.inline_text),
        expected_output: Set(output_body.inline_text),
        input_blob_hash: Set(input_body.blob_hash),
        expected_output_blob_hash: Set(output_body.blob_hash),
        input_size: Set(Some(input_body.size)),
        expected_output_size: Set(Some(output_body.size)),
        input_preview: Set(Some(input_body.preview)),
        expected_output_preview: Set(Some(output_body.preview)),
        score: Set(payload.score),
        description: Set(payload.description.map(|d| d.trim().to_string())),
        label: Set(label),
        is_sample: Set(payload.is_sample),
        position: Set(position),
        problem_id: Set(problem_id),
        created_at: Set(state.clock.now()),
        ..Default::default()
    };

    let model = new_tc.insert(&txn).await?;
    txn.commit().await?;

    Ok((
        StatusCode::CREATED,
        Json(test_case_response_from_model(model, &*state.blob_store).await?),
    ))
}

#[utoipa::path(
    get,
    path = "/",
    tag = "Test Cases",
    operation_id = "listTestCases",
    summary = "List test cases for a problem",
    description = "Returns all test cases for a problem, ordered by position. Requires `problem:create` or `problem:edit` permission. Input and output are truncated to 100-character previews.",
    params(("id" = i32, Path, description = "Problem ID")),
    responses(
        (status = 200, description = "List of test cases", body = Vec<TestCaseListItem>),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Problem not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(problem_id))]
pub async fn list_test_cases(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(problem_id): AppPath<i32>,
) -> Result<Json<Vec<TestCaseListItem>>, AppError> {
    auth_user.require_any_permission(&[perm::PROBLEM_CREATE, perm::PROBLEM_EDIT])?;

    find_problem(&state.db, problem_id).await?;

    let preview_end_index = PREVIEW_LENGTH + 1;

    let rows = test_case::Entity::find()
        .filter(test_case::Column::ProblemId.eq(problem_id))
        .select_only()
        .column(test_case::Column::Id)
        .column(test_case::Column::Score)
        .column(test_case::Column::Label)
        .column(test_case::Column::Description)
        .column(test_case::Column::IsSample)
        .column(test_case::Column::Position)
        .column_as(
            Expr::cust(format!(
                "COALESCE(\"input_preview\", left(\"input\", {preview_end_index}))"
            )),
            "input_preview",
        )
        .column_as(
            Expr::cust(format!(
                "COALESCE(\"expected_output_preview\", left(\"expected_output\", {preview_end_index}))"
            )),
            "output_preview",
        )
        .column(test_case::Column::ProblemId)
        .column(test_case::Column::CreatedAt)
        .order_by_asc(test_case::Column::Position)
        .into_model::<TestCaseListItem>()
        .all(&state.db)
        .await?;

    let items: Vec<TestCaseListItem> = rows
        .into_iter()
        .map(|mut r| {
            r.input_preview = truncate_preview(&r.input_preview);
            r.output_preview = truncate_preview(&r.output_preview);
            r
        })
        .collect();

    Ok(Json(items))
}

#[utoipa::path(
    get,
    path = "/{tc_id}",
    tag = "Test Cases",
    operation_id = "getTestCase",
    summary = "Get a test case by ID",
    description = "Returns the full details of a test case, including complete input and expected_output. Users with `problem:create`/`problem:edit` can access all test cases; contestants can access sample (`is_sample = true`) test cases for problems they can read. The test case must belong to the specified problem.",
    params(
        ("id" = i32, Path, description = "Problem ID"),
        ("tc_id" = i32, Path, description = "Test case ID"),
    ),
    responses(
        (status = 200, description = "Test case details", body = TestCaseResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Test case not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(problem_id, tc_id))]
pub async fn get_test_case(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath((problem_id, tc_id)): AppPath<(i32, i32)>,
) -> Result<Json<TestCaseResponse>, AppError> {
    get_test_case_in(
        &state,
        &auth_user,
        ProblemScope::standalone(problem_id),
        tc_id,
    )
    .await
}

#[utoipa::path(
    get,
    path = "/{problem_id}/samples/{tc_id}",
    tag = "Contest Problems",
    operation_id = "getContestProblemSample",
    summary = "Get one sample test case of a contest problem",
    description = "Returns the full details of one sample (`is_sample = true`) test case, read through this contest. Visibility is decided for this contest only. The contest must have started. Non-sample test cases are not served here, whatever the caller's permissions.",
    params(
        ("id" = i32, Path, description = "Contest ID"),
        ("problem_id" = i32, Path, description = "Problem ID"),
        ("tc_id" = i32, Path, description = "Test case ID"),
    ),
    responses(
        (status = 200, description = "Test case details", body = TestCaseResponse),
        (status = 400, description = "Contest has not started yet (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 404, description = "Test case not found (NOT_FOUND)", body = ErrorBody),
        (status = 503, description = "A plugin this request needs could not answer in time (PLUGIN_UNAVAILABLE); carries Retry-After", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(contest_id, problem_id, tc_id))]
pub async fn get_contest_problem_sample(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath((contest_id, problem_id, tc_id)): AppPath<(i32, i32, i32)>,
) -> Result<Json<TestCaseResponse>, AppError> {
    get_test_case_in(
        &state,
        &auth_user,
        ProblemScope::in_contest(contest_id, problem_id),
        tc_id,
    )
    .await
}

/// Shared body of `get_test_case` and `get_contest_problem_sample`: one test
/// case of the problem as reached under `scope`.
async fn get_test_case_in(
    state: &AppState,
    auth_user: &AuthUser,
    scope: ProblemScope,
    tc_id: i32,
) -> Result<Json<TestCaseResponse>, AppError> {
    // Setters keep full access to every test case, but only on the
    // standalone route: a contest-scoped read serves samples only, under
    // that contest's rules, whoever asks.
    let setter_bypass = scope.contest_id.is_none()
        && (auth_user.has_permission(perm::PROBLEM_CREATE)
            || auth_user.has_permission(perm::PROBLEM_EDIT));

    if !setter_bypass {
        // Reachability to the *problem* is a kernel decision - the same
        // `Resource::Problem` decision `get_problem_in` (handlers/problem/
        // mod.rs) makes under the same scope - rather than a second,
        // hand-written copy of the permission/is_public/via-contest logic
        // that could drift from `visibility::host_rules`.
        // NOTE: `is_denied()` treats `Decision::Redact` the same as `Allow`
        // (only `Deny` is denied) - this gate is a pure yes/no reachability
        // check, not a `fetch_visible`/`into_masked_json` call, so a
        // hypothetical `Redact` here would never actually apply its
        // `FieldMask`: the response below is built straight from `tc` with
        // no masking step. Today no plugin returns `Redact` for
        // `Resource::Problem`, so this is a documented no-op, not a live
        // bug.
        let kernel = VisibilityKernel::new(state, Subject::from_auth_user(auth_user));
        require_problem_part_reachable(
            &kernel,
            state,
            auth_user,
            scope,
            Action::Read,
            "Test case not found",
        )
        .await?;
    }

    // Looked up only after the gate, so a viewer who cannot reach the
    // problem cannot tell which test case ids belong to it.
    let tc = find_test_case_for_problem(&state.db, scope.problem_id, tc_id).await?;
    // The kernel only knows the *problem* is reachable; whether this
    // particular test case is a public sample is local to test cases and
    // has no kernel resource of its own.
    if !setter_bypass && !tc.is_sample {
        return Err(AppError::NotFound("Test case not found".into()));
    }

    Ok(Json(
        test_case_response_from_model(tc, &*state.blob_store).await?,
    ))
}

#[utoipa::path(
    patch,
    path = "/{tc_id}",
    tag = "Test Cases",
    operation_id = "updateTestCase",
    summary = "Update a test case",
    description = "Partially updates a test case using PATCH semantics. Requires `problem:edit` permission. The `description` field supports three-state updates: omit to leave unchanged, set to null to clear, or provide a value. Body limit: 1 GB.",
    params(
        ("id" = i32, Path, description = "Problem ID"),
        ("tc_id" = i32, Path, description = "Test case ID"),
    ),
    request_body = UpdateTestCaseRequest,
    responses(
        (status = 200, description = "Test case updated", body = TestCaseResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 413, description = "Request body over the endpoint's limit (PAYLOAD_TOO_LARGE)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Test case not found (NOT_FOUND)", body = ErrorBody),
        (status = 409, description = "Duplicate label in problem (CONFLICT)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(_auth, state, payload), fields(problem_id, tc_id))]
pub async fn update_test_case(
    _auth: Authorized<ProblemEdit>,
    State(state): State<AppState>,
    AppPath((problem_id, tc_id)): AppPath<(i32, i32)>,
    AppJson(payload): AppJson<UpdateTestCaseRequest>,
) -> Result<Json<TestCaseResponse>, AppError> {
    validate_update_test_case(&payload)?;

    if payload == UpdateTestCaseRequest::default() {
        let existing = find_test_case_for_problem(&state.db, problem_id, tc_id).await?;
        return Ok(Json(
            test_case_response_from_model(existing, &*state.blob_store).await?,
        ));
    }

    let txn = state.db.begin().await?;
    // Take the same per-problem FOR UPDATE lock create/delete/reorder/bulk_delete
    // hold, so a label change is serialized against them: without it,
    // ensure_test_case_label_available below is a read-committed TOCTOU (there is
    // no DB unique index on test_case.label) and two concurrent updates could
    // both pass the check and commit duplicate labels.
    find_problem_for_update(&txn, problem_id).await?;
    let existing = find_test_case_for_problem(&txn, problem_id, tc_id).await?;
    let mut active: test_case::ActiveModel = existing.into();

    if let Some(input) = payload.input {
        let body = prepare_test_case_body(input, state.blob_store.clone()).await?;
        active.input = Set(body.inline_text);
        active.input_blob_hash = Set(body.blob_hash);
        active.input_size = Set(Some(body.size));
        active.input_preview = Set(Some(body.preview));
    }
    if let Some(expected_output) = payload.expected_output {
        let body = prepare_test_case_body(expected_output, state.blob_store.clone()).await?;
        active.expected_output = Set(body.inline_text);
        active.expected_output_blob_hash = Set(body.blob_hash);
        active.expected_output_size = Set(Some(body.size));
        active.expected_output_preview = Set(Some(body.preview));
    }
    if let Some(score) = payload.score {
        active.score = Set(score);
    }
    if let Some(is_sample) = payload.is_sample {
        active.is_sample = Set(is_sample);
    }
    if let Some(position) = payload.position {
        active.position = Set(position);
    }
    if let Some(label) = payload.label {
        let label = label.trim().to_string();
        ensure_test_case_label_available(&txn, problem_id, &label, Some(tc_id)).await?;
        active.label = Set(label);
    }
    match payload.description {
        Some(Some(desc)) => active.description = Set(Some(desc.trim().to_string())),
        Some(None) => active.description = Set(None),
        None => {}
    }

    let model = active.update(&txn).await?;
    txn.commit().await?;

    Ok(Json(
        test_case_response_from_model(model, &*state.blob_store).await?,
    ))
}

#[utoipa::path(
    delete,
    path = "/{tc_id}",
    tag = "Test Cases",
    operation_id = "deleteTestCase",
    summary = "Delete a test case",
    description = "Permanently deletes a test case. Requires `problem:edit` permission. Returns 409 CONFLICT if the test case has judge results.",
    params(
        ("id" = i32, Path, description = "Problem ID"),
        ("tc_id" = i32, Path, description = "Test case ID"),
    ),
    responses(
        (status = 204, description = "Test case deleted"),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Test case not found (NOT_FOUND)", body = ErrorBody),
        (status = 409, description = "Cannot delete: has judge results (CONFLICT)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(problem_id, tc_id))]
pub async fn delete_test_case(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath((problem_id, tc_id)): AppPath<(i32, i32)>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::PROBLEM_EDIT)?;

    let txn = state.db.begin().await?;
    find_problem_for_update(&txn, problem_id).await?;
    let tc = find_test_case_for_problem(&txn, problem_id, tc_id).await?;
    delete_unjudged_test_cases(&txn, test_case::Column::Id.eq(tc.id)).await?;
    txn.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    put,
    path = "/reorder",
    tag = "Test Cases",
    operation_id = "reorderTestCases",
    summary = "Reorder test cases for a problem",
    description = "Replaces the ordering of all test cases in a problem. Requires `problem:edit` permission. The ID array must contain exactly all test cases in the problem. Positions are assigned by array index starting at 0.",
    params(("id" = i32, Path, description = "Problem ID")),
    request_body = ReorderTestCasesRequest,
    responses(
        (status = 204, description = "Test cases reordered"),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Problem not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(_auth, state, payload), fields(problem_id))]
pub async fn reorder_test_cases(
    _auth: Authorized<ProblemEdit>,
    State(state): State<AppState>,
    AppPath(problem_id): AppPath<i32>,
    AppJson(payload): AppJson<ReorderTestCasesRequest>,
) -> Result<impl IntoResponse, AppError> {
    validate_reorder_test_cases(&payload)?;

    let txn = state.db.begin().await?;
    find_problem_for_update(&txn, problem_id).await?;

    let existing: Vec<i32> = test_case::Entity::find()
        .filter(test_case::Column::ProblemId.eq(problem_id))
        .select_only()
        .column(test_case::Column::Id)
        .into_tuple::<i32>()
        .all(&txn)
        .await?;

    let existing_set: std::collections::HashSet<i32> = existing.into_iter().collect();
    let payload_set: std::collections::HashSet<i32> =
        payload.test_case_ids.iter().copied().collect();
    if existing_set != payload_set {
        return Err(AppError::Validation(
            "test_case_ids must contain exactly the test cases currently in the problem".into(),
        ));
    }

    for (i, &tc_id) in payload.test_case_ids.iter().enumerate() {
        test_case::Entity::update_many()
            .filter(test_case::Column::Id.eq(tc_id))
            .col_expr(
                test_case::Column::Position,
                Expr::value(
                    i32::try_from(i).map_err(|_| {
                        AppError::Validation("Too many test cases to reorder".into())
                    })?,
                ),
            )
            .exec(&txn)
            .await?;
    }

    txn.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete,
    path = "/bulk",
    tag = "Test Cases",
    operation_id = "bulkDeleteTestCases",
    summary = "Bulk-delete test cases",
    description = "Deletes multiple test cases in a single operation. Requires `problem:edit` permission. Returns 409 CONFLICT if any test case has judge results, listing the offending IDs.",
    params(("id" = i32, Path, description = "Problem ID")),
    request_body = BulkDeleteTestCasesRequest,
    responses(
        (status = 200, description = "Test cases deleted", body = BulkDeleteTestCasesResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Problem not found or test case IDs not in problem (NOT_FOUND)", body = ErrorBody),
        (status = 409, description = "Some test cases have judge results (CONFLICT)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, payload), fields(problem_id))]
pub async fn bulk_delete_test_cases(
    auth_user: Authorized<ProblemEdit>,
    State(state): State<AppState>,
    AppPath(problem_id): AppPath<i32>,
    AppJson(payload): AppJson<BulkDeleteTestCasesRequest>,
) -> Result<Json<BulkDeleteTestCasesResponse>, AppError> {
    validate_bulk_delete_test_cases(&payload)?;

    let txn = state.db.begin().await?;
    find_problem_for_update(&txn, problem_id).await?;

    let existing_ids: Vec<i32> = test_case::Entity::find()
        .filter(test_case::Column::ProblemId.eq(problem_id))
        .filter(test_case::Column::Id.is_in(payload.test_case_ids.clone()))
        .select_only()
        .column(test_case::Column::Id)
        .into_tuple::<i32>()
        .all(&txn)
        .await?;

    let existing_set: std::collections::HashSet<i32> = existing_ids.into_iter().collect();
    let missing: Vec<i32> = payload
        .test_case_ids
        .iter()
        .filter(|id| !existing_set.contains(id))
        .copied()
        .collect();
    if !missing.is_empty() {
        return Err(AppError::NotFound(format!(
            "Test case IDs not found in problem {problem_id}: {missing:?}"
        )));
    }

    let deleted =
        delete_unjudged_test_cases(&txn, test_case::Column::Id.is_in(payload.test_case_ids))
            .await?;

    txn.commit().await?;

    tracing::info!(
        problem_id,
        deleted,
        user_id = auth_user.user_id,
        "Bulk deleted test cases"
    );

    Ok(Json(BulkDeleteTestCasesResponse {
        deleted: deleted as usize,
    }))
}

pub fn test_case_body_limit() -> usize {
    LARGE_UPLOAD_LIMIT_BYTES
}

async fn find_test_case_for_problem<C: ConnectionTrait>(
    db: &C,
    problem_id: i32,
    tc_id: i32,
) -> Result<test_case::Model, AppError> {
    find_active_child::<problem::Entity, test_case::Entity, _, _>(
        db,
        test_case::Column::ProblemId,
        problem_id,
        tc_id,
        "Test case",
    )
    .await
}

/// Delete the test cases matching `scope`, refusing with `409 CONFLICT` (and
/// the offending IDs) if any of them has judge results.
///
/// The single source of the "never orphan a `test_case_result`" rule shared by
/// single delete, bulk delete and ZIP `strategy=replace`: without it the
/// `test_case_result.test_case_id` FK rejects the delete and the caller sees a
/// 500 instead of an actionable conflict. The check is one `SELECT DISTINCT`
/// over the same scope, so it costs a single extra statement however many test
/// cases are involved. Callers hold the problem row lock (via
/// `find_problem_for_update`) for the surrounding transaction.
pub(super) async fn delete_unjudged_test_cases<C: ConnectionTrait>(
    db: &C,
    scope: impl sea_query::IntoCondition,
) -> Result<u64, AppError> {
    let scope = scope.into_condition();
    let ids_with_results: Vec<i32> = test_case_result::Entity::find()
        .filter(
            test_case_result::Column::TestCaseId.in_subquery(
                test_case::Entity::find()
                    .select_only()
                    .column(test_case::Column::Id)
                    .filter(scope.clone())
                    .into_query(),
            ),
        )
        .select_only()
        .column(test_case_result::Column::TestCaseId)
        .distinct()
        .order_by_asc(test_case_result::Column::TestCaseId)
        .into_tuple::<i32>()
        .all(db)
        .await?;

    if !ids_with_results.is_empty() {
        return Err(AppError::Conflict(format!(
            "Cannot delete: test cases {ids_with_results:?} have judge results"
        )));
    }

    let result = test_case::Entity::delete_many()
        .filter(scope)
        .exec(db)
        .await?;
    Ok(result.rows_affected)
}

pub(super) async fn next_test_case_position<C: ConnectionTrait>(
    db: &C,
    problem_id: i32,
) -> Result<i32, AppError> {
    let max_pos: Option<i32> = test_case::Entity::find()
        .filter(test_case::Column::ProblemId.eq(problem_id))
        .select_only()
        .column_as(test_case::Column::Position.max(), "max_pos")
        .into_tuple::<Option<i32>>()
        .one(db)
        .await?
        .flatten();
    max_pos
        .unwrap_or(-1)
        .checked_add(1)
        .ok_or_else(|| AppError::Validation("Position overflow".into()))
}

/// The first numeric label `>= start` that no test case of the problem uses.
///
/// A generated label must never be one the server then rejects as a
/// duplicate: ZIP uploads number labels from `1` while positions start at
/// `0`, so `position.to_string()` alone collides right after an upload. The
/// caller holds the problem row lock, so the label stays free until insert.
async fn first_free_test_case_label<C: ConnectionTrait>(
    db: &C,
    problem_id: i32,
    start: i32,
) -> Result<String, AppError> {
    let taken: std::collections::HashSet<String> = test_case::Entity::find()
        .filter(test_case::Column::ProblemId.eq(problem_id))
        .select_only()
        .column(test_case::Column::Label)
        .into_tuple::<String>()
        .all(db)
        .await?
        .into_iter()
        .collect();
    // At most `taken.len()` candidates can be occupied, so this terminates
    // within `taken.len() + 1` steps.
    (i64::from(start)..)
        .map(|n| n.to_string())
        .find(|label| !taken.contains(label))
        .ok_or_else(|| AppError::Internal("no free test case label".into()))
}

async fn ensure_test_case_label_available<C: ConnectionTrait>(
    db: &C,
    problem_id: i32,
    label: &str,
    exclude_id: Option<i32>,
) -> Result<(), AppError> {
    let mut query = test_case::Entity::find()
        .filter(test_case::Column::ProblemId.eq(problem_id))
        .filter(test_case::Column::Label.eq(label));

    if let Some(exclude_id) = exclude_id {
        query = query.filter(test_case::Column::Id.ne(exclude_id));
    }

    if query.one(db).await?.is_some() {
        return Err(AppError::Conflict(format!(
            "Test case label '{label}' already exists in problem {problem_id}"
        )));
    }

    Ok(())
}

pub(super) fn tc_to_list_item(m: test_case::Model) -> TestCaseListItem {
    let input_preview = truncate_preview(&test_case_body_preview(
        &m.input,
        m.input_preview.as_deref(),
    ));
    let output_preview = truncate_preview(&test_case_body_preview(
        &m.expected_output,
        m.expected_output_preview.as_deref(),
    ));
    TestCaseListItem {
        id: m.id,
        score: m.score,
        description: m.description,
        label: m.label,
        is_sample: m.is_sample,
        position: m.position,
        input_preview,
        output_preview,
        problem_id: m.problem_id,
        created_at: m.created_at,
    }
}

async fn test_case_response_from_model(
    m: test_case::Model,
    blob_store: &dyn common::storage::BlobStore,
) -> Result<TestCaseResponse, AppError> {
    let input = read_test_case_body(&m.input, m.input_blob_hash.as_deref(), blob_store).await?;
    let expected_output = read_test_case_body(
        &m.expected_output,
        m.expected_output_blob_hash.as_deref(),
        blob_store,
    )
    .await?;

    Ok(TestCaseResponse {
        id: m.id,
        input_size: test_case_body_size(&m.input, m.input_size),
        output_size: test_case_body_size(&m.expected_output, m.expected_output_size),
        input_preview: truncate_preview(&test_case_body_preview(
            &m.input,
            m.input_preview.as_deref(),
        )),
        output_preview: truncate_preview(&test_case_body_preview(
            &m.expected_output,
            m.expected_output_preview.as_deref(),
        )),
        input,
        expected_output,
        score: m.score,
        description: m.description,
        label: m.label,
        is_sample: m.is_sample,
        position: m.position,
        problem_id: m.problem_id,
        created_at: m.created_at,
    })
}
