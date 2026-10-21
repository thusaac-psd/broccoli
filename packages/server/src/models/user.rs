use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Serialize, utoipa::ToSchema)]
pub struct UserResponse {
    #[schema(example = 1)]
    pub id: i32,
    #[schema(example = "alice")]
    pub username: String,
    pub display_name: String,
    #[schema(example = json!(["contestant"]))]
    pub roles: Vec<String>,
    #[schema(example = "2026-03-05T10:00:00Z")]
    pub created_at: DateTime<Utc>,
}

impl From<crate::entity::user::ModelEx> for UserResponse {
    fn from(user: crate::entity::user::ModelEx) -> Self {
        Self {
            id: user.id,
            username: user.username,
            display_name: user.display_name,
            roles: user.roles.into_iter().map(|r| r.name).collect(),
            created_at: user.created_at,
        }
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct UpdateUserRequest {
    pub username: Option<String>,
    /// Omit to keep the current name. Updates require user:manage.
    pub display_name: Option<String>,
    pub password: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct RoleAssignmentRequest {
    #[schema(example = "admin")]
    pub role: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct PermissionGrantRequest {
    #[schema(example = "manage_users")]
    pub permission: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct RoleResponse {
    pub name: String,
    pub permissions: Vec<String>,
}

/// Omit password to generate a random password. Accounts receive the default roles.
#[derive(Deserialize, utoipa::ToSchema)]
pub struct CreateUserRequest {
    pub username: String,
    /// Defaults to the initial username when omitted or blank.
    pub display_name: Option<String>,
    pub password: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct BulkCreateUsersRequest {
    pub users: Vec<CreateUserRequest>,
}

/// Credentials are returned only in the creation response, never in user listings.
#[derive(Serialize, utoipa::ToSchema)]
pub struct CreatedUserResponse {
    pub id: i32,
    pub username: String,
    pub display_name: String,
    pub roles: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub password: String,
}

pub fn normalize_display_name(name: &str) -> Result<String, crate::error::AppError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err(crate::error::AppError::Validation(
            "Display name must be 1-64 characters without control characters".into(),
        ));
    }
    Ok(name.to_owned())
}

pub fn validate_account_credentials(
    username: &str,
    password: Option<&str>,
) -> Result<(), crate::error::AppError> {
    use crate::error::AppError;
    let username = username.trim();
    if username.is_empty() || username.chars().count() > 32 {
        return Err(AppError::Validation(
            "Username must be 1-32 characters".into(),
        ));
    }
    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(AppError::Validation(
            "Username must contain only letters, digits, and underscores".into(),
        ));
    }
    if let Some(password) = password
        && (password.len() < 8 || password.len() > 128)
    {
        return Err(AppError::Validation(
            "Password must be 8-128 characters".into(),
        ));
    }
    Ok(())
}

pub fn validate_create_users(users: &[CreateUserRequest]) -> Result<(), crate::error::AppError> {
    use crate::error::AppError;
    if users.is_empty() || users.len() > 100 {
        return Err(AppError::Validation(
            "Provide between 1 and 100 users per import".into(),
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for entry in users {
        validate_account_credentials(&entry.username, entry.password.as_deref())?;
        if !seen.insert(entry.username.trim().to_ascii_lowercase()) {
            return Err(AppError::Validation(format!(
                "Duplicate username: '{}'",
                entry.username.trim()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::normalize_display_name;

    #[test]
    fn display_names_trim_whitespace_and_count_unicode_characters() {
        assert_eq!(normalize_display_name(" 第一队 🌱 ").unwrap(), "第一队 🌱");
        assert!(normalize_display_name(&"🌱".repeat(64)).is_ok());
        for name in [" ".to_owned(), "🌱".repeat(65), "bad\nname".to_owned()] {
            assert!(normalize_display_name(&name).is_err());
        }
    }
}
