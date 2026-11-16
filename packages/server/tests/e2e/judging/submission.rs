use serde_json::json;

use crate::common::{E2eTestApp, require_real_sandbox};

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges real programs in isolate; the sandbox CI job runs it (see common::require_real_sandbox)"]
async fn submission_reaches_terminal_state() {
    require_real_sandbox();

    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("sub_admin1", "pass1234", "admin")
        .await;

    let problem_id = app.create_problem(&admin, "Terminal Test").await;
    app.create_test_case(problem_id, &admin).await;

    let sub_id = app
        .create_submission(problem_id, &admin, "cpp", CPP_SUM)
        .await;

    let res = app.wait_for_submission_terminal(sub_id, &admin, 60).await;
    let status = res.body["status"].as_str().unwrap();
    assert_eq!(
        status, "Judged",
        "submission should judge cleanly: {}",
        res.text
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges real programs in isolate; the sandbox CI job runs it (see common::require_real_sandbox)"]
async fn submission_has_test_case_results_when_judged() {
    require_real_sandbox();

    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("sub_admin2", "pass1234", "admin")
        .await;

    let problem_id = app.create_problem(&admin, "TCR Test").await;
    app.create_test_case(problem_id, &admin).await;

    let sub_id = app
        .create_submission(problem_id, &admin, "cpp", CPP_SUM)
        .await;

    let res = app.wait_for_submission_terminal(sub_id, &admin, 60).await;
    let status = res.body["status"].as_str().unwrap();
    assert_eq!(
        status, "Judged",
        "submission should judge cleanly: {}",
        res.text
    );

    let result = &res.body["result"];
    assert!(
        !result.is_null(),
        "Judged submission should have a result object"
    );
    let tcrs = result["test_case_results"].as_array().unwrap();
    assert!(
        !tcrs.is_empty(),
        "Judged submission should have at least one test case result"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges real programs in isolate; the sandbox CI job runs it (see common::require_real_sandbox)"]
async fn submission_status_transitions_observed() {
    require_real_sandbox();

    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("sub_admin3", "pass1234", "admin")
        .await;

    let problem_id = app.create_problem(&admin, "Status Transition").await;
    app.create_test_case(problem_id, &admin).await;

    let sub_res = app
        .post_with_token(
            &format!("/api/v1/problems/{problem_id}/submissions"),
            &json!({
                "files": [{"filename": "main.cpp", "content": CPP_SUM}],
                "language": "cpp",
            }),
            &admin,
        )
        .await;
    assert_eq!(sub_res.status, 201);
    let sub_id = sub_res.id();

    let immediate = app
        .get_with_token(&format!("/api/v1/submissions/{sub_id}"), &admin)
        .await;
    assert_eq!(immediate.status, 200);
    let initial_status = immediate.body["status"].as_str().unwrap();
    assert!(
        [
            "Pending",
            "Compiling",
            "Running",
            "Judging",
            "Judged",
            "CompilationError",
            "SystemError"
        ]
        .contains(&initial_status),
        "Unexpected status: {initial_status}"
    );

    app.wait_for_submission_terminal(sub_id, &admin, 60).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges real programs in isolate; the sandbox CI job runs it (see common::require_real_sandbox)"]
async fn contest_submission_reaches_terminal_state() {
    require_real_sandbox();

    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("csub_admin1", "pass1234", "admin")
        .await;
    let user = app
        .create_authenticated_user("csub_user1", "pass1234")
        .await;

    let problem_id = app.create_problem(&admin, "Contest Terminal").await;
    app.create_test_case(problem_id, &admin).await;

    let contest_id = app
        .create_contest(&admin, "Contest Sub Test", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    app.register_for_contest(contest_id, &user).await;

    let sub_id = app
        .create_contest_submission(contest_id, problem_id, &user, "cpp", CPP_SUM)
        .await;

    let res = app.wait_for_submission_terminal(sub_id, &user, 60).await;
    let status = res.body["status"].as_str().unwrap();
    assert_eq!(
        status, "Judged",
        "contest submission should judge cleanly: {}",
        res.text
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rejudge_resets_to_pending_then_reaches_terminal() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("rejudge_admin1", "pass1234", "admin")
        .await;

    let problem_id = app.create_problem(&admin, "Rejudge Test").await;

    let sub_id = app
        .create_submission(problem_id, &admin, "cpp", CPP_SUM)
        .await;
    let first = app.wait_for_submission_terminal(sub_id, &admin, 60).await;
    let original_epoch = first.body["judge_epoch"].as_i64().unwrap();

    let rejudge_res = app
        .post_with_token(
            &format!("/api/v1/submissions/{sub_id}/rejudge"),
            &json!({}),
            &admin,
        )
        .await;
    assert_eq!(
        rejudge_res.status, 200,
        "Rejudge should succeed: {}",
        rejudge_res.text
    );
    let rejudge_epoch = rejudge_res.body["judge_epoch"].as_i64().unwrap();
    assert!(
        rejudge_epoch > original_epoch,
        "judge_epoch should increment on rejudge (was {original_epoch}, got {rejudge_epoch})"
    );

    let second = app.wait_for_submission_terminal(sub_id, &admin, 60).await;
    let final_epoch = second.body["judge_epoch"].as_i64().unwrap();
    assert_eq!(
        final_epoch, rejudge_epoch,
        "judge_epoch should remain at the rejudge value"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn bulk_rejudge_resets_multiple_submissions() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("bulk_admin1", "pass1234", "admin")
        .await;

    let problem_id = app.create_problem(&admin, "Bulk Rejudge").await;

    let mut sub_ids = Vec::new();
    for _ in 0..3 {
        let id = app
            .create_submission(problem_id, &admin, "cpp", CPP_SUM)
            .await;
        sub_ids.push(id);
    }
    for &id in &sub_ids {
        app.wait_for_submission_terminal(id, &admin, 60).await;
    }

    let mut original_epochs = Vec::new();
    for &id in &sub_ids {
        let res = app
            .get_with_token(&format!("/api/v1/submissions/{id}"), &admin)
            .await;
        original_epochs.push(res.body["judge_epoch"].as_i64().unwrap());
    }

    let bulk_res = app
        .post_with_token(
            "/api/v1/submissions/bulk-rejudge",
            &json!({ "submission_ids": sub_ids }),
            &admin,
        )
        .await;
    assert_eq!(
        bulk_res.status, 200,
        "Bulk rejudge failed: {}",
        bulk_res.text
    );
    let queued = bulk_res.body["queued"].as_i64().unwrap();
    assert_eq!(
        queued, 3,
        "All 3 submissions should be queued for rejudging"
    );

    for &id in &sub_ids {
        app.wait_for_submission_terminal(id, &admin, 60).await;
    }

    for (i, &id) in sub_ids.iter().enumerate() {
        let res = app
            .get_with_token(&format!("/api/v1/submissions/{id}"), &admin)
            .await;
        let new_epoch = res.body["judge_epoch"].as_i64().unwrap();
        assert!(
            new_epoch > original_epochs[i],
            "Submission {id}: judge_epoch should increase after bulk rejudge \
             (was {}, got {new_epoch})",
            original_epochs[i]
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges real programs in isolate; the sandbox CI job runs it (see common::require_real_sandbox)"]
async fn multiple_test_cases_all_get_results() {
    require_real_sandbox();

    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("multi_tc_admin1", "pass1234", "admin")
        .await;

    let problem_id = app.create_problem(&admin, "Multi TC").await;
    let tc1 = app
        .create_test_case_with(problem_id, "1\n", "1\n", 10, true, &admin)
        .await;
    let tc2 = app
        .create_test_case_with(problem_id, "2\n", "2\n", 10, false, &admin)
        .await;
    let tc3 = app
        .create_test_case_with(problem_id, "3\n", "3\n", 10, false, &admin)
        .await;

    let sub_id = app
        .create_submission(problem_id, &admin, "cpp", CPP_SUM)
        .await;
    let res = app.wait_for_submission_terminal(sub_id, &admin, 60).await;
    assert_eq!(
        res.body["status"].as_str(),
        Some("Judged"),
        "submission should judge cleanly: {}",
        res.text
    );

    let tcrs = res.body["result"]["test_case_results"].as_array().unwrap();
    assert_eq!(
        tcrs.len(),
        3,
        "Should have exactly 3 test case results, got {}",
        tcrs.len()
    );

    let tcr_tc_ids: Vec<i64> = tcrs
        .iter()
        .filter_map(|r| r["test_case_id"].as_i64())
        .collect();
    for expected_id in [tc1, tc2, tc3] {
        assert!(
            tcr_tc_ids.contains(&(expected_id as i64)),
            "Missing TCR for test_case_id {expected_id}; found: {tcr_tc_ids:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges real programs in isolate; the sandbox CI job runs it (see common::require_real_sandbox)"]
async fn submission_judges_blob_backed_testcase_input_and_expected_output() {
    require_real_sandbox();

    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("blob_tc_admin1", "pass1234", "admin")
        .await;

    let problem_id = app.create_problem(&admin, "Blob Backed Judging").await;
    let large_body = "a".repeat(1_048_576 + 64);
    app.create_test_case_with(problem_id, &large_body, &large_body, 10, false, &admin)
        .await;

    let echo_source = r#"
#include <iostream>
int main() {
    std::cout << std::cin.rdbuf();
    return 0;
}
"#;
    let sub_id = app
        .create_submission(problem_id, &admin, "cpp", echo_source)
        .await;
    let res = app.wait_for_submission_terminal(sub_id, &admin, 120).await;

    assert_eq!(
        res.body["status"].as_str(),
        Some("Judged"),
        "blob-backed testcase submission should judge successfully: {}",
        res.text
    );
    assert_eq!(res.body["verdict"].as_str(), Some("Accepted"));
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_submissions_all_complete() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("conc_admin1", "pass1234", "admin")
        .await;

    let problem_id = app.create_problem(&admin, "Concurrent").await;

    let mut sub_ids = Vec::new();
    for _ in 0..5 {
        let id = app
            .create_submission(problem_id, &admin, "cpp", CPP_SUM)
            .await;
        sub_ids.push(id);
    }

    for &id in &sub_ids {
        let res = app.wait_for_submission_terminal(id, &admin, 60).await;
        let status = res.body["status"].as_str().unwrap();
        assert!(
            matches!(status, "Judged" | "CompilationError" | "SystemError"),
            "Submission {id} did not reach terminal state, got: {status}"
        );
    }
}

use super::CPP_SUM;

/// Status, verdict and judge epoch of the submission, as stored.
async fn stored(app: &E2eTestApp, id: i32) -> server::entity::submission::Model {
    use sea_orm::EntityTrait;
    server::entity::submission::Entity::find_by_id(id)
        .one(&app.db)
        .await
        .unwrap()
        .unwrap()
}

/// The submission was judged and `CPP_SUM` was accepted.
fn assert_accepted(res: &test_support::TestResponse) {
    assert_eq!(res.body["status"], "Judged", "{}", res.text);
    assert_eq!(res.body["result"]["verdict"], "Accepted", "{}", res.text);
}

/// A worker without its platform tools takes no work, so in a fleet with a
/// healthy worker every submission is judged there on its first attempt.
#[tokio::test(flavor = "multi_thread")]
async fn a_worker_without_platform_tools_leaves_every_submission_to_one_that_has_them() {
    let app = E2eTestApp::spawn_with_workers(vec![
        E2eTestApp::platform_tools_dir(),
        Some(E2eTestApp::empty_tools_dir()),
    ])
    .await;
    let admin = app
        .create_user_with_role("no_tools_admin", "pass1234", "admin")
        .await;
    let problem_id = app.create_public_problem(&admin, "No tools").await;
    app.create_test_case(problem_id, &admin).await;
    let contestant = app
        .create_authenticated_user("no_tools_user", "pass1234")
        .await;

    let mut ids = Vec::new();
    for _ in 0..6 {
        ids.push(
            app.create_submission(problem_id, &contestant, "cpp", CPP_SUM)
                .await,
        );
    }
    for id in ids {
        let res = app.wait_for_submission_terminal(id, &contestant, 60).await;
        assert_accepted(&res);
        let row = stored(&app, id).await;
        assert_eq!(row.system_error_retries, 0);
        assert_eq!(row.judge_epoch, 0, "judged on its first attempt");
    }
}

/// While the only live worker lacks a platform tool, submissions wait
/// queued and the worker list says why; once the tool is in place the worker
/// takes them and judges each on its first attempt.
#[tokio::test(flavor = "multi_thread")]
async fn a_lone_worker_without_platform_tools_leaves_submissions_queued_until_it_has_them() {
    let tools = E2eTestApp::empty_tools_dir();
    let app = E2eTestApp::spawn_with_workers(vec![Some(tools.clone())]).await;
    let admin = app
        .create_user_with_role("lone_no_tools_admin", "pass1234", "admin")
        .await;
    let problem_id = app.create_public_problem(&admin, "Lone no tools").await;
    app.create_test_case(problem_id, &admin).await;
    let contestant = app
        .create_authenticated_user("lone_no_tools_user", "pass1234")
        .await;

    let id = app
        .create_submission(problem_id, &contestant, "cpp", CPP_SUM)
        .await;
    let res = app
        .get_with_token("/api/v1/admin/system/workers", &admin)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    let worker = &res.body["workers"][0];
    assert_eq!(worker["accepting_work"], false, "{}", res.text);
    assert_eq!(
        worker["missing_platform_tools"],
        json!(["broccoli-compare"]),
        "{}",
        res.text
    );
    let row = stored(&app, id).await;
    assert_eq!(row.status, common::SubmissionStatus::Queued);

    let healthy = E2eTestApp::platform_tools_dir().expect("the built platform tools");
    for tool in broccoli_server_sdk::types::PLATFORM_TOOLS {
        std::fs::copy(healthy.join(tool), tools.join(tool)).unwrap();
    }
    let res = app.wait_for_submission_terminal(id, &contestant, 120).await;
    assert_accepted(&res);
    let row = stored(&app, id).await;
    assert_eq!(row.system_error_retries, 0);
    assert_eq!(row.judge_epoch, 0, "judged on its first attempt");
}

/// When the last worker that takes work loses a platform tool while a
/// submission is being judged, the platform lost that attempt: it goes back
/// to the queue without the contest plugin ever seeing its failed
/// operations, so neither the judgement nor the submission ever reads
/// `SystemError` or `Judged` before the verdict of the attempt that stands,
/// and no retry is spent.
#[tokio::test(flavor = "multi_thread")]
async fn a_judgement_whose_last_capable_worker_loses_a_tool_mid_run_shows_only_its_real_verdict() {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use server::entity::submission_judgement;

    let healthy = E2eTestApp::platform_tools_dir().expect("the built platform tools");
    let tools = E2eTestApp::empty_tools_dir();
    let restore = || {
        for tool in broccoli_server_sdk::types::PLATFORM_TOOLS {
            std::fs::copy(healthy.join(tool), tools.join(tool)).unwrap();
        }
    };
    restore();
    let app = E2eTestApp::spawn_with_workers(vec![Some(tools.clone())]).await;
    let admin = app
        .create_user_with_role("lost_tool_admin", "pass1234", "admin")
        .await;
    let problem_id = app.create_public_problem(&admin, "Lost tool").await;
    app.create_test_case(problem_id, &admin).await;
    let contestant = app
        .create_authenticated_user("lost_tool_user", "pass1234")
        .await;

    app.log_statuses().await;
    // Every operation waits in the queue, so the judging session is started
    // and waiting for its first result when the tool goes.
    app.pause_worker();
    let id = app
        .create_submission(problem_id, &contestant, "cpp", CPP_SUM)
        .await;
    let current = || async {
        submission_judgement::Entity::find()
            .filter(submission_judgement::Column::SubmissionId.eq(id))
            .filter(submission_judgement::Column::IsCurrent.eq(true))
            .one(&app.db)
            .await
            .unwrap()
            .unwrap()
    };
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    let first = loop {
        let j = current().await;
        if j.status == common::SubmissionStatus::Compiling {
            break j;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "judging never started: {:?}",
            j.status
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };

    for tool in broccoli_server_sdk::types::PLATFORM_TOOLS {
        std::fs::remove_file(tools.join(tool)).unwrap();
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let j = current().await;
        if j.judge_epoch > first.judge_epoch {
            assert_eq!(j.status, common::SubmissionStatus::Queued, "handed back");
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "never handed back: {:?}",
            j.status
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    restore();
    app.resume_worker();
    let res = app.wait_for_submission_terminal(id, &contestant, 120).await;
    assert_accepted(&res);
    app.judging_idle().await;
    let row = stored(&app, id).await;
    assert_eq!(row.system_error_retries, 0, "no retry spent");

    let statuses = app.logged_statuses().await;
    for table in ["submission", "submission_judgement"] {
        let seen: Vec<&str> = statuses
            .iter()
            .filter(|(t, _)| t == table)
            .map(|(_, s)| s.as_str())
            .collect();
        assert!(!seen.contains(&"SystemError"), "{table}: {seen:?}");
        assert_eq!(
            seen.iter().position(|s| *s == "Judged"),
            Some(seen.len() - 1),
            "{table} reads Judged only for the verdict that stands: {seen:?}"
        );
        assert!(seen.contains(&"Queued"), "{table}: {seen:?}");
    }
}

/// A problem with nothing to judge refuses every submission at once, inside a
/// contest and outside: its contest plugin ends it as a system error that
/// stands, under the plugin's own code, so it is judged once and never reads
/// as Accepted, whatever the source.
#[tokio::test(flavor = "multi_thread")]
async fn a_problem_without_test_cases_refuses_its_submissions_at_once() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("no_tests_admin", "pass1234", "admin")
        .await;
    let broken = "int main( {";

    let practice = app.create_public_problem(&admin, "No tests").await;
    let mut cases = vec![(
        app.create_submission(practice, &admin, "cpp", broken).await,
        "icpc.NO_TEST_CASES",
    )];
    for contest_type in ["icpc", "ioi"] {
        let problem = app.create_problem(&admin, "No tests").await;
        let contest = app
            .create_typed_contest(&admin, "No tests", contest_type, true, true)
            .await;
        app.add_problem_to_contest(contest, problem, &admin).await;
        let id = app
            .create_contest_submission(contest, problem, &admin, "cpp", broken)
            .await;
        cases.push((
            id,
            if contest_type == "icpc" {
                "icpc.NO_TEST_CASES"
            } else {
                "ioi.NO_TEST_CASES"
            },
        ));
    }

    for (id, code) in cases {
        let res = app.wait_for_submission_terminal(id, &admin, 60).await;
        assert_eq!(res.body["status"], "Judged", "{}", res.text);
        assert_eq!(res.body["result"]["verdict"], "SystemError", "{}", res.text);
        let row = stored(&app, id).await;
        assert_eq!(row.error_code.as_deref(), Some(code));
        assert_eq!(row.system_error_retries, 0, "{code}: never re-judged");
        assert_eq!(row.judge_epoch, 0, "{code}: judged once");
    }
}
