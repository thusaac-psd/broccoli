use broccoli_server_sdk::types::{
    AfterSubmissionEvent, BeforeSubmissionEvent, HookEvent, SubmissionAdmission,
    SubmissionAttemptAbandonedEvent,
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

/// How long a submission attempt can stay unresolved after its
/// `before_submission` dispatch, published to hooks as
/// [`BeforeSubmissionEvent::attempt_timeout_ms`]. It is the request timeout:
/// the dispatch runs inside the request, and the request's `TimeoutLayer`
/// drops the handler once that elapses, so no insert for the attempt starts
/// later.
pub(super) const ATTEMPT_TIMEOUT: std::time::Duration = crate::REQUEST_TIMEOUT;

/// A submission attempt from its `before_submission` dispatch until its
/// submission commits. An attempt that ends without one announces
/// `submission_attempt_abandoned` to the hooks enabled for the resource, so
/// whatever an earlier hook reserved for it is released at once rather than
/// when the attempt times out. [`PendingAttempt::settle`] announces it before
/// the error response leaves, so the release is in place when the client
/// sees the rejection; a request dropped before settling (its timeout)
/// announces it from `Drop`, in the background.
pub(super) struct PendingAttempt {
    abandoned: Option<SubmissionAttemptAbandonedEvent>,
    enabled_plugins: Option<hooks::ResourceEnablements>,
    hook_registry: hooks::SharedHookRegistry,
}

impl PendingAttempt {
    pub(super) fn new(
        state: &AppState,
        event: &BeforeSubmissionEvent,
        enabled_plugins: &hooks::ResourceEnablements,
    ) -> Self {
        Self {
            abandoned: Some(SubmissionAttemptAbandonedEvent {
                attempt_id: event.attempt_id.clone(),
                user_id: event.user_id,
                problem_id: event.problem_id,
                contest_id: event.contest_id,
            }),
            enabled_plugins: Some(enabled_plugins.clone()),
            hook_registry: state.registries.hook_registry.clone(),
        }
    }

    /// Ends the attempt with the outcome of admitting and inserting it: an
    /// `Err` means no submission committed, and is returned once the hooks
    /// have been told.
    pub(super) async fn settle<T>(mut self, outcome: Result<T, AppError>) -> Result<T, AppError> {
        let Some(event) = self.abandoned.take() else {
            return outcome;
        };
        if outcome.is_err() {
            let payload = serde_json::to_value(&event)
                .map_err(|e| AppError::Internal(format!("Failed to serialize hook event: {e}")))?;
            if let Err(e) = hooks::dispatch_hooks(
                SubmissionAttemptAbandonedEvent::TOPIC,
                payload,
                self.enabled_plugins.as_ref(),
                &self.hook_registry,
                plugin_core::admission::CallClass::Background,
            )
            .await
            {
                tracing::warn!(attempt_id = %event.attempt_id, "Abandoned-attempt hooks failed: {e:?}");
            }
        }
        outcome
    }
}

impl Drop for PendingAttempt {
    fn drop(&mut self) {
        if let Some(event) = self.abandoned.take() {
            let event_id = format!(
                "{}:{}",
                SubmissionAttemptAbandonedEvent::TOPIC,
                event.attempt_id
            );
            hooks::dispatch_hooks_background_typed(
                event,
                self.enabled_plugins.take(),
                self.hook_registry.clone(),
                Some(event_id),
            );
        }
    }
}

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
