use crate::common::{TestApp, routes};
use serde_json::json;

fn valid_code_run_body() -> serde_json::Value {
    json!({
        "files": [{"filename": "main.cpp", "content": "#include <iostream>\nint main() {}"}],
        "language": "cpp",
        "custom_test_cases": [
            {"input": "5\n1 2 3 4 5", "expected_output": "15"}
        ]
    })
}

fn multi_tc_code_run_body() -> serde_json::Value {
    json!({
        "files": [{"filename": "main.cpp", "content": "#include <iostream>\nint main() {}"}],
        "language": "cpp",
        "custom_test_cases": [
            {"input": "3\n1 2 3", "expected_output": "6"},
            {"input": "1\n42"},
            {"input": "0", "expected_output": "0"}
        ]
    })
}

mod code_run_creation {
    use super::*;

    #[tokio::test]
    async fn user_can_run_code_against_custom_test_cases() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;

        let body = valid_code_run_body();
        let res = app
            .post_with_token(&routes::problem_code_runs(problem_id), &body, &user_token)
            .await;

        assert_eq!(res.status, 201);
        assert!(res.body["id"].as_i64().is_some());
        assert_eq!(res.body["language"], "cpp");
        // Fresh code-runs land in `Queued`; the claim fiber
        // promotes to `Pending` asynchronously.
        assert_eq!(res.body["status"], "Queued");
        assert_eq!(res.body["problem_id"], problem_id);
        assert!(res.body["contest_id"].is_null());

        let tcs = res.body["custom_test_cases"].as_array().unwrap();
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0]["input"], "5\n1 2 3 4 5");
        assert_eq!(tcs[0]["expected_output"], "15");
    }

    #[tokio::test]
    async fn code_run_with_multiple_custom_test_cases() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;

        let body = multi_tc_code_run_body();
        let res = app
            .post_with_token(&routes::problem_code_runs(problem_id), &body, &user_token)
            .await;

        assert_eq!(res.status, 201);
        let tcs = res.body["custom_test_cases"].as_array().unwrap();
        assert_eq!(tcs.len(), 3);
        assert!(tcs[1]["expected_output"].is_null());
    }

    #[tokio::test]
    async fn code_run_requires_authentication() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let body = valid_code_run_body();
        let res = app
            .post_without_token(&routes::problem_code_runs(problem_id), &body)
            .await;

        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "TOKEN_MISSING");
    }

    #[tokio::test]
    async fn code_run_returns_404_for_nonexistent_problem() {
        let app = TestApp::spawn().await;
        let user_token = app.create_authenticated_user("user1", "pass1234").await;

        let body = valid_code_run_body();
        let res = app
            .post_with_token(&routes::problem_code_runs(99999), &body, &user_token)
            .await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }
}

mod code_run_validation {
    use super::*;

    #[tokio::test]
    async fn rejects_empty_custom_test_cases() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;

        let body = json!({
            "files": [{"filename": "main.cpp", "content": "int main() {}"}],
            "language": "cpp",
            "custom_test_cases": []
        });
        let res = app
            .post_with_token(&routes::problem_code_runs(problem_id), &body, &user_token)
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn rejects_more_than_10_custom_test_cases() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;

        let tcs: Vec<_> = (0..11)
            .map(|i| json!({"input": format!("test {i}"), "expected_output": "out"}))
            .collect();
        let body = json!({
            "files": [{"filename": "main.cpp", "content": "int main() {}"}],
            "language": "cpp",
            "custom_test_cases": tcs
        });
        let res = app
            .post_with_token(&routes::problem_code_runs(problem_id), &body, &user_token)
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn rejects_empty_files_array() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;

        let body = json!({
            "files": [],
            "language": "cpp",
            "custom_test_cases": [{"input": "1"}]
        });
        let res = app
            .post_with_token(&routes::problem_code_runs(problem_id), &body, &user_token)
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn rejects_missing_language() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;

        let body = json!({
            "files": [{"filename": "main.cpp", "content": "int main() {}"}],
            "language": "",
            "custom_test_cases": [{"input": "1"}]
        });
        let res = app
            .post_with_token(&routes::problem_code_runs(problem_id), &body, &user_token)
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }
}

mod code_run_retrieval {
    use super::*;

    #[tokio::test]
    async fn owner_can_get_their_code_run() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;

        let body = valid_code_run_body();
        let create_res = app
            .post_with_token(&routes::problem_code_runs(problem_id), &body, &user_token)
            .await;
        assert_eq!(create_res.status, 201);
        let code_run_id = create_res.id();

        let get_res = app
            .get_with_token(&routes::code_run(code_run_id), &user_token)
            .await;

        assert_eq!(get_res.status, 200);
        assert_eq!(get_res.body["id"], code_run_id);
        assert_eq!(get_res.body["language"], "cpp");
        assert_eq!(get_res.body["problem_id"], problem_id);
    }

    #[tokio::test]
    async fn other_user_cannot_see_code_run() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;

        let body = valid_code_run_body();
        let create_res = app
            .post_with_token(&routes::problem_code_runs(problem_id), &body, &user_token)
            .await;
        let code_run_id = create_res.id();

        let other_token = app.create_authenticated_user("user2", "pass1234").await;
        let get_res = app
            .get_with_token(&routes::code_run(code_run_id), &other_token)
            .await;

        assert_eq!(get_res.status, 404);
        assert_eq!(get_res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn admin_can_see_any_code_run() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;

        let body = valid_code_run_body();
        let create_res = app
            .post_with_token(&routes::problem_code_runs(problem_id), &body, &user_token)
            .await;
        let code_run_id = create_res.id();

        let get_res = app
            .get_with_token(&routes::code_run(code_run_id), &admin_token)
            .await;

        assert_eq!(get_res.status, 200);
        assert_eq!(get_res.body["id"], code_run_id);
    }

    #[tokio::test]
    async fn returns_404_for_nonexistent_code_run() {
        let app = TestApp::spawn().await;
        let user_token = app.create_authenticated_user("user1", "pass1234").await;

        let res = app
            .get_with_token(&routes::code_run(99999), &user_token)
            .await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn get_code_run_requires_authentication() {
        let app = TestApp::spawn().await;

        let res = app.get_without_token(&routes::code_run(1)).await;

        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "TOKEN_MISSING");
    }
}

