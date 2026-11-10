use broccoli_server_sdk::types::{
    AfterSubmissionEvent, BeforeSubmissionEvent, SubmissionAdmission,
};
use sea_orm::*;

// visibility-bypass-audited: this module has no HTTP handlers - it is
// `pub(super)` plumbing for hook dispatch, called only from callers that are
// themselves permission-gated (`rejudge.rs`'s perm::SUBMISSION_REJUDGE
// handlers, and the create-submission paths in `mod.rs`). None of these
// functions return a `submission` row to an HTTP response body.
use crate::entity::submission;
use crate::error::AppError;
use crate::hooks::{self, HookOutcome};
use crate::state::AppState;

pub(super) async fn dispatch_before_submission_hooks(
    state: &AppState,
    event: &BeforeSubmissionEvent,
    enabled_plugins: Option<&hooks::ResourceEnablements>,
    admission_owner: Option<&str>,
) -> Result<Option<SubmissionAdmission>, AppError> {
    let outcome = hooks::dispatch_submission_hooks(
        event,
        enabled_plugins,
        &state.registries.hook_registry,
        admission_owner,
    )
    .await?;

    match outcome {
        HookOutcome::Allowed(payload) => payload
            .get("submission_admission")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| AppError::Internal(format!("Invalid submission admission: {e}"))),
        HookOutcome::Stopped => Ok(None),
        HookOutcome::Rejected {
            code,
            message,
            status_code,
            details,
        } => Err(AppError::PluginRejection {
            code,
            message,
            status_code,
            details,
        }),
    }
}

pub(super) fn fire_after_submission_hooks(
    state: &AppState,
    submission_id: i32,
    user_id: i32,
    problem_id: i32,
    contest_id: Option<i32>,
    language: String,
    enabled_plugins: Option<hooks::ResourceEnablements>,
) {
    hooks::dispatch_hooks_background_typed(
        AfterSubmissionEvent {
            submission_id,
            user_id,
            problem_id,
            contest_id,
            language,
        },
        enabled_plugins,
        state.registries.hook_registry.clone(),
        Some(format!("after_submission:{}", submission_id)),
    );
}

pub(super) async fn find_submission<C: ConnectionTrait>(
    db: &C,
    id: i32,
) -> Result<submission::Model, AppError> {
    submission::Entity::find_by_id(id)
        .one(db)
        .await?
        .ok_or_else(|| AppError::NotFound("Submission not found".into()))
}
