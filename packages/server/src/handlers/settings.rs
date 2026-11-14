//! Fleet-wide settings an admin changes at runtime.

use axum::Json;
use axum::extract::State;
use broccoli_server_sdk::permissions as perm;
use tracing::{info, instrument};

use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::FreshAuthUser;
use crate::extractors::json::AppJson;
use crate::models::auth::RegistrationStatus;
use crate::state::AppState;

#[utoipa::path(
    put,
    path = "/settings/registration",
    tag = "Admin",
    operation_id = "setRegistrationStatus",
    summary = "Open or close self-registration",
    description = "Requires `user:manage`. Opens or closes `POST /auth/register` for every server at once, with no restart. Closed on a fresh install.",
    request_body = RegistrationStatus,
    responses(
        (status = 200, description = "The registration status now", body = RegistrationStatus),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, payload), fields(user_id = auth_user.user_id, open = payload.open))]
pub async fn set_registration_status(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppJson(payload): AppJson<RegistrationStatus>,
) -> Result<Json<RegistrationStatus>, AppError> {
    auth_user.require_permission(perm::USER_MANAGE)?;
    crate::services::registration::set_open(&state.db, payload.open).await?;
    info!(open = payload.open, "Self-registration changed");
    Ok(Json(payload))
}
