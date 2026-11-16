use serde_json::json;

use crate::common::{TestApp, routes};

async fn assert_plugin_status(app: &TestApp, token: &str, plugin_id: &str, expected_status: &str) {
    let res = app
        .get_with_token(&routes::admin_plugin_details(plugin_id), token)
        .await;
    assert_eq!(res.status, 200);
    assert_eq!(res.body["status"], expected_status);
}

mod plugin_management {
    use super::*;

    #[tokio::test]
    async fn unauthenticated_request_is_rejected() {
        let app = TestApp::spawn().await;

        let res = app
            .get_without_token(&routes::admin_plugin_details("server-plugin"))
            .await;
        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "TOKEN_MISSING");

        let res = app
            .post_without_token(&routes::admin_plugin_enable("server-plugin"), &json!({}))
            .await;
        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "TOKEN_MISSING");

        let res = app
            .post_without_token(&routes::admin_plugin_disable("server-plugin"), &json!({}))
            .await;
        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "TOKEN_MISSING");
    }

    /// `plugin:manage` is the sole gate on every `handlers/admin.rs` endpoint -
    /// this pins that gate against a non-admin authenticated user for the
    /// guard test's `visibility-bypass-audited` comment above that file's
    /// entity import.
    #[tokio::test]
    async fn contestant_cannot_manage_plugins() {
        let app = TestApp::spawn().await;
        let contestant = app
            .create_user_with_role("contestant_plugin_mgmt", "securepass", "contestant")
            .await;

        let res = app.get_with_token(routes::ADMIN_PLUGINS, &contestant).await;
        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");

        let res = app
            .get_with_token(&routes::admin_plugin_details("server-plugin"), &contestant)
            .await;
        assert_eq!(res.status, 403);

        let res = app
            .post_with_token(
                &routes::admin_plugin_enable("server-plugin"),
                &json!({}),
                &contestant,
            )
            .await;
        assert_eq!(res.status, 403);

        let res = app
            .post_with_token(
                &routes::admin_plugin_disable("server-plugin"),
                &json!({}),
                &contestant,
            )
            .await;
        assert_eq!(res.status, 403);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn admin_can_enable_a_valid_plugin() {
        let app = TestApp::spawn_with_plugins().await;
        let token = app
            .create_user_with_role("admin_user", "securepass", "admin")
            .await;

        assert_plugin_status(&app, &token, "server-plugin", "Loaded").await;

        let res = app
            .post_with_token(
                &routes::admin_plugin_disable("server-plugin"),
                &json!({}),
                &token,
            )
            .await;
        assert_eq!(res.status, 200);
        assert_plugin_status(&app, &token, "server-plugin", "Unloaded").await;

        let res = app
            .post_with_token(
                &routes::admin_plugin_enable("server-plugin"),
                &json!({}),
                &token,
            )
            .await;
        assert_eq!(res.status, 200);
        assert_plugin_status(&app, &token, "server-plugin", "Loaded").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn enabling_same_plugin_twice_returns_conflict() {
        let app = TestApp::spawn_with_plugins().await;
        let token = app
            .create_user_with_role("admin_user", "securepass", "admin")
            .await;

        let res = app
            .post_with_token(
                &routes::admin_plugin_enable("server-plugin"),
                &json!({}),
                &token,
            )
            .await;

        assert_eq!(res.status, 409);
        assert_eq!(res.body["code"], "CONFLICT");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn enabling_nonexistent_plugin_returns_not_found() {
        let app = TestApp::spawn_with_plugins().await;
        let token = app
            .create_user_with_role("admin_user", "securepass", "admin")
            .await;

        let res = app
            .post_with_token(
                &routes::admin_plugin_enable("no-such-plugin"),
                &json!({}),
                &token,
            )
            .await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }
}

mod plugin_routing {
    use super::*;

    #[tokio::test]
    async fn get_route_streams_changed_snapshots_and_reconnects_at_current_state() {
        use crate::common::sse_snapshot;
        use std::time::Duration;
        let app = TestApp::spawn_with_plugins().await;
        let path = routes::plugin_proxy("server-plugin", "kv/stream");
        assert_eq!(
            app.post_without_token(&path, &json!({"value": "first"}))
                .await
                .status,
            200
        );
        let url = format!("http://{}{}", app.addr, path);
        let mut response = app
            .client
            .get(&url)
            .header("Accept", "text/event-stream")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        assert_eq!(response.headers()["x-accel-buffering"], "no");
        assert!(response.headers().get("content-encoding").is_none());
        let mut buffer = Vec::new();
        assert_eq!(
            sse_snapshot(&mut response, &mut buffer, |_| true).await["value"],
            "first"
        );
        assert!(
            tokio::time::timeout(
                Duration::from_millis(1200),
                sse_snapshot(&mut response, &mut buffer, |_| true)
            )
            .await
            .is_err()
        );
        assert_eq!(
            app.post_without_token(&path, &json!({"value": "second"}))
                .await
                .status,
            200
        );
        assert_eq!(
            sse_snapshot(&mut response, &mut buffer, |_| true).await["value"],
            "second"
        );
        drop(response);
        let mut resumed = app
            .client
            .get(&url)
            .header("Accept", "text/event-stream")
            .send()
            .await
            .unwrap();
        assert_eq!(
            sse_snapshot(&mut resumed, &mut Vec::new(), |_| true).await["value"],
            "second"
        );
    }

    #[tokio::test]
    async fn streaming_get_keeps_permission_checks_and_stops_when_identity_is_revoked() {
        use crate::common::sse_snapshot;
        use sea_orm::ConnectionTrait;
        use std::time::Duration;
        let app = TestApp::spawn_with_plugins().await;
        let url = format!(
            "http://{}{}",
            app.addr,
            routes::plugin_proxy("server-plugin", "protected/123")
        );
        let missing = app
            .client
            .get(&url)
            .header("Accept", "text/event-stream")
            .send()
            .await
            .unwrap();
        assert_eq!(missing.status(), 401);
        let token = app
            .create_user_with_role("stream_admin", "securepass", "admin")
            .await;
        let mut response = app
            .client
            .get(&url)
            .bearer_auth(&token)
            .header("Accept", "text/event-stream")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let first = sse_snapshot(&mut response, &mut Vec::new(), |_| true).await;
        assert!(first["auth_user_id"].is_number());
        app.db.execute_unprepared(r#"UPDATE "user" SET credentials_changed_at = now() + interval '2 seconds' WHERE username = 'stream_admin'"#).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(3), response.chunk())
                .await
                .unwrap()
                .unwrap()
                .is_none()
        );
        let revoked = app
            .client
            .get(&url)
            .bearer_auth(&token)
            .header("Accept", "text/event-stream")
            .send()
            .await
            .unwrap();
        assert_eq!(revoked.status(), 401);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn public_route_returns_correct_response() {
        let app = TestApp::spawn_with_plugins().await;

        let res = app
            .get_without_token(&routes::plugin_proxy_with_query(
                "server-plugin",
                "reflect/123",
                "page=1&sort=desc",
            ))
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["params"]["id"], "123");
        assert_eq!(res.body["query"]["page"], "1");
        assert_eq!(res.body["query"]["sort"], "desc");
        assert_eq!(res.body["method"], "GET");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn valid_token_is_forwarded_to_unprotected_plugin_routes() {
        let app = TestApp::spawn_with_plugins().await;
        let token = app
            .create_user_with_role("plugin_user", "securepass", "contestant")
            .await;

        let me = app.get_with_token(routes::ME, &token).await;
        assert_eq!(me.status, 200);

        let res = app
            .get_with_token(
                &routes::plugin_proxy("server-plugin", "reflect/123"),
                &token,
            )
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["auth_user_id"], me.body["id"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn protected_route_distinguishes_missing_and_invalid_tokens() {
        let app = TestApp::spawn_with_plugins().await;

        let missing = app
            .get_without_token(&routes::plugin_proxy("server-plugin", "protected/123"))
            .await;
        assert_eq!(missing.status, 401);
        assert_eq!(missing.body["code"], "TOKEN_MISSING");

        let invalid = app
            .get_with_token(
                &routes::plugin_proxy("server-plugin", "protected/123"),
                "bad.token",
            )
            .await;
        assert_eq!(invalid.status, 401);
        assert_eq!(invalid.body["code"], "TOKEN_INVALID");
    }

    /// An INVALID or EXPIRED bearer token must be rejected with 401 on an
    /// unprotected plugin route too - not silently downgraded to anonymous.
    ///
    /// Routes that check permissions inside the plugin (no manifest
    /// `permission`) would otherwise see an expired token as "no caller" and
    /// answer 403 "requires contest:manage". Clients refresh their access
    /// token on 401, never on 403, so a staff member whose 5-minute token
    /// lapsed would hit a misleading permissions error on every staff action
    /// until they reloaded. Plugin routes answer 401 here, as core routes do.
    /// A genuinely ABSENT token is still anonymous, because some plugin routes
    /// are public.
    #[tokio::test(flavor = "multi_thread")]
    async fn unprotected_route_rejects_an_invalid_token_but_allows_no_token() {
        let app = TestApp::spawn_with_plugins().await;
        let route = routes::plugin_proxy("server-plugin", "reflect/123");

        let invalid = app.get_with_token(&route, "bad.token").await;
        assert_eq!(
            invalid.status, 401,
            "a present-but-invalid token must be a 401 so the client refreshes: {}",
            invalid.text
        );
        assert_eq!(invalid.body["code"], "TOKEN_INVALID");

        let anonymous = app.get_without_token(&route).await;
        assert_eq!(
            anonymous.status, 200,
            "no token at all is still anonymous on a public route: {}",
            anonymous.text
        );
    }

    /// A validly signed token minted before the caller's last credential
    /// change (a role grant or revoke, a password reset, a deactivation) is
    /// a 401 on every plugin route, so the client refreshes and the plugin
    /// sees the caller's current roles. Answering such a request as
    /// anonymous would show an admin whose role just changed the public
    /// view of a plugin page.
    #[tokio::test(flavor = "multi_thread")]
    async fn stale_token_is_a_401_on_every_plugin_route() {
        use sea_orm::ConnectionTrait;
        let app = TestApp::spawn_with_plugins().await;
        let token = app
            .create_user_with_role("stale_user", "securepass", "admin")
            .await;
        app.db
            .execute_unprepared(
                r#"UPDATE "user" SET credentials_changed_at = now() + interval '1 hour' WHERE username = 'stale_user'"#,
            )
            .await
            .unwrap();

        for path in ["reflect/123", "protected/123"] {
            let res = app
                .get_with_token(&routes::plugin_proxy("server-plugin", path), &token)
                .await;
            assert_eq!(res.status, 401, "{path}: {}", res.text);
            assert_eq!(res.body["code"], "TOKEN_INVALID", "{path}");
        }
    }

    #[tokio::test]
    async fn nonexistent_route_returns_not_found() {
        let app = TestApp::spawn().await;

        let res = app
            .get_without_token(&routes::plugin_proxy("server-plugin", "no-such-route"))
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn nonexistent_plugin_returns_not_found() {
        let app = TestApp::spawn().await;

        let res = app
            .get_without_token(&routes::plugin_proxy("no-such-plugin", "some-route"))
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn plugin_can_use_kv_store_to_persist_data() {
        let app = TestApp::spawn_with_plugins().await;
        let url = routes::plugin_proxy("server-plugin", "kv/some-key");

        let res = app.get_without_token(&url).await;
        assert_eq!(res.status, 404);

        app.post_without_token(&url, &json!({"value": "42"})).await;

        let res = app.get_without_token(&url).await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["value"], "42");
    }
}

mod sql {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn plugin_can_execute_parameterized_sql() {
        let app = TestApp::spawn_with_plugins().await;

        let res = app
            .post_without_token(
                &routes::plugin_proxy("server-plugin", "sql/params"),
                &json!({ "name": "legit_user" }),
            )
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["found"], 1);

        let injection_attempt = "'; DROP TABLE p_names; --";
        let res = app
            .post_without_token(
                &routes::plugin_proxy("server-plugin", "sql/params"),
                &json!({ "name": injection_attempt }),
            )
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["found"], 1);

        let res = app
            .post_without_token(
                &routes::plugin_proxy("server-plugin", "sql/params"),
                &json!({ "name": "legit_user" }),
            )
            .await;
        assert_eq!(res.body["found"], 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn web_plugin_asset_is_served_with_correct_content_type() {
        let app = TestApp::spawn_with_plugins().await;

        let res = app
            .get_without_token(&routes::plugin_asset("web-plugin", "index.js"))
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.headers["Content-Type"], "text/javascript");
    }

    /// A file the plugin's web root does not contain (an unbuilt bundle, or a
    /// stale chunk name) is a 404, not a 500 the web client keeps retrying.
    #[tokio::test(flavor = "multi_thread")]
    async fn missing_file_in_a_web_plugin_returns_not_found() {
        let app = TestApp::spawn_with_plugins().await;

        let res = app
            .get_without_token(&routes::plugin_asset("web-plugin", "styles.css"))
            .await;
        assert_eq!(res.status, 404, "body: {}", res.text);
    }

    #[tokio::test]
    async fn asset_request_for_plugin_without_web_assets_returns_not_found() {
        let app = TestApp::spawn().await;

        let res = app
            .get_without_token(&routes::plugin_asset("server-plugin", "index.js"))
            .await;
        assert_eq!(res.status, 404);
    }

    #[tokio::test]
    async fn asset_request_for_nonexistent_plugin_returns_not_found() {
        let app = TestApp::spawn().await;

        let res = app
            .get_without_token(&routes::plugin_asset("no-such-plugin", "index.js"))
            .await;
        assert_eq!(res.status, 404);
    }

    #[tokio::test]
    async fn path_traversal_in_asset_request_is_rejected() {
        let app = TestApp::spawn().await;

        let res = app
            .get_without_token(&routes::plugin_asset("web-plugin", "../secret.txt"))
            .await;
        assert_eq!(res.status, 404);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn calling_disabled_plugin_returns_not_found() {
        let app = TestApp::spawn_with_plugins().await;
        let token = app
            .create_user_with_role("admin_user", "securepass", "admin")
            .await;

        let res = app
            .post_with_token(
                &routes::admin_plugin_disable("server-plugin"),
                &json!({}),
                &token,
            )
            .await;
        assert_eq!(res.status, 200);

        let res = app
            .get_without_token(&routes::plugin_proxy("server-plugin", "reflect/123"))
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }
}

/// A plugin route that declares the contest or problem it addresses is
/// answered by the host exactly as the core route for that resource would
/// answer, before the plugin runs; the plugin receives the resolved contest.
mod route_access {
    use super::*;
    use sea_orm::ConnectionTrait;

    fn proxy(path: &str) -> String {
        routes::plugin_proxy("server-plugin", path)
    }

    async fn exec(app: &TestApp, sql: String) {
        app.db.execute_unprepared(&sql).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn contest_route_answers_like_the_core_contest_route() {
        let app = TestApp::spawn_with_plugins().await;
        let admin = app
            .create_user_with_role("ra_admin", "securepass", "admin")
            .await;
        let user = app
            .create_user_with_role("ra_user", "securepass", "contestant")
            .await;
        let public = app.create_contest(&admin, "Public", true, false).await;
        let private = app.create_contest(&admin, "Private", false, false).await;
        let draft = app.create_contest(&admin, "Draft", true, false).await;
        exec(
            &app,
            format!("UPDATE contest SET activate_time = NULL WHERE id = {draft}"),
        )
        .await;

        for (contest, token) in [
            (public, Some(&user)),
            (private, Some(&user)),
            (draft, Some(&user)),
            (draft, Some(&admin)),
            (public, None),
            (999_999, Some(&user)),
        ] {
            let (core, plugin) = match token {
                Some(token) => (
                    app.get_with_token(&routes::contest(contest), token).await,
                    app.get_with_token(&proxy(&format!("contests/{contest}/reflect")), token)
                        .await,
                ),
                None => (
                    app.get_without_token(&routes::contest(contest)).await,
                    app.get_without_token(&proxy(&format!("contests/{contest}/reflect")))
                        .await,
                ),
            };
            assert_eq!(
                plugin.status, core.status,
                "contest {contest}: plugin {} vs core {}",
                plugin.text, core.text
            );
            if core.status != 200 {
                assert_eq!(plugin.body["code"], core.body["code"], "contest {contest}");
            }
        }

        let res = app
            .get_with_token(&proxy(&format!("contests/{public}/reflect")), &user)
            .await;
        assert_eq!(res.body["contest"]["id"], public);
        assert_eq!(res.body["contest"]["phase"], "running");
        assert_eq!(res.body["contest"]["is_public"], true);
        assert_eq!(res.body["contest"]["participant"], false);
        assert_eq!(res.body["contest"]["contest_type"], "standard");

        let res = app
            .get_with_token(&proxy(&format!("contests/{draft}/reflect")), &admin)
            .await;
        assert_eq!(res.body["contest"]["phase"], "unactivated");

        let res = app
            .get_with_token(&proxy("contests/abc/reflect"), &user)
            .await;
        assert_eq!(res.status, 400, "{}", res.text);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn contest_of_another_type_is_not_found() {
        let app = TestApp::spawn_with_plugins().await;
        let admin = app
            .create_user_with_role("rt_admin", "securepass", "admin")
            .await;
        let contest = app.create_contest(&admin, "Typed", true, false).await;
        let path = proxy(&format!("contests/{contest}/typed"));

        let res = app.get_with_token(&path, &admin).await;
        assert_eq!(res.status, 404, "{}", res.text);
        assert_eq!(res.body["code"], "NOT_FOUND");

        exec(
            &app,
            format!("UPDATE contest SET contest_type = 'fixture-type' WHERE id = {contest}"),
        )
        .await;
        let res = app.get_with_token(&path, &admin).await;
        assert_eq!(res.status, 200, "{}", res.text);
        assert_eq!(res.body["contest"]["contest_type"], "fixture-type");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn participant_route_needs_registration_and_a_problem_of_the_contest() {
        let app = TestApp::spawn_with_plugins().await;
        let admin = app
            .create_user_with_role("rp_admin", "securepass", "admin")
            .await;
        let user = app
            .create_user_with_role("rp_user", "securepass", "contestant")
            .await;
        let contest = app.create_contest(&admin, "Open", true, false).await;
        let problem = app.insert_public_problem("In contest").await;
        let other = app.insert_public_problem("Elsewhere").await;
        app.add_problem_to_contest(contest, problem, &admin).await;
        let mine = |p: i32| proxy(&format!("contests/{contest}/problems/{p}/mine"));

        let res = app.get_with_token(&mine(problem), &user).await;
        assert_eq!(res.status, 403, "non-participant: {}", res.text);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");

        app.register_for_contest(contest, &user).await;
        let res = app.get_with_token(&mine(problem), &user).await;
        assert_eq!(res.status, 200, "{}", res.text);
        assert_eq!(res.body["problem_id"], problem);
        assert_eq!(res.body["contest"]["participant"], true);

        let res = app.get_with_token(&mine(other), &user).await;
        assert_eq!(res.status, 404, "problem outside the contest: {}", res.text);

        let res = app.get_with_token(&mine(problem), &admin).await;
        assert_eq!(
            res.status, 200,
            "contest:manage needs no registration: {}",
            res.text
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn standalone_problem_route_answers_like_the_core_problem_route() {
        let app = TestApp::spawn_with_plugins().await;
        let admin = app
            .create_user_with_role("rs_admin", "securepass", "admin")
            .await;
        let user = app
            .create_user_with_role("rs_user", "securepass", "contestant")
            .await;
        let contest = app.create_contest(&admin, "Hidden", false, false).await;
        let hidden = app.insert_public_problem("Hidden problem").await;
        exec(
            &app,
            format!("UPDATE problem SET is_public = FALSE WHERE id = {hidden}"),
        )
        .await;
        app.add_problem_to_contest(contest, hidden, &admin).await;

        for (problem, token) in [(hidden, &user), (hidden, &admin), (999_999, &user)] {
            let core = app.get_with_token(&routes::problem(problem), token).await;
            let plugin = app
                .get_with_token(&proxy(&format!("problems/{problem}/reflect")), token)
                .await;
            assert_eq!(
                plugin.status, core.status,
                "problem {problem}: plugin {} vs core {}",
                plugin.text, core.text
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn signed_in_route_refuses_anonymous_callers() {
        let app = TestApp::spawn_with_plugins().await;
        let res = app.get_without_token(&proxy("me")).await;
        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "TOKEN_MISSING");
    }

    /// A plugin's server error never carries its detail to the caller: the
    /// host answers it with its generic body, whether the plugin built a 5xx
    /// response or failed the call.
    #[tokio::test(flavor = "multi_thread")]
    async fn plugin_server_errors_reach_the_caller_without_detail() {
        let app = TestApp::spawn_with_plugins().await;
        for path in ["server-error", "fail"] {
            let res = app.get_without_token(&proxy(path)).await;
            assert_eq!(res.status, 500, "{path}");
            assert_eq!(res.body["code"], "INTERNAL_ERROR", "{path}");
            assert!(
                !res.text.contains("permission denied"),
                "{path} leaked: {}",
                res.text
            );
        }
    }
}
