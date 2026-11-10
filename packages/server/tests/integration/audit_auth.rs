//! Auth / user-management defects found by the kernel audit (Nov 2026).
//!
//! Each `#[ignore]`d test reproduces a bug and asserts the CORRECT behaviour,
//! so it fails today. Run the whole set with:
//!
//! `cargo test -p server --test integration audit_auth -- --ignored`
//!
//! When fixing a bug, delete its `#[ignore]` so the test becomes a regression
//! guard.

use broccoli_server_sdk::permissions as perm;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set, Statement,
};
use serde_json::json;
use server::entity::{idempotency_key, user, user_role};

use crate::common::{TestApp, routes};

use crate::common::routes::CLI_TOKEN;

async fn user_id(app: &TestApp, username: &str) -> i32 {
    user::Entity::find()
        .filter(user::Column::Username.eq(username))
        .one(&app.db)
        .await
        .expect("db")
        .expect("user exists")
        .id
}

/// Make every token minted so far for `uid` stale, exactly as a role revoke /
/// password reset does (`touch_credentials_changed`); an hour ahead removes
/// same-second flakiness. Mirrors auth.rs `stale_access_token_*`.
async fn invalidate_sessions(app: &TestApp, uid: i32) {
    app.db
        .execute_raw(Statement::from_string(
            app.db.get_database_backend(),
            format!(
                "UPDATE \"user\" SET credentials_changed_at = now() + interval '1 hour' WHERE id = {uid}"
            ),
        ))
        .await
        .expect("bump credentials_changed_at");
}

/// Regression guard for #53. `/auth/cli-token` used to take the stateless
/// `AuthUser` (signature + exp only) and re-sign the OLD token's roles/permissions with `iat = now`.
/// The new token then passes `FreshAuthUser`'s `iat >= credentials_changed_at`
/// check, so a token revoked by a role change / password reset is laundered
/// into a "fresh" one, plus a brand-new 7-day refresh token. Chaining it
/// before each expiry keeps a revoked session alive indefinitely.
/// `/auth/device-authorize` has the same shape.
/// Location: handlers/auth.rs `issue_cli_token`, `authorize_device`.
#[tokio::test]
async fn cli_token_refuses_a_stale_access_token() {
    let app = TestApp::spawn().await;
    let token = app
        .create_user_with_role("aa_admin", "password123", "admin")
        .await;
    let uid = user_id(&app, "aa_admin").await;
    invalidate_sessions(&app, uid).await;

    // Control: the stale token is already refused on a guarded mutation.
    let guarded = app
        .post_with_token(
            &routes::user_roles(uid),
            &json!({"role": "contestant"}),
            &token,
        )
        .await;
    assert_eq!(guarded.status, 401, "control: {}", guarded.text);

    let res = app.post_with_token(CLI_TOKEN, &json!({}), &token).await;
    assert_eq!(
        res.status, 401,
        "stale token was exchanged for a fresh one: {}",
        res.text
    );
}

/// Regression guard for #53: same root cause, soft-deleted user. The deleted
/// account kept minting fresh tokens + refresh tokens.
#[tokio::test]
async fn cli_token_refuses_a_deleted_user() {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_role("aa_admin", "password123", "admin")
        .await;
    let victim = app
        .create_user_with_role("aa_gone", "password123", "contestant")
        .await;
    let vid = user_id(&app, "aa_gone").await;
    let del = app.delete_with_token(&routes::user(vid), &admin).await;
    assert_eq!(del.status, 204, "{}", del.text);

    let res = app.post_with_token(CLI_TOKEN, &json!({}), &victim).await;
    assert_eq!(res.status, 401, "deleted user minted a token: {}", res.text);
}

