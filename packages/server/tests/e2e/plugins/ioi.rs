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
        max_judge_epoch: Set(1),
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

/// At `feedback_level = "none"` a contestant reads no score off the
/// scoreboard, their own row included: no totals, no score times, and no
/// ranks or score order to read them from. Staff still see every score.
#[tokio::test(flavor = "multi_thread")]
async fn ioi_feedback_none_withholds_every_score_on_the_scoreboard() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("ioi_none_board_admin", "password", "admin")
        .await;
    let low = app
        .create_authenticated_user("ioi_none_board_a", "password")
        .await;
    let high = app
        .create_authenticated_user("ioi_none_board_b", "password")
        .await;
    let problem_id = app.create_problem(&admin, "IOI None Board").await;
    let contest_id = app
        .create_typed_contest(&admin, "IOI None Board", "ioi", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    app.register_for_contest(contest_id, &low).await;
    app.register_for_contest(contest_id, &high).await;
    let res = app
        .put_with_token(
            &format!("/api/v1/contests/{contest_id}/config/ioi/contest"),
            &json!({
                "config": {
                    "feedback_level": "none",
                    "scoreboard_visibility": "all_contest_viewers"
                },
                "enabled": true
            }),
            &admin,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    seed_accepted_ioi_submission(&app, "ioi_none_board_a", problem_id, contest_id, 10.0).await;
    seed_accepted_ioi_submission(&app, "ioi_none_board_b", problem_id, contest_id, 90.0).await;

    let path = format!("/api/v1/p/ioi/api/plugins/ioi/contests/{contest_id}/scoreboard");
    let board = app.get_with_token(&path, &low).await;
    assert_eq!(board.status, 200, "{}", board.text);
    let rows = board.body["rankings"].as_array().expect("rankings");
    let names: Vec<&str> = rows
        .iter()
        .map(|r| r["username"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["ioi_none_board_a", "ioi_none_board_b"],
        "{}",
        board.text
    );
    for row in rows {
        for field in ["total_score", "total_time_seconds", "problems"] {
            assert!(
                row.get(field).is_none(),
                "{field} is withheld: {}",
                board.text
            );
        }
        assert_eq!(row["rank"], json!(null), "{}", board.text);
    }

    let staff = app.get_with_token(&path, &admin).await;
    assert_eq!(staff.status, 200, "{}", staff.text);
    let rows = staff.body["rankings"].as_array().expect("rankings");
    assert_eq!(rows[0]["username"], "ioi_none_board_b", "{}", staff.text);
    assert_eq!(
        rows[0]["total_score"].as_f64(),
        Some(90.0),
        "{}",
        staff.text
    );
    assert_eq!(rows[0]["rank"], json!(1), "{}", staff.text);
}

/// A judged submission's subtask scores, read back by the subtask-scores
/// route and by the sum-best-subtask scoreboard, are the ones judging gave it.
#[tokio::test(flavor = "multi_thread")]
async fn a_judged_ioi_submission_reads_back_the_subtask_scores_it_was_judged_with() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("ioi_readback_admin", "password", "admin")
        .await;
    let contestant = app
        .create_authenticated_user("ioi_readback_user", "password")
        .await;
    let problem_id = app.create_problem(&admin, "IOI Read Back").await;
    app.create_test_case_with(problem_id, "2\n1 2", "3\n", 40, false, &admin)
        .await;
    app.create_test_case_with(problem_id, "3\n1 2 3", "6\n", 60, false, &admin)
        .await;
    let contest_id = app
        .create_typed_contest(&admin, "IOI Read Back", "ioi", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    app.register_for_contest(contest_id, &contestant).await;
    let res = app
        .put_with_token(
            &format!("/api/v1/contests/{contest_id}/config/ioi/contest"),
            &json!({ "config": { "scoring_mode": "sum_best_subtask" }, "enabled": true }),
            &admin,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);

    let sub_id = app
        .create_contest_submission(contest_id, problem_id, &contestant, "cpp", CPP_SUM)
        .await;
    let judged = app.wait_for_submission_terminal(sub_id, &admin, 60).await;
    assert_eq!(judged.body["status"], "Judged", "{}", judged.text);
    app.judging_idle().await;

    let scores = app
        .get_with_token(
            &format!(
                "/api/v1/p/ioi/api/plugins/ioi/contests/{contest_id}/submissions/{sub_id}/subtask-scores"
            ),
            &admin,
        )
        .await;
    assert_eq!(scores.status, 200, "{}", scores.text);
    assert_eq!(
        scores.body["subtasks"][0]["score"].as_f64(),
        Some(100.0),
        "{}",
        scores.text
    );

    let board = app
        .get_with_token(
            &format!("/api/v1/p/ioi/api/plugins/ioi/contests/{contest_id}/scoreboard"),
            &admin,
        )
        .await;
    assert_eq!(board.status, 200, "{}", board.text);
    assert_eq!(
        board.body["rankings"][0]["total_score"].as_f64(),
        Some(100.0),
        "{}",
        board.text
    );
}

/// A problem with no subtask config is scored by the default All Tests
/// subtask, which leaves out samples, so its scoreboard maximum is the sum
/// of the non-sample test points and a perfect solution reads as full marks.
#[tokio::test(flavor = "multi_thread")]
async fn ioi_scoreboard_max_score_leaves_out_samples_like_scoring_does() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("ioi_max_admin", "password", "admin")
        .await;
    let problem_id = app.create_problem(&admin, "IOI Max Problem").await;
    app.create_test_case_with(problem_id, "1", "1", 10, true, &admin)
        .await;
    app.create_test_case_with(problem_id, "2", "2", 90, false, &admin)
        .await;
    let contest_id = app
        .create_typed_contest(&admin, "IOI Max Contest", "ioi", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;

    let res = app
        .get_with_token(
            &format!("/api/v1/p/ioi/api/plugins/ioi/contests/{contest_id}/scoreboard"),
            &admin,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert_eq!(
        res.body["max_scores"][problem_id.to_string()].as_f64(),
        Some(90.0),
        "{}",
        res.text
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
        max_judge_epoch: Set(1),
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
        judgement_id: Set(judgement.id),
        judge_epoch: Set(judgement.judge_epoch),
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
// `SUBMISSION_VIEW_ALL` one. `decide_visibility_decisions` honors
// `CONTEST_MANAGE` like `can_view_privileged_submission_feedback` (used
// elsewhere in this plugin) and `api.rs`'s scoreboard ranking do, so a
// viewer holding `contest:manage` without `submission:view_all` -- a
// plausible problem-setter or judge role -- sees the judgement unredacted.
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
    // exercise the plugin's `CONTEST_MANAGE` branch.
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
        judgement_id: Set(judgement_model.id),
        judge_epoch: Set(judgement_model.judge_epoch),
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
    let detail_path = format!("{judgements_path}/{}", judgement_model.id);

    // History is a lightweight summary for every viewer. The pagination
    // metadata still follows the IOI feedback policy.
    let contestant_history = app.get_with_token(&judgements_path, &contestant).await;
    assert_eq!(
        contestant_history.status, 200,
        "{}",
        contestant_history.text
    );
    assert_eq!(contestant_history.body[0]["score"], serde_json::Value::Null);
    assert_eq!(
        contestant_history.body[0]["test_case_pagination"],
        serde_json::Value::Null
    );
    let manager_history = app.get_with_token(&judgements_path, &manage_only).await;
    assert_eq!(manager_history.status, 200, "{}", manager_history.text);
    assert_eq!(manager_history.body[0]["score"], json!(100.0));
    assert_eq!(manager_history.body[0]["test_case_results"], json!([]));
    assert_eq!(
        manager_history.body[0]["test_case_pagination"]["total"],
        json!(1)
    );

    // Expanding a version loads one page and must preserve the same policy.
    let contestant_res = app.get_with_token(&detail_path, &contestant).await;
    assert_eq!(contestant_res.status, 200, "{}", contestant_res.text);
    assert_eq!(contestant_res.body["score"], serde_json::Value::Null);
    assert_eq!(contestant_res.body["test_case_results"], json!([]));
    assert_eq!(
        contestant_res.body["test_case_pagination"],
        serde_json::Value::Null
    );
    let manage_only_res = app.get_with_token(&detail_path, &manage_only).await;
    assert_eq!(manage_only_res.status, 200, "{}", manage_only_res.text);
    assert_eq!(manage_only_res.body["score"], json!(100.0));
    assert_eq!(
        manage_only_res.body["test_case_results"]
            .as_array()
            .map(Vec::len),
        Some(1),
        "a viewer with contest:manage must see testcase rows on expansion: {}",
        manage_only_res.text
    );
    assert_eq!(
        manage_only_res.body["test_case_pagination"]["total"],
        json!(1)
    );
}

// Covers the `subtask_scores` sibling of `per_test_case_mask_fields`'s
// wildcard fan-out (`result.test_case_results.*.verdict`, etc.) through the
// real HTTP + serialization pipeline, rather than a mocked-JSON unit test.
// Unlike `none` (which blanks the whole array to `[]`), `subtask_scores`
// keeps the array but nulls per-test-case fields. A masked `null` verdict is
// indistinguishable on the wire from a genuinely not-yet-judged test case;
// the web client's `getVerdictKey` (`verdict-key.ts`) disambiguates using
// the judgement's terminal `status`.
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
        max_judge_epoch: Set(1),
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
        judgement_id: Set(judgement.id),
        judge_epoch: Set(judgement.judge_epoch),
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

    let history_path = format!("/api/v1/submissions/{sub_id}/judgements");
    for token in [&admin, &contestant] {
        let history = app.get_with_token(&history_path, token).await;
        assert_eq!(history.status, 200, "{}", history.text);
        assert_eq!(history.body[0]["test_case_results"], json!([]));
        assert_eq!(history.body[0]["test_case_pagination"]["total"], json!(1));
    }
    let detail_path = format!("{history_path}/{}", judgement.id);
    let admin_res = app.get_with_token(&detail_path, &admin).await;
    assert_eq!(admin_res.status, 200, "{}", admin_res.text);
    assert_eq!(
        admin_res.body["test_case_results"][0]["verdict"],
        json!("Accepted"),
        "Admin version details should retain the raw testcase verdict: {}",
        admin_res.text
    );
    let contestant_res = app.get_with_token(&detail_path, &contestant).await;
    assert_eq!(contestant_res.status, 200, "{}", contestant_res.text);

    let test_case_results = contestant_res.body["test_case_results"]
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
             pending (see verdict-key.ts's getVerdictKey): {}",
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
            max_judge_epoch: Set(1),
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
            revision: Set(0),
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
    assert_eq!(
        default_res.body["full_board"], false,
        "{}",
        default_res.text
    );
    assert!(
        default_rows[0]["rank"].is_null(),
        "a scoreboard showing only the viewer's row carries no rank: {}",
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
            max_judge_epoch: Set(1),
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
            revision: Set(0),
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

/// Disabling the contest-type plugin while one of its submissions is being
/// judged ends its session at once and hands that judgement back instead of
/// failing it: it waits queued, spending neither retry budget and never
/// showing a `SystemError`, while the contestant sees its outcome withheld
/// for the disabled plugin, and the operator sees why it waits. A new submission or code run of the type, to
/// the contest or as practice, is refused with `CONTEST_TYPE_DISABLED`, which
/// is not a retry-later answer, and the problem names the outage. Enabling the
/// plugin again judges the parked submission to its verdict.
#[tokio::test(flavor = "multi_thread")]
async fn a_submission_cut_by_disabling_its_contest_plugin_waits_and_is_judged_once_enabled() {
    cut_by_a_disable(Change::Local).await;
}

/// The same, on the replica judging the submission, when the disable and the
/// enable were published by another replica: this one applies them through
/// its fleet reconcile, as its periodic poll does, and hands the judgement
/// back exactly as the replica that made the change does.
#[tokio::test(flavor = "multi_thread")]
async fn a_submission_cut_by_a_disable_another_replica_published_is_handed_back_the_same_way() {
    cut_by_a_disable(Change::Peer).await;
}

/// A contest plugin the fleet forgets while one of its submissions is being
/// judged unloads like a disabled one: its judging session ends at once,
/// though no result arrives, instead of when the worker's next result does.
#[tokio::test(flavor = "multi_thread")]
async fn a_judging_session_of_a_plugin_the_fleet_forgets_ends_at_once() {
    let HeldJudging {
        app, sub_id, cut, ..
    } = held_judging().await;
    exec(&app, "DELETE FROM plugin WHERE id = 'ioi'").await;
    app.reconcile_plugins().await;
    tokio::time::timeout(std::time::Duration::from_secs(30), app.judging_idle())
        .await
        .expect("forgetting the plugin cut the judging session");
    let ended = current_judgement(&app, sub_id).await;
    assert!(
        ended.status != SubmissionStatus::Compiling || ended.judge_epoch > cut.judge_epoch,
        "the cut judgement left its attempt: {:?}",
        ended.status
    );
    app.resume_worker();
}

/// Where a plugin change comes from.
#[derive(Clone, Copy)]
enum Change {
    /// An admin asks this replica.
    Local,
    /// Another replica published it: its write to the plugin table, which
    /// this replica applies when it reconciles.
    Peer,
}

impl Change {
    async fn set_enabled(self, app: &E2eTestApp, admin: &str, enabled: bool) {
        match self {
            Change::Local => {
                let verb = if enabled { "enable" } else { "disable" };
                let res = app
                    .post_with_token(
                        &format!("/api/v1/admin/plugins/ioi/{verb}"),
                        &json!({}),
                        admin,
                    )
                    .await;
                assert!(res.status < 300, "{verb} failed: {}", res.text);
            }
            Change::Peer => {
                exec(
                    app,
                    &format!(
                        "UPDATE plugin SET is_enabled = {enabled}, generation = generation + 1 \
                         WHERE id = 'ioi'"
                    ),
                )
                .await;
                app.reconcile_plugins().await;
            }
        }
    }
}

async fn exec(app: &E2eTestApp, sql: &str) {
    use sea_orm::ConnectionTrait;
    app.db.execute_unprepared(sql).await.expect(sql);
}

/// An IOI submission whose judging session has started and waits for its
/// first result, which the paused worker never sends.
struct HeldJudging {
    app: E2eTestApp,
    admin: String,
    contestant: String,
    problem_id: i32,
    contest_id: i32,
    sub_id: i32,
    cut: submission_judgement::Model,
}

async fn held_judging() -> HeldJudging {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("ioi_disable_admin", "password", "admin")
        .await;
    let contestant = app
        .create_authenticated_user("ioi_disable_user", "password")
        .await;
    let problem_id = app.create_problem(&admin, "IOI Disable").await;
    // A scored test case (IOI leaves samples out of the score) whose output
    // the problem's exact checker accepts.
    app.create_test_case_with(problem_id, "5\n1 2 3 4 5", "15\n", 100, false, &admin)
        .await;
    let contest_id = app
        .create_typed_contest(&admin, "IOI Disable Contest", "ioi", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    app.register_for_contest(contest_id, &contestant).await;
    // Practice submissions of the problem are IOI submissions too.
    let res = app
        .patch_with_token(
            &format!("/api/v1/problems/{problem_id}"),
            &json!({"default_contest_type": "ioi"}),
            &admin,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);

    app.log_statuses().await;
    // Hold every operation in the queue, so the judging session is started
    // and waiting for its first result when the plugin goes away.
    app.pause_worker();
    let sub_id = app
        .create_contest_submission(contest_id, problem_id, &contestant, "cpp", CPP_SUM)
        .await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    let cut = loop {
        let judgement = current_judgement(&app, sub_id).await;
        if judgement.status == SubmissionStatus::Compiling {
            break judgement;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "judging never started: {:?}",
            judgement.status
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };

    HeldJudging {
        app,
        admin,
        contestant,
        problem_id,
        contest_id,
        sub_id,
        cut,
    }
}

async fn cut_by_a_disable(change: Change) {
    let HeldJudging {
        app,
        admin,
        contestant,
        problem_id,
        contest_id,
        sub_id,
        cut,
    } = held_judging().await;
    change.set_enabled(&app, &admin, false).await;
    // The disable ends the session at once, on the replica that made it and
    // on one that applied it alike, though no result arrives: the worker
    // still holds every operation.
    tokio::time::timeout(std::time::Duration::from_secs(60), app.judging_idle())
        .await
        .expect("the disable cut the judging session");
    let handed_back = current_judgement(&app, sub_id).await;
    assert_eq!(handed_back.status, SubmissionStatus::Queued);
    assert!(handed_back.judge_epoch > cut.judge_epoch);
    app.resume_worker();

    // The cut judgement goes back to the queue under a fresh epoch and stays
    // there, with its submission, while the plugin is disabled.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let judgement = current_judgement(&app, sub_id).await;
        let shown = submission::Entity::find_by_id(sub_id)
            .one(&app.db)
            .await
            .expect("query submission")
            .expect("the submission");
        if judgement.status == SubmissionStatus::Queued
            && judgement.judge_epoch > cut.judge_epoch
            && shown.status == SubmissionStatus::Queued
        {
            assert_eq!(shown.system_error_retries, 0);
            assert_eq!(judgement.dispatch_retries, cut.dispatch_retries);
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the cut judgement never went back to the queue: {:?} / {:?}",
            judgement.status,
            shown.status
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let res = app
        .get_with_token(&format!("/api/v1/submissions/{sub_id}"), &admin)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert!(res.body["withheld"].is_null(), "{}", res.text);
    assert_eq!(res.body["status"], "Queued", "{}", res.text);
    let outage = json!({"state": "disabled", "plugin_id": "ioi"});
    assert_eq!(res.body["plugin_outage"], outage, "{}", res.text);
    let res = app
        .get_with_token(
            &format!("/api/v1/contests/{contest_id}/submissions"),
            &admin,
        )
        .await;
    assert_eq!(res.body["data"][0]["plugin_outage"], outage, "{}", res.text);
    let res = app
        .get_with_token(&format!("/api/v1/submissions/{sub_id}"), &contestant)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert!(res.body["status"].is_null(), "{}", res.text);
    assert_eq!(
        res.body["withheld"],
        json!({"reason": "plugin_outage", "outage": {"state": "disabled", "plugin_id": "ioi"}}),
        "{}",
        res.text
    );

    let res = app
        .post_with_token(
            &format!("/api/v1/contests/{contest_id}/problems/{problem_id}/submissions"),
            &json!({
                "files": [{"filename": "main.cpp", "content": CPP_SUM}],
                "language": "cpp",
            }),
            &contestant,
        )
        .await;
    assert_eq!(res.status, 503, "{}", res.text);
    assert_eq!(res.body["code"], "CONTEST_TYPE_DISABLED", "{}", res.text);
    assert!(
        !res.headers.contains_key("retry-after"),
        "a disabled contest type is not a retry-later answer"
    );
    let practice = json!({
        "files": [{"filename": "main.cpp", "content": CPP_SUM}],
        "language": "cpp",
    });
    let res = app
        .post_with_token(
            &format!("/api/v1/problems/{problem_id}/submissions"),
            &practice,
            &admin,
        )
        .await;
    assert_eq!(res.status, 503, "{}", res.text);
    assert_eq!(res.body["code"], "CONTEST_TYPE_DISABLED", "{}", res.text);
    let res = app
        .post_with_token(
            &format!("/api/v1/problems/{problem_id}/code-runs"),
            &json!({
                "files": [{"filename": "main.cpp", "content": CPP_SUM}],
                "language": "cpp",
                "custom_test_cases": [{"input": "1\n1"}],
            }),
            &admin,
        )
        .await;
    assert_eq!(res.status, 503, "{}", res.text);
    assert_eq!(res.body["code"], "CONTEST_TYPE_DISABLED", "{}", res.text);
    let res = app
        .get_with_token(&format!("/api/v1/problems/{problem_id}"), &admin)
        .await;
    assert_eq!(res.body["plugin_outage"], outage, "{}", res.text);

    let cut_statuses = app.logged_statuses().await;
    assert!(
        !cut_statuses
            .iter()
            .any(|(_, status)| status == "SystemError"),
        "the cut judgement showed a SystemError on its way back: {cut_statuses:?}"
    );

    change.set_enabled(&app, &admin, true).await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    let shown = loop {
        let shown = submission::Entity::find_by_id(sub_id)
            .one(&app.db)
            .await
            .expect("query submission")
            .expect("the submission");
        if matches!(
            shown.status,
            SubmissionStatus::Judged
                | SubmissionStatus::CompilationError
                | SubmissionStatus::SystemError
        ) {
            break shown;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the parked submission was never judged: {:?}",
            shown.status
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    assert_eq!(shown.status, SubmissionStatus::Judged, "{shown:?}");
    assert_eq!(shown.verdict, Some(Verdict::Accepted), "{shown:?}");
    assert_eq!(shown.system_error_retries, 0);
    assert_eq!(current_judgement(&app, sub_id).await.dispatch_retries, 0);
}

async fn current_judgement(app: &E2eTestApp, sub_id: i32) -> submission_judgement::Model {
    submission_judgement::Entity::find()
        .filter(submission_judgement::Column::SubmissionId.eq(sub_id))
        .filter(submission_judgement::Column::IsCurrent.eq(true))
        .one(&app.db)
        .await
        .expect("query judgement")
        .expect("the submission has a current judgement")
}

async fn create_labelled_test_case(
    app: &E2eTestApp,
    problem_id: i32,
    label: &str,
    token: &str,
) -> i32 {
    let res = app
        .post_with_token(
            &format!("/api/v1/problems/{problem_id}/test-cases"),
            &json!({
                "input": "1 2",
                "expected_output": "3",
                "score": 50,
                "is_sample": false,
                "label": label,
            }),
            token,
        )
        .await;
    assert_eq!(res.status, 201, "create test case: {}", res.text);
    res.id()
}

/// A subtask naming a test case the problem does not have would score 0 for
/// every submission, so the write is refused with the field that names it and
/// nothing is stored.
#[tokio::test(flavor = "multi_thread")]
async fn ioi_task_config_naming_a_missing_test_case_is_refused() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("ioi_validate_admin", "password", "admin")
        .await;
    let problem_id = app.create_problem(&admin, "IOI Validate").await;
    create_labelled_test_case(&app, problem_id, "t1", &admin).await;
    let contest_id = app
        .create_typed_contest(&admin, "IOI Validate Contest", "ioi", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    let path = format!("/api/v1/contests/{contest_id}/problems/{problem_id}/config/ioi/task");
    let subtasks = |members: &[&str]| {
        json!({ "config": { "subtasks": [{
            "name": "S",
            "scoring_method": "sum",
            "max_score": 100,
            "test_cases": members,
        }]}})
    };

    let res = app
        .put_with_token(&path, &subtasks(&["nope"]), &admin)
        .await;
    assert_eq!(res.status, 400, "{}", res.text);
    assert_eq!(res.body["code"], "VALIDATION_ERROR", "{}", res.text);
    assert_eq!(
        res.body["details"]["fields"][0]["path"], "subtasks[0].test_cases[0]",
        "{}",
        res.text
    );
    let stored = app.get_with_token(&path, &admin).await;
    assert_eq!(stored.status, 404, "{}", stored.text);

    let res = app.put_with_token(&path, &subtasks(&["t1"]), &admin).await;
    assert_eq!(res.status, 200, "{}", res.text);
}

/// An IOI problem whose contest-problem `task` config splits test cases
/// `t1` and `t2` into one subtask each. Returns the problem, the contest, and
/// `t2`'s id.
async fn ioi_problem_with_two_subtasks(app: &E2eTestApp, admin: &str) -> (i32, i32, i32) {
    let problem_id = app.create_problem(admin, "IOI Issues").await;
    create_labelled_test_case(app, problem_id, "t1", admin).await;
    let t2 = create_labelled_test_case(app, problem_id, "t2", admin).await;
    let contest_id = app
        .create_typed_contest(admin, "IOI Issues Contest", "ioi", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, admin)
        .await;
    let res = app
        .put_with_token(
            &format!("/api/v1/contests/{contest_id}/problems/{problem_id}/config/ioi/task"),
            &json!({ "config": { "subtasks": [
                { "name": "S1", "test_cases": ["t1"] },
                { "name": "S2", "test_cases": ["t2"] },
            ]}}),
            admin,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    (problem_id, contest_id, t2)
}

/// The one issue a broken `S2` subtask makes, as the API lists it.
fn assert_s2_issue(issues: &serde_json::Value, contest_id: i32, problem_id: i32) {
    let issues = issues.as_array().expect("an array");
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(issues[0]["plugin_id"], "ioi");
    assert_eq!(issues[0]["scope"], "contest_problem");
    assert_eq!(issues[0]["ref_id"], format!("{contest_id}:{problem_id}"));
    assert_eq!(issues[0]["namespace"], "task");
    assert_eq!(
        issues[0]["errors"][0]["path"], "subtasks[1].test_cases[0]",
        "{issues:?}"
    );
}

/// Renaming a test case a stored config names would leave that config
/// pointing at nothing, so the edit is refused with the issues it would cause
/// and nothing changes. Confirming with `force` commits it, and the problem's
/// config issues then list the config.
#[tokio::test(flavor = "multi_thread")]
async fn renaming_a_test_case_a_config_names_is_refused_until_confirmed() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("ioi_rename_admin", "password", "admin")
        .await;
    let (problem_id, contest_id, t2) = ioi_problem_with_two_subtasks(&app, &admin).await;
    let issues_path = format!("/api/v1/problems/{problem_id}/config/issues");
    let issues = app.get_with_token(&issues_path, &admin).await;
    assert_eq!(issues.body, json!([]), "{}", issues.text);

    let tc_path = format!("/api/v1/problems/{problem_id}/test-cases/{t2}");
    let res = app
        .patch_with_token(&tc_path, &json!({ "label": "t9" }), &admin)
        .await;
    assert_eq!(res.status, 409, "{}", res.text);
    assert_eq!(res.body["code"], "CONFIG_WOULD_BREAK", "{}", res.text);
    assert_s2_issue(&res.body["details"]["issues"], contest_id, problem_id);
    let stored = app.get_with_token(&tc_path, &admin).await;
    assert_eq!(stored.body["label"], "t2", "{}", stored.text);
    let issues = app.get_with_token(&issues_path, &admin).await;
    assert_eq!(issues.body, json!([]), "{}", issues.text);

    let res = app
        .patch_with_token(
            &format!("{tc_path}?force=true"),
            &json!({ "label": "t9" }),
            &admin,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert_eq!(res.body["label"], "t9", "{}", res.text);
    let issues = app.get_with_token(&issues_path, &admin).await;
    assert_eq!(issues.status, 200, "{}", issues.text);
    assert_s2_issue(&issues.body, contest_id, problem_id);
}

/// Deleting a test case a stored config names is refused the same way, and
/// commits once confirmed.
#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_test_case_a_config_names_is_refused_until_confirmed() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("ioi_delete_admin", "password", "admin")
        .await;
    let (problem_id, contest_id, t2) = ioi_problem_with_two_subtasks(&app, &admin).await;
    let tc_path = format!("/api/v1/problems/{problem_id}/test-cases/{t2}");

    let res = app.delete_with_token(&tc_path, &admin).await;
    assert_eq!(res.status, 409, "{}", res.text);
    assert_eq!(res.body["code"], "CONFIG_WOULD_BREAK", "{}", res.text);
    assert_s2_issue(&res.body["details"]["issues"], contest_id, problem_id);
    let stored = app.get_with_token(&tc_path, &admin).await;
    assert_eq!(stored.status, 200, "{}", stored.text);

    let res = app
        .delete_with_token(&format!("{tc_path}?force=true"), &admin)
        .await;
    assert_eq!(res.status, 204, "{}", res.text);
    let issues = app
        .get_with_token(
            &format!("/api/v1/problems/{problem_id}/config/issues"),
            &admin,
        )
        .await;
    assert_s2_issue(&issues.body, contest_id, problem_id);
}

/// A test-data edit that leaves every config valid needs no confirmation,
/// and one that only keeps an issue the problem already had is not refused
/// for it.
#[tokio::test(flavor = "multi_thread")]
async fn a_test_data_edit_that_breaks_no_config_needs_no_confirmation() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("ioi_harmless_admin", "password", "admin")
        .await;
    let (problem_id, _contest_id, t2) = ioi_problem_with_two_subtasks(&app, &admin).await;
    let tc_path = format!("/api/v1/problems/{problem_id}/test-cases/{t2}");
    let res = app
        .patch_with_token(&tc_path, &json!({ "score": 70 }), &admin)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);

    let res = app
        .patch_with_token(
            &format!("{tc_path}?force=true"),
            &json!({ "label": "t9" }),
            &admin,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    create_labelled_test_case(&app, problem_id, "t3", &admin).await;
    let res = app
        .patch_with_token(&tc_path, &json!({ "description": "still broken" }), &admin)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
}

/// A submission judged while the problem's stored configs have issues says
/// so on its judgement, for staff, so a score computed from a broken config
/// does not pass unnoticed. Fixing the config clears the mark for the next
/// judging.
#[tokio::test(flavor = "multi_thread")]
async fn a_judgement_made_while_a_config_has_issues_is_marked() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("ioi_honest_admin", "password", "admin")
        .await;
    let contestant = app
        .create_authenticated_user("ioi_honest_user", "password")
        .await;
    let (problem_id, contest_id, t2) = ioi_problem_with_two_subtasks(&app, &admin).await;
    app.register_for_contest(contest_id, &contestant).await;
    let judgement_marks = |sub_id: i32| {
        let app = &app;
        let admin = admin.clone();
        async move {
            app.wait_for_submission_terminal(sub_id, &admin, 60).await;
            let res = app
                .get_with_token(&format!("/api/v1/submissions/{sub_id}/judgements"), &admin)
                .await;
            assert_eq!(res.status, 200, "{}", res.text);
            res.body["data"]
                .as_array()
                .or_else(|| res.body.as_array())
                .expect("judgements")
                .iter()
                .find(|j| j["is_current"] == true)
                .expect("a current judgement")["config_issues"]
                .clone()
        }
    };

    let clean = app
        .create_contest_submission(contest_id, problem_id, &contestant, "cpp", CPP_SUM)
        .await;
    assert_eq!(judgement_marks(clean).await, json!(false));

    let res = app
        .patch_with_token(
            &format!("/api/v1/problems/{problem_id}/test-cases/{t2}?force=true"),
            &json!({ "label": "t9" }),
            &admin,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    let broken = app
        .create_contest_submission(contest_id, problem_id, &contestant, "cpp", CPP_SUM)
        .await;
    assert_eq!(judgement_marks(broken).await, json!(true));

    let res = app
        .put_with_token(
            &format!("/api/v1/contests/{contest_id}/problems/{problem_id}/config/ioi/task"),
            &json!({ "config": { "subtasks": [
                { "name": "S1", "test_cases": ["t1"] },
                { "name": "S2", "test_cases": ["t9"] },
            ]}}),
            &admin,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    let fixed = app
        .create_contest_submission(contest_id, problem_id, &contestant, "cpp", CPP_SUM)
        .await;
    assert_eq!(judgement_marks(fixed).await, json!(false));
}

/// A change to a plugin changes which stored configs are checked, and how,
/// so it rechecks the flag of every problem with a config of that plugin:
/// disabling `ioi` leaves its broken config unchecked, enabling it checks it
/// again, and a reload corrects a flag that went stale.
#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_change_rechecks_the_config_issues_of_the_problems_it_configures() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("ioi_lifecycle_issues_admin", "password", "admin")
        .await;
    let (problem_id, _contest_id, t2) = ioi_problem_with_two_subtasks(&app, &admin).await;
    let res = app
        .patch_with_token(
            &format!("/api/v1/problems/{problem_id}/test-cases/{t2}?force=true"),
            &json!({ "label": "t9" }),
            &admin,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert!(app.config_issues_flag(problem_id).await);

    let res = app
        .post_with_token("/api/v1/admin/plugins/ioi/disable", &json!({}), &admin)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert!(
        !app.config_issues_flag(problem_id).await,
        "a disabled plugin's configs are not checked"
    );

    let res = app
        .post_with_token("/api/v1/admin/plugins/ioi/enable", &json!({}), &admin)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert!(app.config_issues_flag(problem_id).await);

    app.set_config_issues_flag(problem_id, false).await;
    let res = app
        .post_with_token("/api/v1/admin/plugins/ioi/reload", &json!({}), &admin)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert!(app.config_issues_flag(problem_id).await);
}

/// A write to a contest's own configs can change what its problems' configs
/// are checked against, so it rechecks the flag of every problem in the
/// contest, both ways.
#[tokio::test(flavor = "multi_thread")]
async fn a_contest_config_write_rechecks_the_config_issues_of_its_problems() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("ioi_contest_issues_admin", "password", "admin")
        .await;
    let (problem_id, contest_id, t2) = ioi_problem_with_two_subtasks(&app, &admin).await;
    let contest_config = format!("/api/v1/contests/{contest_id}/config/ioi/contest");
    let write_contest_config = |mode: &'static str| {
        let (app, admin, path) = (&app, admin.clone(), contest_config.clone());
        async move {
            let res = app
                .put_with_token(
                    &path,
                    &json!({ "config": { "scoring_mode": mode }, "enabled": true }),
                    &admin,
                )
                .await;
            assert_eq!(res.status, 200, "{}", res.text);
        }
    };

    app.set_config_issues_flag(problem_id, true).await;
    write_contest_config("sum_best_subtask").await;
    assert!(
        !app.config_issues_flag(problem_id).await,
        "no config has issues, so the stale flag is cleared"
    );

    let res = app
        .patch_with_token(
            &format!("/api/v1/problems/{problem_id}/test-cases/{t2}?force=true"),
            &json!({ "label": "t9" }),
            &admin,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    app.set_config_issues_flag(problem_id, false).await;
    write_contest_config("max_submission").await;
    assert!(
        app.config_issues_flag(problem_id).await,
        "the broken subtask config sets the stale flag again"
    );
}

/// An IOI problem whose contest-problem config names a test case that is
/// renamed away, so its `config_issues` flag is set. Returns the problem and
/// the contest.
async fn ioi_problem_with_a_config_issue(app: &E2eTestApp, admin: &str) -> (i32, i32) {
    let (problem_id, contest_id, t2) = ioi_problem_with_two_subtasks(app, admin).await;
    let res = app
        .patch_with_token(
            &format!("/api/v1/problems/{problem_id}/test-cases/{t2}?force=true"),
            &json!({ "label": "t9" }),
            admin,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert!(app.config_issues_flag(problem_id).await);
    (problem_id, contest_id)
}

/// Removing a problem from a contest deletes its configs in that contest, so
/// an issue only they had goes with them, and so does the flag.
#[tokio::test(flavor = "multi_thread")]
async fn removing_a_contest_problem_clears_the_config_issues_its_configs_had() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("ioi_remove_issues_admin", "password", "admin")
        .await;
    let (problem_id, contest_id) = ioi_problem_with_a_config_issue(&app, &admin).await;

    let res = app
        .delete_with_token(
            &format!("/api/v1/contests/{contest_id}/problems/{problem_id}"),
            &admin,
        )
        .await;
    assert_eq!(res.status, 204, "{}", res.text);
    let issues = app
        .get_with_token(
            &format!("/api/v1/problems/{problem_id}/config/issues"),
            &admin,
        )
        .await;
    assert_eq!(issues.body, json!([]), "{}", issues.text);
    assert!(!app.config_issues_flag(problem_id).await);
}

/// The bulk removal deletes the same configs, and clears the flag the same
/// way.
#[tokio::test(flavor = "multi_thread")]
async fn bulk_removing_contest_problems_clears_the_config_issues_their_configs_had() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("ioi_bulk_issues_admin", "password", "admin")
        .await;
    let (problem_id, contest_id) = ioi_problem_with_a_config_issue(&app, &admin).await;

    let res = app
        .delete_with_body_and_token(
            &format!("/api/v1/contests/{contest_id}/problems/bulk"),
            &json!({ "problem_ids": [problem_id] }),
            &admin,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert!(!app.config_issues_flag(problem_id).await);
}

/// Deleting a contest deletes its configs and those of its problems in it,
/// so the flag those configs set clears.
#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_contest_clears_the_config_issues_its_configs_had() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("ioi_contest_delete_issues_admin", "password", "admin")
        .await;
    let (problem_id, contest_id) = ioi_problem_with_a_config_issue(&app, &admin).await;

    let res = app
        .delete_with_token(&format!("/api/v1/contests/{contest_id}"), &admin)
        .await;
    assert_eq!(res.status, 204, "{}", res.text);
    assert!(!app.config_issues_flag(problem_id).await);
}

/// A server start loads plugins that may check configs differently than the
/// ones that checked them last, and finds configs no running server checked
/// (a restored bundle's), so it rechecks the flag of every problem with a
/// stored config, both ways.
#[tokio::test(flavor = "multi_thread")]
async fn a_server_start_rechecks_the_config_issues_of_every_configured_problem() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("ioi_start_issues_admin", "password", "admin")
        .await;
    let (broken, _) = ioi_problem_with_a_config_issue(&app, &admin).await;
    let (clean, _, _) = ioi_problem_with_two_subtasks(&app, &admin).await;
    app.set_config_issues_flag(broken, false).await;
    app.set_config_issues_flag(clean, true).await;

    app.restart_plugins().await;
    assert!(app.config_issues_flag(broken).await);
    assert!(!app.config_issues_flag(clean).await);
}
