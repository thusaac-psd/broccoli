use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use broccoli_server_sdk::permissions as perm;
use chrono::Utc;
use common::SubmissionStatus;
use sea_orm::*;
use tracing::instrument;

use crate::dispatcher::queue_depth::enforce_queue_depth_admission;
// visibility-bypass-audited: `run_code`/`run_contest_code` require
// perm::SUBMISSION_SUBMIT and only ever create a run owned by the caller.
// `get_code_run`, the one read, gates on `cr.user_id == auth_user.user_id ||
// auth_user.has_permission(perm::SUBMISSION_VIEW_ALL)` - pinned by
// `owner_can_get_their_code_run`, `other_user_cannot_see_code_run`, and
// `admin_can_see_any_code_run` (tests/integration/code_run.rs). There is no
// `Resource::CodeRun` in the kernel (code runs are a scratch/practice
// feature, not a contest-scoped resource), so this ownership check cannot be
// expressed as a kernel decision.
use crate::entity::{code_run, code_run_result, problem, user};
use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::AuthUser;
use crate::extractors::json::AppJson;
use crate::extractors::path::AppPath;
use crate::models::code_run::*;
use crate::state::AppState;
use crate::utils::contest::{
    find_contest, is_problem_in_contest, require_contest_participant, require_contest_running,
    require_problem_read_access,
};
use crate::utils::judging::{files_from_json, files_to_json, validate_run_language};
use crate::utils::problem::find_problem;
use crate::utils::rate_limit::check_rate_limit;
use crate::utils::text::sanitize_db_json;

