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
        // Replaying A right away is a benign race (another tab, or a retry
        // after a lost response): it is answered with a sibling token rather
        // than a 401 that would log the client out.
        let replay_a = raw_client
            .post(format!("http://{}{}", app.addr, routes::REFRESH))
            .header("cookie", format!("broccoli_refresh={}", cookie_a))
            .json(&json!({}))
            .send()
            .await
            .expect("send refresh replay A");
        assert_eq!(replay_a.status(), 200, "in-grace replay must succeed");

        // Past the grace window, the same replay is reuse.
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
        let replay_a = raw_client
            .post(format!("http://{}{}", app.addr, routes::REFRESH))
            .header("cookie", format!("broccoli_refresh={}", cookie_a))
            .json(&json!({}))
            .send()
            .await
            .expect("send refresh replay A past grace");
        assert_eq!(replay_a.status(), 401);
        let replay_a_body: serde_json::Value = replay_a.json().await.unwrap();
        assert_eq!(replay_a_body["code"], "TOKEN_INVALID");
    }

    /// Two refreshes carrying the same cookie (two tabs, or a retry after the
    /// response with the new cookie was lost) both succeed, and both resulting
    /// tokens stay usable.
    #[tokio::test]
    async fn concurrent_refreshes_with_one_cookie_both_succeed() {
        fn extract_refresh_cookie(resp: &reqwest::Response) -> String {
            for hv in resp.headers().get_all("set-cookie").iter() {
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
        let body = json!({"username": "dora", "password": "securepass"});
        app.post_without_token(routes::REGISTER, &body).await;
        let raw = reqwest::Client::builder()
            .no_proxy()
            .cookie_store(false)
            .build()
            .unwrap();
        let login = raw
            .post(format!("http://{}{}", app.addr, routes::LOGIN))
            .json(&body)
            .send()
            .await
            .unwrap();
        let cookie = extract_refresh_cookie(&login);

        let refresh = |cookie: String| {
            let raw = raw.clone();
            let url = format!("http://{}{}", app.addr, routes::REFRESH);
            async move {
                raw.post(url)
                    .header("cookie", format!("broccoli_refresh={cookie}"))
                    .json(&json!({}))
                    .send()
                    .await
                    .unwrap()
            }
        };
        let (a, b) = tokio::join!(refresh(cookie.clone()), refresh(cookie));
        assert_eq!(a.status(), 200);
        assert_eq!(b.status(), 200);
        let (cookie_a, cookie_b) = (extract_refresh_cookie(&a), extract_refresh_cookie(&b));
        assert_ne!(cookie_a, cookie_b);

        for next in [cookie_a, cookie_b] {
            let res = refresh(next).await;
            assert_eq!(res.status(), 200, "a sibling token must stay usable");
        }
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
