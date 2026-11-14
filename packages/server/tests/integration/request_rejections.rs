//! Every request an endpoint refuses before its handler runs (a query string
//! or body that does not parse, a body over the endpoint's limit) is answered
//! in the API's JSON error shape with a stable code.

use crate::common::TestApp;
use serde_json::{Value, json};

fn assert_error(status: u16, body: &Value, text: &str, want_status: u16, want_code: &str) {
    assert_eq!(status, want_status, "{text}");
    assert_eq!(body["code"], want_code, "{text}");
    assert!(
        body["message"].as_str().is_some_and(|m| !m.is_empty()),
        "{text}"
    );
}

#[tokio::test]
async fn a_query_string_that_does_not_parse_is_a_json_validation_error() {
    let app = TestApp::spawn().await;
    let token = app
        .create_user_with_role("query_reject_admin", "password123", "admin")
        .await;
    for query in ["per_page=-1", "status=judged", "problem_id=abc"] {
        let res = app
            .get_with_token(&format!("/api/v1/submissions?{query}"), &token)
            .await;
        assert_error(res.status, &res.body, &res.text, 400, "VALIDATION_ERROR");
        assert!(
            res.body["message"]
                .as_str()
                .unwrap()
                .starts_with("Invalid query string"),
            "{query}: {}",
            res.text
        );
    }
}

/// The telemetry endpoints take at most 16 KiB.
const TELEMETRY: &str = "/api/v1/telemetry/errors";

fn oversized_report() -> Vec<u8> {
    serde_json::to_vec(&json!({ "message": "x".repeat(17 * 1024) })).unwrap()
}

#[tokio::test]
async fn a_declared_body_over_the_limit_is_payload_too_large_naming_the_limit() {
    let app = TestApp::spawn().await;
    let res = app
        .client
        .post(app.url(TELEMETRY))
        .header("content-type", "application/json")
        .body(oversized_report())
        .send()
        .await
        .unwrap();
    let res = test_support::TestResponse::from_response(res).await;
    assert_error(res.status, &res.body, &res.text, 413, "PAYLOAD_TOO_LARGE");
    assert!(
        res.body["message"]
            .as_str()
            .unwrap()
            .contains("16384 bytes"),
        "{}",
        res.text
    );
}

#[tokio::test]
async fn a_streamed_body_over_the_limit_is_payload_too_large_naming_the_limit() {
    let app = TestApp::spawn().await;
    let chunks: Vec<Result<Vec<u8>, std::io::Error>> = oversized_report()
        .chunks(1024)
        .map(|c| Ok(c.to_vec()))
        .collect();
    // No Content-Length: the limit is found while reading the body.
    let body = reqwest::Body::wrap_stream(futures::stream::iter(chunks));
    let res = app
        .client
        .post(app.url(TELEMETRY))
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    let res = test_support::TestResponse::from_response(res).await;
    assert_error(res.status, &res.body, &res.text, 413, "PAYLOAD_TOO_LARGE");
    assert!(
        res.body["message"]
            .as_str()
            .unwrap()
            .contains("16384 bytes"),
        "{}",
        res.text
    );
}
