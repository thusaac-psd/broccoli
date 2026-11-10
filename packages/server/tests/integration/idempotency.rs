//! Idempotency-Key claim lifecycle: stale-pending reclaim judged by the
//! database clock, and release of the key on error responses.

use sea_orm::{ConnectionTrait, EntityTrait};
use serde_json::json;
use server::entity::idempotency_key;

use crate::common::{TestApp, routes};

fn problem_body(title: &str) -> serde_json::Value {
    json!({
        "title": title,
        "content": "## Description\nSolve this.",
        "time_limit": 1000,
        "memory_limit": 262144,
        "problem_type": "standard",
        "default_contest_type": "standard",
        "checker_format": "exact",
        "is_public": true,
    })
}

async fn post_problem(
    app: &TestApp,
    token: &str,
    key: &str,
    body: &serde_json::Value,
) -> reqwest::Response {
    app.client
        .post(format!("http://{}{}", app.addr, routes::PROBLEMS))
        .header("Authorization", format!("Bearer {token}"))
        .header("Idempotency-Key", key)
        .json(body)
        .send()
        .await
        .expect("send")
}

/// Seed a `pending` claim aged `age_secs` by the DATABASE clock, as if a
/// request on another replica had claimed it and then died.
async fn seed_pending(app: &TestApp, key: &str, user_id: i32, age_secs: i64) {
    app.db
        .execute_unprepared(&format!(
            "INSERT INTO idempotency_key (key, user_id, request_path, request_method, status, created_at) \
             VALUES ('{key}', {user_id}, '/v1/problems', 'POST', 'pending', NOW() - INTERVAL '{age_secs} seconds')"
        ))
        .await
        .expect("seed pending key");
}

#[tokio::test]
async fn fresh_pending_key_is_in_progress() {
    let app = TestApp::spawn().await;
    let token = app
        .create_user_with_role("idem_admin", "password123", "admin")
        .await;
    let user_id = app.get_with_token(routes::ME, &token).await.id();
    seed_pending(&app, "idem-fresh", user_id, 5).await;

    let res = post_problem(&app, &token, "idem-fresh", &problem_body("P")).await;
    assert_eq!(res.status().as_u16(), 409);
    let body: serde_json::Value = res.json().await.expect("json");
    assert_eq!(body["code"], "IDEMPOTENCY_KEY_IN_PROGRESS");
}

#[tokio::test]
async fn stale_pending_key_is_reclaimed_and_completed() {
    let app = TestApp::spawn().await;
    let token = app
        .create_user_with_role("idem_admin", "password123", "admin")
        .await;
    let user_id = app.get_with_token(routes::ME, &token).await.id();
    seed_pending(&app, "idem-stale", user_id, 600).await;

    let res = post_problem(&app, &token, "idem-stale", &problem_body("P")).await;
    assert_eq!(res.status().as_u16(), 201);
    let first: serde_json::Value = res.json().await.expect("json");

    let row = idempotency_key::Entity::find_by_id(("idem-stale".to_string(), user_id))
        .one(&app.db)
        .await
        .expect("db")
        .expect("row");
    assert_eq!(row.status, "completed");
    assert!(row.request_fingerprint.is_some());

    let replay = post_problem(&app, &token, "idem-stale", &problem_body("P")).await;
    assert_eq!(replay.status().as_u16(), 201);
    let replay: serde_json::Value = replay.json().await.expect("json");
    assert_eq!(replay["id"], first["id"]);
}

#[tokio::test]
async fn error_response_releases_the_key() {
    let app = TestApp::spawn().await;
    let token = app
        .create_user_with_role("idem_admin", "password123", "admin")
        .await;
    let user_id = app.get_with_token(routes::ME, &token).await.id();

    let res = post_problem(&app, &token, "idem-error", &json!({"title": ""})).await;
    assert!(res.status().is_client_error(), "{}", res.status());
    let row = idempotency_key::Entity::find_by_id(("idem-error".to_string(), user_id))
        .one(&app.db)
        .await
        .expect("db");
    assert!(row.is_none(), "error response left the key behind: {row:?}");

    // The same key is free for a corrected request.
    let res = post_problem(&app, &token, "idem-error", &problem_body("P")).await;
    assert_eq!(res.status().as_u16(), 201);
}
