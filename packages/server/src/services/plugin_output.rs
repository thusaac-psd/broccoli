//! What a plugin says to a client, as the host sends it on.
//!
//! A plugin reaches a client through its route responses, its hook
//! rejections and the error codes it records on judgements and code runs.
//! Everything here confines that output to the plugin's own namespace and to a
//! shape the host vouches for, so a plugin can never speak as the host (a
//! `TOKEN_INVALID` that makes the client rotate its session, a
//! `PLUGIN_UNAVAILABLE` that reads as "busy, retrying") or as another plugin.

use axum::http::StatusCode;
use broccoli_api_types::error::ErrorBody;

/// A plugin's error code as the host passes it on: `<plugin_id>.<code>`. A
/// plugin id holds no `.`, so a client reads the plugin off the code and
/// looks its translation up in that plugin's pack
/// (`<plugin_id>.apiError.<code>`), and two plugins' codes never collide. A
/// code without a plugin id is the host's own: a plugin never sends one.
pub(crate) fn plugin_code(plugin_id: &str, code: &str) -> String {
    format!("{plugin_id}.{code}")
}

/// The statuses a plugin may refuse a request with. Who is signed in (401)
/// is the host's to say, as is every 5xx; any other client error a plugin
/// names is sent as 400.
const REFUSAL_STATUSES: [StatusCode; 6] = [
    StatusCode::BAD_REQUEST,
    StatusCode::FORBIDDEN,
    StatusCode::NOT_FOUND,
    StatusCode::CONFLICT,
    StatusCode::PAYLOAD_TOO_LARGE,
    StatusCode::TOO_MANY_REQUESTS,
];

/// A plugin's refusal of a request (a route's client error or a blocking
/// hook's rejection) in the host's error shape: one of
/// [`REFUSAL_STATUSES`] and the plugin's code under its id
/// ([`plugin_code`]). The only way to build one is [`Self::new`], so no
/// refusal reaches a client unconfined.
#[derive(Debug)]
pub struct PluginRefusal {
    status: StatusCode,
    body: ErrorBody,
}

impl PluginRefusal {
    pub(crate) fn new(
        plugin_id: &str,
        status: u16,
        code: &str,
        message: String,
        details: Option<serde_json::Value>,
    ) -> Self {
        let status = REFUSAL_STATUSES
            .into_iter()
            .find(|allowed| allowed.as_u16() == status)
            .unwrap_or(StatusCode::BAD_REQUEST);
        Self {
            status,
            body: ErrorBody {
                code: plugin_code(plugin_id, code),
                message,
                details,
            },
        }
    }

    /// A route's client error from the body the plugin answered with. A body
    /// without a code gets the code the server SDK gives `status`.
    pub(crate) fn from_route_body(
        plugin_id: &str,
        status: u16,
        body: Option<serde_json::Value>,
    ) -> Self {
        let field = |name: &str| body.as_ref().and_then(|body| body.get(name));
        let code = field("code")
            .and_then(|code| code.as_str())
            .unwrap_or_else(|| broccoli_server_sdk::types::error_code_for_status(status));
        let message = field("message")
            .and_then(|message| message.as_str())
            .unwrap_or_default()
            .to_string();
        Self::new(plugin_id, status, code, message, field("details").cloned())
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn code(&self) -> &str {
        &self.body.code
    }

    pub(crate) fn into_parts(self) -> (StatusCode, ErrorBody) {
        (self.status, self.body)
    }
}

impl From<PluginRefusal> for crate::error::AppError {
    fn from(refusal: PluginRefusal) -> Self {
        Self::PluginRefusal(refusal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_carries_the_code_under_the_plugin_id() {
        let refusal = PluginRefusal::new("cooldown", 429, "COOLDOWN_ACTIVE", "wait".into(), None);
        assert_eq!(refusal.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(refusal.code(), "cooldown.COOLDOWN_ACTIVE");
    }

    #[test]
    fn a_host_code_from_a_plugin_is_the_plugins_own() {
        for code in [
            "TOKEN_INVALID",
            "PLUGIN_UNAVAILABLE",
            "IDEMPOTENCY_RESPONSE_NOT_STORED",
        ] {
            let refusal = PluginRefusal::new("evil", 409, code, String::new(), None);
            assert_eq!(refusal.code(), format!("evil.{code}"));
        }
    }

    #[test]
    fn a_status_the_host_owns_is_sent_as_400() {
        for status in [401, 402, 418, 451, 500, 503, 200, 302, 99] {
            let refusal = PluginRefusal::new("p", status, "X", String::new(), None);
            assert_eq!(refusal.status(), StatusCode::BAD_REQUEST, "{status}");
        }
        for status in [400, 403, 404, 409, 413, 429] {
            let refusal = PluginRefusal::new("p", status, "X", String::new(), None);
            assert_eq!(refusal.status().as_u16(), status);
        }
    }

    #[test]
    fn a_route_body_without_a_code_gets_the_sdk_code_for_its_status() {
        let refusal = PluginRefusal::from_route_body(
            "p",
            404,
            Some(serde_json::json!({"message": "gone", "details": {"id": 3}})),
        );
        assert_eq!(refusal.code(), "p.NOT_FOUND");
        let (_, body) = refusal.into_parts();
        assert_eq!(body.message, "gone");
        assert_eq!(body.details, Some(serde_json::json!({"id": 3})));

        let refusal = PluginRefusal::from_route_body("p", 400, Some(serde_json::json!("text")));
        assert_eq!(refusal.code(), "p.VALIDATION_ERROR");
    }
}
