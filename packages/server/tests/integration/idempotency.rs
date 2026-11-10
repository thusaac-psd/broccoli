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
        assert_eq!(replay["code"], "CONFLICT");
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
