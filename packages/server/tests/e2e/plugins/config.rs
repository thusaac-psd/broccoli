use serde_json::json;

use crate::common::E2eTestApp;

#[tokio::test(flavor = "multi_thread")]
async fn set_and_get_problem_scope_config() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("cfg_admin1", "password", "admin")
        .await;
    let problem_id = app.create_problem(&admin, "Config Problem 1").await;

    let config_path = format!("/api/v1/problems/{problem_id}/config/cooldown/cooldown");
    let put_res = app
        .put_with_token(
            &config_path,
            &json!({ "config": { "cooldown_seconds": 30 }, "enabled": true }),
            &admin,
        )
        .await;
    assert_eq!(put_res.status, 200, "PUT config failed: {}", put_res.text);
    assert_eq!(put_res.body["plugin_id"].as_str(), Some("cooldown"));
    assert_eq!(put_res.body["namespace"].as_str(), Some("cooldown"));
    assert_eq!(
        put_res.body["config"]["cooldown_seconds"].as_u64(),
        Some(30)
    );
    assert_eq!(put_res.body["enabled"].as_bool(), Some(true));

    let get_res = app.get_with_token(&config_path, &admin).await;
    assert_eq!(get_res.status, 200, "GET config failed: {}", get_res.text);
    assert_eq!(
        get_res.body["config"]["cooldown_seconds"].as_u64(),
        Some(30)
    );
    assert_eq!(get_res.body["enabled"].as_bool(), Some(true));
}

#[tokio::test(flavor = "multi_thread")]
async fn set_and_get_contest_scope_config() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("cfg_admin2", "password", "admin")
        .await;
    let contest_id = app
        .create_typed_contest(&admin, "Config Contest 2", "icpc", true, true)
        .await;

    let config_path = format!("/api/v1/contests/{contest_id}/config/icpc/contest");
    let put_res = app
        .put_with_token(
            &config_path,
            &json!({
                "config": { "penalty_minutes": 25 },
                "enabled": true
            }),
            &admin,
        )
        .await;
    assert_eq!(put_res.status, 200, "PUT config failed: {}", put_res.text);
    assert_eq!(put_res.body["config"]["penalty_minutes"].as_u64(), Some(25));

    let get_res = app.get_with_token(&config_path, &admin).await;
    assert_eq!(get_res.status, 200, "GET config failed: {}", get_res.text);
    assert_eq!(get_res.body["config"]["penalty_minutes"].as_u64(), Some(25));
}

