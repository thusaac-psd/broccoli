use std::collections::HashMap;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::contest::RouteContest;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginHttpAuth {
    pub user_id: i32,
    pub username: String,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginHttpRequest {
    pub method: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub params: HashMap<String, String>,
    #[serde(default)]
    pub query: HashMap<String, String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub body: Option<serde_json::Value>,
    #[serde(default)]
    pub auth: Option<PluginHttpAuth>,
    /// The contest the route's `contest` path parameter names, resolved and
    /// checked by the host. Set exactly when the route declares `contest`.
    #[serde(default)]
    pub contest: Option<RouteContest>,
    /// The problem the route's `problem` path parameter names, checked by the
    /// host to be readable by the caller (inside [`Self::contest`] when the
    /// route declares both). Set exactly when the route declares `problem`.
    #[serde(default)]
    pub problem_id: Option<i32>,
}

impl PluginHttpRequest {
    pub fn user_id(&self) -> Option<i32> {
        self.auth.as_ref().map(|auth| auth.user_id)
    }

    pub fn has_permission(&self, permission: &str) -> bool {
        self.auth
            .as_ref()
            .is_some_and(|auth| auth.permissions.iter().any(|p| p == permission))
    }

    /// The path parameter `name`, parsed. A missing or unparsable value is
    /// the caller's mistake: a 400 `VALIDATION_ERROR` response.
    pub fn param<T: FromStr>(&self, name: &str) -> Result<T, PluginHttpResponse> {
        self.params
            .get(name)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| {
                PluginHttpResponse::error(400, format!("Invalid path parameter: {name}"))
            })
    }

    /// The caller's user id. Only a `public` route can be reached without
    /// one; there, an anonymous caller gets a 401 `TOKEN_MISSING` response.
    pub fn require_user_id(&self) -> Result<i32, PluginHttpResponse> {
        self.user_id()
            .ok_or_else(|| PluginHttpResponse::error(401, "Authentication required"))
    }

    /// The contest the route addresses. A route that does not declare
    /// `contest` in its manifest never has one, which is the plugin's bug,
    /// not the caller's: the error is the internal-error message.
    pub fn contest(&self) -> Result<&RouteContest, String> {
        self.contest
            .as_ref()
            .ok_or_else(|| format!("route {} declares no contest", self.path))
    }

    /// The problem the route addresses; see [`Self::contest`].
    pub fn problem_id(&self) -> Result<i32, String> {
        self.problem_id
            .ok_or_else(|| format!("route {} declares no problem", self.path))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginHttpResponse {
    pub status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
}

impl PluginHttpResponse {
    /// A client error in the host's error shape, `{"code", "message"}`, with
    /// the host's code for `status` (see [`error_code_for_status`]).
    pub fn error(status: u16, message: impl Into<String>) -> Self {
        Self::error_with_code(status, error_code_for_status(status), message)
    }

    /// A client error with a code of the plugin's own, for a condition its
    /// frontend tells apart from other errors of the same status.
    pub fn error_with_code(status: u16, code: &str, message: impl Into<String>) -> Self {
        Self {
            status,
            headers: None,
            body: Some(serde_json::json!({ "code": code, "message": message.into() })),
        }
    }
}

/// The host's error code for a client error status. The host answers every
/// 5xx from a plugin route itself, with `INTERNAL_ERROR` and no detail, so a
/// plugin has no 5xx to name.
pub fn error_code_for_status(status: u16) -> &'static str {
    match status {
        400 => "VALIDATION_ERROR",
        401 => "TOKEN_MISSING",
        403 => "PERMISSION_DENIED",
        404 => "NOT_FOUND",
        405 => "METHOD_NOT_ALLOWED",
        409 => "CONFLICT",
        413 => "PAYLOAD_TOO_LARGE",
        429 => "RATE_LIMITED",
        _ => "REQUEST_FAILED",
    }
}
