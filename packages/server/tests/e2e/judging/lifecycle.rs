use chrono::Utc;
use common::{SubmissionStatus, Verdict};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use serde_json::json;
use server::entity::{submission, user};

use crate::common::E2eTestApp;

async fn seed_contest_submission(
    app: &E2eTestApp,
    username: &str,
    problem_id: i32,
    contest_id: i32,
    contest_type: &str,
) -> i32 {
    let user_model = user::Entity::find()
        .filter(user::Column::Username.eq(username))
        .one(&app.db)
        .await
        .expect("query contestant")
        .expect("contestant should exist");
    let now = Utc::now();
    submission::ActiveModel {
        files: Set(json!([{ "filename": "main.cpp", "content": CPP_SUM }])),
        language: Set("cpp".into()),
        user_id: Set(user_model.id),
        problem_id: Set(problem_id),
        contest_id: Set(Some(contest_id)),
        contest_type: Set(contest_type.into()),
        status: Set(SubmissionStatus::Judged),
        verdict: Set(Some(Verdict::Accepted)),
        score: Set(Some(1.0)),
        judge_epoch: Set(1),
        max_judge_epoch: Set(1),
        created_at: Set(now),
        judged_at: Set(Some(now)),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert contest submission")
    .id
}

#[tokio::test(flavor = "multi_thread")]
async fn icpc_contest_full_lifecycle() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("icpc_lc_admin", "pass1234", "admin")
        .await;
    let user1 = app
        .create_authenticated_user("icpc_lc_user1", "pass1234")
        .await;
    let user2 = app
        .create_authenticated_user("icpc_lc_user2", "pass1234")
        .await;

    let p1 = app.create_problem(&admin, "ICPC P1").await;
    app.create_test_case_with(p1, "3\n1 2 3", "6", 10, true, &admin)
        .await;

    let p2 = app.create_problem(&admin, "ICPC P2").await;
    app.create_test_case_with(p2, "2\n10 20", "30", 10, true, &admin)
        .await;

    let contest_id = app
        .create_typed_contest(&admin, "ICPC Lifecycle", "icpc", true, true)
        .await;
    app.add_problem_to_contest_with_label(contest_id, p1, "A", &admin)
        .await;
    app.add_problem_to_contest_with_label(contest_id, p2, "B", &admin)
        .await;

    app.register_for_contest(contest_id, &user1).await;
    app.register_for_contest(contest_id, &user2).await;

    seed_contest_submission(&app, "icpc_lc_user1", p1, contest_id, "icpc").await;
    seed_contest_submission(&app, "icpc_lc_user1", p2, contest_id, "icpc").await;
    seed_contest_submission(&app, "icpc_lc_user2", p1, contest_id, "icpc").await;
    seed_contest_submission(&app, "icpc_lc_user2", p2, contest_id, "icpc").await;

    let contest_res = app
        .get_with_token(&format!("/api/v1/contests/{contest_id}"), &admin)
        .await;
    assert_eq!(contest_res.status, 200);
    assert_eq!(
        contest_res.body["contest_type"].as_str().unwrap_or(""),
        "icpc"
    );

    let list = app
        .get_with_token(
            &format!("/api/v1/contests/{contest_id}/submissions"),
            &admin,
        )
        .await;
    assert_eq!(list.status, 200);
    let total = list.body["data"].as_array().unwrap().len() as u64;
    assert!(
        total >= 4,
        "Contest should have at least 4 submissions, got {total}"
    );

    let standings = app
        .get_with_token(
            &format!("/api/v1/p/icpc/api/plugins/icpc/contests/{contest_id}/standings"),
            &admin,
        )
        .await;
    assert_eq!(
        standings.status, 200,
        "Standings endpoint should return 200, got {}",
        standings.status
    );

    let my_info = app
        .get_with_token(&format!("/api/v1/contests/{contest_id}/me"), &user1)
        .await;
    assert_eq!(my_info.status, 200);
    assert!(
        my_info.body["is_registered"].as_bool().unwrap(),
        "User1 should be registered"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ioi_contest_full_lifecycle() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("ioi_lc_admin", "pass1234", "admin")
        .await;
    let user1 = app
        .create_authenticated_user("ioi_lc_user1", "pass1234")
        .await;
    let user2 = app
        .create_authenticated_user("ioi_lc_user2", "pass1234")
        .await;

    let p1 = app.create_problem(&admin, "IOI P1").await;
    app.create_test_case_with(p1, "3\n1 2 3", "6", 50, true, &admin)
        .await;
    app.create_test_case_with(p1, "4\n1 2 3 4", "10", 50, false, &admin)
        .await;

    let p2 = app.create_problem(&admin, "IOI P2").await;
    app.create_test_case_with(p2, "2\n5 5", "10", 100, true, &admin)
        .await;

    let contest_id = app
        .create_typed_contest(&admin, "IOI Lifecycle", "ioi", true, true)
        .await;
    app.add_problem_to_contest_with_label(contest_id, p1, "A", &admin)
        .await;
    app.add_problem_to_contest_with_label(contest_id, p2, "B", &admin)
        .await;

    app.register_for_contest(contest_id, &user1).await;
    app.register_for_contest(contest_id, &user2).await;

    seed_contest_submission(&app, "ioi_lc_user1", p1, contest_id, "ioi").await;
    seed_contest_submission(&app, "ioi_lc_user1", p2, contest_id, "ioi").await;
    seed_contest_submission(&app, "ioi_lc_user2", p1, contest_id, "ioi").await;
    seed_contest_submission(&app, "ioi_lc_user2", p2, contest_id, "ioi").await;

    let contest_res = app
        .get_with_token(&format!("/api/v1/contests/{contest_id}"), &admin)
        .await;
    assert_eq!(contest_res.status, 200);
    assert_eq!(
        contest_res.body["contest_type"].as_str().unwrap_or(""),
        "ioi"
    );

    let list = app
        .get_with_token(
            &format!("/api/v1/contests/{contest_id}/submissions"),
            &admin,
        )
        .await;
    assert_eq!(list.status, 200);
    let total = list.body["data"].as_array().unwrap().len() as u64;
    assert!(
        total >= 4,
        "Contest should have at least 4 submissions, got {total}"
    );

    let scoreboard = app
        .get_with_token(
            &format!("/api/v1/p/ioi/api/plugins/ioi/contests/{contest_id}/scoreboard"),
            &admin,
        )
        .await;
    assert_eq!(
        scoreboard.status, 200,
        "Scoreboard endpoint should return 200, got {}",
        scoreboard.status
    );

    let my_info = app
        .get_with_token(&format!("/api/v1/contests/{contest_id}/me"), &user1)
        .await;
    assert_eq!(my_info.status, 200);
    assert!(my_info.body["is_registered"].as_bool().unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn mixed_contest_with_multiple_problems_and_contestants() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("mixed_lc_admin", "pass1234", "admin")
        .await;
    let user1 = app
        .create_authenticated_user("mixed_lc_u1", "pass1234")
        .await;
    let user2 = app
        .create_authenticated_user("mixed_lc_u2", "pass1234")
        .await;
    let user3 = app
        .create_authenticated_user("mixed_lc_u3", "pass1234")
        .await;

    let p1 = app.create_problem(&admin, "Mixed P1").await;
    app.create_test_case_with(p1, "1\n5", "5", 10, true, &admin)
        .await;

    let p2 = app.create_problem(&admin, "Mixed P2").await;
    app.create_test_case_with(p2, "2\n3 7", "10", 10, true, &admin)
        .await;
    app.create_test_case_with(p2, "3\n1 1 1", "3", 10, false, &admin)
        .await;

    let p3 = app.create_problem(&admin, "Mixed P3").await;
    app.create_test_case_with(p3, "1\n100", "100", 10, true, &admin)
        .await;
    app.create_test_case_with(p3, "2\n50 50", "100", 10, false, &admin)
        .await;
    app.create_test_case_with(p3, "3\n10 20 30", "60", 10, false, &admin)
        .await;

    let contest_id = app
        .create_contest(&admin, "Mixed Lifecycle", true, true)
        .await;
    app.add_problem_to_contest_with_label(contest_id, p1, "A", &admin)
        .await;
    app.add_problem_to_contest_with_label(contest_id, p2, "B", &admin)
        .await;
    app.add_problem_to_contest_with_label(contest_id, p3, "C", &admin)
        .await;

    app.register_for_contest(contest_id, &user1).await;
    app.register_for_contest(contest_id, &user2).await;
    app.register_for_contest(contest_id, &user3).await;

    seed_contest_submission(&app, "mixed_lc_u1", p1, contest_id, "icpc").await;
    seed_contest_submission(&app, "mixed_lc_u1", p2, contest_id, "icpc").await;
    seed_contest_submission(&app, "mixed_lc_u1", p3, contest_id, "icpc").await;
    seed_contest_submission(&app, "mixed_lc_u2", p1, contest_id, "icpc").await;
    seed_contest_submission(&app, "mixed_lc_u2", p3, contest_id, "icpc").await;
    seed_contest_submission(&app, "mixed_lc_u3", p2, contest_id, "icpc").await;

    let list = app
        .get_with_token(
            &format!("/api/v1/contests/{contest_id}/submissions"),
            &admin,
        )
        .await;
    assert_eq!(list.status, 200);
    let total = list.body["data"].as_array().unwrap().len() as u64;
    assert_eq!(
        total, 6,
        "Contest should have exactly 6 submissions, got {total}"
    );

    let p2_list = app
        .get_with_token(
            &format!("/api/v1/contests/{contest_id}/submissions?problem_id={p2}"),
            &admin,
        )
        .await;
    assert_eq!(p2_list.status, 200);
    let p2_total = p2_list.body["data"].as_array().unwrap().len() as u64;
    assert_eq!(
        p2_total, 2,
        "Problem B should have 2 submissions (user1 + user3), got {p2_total}"
    );

    let problems = app
        .get_with_token(&format!("/api/v1/contests/{contest_id}/problems"), &admin)
        .await;
    assert_eq!(problems.status, 200);
    let problem_data = problems.body.as_array().unwrap();
    assert_eq!(
        problem_data.len(),
        3,
        "Contest should have 3 problems, got {}",
        problem_data.len()
    );

    for (token, name) in [(&user1, "user1"), (&user2, "user2"), (&user3, "user3")] {
        let info = app
            .get_with_token(&format!("/api/v1/contests/{contest_id}/me"), token)
            .await;
        assert_eq!(info.status, 200, "{name} should access contest my-info");
        assert!(
            info.body["is_registered"].as_bool().unwrap(),
            "{name} should be registered"
        );
    }

    let participants = app
        .get_with_token(
            &format!("/api/v1/contests/{contest_id}/participants"),
            &admin,
        )
        .await;
    assert_eq!(participants.status, 200);
    let participant_data = participants.body.as_array().unwrap();
    assert_eq!(
        participant_data.len(),
        3,
        "Contest should have 3 participants, got {}",
        participant_data.len()
    );
}

use super::CPP_SUM;

/// The problem, public, with one test case, under `title`, and a contestant.
async fn problem_with_checker_format(
    app: &E2eTestApp,
    admin: &str,
    title: &str,
    format: &str,
) -> i32 {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let problem_id = app.create_problem(admin, title).await;
    app.create_test_case_with(problem_id, "5\n1 2 3 4 5", "15", 100, true, admin)
        .await;
    let res = app
        .patch_with_token(
            &format!("/api/v1/problems/{problem_id}"),
            &json!({"is_public": true}),
            admin,
        )
        .await;
    assert!(res.status < 300, "{}", res.text);
    app.db
        .execute_raw(Statement::from_string(
            DbBackend::Postgres,
            format!("UPDATE problem SET checker_format = '{format}' WHERE id = {problem_id}"),
        ))
        .await
        .unwrap();
    problem_id
}

/// Work whose checker plugin is enabled fleet-wide but loaded on no replica
/// waits for a replica to load it, as work of a disabled plugin does: a
/// submission is refused, retryably, naming the plugin
/// (`503 PLUGIN_NOT_LOADED` with `Retry-After`), and none is taken to fail.
#[tokio::test(flavor = "multi_thread")]
async fn a_submission_whose_plugin_is_loaded_nowhere_is_refused_until_one_loads_it() {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("nowhere_admin", "pass1234", "admin")
        .await;
    let problem_id =
        problem_with_checker_format(&app, &admin, "Checker loaded nowhere", "absent-format").await;
    app.db
        .execute_raw(Statement::from_string(
            DbBackend::Postgres,
            "INSERT INTO plugin (id, is_enabled, generation, updated_at, governs) VALUES \
             ('absent-checker', TRUE, 1, NOW(), '{\"visibility\": false, \"contest_types\": [], \
               \"problem_types\": [], \"checker_formats\": [\"absent-format\"], \
               \"languages\": [], \"unchecked_problem_types\": [], \"hooks\": [], \"judging_inputs\": []}')"
                .to_string(),
        ))
        .await
        .unwrap();
    // Converge on the table; the recorded plugin cannot load here (it has
    // no files), which is the point.
    assert_eq!(app.reconcile_plugins_failing().await, ["absent-checker"]);
    let contestant = app
        .create_authenticated_user("nowhere_user", "pass1234")
        .await;

    let res = app
        .post_with_token(
            &format!("/api/v1/problems/{problem_id}/submissions"),
            &json!({
                "language": "cpp",
                "files": [{"filename": "main.cpp", "content": CPP_SUM}],
            }),
            &contestant,
        )
        .await;
    assert_eq!(res.status, 503, "{}", res.text);
    assert_eq!(res.body["code"], "PLUGIN_NOT_LOADED", "{}", res.text);
    assert!(res.text.contains("absent-checker"), "{}", res.text);
    assert!(res.headers.contains_key("retry-after"), "{:?}", res.headers);
}

/// A contest whose type's fleet-wide provider is enabled but loaded on no
/// replica names that plugin wherever it is out: a submission to the
/// contest is refused, retryably, as `PLUGIN_NOT_LOADED` with the plugin in
/// its details, and a submission already in the contest is withheld as
/// waiting for that plugin to load, not as a brief outage of this server.
#[tokio::test(flavor = "multi_thread")]
async fn a_contest_whose_plugin_is_loaded_nowhere_names_it_wherever_it_is_out() {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("nowhere_ct_admin", "pass1234", "admin")
        .await;
    let problem_id = app.create_problem(&admin, "Contest loaded nowhere").await;
    app.create_test_case(problem_id, &admin).await;
    let contest_id = app
        .create_typed_contest(&admin, "Loaded nowhere", "icpc", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    let contestant = app
        .create_authenticated_user("nowhere_ct_user", "pass1234")
        .await;
    app.register_for_contest(contest_id, &contestant).await;
    let queued =
        seed_contest_submission(&app, "nowhere_ct_user", problem_id, contest_id, "icpc").await;

    // A plugin whose id sorts before the shipped one provides the type, so
    // it is the type's fleet-wide provider; it has no files, so it loads on
    // no replica.
    app.db
        .execute_raw(Statement::from_string(
            DbBackend::Postgres,
            "INSERT INTO plugin (id, is_enabled, generation, updated_at, governs) VALUES \
             ('aaa-judge', TRUE, 1, NOW(), '{\"visibility\": true, \"contest_types\": [\"icpc\"], \
               \"problem_types\": [], \"checker_formats\": [], \
               \"languages\": [], \"unchecked_problem_types\": [], \"hooks\": [], \"judging_inputs\": []}')"
                .to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(app.reconcile_plugins_failing().await, ["aaa-judge"]);

    let res = app
        .post_with_token(
            &format!("/api/v1/contests/{contest_id}/problems/{problem_id}/submissions"),
            &json!({
                "language": "cpp",
                "files": [{"filename": "main.cpp", "content": CPP_SUM}],
            }),
            &contestant,
        )
        .await;
    assert_eq!(res.status, 503, "{}", res.text);
    assert_eq!(res.body["code"], "PLUGIN_NOT_LOADED", "{}", res.text);
    assert_eq!(
        res.body["details"]["plugin_id"], "aaa-judge",
        "{}",
        res.text
    );
    assert!(res.headers.contains_key("retry-after"), "{:?}", res.headers);

    let view = app
        .get_with_token(&format!("/api/v1/submissions/{queued}"), &contestant)
        .await;
    assert_eq!(view.status, 200, "{}", view.text);
    assert_eq!(
        view.body["withheld"],
        json!({
            "reason": "plugin_outage",
            "outage": {"state": "not_loaded", "plugin_id": "aaa-judge"},
        }),
        "{}",
        view.text
    );
}

/// Work needing a key no enabled plugin provides (a checker format nothing
/// registered) fails on its first attempt and stands: no replica will ever
/// run it. Its system error names the role and the key (`NO_PROVIDER`); it
/// is judged once, never retried.
#[tokio::test(flavor = "multi_thread")]
async fn work_whose_key_no_plugin_provides_stands_at_once_naming_it() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("unprovided_admin", "pass1234", "admin")
        .await;
    let problem_id =
        problem_with_checker_format(&app, &admin, "Checker nobody provides", "absent-format").await;
    let contestant = app
        .create_authenticated_user("unprovided_user", "pass1234")
        .await;

    let id = app
        .create_submission(problem_id, &contestant, "cpp", CPP_SUM)
        .await;
    app.wait_for_submission_terminal(id, &contestant, 120).await;
    app.judging_idle().await;

    let staff = app
        .get_with_token(&format!("/api/v1/submissions/{id}"), &admin)
        .await;
    assert_eq!(staff.status, 200, "{}", staff.text);
    let result = &staff.body["result"];
    assert_eq!(result["verdict"], "SystemError", "{}", staff.text);
    assert_eq!(result["error_code"], "NO_PROVIDER", "{}", staff.text);
    assert_eq!(
        result["error_message"], "no checker plugin provides checker format \"absent-format\"",
        "{}",
        staff.text
    );
    let row = submission::Entity::find_by_id(id)
        .one(&app.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.system_error_retries, 0, "never retried");
    assert_eq!(row.judge_epoch, 0, "judged once");
}
