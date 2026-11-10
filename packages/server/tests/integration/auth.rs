use broccoli_server_sdk::permissions as perm;
use serde_json::json;

use crate::common::{TestApp, TestResponse, routes};

mod registration {
    use super::*;

    #[tokio::test]
    async fn new_user_can_register_with_valid_credentials() {
        let app = TestApp::spawn().await;

        let res = app
            .post_without_token(
                routes::REGISTER,
                &json!({"username": "alice", "password": "securepass"}),
            )
            .await;

        assert_eq!(res.status, 201);
        assert!(res.body["id"].is_number());
        assert_eq!(res.body["username"], "alice");
    }

    #[tokio::test]
    async fn cannot_register_with_an_already_taken_username() {
        let app = TestApp::spawn().await;
        let body = json!({"username": "alice", "password": "securepass"});

        let first = app.post_without_token(routes::REGISTER, &body).await;
        assert_eq!(
            first.status, 201,
            "First registration failed: {}",
            first.text
        );

        let res = app.post_without_token(routes::REGISTER, &body).await;

        assert_eq!(res.status, 409);
        assert_eq!(res.body["code"], "USERNAME_TAKEN");
    }

    #[tokio::test]
    async fn cannot_register_with_a_password_that_is_too_short() {
        let app = TestApp::spawn().await;

        let res = app
            .post_without_token(
                routes::REGISTER,
                &json!({"username": "alice", "password": "short"}),
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn cannot_register_with_a_password_that_is_too_long() {
        let app = TestApp::spawn().await;
        let long_password = "a".repeat(129);

        let res = app
            .post_without_token(
                routes::REGISTER,
                &json!({"username": "alice", "password": long_password}),
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn cannot_register_with_an_invalid_username() {
        let app = TestApp::spawn().await;

        let res = app
            .post_without_token(
                routes::REGISTER,
                &json!({"username": "no spaces!", "password": "securepass"}),
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn cannot_register_with_an_empty_username() {
        let app = TestApp::spawn().await;

        let res = app
            .post_without_token(
                routes::REGISTER,
                &json!({"username": "   ", "password": "securepass"}),
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn cannot_register_with_a_username_that_is_too_long() {
        let app = TestApp::spawn().await;
        let long_name = "a".repeat(33);

        let res = app
            .post_without_token(
                routes::REGISTER,
                &json!({"username": long_name, "password": "securepass"}),
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }
}

mod login {
    use super::*;

    #[tokio::test]
    async fn registered_user_can_login_and_receives_token() {
        let app = TestApp::spawn().await;
        let body = json!({"username": "alice", "password": "securepass"});

        let reg = app.post_without_token(routes::REGISTER, &body).await;
        assert_eq!(reg.status, 201, "Registration failed: {}", reg.text);
        let res = app.post_without_token(routes::LOGIN, &body).await;

        assert_eq!(res.status, 200);
        assert!(res.body["token"].is_string());
        assert_eq!(res.body["username"], "alice");
    }

    #[tokio::test]
    async fn new_user_receives_contestant_role_with_submit_permission() {
        let app = TestApp::spawn().await;
        let body = json!({"username": "alice", "password": "securepass"});

        let reg = app.post_without_token(routes::REGISTER, &body).await;
        assert_eq!(reg.status, 201, "Registration failed: {}", reg.text);
        let res = app.post_without_token(routes::LOGIN, &body).await;

        assert_eq!(res.body["roles"], json!(["contestant"]));
        let permissions = res.body["permissions"]
            .as_array()
            .expect("permissions should be an array");
        assert!(permissions.contains(&json!(perm::SUBMISSION_SUBMIT)));
    }

    #[tokio::test]
    async fn cannot_login_with_wrong_password() {
        let app = TestApp::spawn().await;

        let reg = app
            .post_without_token(
                routes::REGISTER,
                &json!({"username": "alice", "password": "securepass"}),
            )
            .await;
        assert_eq!(reg.status, 201, "Registration failed: {}", reg.text);

        let res = app
            .post_without_token(
                routes::LOGIN,
                &json!({"username": "alice", "password": "wrongpass"}),
            )
            .await;

        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "INVALID_CREDENTIALS");
    }

    #[tokio::test]
    async fn cannot_login_with_nonexistent_username() {
        let app = TestApp::spawn().await;

        let res = app
            .post_without_token(
                routes::LOGIN,
                &json!({"username": "nobody", "password": "securepass"}),
            )
            .await;

        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "INVALID_CREDENTIALS");
    }
}

mod token_refresh {
    use super::*;

    #[tokio::test]
    async fn authenticated_user_can_refresh_access_token() {
        let app = TestApp::spawn().await;
        let body = json!({"username": "alice", "password": "securepass"});
        app.post_without_token(routes::REGISTER, &body).await;

        let login_res = app.post_without_token(routes::LOGIN, &body).await;
        let old_token = login_res.body["token"].as_str().unwrap().to_string();

        let refresh_res = app.post_without_token(routes::REFRESH, &json!({})).await;

        assert_eq!(refresh_res.status, 200);
        assert!(refresh_res.body["token"].is_string());
        assert_ne!(refresh_res.body["token"].as_str().unwrap(), old_token);
    }

    #[tokio::test]
    async fn cannot_refresh_without_cookie() {
        let app = TestApp::spawn().await;

        let res = app.post_without_token(routes::REFRESH, &json!({})).await;

        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "TOKEN_MISSING");
    }

    #[tokio::test]
    async fn refresh_rotates_the_refresh_cookie() {
        fn extract_refresh_cookie(resp: &TestResponse) -> String {
            for hv in resp.headers.get_all("set-cookie").iter() {
                let s = hv.to_str().expect("set-cookie utf-8");
                if let Some(rest) = s.strip_prefix("broccoli_refresh=") {
                    let val = rest.split(';').next().unwrap_or("").to_string();
                    if !val.is_empty() {
                        return val;
                    }
                }
            }
            panic!("no broccoli_refresh Set-Cookie on response");
        }

        let app = TestApp::spawn().await;
        let body = json!({"username": "alice", "password": "securepass"});
        app.post_without_token(routes::REGISTER, &body).await;

        let login_res = app.post_without_token(routes::LOGIN, &body).await;
        let cookie_a = extract_refresh_cookie(&login_res);

        // First refresh via the cookie-store-aware client: success, sets cookie B.
        let r1 = app.post_without_token(routes::REFRESH, &json!({})).await;
        assert_eq!(r1.status, 200);
        let cookie_b = extract_refresh_cookie(&r1);
        assert_ne!(
            cookie_a, cookie_b,
            "refresh must rotate the cookie (selector + validator)"
        );

        let raw_client = reqwest::Client::builder()
            .no_proxy()
            .cookie_store(false)
            .build()
            .unwrap();
        let replay_a = raw_client
            .post(format!("http://{}{}", app.addr, routes::REFRESH))
            .header("cookie", format!("broccoli_refresh={}", cookie_a))
            .json(&json!({}))
            .send()
            .await
            .expect("send refresh replay A");
        assert_eq!(replay_a.status(), 401);
        let replay_a_body: serde_json::Value = replay_a.json().await.unwrap();
        assert_eq!(replay_a_body["code"], "TOKEN_INVALID");

        // Second legit refresh with cookie B (via the cookie-store client): rotates to cookie C.
        let r2 = app.post_without_token(routes::REFRESH, &json!({})).await;
        assert_eq!(r2.status, 200);
        let cookie_c = extract_refresh_cookie(&r2);
        assert_ne!(cookie_b, cookie_c, "second refresh must also rotate");

        // Replay cookie B: 401 TOKEN_INVALID.
        let replay_b = raw_client
            .post(format!("http://{}{}", app.addr, routes::REFRESH))
            .header("cookie", format!("broccoli_refresh={}", cookie_b))
            .json(&json!({}))
            .send()
            .await
            .expect("send refresh replay B");
        assert_eq!(replay_b.status(), 401);
        let replay_b_body: serde_json::Value = replay_b.json().await.unwrap();
        assert_eq!(replay_b_body["code"], "TOKEN_INVALID");
    }

    /// Replaying a rotated refresh token AFTER the benign-race grace window is
    /// treated as theft: it revokes the user's entire token family, so even the
    /// current (legitimately rotated) token stops working.
    #[tokio::test]
    async fn reused_refresh_token_past_grace_kills_the_family() {
        fn extract_refresh_cookie(resp: &TestResponse) -> String {
            for hv in resp.headers.get_all("set-cookie").iter() {
                let s = hv.to_str().expect("set-cookie utf-8");
                if let Some(rest) = s.strip_prefix("broccoli_refresh=") {
                    let val = rest.split(';').next().unwrap_or("").to_string();
                    if !val.is_empty() {
                        return val;
                    }
                }
            }
            panic!("no broccoli_refresh Set-Cookie on response");
        }

        let app = TestApp::spawn().await;
        let body = json!({"username": "carol", "password": "securepass"});
        app.post_without_token(routes::REGISTER, &body).await;
        let login_res = app.post_without_token(routes::LOGIN, &body).await;
        let cookie_a = extract_refresh_cookie(&login_res);

        // Rotate A -> B (A is now retained as revoked).
        let r1 = app.post_without_token(routes::REFRESH, &json!({})).await;
        assert_eq!(r1.status, 200);
        let cookie_b = extract_refresh_cookie(&r1);

        // Backdate the revoked token past the grace window so a replay reads as
        // theft rather than a concurrent-refresh race (avoids sleeping the grace
        // period). A is the only revoked row in this fresh DB.
        {
            use sea_orm::ConnectionTrait;
            app.db
                .execute_unprepared(
                    "UPDATE refresh_tokens SET revoked_at = NOW() - INTERVAL '1 hour' \
                     WHERE revoked_at IS NOT NULL",
                )
                .await
                .expect("backdate revoked_at");
        }

        let raw = reqwest::Client::builder()
            .no_proxy()
            .cookie_store(false)
            .build()
            .unwrap();

        // Replay A past grace: reuse detected -> family kill -> 401.
        let replay_a = raw
            .post(format!("http://{}{}", app.addr, routes::REFRESH))
            .header("cookie", format!("broccoli_refresh={cookie_a}"))
            .json(&json!({}))
            .send()
            .await
            .expect("send refresh replay A past grace");
        assert_eq!(replay_a.status(), 401);

        // The current token B was legitimately issued, but the family kill revoked
        // it too: a stolen chain cannot outlive detection.
        let use_b = raw
            .post(format!("http://{}{}", app.addr, routes::REFRESH))
            .header("cookie", format!("broccoli_refresh={cookie_b}"))
            .json(&json!({}))
            .send()
            .await
            .expect("send refresh with current token B");
        assert_eq!(
            use_b.status(),
            401,
            "reuse past grace must revoke the whole token family, including the current token"
        );
    }
}

mod refresh_validator_format {
    use super::*;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use server::entity::{refresh_token, user};

    /// Refresh rows written before validators moved to SHA-256 hold argon2
    /// hashes. They must keep working until they expire, and rotating one
    /// writes its successor in the new format.
    #[tokio::test]
    async fn legacy_argon2_refresh_token_still_rotates_into_the_new_format() {
        let app = TestApp::spawn().await;
        app.create_authenticated_user("legacy_rt", "securepass")
            .await;
        let uid = user::Entity::find()
            .filter(user::Column::Username.eq("legacy_rt"))
            .one(&app.db)
            .await
            .unwrap()
            .unwrap()
            .id;

        let now = chrono::Utc::now();
        let validator = "legacy-validator";
        sea_orm::ActiveModelTrait::insert(
            refresh_token::ActiveModel {
                selector: sea_orm::Set("legacy-selector".into()),
                validator: sea_orm::Set(server::utils::hash::hash_password(validator).unwrap()),
                user_id: sea_orm::Set(uid),
                expires_at: sea_orm::Set(now + chrono::Duration::days(1)),
                created_at: sea_orm::Set(now),
                revoked_at: sea_orm::Set(None),
            },
            &app.db,
        )
        .await
        .unwrap();

        let res = app
            .post_without_token(
                routes::CLI_REFRESH,
                &json!({ "refresh_token": format!("legacy-selector:{validator}") }),
            )
            .await;
        assert_eq!(res.status, 200, "legacy refresh: {}", res.text);

        let successor = res.body["refresh_token"].as_str().unwrap();
        let (selector, _) = successor.split_once(':').unwrap();
        let row = refresh_token::Entity::find_by_id(selector.to_owned())
            .one(&app.db)
            .await
            .unwrap()
            .unwrap();
        assert!(
            row.validator.starts_with("sha256$"),
            "successor stored as {}",
            row.validator
        );

        let again = app
            .post_without_token(routes::CLI_REFRESH, &json!({ "refresh_token": successor }))
            .await;
        assert_eq!(again.status, 200, "sha256 refresh: {}", again.text);
    }

    #[tokio::test]
    async fn wrong_validator_is_rejected() {
        let app = TestApp::spawn().await;
        let body = json!({"username": "wrong_rt", "password": "securepass"});
        app.post_without_token(routes::REGISTER, &body).await;
        let login = app.post_without_token(routes::CLI_TOKEN, &json!({})).await;
        assert_eq!(
            login.status, 401,
            "cli-token needs a bearer: {}",
            login.text
        );

        let token = app.post_without_token(routes::LOGIN, &body).await.body["token"]
            .as_str()
            .unwrap()
            .to_string();
        let issued = app
            .post_with_token(routes::CLI_TOKEN, &json!({}), &token)
            .await;
        let rt = issued.body["refresh_token"].as_str().unwrap();
        let (selector, _) = rt.split_once(':').unwrap();

        let res = app
            .post_without_token(
                routes::CLI_REFRESH,
                &json!({ "refresh_token": format!("{selector}:not-the-validator") }),
            )
            .await;
        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "TOKEN_INVALID");
    }
}

mod logout {
    use super::*;

    #[tokio::test]
    async fn authenticated_user_can_logout_and_revoke_refresh_token() {
        let app = TestApp::spawn().await;
        let body = json!({"username": "alice", "password": "securepass"});
        app.post_without_token(routes::REGISTER, &body).await;
        app.post_without_token(routes::LOGIN, &body).await;

        let logout_res = app.post_without_token(routes::LOGOUT, &json!({})).await;
        assert_eq!(logout_res.status, 204);

        let refresh_res = app.post_without_token(routes::REFRESH, &json!({})).await;
        assert_eq!(refresh_res.status, 401);
        assert_eq!(refresh_res.body["code"], "TOKEN_MISSING");
    }

    #[tokio::test]
    async fn logout_is_idempotent_and_handles_missing_token_gracefully() {
        let app = TestApp::spawn().await;

        let res = app.post_without_token(routes::LOGOUT, &json!({})).await;

        assert_eq!(res.status, 204);
    }
}

mod request_validation {
    use super::*;

    #[tokio::test]
    async fn malformed_json_body_returns_validation_error() {
        let app = TestApp::spawn().await;

        let res = app
            .client
            .post(format!("http://{}{}", app.addr, routes::REGISTER))
            .header("Content-Type", "application/json")
            .body("not valid json")
            .send()
            .await
            .expect("Failed to send request");

        let res = TestResponse::from_response(res).await;
        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn missing_required_fields_returns_validation_error() {
        let app = TestApp::spawn().await;

        let res = app
            .post_without_token(routes::REGISTER, &json!({"username": "alice"}))
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }
}

mod authenticated_access {
    use super::*;

    #[tokio::test]
    async fn authenticated_user_can_retrieve_their_profile() {
        let app = TestApp::spawn().await;
        let token = app.create_authenticated_user("alice", "securepass").await;

        let res = app.get_with_token(routes::ME, &token).await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["username"], "alice");
        assert!(res.body["id"].is_number());
        assert_eq!(res.body["roles"], json!(["contestant"]));
        assert!(res.body["permissions"].is_array());
    }

    #[tokio::test]
    async fn request_without_token_is_rejected() {
        let app = TestApp::spawn().await;

        let res = app.get_without_token(routes::ME).await;

        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "TOKEN_MISSING");
    }

    #[tokio::test]
    async fn request_with_malformed_token_is_rejected() {
        let app = TestApp::spawn().await;

        let res = app.get_with_token(routes::ME, "not-a-valid-jwt").await;

        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "TOKEN_INVALID");
    }

    #[tokio::test]
    async fn request_with_non_bearer_auth_scheme_is_rejected() {
        let app = TestApp::spawn().await;

        let res = app
            .client
            .get(format!("http://{}{}", app.addr, routes::ME))
            .header("Authorization", "Basic abc123")
            .send()
            .await
            .expect("Failed to send request");

        let res = TestResponse::from_response(res).await;
        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "TOKEN_INVALID");
    }
}

mod token_freshness {
    use super::*;
    use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Statement};
    use server::entity::user;

    /// A `FreshAuthUser`-guarded mutation must reject an access token minted
    /// before the caller's `credentials_changed_at`, while the stateless read
    /// path keeps accepting the same token (so the hot path stays DB-free).
    #[tokio::test]
    async fn stale_access_token_rejected_on_mutation_but_accepted_on_reads() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("fresh_admin", "securepass", "admin")
            .await;

        let admin_id = user::Entity::find()
            .filter(user::Column::Username.eq("fresh_admin"))
            .one(&app.db)
            .await
            .expect("db query failed")
            .expect("admin should exist")
            .id;

        // Simulate a credential/authorization change strictly after the token
        // was minted (an hour ahead removes any same-second flakiness).
        app.db
            .execute_raw(Statement::from_string(
                app.db.get_database_backend(),
                format!(
                    "UPDATE \"user\" SET credentials_changed_at = now() + interval '1 hour' WHERE id = {admin_id}"
                ),
            ))
            .await
            .expect("bump credentials_changed_at");

        // Guarded high-value mutation rejects the now-stale token.
        let stale = app
            .post_with_token(
                &routes::user_roles(admin_id),
                &json!({ "role": "contestant" }),
                &admin_token,
            )
            .await;
        assert_eq!(
            stale.status, 401,
            "stale token must be rejected on a guarded mutation: {}",
            stale.text
        );
        assert_eq!(stale.body["code"], "TOKEN_INVALID");

        // The stateless read path still accepts the same token.
        let read = app.get_with_token(routes::USERS, &admin_token).await;
        assert_eq!(
            read.status, 200,
            "stateless read path must keep accepting the token: {}",
            read.text
        );
    }
}