#[tokio::test(flavor = "multi_thread")]
async fn set_and_get_contest_problem_scope_config() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("cfg_admin3", "password", "admin")
        .await;
    let problem_id = app.create_problem(&admin, "Config Problem 3").await;
    let contest_id = app
        .create_typed_contest(&admin, "Config Contest 3", "icpc", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;

    let config_path =
        format!("/api/v1/contests/{contest_id}/problems/{problem_id}/config/cooldown/cooldown");
    let put_res = app
        .put_with_token(
            &config_path,
            &json!({ "config": { "cooldown_seconds": 120 }, "enabled": true }),
            &admin,
        )
        .await;
    assert_eq!(put_res.status, 200, "PUT config failed: {}", put_res.text);
    assert_eq!(
        put_res.body["config"]["cooldown_seconds"].as_u64(),
        Some(120)
    );

    let get_res = app.get_with_token(&config_path, &admin).await;
    assert_eq!(get_res.status, 200, "GET config failed: {}", get_res.text);
    assert_eq!(
        get_res.body["config"]["cooldown_seconds"].as_u64(),
        Some(120)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_config_removes_it() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("cfg_admin4", "password", "admin")
        .await;
    let problem_id = app.create_problem(&admin, "Config Problem 4").await;

    let config_path = format!("/api/v1/problems/{problem_id}/config/cooldown/cooldown");

    let put_res = app
        .put_with_token(
            &config_path,
            &json!({ "config": { "cooldown_seconds": 10 }, "enabled": true }),
            &admin,
        )
        .await;
    assert_eq!(put_res.status, 200, "PUT config failed: {}", put_res.text);

    let del_res = app.delete_with_token(&config_path, &admin).await;
    assert_eq!(
        del_res.status, 204,
        "DELETE config failed: {}",
        del_res.text
    );

    let get_res = app.get_with_token(&config_path, &admin).await;
    assert_eq!(
        get_res.status, 404,
        "GET after DELETE should be 404: {}",
        get_res.text
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn list_config_shows_all_namespaces() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("cfg_admin5", "password", "admin")
        .await;
    let problem_id = app.create_problem(&admin, "Config Problem 5").await;

    let cd_path = format!("/api/v1/problems/{problem_id}/config/cooldown/cooldown");
    app.put_with_token(
        &cd_path,
        &json!({ "config": { "cooldown_seconds": 45 }, "enabled": true }),
        &admin,
    )
    .await;

    let sl_path = format!("/api/v1/problems/{problem_id}/config/submission-limit/limits");
    app.put_with_token(
        &sl_path,
        &json!({ "config": { "max_submissions": 10 }, "enabled": true }),
        &admin,
    )
    .await;

    let list_path = format!("/api/v1/problems/{problem_id}/config");
    let list_res = app.get_with_token(&list_path, &admin).await;
    assert_eq!(
        list_res.status, 200,
        "List config failed: {}",
        list_res.text
    );

    let configs = list_res
        .body
        .as_array()
        .expect("Response should be an array");
    let plugin_ids: Vec<&str> = configs
        .iter()
        .filter_map(|c| c["plugin_id"].as_str())
        .collect();
    assert!(
        plugin_ids.contains(&"cooldown"),
        "Should list cooldown config, got: {:?}",
        plugin_ids
    );
    assert!(
        plugin_ids.contains(&"submission-limit"),
        "Should list submission-limit config, got: {:?}",
        plugin_ids
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn config_for_nonexistent_problem_returns_404() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("cfg_admin6", "password", "admin")
        .await;

    let config_path = "/api/v1/problems/99999/config/cooldown/cooldown";
    let res = app.get_with_token(config_path, &admin).await;
    assert_eq!(
        res.status, 404,
        "Should return 404 for nonexistent problem: {}",
        res.text
    );
}

/// A contest's own configs are checked again whenever its config issues are
/// read, against the contest as it is now: a qualifier contest config whose
/// advancement places fit its problems is accepted, a problem added later
/// that lets more contestants qualify directly than there are places makes
/// it an issue there, and saving the config again is not refused for it.
#[tokio::test(flavor = "multi_thread")]
async fn a_problem_added_to_a_contest_shows_the_contest_config_it_outgrows() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("cfg_contest_issues_admin", "password", "admin")
        .await;
    let contest_id = app
        .create_typed_contest(&admin, "Qualifier", "codelink-qualifier", true, true)
        .await;
    let first = app.create_problem(&admin, "Qualifier A").await;
    app.add_problem_to_contest_with_label(contest_id, first, "A", &admin)
        .await;
    let config_path = format!("/api/v1/contests/{contest_id}/config/codelink-qualifier/contest");
    let config = json!({ "config": {
        "expected_problem_count": 0,
        "slots_per_problem": 2,
        "solves_to_qualify": 1,
        "advancement_slots": 2,
    }});
    let res = app.put_with_token(&config_path, &config, &admin).await;
    assert_eq!(res.status, 200, "{}", res.text);
    let issues_path = format!("/api/v1/contests/{contest_id}/config/issues");
    let issues = app.get_with_token(&issues_path, &admin).await;
    assert_eq!(issues.status, 200, "{}", issues.text);
    assert_eq!(issues.body, json!([]), "{}", issues.text);

    let second = app.create_problem(&admin, "Qualifier B").await;
    app.add_problem_to_contest_with_label(contest_id, second, "B", &admin)
        .await;
    let issues = app.get_with_token(&issues_path, &admin).await;
    assert_eq!(issues.status, 200, "{}", issues.text);
    let issues = issues.body.as_array().expect("an array").clone();
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(issues[0]["plugin_id"], "codelink-qualifier");
    assert_eq!(issues[0]["scope"], "contest");
    assert_eq!(issues[0]["ref_id"], contest_id.to_string());
    assert_eq!(issues[0]["namespace"], "contest");
    assert_eq!(
        issues[0]["errors"][0]["key"], "codelink-qualifier.config.placesBelowDirectQualifiers",
        "{issues:?}"
    );

    // The problems outgrew the places as much as the other way round: a save
    // is never refused for it, an unrelated field's included, and the issue
    // stays listed until the places or the problems change.
    let mut refreshed = config.clone();
    refreshed["config"]["scoreboard_refresh_seconds"] = json!(30);
    let res = app.put_with_token(&config_path, &refreshed, &admin).await;
    assert_eq!(res.status, 200, "{}", res.text);
    let issues = app.get_with_token(&issues_path, &admin).await;
    assert_eq!(
        issues.body.as_array().map(Vec::len),
        Some(1),
        "{}",
        issues.text
    );
}

/// Only staff who manage contests read a contest's config issues.
#[tokio::test(flavor = "multi_thread")]
async fn contest_config_issues_need_contest_manage() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("cfg_contest_issues_owner", "password", "admin")
        .await;
    let user = app
        .create_authenticated_user("cfg_contest_issues_user", "password")
        .await;
    let contest_id = app
        .create_typed_contest(&admin, "Qualifier", "codelink-qualifier", true, true)
        .await;
    let path = format!("/api/v1/contests/{contest_id}/config/issues");
    let res = app.get_with_token(&path, &user).await;
    assert_eq!(res.status, 403, "{}", res.text);
    let res = app
        .get_with_token("/api/v1/contests/999999/config/issues", &admin)
        .await;
    assert_eq!(res.status, 404, "{}", res.text);
}

/// The admin form of a contest problem reads what it inherits from the
/// server, resolved by the rule plugins read configs by: a field the
/// deployment sets plugin-wide comes from there, with every default filled,
/// and a narrower scope switched off leaves the plugin-wide config in place.
#[tokio::test(flavor = "multi_thread")]
async fn a_contest_problem_form_inherits_what_the_plugin_reads() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("cfg_cascade_admin", "password", "admin")
        .await;
    let problem_id = app.create_problem(&admin, "Cascade").await;
    let contest_id = app.create_contest(&admin, "Cascade Cup", true, true).await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    let res = app
        .put_with_token(
            "/api/v1/admin/plugins/standard-languages/config/compilation",
            &json!({ "config": { "cpp": { "compiler": "/opt/gcc-14/bin/g++" } } }),
            &admin,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    let res = app
        .put_with_token(
            &format!("/api/v1/problems/{problem_id}/config/standard-languages/compilation"),
            &json!({ "config": { "cpp": { "flags": ["-O3"] } }, "enabled": false }),
            &admin,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);

    let res = app
        .get_with_token(
            &format!("/api/v1/contests/{contest_id}/problems/{problem_id}/config/cascade"),
            &admin,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    let compilation = res
        .body
        .as_array()
        .expect("an array")
        .iter()
        .find(|c| c["plugin_id"] == "standard-languages" && c["namespace"] == "compilation")
        .expect("the compilation namespace")
        .clone();
    let inherited = &compilation["inherited"];
    assert_eq!(
        inherited["config"]["cpp"]["compiler"], "/opt/gcc-14/bin/g++",
        "the switched-off problem row leaves the deployment's compiler: {inherited}"
    );
    assert_eq!(
        inherited["config"]["cpp"]["flags"],
        json!(["-O2"]),
        "with the schema's defaults filled in"
    );
    assert_eq!(inherited["source"], "disabled");
    assert_eq!(inherited["fields"]["cpp"], "plugin");
    assert_eq!(inherited["levels"]["problem"]["enabled"], false);
    assert_eq!(compilation["effective"]["config"], inherited["config"]);
}

/// A deployment whose plugin-wide print config holds a station token, whose
/// problem stores its own compile flags, and a contest using the problem.
/// Returns the admin token, the problem and the contest.
async fn configured_everywhere(app: &E2eTestApp, admin_name: &str) -> (String, i32, i32) {
    let admin = app
        .create_user_with_role(admin_name, "password", "admin")
        .await;
    let problem_id = app.create_problem(&admin, "Access").await;
    let contest_id = app.create_contest(&admin, "Access Cup", true, true).await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    let res = app
        .put_with_token(
            "/api/v1/admin/plugins/print/config/print",
            &json!({ "config": {
                "banner": "Global banner",
                "stations": [{ "name": "desk", "token": "station-secret-token" }],
            }}),
            &admin,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    let res = app
        .put_with_token(
            &format!("/api/v1/problems/{problem_id}/config/standard-languages/compilation"),
            &json!({ "config": { "cpp": { "flags": ["-O3", "-problem-only"] } } }),
            &admin,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    (admin, problem_id, contest_id)
}

fn cascade_of<'a>(
    body: &'a serde_json::Value,
    plugin_id: &str,
    namespace: &str,
) -> &'a serde_json::Value {
    body.as_array()
        .expect("an array")
        .iter()
        .find(|c| c["plugin_id"] == plugin_id && c["namespace"] == namespace)
        .unwrap_or_else(|| panic!("no {plugin_id}/{namespace} in {body}"))
}

/// A cascade answer shows a caller only the scopes they may read directly:
/// the plugin-wide row needs `plugin:manage`, a problem's row
/// `problem:edit`, a contest's `contest:manage`. A field whose value comes
/// from a scope the caller may not read is named as withheld, its value left
/// out.
#[tokio::test(flavor = "multi_thread")]
async fn a_cascade_shows_only_the_scopes_its_caller_may_read() {
    let app = E2eTestApp::spawn().await;
    let (admin, problem_id, contest_id) = configured_everywhere(&app, "cascade_access_admin").await;
    let organizer = app
        .create_user_with_permissions("cascade_organizer", "password", &["contest:manage"])
        .await;
    let setter = app
        .create_user_with_permissions("cascade_setter", "password", &["problem:edit"])
        .await;

    let contest = format!("/api/v1/contests/{contest_id}/config/cascade");
    let contest_problem =
        format!("/api/v1/contests/{contest_id}/problems/{problem_id}/config/cascade");
    let problem = format!("/api/v1/problems/{problem_id}/config/cascade");

    // A contest manager reads neither the plugin-wide row nor the problem's.
    for path in [&contest, &contest_problem] {
        let res = app.get_with_token(path, &organizer).await;
        assert_eq!(res.status, 200, "{}", res.text);
        assert!(!res.text.contains("Global banner"), "{path}: {}", res.text);
        assert!(
            !res.text.contains("station-secret-token"),
            "{path}: {}",
            res.text
        );
        assert!(!res.text.contains("-problem-only"), "{path}: {}", res.text);
    }
    let res = app.get_with_token(&contest, &organizer).await;
    let print = cascade_of(&res.body, "print", "print");
    for view in ["effective", "inherited"] {
        assert!(print[view]["levels"]["plugin"].is_null(), "{print}");
        let withheld = print[view]["withheld"].as_array().expect("withheld");
        assert!(withheld.contains(&json!("banner")), "{print}");
        assert!(withheld.contains(&json!("stations")), "{print}");
    }
    let res = app.get_with_token(&contest_problem, &organizer).await;
    let compilation = cascade_of(&res.body, "standard-languages", "compilation");
    assert!(
        compilation["inherited"]["levels"]["problem"].is_null(),
        "{compilation}"
    );
    assert!(
        compilation["inherited"]["withheld"]
            .as_array()
            .expect("withheld")
            .contains(&json!("cpp")),
        "{compilation}"
    );

    // A problem setter reads the problem's own row, never the plugin-wide one.
    let res = app.get_with_token(&problem, &setter).await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert!(res.text.contains("-problem-only"), "{}", res.text);
    let compilation = cascade_of(&res.body, "standard-languages", "compilation");
    assert!(
        compilation["effective"]["levels"]["plugin"].is_null(),
        "{compilation}"
    );
    assert!(
        !compilation["effective"]["levels"]["problem"].is_null(),
        "{compilation}"
    );

    // An admin reads every scope.
    let res = app.get_with_token(&contest_problem, &admin).await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert!(res.text.contains("-problem-only"), "{}", res.text);
    let res = app.get_with_token(&contest, &admin).await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert!(res.text.contains("Global banner"), "{}", res.text);
}