/// Replace a user's roles directly in the DB WITHOUT bumping
/// `credentials_changed_at`, so their existing access token still counts as
/// fresh. Stands in for a change landing in the same second the token was
/// minted, which `FreshAuthUser`'s strict `<` tolerates by design: a minted
/// token must still reflect the new roles rather than the presented claims.
async fn replace_roles_without_bump(app: &TestApp, uid: i32, role: &str) {
    user_role::Entity::delete_many()
        .filter(user_role::Column::UserId.eq(uid))
        .exec(&app.db)
        .await
        .expect("clear user roles");
    user_role::ActiveModel {
        user_id: Set(uid),
        role: Set(role.to_string()),
    }
    .insert(&app.db)
    .await
    .expect("insert user role");
}

/// Roles and permissions a token carries, as `/auth/me` echoes its claims.
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
async fn cli_token_reissues_from_current_roles_not_the_presented_claims() {
    let app = TestApp::spawn().await;
    let admin_token = app
        .create_user_with_role("aa_demoted", "password123", "admin")
        .await;
    let uid = user_id(&app, "aa_demoted").await;
    replace_roles_without_bump(&app, uid, "contestant").await;

    let res = app
        .post_with_token(CLI_TOKEN, &json!({}), &admin_token)
        .await;
    assert_eq!(res.status, 200, "cli-token: {}", res.text);

    let (roles, permissions) = token_grants(&app, res.body["token"].as_str().unwrap()).await;
    assert_eq!(roles, json!(["contestant"]));
    assert!(
        !permissions.iter().any(|p| p == perm::USER_MANAGE),
        "minted token must not carry the revoked admin permissions: {permissions:?}"
    );

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
        .create_user_with_role("aa_stale_dev", "password123", "admin")
        .await;
    let uid = user_id(&app, "aa_stale_dev").await;
    invalidate_sessions(&app, uid).await;

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
        "stale token authorized a device: {}",
        res.text
    );
    assert_eq!(res.body["code"], "TOKEN_INVALID");

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
        .create_user_with_role("aa_dev_demoted", "password123", "admin")
        .await;
    let uid = user_id(&app, "aa_dev_demoted").await;
    replace_roles_without_bump(&app, uid, "contestant").await;

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

/// Regression guard for #54. `update_user` and `delete_user` used to check only
/// USER_MANAGE, so a USER_MANAGE-only principal could reset an admin's
/// password and log in as them, or delete every admin. Both now apply
/// `services::privileges::require_within_ceiling`.
#[tokio::test]
async fn user_manager_cannot_take_over_an_admin() {
    let app = TestApp::spawn().await;
    let mgr = app
        .create_user_with_permissions("aa_mgr", "password123", &[perm::USER_MANAGE])
        .await;
    app.create_user_with_role("aa_boss", "password123", "admin")
        .await;
    let boss = user_id(&app, "aa_boss").await;

    let res = app
        .patch_with_token(
            &routes::user(boss),
            &json!({"password": "takeover123"}),
            &mgr,
        )
        .await;
    assert_eq!(
        res.status, 403,
        "user:manage reset an admin's password: {}",
        res.text
    );

    let login = app
        .post_without_token(
            routes::LOGIN,
            &json!({"username": "aa_boss", "password": "takeover123"}),
        )
        .await;
    assert_ne!(login.status, 200, "attacker logged in as the admin");
}

#[tokio::test]
async fn user_manager_cannot_delete_an_admin() {
    let app = TestApp::spawn().await;
    let mgr = app
        .create_user_with_permissions("aa_mgr", "password123", &[perm::USER_MANAGE])
        .await;
    app.create_user_with_role("aa_boss", "password123", "admin")
        .await;
    let boss = user_id(&app, "aa_boss").await;

    let res = app.delete_with_token(&routes::user(boss), &mgr).await;
    assert_eq!(
        res.status, 403,
        "user:manage deleted an admin: {}",
        res.text
    );
}

