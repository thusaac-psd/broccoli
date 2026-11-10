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
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Statement};
use serde_json::json;
use server::entity::{idempotency_key, user};

use crate::common::{TestApp, routes};

const CLI_TOKEN: &str = "/api/v1/auth/cli-token";

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

/// BUG (high): `/auth/cli-token` takes the stateless `AuthUser` (signature +
/// exp only) and re-signs the OLD token's roles/permissions with `iat = now`.
/// The new token then passes `FreshAuthUser`'s `iat >= credentials_changed_at`
/// check, so a token revoked by a role change / password reset is laundered
/// into a "fresh" one, plus a brand-new 7-day refresh token. Chaining it
/// before each expiry keeps a revoked session alive indefinitely.
/// `/auth/device-authorize` has the same shape.
/// Location: handlers/auth.rs `issue_cli_token`, `authorize_device`.
#[tokio::test]
#[ignore = "BUG: /auth/cli-token re-signs a revoked token's claims with a fresh iat"]
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

/// BUG (high): same root cause, soft-deleted user. The deleted account keeps
/// minting fresh tokens + refresh tokens.
#[tokio::test]
#[ignore = "BUG: a soft-deleted user's token can still mint fresh tokens via /auth/cli-token"]
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

/// BUG (high): `assign_role`/`revoke_role` enforce a privilege ceiling
/// (`require_role_within_ceiling`: "you cannot grant or revoke what you do
/// not hold") but `update_user` and `delete_user` only check USER_MANAGE. A
/// USER_MANAGE-only principal can reset an admin's password and log in as
/// them, or delete every admin.
/// Location: handlers/user.rs `update_user`, `delete_user`.
#[tokio::test]
#[ignore = "BUG: USER_MANAGE-only principal can reset an admin's password (no privilege ceiling)"]
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
#[ignore = "BUG: USER_MANAGE-only principal can delete an admin (no privilege ceiling)"]
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

/// BUG (med): `update_user` applies `username` raw: no trim/length/charset
/// validation and no uniqueness pre-check, so a duplicate surfaces as a DB
/// error -> 500 INTERNAL_ERROR (the create path maps it to 409
/// USERNAME_TAKEN). The password is not length-checked either.
/// Location: handlers/user.rs `update_user`.
#[tokio::test]
#[ignore = "BUG: update_user with a taken username returns 500 instead of 409 USERNAME_TAKEN"]
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
#[ignore = "BUG: update_user accepts usernames/passwords that registration rejects"]
async fn update_user_validates_like_registration() {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_role("aa_admin", "password123", "admin")
        .await;
    app.create_authenticated_user("aa_carol", "password123")
        .await;
    let carol = user_id(&app, "aa_carol").await;

    for body in [
        json!({"password": "x"}),
        json!({"username": " padded"}),
        json!({"username": "has space!"}),
    ] {
        let res = app
            .patch_with_token(&routes::user(carol), &body, &admin)
            .await;
        assert_eq!(res.status, 400, "{body} accepted: {}", res.text);
    }
}

/// BUG (med): assigning a role the user already holds (every new user holds
/// `contestant` via DEFAULT_ROLES) inserts a duplicate composite PK -> 500.
/// Location: handlers/user.rs `assign_role`.
#[tokio::test]
#[ignore = "BUG: assigning an already-held role returns 500"]
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
    assert!(
        res.status < 500,
        "duplicate role assignment -> {}: {}",
        res.status,
        res.text
    );
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

/// DESIGN BUG (med, policy to confirm): `seed_role_permissions` re-inserts
/// every DEFAULT_MAPPINGS row on every boot (`ON CONFLICT DO NOTHING`), so an
/// admin's deliberate revoke of a built-in permission is silently undone at
/// the next restart. Either persist the revoke or reject edits to built-ins.
/// Location: seed.rs, called unconditionally from runtime.rs.
#[tokio::test]
#[ignore = "BUG: revoking a built-in role permission is silently undone on restart (re-seed)"]
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
