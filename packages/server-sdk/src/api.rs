use crate::error::SdkError;
use crate::types::PluginHttpResponse;

/// Why a plugin route handler did not answer with success.
///
/// It is one of two things, and only the first ever reaches the caller:
///
/// - a client error, the response the handler built with
///   [`PluginHttpResponse::error`] (or any other non-5xx response);
/// - an internal error: every [`SdkError`] (a failed query, a host call
///   refused, a serialization error) and anything [`ApiError::internal`]
///   wraps. Its detail goes to the server log and the caller gets the host's
///   generic 500 `INTERNAL_ERROR`, so database or host text never reaches a
///   response body.
#[derive(Debug)]
pub struct ApiError(Failure);

#[derive(Debug)]
enum Failure {
    Response(PluginHttpResponse),
    Internal(String),
}

impl ApiError {
    /// An internal error with `detail` for the server log.
    pub fn internal(detail: impl Into<String>) -> Self {
        Self(Failure::Internal(detail.into()))
    }

    /// Whether this is an internal error.
    pub fn is_internal(&self) -> bool {
        matches!(self.0, Failure::Internal(_))
    }

    /// The response the caller receives. An internal error is a bodiless
    /// 500 here; the host answers it with its generic error body.
    pub fn into_response(self) -> PluginHttpResponse {
        match self.0 {
            Failure::Response(r) => r,
            Failure::Internal(_) => PluginHttpResponse {
                status: 500,
                headers: None,
                body: None,
            },
        }
    }

    /// The client error's response, or the internal error's detail.
    pub fn into_result(self) -> Result<PluginHttpResponse, String> {
        match self.0 {
            Failure::Response(r) => Ok(r),
            Failure::Internal(detail) => Err(detail),
        }
    }
}

impl From<SdkError> for ApiError {
    fn from(e: SdkError) -> Self {
        Self::internal(e.to_string())
    }
}

impl From<PluginHttpResponse> for ApiError {
    fn from(r: PluginHttpResponse) -> Self {
        Self(Failure::Response(r))
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        Self::internal(e.to_string())
    }
}

/// A plugin bug the SDK reports as text, such as reading the contest of a
/// route that declares none.
impl From<String> for ApiError {
    fn from(detail: String) -> Self {
        Self::internal(detail)
    }
}

/// Run a route handler on the proxy's JSON request. A client error is
/// returned as the response; an internal error fails the call, which the
/// host logs and answers with 500 `INTERNAL_ERROR`.
#[cfg(target_arch = "wasm32")]
pub fn run_api_handler(
    input: &str,
    handler: impl FnOnce(
        &crate::sdk::Host,
        &crate::types::PluginHttpRequest,
    ) -> Result<PluginHttpResponse, ApiError>,
) -> extism_pdk::FnResult<String> {
    let host = crate::sdk::Host::new();
    let req: crate::types::PluginHttpRequest =
        serde_json::from_str(input).map_err(|e| SdkError::Serialization(e.to_string()))?;
    let resp = match handler(&host, &req).map_err(ApiError::into_result) {
        Ok(r) | Err(Ok(r)) => r,
        Err(Err(detail)) => return Err(extism_pdk::Error::msg(detail).into()),
    };
    Ok(serde_json::to_string(&resp)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_errors_never_reach_the_response_body() {
        let err = ApiError::from(SdkError::Database(
            "permission denied for table submission".into(),
        ));
        assert!(err.is_internal());
        let resp = err.into_response();
        assert_eq!(resp.status, 500);
        assert!(resp.body.is_none());
    }

    #[test]
    fn client_errors_carry_the_host_error_shape() {
        let resp =
            ApiError::from(PluginHttpResponse::error(404, "Match not found")).into_response();
        assert_eq!(resp.status, 404);
        assert_eq!(
            resp.body,
            Some(serde_json::json!({"code": "NOT_FOUND", "message": "Match not found"}))
        );
    }
}
