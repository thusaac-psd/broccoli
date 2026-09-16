use broccoli_server_sdk::permissions as perm;
use chrono::{Duration, TimeZone, Utc};
use common::{SubmissionStatus, Verdict};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use serde_json::json;
use server::entity::{contest, plugin_storage, submission, submission_judgement, test_case_result, user};

use crate::common::E2eTestApp;

async fn seed_accepted_icpc_submission(
    app: &E2eTestApp,
    username: &str,
    problem_id: i32,
    contest_id: i32,
) -> i32 {
    let user_model = user::Entity::find()
        .filter(user::Column::Username.eq(username))
        .one(&app.db)
        .await
        .expect("query contestant")
        .expect("contestant should exist");
    let now = Utc::now();
    let submission = submission::ActiveModel {
        files: Set(json!([{ "filename": "main.cpp", "content": "int main() { return 0; }" }])),
        language: Set("cpp".into()),
        user_id: Set(user_model.id),
        problem_id: Set(problem_id),
        contest_id: Set(Some(contest_id)),
        contest_type: Set("icpc".into()),
        status: Set(SubmissionStatus::Judged),
        verdict: Set(Some(Verdict::Accepted)),
        score: Set(Some(1.0)),
        judge_epoch: Set(1),
        created_at: Set(now),
        judged_at: Set(Some(now)),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert ICPC submission");

    submission_judgement::ActiveModel {
        submission_id: Set(submission.id),
        version: Set(1),
        is_current: Set(true),
        is_finalized: Set(true),
        triggered_by_user_id: Set(None),
        status: Set(SubmissionStatus::Judged),
        verdict: Set(Some(Verdict::Accepted)),
        score: Set(Some(1.0)),
        judge_epoch: Set(1),
        created_at: Set(now),
        finalized_at: Set(Some(now)),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert ICPC judgement");

    plugin_storage::ActiveModel {
        plugin_id: Set("icpc".into()),
        collection: Set("default".into()),
        key: Set(format!(
            "standings:{contest_id}:{}:{problem_id}",
            user_model.id
        )),
        data: Set(json!(
            serde_json::to_string(&json!({
                "attempts": 1,
                "solved": true,
                "solve_time_ms": 60_000
            }))
            .unwrap()
        )),
        created_at: Set(now),
    }
    .insert(&app.db)
    .await
    .expect("insert ICPC standings state");

    submission.id
}

async fn user_id_by_username(app: &E2eTestApp, username: &str) -> i32 {
    user::Entity::find()
        .filter(user::Column::Username.eq(username))
        .one(&app.db)
        .await
        .expect("query contestant")
        .expect("contestant should exist")
        .id
}

async fn seed_current_icpc_submission(
    app: &E2eTestApp,
    user_id: i32,
    problem_id: i32,
    contest_id: i32,
    verdict: Verdict,
    created_at: chrono::DateTime<Utc>,
    version: i32,
) -> i32 {
    let accepted = verdict == Verdict::Accepted;
    let submission = submission::ActiveModel {
        files: Set(json!([{ "filename": "main.cpp", "content": "int main() { return 0; }" }])),
        language: Set("cpp".into()),
        user_id: Set(user_id),
        problem_id: Set(problem_id),
        contest_id: Set(Some(contest_id)),
        contest_type: Set("icpc".into()),
        status: Set(SubmissionStatus::Judged),
        verdict: Set(Some(verdict.clone())),
        score: Set(Some(if accepted { 1.0 } else { 0.0 })),
        judge_epoch: Set(version),
        created_at: Set(created_at),
        judged_at: Set(Some(Utc::now())),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert ICPC submission");

    submission_judgement::ActiveModel {
        submission_id: Set(submission.id),
        version: Set(version),
        is_current: Set(true),
        is_finalized: Set(true),
        triggered_by_user_id: Set(None),
        status: Set(SubmissionStatus::Judged),
        verdict: Set(Some(verdict)),
        score: Set(Some(if accepted { 1.0 } else { 0.0 })),
        judge_epoch: Set(version),
        created_at: Set(created_at),
        finalized_at: Set(Some(Utc::now())),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert ICPC judgement");

    submission.id
}

async fn seed_icpc_standings_state(
    app: &E2eTestApp,
    contest_id: i32,
    user_id: i32,
    problem_id: i32,
    attempts: i32,
    solved: bool,
    solve_time_ms: Option<i64>,
) {
    plugin_storage::ActiveModel {
        plugin_id: Set("icpc".into()),
        collection: Set("default".into()),
        key: Set(format!("standings:{contest_id}:{user_id}:{problem_id}")),
        data: Set(json!(
            serde_json::to_string(&json!({
                "attempts": attempts,
                "solved": solved,
                "solve_time_ms": solve_time_ms
            }))
            .unwrap()
        )),
        created_at: Set(Utc::now()),
    }
    .insert(&app.db)
    .await
    .expect("insert stale ICPC standings state");
}

#[tokio::test(flavor = "multi_thread")]
async fn icpc_contest_type_registered() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("icpc_admin1", "password", "admin")
        .await;
    let contest_id = app
        .create_typed_contest(&admin, "ICPC Contest 1", "icpc", true, true)
        .await;
    assert!(contest_id > 0, "Should successfully create an ICPC contest");
}

#[tokio::test(flavor = "multi_thread")]
async fn icpc_standings_reflects_judged_submission() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("icpc_admin3", "password", "admin")
        .await;
    let contestant = app
        .create_authenticated_user("icpc_user3", "password")
        .await;

    let problem_id = app.create_problem(&admin, "ICPC Problem 3").await;

    let contest_id = app
        .create_typed_contest(&admin, "ICPC Contest 3", "icpc", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    app.register_for_contest(contest_id, &contestant).await;

    seed_accepted_icpc_submission(&app, "icpc_user3", problem_id, contest_id).await;

    let standings_path = format!("/api/v1/p/icpc/api/plugins/icpc/contests/{contest_id}/standings");
    let res = app.get_with_token(&standings_path, &contestant).await;
    assert_eq!(res.status, 200, "Standings request failed: {}", res.text);

    let rows = &res.body["rows"];
    assert!(rows.is_array(), "rows should be an array");
    let rows_arr = rows.as_array().unwrap();
    assert!(
        !rows_arr.is_empty(),
        "Standings should have at least one entry after a judged submission"
    );
    assert!(
        rows_arr[0]["username"].is_string(),
        "Row should have a username"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn icpc_standings_uses_current_judgements_instead_of_stale_storage() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("icpc_admin_rejudge1", "password", "admin")
        .await;
    let contestant = app
        .create_authenticated_user("icpc_user_rejudge1", "password")
        .await;

    let problem_id = app.create_problem(&admin, "ICPC Rejudge Problem 1").await;
    let contest_id = app
        .create_typed_contest(&admin, "ICPC Rejudge Contest 1", "icpc", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    app.register_for_contest(contest_id, &contestant).await;

    let user_id = user_id_by_username(&app, "icpc_user_rejudge1").await;
    let submitted_at = Utc.with_ymd_and_hms(2020, 1, 1, 0, 10, 0).unwrap();
    seed_current_icpc_submission(
        &app,
        user_id,
        problem_id,
        contest_id,
        Verdict::WrongAnswer,
        submitted_at,
        2,
    )
    .await;
    seed_icpc_standings_state(&app, contest_id, user_id, problem_id, 0, true, Some(60_000)).await;

    let standings_path = format!("/api/v1/p/icpc/api/plugins/icpc/contests/{contest_id}/standings");
    let res = app.get_with_token(&standings_path, &admin).await;
    assert_eq!(res.status, 200, "Standings request failed: {}", res.text);

    let row = res.body["rows"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["username"].as_str() == Some("icpc_user_rejudge1"))
        })
        .expect("contestant row should exist");
    assert_eq!(row["solved"].as_i64(), Some(0), "{}", res.text);
    assert_eq!(
        row["problems"]["A"]["solved"].as_bool(),
        Some(false),
        "{}",
        res.text
    );
    assert_eq!(
        row["problems"]["A"]["attempts"].as_i64(),
        Some(1),
        "{}",
        res.text
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn icpc_standings_uses_submission_time_for_accept_penalty() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("icpc_admin_rejudge2", "password", "admin")
        .await;
    let contestant = app
        .create_authenticated_user("icpc_user_rejudge2", "password")
        .await;

    let problem_id = app.create_problem(&admin, "ICPC Rejudge Problem 2").await;
    let contest_id = app
        .create_typed_contest(&admin, "ICPC Rejudge Contest 2", "icpc", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    app.register_for_contest(contest_id, &contestant).await;

    let user_id = user_id_by_username(&app, "icpc_user_rejudge2").await;
    let contest_start = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
    seed_current_icpc_submission(
        &app,
        user_id,
        problem_id,
        contest_id,
        Verdict::WrongAnswer,
        contest_start + Duration::minutes(3),
        1,
    )
    .await;
    seed_current_icpc_submission(
        &app,
        user_id,
        problem_id,
        contest_id,
        Verdict::Accepted,
        contest_start + Duration::minutes(10),
        1,
    )
    .await;
    seed_icpc_standings_state(&app, contest_id, user_id, problem_id, 0, true, Some(60_000)).await;

    let standings_path = format!("/api/v1/p/icpc/api/plugins/icpc/contests/{contest_id}/standings");
    let res = app.get_with_token(&standings_path, &admin).await;
    assert_eq!(res.status, 200, "Standings request failed: {}", res.text);

    let row = res.body["rows"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["username"].as_str() == Some("icpc_user_rejudge2"))
        })
        .expect("contestant row should exist");
    assert_eq!(row["solved"].as_i64(), Some(1), "{}", res.text);
    assert_eq!(row["penalty"].as_i64(), Some(30), "{}", res.text);
    assert_eq!(
        row["problems"]["A"]["time"].as_i64(),
        Some(10),
        "{}",
        res.text
    );
    assert_eq!(
        row["problems"]["A"]["attempts"].as_i64(),
        Some(1),
        "{}",
        res.text
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn icpc_contest_info_returns_metadata() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("icpc_admin4", "password", "admin")
        .await;
    let contestant = app
        .create_authenticated_user("icpc_user4", "password")
        .await;

    let contest_id = app
        .create_typed_contest(&admin, "ICPC Contest 4", "icpc", true, true)
        .await;
    app.register_for_contest(contest_id, &contestant).await;

    let info_path = format!("/api/v1/p/icpc/api/plugins/icpc/contests/{contest_id}/info");
    let res = app.get_with_token(&info_path, &contestant).await;
    assert_eq!(res.status, 200, "Contest info request failed: {}", res.text);
    assert!(
        res.body["penalty_minutes"].is_number(),
        "penalty_minutes should be present: {}",
        res.text
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn icpc_config_penalty_minutes() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("icpc_admin5", "password", "admin")
        .await;

    let contest_id = app
        .create_typed_contest(&admin, "ICPC Contest 5", "icpc", true, true)
        .await;

    let config_path = format!("/api/v1/contests/{contest_id}/config/icpc/contest");
    let put_res = app
        .put_with_token(
            &config_path,
            &json!({
                "config": {
                    "penalty_minutes": 30,
                    "count_compile_error": true,
                    "show_test_details": true
                },
                "enabled": true
            }),
            &admin,
        )
        .await;
    assert_eq!(
        put_res.status, 200,
        "Failed to set ICPC contest config: {}",
        put_res.text
    );

    let get_res = app.get_with_token(&config_path, &admin).await;
    assert_eq!(
        get_res.status, 200,
        "Failed to get ICPC contest config: {}",
        get_res.text
    );
    assert_eq!(get_res.body["config"]["penalty_minutes"].as_u64(), Some(30));
    assert_eq!(
        get_res.body["config"]["count_compile_error"].as_bool(),
        Some(true)
    );
    assert_eq!(
        get_res.body["config"]["show_test_details"].as_bool(),
        Some(true)
    );
}

// Pins the ICPC scoreboard freeze end to end: the redaction it depends on is
// authored by the `icpc` plugin's own `decide_visibility` export (see
// `plugins/icpc/src/lib.rs::decide_visibility_decisions`), reached through
// the generic `GET /api/v1/submissions/{id}` handler rather than a
// plugin-specific endpoint. Before this test, the entire evidence base for
// this contest-integrity feature was 2 unit tests exercising the plugin in
// isolation against a mocked host - nothing proved the mechanism was even
// wired up behind a real HTTP handler with a real registered plugin. Mirrors
// the shape of `ioi_feedback_filter_redacts_judgement_history`.
#[tokio::test(flavor = "multi_thread")]
async fn icpc_scoreboard_freeze_redacts_peer_submission_but_not_owner_or_organizer() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("icpc_freeze_admin", "password", "admin")
        .await;
    let contestant_a = app
        .create_authenticated_user("icpc_freeze_a", "password")
        .await;
    let contestant_b = app
        .create_authenticated_user("icpc_freeze_b", "password")
        .await;
    // Task 17: holds `contest:manage` WITHOUT `submission:view_all` - unlike
    // `admin` above, which holds BOTH and so cannot isolate which permission
    // a bypass actually checks. A plausible problem-setter/judge role.
    let manage_only = app
        .create_user_with_permissions(
            "icpc_freeze_manage_only",
            "password",
            &[perm::CONTEST_MANAGE],
        )
        .await;

    let problem_id = app.create_problem(&admin, "ICPC Freeze Problem").await;
    app.create_test_case(problem_id, &admin).await;

    let contest_id = app
        .create_typed_contest(&admin, "ICPC Freeze Contest", "icpc", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    app.register_for_contest(contest_id, &contestant_a).await;
    app.register_for_contest(contest_id, &contestant_b).await;
    // Host-level reachability for a non-owner, non-`submission:view_all`
    // viewer requires contest participation when `submissions_visible` is
    // true (see `visibility::host_rules::decide_submission`) - `contest:manage`
    // alone does not grant it. Without this, the request 404s before the
    // plugin's decision is ever consulted, and the assertions below would
    // not actually exercise the fix.
    app.register_for_contest(contest_id, &manage_only).await;

    // Non-zero freeze window, and public standings so the redaction under
    // test is purely the freeze mechanism, not the separate
    // hide-other-teams-during-the-contest rule that also lives in
    // `must_hide_other_submission`.
    let config_path = format!("/api/v1/contests/{contest_id}/config/icpc/contest");
    let put_res = app
        .put_with_token(
            &config_path,
            &json!({
                "config": {
                    "public_standings": true,
                    "freeze_minutes": 4
                },
                "enabled": true
            }),
            &admin,
        )
        .await;
    assert_eq!(
        put_res.status, 200,
        "Failed to set ICPC freeze config: {}",
        put_res.text
    );

    // Advance into the freeze window. There is no clock-mocking harness for
    // e2e tests, so this is done the same way the rest of this file
    // simulates "elapsed contest time": by writing concrete timestamps
    // directly. A 10-minute contest, already 8 minutes in, with a 4-minute
    // freeze: the freeze window (the final 4 minutes) opened 2 minutes ago
    // and the contest has not ended yet ("during").
    let now = Utc::now();
    let start_time = now - Duration::minutes(8);
    let end_time = now + Duration::minutes(2);
    let contest_model = contest::Entity::find_by_id(contest_id)
        .one(&app.db)
        .await
        .expect("query contest")
        .expect("contest should exist");
    let mut contest_active: contest::ActiveModel = contest_model.into();
    contest_active.start_time = Set(start_time);
    contest_active.end_time = Set(end_time);
    contest_active.activate_time = Set(Some(start_time));
    contest_active
        .update(&app.db)
        .await
        .expect("advance contest into its freeze window");

    let user_b = user::Entity::find()
        .filter(user::Column::Username.eq("icpc_freeze_b"))
        .one(&app.db)
        .await
        .expect("query contestant B")
        .expect("contestant B should exist");

    // Submitted 7 minutes after contest start: past the freeze start (6
    // minutes in, i.e. duration 10 - freeze 4) and before "now" (8 minutes
    // in), so it is a genuinely past submission sitting inside the window.
    let submitted_at = start_time + Duration::minutes(7);
    let submission_model = submission::ActiveModel {
        files: Set(json!([{ "filename": "main.cpp", "content": "int main() { return 0; }" }])),
        language: Set("cpp".into()),
        user_id: Set(user_b.id),
        problem_id: Set(problem_id),
        contest_id: Set(Some(contest_id)),
        contest_type: Set("icpc".into()),
        status: Set(SubmissionStatus::Judged),
        verdict: Set(Some(Verdict::Accepted)),
        score: Set(Some(1.0)),
        time_used: Set(Some(123)),
        memory_used: Set(Some(4096)),
        judge_epoch: Set(1),
        created_at: Set(submitted_at),
        judged_at: Set(Some(submitted_at)),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert frozen ICPC submission");
    let sub_id = submission_model.id;

    let judgement_model = submission_judgement::ActiveModel {
        submission_id: Set(sub_id),
        version: Set(1),
        is_current: Set(true),
        is_finalized: Set(true),
        triggered_by_user_id: Set(None),
        status: Set(SubmissionStatus::Judged),
        verdict: Set(Some(Verdict::Accepted)),
        score: Set(Some(1.0)),
        time_used: Set(Some(123)),
        memory_used: Set(Some(4096)),
        judge_epoch: Set(1),
        created_at: Set(submitted_at),
        finalized_at: Set(Some(submitted_at)),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert frozen ICPC judgement");

    // A non-empty test_case_results so the peer/owner distinction on this
    // field ([] vs non-empty) is meaningful rather than a no-op.
    test_case_result::ActiveModel {
        submission_id: Set(sub_id),
        judgement_id: Set(Some(judgement_model.id)),
        test_case_id: Set(None),
        verdict: Set(Verdict::Accepted),
        score: Set(1.0),
        time_used: Set(Some(123)),
        memory_used: Set(Some(4096)),
        created_at: Set(submitted_at),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert frozen ICPC test case result");

    let sub_path = format!("/api/v1/submissions/{sub_id}");

    // Peer (contestant A) reading contestant B's submission during the
    // freeze: verdict/score/time/memory blanked, and test_case_results is an
    // empty array - not null.
    let peer_res = app.get_with_token(&sub_path, &contestant_a).await;
    assert_eq!(peer_res.status, 200, "Peer read failed: {}", peer_res.text);
    assert_eq!(
        peer_res.body["result"]["verdict"],
        serde_json::Value::Null,
        "{}",
        peer_res.text
    );
    assert_eq!(
        peer_res.body["result"]["score"],
        serde_json::Value::Null,
        "{}",
        peer_res.text
    );
    assert_eq!(
        peer_res.body["result"]["time_used"],
        serde_json::Value::Null,
        "{}",
        peer_res.text
    );
    assert_eq!(
        peer_res.body["result"]["memory_used"],
        serde_json::Value::Null,
        "{}",
        peer_res.text
    );
    assert_eq!(
        peer_res.body["result"]["test_case_results"]
            .as_array()
            .map(Vec::len),
        Some(0),
        "a frozen peer must see an empty array, not null: {}",
        peer_res.text
    );

    // Owner (contestant B) reading their own submission during the same
    // freeze: a team always sees its own results, even while frozen.
    let owner_res = app.get_with_token(&sub_path, &contestant_b).await;
    assert_eq!(owner_res.status, 200, "Owner read failed: {}", owner_res.text);
    assert_eq!(
        owner_res.body["result"]["verdict"].as_str(),
        Some("Accepted"),
        "the owner must not be blanked: {}",
        owner_res.text
    );
    assert_eq!(
        owner_res.body["result"]["score"].as_f64(),
        Some(1.0),
        "{}",
        owner_res.text
    );
    assert_eq!(
        owner_res.body["result"]["time_used"].as_i64(),
        Some(123),
        "{}",
        owner_res.text
    );
    assert_eq!(
        owner_res.body["result"]["memory_used"].as_i64(),
        Some(4096),
        "{}",
        owner_res.text
    );
    assert_eq!(
        owner_res.body["result"]["test_case_results"]
            .as_array()
            .map(Vec::len),
        Some(1),
        "the owner should still see their own test case result: {}",
        owner_res.text
    );

    // Organizer (contest:manage, via the admin role) reading contestant B's
    // submission during the freeze: the ICPC rule is that organisers always
    // see the true board.
    let organizer_res = app.get_with_token(&sub_path, &admin).await;
    assert_eq!(
        organizer_res.status, 200,
        "Organizer read failed: {}",
        organizer_res.text
    );
    assert_eq!(
        organizer_res.body["result"]["verdict"].as_str(),
        Some("Accepted"),
        "an organiser must see the true board: {}",
        organizer_res.text
    );
    assert_eq!(
        organizer_res.body["result"]["score"].as_f64(),
        Some(1.0),
        "{}",
        organizer_res.text
    );
    assert_eq!(
        organizer_res.body["result"]["time_used"].as_i64(),
        Some(123),
        "{}",
        organizer_res.text
    );
    assert_eq!(
        organizer_res.body["result"]["memory_used"].as_i64(),
        Some(4096),
        "{}",
        organizer_res.text
    );

    // Task 17: a viewer holding `contest:manage` WITHOUT `submission:view_all`
    // reading contestant B's submission during the freeze. Before this task,
    // `decide_visibility_decisions` checked only `SUBMISSION_VIEW_ALL`, so
    // this viewer would have been wrongly redacted here - the `organizer_res`
    // assertions above pass via `admin`'s `SUBMISSION_VIEW_ALL` alone and
    // never actually exercise the `CONTEST_MANAGE` branch, since the default
    // `admin` role holds both permissions. This is the assertion that pins
    // the fix and would fail against pre-fix code.
    let manage_only_res = app.get_with_token(&sub_path, &manage_only).await;
    assert_eq!(
        manage_only_res.status, 200,
        "contest:manage-only read failed: {}",
        manage_only_res.text
    );
    assert_eq!(
        manage_only_res.body["result"]["verdict"].as_str(),
        Some("Accepted"),
        "a viewer with contest:manage but not submission:view_all must see the true board: {}",
        manage_only_res.text
    );
    assert_eq!(
        manage_only_res.body["result"]["score"].as_f64(),
        Some(1.0),
        "{}",
        manage_only_res.text
    );
    assert_eq!(
        manage_only_res.body["result"]["time_used"].as_i64(),
        Some(123),
        "{}",
        manage_only_res.text
    );
    assert_eq!(
        manage_only_res.body["result"]["memory_used"].as_i64(),
        Some(4096),
        "{}",
        manage_only_res.text
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker-backed e2e services plus a non-mock judge sandbox and C++ toolchain"]
async fn icpc_short_circuits_after_first_wrong_answer() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("icpc_short_admin1", "password", "admin")
        .await;
    let contestant = app
        .create_authenticated_user("icpc_short_user1", "password")
        .await;

    let problem_id = app.create_problem(&admin, "ICPC Short Circuit").await;
    let tc1 = app
        .create_test_case_with(problem_id, "1\n", "1\n", 10, true, &admin)
        .await;
    let tc2 = app
        .create_test_case_with(problem_id, "2\n", "0\n", 10, false, &admin)
        .await;
    let tc3 = app
        .create_test_case_with(problem_id, "3\n", "0\n", 10, false, &admin)
        .await;

    let contest_id = app
        .create_typed_contest(&admin, "ICPC Short Circuit Contest", "icpc", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    app.register_for_contest(contest_id, &contestant).await;

    let always_zero = r#"
#include <iostream>
int main() {
    std::cout << 0 << std::endl;
    return 0;
}
"#;
    let sub_id = app
        .create_contest_submission(contest_id, problem_id, &contestant, "cpp", always_zero)
        .await;
    let res = app.wait_for_submission_terminal(sub_id, &admin, 120).await;

    assert_eq!(
        res.body["status"].as_str(),
        Some("Judged"),
        "ICPC short-circuit submission should judge cleanly: {}",
        res.text
    );
    assert_eq!(
        res.body["result"]["verdict"].as_str(),
        Some("WrongAnswer"),
        "ICPC short-circuit submission should stop on first WA: {}",
        res.text
    );

    let results = res.body["result"]["test_case_results"]
        .as_array()
        .expect("submission response should include testcase results");
    assert_eq!(results.len(), 3, "{}", res.text);

    for (test_case_id, verdict) in [(tc1, "WrongAnswer"), (tc2, "Skipped"), (tc3, "Skipped")] {
        let row = results
            .iter()
            .find(|row| row["test_case_id"].as_i64() == Some(test_case_id as i64))
            .unwrap_or_else(|| panic!("missing testcase result for {test_case_id}: {}", res.text));
        assert_eq!(
            row["verdict"].as_str(),
            Some(verdict),
            "unexpected testcase verdict for {test_case_id}: {}",
            res.text
        );
    }
}

// -- Real-isolate judging through the shared detached-eval driver ------------
// Unlike the seeded tests above, these judge for real end to end: submission
// POST -> dispatch routes to the icpc contest type -> on_submission ->
// DetachedEval::start -> windowed evaluate in a real isolate sandbox ->
// on_icpc_eval_result callback -> the shared driver's record/finalize ->
// terminal submission verdict. Gated on a real sandbox; run with `-- --ignored`.

fn is_real_sandbox() -> bool {
    if std::env::var("E2E_SERVER_URL").is_ok() {
        return true;
    }
    match std::env::var("E2E_SANDBOX_BACKEND") {
        Ok(v) if v.eq_ignore_ascii_case("mock") => false,
        Ok(v) if v.eq_ignore_ascii_case("isolate") => isolate_available(),
        Ok(_) => false,
        Err(_) => cfg!(target_os = "linux") && isolate_available(),
    }
}

fn isolate_available() -> bool {
    std::process::Command::new("isolate")
        .arg("--version")
        .status()
        .is_ok_and(|status| status.success())
}

/// Correct: prints the sum of the n integers (the default test case expects "15").
const CPP_SUM_AC: &str = r#"#include <iostream>
int main() { int n; std::cin >> n; long long s = 0, x; for (int i = 0; i < n; i++) { std::cin >> x; s += x; } std::cout << s << std::endl; return 0; }
"#;

/// Wrong: prints sum + 1 (yields "16", expected "15").
const CPP_SUM_WA: &str = r#"#include <iostream>
int main() { int n; std::cin >> n; long long s = 0, x; for (int i = 0; i < n; i++) { std::cin >> x; s += x; } std::cout << (s + 1) << std::endl; return 0; }
"#;

async fn judge_icpc_contest_solution(
    prefix: &str,
    contest_name: &str,
    code: &str,
) -> (E2eTestApp, i32) {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role(&format!("{prefix}_admin"), "pass1234", "admin")
        .await;
    let user = app
        .create_authenticated_user(&format!("{prefix}_user"), "pass1234")
        .await;

    let problem_id = app.create_problem(&admin, "ICPC Detached Problem").await;
    app.create_test_case(problem_id, &admin).await;

    let contest_id = app
        .create_typed_contest(&admin, contest_name, "icpc", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    app.register_for_contest(contest_id, &user).await;

    let sub_id = app
        .create_contest_submission(contest_id, problem_id, &user, "cpp", code)
        .await;
    let res = app.wait_for_submission_terminal(sub_id, &user, 90).await;
    assert_eq!(
        res.body["status"], "Judged",
        "submission should judge cleanly: {}",
        res.text
    );
    (app, sub_id)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a real isolate sandbox and C++ toolchain"]
async fn icpc_contest_accepted_through_detached_driver() {
    if !is_real_sandbox() {
        return;
    }
    let (app, sub_id) =
        judge_icpc_contest_solution("icpc_rj_ac", "ICPC Detached AC", CPP_SUM_AC).await;
    let sub = submission::Entity::find_by_id(sub_id)
        .one(&app.db)
        .await
        .expect("query submission")
        .expect("submission exists");
    assert_eq!(
        sub.verdict,
        Some(Verdict::Accepted),
        "a correct sum must judge Accepted through the detached driver"
    );
    assert_eq!(
        sub.score,
        Some(1.0),
        "an accepted ICPC submission scores 1.0"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a real isolate sandbox and C++ toolchain"]
async fn icpc_contest_wrong_answer_through_detached_driver() {
    if !is_real_sandbox() {
        return;
    }
    let (app, sub_id) =
        judge_icpc_contest_solution("icpc_rj_wa", "ICPC Detached WA", CPP_SUM_WA).await;
    let sub = submission::Entity::find_by_id(sub_id)
        .one(&app.db)
        .await
        .expect("query submission")
        .expect("submission exists");
    assert_eq!(
        sub.verdict,
        Some(Verdict::WrongAnswer),
        "a wrong sum must judge WrongAnswer through the detached driver"
    );
    assert_eq!(
        sub.score,
        Some(0.0),
        "a wrong-answer ICPC submission scores 0.0"
    );
}
