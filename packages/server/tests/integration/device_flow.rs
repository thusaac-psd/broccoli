//! Device-flow login (RFC 8628). Pending codes live in the database, so the
//! request, the browser's approval and the CLI's polls may each be served by
//! a different replica.

use sea_orm::{ConnectionTrait, EntityTrait};
use serde_json::json;
use server::entity::device_authorization;

use crate::common::{TestApp, routes};

async fn start(app: &TestApp) -> (String, String) {
    let res = app
        .post_without_token(routes::DEVICE_CODE, &json!({}))
        .await;
    assert_eq!(res.status, 200, "device-code: {}", res.text);
    (
        res.body["device_code"].as_str().unwrap().to_string(),
        res.body["user_code"].as_str().unwrap().to_string(),
    )
}

async fn poll(app: &TestApp, device_code: &str) -> crate::common::TestResponse {
    app.post_without_token(routes::DEVICE_TOKEN, &json!({ "device_code": device_code }))
        .await
}

#[tokio::test]
async fn pending_codes_are_shared_state_not_process_memory() {
    let app = TestApp::spawn().await;
    let (device_code, user_code) = start(&app).await;

    let rows = device_authorization::Entity::find()
        .all(&app.db)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.user_code, user_code.replace('-', ""));
    assert_ne!(
        row.device_code_hash, device_code,
        "the bearer device code must not be stored"
    );
    assert_eq!(row.approved_by, None);
}

#[tokio::test]
async fn full_flow_pending_then_token_then_consumed() {
    let app = TestApp::spawn().await;
    let token = app
        .create_authenticated_user("dev_alice", "password123")
        .await;
    let (device_code, user_code) = start(&app).await;

    let first = poll(&app, &device_code).await;
    assert_eq!(first.status, 400);
    assert_eq!(first.body["error"], "authorization_pending");

    // Polling faster than the interval is refused.
    let fast = poll(&app, &device_code).await;
    assert_eq!(fast.body["error"], "slow_down");

    // Entered with lower case and without the dash.
    let auth = app
        .post_with_token(
            routes::DEVICE_AUTHORIZE,
            &json!({ "user_code": user_code.replace('-', "").to_lowercase() }),
            &token,
        )
        .await;
    assert_eq!(auth.status, 200, "authorize: {}", auth.text);

    let again = app
        .post_with_token(
            routes::DEVICE_AUTHORIZE,
            &json!({ "user_code": user_code }),
            &token,
        )
        .await;
    assert_eq!(again.status, 409, "second authorize: {}", again.text);

    app.db
        .execute_unprepared("UPDATE device_authorization SET last_poll_at = NULL")
        .await
        .unwrap();
    let granted = poll(&app, &device_code).await;
    assert_eq!(granted.status, 200, "poll: {}", granted.text);
    let me = app
        .get_with_token(routes::ME, granted.body["token"].as_str().unwrap())
        .await;
    assert_eq!(me.body["username"], "dev_alice");

    let consumed = poll(&app, &device_code).await;
    assert_eq!(consumed.body["error"], "expired_token");
}

#[tokio::test]
async fn expiry_is_judged_on_the_database_clock() {
    let app = TestApp::spawn().await;
    let token = app
        .create_authenticated_user("dev_bob", "password123")
        .await;
    let (device_code, user_code) = start(&app).await;
    app.db
        .execute_unprepared(
            "UPDATE device_authorization SET expires_at = now() - interval '1 second'",
        )
        .await
        .unwrap();

    let auth = app
        .post_with_token(
            routes::DEVICE_AUTHORIZE,
            &json!({ "user_code": user_code }),
            &token,
        )
        .await;
    assert_eq!(auth.status, 404, "expired code authorized: {}", auth.text);

    let res = poll(&app, &device_code).await;
    assert_eq!(res.body["error"], "expired_token");

    // A new request purges the expired row.
    start(&app).await;
    let rows = device_authorization::Entity::find()
        .all(&app.db)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
}

#[tokio::test]
async fn unknown_code_is_not_found() {
    let app = TestApp::spawn().await;
    let token = app
        .create_authenticated_user("dev_carol", "password123")
        .await;
    let res = app
        .post_with_token(
            routes::DEVICE_AUTHORIZE,
            &json!({ "user_code": "BBBB-CCCC" }),
            &token,
        )
        .await;
    assert_eq!(res.status, 404);
    assert_eq!(poll(&app, "nope").await.body["error"], "expired_token");
}

/// What ends the browser session that approved a code before the CLI
/// collects it.
#[derive(Debug, Clone, Copy)]
enum SessionEnd {
    Logout,
    PasswordReset,
    RoleGrant,
    AccountDeletion,
}

/// An approval belongs to the session that approved it: once that session
/// ends, polling the code yields no token, however it ended.
#[tokio::test]
async fn an_approval_ends_with_the_session_that_approved_it() {
    for end in [
        SessionEnd::Logout,
        SessionEnd::PasswordReset,
        SessionEnd::RoleGrant,
        SessionEnd::AccountDeletion,
    ] {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("dev_admin", "password123", "admin")
            .await;
        let token = app
            .create_authenticated_user("dev_dora", "password123")
            .await;
        let dora = app.get_with_token(routes::ME, &token).await.body["id"]
            .as_i64()
            .unwrap() as i32;
        let (device_code, user_code) = start(&app).await;
        let auth = app
            .post_with_token(
                routes::DEVICE_AUTHORIZE,
                &json!({ "user_code": user_code }),
                &token,
            )
            .await;
        assert_eq!(auth.status, 200, "{end:?}: authorize: {}", auth.text);

        let ended = match end {
            SessionEnd::Logout => {
                app.post_with_token(routes::LOGOUT, &json!({}), &token)
                    .await
            }
            SessionEnd::PasswordReset => {
                app.patch_with_token(
                    &routes::user(dora),
                    &json!({ "password": "brandnewpass" }),
                    &admin,
                )
                .await
            }
            SessionEnd::RoleGrant => {
                app.post_with_token(
                    &routes::user_roles(dora),
                    &json!({ "role": "admin" }),
                    &admin,
                )
                .await
            }
            SessionEnd::AccountDeletion => app.delete_with_token(&routes::user(dora), &admin).await,
        };
        assert!(ended.status < 300, "{end:?}: {}", ended.text);

        let res = poll(&app, &device_code).await;
        assert_eq!(
            res.status, 400,
            "{end:?}: the approval outlived its session: {}",
            res.text
        );
        assert_eq!(res.body["error"], "expired_token", "{end:?}");
        assert!(
            device_authorization::Entity::find()
                .all(&app.db)
                .await
                .unwrap()
                .is_empty(),
            "{end:?}: the approval is deleted with its session"
        );
    }
}
