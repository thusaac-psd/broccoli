use serde_json::json;

use crate::common::{E2eTestApp, require_real_sandbox};

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges real programs in isolate; the sandbox CI job runs it (see common::require_real_sandbox)"]
async fn code_run_reaches_terminal_state() {
    require_real_sandbox();

    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("cr_admin1", "pass1234", "admin")
        .await;

    let problem_id = app.create_problem(&admin, "Code Run Terminal").await;

    let res = app
        .post_with_token(
            &format!("/api/v1/problems/{problem_id}/code-runs"),
            &json!({
                "files": [{"filename": "main.cpp", "content": CPP_ECHO}],
                "language": "cpp",
                "custom_test_cases": [
                    {"input": "hello world", "expected_output": "hello world"}
                ],
            }),
            &admin,
        )
        .await;
    assert_eq!(res.status, 201, "create code run failed: {}", res.text);
    let cr_id = res.id();

    let terminal = app.wait_for_code_run_terminal(cr_id, &admin, 60).await;
    let status = terminal.body["status"].as_str().unwrap();
    assert_eq!(
        status, "Judged",
        "code run should judge cleanly: {}",
        terminal.text
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges real programs in isolate; the sandbox CI job runs it (see common::require_real_sandbox)"]
async fn code_run_with_multiple_custom_test_cases() {
    require_real_sandbox();

    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("cr_admin2", "pass1234", "admin")
        .await;

    let problem_id = app.create_problem(&admin, "Code Run Multi TC").await;

    let res = app
        .post_with_token(
            &format!("/api/v1/problems/{problem_id}/code-runs"),
            &json!({
                "files": [{"filename": "main.cpp", "content": CPP_ECHO}],
                "language": "cpp",
                "custom_test_cases": [
                    {"input": "tc1", "expected_output": "tc1"},
                    {"input": "tc2", "expected_output": "tc2"},
                    {"input": "tc3", "expected_output": "tc3"},
                ],
            }),
            &admin,
        )
        .await;
    assert_eq!(res.status, 201, "create code run failed: {}", res.text);
    let cr_id = res.id();

    let terminal = app.wait_for_code_run_terminal(cr_id, &admin, 60).await;
    let status = terminal.body["status"].as_str().unwrap();
    assert_eq!(
        status, "Judged",
        "code run should judge cleanly: {}",
        terminal.text
    );

    let result = &terminal.body["result"];
    assert!(!result.is_null(), "Judged code run should have a result");

    let tcrs = result["test_case_results"].as_array().unwrap();
    assert_eq!(
        tcrs.len(),
        3,
        "Should have 3 test case results for 3 custom TCs, got {}",
        tcrs.len()
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges real programs in isolate; the sandbox CI job runs it (see common::require_real_sandbox)"]
async fn contest_code_run_reaches_terminal_state() {
    require_real_sandbox();

    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("cr_admin3", "pass1234", "admin")
        .await;
    let user = app.create_authenticated_user("cr_user3", "pass1234").await;

    let problem_id = app.create_problem(&admin, "Contest Code Run").await;

    let contest_id = app.create_contest(&admin, "CR Contest", true, true).await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    app.register_for_contest(contest_id, &user).await;

    let res = app
        .post_with_token(
            &format!("/api/v1/contests/{contest_id}/problems/{problem_id}/code-runs"),
            &json!({
                "files": [{"filename": "main.cpp", "content": CPP_ECHO}],
                "language": "cpp",
                "custom_test_cases": [
                    {"input": "test", "expected_output": "test"}
                ],
            }),
            &user,
        )
        .await;
    assert_eq!(
        res.status, 201,
        "create contest code run failed: {}",
        res.text
    );
    let cr_id = res.id();

    let terminal = app.wait_for_code_run_terminal(cr_id, &user, 60).await;
    let status = terminal.body["status"].as_str().unwrap();
    assert_eq!(
        status, "Judged",
        "contest code run should judge cleanly: {}",
        terminal.text
    );

    assert_eq!(
        terminal.body["contest_id"].as_i64().unwrap(),
        contest_id as i64,
        "Code run should have contest_id set"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges real programs in isolate; the sandbox CI job runs it (see common::require_real_sandbox)"]
async fn code_run_does_not_appear_in_submissions() {
    require_real_sandbox();

    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("cr_admin4", "pass1234", "admin")
        .await;

    let problem_id = app.create_problem(&admin, "CR Not Sub").await;

    let cr_res = app
        .post_with_token(
            &format!("/api/v1/problems/{problem_id}/code-runs"),
            &json!({
                "files": [{"filename": "main.cpp", "content": CPP_ECHO}],
                "language": "cpp",
                "custom_test_cases": [{"input": "x", "expected_output": "x"}],
            }),
            &admin,
        )
        .await;
    assert_eq!(cr_res.status, 201);
    let cr_id = cr_res.id();

    let list = app
        .get_with_token(
            &format!("/api/v1/submissions?problem_id={problem_id}"),
            &admin,
        )
        .await;
    assert_eq!(list.status, 200);

    let data = list.body["data"].as_array().unwrap();
    assert!(
        !data
            .iter()
            .any(|s| s["id"].as_i64().unwrap() == cr_id as i64),
        "Code run {cr_id} should not appear in submissions list"
    );

    let sub_res = app
        .get_with_token(&format!("/api/v1/submissions/{cr_id}"), &admin)
        .await;
    assert_eq!(
        sub_res.status, 404,
        "code run id must not resolve through submission detail: {}",
        sub_res.text
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn code_run_max_test_cases_validation() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("cr_admin5", "pass1234", "admin")
        .await;

    let problem_id = app.create_problem(&admin, "CR Max TC").await;

    let tcs: Vec<serde_json::Value> = (0..11)
        .map(|i| json!({"input": format!("input_{i}"), "expected_output": format!("output_{i}")}))
        .collect();

    let res = app
        .post_with_token(
            &format!("/api/v1/problems/{problem_id}/code-runs"),
            &json!({
                "files": [{"filename": "main.cpp", "content": CPP_ECHO}],
                "language": "cpp",
                "custom_test_cases": tcs,
            }),
            &admin,
        )
        .await;
    assert_eq!(
        res.status, 400,
        "More than 10 custom test cases should return 400, got: {} — {}",
        res.status, res.text
    );
    assert_eq!(
        res.body["code"].as_str().unwrap(),
        "VALIDATION_ERROR",
        "Error code should be VALIDATION_ERROR"
    );
}

const CPP_ECHO: &str = r#"
#include <iostream>
#include <string>
int main() {
    std::string line;
    while (std::getline(std::cin, line)) {
        std::cout << line << "\n";
    }
    return 0;
}
"#;
