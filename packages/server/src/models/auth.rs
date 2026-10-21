use crate::error::AppError;
use broccoli_server_sdk::permissions as perm;
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

#[derive(Deserialize, utoipa::ToSchema)]
pub struct LoginRequest {
    #[schema(example = "alice_wonder")]
    pub username: String,
    #[schema(example = "s3cure_P@ss!")]
    pub password: String,
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

#[derive(Serialize, utoipa::ToSchema)]
pub struct LoginResponse {
    #[schema(example = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9...")]
    pub token: String,
    #[schema(example = 42)]
    pub id: i32,
    #[schema(example = "alice_wonder")]
    pub username: String,
    pub display_name: String,
    #[schema(example = json!(["contestant"]))]
    pub roles: Vec<String>,
    #[schema(example = json!([perm::SUBMISSION_SUBMIT]))]
    pub permissions: Vec<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct MeResponse {
    #[schema(example = 42)]
    pub id: i32,
    #[schema(example = "alice_wonder")]
    pub username: String,
    pub display_name: String,
    #[schema(example = json!(["contestant"]))]
    pub roles: Vec<String>,
    #[schema(example = json!([perm::SUBMISSION_SUBMIT]))]
    pub permissions: Vec<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct DeviceCodeRequest {}

#[derive(Serialize, utoipa::ToSchema)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    #[schema(example = "BCDF-GHJK")]
    pub user_code: String,
    #[schema(example = "http://localhost:5173/auth/device")]
    pub verification_url: String,
    #[schema(example = 900)]
    pub expires_in: u64,
    #[schema(example = 5)]
    pub interval: u64,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct DeviceAuthorizeRequest {
    #[schema(example = "BCDF-GHJK")]
    pub user_code: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct DeviceTokenRequest {
    pub device_code: String,
}

/// Access and refresh tokens returned to the CLI.
#[derive(Serialize, utoipa::ToSchema)]
pub struct CliTokenResponse {
    #[schema(example = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9...")]
    pub token: String,
    #[schema(example = "a1b2c3...:d4e5f6...")]
    pub refresh_token: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct CliRefreshRequest {
    #[schema(example = "a1b2c3...:d4e5f6...")]
    pub refresh_token: String,
}