/// The ceiling allows what the caller does hold: a user manager can still
/// reset a contestant's password and delete a contestant.
#[tokio::test]
async fn user_manager_can_manage_accounts_within_its_ceiling() {
    let app = TestApp::spawn().await;
    let mgr = app
        .create_user_with_permissions(
            "aa_mgr",
            "password123",
            &[perm::USER_MANAGE, perm::SUBMISSION_SUBMIT],
        )
        .await;
    app.create_authenticated_user("aa_pupil", "password123")
        .await;
    let pupil = user_id(&app, "aa_pupil").await;

    let res = app
        .patch_with_token(
            &routes::user(pupil),
            &json!({"password": "newpass123"}),
            &mgr,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    let res = app.delete_with_token(&routes::user(pupil), &mgr).await;
    assert_eq!(res.status, 204, "{}", res.text);
}

/// Regression guard for #67. `update_user` used to store `username` raw (no
/// trim/length/charset validation) and let a duplicate surface as a 500; the
/// password was not length-checked either. It now validates like registration
/// and maps a clash to 409 USERNAME_TAKEN.
#[tokio::test]
async fn update_user_to_taken_username_is_a_conflict() {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_role("aa_admin", "password123", "admin")
        .await;
    app.create_authenticated_user("aa_alice", "password123")
        .await;
    app.create_authenticated_user("aa_bob", "password123").await;
    let bob = user_id(&app, "aa_bob").await;

    let res = app
        .patch_with_token(&routes::user(bob), &json!({"username": "aa_alice"}), &admin)
        .await;
    assert_eq!(res.status, 409, "{}", res.text);
    assert_eq!(res.body["code"], "USERNAME_TAKEN");
}

#[tokio::test]
async fn update_user_validates_like_registration() {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_role("aa_admin", "password123", "admin")
        .await;
    app.create_authenticated_user("aa_carol", "password123")
        .await;
    let carol = user_id(&app, "aa_carol").await;

    for body in [json!({"password": "x"}), json!({"username": "has space!"})] {
        let res = app
            .patch_with_token(&routes::user(carol), &body, &admin)
            .await;
        assert_eq!(res.status, 400, "{body} accepted: {}", res.text);
    }

    // Surrounding whitespace is trimmed, as at registration and login.
    let res = app
        .patch_with_token(
            &routes::user(carol),
            &json!({"username": " aa_carol2 "}),
            &admin,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert_eq!(res.body["username"], "aa_carol2");
    let login = app
        .post_without_token(
            routes::LOGIN,
            &json!({"username": "aa_carol2", "password": "password123"}),
        )
        .await;
    assert_eq!(login.status, 200, "{}", login.text);
}

/// Regression guard for #69. Assigning a role the user already holds (every
/// new user holds `contestant` via DEFAULT_ROLES) used to insert a duplicate
/// composite PK and return 500; it is now an idempotent 200.
#[tokio::test]
async fn assigning_an_already_held_role_is_not_a_server_error() {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_role("aa_admin", "password123", "admin")
        .await;
    app.create_authenticated_user("aa_dave", "password123")
        .await;
    let dave = user_id(&app, "aa_dave").await;

    let res = app
        .post_with_token(
            &routes::user_roles(dave),
            &json!({"role": "contestant"}),
            &admin,
        )
        .await;
    assert_eq!(
        res.status, 200,
        "duplicate role assignment -> {}: {}",
        res.status, res.text
    );

    // A no-op does not end the user's sessions.
    let dave_token = app
        .post_without_token(
            routes::LOGIN,
            &json!({"username": "aa_dave", "password": "password123"}),
        )
        .await
        .body["token"]
        .as_str()
        .unwrap()
        .to_string();
    let again = app
        .post_with_token(
            &routes::user_roles(dave),
            &json!({"role": "contestant"}),
            &admin,
        )
        .await;
    assert_eq!(again.status, 200, "{}", again.text);
    let me = app
        .post_with_token(CLI_TOKEN, &json!({}), &dave_token)
        .await;
    assert_eq!(
        me.status, 200,
        "no-op assignment invalidated the session: {}",
        me.text
    );

    // A malformed body is a structured validation error.
    let malformed = app
        .post_with_token(&routes::user_roles(dave), &json!({"rol": "admin"}), &admin)
        .await;
    assert_eq!(malformed.status, 400, "{}", malformed.text);
    assert_eq!(malformed.body["code"], "VALIDATION_ERROR");
}

/// BUG (med): the idempotency middleware caches the full 2xx body of any POST
/// carrying `Idempotency-Key`. The web client sends that header on user and
/// participant creation, whose responses contain the generated PLAINTEXT
/// password, so credentials sit in `idempotency_key.response_body` for 24h,
/// readable by the plugin DB role (m0003 re-granted SELECT on all tables).
/// Location: middleware/idempotency.rs `complete_key`.
#[tokio::test]
#[ignore = "BUG: generated plaintext passwords are persisted in idempotency_key.response_body"]
async fn created_user_password_is_not_persisted_by_idempotency_cache() {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_role("aa_admin", "password123", "admin")
        .await;
    let res = app
        .client
        .post(format!("http://{}{}", app.addr, routes::USERS))
        .header("Authorization", format!("Bearer {admin}"))
        .header("Idempotency-Key", "audit-create-user-1")
        .json(&json!({"username": "aa_newbie"}))
        .send()
        .await
        .expect("send");
    assert_eq!(res.status().as_u16(), 201);
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
}

/// Regression guard for #68. `seed_role_permissions` used to re-insert every
/// DEFAULT_MAPPINGS row on every boot, silently undoing an admin's revoke of a
/// built-in permission. Seeding now records each mapping in
/// `seeded_role_permission` and applies it once.
#[tokio::test]
async fn revoked_builtin_permission_survives_reseed() {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_role("aa_admin", "password123", "admin")
        .await;
    let res = app
        .delete_with_token(
            &routes::role_permission("contestant", perm::SUBMISSION_SUBMIT),
            &admin,
        )
        .await;
    if res.status == 400 || res.status == 403 || res.status == 409 {
        // Refusing to edit built-in mappings is an acceptable fix.
        return;
    }
    assert!(res.status < 300, "revoke failed: {}", res.text);

    server::seed::seed_role_permissions(&app.db)
        .await
        .expect("reseed (simulated restart)");

    let perms = app
        .get_with_token(&routes::role_permissions("contestant"), &admin)
        .await;
    assert!(
        !perms.text.contains(perm::SUBMISSION_SUBMIT),
        "revoked permission came back after restart: {}",
        perms.text
    );
}

/// A built-in mapping that seeding has never applied (one added by a later
/// release) is still seeded once, then follows the same rule.
#[tokio::test]
async fn builtin_mapping_new_to_this_release_is_seeded_once() {
    use server::entity::{role_permission, seeded_role_permission};

    let app = TestApp::spawn().await;
    // Simulate a database from before this mapping existed.
    for table in ["seeded_role_permission", "role_permission"] {
        app.db
            .execute_raw(Statement::from_string(
                app.db.get_database_backend(),
                format!(
                    "DELETE FROM {table} WHERE role = 'problem_setter' AND permission = '{}'",
                    perm::PROBLEM_EDIT
                ),
            ))
            .await
            .expect("delete mapping");
    }

    server::seed::seed_role_permissions(&app.db)
        .await
        .expect("reseed (upgrade boot)");
    let granted = role_permission::Entity::find_by_id((
        "problem_setter".to_string(),
        perm::PROBLEM_EDIT.to_string(),
    ))
    .one(&app.db)
    .await
    .expect("db");
    assert!(granted.is_some(), "new built-in mapping was not seeded");
    let recorded = seeded_role_permission::Entity::find_by_id((
        "problem_setter".to_string(),
        perm::PROBLEM_EDIT.to_string(),
    ))
    .one(&app.db)
    .await
    .expect("db");
    assert!(recorded.is_some(), "seeding was not recorded");
}
