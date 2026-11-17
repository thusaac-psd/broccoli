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

    /// Whether the caller is contest staff by the host's rule
    /// ([`crate::permissions::is_staff`]). A contest plugin shows staff what
    /// the host shows them, so it decides staff through this and never
    /// through a single permission.
    pub fn is_staff(&self) -> bool {
        self.auth
            .as_ref()
            .is_some_and(|auth| crate::permissions::is_staff(&auth.permissions))
    }

    /// The path parameter `name`, parsed. A missing or unparsable value is
    /// the caller's mistake: a 400 `INVALID_PATH_PARAMETER` response.
    pub fn param<T: FromStr>(&self, name: &str) -> Result<T, PluginHttpResponse> {
        self.params
            .get(name)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| {
                PluginHttpResponse::refuse(
                    400,
                    super::SdkRefusal::InvalidPathParameter,
                    format!("Invalid path parameter: {name}"),
                )
            })
    }

    /// The caller's user id. Only a `public` route can be reached without
    /// one; there, an anonymous caller gets a 403 `SIGN_IN_REQUIRED`
    /// response (a route every caller must be signed in for declares it in
    /// its manifest, and the host answers 401 before the handler runs).
    pub fn require_user_id(&self) -> Result<i32, PluginHttpResponse> {
        self.user_id().ok_or_else(|| {
            PluginHttpResponse::refuse(403, super::SdkRefusal::SignInRequired, "Sign in to do this")
        })
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

/// A plugin route handler's answer: a status and a JSON body, nothing else.
/// The host writes every response header itself (the body is always
/// `application/json`), so a plugin cannot set cookies, redirect or serve
/// a page on the host's origin.
///
/// A 2xx sends `body` as it is. A client error is sent in the host's error
/// shape with the plugin's code under its id, `<plugin id>.<code>`, and only
/// as 400, 403, 404, 409, 413 or 429; any other client error is sent as 400
/// (who is signed in is the host's to say). Every other status is the
/// host's generic 500 `INTERNAL_ERROR`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginHttpResponse {
    pub status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
}

impl PluginHttpResponse {
    /// A client error in the host's error shape, `{"code", "message"}`: one
    /// of the plugin's refusal codes (see [`super::RefusalCode`]), which a
    /// client shows in the user's language, and the plugin's English words
    /// for logs and a client without a text for the code.
    pub fn refuse(status: u16, code: impl super::RefusalCode, message: impl Into<String>) -> Self {
        Self {
            status,
            body: Some(serde_json::json!({ "code": code.as_str(), "message": message.into() })),
        }
    }

    /// The refusal with `value` under `name` in its `details`, which a client
    /// passes to the code's text as a parameter (`{name}` in the pack), so the
    /// user reads the numbers the refusal is about.
    pub fn detail(mut self, name: &str, value: impl Into<RefusalDetail>) -> Self {
        if let Some(body) = self.body.as_mut().and_then(|b| b.as_object_mut()) {
            let details = body
                .entry("details")
                .or_insert_with(|| serde_json::Value::Object(Default::default()));
            if let Some(details) = details.as_object_mut() {
                details.insert(name.to_string(), value.into().0);
            }
        }
        self
    }
}

/// A value a refusal's text may name: a number or a word, the only kinds a
/// client passes to a text as a parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct RefusalDetail(serde_json::Value);

macro_rules! number_detail {
    ($($t:ty),*) => {$(
        impl From<$t> for RefusalDetail {
            fn from(value: $t) -> Self {
                Self(serde_json::Value::from(value))
            }
        }
    )*};
}
number_detail!(i32, i64, u32, u64, usize);

impl From<&str> for RefusalDetail {
    fn from(value: &str) -> Self {
        Self(serde_json::Value::from(value))
    }
}

impl From<String> for RefusalDetail {
    fn from(value: String) -> Self {
        Self(serde_json::Value::from(value))
    }
}

/// The error code the host gives a plugin route's client error whose body
/// names no code (it sends it under the plugin's id). The host answers every
/// 5xx from a plugin route itself, with `INTERNAL_ERROR` and no detail, so a
/// plugin has no 5xx to name.
pub fn error_code_for_status(status: u16) -> &'static str {
    match status {
        400 => "VALIDATION_ERROR",
        403 => "PERMISSION_DENIED",
        404 => "NOT_FOUND",
        405 => "METHOD_NOT_ALLOWED",
        409 => "CONFLICT",
        413 => "PAYLOAD_TOO_LARGE",
        429 => "RATE_LIMITED",
        _ => "REQUEST_FAILED",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    crate::refusal_codes! {
        enum Refusal {
            TooManyPages = "TOO_MANY_PAGES",
        }
    }

    /// A refusal's details are the numbers and words its text names.
    #[test]
    fn a_refusal_carries_the_details_its_text_names() {
        let response = PluginHttpResponse::refuse(413, Refusal::TooManyPages, "too long")
            .detail("pages", 12_u32)
            .detail("limit", 10_i64)
            .detail("file", "main.cpp");
        assert_eq!(
            response.body,
            Some(serde_json::json!({
                "code": "TOO_MANY_PAGES",
                "message": "too long",
                "details": { "pages": 12, "limit": 10, "file": "main.cpp" },
            }))
        );
    }
}
