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
    /// this pins that gate (previously untested against a non-admin
    /// authenticated user) for the guard test's `visibility-bypass-audited`
    /// comment above that file's entity import.
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
    /// `permission`) used to see an expired token as "no caller", so the plugin
    /// answered 403 "requires contest:manage". Clients refresh their access
    /// token on 401, never on 403, so a staff member whose 5-minute token
    /// lapsed hit a misleading permissions error on every afternoon-bracket
    /// staff action until they reloaded - found running a real 128-candidate
    /// tournament. Core routes already answer 401 here; plugin routes now match.
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
