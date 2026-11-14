//! Who may call a plugin route, and the resources it addresses. The proxy
//! runs [`authorize`] before every plugin route handler, so a plugin never
//! decides contest or problem visibility, contest phases or authentication
//! itself: it declares them in `plugin.toml` and receives the result.
//!
//! Every answer matches the core route for the same resource: 401 without a
//! (fresh) token, 403 for a missing permission, 404 for a contest or problem
//! the caller cannot read (the visibility kernel decides), 400 for a path
//! parameter that is not an id.

use std::collections::HashMap;

use broccoli_server_sdk::types::RouteContest;
use plugin_core::manifest::{ContestTarget, RouteAccess, RouteTarget};

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::state::AppState;
use crate::utils::contest::{
    contest_phase, find_contest, is_contest_participant, participant_gate,
};
use crate::visibility::{Action, ProblemScope, Resource, Subject, VisibilityKernel};

/// What a route addresses, resolved and checked for its caller.
#[derive(Debug, Default)]
pub struct Resolved {
    pub contest: Option<RouteContest>,
    pub problem_id: Option<i32>,
}

/// Check that `caller` may call a route with `access`, and resolve what the
/// route addresses from its path `params`.
pub async fn authorize(
    state: &AppState,
    caller: Option<&AuthUser>,
    access: &RouteAccess,
    params: &HashMap<String, String>,
) -> Result<Resolved, AppError> {
    let (permission, target) = match access {
        RouteAccess::Public => return Ok(Resolved::default()),
        RouteAccess::SignedIn { permission, target } => (permission, target),
    };
    let user = caller.ok_or(AppError::TokenMissing)?;
    if let Some(permission) = permission {
        user.require_permission(permission)?;
    }
    match target {
        RouteTarget::Nothing => Ok(Resolved::default()),
        RouteTarget::Problem { problem } => {
            let problem_id = path_id(params, problem)?;
            let kernel = VisibilityKernel::new(state, Subject::from_auth_user(user));
            if kernel
                .decide(Action::Read, ProblemScope::standalone(problem_id).problem())
                .await?
                .is_denied()
            {
                return Err(AppError::NotFound("Problem not found".into()));
            }
            Ok(Resolved {
                contest: None,
                problem_id: Some(problem_id),
            })
        }
        RouteTarget::Contest(target) => resolve_contest(state, user, target, params).await,
    }
}

async fn resolve_contest(
    state: &AppState,
    user: &AuthUser,
    target: &ContestTarget,
    params: &HashMap<String, String>,
) -> Result<Resolved, AppError> {
    let contest_id = path_id(params, &target.contest)?;
    let problem_id = target
        .problem
        .as_deref()
        .map(|name| path_id(params, name))
        .transpose()?;
    let contest = find_contest(&state.db, contest_id).await?;
    if target
        .contest_type
        .as_ref()
        .is_some_and(|served| contest.contest_type.as_ref() != Some(served))
    {
        return Err(AppError::NotFound("Contest not found".into()));
    }

    let kernel = VisibilityKernel::new(state, Subject::from_auth_user(user));
    kernel.know_contest(contest.clone());
    let mut resources = vec![Resource::Contest(contest_id)];
    resources.extend(problem_id.map(|p| ProblemScope::in_contest(contest_id, p).problem()));
    let decisions = kernel.decide_batch(Action::Read, &resources).await?;
    if decisions[0].is_denied() {
        return Err(AppError::NotFound("Contest not found".into()));
    }
    if decisions.get(1).is_some_and(|d| d.is_denied()) {
        return Err(AppError::NotFound("Contest problem not found".into()));
    }

    let participant = match kernel.participates(contest_id) {
        Some(participant) => participant,
        None => is_contest_participant(&state.db, contest_id, user.user_id).await?,
    };
    if target.participant {
        participant_gate(user, &contest, participant)?;
    }
    let phase = contest_phase(&contest, state.clock.now());
    Ok(Resolved {
        contest: Some(RouteContest {
            id: contest.id,
            contest_type: contest.contest_type,
            is_public: contest.is_public,
            phase,
            participant,
        }),
        problem_id,
    })
}

/// The id in path parameter `name`. The router matched the route, so the
/// parameter is present; a value that is not an id is a 400, as on core
/// routes.
fn path_id(params: &HashMap<String, String>, name: &str) -> Result<i32, AppError> {
    params
        .get(name)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| AppError::Validation(format!("Invalid path parameter: {name}")))
}
