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

/// A keyed POST must not make the server buffer an arbitrarily
/// large body. A declared `Content-Length` over the bound is refused before
/// any of the body is read, so this request (which never sends its body)
/// gets an immediate 413 instead of hanging while the server waits to buffer.
#[tokio::test]
async fn oversized_keyed_body_is_refused_before_buffering() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let app = TestApp::spawn().await;
    let token = app
        .create_user_with_role("idem_admin", "password123", "admin")
        .await;

    let mut stream = tokio::net::TcpStream::connect(app.addr)
        .await
        .expect("connect");
    let head = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {token}\r\n\
         Idempotency-Key: idem-huge\r\nContent-Type: application/json\r\n\
         Content-Length: 1000000000\r\n\r\n{{",
        routes::PROBLEMS,
        app.addr
    );
    stream.write_all(head.as_bytes()).await.expect("write");

    let mut response = vec![0u8; 512];
    let read = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        stream.read(&mut response),
    )
    .await
    .expect("server waited for the body instead of refusing it")
    .expect("read");
    let response = String::from_utf8_lossy(&response[..read]);
    assert!(response.starts_with("HTTP/1.1 413"), "{response}");
}

/// A router behind the real idempotency middleware whose handlers count how
/// often they ran: `/fail` commits (counts) and then answers 500, `/hang`
/// commits and never answers, `/reject` answers 400 without committing.
fn outcome_router(
    app: &TestApp,
    runs: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> axum::Router {
    use axum::http::StatusCode;
    use axum::routing::post;
    use std::sync::atomic::Ordering;

    let fail_runs = runs.clone();
    let hang_runs = runs;
    axum::Router::new()
        .route(
            "/fail",
            post(move || {
                let runs = fail_runs.clone();
                async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                    StatusCode::INTERNAL_SERVER_ERROR
                }
            }),
        )
        .route(
            "/hang",
            post(move || {
                let runs = hang_runs.clone();
                async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                    std::future::pending::<StatusCode>().await
                }
            }),
        )
        .route("/reject", post(|| async { StatusCode::BAD_REQUEST }))
        .layer(axum::middleware::from_fn_with_state(
            app.state.clone(),
            server::middleware::idempotency_middleware,
        ))
}

fn keyed_post(path: &str, token: &str, key: &str) -> axum::http::Request<axum::body::Body> {
    axum::http::Request::post(path)
        .header("Authorization", format!("Bearer {token}"))
        .header("Idempotency-Key", key)
        .body(axum::body::Body::from("{}"))
        .unwrap()
}

async fn key_status(app: &TestApp, key: &str, user_id: i32) -> Option<String> {
    idempotency_key::Entity::find_by_id((key.to_string(), user_id))
        .one(&app.db)
        .await
        .expect("db")
        .map(|row| row.status)
}

async fn error_code(response: axum::response::Response) -> serde_json::Value {
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    serde_json::from_slice::<serde_json::Value>(&body).unwrap()["code"].clone()
}

/// Once the handler has run, a 5xx proves nothing about
/// whether it committed, so the key must not be released for a re-execution.
#[tokio::test]
async fn server_error_after_the_handler_ran_does_not_allow_re_execution() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;

    let app = TestApp::spawn().await;
    let token = app
        .create_user_with_role("idem_admin", "password123", "admin")
        .await;
    let user_id = app.get_with_token(routes::ME, &token).await.id();
    let runs = std::sync::Arc::new(AtomicUsize::new(0));
    let router = outcome_router(&app, runs.clone());

    let first = router
        .clone()
        .oneshot(keyed_post("/fail", &token, "idem-5xx"))
        .await
        .unwrap();
    assert_eq!(first.status().as_u16(), 500);
    assert_eq!(
        key_status(&app, "idem-5xx", user_id).await.as_deref(),
        Some("outcome_unknown")
    );

    let retry = router
        .oneshot(keyed_post("/fail", &token, "idem-5xx"))
        .await
        .unwrap();
    assert_eq!(retry.status().as_u16(), 409);
    assert_eq!(error_code(retry).await, "IDEMPOTENCY_OUTCOME_UNKNOWN");
    assert_eq!(runs.load(Ordering::SeqCst), 1, "the handler ran twice");
}

/// A request cancelled mid-handler (client disconnect, an
/// outer timeout) may have committed, so its key is settled as unknown too.
#[tokio::test]
async fn cancelled_request_does_not_allow_re_execution() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;

    let app = TestApp::spawn().await;
    let token = app
        .create_user_with_role("idem_admin", "password123", "admin")
        .await;
    let user_id = app.get_with_token(routes::ME, &token).await.id();
    let runs = std::sync::Arc::new(AtomicUsize::new(0));
    let router = outcome_router(&app, runs.clone());

    let cancelled = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        router
            .clone()
            .oneshot(keyed_post("/hang", &token, "idem-cancel")),
    )
    .await;
    assert!(cancelled.is_err(), "the hanging handler answered");
    assert_eq!(runs.load(Ordering::SeqCst), 1);

    // The drop guard settles the key from a spawned task.
    let mut status = None;
    for _ in 0..50 {
        status = key_status(&app, "idem-cancel", user_id).await;
        if status.as_deref() != Some("pending") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(status.as_deref(), Some("outcome_unknown"));

    let retry = router
        .oneshot(keyed_post("/hang", &token, "idem-cancel"))
        .await
        .unwrap();
    assert_eq!(retry.status().as_u16(), 409);
    assert_eq!(runs.load(Ordering::SeqCst), 1, "the handler ran twice");
}

