use crate::common::E2eTestApp;

#[tokio::test(flavor = "multi_thread")]
async fn admin_can_list_loaded_plugins() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("pa_admin1", "password", "admin")
        .await;

    let res = app.get_with_token("/api/v1/admin/plugins", &admin).await;
    assert_eq!(res.status, 200, "List plugins failed: {}", res.text);

    let plugins = res.body.as_array().expect("Response should be an array");
    let plugin_ids: Vec<&str> = plugins.iter().filter_map(|p| p["id"].as_str()).collect();

    assert!(
        plugin_ids.contains(&"cooldown"),
        "Plugin list should include 'cooldown', got: {:?}",
        plugin_ids
    );
    assert!(
        plugin_ids.contains(&"submission-limit"),
        "Plugin list should include 'submission-limit', got: {:?}",
        plugin_ids
    );
    assert!(
        plugin_ids.contains(&"icpc"),
        "Plugin list should include 'icpc', got: {:?}",
        plugin_ids
    );
    assert!(
        plugin_ids.contains(&"ioi"),
        "Plugin list should include 'ioi', got: {:?}",
        plugin_ids
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_can_get_plugin_details() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("pa_admin2", "password", "admin")
        .await;

    let res = app
        .get_with_token("/api/v1/admin/plugins/icpc", &admin)
        .await;
    assert_eq!(res.status, 200, "Get plugin details failed: {}", res.text);
    assert_eq!(res.body["id"].as_str(), Some("icpc"));
    assert!(
        res.body["name"].is_string() || res.body["id"].is_string(),
        "Plugin details should have identifying info"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn non_admin_cannot_access_plugin_admin() {
    let app = E2eTestApp::spawn().await;

    let user = app.create_authenticated_user("pa_user3", "password").await;

    let res = app.get_with_token("/api/v1/admin/plugins", &user).await;
    assert_eq!(res.status, 403, "Non-admin should get 403: {}", res.text);
    assert_eq!(res.body["code"].as_str().unwrap_or(""), "PERMISSION_DENIED");
}

#[tokio::test(flavor = "multi_thread")]
async fn plugin_proxy_routes_to_plugin() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("pa_admin4", "password", "admin")
        .await;
    let contestant = app.create_authenticated_user("pa_user4", "password").await;

    let contest_id = app
        .create_typed_contest(&admin, "Proxy Contest 4", "icpc", true, true)
        .await;
    let problem_id = app.create_problem(&admin, "Proxy Problem 4").await;
    app.create_test_case(problem_id, &admin).await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    app.register_for_contest(contest_id, &contestant).await;

    let proxy_path = format!("/api/v1/p/icpc/api/plugins/icpc/contests/{contest_id}/standings");
    let res = app.get_with_token(&proxy_path, &contestant).await;
    assert_eq!(
        res.status, 200,
        "Proxy route to ICPC standings should succeed: {}",
        res.text
    );
    assert!(res.body["rows"].is_array(), "Should return standings data");
}

#[tokio::test(flavor = "multi_thread")]
async fn nonexistent_plugin_proxy_returns_404() {
    let app = E2eTestApp::spawn().await;

    let user = app.create_authenticated_user("pa_user5", "password").await;

    let res = app
        .get_with_token("/api/v1/p/nonexistent-plugin/anything", &user)
        .await;
    assert_eq!(
        res.status, 404,
        "Non-existent plugin proxy should return 404: {}",
        res.text
    );
}

/// Disabling a plugin stops what it does, not how the UI names it: its name
/// and description stay translated in every served language, while the rest
/// of its pack goes until it is enabled again.
#[tokio::test(flavor = "multi_thread")]
async fn a_disabled_plugin_keeps_its_name_and_description_in_every_language() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("i18n_disable_admin", "password", "admin")
        .await;
    let zh = || async {
        let res = app
            .get_with_token("/api/v1/i18n/translations/zh-CN", &admin)
            .await;
        assert_eq!(res.status, 200, "{}", res.text);
        res.body
    };
    let enabled = zh().await;
    assert_eq!(enabled["plugin.cooldown.name"], "提交冷却");
    assert_eq!(enabled["cooldown.ready"], "就绪");

    let res = app
        .post_with_token(
            "/api/v1/admin/plugins/cooldown/disable",
            &serde_json::json!({}),
            &admin,
        )
        .await;
    assert!(res.status < 300, "disable failed: {}", res.text);
    let disabled = zh().await;
    assert_eq!(disabled["plugin.cooldown.name"], "提交冷却", "{disabled}");
    assert_eq!(
        disabled["plugin.cooldown.description"], enabled["plugin.cooldown.description"],
        "{disabled}"
    );
    assert!(disabled["cooldown.ready"].is_null(), "{disabled}");
}