mod token_reissue {
    use super::*;
    use crate::common::routes::CLI_TOKEN;
    use sea_orm::{
        ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set, Statement,
    };
    use server::entity::user_role;

    /// Looks up the id of the user named `username`.
    async fn user_id(app: &TestApp, username: &str) -> i32 {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use server::entity::user;

        user::Entity::find()
            .filter(user::Column::Username.eq(username))
            .one(&app.db)
            .await
            .expect("db")
            .expect("user exists")
            .id
    }

    /// Makes every token minted so far for `uid` stale, the same way a role revoke
    /// or password reset does. Setting `credentials_changed_at` an hour ahead keeps
    /// the check clear of same-second timing.
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

    /// Replaces a user's roles directly in the database without bumping
    /// `credentials_changed_at`, so their existing access token still counts as
    /// fresh. This stands in for a role change landing in the same second the token
    /// was minted, which `FreshAuthUser` tolerates by design.
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

    /// Starts a device-flow login and returns `(device_code, user_code)`.
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

    /// `/auth/cli-token` refuses an access token minted before the user's
    /// `credentials_changed_at`. Exchanging it would turn a session revoked by a role
    /// change or password reset into a fresh token plus a new refresh token.
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

    /// `/auth/cli-token` refuses the token of a soft-deleted user, so a deleted
    /// account cannot keep minting fresh tokens.
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

    /// A token minted by `/auth/cli-token` carries the user's current roles and
    /// permissions from the database, not the claims of the presented token, and its
    /// refresh token works.
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

    /// `/auth/device-authorize` refuses a stale access token with `TOKEN_INVALID`
    /// and leaves the device code pending.
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

    /// The token a device poll receives carries the approving user's current roles
    /// and permissions, not the claims of the token that approved the device.
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
}
