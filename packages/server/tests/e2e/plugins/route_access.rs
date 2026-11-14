//! The shipped plugins' routes answer exactly like the core route for the
//! contest or problem they address: the host resolves it before the plugin
//! runs, so no plugin route can show a contest or problem its caller cannot
//! read, and no plugin chooses whether to check.

use sea_orm::ConnectionTrait;

use crate::common::E2eTestApp;

const ICPC_STANDINGS: &str = "/api/v1/p/icpc/api/plugins/icpc/contests";
const IOI: &str = "/api/v1/p/ioi/api/plugins/ioi/contests";

async fn exec(app: &E2eTestApp, sql: String) {
    app.db.execute_unprepared(&sql).await.unwrap();
}

/// A contest that is not activated is visible to `contest:manage` only, on
/// every plugin route as on `GET /contests/{id}`.
#[tokio::test(flavor = "multi_thread")]
async fn a_draft_contest_is_hidden_on_every_plugin_route() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("draft_admin", "password", "admin")
        .await;
    let user = app
        .create_authenticated_user("draft_user", "password")
        .await;
    let icpc = app
        .create_typed_contest(&admin, "Draft ICPC", "icpc", true, true)
        .await;
    let ioi = app
        .create_typed_contest(&admin, "Draft IOI", "ioi", true, true)
        .await;
    exec(
        &app,
        format!("UPDATE contest SET activate_time = NULL WHERE id IN ({icpc}, {ioi})"),
    )
    .await;

    for path in [
        format!("{ICPC_STANDINGS}/{icpc}/standings"),
        format!("{ICPC_STANDINGS}/{icpc}/info"),
        format!("{IOI}/{ioi}/scoreboard"),
        format!("{IOI}/{ioi}/info"),
    ] {
        let res = app.get_with_token(&path, &user).await;
        assert_eq!(res.status, 404, "{path}: {}", res.text);
        assert_eq!(res.body["code"], "NOT_FOUND", "{path}");
        let res = app.get_with_token(&path, &admin).await;
        assert_eq!(res.status, 200, "{path} for an organizer: {}", res.text);
    }
}

/// Plugin standings take the same callers as `GET /contests/{id}`: a
/// signed-in user, never an anonymous one.
#[tokio::test(flavor = "multi_thread")]
async fn plugin_standings_need_a_signed_in_caller_like_the_contest() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("anon_admin", "password", "admin")
        .await;
    let contest = app
        .create_typed_contest(&admin, "Public ICPC", "icpc", true, true)
        .await;

    let core = app
        .get_without_token(&format!("/api/v1/contests/{contest}"))
        .await;
    let plugin = app
        .get_without_token(&format!("{ICPC_STANDINGS}/{contest}/standings"))
        .await;
    assert_eq!(core.status, 401);
    assert_eq!(plugin.status, 401, "{}", plugin.text);
    assert_eq!(plugin.body["code"], "TOKEN_MISSING");
}

/// A contest-type plugin's routes serve only contests of its type: the IOI
/// scoreboard of an ICPC contest does not exist.
#[tokio::test(flavor = "multi_thread")]
async fn a_contest_type_route_does_not_serve_other_contest_types() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("type_admin", "password", "admin")
        .await;
    let user = app.create_authenticated_user("type_user", "password").await;
    let icpc = app
        .create_typed_contest(&admin, "ICPC", "icpc", true, true)
        .await;

    for token in [&user, &admin] {
        let res = app
            .get_with_token(&format!("{IOI}/{icpc}/scoreboard"), token)
            .await;
        assert_eq!(res.status, 404, "{}", res.text);
    }
}

/// Per-caller status routes read the caller's standing in one contest, so
/// they take participants only; a private contest stays not found, and a
/// problem that does not exist is not found on the standalone route too.
#[tokio::test(flavor = "multi_thread")]
async fn status_routes_answer_like_the_resources_they_address() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("status_admin", "password", "admin")
        .await;
    let user = app
        .create_authenticated_user("status_user", "password")
        .await;
    let private = app
        .create_typed_contest(&admin, "Private IOI", "ioi", false, true)
        .await;
    let problem = app.create_problem(&admin, "Hidden").await;
    app.add_problem_to_contest(private, problem, &admin).await;

    for path in [
        format!("{IOI}/{private}/token-status"),
        format!("{IOI}/{private}/problems/{problem}/submission-status"),
        format!(
            "/api/v1/p/submission-limit/api/plugins/submission-limit/contests/{private}/problems/{problem}/status"
        ),
        format!(
            "/api/v1/p/cooldown/api/plugins/cooldown/contests/{private}/problems/{problem}/status"
        ),
        format!(
            "/api/v1/p/submission-limit/api/plugins/submission-limit/problems/{problem}/status"
        ),
        format!("/api/v1/p/cooldown/api/plugins/cooldown/problems/{problem}/status"),
        "/api/v1/p/submission-limit/api/plugins/submission-limit/problems/999999/status".into(),
    ] {
        let res = app.get_with_token(&path, &user).await;
        assert_eq!(res.status, 404, "{path}: {}", res.text);
    }

    let public = app
        .create_typed_contest(&admin, "Public IOI", "ioi", true, true)
        .await;
    let res = app
        .get_with_token(&format!("{IOI}/{public}/token-status"), &user)
        .await;
    assert_eq!(res.status, 403, "a non-participant: {}", res.text);
    app.register_for_contest(public, &user).await;
    let res = app
        .get_with_token(&format!("{IOI}/{public}/token-status"), &user)
        .await;
    assert_eq!(res.status, 200, "a participant: {}", res.text);
}

/// Errors from plugin routes use the host's error shape: a path id that is
/// not a number is a 400 `VALIDATION_ERROR`, not a 500.
#[tokio::test(flavor = "multi_thread")]
async fn a_malformed_path_id_is_a_validation_error() {
    let app = E2eTestApp::spawn().await;
    let user = app
        .create_authenticated_user("bad_id_user", "password")
        .await;
    let res = app
        .get_with_token(&format!("{IOI}/abc/scoreboard"), &user)
        .await;
    assert_eq!(res.status, 400, "{}", res.text);
    assert_eq!(res.body["code"], "VALIDATION_ERROR");
    assert!(res.body["message"].is_string());
}
