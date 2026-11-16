//! Login, device-flow and CLI token types.

use broccoli_types::permissions as perm;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct LoginRequest {
    #[schema(example = "alice_wonder")]
    pub username: String,
    #[schema(example = "s3cure_P@ss!")]
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DeviceTokenRequest {
    pub device_code: String,
}

/// Access and refresh tokens returned to the CLI.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CliTokenResponse {
    #[schema(example = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9...")]
    pub token: String,
    #[schema(example = "a1b2c3...:d4e5f6...")]
    pub refresh_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CliRefreshRequest {
    #[schema(example = "a1b2c3...:d4e5f6...")]
    pub refresh_token: String,
}
