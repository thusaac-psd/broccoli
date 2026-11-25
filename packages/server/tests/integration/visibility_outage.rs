//! A contest-type plugin that cannot decide (disabled, or enabled but not
//! loaded here) degrades exactly what it governs, per resource and per field:
//! lists keep every row, with the plugin-governed fields left out and a
//! `withheld` marker saying why; contests stay readable and say their plugin
//! is out; staff, whom plugins never narrow, keep the full view.

use sea_orm::{ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter, Set, Statement};
use serde_json::{Value, json};
use server::plugin_lifecycle;

use crate::common::{SpawnOptions, TestApp, routes};

/// The fixture plugin that declares a visibility query.
const PLUGIN: &str = "server-plugin";
/// The contest type the plugin recorded when it last loaded.
const GOVERNED_TYPE: &str = "fleet-only";

struct Fixture {
    admin: String,
    contestant: String,
    governed: i32,
    other: i32,
    problem: i32,
    submission: i32,
}

async fn exec(app: &TestApp, sql: String) {
    app.db
        .execute_raw(Statement::from_string(DbBackend::Postgres, sql))
        .await
        .unwrap();
}

async fn user_id(app: &TestApp, username: &str) -> i32 {
    server::entity::user::Entity::find()
        .filter(server::entity::user::Column::Username.eq(username))
        .one(&app.db)
        .await
        .unwrap()
        .unwrap()
        .id
}

/// A running public contest of the plugin's type with one public problem and
/// one judged submission of the registered contestant, and a contest of
/// another type.
async fn fixture(app: &TestApp) -> Fixture {
    let admin = app
        .create_user_with_role("outage_admin", "password", "admin")
        .await;
    let contestant = app
        .create_authenticated_user("outage_user", "password")
        .await;
    let governed = app.create_contest(&admin, "Governed", true, true).await;
    let other = app.create_contest(&admin, "Other", true, true).await;
    let problem = app.insert_public_problem("Shared Problem").await;
    app.add_problem_to_contest(governed, problem, &admin).await;
    app.register_for_contest(governed, &contestant).await;
    for sql in [
        format!(
            "UPDATE plugin SET governs = '{{\"visibility\": true, \"contest_types\": [\"{GOVERNED_TYPE}\"], \"problem_types\": [], \"checker_formats\": [], \"languages\": [], \"unchecked_problem_types\": [], \"hooks\": [], \"judging_inputs\": [], \"checker_languages\": []}}' \
             WHERE id = '{PLUGIN}'"
        ),
        // The replica that loaded the plugin registered the type, so it
        // reports the type loaded.
        format!(
            "UPDATE plugin_replica SET provides = jsonb_set(\
               COALESCE(provides::jsonb, '{{\"contest_types\": {{}}}}'::jsonb), \
               '{{contest_types,{GOVERNED_TYPE}}}', '[\"{PLUGIN}\"]') \
             WHERE plugins::jsonb -> '{PLUGIN}' ->> 'status' = 'loaded'"
        ),
        format!("UPDATE contest SET contest_type = '{GOVERNED_TYPE}' WHERE id = {governed}"),
    ] {
        exec(app, sql).await;
    }
    let submission = app
        .insert_submission(server::entity::submission::ActiveModel {
            files: Set(json!([{"filename": "main.cpp", "content": "int main() {}"}])),
            language: Set("cpp".into()),
            user_id: Set(user_id(app, "outage_user").await),
            problem_id: Set(problem),
            contest_id: Set(Some(governed)),
            contest_type: Set(GOVERNED_TYPE.into()),
            ..Default::default()
        })
        .await
        .id;
    exec(
        app,
        format!(
            "UPDATE submission SET status = 'Judged', verdict = 'Accepted', score = 100 \
             WHERE id = {submission}"
        ),
    )
    .await;
    Fixture {
        admin,
        contestant,
        governed,
        other,
        problem,
        submission,
    }
}

async fn disable(app: &TestApp, fx: &Fixture) {
    let res = app
        .post_with_token(&routes::admin_plugin_disable(PLUGIN), &json!({}), &fx.admin)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    plugin_lifecycle::reconcile(&app.state).await.unwrap();
}

fn disabled() -> Value {
    json!({"state": "disabled", "plugin_id": PLUGIN})
}

fn withheld_by(outage: Value) -> Value {
    json!({"reason": "plugin_outage", "outage": outage})
}

