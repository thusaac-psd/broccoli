//! While the database cannot be reached, every API call answers a retryable
//! `503 DATABASE_UNAVAILABLE` with `Retry-After`, the way `/healthz` reports
//! it, so clients wait and retry instead of reporting a server bug.

use serde_json::json;

use crate::common::{TestApp, routes};

fn assert_unavailable(what: &str, res: &crate::common::TestResponse) {
    assert_eq!(res.status, 503, "{what}: {}", res.text);
    assert_eq!(
        res.body["code"], "DATABASE_UNAVAILABLE",
        "{what}: {}",
        res.text
    );
    assert_eq!(
        res.headers.get("retry-after").and_then(|v| v.to_str().ok()),
        Some(
            server::error::DATABASE_RETRY_AFTER_SECS
                .to_string()
                .as_str()
        ),
        "{what}"
    );
}

#[tokio::test]
async fn api_calls_answer_a_database_outage_as_retryable() {
    let app = TestApp::spawn().await;
    let token = app.create_authenticated_user("dana", "securepass").await;
    let cli = app
        .post_with_token(routes::CLI_TOKEN, &json!({}), &token)
        .await;
    assert_eq!(cli.status, 200, "{}", cli.text);
    let refresh_token = cli.body["refresh_token"].as_str().unwrap().to_string();

    // Every clone shares one pool, so this takes the server's DB away.
    app.db.clone().close().await.expect("close pool");

    assert_unavailable(
        "contest list",
        &app.get_with_token("/api/v1/contests", &token).await,
    );
    assert_unavailable("me", &app.get_with_token(routes::ME, &token).await);
    assert_unavailable(
        "cli refresh",
        &app.post_without_token(
            routes::CLI_REFRESH,
            &json!({ "refresh_token": refresh_token }),
        )
        .await,
    );
}