async fn build_code_run_response(
    db: &DatabaseConnection,
    cr: code_run::Model,
) -> Result<CodeRunResponse, AppError> {
    let user_model = user::Entity::find_by_id(cr.user_id)
        .one(db)
        .await?
        .ok_or_else(|| AppError::Internal("Code run user not found".into()))?;

    let problem_model = problem::Entity::find_by_id(cr.problem_id)
        .one(db)
        .await?
        .ok_or_else(|| AppError::Internal("Code run problem not found".into()))?;

    let custom_tcs: Vec<CustomTestCaseInput> =
        serde_json::from_value(cr.custom_test_cases.clone()).unwrap_or_default();

    let is_running = cr.status == SubmissionStatus::Running;
    let show_results = cr.status.is_terminal() || is_running;

    let result_response = if show_results {
        let results = code_run_result::Entity::find()
            .filter(code_run_result::Column::CodeRunId.eq(cr.id))
            .order_by_asc(code_run_result::Column::RunIndex)
            .all(db)
            .await?;

        let test_case_results: Vec<CodeRunResultResponse> = results
            .into_iter()
            .map(|r| {
                let tc = custom_tcs.get(r.run_index as usize);
                CodeRunResultResponse {
                    id: r.id,
                    verdict: r.verdict,
                    score: r.score,
                    time_used: r.time_used,
                    memory_used: r.memory_used,
                    run_index: r.run_index,
                    input: tc.map(|t| t.input.clone()),
                    expected_output: tc.and_then(|t| t.expected_output.clone()),
                    stdout: r.stdout,
                    stderr: r.stderr,
                    checker_output: r.checker_output,
                }
            })
            .collect();

        if is_running {
            Some(CodeRunJudgeResult {
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
            Some(CodeRunJudgeResult {
                verdict: cr.verdict,
                score: cr.score,
                time_used: cr.time_used,
                memory_used: cr.memory_used,
                compile_output: cr.compile_output.clone(),
                error_message: cr.error_message.clone(),
                judged_at: cr.judged_at,
                test_case_results,
            })
        }
    } else {
        None
    };

    let files = files_from_json(&cr.files);

    Ok(CodeRunResponse {
        id: cr.id,
        files,
        language: cr.language,
        status: cr.status,
        user_id: cr.user_id,
        username: user_model.username,
        problem_id: cr.problem_id,
        problem_title: problem_model.title,
        contest_id: cr.contest_id,
        contest_type: cr.contest_type,
        custom_test_cases: custom_tcs,
        created_at: cr.created_at,
        result: result_response,
    })
}

#[utoipa::path(
    post,
    path = "/",
    tag = "Code Runs",
    operation_id = "runCode",
    summary = "Run code against custom test cases",
    description = "Runs code against custom test cases for a problem. Results are ephemeral and don't affect scoring.",
    params(("id" = i32, Path, description = "Problem ID")),
    request_body = RunCodeRequest,
    responses(
        (status = 201, description = "Code run created", body = CodeRunResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Problem not found (NOT_FOUND)", body = ErrorBody),
        (status = 429, description = "Per-user rate limited (RATE_LIMITED)", body = ErrorBody),
        (status = 503, description = "Durable queue depth exceeded (QUEUE_OVERLOADED)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, payload), fields(problem_id = %problem_id))]
pub async fn run_code(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(problem_id): AppPath<i32>,
    AppJson(payload): AppJson<RunCodeRequest>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::SUBMISSION_SUBMIT)?;
    validate_run_code(&payload, state.config.submission.max_size)?;
    check_rate_limit(
        &state.db,
        auth_user.user_id,
        state.config.submission.rate_limit_per_minute,
    )
    .await?;
    // UP#39 backpressure-on-post: code-run rows ride the same
    // durable-accept lifecycle as submissions (see
    // `submission::create_submission`) and are counted toward the
    // same cap.
    enforce_queue_depth_admission(&state).await?;

    let txn = state.db.begin().await?;
    let problem = find_problem(&txn, problem_id).await?;
    // Prevent probing hidden/unreleased problems: a code run must be gated by
    // the same read-access rule as viewing the problem (contest membership or
    // problem-edit permission), not merely the `submission:submit` capability.
    require_problem_read_access(&txn, &auth_user, problem_id).await?;

    let known_languages: std::collections::HashSet<String> = state
        .registries
        .language_resolver_registry
        .read()
        .await
        .keys()
        .cloned()
        .collect();
    validate_run_language(&payload.language, &known_languages)?;

    let contest_type = problem.default_contest_type.clone();
    let custom_tcs_json = sanitize_db_json(
        serde_json::to_value(&payload.custom_test_cases).unwrap_or(serde_json::Value::Null),
    );

    let now = Utc::now();
    let language = payload.language.trim().to_string();
    let new_code_run = code_run::ActiveModel {
        files: Set(files_to_json(&payload.files)),
        language: Set(language),
        // UP#37: code-run rows ride the same durable-accept lifecycle
        // as submissions - see `handlers/submission.rs::create_submission`
        // for the gap being closed and `dispatcher/claim.rs` for the
        // claim fiber that picks `Queued` rows off this table too.
        status: Set(SubmissionStatus::Queued),
        user_id: Set(auth_user.user_id),
        problem_id: Set(problem_id),
        contest_id: Set(None),
        contest_type: Set(contest_type),
        custom_test_cases: Set(custom_tcs_json),
        created_at: Set(now),
        ..Default::default()
    };

    let model = new_code_run.insert(&txn).await?;
    txn.commit().await?;

    let response = build_code_run_response(&state.db, model).await?;

    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    post,
    path = "/",
    tag = "Code Runs",
    operation_id = "runContestCode",
    summary = "Run code against test cases in a contest",
    description = "Runs code against custom test cases for a contest problem. The user must be a contest participant and the contest must be running.",
    params(
        ("id" = i32, Path, description = "Contest ID"),
        ("problem_id" = i32, Path, description = "Problem ID")
    ),
    request_body = RunCodeRequest,
    responses(
        (status = 201, description = "Code run created", body = CodeRunResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Contest or problem not found (NOT_FOUND)", body = ErrorBody),
        (status = 429, description = "Per-user rate limited (RATE_LIMITED)", body = ErrorBody),
        (status = 503, description = "Durable queue depth exceeded (QUEUE_OVERLOADED)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, payload), fields(id = %id, problem_id = %problem_id))]
pub async fn run_contest_code(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath((id, problem_id)): AppPath<(i32, i32)>,
    AppJson(payload): AppJson<RunCodeRequest>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::SUBMISSION_SUBMIT)?;
    validate_run_code(&payload, state.config.submission.max_size)?;
    check_rate_limit(
        &state.db,
        auth_user.user_id,
        state.config.submission.rate_limit_per_minute,
    )
    .await?;
    // UP#39 backpressure-on-post: same rationale as `run_code`.
    enforce_queue_depth_admission(&state).await?;

    let contest_id = id;
    // No wrapping transaction: independent read validations + a single-row
    // INSERT. Holding a pooled txn connection open across the later
    // `require_contest_participant(&state.db)` acquisition deadlocked the core
    // pool under sustained load — see `submission::create_contest_submission`
    // for the full analysis. Each step runs on the pool directly.
    let contest_model = find_contest(&state.db, contest_id).await?;
    let _problem = find_problem(&state.db, problem_id).await?;
    if !is_problem_in_contest(&state.db, contest_id, problem_id).await? {
        return Err(AppError::NotFound(
            "Problem not found in this contest".into(),
        ));
    }

    let now = Utc::now();
    require_contest_running(&auth_user, &contest_model, now)?;
    require_contest_participant(&state.db, &auth_user, &contest_model).await?;

    let known_languages: std::collections::HashSet<String> = state
        .registries
        .language_resolver_registry
        .read()
        .await
        .keys()
        .cloned()
        .collect();
    validate_run_language(&payload.language, &known_languages)?;

    let custom_tcs_json = sanitize_db_json(
        serde_json::to_value(&payload.custom_test_cases).unwrap_or(serde_json::Value::Null),
    );

    let language = payload.language.trim().to_string();
    let contest_type = match &contest_model.contest_type {
        Some(ct) => ct.clone(),
        None => {
            let reg = state.registries.contest_type_registry.read().await;
            reg.keys().min().cloned().unwrap_or_default()
        }
    };
    let new_code_run = code_run::ActiveModel {
        files: Set(files_to_json(&payload.files)),
        language: Set(language),
        // UP#37: durable-accept - see `run_code` above for the rationale.
        status: Set(SubmissionStatus::Queued),
        user_id: Set(auth_user.user_id),
        problem_id: Set(problem_id),
        contest_id: Set(Some(contest_id)),
        contest_type: Set(contest_type),
        custom_test_cases: Set(custom_tcs_json),
        created_at: Set(now),
        ..Default::default()
    };

    let model = new_code_run.insert(&state.db).await?;

    let response = build_code_run_response(&state.db, model).await?;

    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    get,
    path = "/{id}",
    tag = "Code Runs",
    operation_id = "getCodeRun",
    summary = "Get a code run by ID",
    description = "Returns the code run details including judge results. Used for polling run status.",
    params(("id" = i32, Path, description = "Code run ID")),
    responses(
        (status = 200, description = "Code run details", body = CodeRunResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 404, description = "Code run not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(id = %id))]
pub async fn get_code_run(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<i32>,
) -> Result<Json<CodeRunResponse>, AppError> {
    let cr = code_run::Entity::find_by_id(id)
        .one(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Code run not found".into()))?;

    let can_view =
        cr.user_id == auth_user.user_id || auth_user.has_permission(perm::SUBMISSION_VIEW_ALL);

    if !can_view {
        return Err(AppError::NotFound("Code run not found".into()));
    }

    let response = build_code_run_response(&state.db, cr).await?;
    Ok(Json(response))
}

pub fn code_run_body_limit(max_size: usize) -> axum::extract::DefaultBodyLimit {
    axum::extract::DefaultBodyLimit::max(max_size + 4096)
}
