use crate::error::AppError;
pub use broccoli_api_types::auth::{
    CliRefreshRequest, CliTokenResponse, DeviceCodeResponse, DeviceTokenRequest, LoginRequest,
    LoginResponse, MeResponse,
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, utoipa::ToSchema)]
pub struct RegisterRequest {
    #[schema(example = "alice_wonder")]
    pub username: String,
    /// Defaults to the initial username when omitted or blank.
    pub display_name: Option<String>,
    #[schema(example = "s3cure_P@ss!")]
    pub password: String,
}

pub fn validate_register_request(payload: &RegisterRequest) -> Result<(), AppError> {
    super::user::validate_account_credentials(&payload.username, Some(&payload.password))
}

pub fn validate_login_request(payload: &LoginRequest) -> Result<(), AppError> {
    if payload.username.trim().is_empty() {
        return Err(AppError::Validation("Username must not be empty".into()));
    }
    if payload.password.is_empty() {
        return Err(AppError::Validation("Password must not be empty".into()));
    }
    Ok(())
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct RegisterResponse {
    #[schema(example = 42)]
    pub id: i32,
    #[schema(example = "alice_wonder")]
    pub username: String,
    pub display_name: String,
}

impl From<crate::entity::user::Model> for RegisterResponse {
    fn from(user: crate::entity::user::Model) -> Self {
        Self {
            id: user.id,
            username: user.username,
            display_name: user.display_name,
        }
    }
}

impl From<crate::services::auth_tokens::AccessGrant> for LoginResponse {
    fn from(grant: crate::services::auth_tokens::AccessGrant) -> Self {
        Self {
            token: grant.token,
            id: grant.user_id,
            username: grant.username,
            display_name: grant.display_name,
            roles: grant.roles,
            permissions: grant.permissions,
        }
    }
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct DeviceCodeRequest {}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct DeviceAuthorizeRequest {
    #[schema(example = "BCDF-GHJK")]
    pub user_code: String,
}
