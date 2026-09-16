use broccoli_server_sdk::permissions as perm;
use chrono::{Duration, TimeZone, Utc};
use common::{SubmissionStatus, Verdict};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use serde_json::json;
use server::entity::{plugin_storage, submission, submission_judgement, test_case_result, user};

use crate::{common::E2eTestApp, judging::CPP_SUM};

async fn seed_accepted_ioi_submission(
    app: &E2eTestApp,
    username: &str,
    problem_id: i32,
    contest_id: i32,
    score: f64,
) -> i32 {
    let user_model = user::Entity::find()
        .filter(user::Column::Username.eq(username))
        .one(&app.db)
        .await
        .expect("query contestant")
        .expect("contestant should exist");
    let now = Utc::now();
    let submission = submission::ActiveModel {
        files: Set(json!([{ "filename": "main.cpp", "content": CPP_SUM }])),
        language: Set("cpp".into()),
        user_id: Set(user_model.id),
        problem_id: Set(problem_id),
        contest_id: Set(Some(contest_id)),
        contest_type: Set("ioi".into()),
        status: Set(SubmissionStatus::Judged),
        verdict: Set(Some(Verdict::Accepted)),
        score: Set(Some(score)),
        time_used: Set(Some(10)),
        memory_used: Set(Some(256)),
        judge_epoch: Set(1),
        created_at: Set(now),
        judged_at: Set(Some(now)),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert IOI submission");

    submission_judgement::ActiveModel {
        submission_id: Set(submission.id),
        version: Set(1),
        is_current: Set(true),
        is_finalized: Set(true),
        triggered_by_user_id: Set(None),
        status: Set(SubmissionStatus::Judged),
        verdict: Set(Some(Verdict::Accepted)),
        score: Set(Some(score)),
        time_used: Set(Some(10)),
        memory_used: Set(Some(256)),
        judge_epoch: Set(1),
        created_at: Set(now),
        finalized_at: Set(Some(now)),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert IOI judgement");

    plugin_storage::ActiveModel {
        plugin_id: Set("ioi".into()),
        collection: Set("default".into()),
        key: Set(format!(
            "task_score:{contest_id}:{problem_id}:{}",
            user_model.id
        )),
        data: Set(json!(score.to_string())),
        created_at: Set(now),
    }
    .insert(&app.db)
    .await
    .expect("insert IOI task score");

    submission.id
}

#[tokio::test(flavor = "multi_thread")]
async fn ioi_contest_type_registered() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("ioi_admin1", "password", "admin")
        .await;
    let contest_id = app
        .create_typed_contest(&admin, "IOI Contest 1", "ioi", true, true)
        .await;
    assert!(contest_id > 0, "Should successfully create an IOI contest");
}

#[tokio::test(flavor = "multi_thread")]
async fn ioi_scoreboard_reflects_judged_submission() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("ioi_admin3", "password", "admin")
        .await;
    let contestant = app.create_authenticated_user("ioi_user3", "password").await;

    let problem_id = app.create_problem(&admin, "IOI Problem 3").await;

    let contest_id = app
        .create_typed_contest(&admin, "IOI Contest 3", "ioi", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    app.register_for_contest(contest_id, &contestant).await;

    seed_accepted_ioi_submission(&app, "ioi_user3", problem_id, contest_id, 100.0).await;

    let scoreboard_path = format!("/api/v1/p/ioi/api/plugins/ioi/contests/{contest_id}/scoreboard");
    let res = app.get_with_token(&scoreboard_path, &contestant).await;
    assert_eq!(res.status, 200, "Scoreboard request failed: {}", res.text);

    let rows = &res.body["rankings"];
    assert!(rows.is_array(), "rows should be an array");
    let rankings_arr = rows.as_array().unwrap();
    assert!(
        !rankings_arr.is_empty(),
        "Scoreboard should have at least one entry after a judged submission"
    );
    assert!(
        rankings_arr[0]["username"].is_string(),
        "Row should have a username"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ioi_feedback_filter_redacts_judgement_history() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("ioi_admin_history", "password", "admin")
        .await;
    let contestant = app
        .create_authenticated_user("ioi_history_user", "password")
        .await;

    let problem_id = app.create_problem(&admin, "IOI History Problem").await;
    app.create_test_case(problem_id, &admin).await;

    let contest_id = app
        .create_typed_contest(&admin, "IOI History Contest", "ioi", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    app.register_for_contest(contest_id, &contestant).await;

    let config_path = format!("/api/v1/contests/{contest_id}/config/ioi/contest");
    let put_res = app
        .put_with_token(
            &config_path,
            &json!({
                "config": {
                    "feedback_level": "none"
                },
                "enabled": true
            }),
            &admin,
        )
        .await;
    assert_eq!(
        put_res.status, 200,
        "Failed to set IOI feedback config: {}",
        put_res.text
    );

    let contestant_model = user::Entity::find()
        .filter(user::Column::Username.eq("ioi_history_user"))
        .one(&app.db)
        .await;
    let contestant_model = contestant_model
        .expect("query contestant")
        .expect("contestant should exist");
    let now = Utc::now();
    let submission = submission::ActiveModel {
        files: Set(json!([{ "filename": "main.cpp", "content": CPP_SUM }])),
        language: Set("cpp".into()),
        user_id: Set(contestant_model.id),
        problem_id: Set(problem_id),
        contest_id: Set(Some(contest_id)),
        contest_type: Set("ioi".into()),
        status: Set(SubmissionStatus::Judged),
        verdict: Set(Some(Verdict::Accepted)),
        score: Set(Some(100.0)),
        time_used: Set(Some(10)),
        memory_used: Set(Some(256)),
        judge_epoch: Set(1),
        created_at: Set(now),
        judged_at: Set(Some(now)),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert judged submission");
    let sub_id = submission.id;
    let judgement = submission_judgement::ActiveModel {
        submission_id: Set(submission.id),
        version: Set(1),
        is_current: Set(true),
        is_finalized: Set(true),
        triggered_by_user_id: Set(None),
        status: Set(SubmissionStatus::Judged),
        verdict: Set(Some(Verdict::Accepted)),
        score: Set(Some(100.0)),
        time_used: Set(Some(10)),
        memory_used: Set(Some(256)),
        judge_epoch: Set(1),
        created_at: Set(now),
        finalized_at: Set(Some(now)),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert judged judgement");
    test_case_result::ActiveModel {
        submission_id: Set(submission.id),
        judgement_id: Set(Some(judgement.id)),
        test_case_id: Set(None),
        run_index: Set(None),
        verdict: Set(Verdict::Accepted),
        score: Set(100.0),
        time_used: Set(Some(10)),
        memory_used: Set(Some(256)),
        created_at: Set(now),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert judgement result");

    let admin_res = app
        .get_with_token(&format!("/api/v1/submissions/{sub_id}/judgements"), &admin)
        .await;
    assert_eq!(
        admin_res.status, 200,
        "Admin judgement history failed: {}",
        admin_res.text
    );
    assert!(
        admin_res.body[0]["score"].is_number(),
        "Admin history should retain the raw score: {}",
        admin_res.text
    );

    let contestant_res = app
        .get_with_token(
            &format!("/api/v1/submissions/{sub_id}/judgements"),
            &contestant,
        )
        .await;
    assert_eq!(
        contestant_res.status, 200,
        "Contestant judgement history failed: {}",
        contestant_res.text
    );
    assert_eq!(contestant_res.body[0]["score"], serde_json::Value::Null);
    assert_eq!(
        contestant_res.body[0]["test_case_results"]
            .as_array()
            .map(Vec::len),
        Some(0),
        "Contestant history should hide per-test-case rows: {}",
        contestant_res.text
    );
}

// Mirrors `icpc_scoreboard_freeze_redacts_peer_submission_but_not_owner_or_
// organizer`'s use of a `contest:manage`-only viewer to isolate the
// `CONTEST_MANAGE` branch of `decide_visibility_decisions` from the
// pre-existing `SUBMISSION_VIEW_ALL` one. Before this fix,
// `decide_visibility_decisions` checked only `SUBMISSION_VIEW_ALL`, unlike
// `can_view_privileged_submission_feedback` (used elsewhere in this plugin)
// and `api.rs`'s scoreboard ranking, which both also honor `CONTEST_MANAGE`.
// A viewer holding `contest:manage` without `submission:view_all` -- a
// plausible problem-setter or judge role -- was wrongly redacted here; this
// is the assertion that pins the fix and would fail against pre-fix code.
#[tokio::test(flavor = "multi_thread")]
async fn ioi_feedback_organiser_without_submission_view_all_sees_unredacted_judgement() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("ioi_manage_admin", "password", "admin")
        .await;
    let contestant = app
        .create_authenticated_user("ioi_manage_contestant", "password")
        .await;
    // Holds `contest:manage` WITHOUT `submission:view_all` - unlike `admin`
    // above, which holds BOTH and so cannot isolate which permission a
    // bypass actually checks.
    let manage_only = app
        .create_user_with_permissions("ioi_manage_only", "password", &[perm::CONTEST_MANAGE])
        .await;

    let problem_id = app.create_problem(&admin, "IOI Manage Problem").await;
    app.create_test_case(problem_id, &admin).await;

    let contest_id = app
        .create_typed_contest(&admin, "IOI Manage Contest", "ioi", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    app.register_for_contest(contest_id, &contestant).await;
    // Host-level reachability for a non-owner, non-`submission:view_all`
    // viewer requires contest participation (see
    // `visibility::host_rules::decide_submission`) - `contest:manage` alone
    // does not grant it. Without this, the request 404s before the plugin's
    // decision is ever consulted, and the assertions below would not
    // actually exercise the fix.
    app.register_for_contest(contest_id, &manage_only).await;

    let config_path = format!("/api/v1/contests/{contest_id}/config/ioi/contest");
    let put_res = app
        .put_with_token(
            &config_path,
            &json!({
                "config": {
                    "feedback_level": "none"
                },
                "enabled": true
            }),
            &admin,
        )
        .await;
    assert_eq!(
        put_res.status, 200,
        "Failed to set IOI feedback config: {}",
        put_res.text
    );

    let sub_id =
        seed_accepted_ioi_submission(&app, "ioi_manage_contestant", problem_id, contest_id, 100.0)
            .await;

    // `seed_accepted_ioi_submission` does not insert a `test_case_result`
    // row, so add one directly - otherwise the `test_case_results`
    // assertions below would pass trivially (empty either way) rather than
    // exercising the mask.
    let judgement_model = submission_judgement::Entity::find()
        .filter(submission_judgement::Column::SubmissionId.eq(sub_id))
        .one(&app.db)
        .await
        .expect("query judgement")
        .expect("judgement should exist");
    test_case_result::ActiveModel {
        submission_id: Set(sub_id),
        judgement_id: Set(Some(judgement_model.id)),
        test_case_id: Set(None),
        run_index: Set(None),
        verdict: Set(Verdict::Accepted),
        score: Set(100.0),
        time_used: Set(Some(10)),
        memory_used: Set(Some(256)),
        created_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert judgement test case result");

    let judgements_path = format!("/api/v1/submissions/{sub_id}/judgements");

    // Baseline: with `feedback_level: none`, even the owning contestant sees
    // their score and test case results blanked.
    let contestant_res = app.get_with_token(&judgements_path, &contestant).await;
    assert_eq!(
        contestant_res.status, 200,
        "Contestant judgement history failed: {}",
        contestant_res.text
    );
    assert_eq!(contestant_res.body[0]["score"], serde_json::Value::Null);
    assert_eq!(
        contestant_res.body[0]["test_case_results"]
            .as_array()
            .map(Vec::len),
        Some(0),
        "the owning contestant should also have test case rows hidden under feedback_level none: {}",
        contestant_res.text
    );

    // A viewer with `contest:manage` but not `submission:view_all` must see
    // the unredacted judgement history - this is the branch this fix adds.
    let manage_only_res = app.get_with_token(&judgements_path, &manage_only).await;
    assert_eq!(
        manage_only_res.status, 200,
        "contest:manage-only judgement history read failed: {}",
        manage_only_res.text
    );
    assert_eq!(
        manage_only_res.body[0]["score"].as_f64(),
        Some(100.0),
        "a viewer with contest:manage but not submission:view_all must see the raw score: {}",
        manage_only_res.text
    );
    assert_eq!(
        manage_only_res.body[0]["test_case_results"]
            .as_array()
            .map(Vec::len),
        Some(1),
        "a viewer with contest:manage but not submission:view_all must see test case rows: {}",
        manage_only_res.text
    );
}

// Covers the `subtask_scores` sibling of `per_test_case_mask_fields`'s
// wildcard fan-out (`result.test_case_results.*.verdict`, etc.) through the
// real HTTP + serialization pipeline, rather than a mocked-JSON unit test.
// Unlike `none` (which blanks the whole array to `[]`), `subtask_scores`
// keeps the array but nulls per-test-case fields -- this is exactly the
// shape that used to be misrendered as "pending" client-side (a masked
// `null` verdict is indistinguishable on the wire from a genuinely
// not-yet-judged test case; see `TestCaseRow.tsx`'s `getVerdictKey`, which
// now disambiguates using the judgement's terminal `status` instead).
#[tokio::test(flavor = "multi_thread")]
async fn ioi_feedback_filter_subtask_scores_redacts_per_test_case_verdict() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("ioi_admin_history2", "password", "admin")
        .await;
    let contestant = app
        .create_authenticated_user("ioi_history_user2", "password")
        .await;

    let problem_id = app.create_problem(&admin, "IOI History Problem 2").await;
    app.create_test_case(problem_id, &admin).await;

    let contest_id = app
        .create_typed_contest(&admin, "IOI History Contest 2", "ioi", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    app.register_for_contest(contest_id, &contestant).await;

    let config_path = format!("/api/v1/contests/{contest_id}/config/ioi/contest");
    let put_res = app
        .put_with_token(
            &config_path,
            &json!({
                "config": {
                    "feedback_level": "subtask_scores"
                },
                "enabled": true
            }),
            &admin,
        )
        .await;
    assert_eq!(
        put_res.status, 200,
        "Failed to set IOI feedback config: {}",
        put_res.text
    );

    let contestant_model = user::Entity::find()
        .filter(user::Column::Username.eq("ioi_history_user2"))
        .one(&app.db)
        .await;
    let contestant_model = contestant_model
        .expect("query contestant")
        .expect("contestant should exist");
    let now = Utc::now();
    let submission = submission::ActiveModel {
        files: Set(json!([{ "filename": "main.cpp", "content": CPP_SUM }])),
        language: Set("cpp".into()),
        user_id: Set(contestant_model.id),
        problem_id: Set(problem_id),
        contest_id: Set(Some(contest_id)),
        contest_type: Set("ioi".into()),
        status: Set(SubmissionStatus::Judged),
        verdict: Set(Some(Verdict::Accepted)),
        score: Set(Some(100.0)),
        time_used: Set(Some(10)),
        memory_used: Set(Some(256)),
        judge_epoch: Set(1),
        created_at: Set(now),
        judged_at: Set(Some(now)),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert judged submission");
    let sub_id = submission.id;
    let judgement = submission_judgement::ActiveModel {
        submission_id: Set(submission.id),
        version: Set(1),
        is_current: Set(true),
        is_finalized: Set(true),
        triggered_by_user_id: Set(None),
        status: Set(SubmissionStatus::Judged),
        verdict: Set(Some(Verdict::Accepted)),
        score: Set(Some(100.0)),
        time_used: Set(Some(10)),
        memory_used: Set(Some(256)),
        judge_epoch: Set(1),
        created_at: Set(now),
        finalized_at: Set(Some(now)),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert judged judgement");
    test_case_result::ActiveModel {
        submission_id: Set(submission.id),
        judgement_id: Set(Some(judgement.id)),
        test_case_id: Set(None),
        run_index: Set(None),
        verdict: Set(Verdict::Accepted),
        score: Set(100.0),
        time_used: Set(Some(10)),
        memory_used: Set(Some(256)),
        created_at: Set(now),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert judgement result");

    let admin_res = app
        .get_with_token(&format!("/api/v1/submissions/{sub_id}/judgements"), &admin)
        .await;
    assert_eq!(
        admin_res.status, 200,
        "Admin judgement history failed: {}",
        admin_res.text
    );
    assert_eq!(
        admin_res.body[0]["test_case_results"][0]["verdict"],
        serde_json::Value::String("Accepted".to_string()),
        "Admin history should retain the raw per-test-case verdict: {}",
        admin_res.text
    );

    let contestant_res = app
        .get_with_token(
            &format!("/api/v1/submissions/{sub_id}/judgements"),
            &contestant,
        )
        .await;
    assert_eq!(
        contestant_res.status, 200,
        "Contestant judgement history failed: {}",
        contestant_res.text
    );

    let test_case_results = contestant_res.body[0]["test_case_results"]
        .as_array()
        .expect("test_case_results should be an array");
    assert!(
        !test_case_results.is_empty(),
        "subtask_scores should keep the per-test-case rows (unlike `none`), just with fields blanked: {}",
        contestant_res.text
    );
    for (i, tc) in test_case_results.iter().enumerate() {
        assert_eq!(
            tc["verdict"],
            serde_json::Value::Null,
            "test_case_results[{i}] verdict should be masked to null under subtask_scores, \
             not authored as a string, and must not be misrendered client-side as still \
             pending (see TestCaseRow.tsx's getVerdictKey): {}",
            contestant_res.text
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ioi_contest_config_scoring_mode() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("ioi_admin5", "password", "admin")
        .await;

    let contest_id = app
        .create_typed_contest(&admin, "IOI Contest 5", "ioi", true, true)
        .await;

    let config_path = format!("/api/v1/contests/{contest_id}/config/ioi/contest");
    let put_res = app
        .put_with_token(
            &config_path,
            &json!({
                "config": {
                    "scoring_mode": "sum_best_subtask",
                    "feedback_level": "subtask_scores"
                },
                "enabled": true
            }),
            &admin,
        )
        .await;
    assert_eq!(
        put_res.status, 200,
        "Failed to set IOI contest config: {}",
        put_res.text
    );

    let get_res = app.get_with_token(&config_path, &admin).await;
    assert_eq!(
        get_res.status, 200,
        "Failed to get IOI contest config: {}",
        get_res.text
    );
    assert_eq!(
        get_res.body["config"]["scoring_mode"].as_str(),
        Some("sum_best_subtask")
    );
    assert_eq!(
        get_res.body["config"]["feedback_level"].as_str(),
        Some("subtask_scores")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ioi_scoreboard_visibility_config_can_show_all_viewers_during_contest() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("ioi_admin6", "password", "admin")
        .await;
    let contestant_a = app
        .create_authenticated_user("ioi_user6a", "password")
        .await;
    let contestant_b = app
        .create_authenticated_user("ioi_user6b", "password")
        .await;

    let problem_id = app.create_problem(&admin, "IOI Problem 6").await;
    app.create_test_case(problem_id, &admin).await;

    let contest_id = app
        .create_typed_contest(&admin, "IOI Contest 6", "ioi", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    app.register_for_contest(contest_id, &contestant_a).await;
    app.register_for_contest(contest_id, &contestant_b).await;

    let contestant_a_model = user::Entity::find()
        .filter(user::Column::Username.eq("ioi_user6a"))
        .one(&app.db)
        .await
        .expect("query contestant A")
        .expect("contestant A should exist");
    let contestant_b_model = user::Entity::find()
        .filter(user::Column::Username.eq("ioi_user6b"))
        .one(&app.db)
        .await
        .expect("query contestant B")
        .expect("contestant B should exist");

    let contest_start = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
    for (user_id, created_at) in [
        (contestant_a_model.id, contest_start + Duration::minutes(10)),
        (contestant_b_model.id, contest_start + Duration::minutes(20)),
    ] {
        let submission = submission::ActiveModel {
            files: Set(json!([{ "filename": "main.cpp", "content": CPP_SUM }])),
            language: Set("cpp".into()),
            user_id: Set(user_id),
            problem_id: Set(problem_id),
            contest_id: Set(Some(contest_id)),
            contest_type: Set("ioi".into()),
            status: Set(SubmissionStatus::Judged),
            verdict: Set(Some(Verdict::Accepted)),
            score: Set(Some(10.0)),
            time_used: Set(Some(10)),
            memory_used: Set(Some(256)),
            judge_epoch: Set(1),
            created_at: Set(created_at),
            judged_at: Set(Some(created_at)),
            ..Default::default()
        }
        .insert(&app.db)
        .await
        .expect("insert judged submission");

        submission_judgement::ActiveModel {
            submission_id: Set(submission.id),
            version: Set(1),
            is_current: Set(true),
            is_finalized: Set(true),
            triggered_by_user_id: Set(None),
            status: Set(SubmissionStatus::Judged),
            verdict: Set(Some(Verdict::Accepted)),
            score: Set(Some(10.0)),
            time_used: Set(Some(10)),
            memory_used: Set(Some(256)),
            judge_epoch: Set(1),
            created_at: Set(created_at),
            finalized_at: Set(Some(created_at)),
            ..Default::default()
        }
        .insert(&app.db)
        .await
        .expect("insert judged judgement");

        plugin_storage::ActiveModel {
            plugin_id: Set("ioi".into()),
            collection: Set("default".into()),
            key: Set(format!("task_score:{contest_id}:{problem_id}:{user_id}")),
            data: Set(json!("10")),
            created_at: Set(created_at),
        }
        .insert(&app.db)
        .await
        .expect("insert task score");
    }

    let scoreboard_path = format!("/api/v1/p/ioi/api/plugins/ioi/contests/{contest_id}/scoreboard");
    let default_res = app.get_with_token(&scoreboard_path, &contestant_a).await;
    assert_eq!(
        default_res.status, 200,
        "Default scoreboard request failed: {}",
        default_res.text
    );
    assert_eq!(
        default_res.body["scoreboard_visibility"].as_str(),
        Some("admins_only")
    );
    assert_eq!(
        default_res.body["rankings"].as_array().map(Vec::len),
        Some(1),
        "Default live scoreboard should only show the viewer row: {}",
        default_res.text
    );
    let default_rows = default_res.body["rankings"].as_array().unwrap();
    assert_eq!(
        default_rows[0]["username"].as_str(),
        Some("ioi_user6a"),
        "Default live scoreboard should show only the viewer row: {}",
        default_res.text
    );
    assert_eq!(
        default_rows[0]["total_score"].as_f64(),
        Some(10.0),
        "Viewer score should be visible in the default live scoreboard: {}",
        default_res.text
    );
    assert!(
        default_rows
            .iter()
            .all(|row| row["username"].as_str() != Some("ioi_user6b")),
        "Default live scoreboard should not expose another contestant row: {}",
        default_res.text
    );

    let config_path = format!("/api/v1/contests/{contest_id}/config/ioi/contest");
    let put_res = app
        .put_with_token(
            &config_path,
            &json!({
                "config": {
                    "scoreboard_visibility": "all_contest_viewers"
                },
                "enabled": true
            }),
            &admin,
        )
        .await;
    assert_eq!(
        put_res.status, 200,
        "Failed to set IOI scoreboard visibility config: {}",
        put_res.text
    );

    let visible_res = app.get_with_token(&scoreboard_path, &contestant_a).await;
    assert_eq!(
        visible_res.status, 200,
        "Configured scoreboard request failed: {}",
        visible_res.text
    );
    assert_eq!(
        visible_res.body["scoreboard_visibility"].as_str(),
        Some("all_contest_viewers")
    );
    assert_eq!(
        visible_res.body["rankings"].as_array().map(Vec::len),
        Some(2),
        "Configured live scoreboard should show all contest viewers: {}",
        visible_res.text
    );
    let visible_rows = visible_res.body["rankings"].as_array().unwrap();
    let contestant_b_row = visible_rows
        .iter()
        .find(|row| row["username"].as_str() == Some("ioi_user6b"))
        .unwrap_or_else(|| {
            panic!(
                "Configured scoreboard should include contestant B: {}",
                visible_res.text
            )
        });
    assert_eq!(
        contestant_b_row["total_score"].as_f64(),
        Some(10.0),
        "Configured live scoreboard should expose contestant B's total score: {}",
        visible_res.text
    );
    assert_eq!(
        contestant_b_row["problems"][0]["score"].as_f64(),
        Some(10.0),
        "Configured live scoreboard should expose contestant B's per-problem score: {}",
        visible_res.text
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ioi_scoreboard_uses_time_taken_as_score_tiebreaker() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("ioi_time_admin", "password", "admin")
        .await;
    let slow_token = app
        .create_authenticated_user("ioi_time_a_slow", "password")
        .await;
    let fast_token = app
        .create_authenticated_user("ioi_time_z_fast", "password")
        .await;

    let problem_id = app.create_problem(&admin, "IOI Time Tie Problem A").await;
    let problem_id_b = app.create_problem(&admin, "IOI Time Tie Problem B").await;
    app.create_test_case(problem_id, &admin).await;
    app.create_test_case(problem_id_b, &admin).await;

    let contest_id = app
        .create_typed_contest(&admin, "IOI Time Tie Contest", "ioi", true, true)
        .await;
    app.add_problem_to_contest_with_label(contest_id, problem_id, "A", &admin)
        .await;
    app.add_problem_to_contest_with_label(contest_id, problem_id_b, "B", &admin)
        .await;
    app.register_for_contest(contest_id, &slow_token).await;
    app.register_for_contest(contest_id, &fast_token).await;

    let slow_user = user::Entity::find()
        .filter(user::Column::Username.eq("ioi_time_a_slow"))
        .one(&app.db)
        .await
        .expect("query slow user")
        .expect("slow user should exist");
    let fast_user = user::Entity::find()
        .filter(user::Column::Username.eq("ioi_time_z_fast"))
        .one(&app.db)
        .await
        .expect("query fast user")
        .expect("fast user should exist");

    let contest_start = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
    for (user_id, problem_id, created_at) in [
        (
            slow_user.id,
            problem_id,
            contest_start + Duration::minutes(60),
        ),
        (
            slow_user.id,
            problem_id_b,
            contest_start + Duration::minutes(5),
        ),
        (
            fast_user.id,
            problem_id,
            contest_start + Duration::minutes(40),
        ),
        (
            fast_user.id,
            problem_id_b,
            contest_start + Duration::minutes(40),
        ),
    ] {
        let submission = submission::ActiveModel {
            files: Set(json!([{ "filename": "main.cpp", "content": CPP_SUM }])),
            language: Set("cpp".into()),
            user_id: Set(user_id),
            problem_id: Set(problem_id),
            contest_id: Set(Some(contest_id)),
            contest_type: Set("ioi".into()),
            status: Set(SubmissionStatus::Judged),
            verdict: Set(Some(Verdict::Accepted)),
            score: Set(Some(10.0)),
            time_used: Set(Some(10)),
            memory_used: Set(Some(256)),
            judge_epoch: Set(1),
            created_at: Set(created_at),
            judged_at: Set(Some(created_at)),
            ..Default::default()
        }
        .insert(&app.db)
        .await
        .expect("insert judged submission");

        submission_judgement::ActiveModel {
            submission_id: Set(submission.id),
            version: Set(1),
            is_current: Set(true),
            is_finalized: Set(true),
            triggered_by_user_id: Set(None),
            status: Set(SubmissionStatus::Judged),
            verdict: Set(Some(Verdict::Accepted)),
            score: Set(Some(10.0)),
            time_used: Set(Some(10)),
            memory_used: Set(Some(256)),
            judge_epoch: Set(1),
            created_at: Set(created_at),
            finalized_at: Set(Some(created_at)),
            ..Default::default()
        }
        .insert(&app.db)
        .await
        .expect("insert judged judgement");

        plugin_storage::ActiveModel {
            plugin_id: Set("ioi".into()),
            collection: Set("default".into()),
            key: Set(format!("task_score:{contest_id}:{problem_id}:{user_id}")),
            data: Set(json!("10")),
            created_at: Set(created_at),
        }
        .insert(&app.db)
        .await
        .expect("insert task score");
    }

    let scoreboard_path = format!("/api/v1/p/ioi/api/plugins/ioi/contests/{contest_id}/scoreboard");
    let res = app.get_with_token(&scoreboard_path, &admin).await;
    assert_eq!(res.status, 200, "Scoreboard request failed: {}", res.text);

    let rows = res.body["rankings"].as_array().expect("rankings array");
    let first = rows
        .iter()
        .find(|row| row["username"].as_str() == Some("ioi_time_z_fast"))
        .expect("fast row should be present");
    let second = rows
        .iter()
        .find(|row| row["username"].as_str() == Some("ioi_time_a_slow"))
        .expect("slow row should be present");
    assert_eq!(first["rank"].as_u64(), Some(1), "fast row: {first}");
    assert_eq!(second["rank"].as_u64(), Some(2), "slow row: {second}");
    assert_eq!(first["total_time_seconds"].as_i64(), Some(2400), "{first}");
    assert_eq!(
        second["total_time_seconds"].as_i64(),
        Some(3600),
        "{second}"
    );
}