fn row(list: &Value, id: i32) -> &Value {
    list.as_array()
        .unwrap_or_else(|| panic!("not a list: {list}"))
        .iter()
        .find(|row| row["id"] == id)
        .unwrap_or_else(|| panic!("row {id} missing from {list}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disabled_contest_plugin_withholds_only_what_it_governs() {
    let app = TestApp::spawn_with_plugins().await;
    let fx = fixture(&app).await;
    disable(&app, &fx).await;
    let token = &fx.contestant;

    // Contests stay listed and readable, and say their plugin is out.
    let res = app.get_with_token(routes::CONTESTS, token).await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert_eq!(
        row(&res.body["data"], fx.governed)["plugin_outage"],
        disabled()
    );
    assert!(row(&res.body["data"], fx.other)["plugin_outage"].is_null());
    let res = app
        .get_with_token(&routes::contest(fx.governed), token)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert_eq!(res.body["plugin_outage"], disabled());
    assert_eq!(res.body["title"], "Governed");
    let res = app
        .get_with_token(&routes::contest_participants(fx.governed), token)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);

    // The problem list keeps every row with the contest's own fields only.
    let res = app
        .get_with_token(&routes::contest_problems(fx.governed), token)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    let problems = res.body.as_array().unwrap();
    assert_eq!(problems.len(), 1, "{}", res.text);
    assert_eq!(problems[0]["problem_id"], fx.problem);
    assert_eq!(problems[0]["label"], "A");
    assert!(problems[0]["problem_title"].is_null(), "{}", res.text);
    assert_eq!(problems[0]["withheld"], withheld_by(disabled()));

    // A problem read through the contest is entirely the plugin's to decide.
    let res = app
        .get_with_token(&routes::contest_problem(fx.governed, fx.problem), token)
        .await;
    assert_eq!(res.status, 503, "{}", res.text);
    assert_eq!(res.body["code"], "PLUGIN_DISABLED");
    assert_eq!(
        res.headers.get("retry-after").map(|v| v.to_str().unwrap()),
        Some(
            server::error::DISABLED_RETRY_AFTER_SECS
                .to_string()
                .as_str()
        )
    );

    // The same problem read on its own is not governed by the contest plugin.
    let res = app
        .get_with_token(&routes::problem(fx.problem), token)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert_eq!(res.body["title"], "Shared Problem");

    // Submissions keep their rows; only their outcome is withheld.
    for path in [
        routes::contest_submissions(fx.governed),
        format!("{}?problem_id={}", routes::SUBMISSIONS, fx.problem),
        routes::SUBMISSIONS.to_string(),
    ] {
        let res = app.get_with_token(&path, token).await;
        assert_eq!(res.status, 200, "{path}: {}", res.text);
        let sub = row(&res.body["data"], fx.submission);
        assert!(sub["status"].is_null(), "{path}: {sub}");
        assert!(sub["verdict"].is_null(), "{path}: {sub}");
        assert!(sub["score"].is_null(), "{path}: {sub}");
        assert_eq!(sub["problem_id"], fx.problem, "{path}: {sub}");
        assert_eq!(sub["withheld"], withheld_by(disabled()), "{path}: {sub}");
    }
    let res = app
        .get_with_token(&routes::submission(fx.submission), token)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert!(res.body["status"].is_null(), "{}", res.text);
    assert!(res.body["result"].is_null(), "{}", res.text);
    assert_eq!(res.body["language"], "cpp");
    assert_eq!(res.body["withheld"], withheld_by(disabled()));

    // Another contest type is untouched.
    let res = app
        .get_with_token(&routes::contest_submissions(fx.other), token)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
}

#[tokio::test(flavor = "multi_thread")]
async fn staff_keep_the_full_view_while_a_contest_plugin_is_disabled() {
    let app = TestApp::spawn_with_plugins().await;
    let fx = fixture(&app).await;
    disable(&app, &fx).await;

    let res = app
        .get_with_token(&routes::contest_submissions(fx.governed), &fx.admin)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    let sub = row(&res.body["data"], fx.submission);
    assert_eq!(sub["status"], "Judged", "{sub}");
    assert_eq!(sub["verdict"], "Accepted", "{sub}");
    assert!(sub["withheld"].is_null(), "{sub}");
    let res = app
        .get_with_token(&routes::contest_problems(fx.governed), &fx.admin)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert_eq!(res.body[0]["problem_title"], "Shared Problem");
    let res = app
        .get_with_token(&routes::contest_problem(fx.governed, fx.problem), &fx.admin)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    // The contest still says its plugin is out: its submissions are refused.
    let res = app
        .get_with_token(&routes::contest(fx.governed), &fx.admin)
        .await;
    assert_eq!(res.body["plugin_outage"], disabled());
}

/// An enabled plugin missing on this replica while another serves it is
/// named: the same rows, marked as missing here, and a retryable 503 naming
/// it where the whole answer is the plugin's.
#[tokio::test(flavor = "multi_thread")]
async fn a_missing_contest_plugin_marks_what_it_governs_unavailable() {
    let a = TestApp::spawn_with_plugins().await;
    let fx = fixture(&a).await;
    let b_dir = tempfile::tempdir().unwrap();
    let b = a
        .spawn_replica(SpawnOptions {
            plugins_dir: Some(b_dir.path().to_path_buf()),
            allow_plugin_activation_failures: true,
            ..Default::default()
        })
        .await;
    plugin_lifecycle::reconcile(&b.state).await.unwrap();
    // Replica a serves the plugin: b lacks it, by name.
    let unavailable = json!({"state": "not_loaded_here", "plugin_id": "server-plugin"});

    let res = b
        .get_with_token(&routes::contest(fx.governed), &fx.contestant)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert_eq!(res.body["plugin_outage"], unavailable);
    let res = b
        .get_with_token(&routes::contest_submissions(fx.governed), &fx.contestant)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert_eq!(
        row(&res.body["data"], fx.submission)["withheld"],
        withheld_by(unavailable)
    );
    let res = b
        .get_with_token(
            &routes::contest_problem(fx.governed, fx.problem),
            &fx.contestant,
        )
        .await;
    assert_eq!(res.status, 503, "{}", res.text);
    assert_eq!(res.body["code"], "PLUGIN_NOT_LOADED_HERE");
    assert_eq!(res.body["details"]["plugin_id"], "server-plugin");
    assert!(res.headers.contains_key("retry-after"));
}
