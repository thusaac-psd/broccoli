//! Regression tests for kernel audit 2026-11 finding B2: the token-minting
//! endpoints `/auth/cli-token` and `/auth/device-authorize` must not launder a
//! revoked or stale access token into a fresh one.
//!
//! Before the fix both endpoints took the stateless `AuthUser` extractor and
//! re-signed the caller's OLD claims with `iat = now`. The re-signed token then
//! passed `FreshAuthUser`'s `iat >= credentials_changed_at` check, so a user
//! whose admin role had been revoked (or whose password had been reset, or who
//! had been soft-deleted) could mint themselves a working admin token, plus a
//! 7-day refresh token on `/auth/cli-token`.

use broccoli_server_sdk::permissions as perm;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set, Statement,
};
use serde_json::json;
use server::entity::{user, user_role};

use crate::common::{TestApp, routes};

async fn user_id(app: &TestApp, username: &str) -> i32 {
    user::Entity::find()
        .filter(user::Column::Username.eq(username))
        .one(&app.db)
        .await
        .expect("db query failed")
        .expect("user should exist")
        .id
}

/// Simulate a credential/authorization change (password reset, role revoke)
/// strictly after every token minted so far. An hour ahead rules out the
/// same-second tolerance `FreshAuthUser` deliberately allows.
async fn bump_credentials_changed(app: &TestApp, id: i32) {
    app.db
        .execute_raw(Statement::from_sql_and_values(
            app.db.get_database_backend(),
            r#"UPDATE "user" SET credentials_changed_at = now() + interval '1 hour' WHERE id = $1"#,
            [id.into()],
        ))
        .await
        .expect("bump credentials_changed_at");
}

/// Replace a user's roles directly in the DB WITHOUT bumping
/// `credentials_changed_at`, so their existing access token still counts as
/// fresh. This stands in for a change landing in the same second the token was
/// minted, which `FreshAuthUser`'s strict `<` tolerates by design. A minted
/// token must still reflect the new roles rather than the presented claims.
async fn replace_roles_without_bump(app: &TestApp, id: i32, role: &str) {
    user_role::Entity::delete_many()
        .filter(user_role::Column::UserId.eq(id))
        .exec(&app.db)
        .await
        .expect("clear user roles");
    user_role::ActiveModel {
        user_id: Set(id),
        role: Set(role.to_string()),
    }
    .insert(&app.db)
    .await
    .expect("insert user role");
}

/// Roles and permissions a token carries, as reported by `/auth/me` (which
/// echoes the JWT claims).
async fn token_grants(app: &TestApp, token: &str) -> (serde_json::Value, Vec<String>) {
    let me = app.get_with_token(routes::ME, token).await;
    assert_eq!(me.status, 200, "/me with minted token: {}", me.text);
    let permissions = me.body["permissions"]
        .as_array()
        .expect("permissions array")
        .iter()
        .map(|p| p.as_str().expect("permission string").to_string())
        .collect();
    (me.body["roles"].clone(), permissions)
}

/// Start a device-flow login and return `(device_code, user_code)`.
async fn start_device_flow(app: &TestApp) -> (String, String) {
    let res = app
        .post_without_token(routes::DEVICE_CODE, &json!({}))
        .await;
    assert_eq!(res.status, 200, "device-code: {}", res.text);
    (
        res.body["device_code"].as_str().unwrap().to_string(),
        res.body["user_code"].as_str().unwrap().to_string(),
    )
}

#[tokio::test]
async fn cli_token_refuses_a_stale_access_token() {
    let app = TestApp::spawn().await;
    let token = app
        .create_user_with_role("stale_admin", "securepass", "admin")
        .await;
    let id = user_id(&app, "stale_admin").await;
    bump_credentials_changed(&app, id).await;

    // The token is still accepted by the stateless read path, so it is the
    // freshness check (not plain signature/expiry) that must reject it below.
    let me = app.get_with_token(routes::ME, &token).await;
    assert_eq!(
        me.status, 200,
        "stale token is otherwise valid: {}",
        me.text
    );

    let res = app
        .post_with_token(routes::CLI_TOKEN, &json!({}), &token)
        .await;
    assert_eq!(
        res.status, 401,
        "cli-token must not re-sign a token older than credentials_changed_at: {}",
        res.text
    );
    assert_eq!(res.body["code"], "TOKEN_INVALID");
}