mod code_run_isolation {
    use super::*;

    #[tokio::test]
    async fn code_runs_do_not_appear_in_submission_list() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;

        let run_body = valid_code_run_body();
        let run_res = app
            .post_with_token(
                &routes::problem_code_runs(problem_id),
                &run_body,
                &user_token,
            )
            .await;
        assert_eq!(run_res.status, 201);

        let sub_body = json!({
            "files": [{"filename": "main.cpp", "content": "#include <iostream>\nint main() {}"}],
            "language": "cpp",
        });
        let sub_res = app
            .post_with_token(
                &routes::problem_submissions(problem_id),
                &sub_body,
                &user_token,
            )
            .await;
        assert_eq!(sub_res.status, 201);

        let list_res = app.get_with_token(routes::SUBMISSIONS, &user_token).await;

        assert_eq!(list_res.status, 200);
        let data = list_res.body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["id"], sub_res.body["id"]);
    }

    #[tokio::test]
    async fn code_run_response_has_no_mode_field() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;

        let body = valid_code_run_body();
        let res = app
            .post_with_token(&routes::problem_code_runs(problem_id), &body, &user_token)
            .await;

        assert_eq!(res.status, 201);
        assert!(res.body.get("mode").is_none());
        assert!(res.body["custom_test_cases"].is_array());
    }

    #[tokio::test]
    async fn submission_response_has_no_mode_or_custom_test_cases() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;

        let body = json!({
            "files": [{"filename": "main.cpp", "content": "#include <iostream>\nint main() {}"}],
            "language": "cpp",
        });
        let res = app
            .post_with_token(&routes::problem_submissions(problem_id), &body, &user_token)
            .await;

        assert_eq!(res.status, 201);
        assert!(res.body.get("mode").is_none());
        assert!(res.body.get("custom_test_cases").is_none());
    }
}

mod contest_code_runs {
    use super::*;

    #[tokio::test]
    async fn participant_can_run_code_in_contest() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Contest Problem").await;
        let contest_id = app
            .create_contest(&admin_token, "Test Contest", true, false)
            .await;
        app.add_problem_to_contest(contest_id, problem_id, &admin_token)
            .await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;
        app.register_for_contest(contest_id, &user_token).await;

        let body = valid_code_run_body();
        let res = app
            .post_with_token(
                &routes::contest_problem_code_runs(contest_id, problem_id),
                &body,
                &user_token,
            )
            .await;

        assert_eq!(res.status, 201);
        assert_eq!(res.body["contest_id"], contest_id);
        assert_eq!(res.body["problem_id"], problem_id);
        assert!(res.body["custom_test_cases"].is_array());
    }

    #[tokio::test]
    async fn non_participant_cannot_run_code_in_contest() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Contest Problem").await;
        let contest_id = app
            .create_contest(&admin_token, "Test Contest", true, false)
            .await;
        app.add_problem_to_contest(contest_id, problem_id, &admin_token)
            .await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;

        let body = valid_code_run_body();
        let res = app
            .post_with_token(
                &routes::contest_problem_code_runs(contest_id, problem_id),
                &body,
                &user_token,
            )
            .await;

        assert_eq!(res.status, 403);
    }

    #[tokio::test]
    async fn contest_code_run_returns_404_for_problem_not_in_contest() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Not In Contest").await;
        let contest_id = app
            .create_contest(&admin_token, "Test Contest", true, false)
            .await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;
        app.register_for_contest(contest_id, &user_token).await;

        let body = valid_code_run_body();
        let res = app
            .post_with_token(
                &routes::contest_problem_code_runs(contest_id, problem_id),
                &body,
                &user_token,
            )
            .await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn contest_code_runs_do_not_appear_in_contest_submission_list() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Contest Problem").await;
        let contest_id = app
            .create_contest(&admin_token, "Test Contest", true, true)
            .await;
        app.add_problem_to_contest(contest_id, problem_id, &admin_token)
            .await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;
        app.register_for_contest(contest_id, &user_token).await;

        let run_body = valid_code_run_body();
        let run_res = app
            .post_with_token(
                &routes::contest_problem_code_runs(contest_id, problem_id),
                &run_body,
                &user_token,
            )
            .await;
        assert_eq!(run_res.status, 201);

        let sub_body = json!({
            "files": [{"filename": "main.cpp", "content": "#include <iostream>\nint main() {}"}],
            "language": "cpp",
        });
        let sub_res = app
            .post_with_token(
                &routes::contest_problem_submissions(contest_id, problem_id),
                &sub_body,
                &user_token,
            )
            .await;
        assert_eq!(sub_res.status, 201);

        let list_res = app
            .get_with_token(&routes::contest_submissions(contest_id), &admin_token)
            .await;

        assert_eq!(list_res.status, 200);
        let data = list_res.body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["id"], sub_res.body["id"]);
    }
}