/// A 4xx proves the request was refused before committing: the key is freed.
#[tokio::test]
async fn client_error_releases_the_key_for_a_corrected_retry() {
    use std::sync::atomic::AtomicUsize;
    use tower::ServiceExt;

    let app = TestApp::spawn().await;
    let token = app
        .create_user_with_role("idem_admin", "password123", "admin")
        .await;
    let user_id = app.get_with_token(routes::ME, &token).await.id();
    let router = outcome_router(&app, std::sync::Arc::new(AtomicUsize::new(0)));

    let res = router
        .oneshot(keyed_post("/reject", &token, "idem-4xx"))
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 400);
    assert_eq!(key_status(&app, "idem-4xx", user_id).await, None);
}

/// The query string is part of the request a key is bound to,
/// so reusing a key with a different query is a mismatch, not a replay.
#[tokio::test]
async fn reusing_a_key_with_a_different_query_is_a_mismatch() {
    let app = TestApp::spawn().await;
    let token = app
        .create_user_with_role("idem_admin", "password123", "admin")
        .await;
    let send = |query: &'static str| {
        app.client
            .post(format!("http://{}{}?{query}", app.addr, routes::PROBLEMS))
            .header("Authorization", format!("Bearer {token}"))
            .header("Idempotency-Key", "idem-query")
            .json(&problem_body("P"))
            .send()
    };
    let first = send("target=a").await.expect("send");
    assert_eq!(first.status().as_u16(), 201);

    let second = send("target=b").await.expect("send");
    assert_eq!(second.status().as_u16(), 422);
    let second: serde_json::Value = second.json().await.expect("json");
    assert_eq!(second["code"], "IDEMPOTENCY_KEY_MISMATCH");

    let replay = send("target=a").await.expect("send");
    assert_eq!(replay.status().as_u16(), 201);
}

mod response_binding {
    use super::*;
    use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};
    use server::entity::{idempotency_key, submission};

    /// A response that carries a generated password is marked `no-store`: the
    /// idempotency cache records only that the request completed, and a replay
    /// answers 409 instead of re-sending the secret or creating the account twice.
    #[tokio::test]
    async fn created_user_password_is_not_persisted_by_idempotency_cache() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("idem_admin", "password123", "admin")
            .await;
        let send = || {
            app.client
                .post(format!("http://{}{}", app.addr, routes::USERS))
                .header("Authorization", format!("Bearer {admin}"))
                .header("Idempotency-Key", "create-user-1")
                .json(&json!({"username": "idem_newbie"}))
                .send()
        };
        let res = send().await.expect("send");
        assert_eq!(res.status().as_u16(), 201);
        assert_eq!(
            res.headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some("no-store")
        );
        let body: serde_json::Value = res.json().await.expect("json");
        let password = body["password"].as_str().expect("generated password");

        let cached: Vec<String> = idempotency_key::Entity::find()
            .all(&app.db)
            .await
            .expect("db")
            .into_iter()
            .filter_map(|k| k.response_body)
            .collect();
        assert!(
            cached.iter().all(|b| !b.contains(password)),
            "plaintext password found in idempotency_key.response_body"
        );

        let replay = send().await.expect("send");
        assert_eq!(replay.status().as_u16(), 409);
        let replay: serde_json::Value = replay.json().await.expect("json");
        assert_eq!(replay["code"], "IDEMPOTENCY_RESPONSE_NOT_STORED");
        assert!(replay.get("password").is_none());
    }

    /// A key is bound to the request body: reusing it with a different body is a
    /// 422 mismatch decided before the handler runs, while the identical body
    /// still replays the original response.
    #[tokio::test]
    async fn idempotency_key_reuse_with_different_body_is_rejected() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("idem_admin", "password123", "admin")
            .await;
        let pid = app.create_problem(&token, "P").await;

        let send = |code: &'static str| {
            app.client
            .post(format!(
                "http://{}{}",
                app.addr,
                routes::problem_submissions(pid)
            ))
            .header("Authorization", format!("Bearer {token}"))
            .header("Idempotency-Key", "submit-1")
            .json(&json!({"language": "cpp", "files": [{"filename": "main.cpp", "content": code}]}))
            .send()
        };
        let first = send("int main(){return 0;}").await.expect("send");
        assert_eq!(first.status().as_u16(), 201);
        let first: serde_json::Value = first.json().await.expect("json");

        let second = send("int main(){return 1;}").await.expect("send");
        assert_eq!(second.status().as_u16(), 422);
        let second: serde_json::Value = second.json().await.expect("json");
        assert_eq!(second["code"], "IDEMPOTENCY_KEY_MISMATCH");
        // The mismatch is decided before the handler runs: nothing was submitted.
        let submitted = submission::Entity::find()
            .filter(submission::Column::ProblemId.eq(pid))
            .count(&app.db)
            .await
            .expect("db");
        assert_eq!(submitted, 1, "a mismatched request must not execute");

        // The identical body still replays the original submission.
        let replay = send("int main(){return 0;}").await.expect("send");
        assert_eq!(replay.status().as_u16(), 201);
        let replay: serde_json::Value = replay.json().await.expect("json");
        assert_eq!(replay["id"], first["id"]);
    }
}