#[tokio::test]
async fn cli_token_refuses_a_deleted_user() {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_role("deleter", "securepass", "admin")
        .await;
    let victim_token = app
        .create_authenticated_user("deleted_user", "securepass")
        .await;
    let victim_id = user_id(&app, "deleted_user").await;

    let del = app
        .delete_with_token(&routes::user(victim_id), &admin)
        .await;
    assert_eq!(del.status, 204, "soft-delete user: {}", del.text);

    let res = app
        .post_with_token(routes::CLI_TOKEN, &json!({}), &victim_token)
        .await;
    assert_eq!(
        res.status, 401,
        "cli-token must not mint tokens for a soft-deleted user: {}",
        res.text
    );
    assert_eq!(res.body["code"], "TOKEN_INVALID");
}

#[tokio::test]
async fn cli_token_reissues_from_current_roles_not_the_presented_claims() {
    let app = TestApp::spawn().await;
    let admin_token = app
        .create_user_with_role("demoted", "securepass", "admin")
        .await;
    let id = user_id(&app, "demoted").await;
    replace_roles_without_bump(&app, id, "contestant").await;

    let res = app
        .post_with_token(routes::CLI_TOKEN, &json!({}), &admin_token)
        .await;
    assert_eq!(res.status, 200, "cli-token: {}", res.text);

    let (roles, permissions) = token_grants(&app, res.body["token"].as_str().unwrap()).await;
    assert_eq!(roles, json!(["contestant"]));
    assert!(
        !permissions.iter().any(|p| p == perm::USER_MANAGE),
        "minted token must not carry the revoked admin permissions: {permissions:?}"
    );

    // The refresh token issued alongside it keeps working for the CLI.
    let refreshed = app
        .post_without_token(
            routes::CLI_REFRESH,
            &json!({ "refresh_token": res.body["refresh_token"] }),
        )
        .await;
    assert_eq!(refreshed.status, 200, "cli-refresh: {}", refreshed.text);
}

#[tokio::test]
async fn device_authorize_refuses_a_stale_access_token() {
    let app = TestApp::spawn().await;
    let token = app
        .create_user_with_role("stale_device_admin", "securepass", "admin")
        .await;
    let id = user_id(&app, "stale_device_admin").await;
    bump_credentials_changed(&app, id).await;

    let (device_code, user_code) = start_device_flow(&app).await;

    let res = app
        .post_with_token(
            routes::DEVICE_AUTHORIZE,
            &json!({ "user_code": user_code }),
            &token,
        )
        .await;
    assert_eq!(
        res.status, 401,
        "device-authorize must not re-sign a token older than credentials_changed_at: {}",
        res.text
    );
    assert_eq!(res.body["code"], "TOKEN_INVALID");

    // Nothing was attached to the pending code, so the polling device gets no token.
    let poll = app
        .post_without_token(routes::DEVICE_TOKEN, &json!({ "device_code": device_code }))
        .await;
    assert_eq!(poll.status, 400, "device-token: {}", poll.text);
    assert_eq!(poll.body["error"], "authorization_pending");
}

#[tokio::test]
async fn device_authorize_issues_a_token_from_current_roles() {
    let app = TestApp::spawn().await;
    let admin_token = app
        .create_user_with_role("device_demoted", "securepass", "admin")
        .await;
    let id = user_id(&app, "device_demoted").await;
    replace_roles_without_bump(&app, id, "contestant").await;

    let (device_code, user_code) = start_device_flow(&app).await;
    let res = app
        .post_with_token(
            routes::DEVICE_AUTHORIZE,
            &json!({ "user_code": user_code }),
            &admin_token,
        )
        .await;
    assert_eq!(res.status, 200, "device-authorize: {}", res.text);

    let poll = app
        .post_without_token(routes::DEVICE_TOKEN, &json!({ "device_code": device_code }))
        .await;
    assert_eq!(poll.status, 200, "device-token: {}", poll.text);

    let (roles, permissions) = token_grants(&app, poll.body["token"].as_str().unwrap()).await;
    assert_eq!(roles, json!(["contestant"]));
    assert!(
        !permissions.iter().any(|p| p == perm::USER_MANAGE),
        "device token must not carry the revoked admin permissions: {permissions:?}"
    );
}
