use crate::common::{TestApp, routes};
use broccoli_server_sdk::permissions as perm;
use serde_json::json;

fn valid_submission_body(language: &str) -> serde_json::Value {
    json!({
        "files": [{"filename": "main.cpp", "content": "#include <iostream>\nint main() {}"}],
        "language": language,
    })
}

fn multi_file_submission_body() -> serde_json::Value {
    json!({
        "files": [
            {"filename": "Main.java", "content": "public class Main {}"},
            {"filename": "Helper.java", "content": "public class Helper {}"},
        ],
        "language": "java",
    })
}

async fn poll_submission_until_status(
    app: &TestApp,
    submission_id: i32,
    expected: common::SubmissionStatus,
    budget: std::time::Duration,
) -> server::entity::submission::Model {
    use sea_orm::EntityTrait;

    let deadline = tokio::time::Instant::now() + budget;
    loop {
        let sub = server::entity::submission::Entity::find_by_id(submission_id)
            .one(&app.db)
            .await
            .expect("load submission")
            .expect("submission should exist");
        if sub.status == expected || tokio::time::Instant::now() >= deadline {
            return sub;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

mod submission_creation {
    use super::*;

    #[tokio::test]
    async fn user_can_create_a_submission() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;
        let body = valid_submission_body("cpp");
        let res = app
            .post_with_token(&routes::problem_submissions(problem_id), &body, &user_token)
            .await;

        assert_eq!(res.status, 201);
        assert_eq!(res.body["language"], "cpp");
        // Fresh submissions land in `Queued`; the per-server
        // claim fiber promotes them to `Pending` asynchronously, but
        // the POST response reflects the just-committed row.
        assert_eq!(res.body["status"], "Queued");
        assert!(res.body["id"].as_i64().is_some());
    }

    #[tokio::test]
    async fn submission_includes_user_info() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;
        let body = valid_submission_body("cpp");
        let res = app
            .post_with_token(&routes::problem_submissions(problem_id), &body, &user_token)
            .await;

        assert_eq!(res.status, 201);
        assert_eq!(res.body["username"], "user1");
        assert_eq!(res.body["problem_title"], "Test Problem");
    }

    #[tokio::test]
    async fn requires_authentication() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let body = valid_submission_body("cpp");
        let res = app
            .post_without_token(&routes::problem_submissions(problem_id), &body)
            .await;

        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "TOKEN_MISSING");
    }

    #[tokio::test]
    async fn returns_404_for_nonexistent_problem() {
        let app = TestApp::spawn().await;
        let user_token = app.create_authenticated_user("user1", "pass1234").await;

        let body = valid_submission_body("cpp");
        let res = app
            .post_with_token(&routes::problem_submissions(99999), &body, &user_token)
            .await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn supports_multi_file_submissions() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Java Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;
        let body = multi_file_submission_body();
        let res = app
            .post_with_token(&routes::problem_submissions(problem_id), &body, &user_token)
            .await;

        assert_eq!(res.status, 201);
        let files = res.body["files"].as_array().expect("files should be array");
        assert_eq!(files.len(), 2);
        assert_eq!(files[0]["filename"], "Main.java");
        assert_eq!(files[1]["filename"], "Helper.java");
    }
}

mod submission_validation {
    use super::*;

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
        });
        let res = app
            .post_with_token(&routes::problem_submissions(problem_id), &body, &user_token)
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn rejects_empty_filename() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;
        let body = json!({
            "files": [{"filename": "   ", "content": "code"}],
            "language": "cpp",
        });
        let res = app
            .post_with_token(&routes::problem_submissions(problem_id), &body, &user_token)
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn rejects_empty_content() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;
        let body = json!({
            "files": [{"filename": "main.cpp", "content": ""}],
            "language": "cpp",
        });
        let res = app
            .post_with_token(&routes::problem_submissions(problem_id), &body, &user_token)
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn rejects_path_traversal_in_filename() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;
        let body = json!({
            "files": [{"filename": "../../../etc/passwd", "content": "malicious"}],
            "language": "cpp",
        });
        let res = app
            .post_with_token(&routes::problem_submissions(problem_id), &body, &user_token)
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn rejects_forward_slash_in_filename() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;
        let body = json!({
            "files": [{"filename": "src/main.cpp", "content": "code"}],
            "language": "cpp",
        });
        let res = app
            .post_with_token(&routes::problem_submissions(problem_id), &body, &user_token)
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn rejects_backslash_in_filename() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;
        let body = json!({
            "files": [{"filename": "src\\main.cpp", "content": "code"}],
            "language": "cpp",
        });
        let res = app
            .post_with_token(&routes::problem_submissions(problem_id), &body, &user_token)
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn rejects_duplicate_filenames() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;
        let body = json!({
            "files": [
                {"filename": "main.cpp", "content": "first"},
                {"filename": "main.cpp", "content": "duplicate"},
            ],
            "language": "cpp",
        });
        let res = app
            .post_with_token(&routes::problem_submissions(problem_id), &body, &user_token)
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
        assert!(
            res.body["message"]
                .as_str()
                .unwrap()
                .contains("Duplicate filename")
        );
    }

    #[tokio::test]
    async fn rejects_empty_language() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;
        let body = json!({
            "files": [{"filename": "main.cpp", "content": "code"}],
            "language": "   ",
        });
        let res = app
            .post_with_token(&routes::problem_submissions(problem_id), &body, &user_token)
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn rejects_hidden_files() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;
        let body = json!({
            "files": [{"filename": ".hidden", "content": "code"}],
            "language": "cpp",
        });
        let res = app
            .post_with_token(&routes::problem_submissions(problem_id), &body, &user_token)
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
        assert!(res.body["message"].as_str().unwrap().contains("hidden"));
    }
}

mod rate_limiting {
    use super::*;

    #[tokio::test]
    async fn returns_429_when_rate_limit_exceeded() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;
        let body = valid_submission_body("cpp");

        for i in 0..10 {
            let res = app
                .post_with_token(&routes::problem_submissions(problem_id), &body, &user_token)
                .await;
            assert_eq!(res.status, 201, "Submission {} failed", i + 1);
        }

        let res = app
            .post_with_token(&routes::problem_submissions(problem_id), &body, &user_token)
            .await;

        assert_eq!(res.status, 429);
        assert_eq!(res.body["code"], "RATE_LIMITED");
    }

    #[tokio::test]
    async fn rate_limit_is_per_user() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user1_token = app.create_authenticated_user("user1", "pass1234").await;
        let user2_token = app.create_authenticated_user("user2", "pass1234").await;
        let body = valid_submission_body("cpp");

        for _ in 0..10 {
            app.post_with_token(
                &routes::problem_submissions(problem_id),
                &body,
                &user1_token,
            )
            .await;
        }

        let res = app
            .post_with_token(
                &routes::problem_submissions(problem_id),
                &body,
                &user2_token,
            )
            .await;

        assert_eq!(res.status, 201);
    }
}

mod submission_listing {
    use super::*;

    #[tokio::test]
    async fn user_sees_own_submissions() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;
        app.create_submission(problem_id, &user_token, "cpp", "int main() {}")
            .await;

        let res = app.get_with_token(routes::SUBMISSIONS, &user_token).await;

        assert_eq!(res.status, 200, "unexpected body: {}", res.body);
        let data = res.body["data"].as_array().expect("data should be array");
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["username"], "user1");
    }

    #[tokio::test]
    async fn user_does_not_see_other_users_submissions() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user1_token = app.create_authenticated_user("user1", "pass1234").await;
        let user2_token = app.create_authenticated_user("user2", "pass1234").await;

        app.create_submission(problem_id, &user1_token, "cpp", "int main() {}")
            .await;

        let res = app.get_with_token(routes::SUBMISSIONS, &user2_token).await;

        assert_eq!(res.status, 200, "unexpected body: {}", res.body);
        let data = res.body["data"].as_array().expect("data should be array");
        assert_eq!(data.len(), 0);
    }

    #[tokio::test]
    async fn admin_sees_all_submissions() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user1_token = app.create_authenticated_user("user1", "pass1234").await;
        let user2_token = app.create_authenticated_user("user2", "pass1234").await;

        app.create_submission(problem_id, &user1_token, "cpp", "int main() {}")
            .await;
        app.create_submission(problem_id, &user2_token, "java", "class Main {}")
            .await;

        let res = app.get_with_token(routes::SUBMISSIONS, &admin_token).await;

        assert_eq!(res.status, 200);
        let data = res.body["data"].as_array().expect("data should be array");
        assert_eq!(data.len(), 2);
    }

    #[tokio::test]
    async fn can_filter_by_problem_id() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem1_id = app.create_problem(&admin_token, "Problem 1").await;
        let problem2_id = app.create_problem(&admin_token, "Problem 2").await;

        app.create_submission(problem1_id, &admin_token, "cpp", "int main() {}")
            .await;
        app.create_submission(problem2_id, &admin_token, "cpp", "int main() {}")
            .await;

        let url = format!("{}?problem_id={}", routes::SUBMISSIONS, problem1_id);
        let res = app.get_with_token(&url, &admin_token).await;

        assert_eq!(res.status, 200);
        let data = res.body["data"].as_array().expect("data should be array");
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["problem_id"], problem1_id);
    }

    #[tokio::test]
    async fn can_filter_by_language() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        app.create_submission(problem_id, &admin_token, "cpp", "int main() {}")
            .await;
        app.create_submission(problem_id, &admin_token, "python3", "print('hi')")
            .await;

        let url = format!("{}?language=cpp", routes::SUBMISSIONS);
        let res = app.get_with_token(&url, &admin_token).await;

        assert_eq!(res.status, 200);
        let data = res.body["data"].as_array().expect("data should be array");
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["language"], "cpp");
    }

    #[tokio::test]
    async fn rejects_unsupported_language() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;
        let user_token = app.create_authenticated_user("user1", "pass1234").await;

        let body = json!({
            "files": [{"filename": "main.rb", "content": "puts 'hi'"}],
            "language": "ruby",
        });
        let res = app
            .post_with_token(&routes::problem_submissions(problem_id), &body, &user_token)
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
        assert_eq!(res.body["message"], "Unsupported language: ruby");
    }

    #[tokio::test]
    async fn rejects_submission_format_filename_mismatch() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_res = app
            .post_with_token(
                routes::PROBLEMS,
                &json!({
                    "title": "Multi-file Problem",
                    "content": "## Description\nSolve this.",
                    "time_limit": 1000,
                    "memory_limit": 262144,
                    "problem_type": "standard",
                    "default_contest_type": "standard",
                    "checker_format": "exact",
                    "submission_format": {
                        "cpp": ["main.cpp", "grader.cpp"]
                    },
                    // Public so the contestant reaches format validation rather
                    // than the read-access gate (which 404s hidden problems).
                    "is_public": true,
                }),
                &admin_token,
            )
            .await;
        assert_eq!(
            problem_res.status, 201,
            "create_problem failed: {}",
            problem_res.text
        );
        let problem_id = problem_res.id();

        let user_token = app.create_authenticated_user("user1", "pass1234").await;
        let body = json!({
            "files": [
                {"filename": "main.cpp", "content": "int main() {}"},
            ],
            "language": "cpp",
        });
        let res = app
            .post_with_token(&routes::problem_submissions(problem_id), &body, &user_token)
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
        assert_eq!(
            res.body["message"],
            "Files for language 'cpp' must exactly match: grader.cpp, main.cpp",
        );
    }

    #[tokio::test]
    async fn returns_pagination_info() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        for _ in 0..5 {
            app.create_submission(problem_id, &admin_token, "cpp", "int main() {}")
                .await;
        }

        let url = format!("{}?per_page=2", routes::SUBMISSIONS);
        let res = app.get_with_token(&url, &admin_token).await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["pagination"]["per_page"], 2);
        assert_eq!(res.body["pagination"]["total"], 5);
        assert_eq!(res.body["pagination"]["total_pages"], 3);
    }
}

mod submission_detail {
    use super::*;

    #[tokio::test]
    async fn owner_can_view_submission_detail() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;
        let submission_id = app
            .create_submission(problem_id, &user_token, "cpp", "int main() {}")
            .await;

        let res = app
            .get_with_token(&routes::submission(submission_id), &user_token)
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["id"], submission_id);
        assert!(res.body["files"].as_array().is_some());
    }

    #[tokio::test]
    async fn non_owner_cannot_view_submission() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user1_token = app.create_authenticated_user("user1", "pass1234").await;
        let user2_token = app.create_authenticated_user("user2", "pass1234").await;
        let submission_id = app
            .create_submission(problem_id, &user1_token, "cpp", "int main() {}")
            .await;

        let res = app
            .get_with_token(&routes::submission(submission_id), &user2_token)
            .await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn admin_can_view_any_submission() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;
        let submission_id = app
            .create_submission(problem_id, &user_token, "cpp", "int main() {}")
            .await;

        let res = app
            .get_with_token(&routes::submission(submission_id), &admin_token)
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["id"], submission_id);
    }

    #[tokio::test]
    async fn returns_404_for_nonexistent_submission() {
        let app = TestApp::spawn().await;
        let user_token = app.create_authenticated_user("user1", "pass1234").await;

        let res = app
            .get_with_token(&routes::submission(99999), &user_token)
            .await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }
}

mod rejudge {
    use super::*;
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
    use server::entity::{role_permission, submission};

    #[tokio::test]
    async fn admin_can_rejudge_submission() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;
        let submission_id = app
            .create_submission(problem_id, &admin_token, "cpp", "int main() {}")
            .await;

        let res = app
            .post_with_token(
                &routes::submission_rejudge(submission_id),
                &json!({}),
                &admin_token,
            )
            .await;

        assert_eq!(res.status, 200);
        // Rejudge (apply_immediately defaults to true) writes the
        // submission back to `Queued`. The claim fiber owns the
        // promotion to `Pending`. We assert on the POST body which
        // reflects the just-committed row, not the eventual state.
        let status = res.body["status"].as_str().expect("status string");
        assert!(
            status == "Queued" || status == "Pending",
            "expected Queued or Pending, got {status}"
        );
    }

    #[tokio::test]
    async fn contestant_cannot_rejudge() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;
        let submission_id = app
            .create_submission(problem_id, &user_token, "cpp", "int main() {}")
            .await;

        let res = app
            .post_with_token(
                &routes::submission_rejudge(submission_id),
                &json!({}),
                &user_token,
            )
            .await;

        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn rejudge_requires_system_admin_to_clear_worker_pin() {
        let app = TestApp::spawn().await;
        role_permission::ActiveModel {
            role: Set("problem_setter".to_string()),
            permission: Set(perm::SUBMISSION_REJUDGE.to_string()),
        }
        .insert(&app.db)
        .await
        .expect("grant rejudge permission");

        let admin_token = app
            .create_user_with_role("admin_clear_pin_single", "pass1234", "admin")
            .await;
        let rejudge_token = app
            .create_user_with_role("setter_clear_pin_single", "pass1234", "problem_setter")
            .await;
        let problem_id = app
            .create_problem(&admin_token, "Pinned Rejudge Problem")
            .await;
        let submission_id = app
            .create_submission(problem_id, &admin_token, "cpp", "int main() {}")
            .await;

        submission::Entity::update_many()
            .col_expr(
                submission::Column::TargetWorkerId,
                sea_orm::sea_query::Expr::value(Some("worker-a".to_string())),
            )
            .filter(submission::Column::Id.eq(submission_id))
            .exec(&app.db)
            .await
            .expect("pin submission");

        let res = app
            .post_with_token(
                &routes::submission_rejudge(submission_id),
                &json!({"target_worker_id": ""}),
                &rejudge_token,
            )
            .await;

        assert_eq!(res.status, 403, "unexpected body: {}", res.body);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");

        let submission = submission::Entity::find_by_id(submission_id)
            .one(&app.db)
            .await
            .expect("load submission")
            .expect("submission exists");
        assert_eq!(submission.target_worker_id.as_deref(), Some("worker-a"));
    }
}

mod judgement_history {
    use super::*;
    use chrono::Utc;
    use common::{SubmissionStatus, Verdict};
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
    use server::entity::{submission, submission_judgement, test_case, test_case_result, user};

    async fn seed_history(app: &TestApp, username: &str, problem_id: i32) -> (i32, i32, i32) {
        let now = Utc::now();
        let user_model = user::Entity::find()
            .filter(user::Column::Username.eq(username))
            .one(&app.db)
            .await
            .expect("query user")
            .expect("user should exist");

        let submission = submission::ActiveModel {
            files: Set(json!([{ "filename": "main.cpp", "content": "int main() {}" }])),
            language: Set("cpp".into()),
            user_id: Set(user_model.id),
            problem_id: Set(problem_id),
            contest_id: Set(None),
            contest_type: Set("standard".into()),
            status: Set(SubmissionStatus::Judged),
            verdict: Set(Some(Verdict::Accepted)),
            score: Set(Some(100.0)),
            time_used: Set(Some(7)),
            memory_used: Set(Some(128)),
            compile_output: Set(Some("ok".to_string())),
            judge_epoch: Set(11),
            max_judge_epoch: Set(11),
            created_at: Set(now),
            judged_at: Set(Some(now)),
            ..Default::default()
        }
        .insert(&app.db)
        .await
        .expect("insert submission");
        let submission_id = submission.id;

        let old = submission_judgement::ActiveModel {
            submission_id: Set(submission_id),
            version: Set(10),
            is_current: Set(false),
            is_finalized: Set(true),
            triggered_by_user_id: Set(None),
            status: Set(SubmissionStatus::Judged),
            verdict: Set(Some(Verdict::WrongAnswer)),
            score: Set(Some(40.0)),
            judge_epoch: Set(10),
            created_at: Set(now),
            finalized_at: Set(Some(now)),
            ..Default::default()
        }
        .insert(&app.db)
        .await
        .expect("insert old judgement");

        let current = submission_judgement::ActiveModel {
            submission_id: Set(submission_id),
            version: Set(11),
            is_current: Set(true),
            is_finalized: Set(true),
            triggered_by_user_id: Set(None),
            status: Set(SubmissionStatus::Judged),
            verdict: Set(Some(Verdict::Accepted)),
            score: Set(Some(100.0)),
            time_used: Set(Some(7)),
            memory_used: Set(Some(128)),
            compile_output: Set(Some("ok".to_string())),
            judge_epoch: Set(11),
            created_at: Set(now),
            finalized_at: Set(Some(now)),
            ..Default::default()
        }
        .insert(&app.db)
        .await
        .expect("insert current judgement");

        let test_case = test_case::ActiveModel {
            problem_id: Set(problem_id),
            input: Set("1 2\n".to_string()),
            expected_output: Set("3\n".to_string()),
            score: Set(100),
            label: Set("sum".to_string()),
            is_sample: Set(false),
            position: Set(1),
            created_at: Set(now),
            ..Default::default()
        }
        .insert(&app.db)
        .await
        .expect("insert test case");

        for (judgement, verdict, score) in [
            (&old, Verdict::WrongAnswer, 40.0),
            (&current, Verdict::Accepted, 100.0),
        ] {
            test_case_result::ActiveModel {
                submission_id: Set(submission_id),
                judgement_id: Set(Some(judgement.id)),
                judge_epoch: Set(judgement.judge_epoch),
                test_case_id: Set(Some(test_case.id)),
                run_index: Set(None),
                verdict: Set(verdict),
                score: Set(score),
                created_at: Set(now),
                ..Default::default()
            }
            .insert(&app.db)
            .await
            .expect("insert test case result");
        }

        (submission_id, old.id, current.id)
    }

    #[tokio::test]
    async fn get_submission_only_returns_current_judgement_results() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_jhist1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "History Problem").await;
        let (submission_id, _old_judgement_id, current_judgement_id) =
            seed_history(&app, "admin_jhist1", problem_id).await;

        let res = app
            .get_with_token(&routes::submission(submission_id), &admin_token)
            .await;

        assert_eq!(res.status, 200, "unexpected body: {}", res.body);
        let results = res.body["result"]["test_case_results"]
            .as_array()
            .expect("test_case_results should be an array");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["score"], 100.0);

        let db_rows = test_case_result::Entity::find()
            .filter(test_case_result::Column::JudgementId.eq(Some(current_judgement_id)))
            .all(&app.db)
            .await
            .expect("query current rows");
        assert_eq!(db_rows.len(), 1);
    }

    #[tokio::test]
    async fn list_judgements_returns_summaries_and_loads_results_on_demand() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_jhist2", "pass1234", "admin")
            .await;
        let problem_id = app
            .create_problem(&admin_token, "History List Problem")
            .await;
        let (submission_id, _, _) = seed_history(&app, "admin_jhist2", problem_id).await;

        // The current judgement has a testcase that did not exist in the old
        // judgement. The history summary must count this current-only row as
        // a change as well as the modified shared testcase.
        let current_judgement_id = submission_judgement::Entity::find()
            .filter(submission_judgement::Column::SubmissionId.eq(submission_id))
            .filter(submission_judgement::Column::IsCurrent.eq(true))
            .one(&app.db)
            .await
            .expect("query current judgement")
            .expect("current judgement should exist")
            .id;
        let added_case = test_case::ActiveModel {
            problem_id: Set(problem_id),
            input: Set("3 4\n".to_string()),
            expected_output: Set("7\n".to_string()),
            score: Set(100),
            label: Set("added".to_string()),
            is_sample: Set(false),
            position: Set(2),
            created_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(&app.db)
        .await
        .expect("insert added test case");
        test_case_result::ActiveModel {
            submission_id: Set(submission_id),
            judgement_id: Set(Some(current_judgement_id)),
            // The current judgement's epoch (see `seed_history`).
            judge_epoch: Set(11),
            test_case_id: Set(Some(added_case.id)),
            run_index: Set(None),
            verdict: Set(Verdict::Accepted),
            score: Set(100.0),
            created_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(&app.db)
        .await
        .expect("insert added test case result");

        let res = app
            .get_with_token(&routes::submission_judgements(submission_id), &admin_token)
            .await;

        assert_eq!(res.status, 200, "unexpected body: {}", res.body);
        let versions = res.body.as_array().expect("response should be an array");
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0]["version"], 10);
        assert_eq!(versions[0]["test_case_results"], json!([]));
        assert_eq!(versions[0]["test_case_pagination"]["total"], 1);
        assert_eq!(versions[0]["case_changes"], 2);
        assert_eq!(versions[1]["version"], 11);
        assert_eq!(versions[1]["is_current"], true);
        let version_id = versions[0]["id"].as_i64().unwrap();
        let detail = app
            .get_with_token(
                &format!(
                    "{}/{version_id}",
                    routes::submission_judgements(submission_id)
                ),
                &admin_token,
            )
            .await;
        assert_eq!(detail.status, 200, "{}", detail.body);
        assert_eq!(detail.body["test_case_results"][0]["score"], 40.0);
        assert_eq!(detail.body["test_case_results"][0]["input"], "1 2\n");
        assert_eq!(
            detail.body["test_case_results"][0]["expected_output"],
            "3\n"
        );
    }

    #[tokio::test]
    async fn testcase_pages_and_expanded_outputs_are_bounded_and_authorized() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("page_admin", "pass1234", "admin")
            .await;
        let owner = app
            .create_authenticated_user("page_owner", "pass1234")
            .await;
        let stranger = app
            .create_authenticated_user("page_stranger", "pass1234")
            .await;
        let problem_id = app.create_problem(&admin, "Paged results").await;
        let (submission_id, old_id, current_id) =
            seed_history(&app, "page_owner", problem_id).await;
        let text = "x\n".repeat(10_000);
        let mut result_id = 0;
        let mut testcase_id = 0;
        for position in 2..=42 {
            let tc = test_case::ActiveModel {
                problem_id: Set(problem_id),
                input: Set(text.clone()),
                expected_output: Set(text.clone()),
                score: Set(1),
                label: Set(format!("case-{position}")),
                is_sample: Set(false),
                position: Set(position),
                created_at: Set(Utc::now()),
                ..Default::default()
            }
            .insert(&app.db)
            .await
            .unwrap();
            let result = test_case_result::ActiveModel {
                submission_id: Set(submission_id),
                judgement_id: Set(Some(current_id)),
                // The current judgement's epoch (see `seed_history`).
                judge_epoch: Set(11),
                test_case_id: Set(Some(tc.id)),
                verdict: Set(Verdict::Accepted),
                score: Set(1.0),
                stdout: Set(Some(text.clone())),
                stderr: Set(Some("界".repeat(100_000))),
                checker_output: Set(Some(text.clone())),
                created_at: Set(Utc::now()),
                ..Default::default()
            }
            .insert(&app.db)
            .await
            .unwrap();
            result_id = result.id;
            testcase_id = tc.id;
        }
        let url = routes::submission(submission_id);
        let first = app
            .get_with_token(&format!("{url}?per_page=99999"), &admin)
            .await;
        assert_eq!(first.status, 200, "{}", first.body);
        assert_eq!(
            first.body["result"]["test_case_results"]
                .as_array()
                .unwrap()
                .len(),
            20
        );
        assert_eq!(first.body["result"]["test_case_pagination"]["total"], 42);
        assert_eq!(first.body["result"]["test_case_pagination"]["per_page"], 20);
        let second = app.get_with_token(&format!("{url}?page=2"), &admin).await;
        assert_eq!(second.status, 200, "{}", second.body);
        let cases = second.body["result"]["test_case_results"]
            .as_array()
            .unwrap();
        assert_eq!(cases.len(), 20);
        assert_ne!(
            cases[0]["id"],
            first.body["result"]["test_case_results"][0]["id"]
        );
        for field in ["input", "expected_output", "stdout", "checker_output"] {
            assert_eq!(cases[0][field], "x\nx\nx\nx\nx\n… (truncated)");
        }
        let full_url = format!("{url}?result_id={result_id}&full_output=true");
        let expanded = app.get_with_token(&full_url, &admin).await;
        assert_eq!(expanded.status, 200, "{}", expanded.body);
        let rows = expanded.body["result"]["test_case_results"]
            .as_array()
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["stdout"], text);
        assert!(rows[0]["stderr"].as_str().unwrap().len() <= 65556);
        let hidden = app.get_with_token(&full_url, &owner).await;
        assert_eq!(hidden.status, 200);
        assert!(hidden.body["result"]["test_case_results"][0]["input"].is_null());
        assert!(hidden.body["result"]["test_case_results"][0]["stdout"].is_null());
        assert_eq!(app.get_with_token(&full_url, &stranger).await.status, 404);
        assert_eq!(
            app.get_with_token(&format!("{url}?full_output=true"), &admin)
                .await
                .status,
            400
        );
        let selected = app
            .get_with_token(&format!("{url}?test_case_ids={testcase_id}"), &admin)
            .await;
        assert_eq!(
            selected.body["result"]["test_case_results"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let old_url = format!("{}/{old_id}", routes::submission_judgements(submission_id));
        assert_eq!(app.get_with_token(&old_url, &owner).await.status, 404);
        let old = app.get_with_token(&old_url, &admin).await;
        assert_eq!(old.status, 200, "{}", old.body);
        assert_eq!(old.body["test_case_pagination"]["total"], 1);
    }

    #[tokio::test]
    async fn deferred_rejudge_preserves_current_cache_and_creates_pending_non_current_version() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_jhist3", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Deferred Problem").await;
        let (submission_id, _old_judgement_id, current_judgement_id) =
            seed_history(&app, "admin_jhist3", problem_id).await;

        let res = app
            .post_with_token(
                &routes::submission_rejudge(submission_id),
                &json!({"apply_immediately": false}),
                &admin_token,
            )
            .await;

        assert_eq!(res.status, 200, "unexpected body: {}", res.body);
        assert_eq!(res.body["status"], "Judged");
        assert_eq!(res.body["result"]["verdict"], "Accepted");
        assert_eq!(res.body["judge_epoch"], 11);

        let current = submission_judgement::Entity::find_by_id(current_judgement_id)
            .one(&app.db)
            .await
            .expect("load current judgement")
            .expect("current judgement exists");
        assert!(current.is_current);

        let pending = submission_judgement::Entity::find()
            .filter(submission_judgement::Column::SubmissionId.eq(submission_id))
            .filter(submission_judgement::Column::Version.eq(12))
            .one(&app.db)
            .await
            .expect("load pending judgement")
            .expect("pending judgement exists");
        assert!(!pending.is_current);
        assert_eq!(pending.judge_epoch, 12);
    }

    #[tokio::test]
    async fn apply_finalized_judgement_makes_it_current_and_updates_submission_cache() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_jhist4", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Apply Problem").await;
        let (submission_id, old_judgement_id, _current_judgement_id) =
            seed_history(&app, "admin_jhist4", problem_id).await;

        let res = app
            .post_with_token(
                &routes::submission_judgement_apply(submission_id, old_judgement_id),
                &json!({}),
                &admin_token,
            )
            .await;

        assert_eq!(res.status, 200, "unexpected body: {}", res.body);
        assert_eq!(res.body["result"]["verdict"], "WrongAnswer");
        assert_eq!(res.body["result"]["score"], 40.0);

        let applied = submission_judgement::Entity::find_by_id(old_judgement_id)
            .one(&app.db)
            .await
            .expect("load applied judgement")
            .expect("applied judgement exists");
        assert!(applied.is_current);
    }

    #[tokio::test]
    async fn discard_non_current_judgement_deletes_only_that_version_and_results() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_jhist5", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Discard Problem").await;
        let (submission_id, old_judgement_id, current_judgement_id) =
            seed_history(&app, "admin_jhist5", problem_id).await;

        let res = app
            .post_with_token(
                &routes::submission_judgement_discard(submission_id, old_judgement_id),
                &json!({}),
                &admin_token,
            )
            .await;

        assert_eq!(res.status, 204, "unexpected body: {}", res.body);
        let old = submission_judgement::Entity::find_by_id(old_judgement_id)
            .one(&app.db)
            .await
            .expect("query discarded judgement");
        assert!(old.is_none());

        let current_rows = test_case_result::Entity::find()
            .filter(test_case_result::Column::JudgementId.eq(Some(current_judgement_id)))
            .all(&app.db)
            .await
            .expect("query current rows");
        assert_eq!(current_rows.len(), 1);
    }

    #[tokio::test]
    async fn discard_rejects_pending_non_current_judgement() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_jhist6", "pass1234", "admin")
            .await;
        let problem_id = app
            .create_problem(&admin_token, "Pending Discard Problem")
            .await;
        let (submission_id, _, _) = seed_history(&app, "admin_jhist6", problem_id).await;

        let pending = submission_judgement::ActiveModel {
            submission_id: Set(submission_id),
            version: Set(12),
            is_current: Set(false),
            is_finalized: Set(false),
            triggered_by_user_id: Set(None),
            status: Set(SubmissionStatus::Running),
            judge_epoch: Set(12),
            created_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(&app.db)
        .await
        .expect("insert pending judgement");

        let res = app
            .post_with_token(
                &routes::submission_judgement_discard(submission_id, pending.id),
                &json!({}),
                &admin_token,
            )
            .await;

        assert_eq!(res.status, 409, "unexpected body: {}", res.body);
        let still_exists = submission_judgement::Entity::find_by_id(pending.id)
            .one(&app.db)
            .await
            .expect("query pending judgement")
            .is_some();
        assert!(still_exists);
    }
}

mod contest_submissions {
    use super::*;

    #[tokio::test]
    async fn participant_can_submit_to_contest_problem() {
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

        let body = valid_submission_body("cpp");
        let res = app
            .post_with_token(
                &routes::contest_problem_submissions(contest_id, problem_id),
                &body,
                &user_token,
            )
            .await;

        assert_eq!(res.status, 201);
        assert_eq!(res.body["contest_id"], contest_id);
    }

    #[tokio::test]
    async fn non_participant_cannot_submit_to_public_contest() {
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

        let body = valid_submission_body("cpp");
        let res = app
            .post_with_token(
                &routes::contest_problem_submissions(contest_id, problem_id),
                &body,
                &user_token,
            )
            .await;

        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn non_participant_cannot_submit_to_private_contest() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Contest Problem").await;
        let contest_id = app
            .create_contest(&admin_token, "Private Contest", false, false)
            .await;
        app.add_problem_to_contest(contest_id, problem_id, &admin_token)
            .await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;

        let body = valid_submission_body("cpp");
        let res = app
            .post_with_token(
                &routes::contest_problem_submissions(contest_id, problem_id),
                &body,
                &user_token,
            )
            .await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn cannot_submit_to_problem_not_in_contest() {
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

        let body = valid_submission_body("cpp");
        let res = app
            .post_with_token(
                &routes::contest_problem_submissions(contest_id, problem_id),
                &body,
                &user_token,
            )
            .await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn admin_can_submit_without_registration() {
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

        let body = valid_submission_body("cpp");
        let res = app
            .post_with_token(
                &routes::contest_problem_submissions(contest_id, problem_id),
                &body,
                &admin_token,
            )
            .await;

        assert_eq!(res.status, 201);
    }

    #[tokio::test]
    async fn cannot_submit_before_contest_activates() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Contest Problem").await;

        let res = app
            .post_with_token(
                routes::CONTESTS,
                &json!({
                    "title": "Future Contest",
                    "description": "Hasn't activated yet",
                    "activate_time": "2099-01-01T00:00:00Z",
                    "start_time": "2099-01-02T00:00:00Z",
                    "end_time": "2099-12-31T00:00:00Z",
                    "is_public": true,
                    "submissions_visible": false,
                    "contest_type": "standard",
                }),
                &admin_token,
            )
            .await;
        assert_eq!(res.status, 201);
        let contest_id = res.id();

        app.add_problem_to_contest(contest_id, problem_id, &admin_token)
            .await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;

        let body = valid_submission_body("cpp");
        let res = app
            .post_with_token(
                &routes::contest_problem_submissions(contest_id, problem_id),
                &body,
                &user_token,
            )
            .await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn cannot_submit_after_contest_deactivates() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Contest Problem").await;

        let res = app
            .post_with_token(
                routes::CONTESTS,
                &json!({
                    "title": "Past Contest",
                    "description": "Already deactivated",
                    "activate_time": "2020-01-01T00:00:00Z",
                    "start_time": "2020-01-02T00:00:00Z",
                    "end_time": "2020-12-31T00:00:00Z",
                    "deactivate_time": "2020-12-31T00:00:00Z",
                    "is_public": true,
                    "submissions_visible": false,
                    "contest_type": "standard",
                }),
                &admin_token,
            )
            .await;
        assert_eq!(res.status, 201);
        let contest_id = res.id();

        app.add_problem_to_contest(contest_id, problem_id, &admin_token)
            .await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;

        let body = valid_submission_body("cpp");
        let res = app
            .post_with_token(
                &routes::contest_problem_submissions(contest_id, problem_id),
                &body,
                &user_token,
            )
            .await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn cannot_submit_before_contest_starts() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Contest Problem").await;

        let res = app
            .post_with_token(
                routes::CONTESTS,
                &json!({
                    "title": "Future Contest",
                    "description": "Hasn't started yet",
                    "activate_time": "2020-01-01T00:00:00Z",
                    "start_time": "2099-01-01T00:00:00Z",
                    "end_time": "2099-12-31T00:00:00Z",
                    "is_public": true,
                    "submissions_visible": false,
                    "contest_type": "standard",
                }),
                &admin_token,
            )
            .await;
        assert_eq!(res.status, 201);
        let contest_id = res.id();

        app.add_problem_to_contest(contest_id, problem_id, &admin_token)
            .await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;
        app.register_for_contest(contest_id, &user_token).await;

        let body = valid_submission_body("cpp");
        let res = app
            .post_with_token(
                &routes::contest_problem_submissions(contest_id, problem_id),
                &body,
                &user_token,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
        assert!(
            res.body["message"]
                .as_str()
                .unwrap()
                .contains("not started")
        );
    }

    #[tokio::test]
    async fn cannot_submit_after_contest_ends() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Contest Problem").await;

        let res = app
            .post_with_token(
                routes::CONTESTS,
                &json!({
                    "title": "Past Contest",
                    "description": "Already ended",
                    "activate_time": "2020-01-01T00:00:00Z",
                    "start_time": "2020-01-01T00:00:00Z",
                    "end_time": "2020-01-02T00:00:00Z",
                    "is_public": true,
                    "submissions_visible": false,
                    "contest_type": "standard",
                }),
                &admin_token,
            )
            .await;
        assert_eq!(res.status, 201);
        let contest_id = res.id();

        app.add_problem_to_contest(contest_id, problem_id, &admin_token)
            .await;

        let user_token = app.create_authenticated_user("user1", "pass1234").await;

        let body = valid_submission_body("cpp");
        let res = app
            .post_with_token(
                &routes::contest_problem_submissions(contest_id, problem_id),
                &body,
                &user_token,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
        assert!(res.body["message"].as_str().unwrap().contains("ended"));
    }
}

mod bulk_rejudge {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn admin_can_bulk_rejudge_by_submission_ids() {
        use common::{SubmissionStatus, Verdict};
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use server::entity::submission;

        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_brj1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Rejudge Problem").await;

        let sub1 = app
            .create_submission(problem_id, &admin_token, "cpp", "int main() {}")
            .await;
        let sub2 = app
            .create_submission(problem_id, &admin_token, "cpp", "int main() { return 0; }")
            .await;

        submission::Entity::update_many()
            .col_expr(
                submission::Column::Status,
                sea_orm::sea_query::Expr::value(SubmissionStatus::Judged),
            )
            .col_expr(
                submission::Column::Verdict,
                sea_orm::sea_query::Expr::value(Some(Verdict::WrongAnswer)),
            )
            .filter(submission::Column::Id.is_in(vec![sub1, sub2]))
            .exec(&app.db)
            .await
            .expect("update submission status");

        let res = app
            .post_with_token(
                routes::SUBMISSIONS_BULK_REJUDGE,
                &json!({"submission_ids": [sub1, sub2]}),
                &admin_token,
            )
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["queued"], 2);

        let s1 = app
            .get_with_token(&routes::submission(sub1), &admin_token)
            .await;
        assert_ne!(
            s1.body["verdict"], "WrongAnswer",
            "verdict should have been cleared"
        );

        let s2 = app
            .get_with_token(&routes::submission(sub2), &admin_token)
            .await;
        assert_ne!(
            s2.body["verdict"], "WrongAnswer",
            "verdict should have been cleared"
        );
    }

    #[tokio::test]
    async fn returns_validation_error_with_empty_submission_ids() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_brj2", "pass1234", "admin")
            .await;

        let res = app
            .post_with_token(
                routes::SUBMISSIONS_BULK_REJUDGE,
                &json!({"submission_ids": []}),
                &admin_token,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn returns_validation_error_for_invalid_submission_id() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_brj3", "pass1234", "admin")
            .await;

        let res = app
            .post_with_token(
                routes::SUBMISSIONS_BULK_REJUDGE,
                &json!({"submission_ids": [0]}),
                &admin_token,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn bulk_rejudge_allows_non_terminal_submissions() {
        use common::{SubmissionStatus, Verdict};
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use server::entity::submission;

        let app = TestApp::spawn_with_options(crate::common::SpawnOptions {
            start_dispatcher: true,
            ..Default::default()
        })
        .await;
        let admin_token = app
            .create_user_with_role("admin_brj_custom", "pass1234", "admin")
            .await;
        let problem_id = app
            .create_problem(&admin_token, "Custom Verdict Problem")
            .await;

        let terminal_id = app
            .create_submission(problem_id, &admin_token, "cpp", "int main() { return 0; }")
            .await;
        let pending_id = app
            .create_submission(problem_id, &admin_token, "cpp", "int main() { return 1; }")
            .await;

        submission::Entity::update_many()
            .col_expr(
                submission::Column::Status,
                sea_orm::sea_query::Expr::value(SubmissionStatus::Judged),
            )
            .col_expr(
                submission::Column::Verdict,
                sea_orm::sea_query::Expr::value(Some(Verdict::Other(
                    "PartiallyAccepted".to_string(),
                ))),
            )
            .filter(submission::Column::Id.eq(terminal_id))
            .exec(&app.db)
            .await
            .expect("update terminal submission");

        submission::Entity::update_many()
            .col_expr(
                submission::Column::Status,
                sea_orm::sea_query::Expr::value(SubmissionStatus::Pending),
            )
            .col_expr(
                submission::Column::Verdict,
                sea_orm::sea_query::Expr::value(Option::<Verdict>::None),
            )
            .filter(submission::Column::Id.eq(pending_id))
            .exec(&app.db)
            .await
            .expect("update pending submission");

        let res = app
            .post_with_token(
                routes::SUBMISSIONS_BULK_REJUDGE,
                &json!({"submission_ids": [terminal_id, pending_id]}),
                &admin_token,
            )
            .await;

        assert_eq!(res.status, 200, "unexpected body: {}", res.body);
        assert_eq!(res.body["queued"], 2);

        let terminal = submission::Entity::find_by_id(terminal_id)
            .one(&app.db)
            .await
            .expect("load terminal submission")
            .expect("terminal submission should exist");
        assert_ne!(
            terminal.verdict,
            Some(Verdict::Other("PartiallyAccepted".to_string())),
            "terminal submission verdict should have been cleared after rejudge dispatch"
        );

        let pending = poll_submission_until_status(
            &app,
            pending_id,
            SubmissionStatus::Pending,
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(pending.status, SubmissionStatus::Pending);
        assert_eq!(pending.verdict, None);
        assert!(
            pending.judge_epoch > 0,
            "pending submission should have been re-dispatched"
        );
    }

    #[tokio::test]
    async fn contestant_cannot_bulk_rejudge() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_brj4", "pass1234", "admin")
            .await;
        let contestant_token = app
            .create_authenticated_user("contestant_brj4", "pass1234")
            .await;

        let problem_id = app.create_problem(&admin_token, "Rejudge Problem").await;
        let submission_id = app
            .create_submission(problem_id, &admin_token, "cpp", "int main() {}")
            .await;

        let res = app
            .post_with_token(
                routes::SUBMISSIONS_BULK_REJUDGE,
                &json!({"submission_ids": [submission_id]}),
                &contestant_token,
            )
            .await;

        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn bulk_rejudge_requires_system_admin_to_clear_worker_pin() {
        use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
        use server::entity::{role_permission, submission};

        let app = TestApp::spawn().await;
        role_permission::ActiveModel {
            role: Set("problem_setter".to_string()),
            permission: Set(perm::SUBMISSION_REJUDGE.to_string()),
        }
        .insert(&app.db)
        .await
        .expect("grant rejudge permission");

        let admin_token = app
            .create_user_with_role("admin_clear_pin_bulk", "pass1234", "admin")
            .await;
        let rejudge_token = app
            .create_user_with_role("setter_clear_pin_bulk", "pass1234", "problem_setter")
            .await;
        let problem_id = app
            .create_problem(&admin_token, "Pinned Bulk Rejudge Problem")
            .await;
        let submission_id = app
            .create_submission(problem_id, &admin_token, "cpp", "int main() {}")
            .await;

        submission::Entity::update_many()
            .col_expr(
                submission::Column::TargetWorkerId,
                sea_orm::sea_query::Expr::value(Some("worker-a".to_string())),
            )
            .filter(submission::Column::Id.eq(submission_id))
            .exec(&app.db)
            .await
            .expect("pin submission");

        let res = app
            .post_with_token(
                routes::SUBMISSIONS_BULK_REJUDGE,
                &json!({"submission_ids": [submission_id], "target_worker_id": ""}),
                &rejudge_token,
            )
            .await;

        assert_eq!(res.status, 403, "unexpected body: {}", res.body);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");

        let submission = submission::Entity::find_by_id(submission_id)
            .one(&app.db)
            .await
            .expect("load submission")
            .expect("submission exists");
        assert_eq!(submission.target_worker_id.as_deref(), Some("worker-a"));
    }
}

mod contest_submission_visibility {
    use super::*;

    #[tokio::test]
    async fn participant_sees_own_submissions_when_visibility_off() {
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

        let body = valid_submission_body("cpp");
        app.post_with_token(
            &routes::contest_problem_submissions(contest_id, problem_id),
            &body,
            &user_token,
        )
        .await;

        let res = app
            .get_with_token(&routes::contest_submissions(contest_id), &user_token)
            .await;

        assert_eq!(res.status, 200);
        let data = res.body["data"].as_array().expect("data should be array");
        assert_eq!(data.len(), 1);
    }

    #[tokio::test]
    async fn participant_cannot_see_others_when_visibility_off() {
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

        let user1_token = app.create_authenticated_user("user1", "pass1234").await;
        let user2_token = app.create_authenticated_user("user2", "pass1234").await;
        app.register_for_contest(contest_id, &user1_token).await;
        app.register_for_contest(contest_id, &user2_token).await;

        let body = valid_submission_body("cpp");
        app.post_with_token(
            &routes::contest_problem_submissions(contest_id, problem_id),
            &body,
            &user1_token,
        )
        .await;

        let res = app
            .get_with_token(&routes::contest_submissions(contest_id), &user2_token)
            .await;

        assert_eq!(res.status, 200);
        let data = res.body["data"].as_array().expect("data should be array");
        assert_eq!(data.len(), 0);
    }

    #[tokio::test]
    async fn participant_sees_all_when_visibility_on() {
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

        let user1_token = app.create_authenticated_user("user1", "pass1234").await;
        let user2_token = app.create_authenticated_user("user2", "pass1234").await;
        app.register_for_contest(contest_id, &user1_token).await;
        app.register_for_contest(contest_id, &user2_token).await;

        let body = valid_submission_body("cpp");
        app.post_with_token(
            &routes::contest_problem_submissions(contest_id, problem_id),
            &body,
            &user1_token,
        )
        .await;

        let res = app
            .get_with_token(&routes::contest_submissions(contest_id), &user2_token)
            .await;

        assert_eq!(res.status, 200);
        let data = res.body["data"].as_array().expect("data should be array");
        assert_eq!(data.len(), 1);
    }

    #[tokio::test]
    async fn admin_sees_all_regardless_of_visibility() {
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

        let body = valid_submission_body("cpp");
        app.post_with_token(
            &routes::contest_problem_submissions(contest_id, problem_id),
            &body,
            &user_token,
        )
        .await;

        let res = app
            .get_with_token(&routes::contest_submissions(contest_id), &admin_token)
            .await;

        assert_eq!(res.status, 200);
        let data = res.body["data"].as_array().expect("data should be array");
        assert_eq!(data.len(), 1);
    }

    /// Pagination-count leak fix: a PUBLIC, `submissions_visible=true`
    /// contest is reachable (via `check_contest_access`'s `is_public`
    /// branch) by an authenticated user who never registered. Before the
    /// fix, `can_see_all` was `can_view_all || contest.submissions_visible`
    /// with no participation check, so the SQL `total` counted every
    /// submission in the contest while `apply_filter_to_list`'s per-row
    /// kernel decision (which DOES require participation - see
    /// `visibility::host_rules::decide_submission`) denied every one of
    /// them - `{"data": [], "pagination": {"total": <every submission>}}`.
    /// `total` must now be bounded by what the static
    /// participation-or-ownership rule admits, i.e. 0 here.
    #[tokio::test]
    async fn non_participant_pagination_total_excludes_denied_rows_when_visibility_on() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin2", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Contest Problem").await;
        let contest_id = app
            .create_contest(&admin_token, "Public Visible Contest", true, true)
            .await;
        app.add_problem_to_contest(contest_id, problem_id, &admin_token)
            .await;

        let participant_token = app
            .create_authenticated_user("participant1", "pass1234")
            .await;
        app.register_for_contest(contest_id, &participant_token)
            .await;

        let body = valid_submission_body("cpp");
        app.post_with_token(
            &routes::contest_problem_submissions(contest_id, problem_id),
            &body,
            &participant_token,
        )
        .await;

        // Authenticated, but never registered for this contest. The contest
        // is public, so the top-level `check_contest_access` gate still lets
        // the request through (no 404) - only the row-level filter and the
        // aggregate count are at stake here.
        let outsider_token = app.create_authenticated_user("outsider1", "pass1234").await;

        let res = app
            .get_with_token(&routes::contest_submissions(contest_id), &outsider_token)
            .await;

        assert_eq!(res.status, 200);
        let data = res.body["data"].as_array().expect("data should be array");
        assert_eq!(
            data.len(),
            0,
            "non-participant must not see peer submissions"
        );
        assert_eq!(
            res.body["pagination"]["total"], 0,
            "pagination.total must not count rows the viewer cannot see"
        );
        assert_eq!(res.body["pagination"]["total_pages"], 0);
    }
}

/// Claim-fiber coverage.
///
/// These tests exercise the durable-accept path end-to-end: rows that
/// land in `Queued` (either via a POST or a direct DB write that
/// simulates an api crash mid-flight) must be promoted to `Pending` by
/// the per-server claim fiber within a small wall-clock budget. A full
/// kill-api-mid-POST harness would require subprocess control which the
/// integration suite doesn't have today; the equivalent invariant is
/// covered here by writing the `Queued` row directly with sea-orm,
/// bypassing the POST handler.
mod claim_fiber {
    use super::*;
    use sea_orm::{ActiveModelTrait, EntityTrait, Set};
    use server::entity::submission;
    use std::time::Duration;
    use tokio::time::{Instant, sleep};

    /// Best-effort wait until either the row's status changes off
    /// `Queued` or the deadline elapses. Returns the last-observed
    /// status string.
    async fn poll_until_not_queued(app: &TestApp, submission_id: i32, budget: Duration) -> String {
        let deadline = Instant::now() + budget;
        loop {
            let row = submission::Entity::find_by_id(submission_id)
                .one(&app.db)
                .await
                .expect("db lookup")
                .expect("submission row");
            let status = row.status.as_str().to_string();
            if status != "Queued" {
                return status;
            }
            if Instant::now() >= deadline {
                return status;
            }
            sleep(Duration::from_millis(50)).await;
        }
    }

    /// Posting a submission goes through the durable-accept path
    /// (POST inserts `Queued` and returns 201). The claim fiber
    /// running in the test app's runtime must promote the row to
    /// `Pending` (or further) without any other actor intervening.
    #[tokio::test]
    async fn claim_fiber_promotes_queued_submission_to_pending() {
        let app = TestApp::spawn_with_options(crate::common::SpawnOptions {
            start_dispatcher: true,
            ..Default::default()
        })
        .await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;
        let user_token = app.create_authenticated_user("user1", "pass1234").await;

        let body = valid_submission_body("cpp");
        let res = app
            .post_with_token(&routes::problem_submissions(problem_id), &body, &user_token)
            .await;
        assert_eq!(res.status, 201);
        // Contract: POST response reflects the just-committed row.
        assert_eq!(res.body["status"], "Queued");
        let submission_id = res.body["id"]
            .as_i64()
            .expect("submission id should be i64") as i32;

        // 5s budget is generous: the test fixture sets
        // `claim_poll_interval_ms=100`, so a healthy fiber promotes
        // within ~200ms. If the fiber is broken or never started this
        // assertion fails fast.
        let final_status = poll_until_not_queued(&app, submission_id, Duration::from_secs(5)).await;
        assert_ne!(
            final_status, "Queued",
            "claim fiber did not promote Queued submission within 5s"
        );

        // The claim also stamps ownership on the current judgement, the unit
        // the lease steal and the stuck detector recover.
        let row = app.current_judgement(submission_id).await;
        assert!(
            row.owner_server_id.is_some(),
            "claim fiber should set owner_server_id when promoting"
        );
        assert!(
            row.lease_heartbeat_at.is_some(),
            "claim fiber should set lease_heartbeat_at when promoting"
        );
    }

    /// Simulates an api crash between the POST handler's INSERT
    /// and any client-visible response: write a `Queued` row directly
    /// via sea-orm, then verify the claim fiber picks it up. This is
    /// the strongest assertion we can make in-process without
    /// subprocess control over the api.
    #[tokio::test]
    async fn claim_fiber_recovers_directly_inserted_queued_row() {
        let app = TestApp::spawn_with_options(crate::common::SpawnOptions {
            start_dispatcher: true,
            ..Default::default()
        })
        .await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;
        let user_token = app.create_authenticated_user("user1", "pass1234").await;

        // Resolve user id via /auth/me so we don't depend on the seed.
        let me = app.get_with_token("/api/v1/auth/me", &user_token).await;
        let user_id = me.body["id"].as_i64().expect("user id") as i32;

        let now = chrono::Utc::now();
        let inserted = app
            .insert_submission(submission::ActiveModel {
                files: Set(serde_json::json!([
                    {"filename": "main.cpp", "content": "int main(){}"}
                ])),
                language: Set("cpp".to_string()),
                user_id: Set(user_id),
                problem_id: Set(problem_id),
                contest_id: Set(None),
                // A contest type this replica has a handler for; the claim fiber
                // leaves unhandled ones to a replica that can judge them.
                contest_type: Set("standard".to_string()),
                created_at: Set(now),
                ..Default::default()
            })
            .await;

        let final_status = poll_until_not_queued(&app, inserted.id, Duration::from_secs(5)).await;
        assert_ne!(
            final_status, "Queued",
            "claim fiber failed to recover a directly-inserted Queued row"
        );
    }

    /// A non-current `submission_judgement` row
    /// inserted at `status=Queued` (simulating the deferred-rejudge
    /// post-commit state) must be picked up by the claim fiber's
    /// judgement scan and promoted to Pending. Without this path the
    /// `apply_immediately=false` rejudge silently strands on api crash.
    #[tokio::test]
    async fn claim_fiber_recovers_directly_inserted_queued_judgement() {
        use server::entity::submission_judgement;

        let app = TestApp::spawn_with_options(crate::common::SpawnOptions {
            start_dispatcher: true,
            ..Default::default()
        })
        .await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;
        let user_token = app.create_authenticated_user("user1", "pass1234").await;

        let me = app.get_with_token("/api/v1/auth/me", &user_token).await;
        let user_id = me.body["id"].as_i64().expect("user id") as i32;

        // First a submission row that's already terminal (mimicking
        // the deferred-rejudge precondition: a previously-judged
        // submission with a *new* non-current judgement pending).
        let now = chrono::Utc::now();
        let sub = submission::ActiveModel {
            files: Set(serde_json::json!([
                {"filename": "main.cpp", "content": "int main(){}"}
            ])),
            language: Set("cpp".to_string()),
            status: Set(common::SubmissionStatus::Judged),
            user_id: Set(user_id),
            problem_id: Set(problem_id),
            contest_id: Set(None),
            // A contest type this replica has a handler for (see above).
            contest_type: Set("standard".to_string()),
            created_at: Set(now),
            judge_epoch: Set(1),
            max_judge_epoch: Set(1),
            ..Default::default()
        }
        .insert(&app.db)
        .await
        .expect("direct submission insert");

        let queued_judgement = submission_judgement::ActiveModel {
            submission_id: Set(sub.id),
            version: Set(2),
            is_current: Set(false),
            is_finalized: Set(false),
            triggered_by_user_id: Set(Some(user_id)),
            status: Set(common::SubmissionStatus::Queued),
            judge_epoch: Set(2),
            created_at: Set(now),
            ..Default::default()
        }
        .insert(&app.db)
        .await
        .expect("direct judgement insert at Queued");

        // Poll the judgement row directly - the claim fiber's
        // judgement scan should flip its status off Queued. We assert
        // owner_server_id was populated on the judgement (not the
        // submission, which stays at Judged because the deferred
        // judgement intentionally doesn't override the displayed
        // verdict).
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let final_judgement = loop {
            let row = submission_judgement::Entity::find_by_id(queued_judgement.id)
                .one(&app.db)
                .await
                .expect("db lookup")
                .expect("judgement row");
            if row.status.as_str() != "Queued" {
                break row;
            }
            if tokio::time::Instant::now() >= deadline {
                break row;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert_ne!(
            final_judgement.status.as_str(),
            "Queued",
            "claim fiber did not promote Queued judgement within 5s"
        );
        assert!(
            final_judgement.owner_server_id.is_some(),
            "claim fiber should set owner_server_id on promoted judgement"
        );
        assert!(
            final_judgement.lease_heartbeat_at.is_some(),
            "claim fiber should set lease_heartbeat_at on promoted judgement"
        );
        // The submission's own status must NOT have been touched -
        // the deferred path doesn't override the displayed verdict.
        let parent = submission::Entity::find_by_id(sub.id)
            .one(&app.db)
            .await
            .expect("db")
            .expect("submission row");
        assert_eq!(parent.status.as_str(), "Judged");
    }
}

/// Backpressure on POST - `server.max_queued_submissions` caps
/// the durable `Queued` row depth across `submission`, `code_run`,
/// and `submission_judgement`. POST endpoints that would insert a
/// new `Queued` row return 503 + `Retry-After` when the cap is hit
/// (RFC 7231 §6.6.4: "server is currently unable to handle the
/// request due to a temporary overload").
mod backpressure {
    use super::*;
    use crate::common::SpawnOptions;
    use chrono::{Duration, Utc};
    use sea_orm::{ActiveModelTrait, Set};
    use server::entity::submission;

    /// Inserts `n` placeholder `Queued` rows directly into the
    /// `submission` table, bypassing the API. Returns once the
    /// COUNT(*) the handler will observe is at least `n`.
    ///
    /// Done directly via sea-orm (not via POST) because each POST
    /// would itself be subject to the cap under test. Pre-staging
    /// also lets us land on an exact depth deterministically.
    async fn stage_queued_submissions(app: &TestApp, user_id: i32, problem_id: i32, n: usize) {
        for _ in 0..n {
            app.insert_submission(submission::ActiveModel {
                files: Set(serde_json::json!([
                    {"filename": "main.cpp", "content": "int main(){}"}
                ])),
                language: Set("cpp".into()),
                user_id: Set(user_id),
                problem_id: Set(problem_id),
                contest_id: Set(None),
                contest_type: Set("standard".into()),
                created_at: Set(Utc::now() - Duration::minutes(2)),
                ..Default::default()
            })
            .await;
        }
    }

    async fn fetch_user_id(app: &TestApp, username: &str) -> i32 {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        let row = server::entity::user::Entity::find()
            .filter(server::entity::user::Column::Username.eq(username))
            .one(&app.db)
            .await
            .expect("db")
            .expect("user row");
        row.id
    }

    /// **Test 1**: when `max_queued_submissions=1`, the very first
    /// POST that pushes the depth to the cap is accepted, and the
    /// next POST is rejected with 503 + `Retry-After`. The 503 path
    /// runs through `AppError::Overloaded` so the response body has
    /// `code=QUEUE_OVERLOADED` and the header is set as integer
    /// seconds.
    #[tokio::test]
    async fn rejects_with_503_when_queue_depth_at_cap() {
        let app = TestApp::spawn_with_options(SpawnOptions {
            max_queued_submissions: Some(1),
            disable_claim_fiber: true,
            ..Default::default()
        })
        .await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;
        let user_token = app.create_authenticated_user("user1", "pass1234").await;
        let user_id = fetch_user_id(&app, "user1").await;

        // Pre-stage one `Queued` row so the next POST observes
        // depth >= cap. Direct DB insert dodges the cap (which is
        // checked only in the handler) and is the only way to land
        // on exactly depth=1.
        stage_queued_submissions(&app, user_id, problem_id, 1).await;

        let body = valid_submission_body("cpp");
        let res = app
            .post_with_token(&routes::problem_submissions(problem_id), &body, &user_token)
            .await;

        assert_eq!(
            res.status, 503,
            "expected 503 once durable Queued depth reaches cap; got body={}",
            res.text
        );
        assert_eq!(res.body["code"], "QUEUE_OVERLOADED");
        // Retry-After must be set (RFC 7231 §7.1.3) so clients know
        // when to retry. We don't pin a specific value to keep the
        // const free to evolve.
        let retry_after = res
            .headers
            .get("Retry-After")
            .expect("Retry-After header should be present on 503 backpressure response");
        let parsed: u64 = retry_after
            .to_str()
            .expect("Retry-After should be ASCII")
            .parse()
            .expect("Retry-After should be integer seconds");
        assert!(parsed > 0, "Retry-After must be a positive integer");
    }

    /// **Test 2**: when the queue is empty, POST succeeds even with
    /// a low cap. This is the "cap is permissive when fleet is
    /// idle" baseline that any false-positive backpressure logic
    /// (e.g., off-by-one on the comparison operator, double-counting
    /// across tables) would break.
    #[tokio::test]
    async fn accepts_when_queue_is_below_cap() {
        let app = TestApp::spawn_with_options(SpawnOptions {
            max_queued_submissions: Some(10),
            disable_claim_fiber: true,
            ..Default::default()
        })
        .await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;
        let user_token = app.create_authenticated_user("user1", "pass1234").await;

        let body = valid_submission_body("cpp");
        let res = app
            .post_with_token(&routes::problem_submissions(problem_id), &body, &user_token)
            .await;

        assert_eq!(
            res.status, 201,
            "expected 201 when queue is empty; got body={}",
            res.text
        );
        assert_eq!(res.body["status"], "Queued");
    }

    /// **Test 3**: covers a second site (`run_code`) to confirm the
    /// admission check is wired at the code-run endpoint too. Same
    /// pre-staged-depth pattern: one queued submission is enough to trip
    /// the cap because `count_queued_rows` sums queued judgements and code
    /// runs, not just the table being written.
    #[tokio::test]
    async fn run_code_rejects_when_queue_depth_at_cap() {
        let app = TestApp::spawn_with_options(SpawnOptions {
            max_queued_submissions: Some(1),
            disable_claim_fiber: true,
            ..Default::default()
        })
        .await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;
        let user_token = app.create_authenticated_user("user1", "pass1234").await;
        let user_id = fetch_user_id(&app, "user1").await;

        stage_queued_submissions(&app, user_id, problem_id, 1).await;

        let body = serde_json::json!({
            "files": [{"filename": "main.cpp", "content": "int main(){}"}],
            "language": "cpp",
            "custom_test_cases": [{"input": "1\n"}],
        });
        let res = app
            .post_with_token(&routes::problem_code_runs(problem_id), &body, &user_token)
            .await;

        assert_eq!(res.status, 503);
        assert_eq!(res.body["code"], "QUEUE_OVERLOADED");
    }

    /// **Test 4**: covers `rejudge_submission` - confirms that
    /// non-ingress write paths that also insert a `Queued` row are
    /// gated by the same cap. Picks the deferred-rejudge branch
    /// (`apply_immediately=false`) because that one queues a non-current
    /// judgement and leaves the submission alone.
    #[tokio::test]
    async fn rejudge_rejects_when_queue_depth_at_cap() {
        let app = TestApp::spawn_with_options(SpawnOptions {
            max_queued_submissions: Some(1),
            disable_claim_fiber: true,
            ..Default::default()
        })
        .await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;
        let _user_token = app.create_authenticated_user("user1", "pass1234").await;
        let user_id = fetch_user_id(&app, "user1").await;

        // Stage two rows: one to *be* the submission targeted by
        // rejudge (any non-terminal status would do; `Judged` is the
        // canonical "ready to rejudge" state), and one to pin
        // the durable-Queued depth at the cap. We have to write the
        // target submission with `Judged` so it isn't itself counted
        // toward the cap - otherwise we couldn't distinguish "cap
        // tripped by setup" from "cap tripped by the handler".
        let target = submission::ActiveModel {
            files: Set(serde_json::json!([
                {"filename": "main.cpp", "content": "int main(){}"}
            ])),
            language: Set("cpp".into()),
            status: Set(common::SubmissionStatus::Judged),
            user_id: Set(user_id),
            problem_id: Set(problem_id),
            contest_id: Set(None),
            contest_type: Set("standard".into()),
            created_at: Set(Utc::now()),
            ..Default::default()
        };
        let target = target.insert(&app.db).await.expect("insert target");
        stage_queued_submissions(&app, user_id, problem_id, 1).await;

        // Rejudge admin permission piggybacks on `admin`.
        let body = serde_json::json!({ "apply_immediately": false });
        let res = app
            .post_with_token(
                &format!("/api/v1/submissions/{}/rejudge", target.id),
                &body,
                &admin_token,
            )
            .await;

        assert_eq!(
            res.status, 503,
            "rejudge should observe the durable-Queued cap; got body={}",
            res.text
        );
        assert_eq!(res.body["code"], "QUEUE_OVERLOADED");
    }

    /// **Test 5**: `max_queued_submissions=0` disables the check
    /// (used by all tests outside this module). Even with a packed
    /// queue, the POST is accepted. Without this escape hatch, the
    /// fixture default would have to track a moving target every
    /// time the production default changes.
    #[tokio::test]
    async fn cap_of_zero_disables_backpressure() {
        let app = TestApp::spawn_with_options(SpawnOptions {
            max_queued_submissions: Some(0),
            disable_claim_fiber: true,
            ..Default::default()
        })
        .await;
        let admin_token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin_token, "Test Problem").await;
        let user_token = app.create_authenticated_user("user1", "pass1234").await;
        let user_id = fetch_user_id(&app, "user1").await;

        // 50 staged rows would trip *any* sane cap; with cap=0 the
        // handler should not even count them.
        stage_queued_submissions(&app, user_id, problem_id, 50).await;

        let body = valid_submission_body("cpp");
        let res = app
            .post_with_token(&routes::problem_submissions(problem_id), &body, &user_token)
            .await;

        assert_eq!(
            res.status, 201,
            "cap=0 should be a no-op; got body={}",
            res.text
        );
    }
}

mod page_window_bounds {
    use super::*;

    /// `page` large enough that `(page - 1) * 20` overflows `u64`.
    const OVERFLOWING_PAGE: &str = "18446744073709551615";

    /// `page` whose offset fits in `u64` but not in Postgres' `bigint`:
    /// `(page - 1) * 2 = 2^63`, one past `i64::MAX`.
    const BIGINT_OVERFLOW_PAGE: &str = "4611686018427387905";

    /// Send a GET and return the status, or `None` if the server closed the
    /// connection without answering (what a panicking handler looks like).
    async fn status_of(app: &TestApp, path: &str, token: &str) -> Option<u16> {
        app.client
            .get(format!("http://{}{}", app.addr, path))
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
            .ok()
            .map(|r| r.status().as_u16())
    }

    /// Assert that an out-of-range page got a 4xx answer, not a dropped
    /// connection or a 5xx.
    fn assert_client_error(path: &str, status: Option<u16>) {
        match status {
            None => panic!(
                "{path}: server dropped the connection without a response \
             (handler panicked on u64 overflow in `(page - 1) * per_page`)"
            ),
            Some(s) => assert!(
                (400..500).contains(&s),
                "{path}: expected a 4xx for an out-of-range page, got {s}"
            ),
        }
    }

    async fn admin(app: &TestApp) -> String {
        app.create_user_with_role("pg_admin", "password123", "admin")
            .await
    }

    /// Testcase-result pages go through the same page window: an overflowing
    /// `page` gets an answer below 500, never a dropped connection.
    #[tokio::test]
    async fn submission_result_page_overflow_is_a_validation_error() {
        let app = TestApp::spawn().await;
        let token = admin(&app).await;
        let pid = app.create_problem(&token, "P").await;
        let sid = app
            .create_submission(pid, &token, "cpp", "int main(){}")
            .await;

        let path = format!(
            "{}?page={OVERFLOWING_PAGE}",
            routes::submission_judgements(sid)
        );
        let status = status_of(&app, &path, &token).await;
        assert_ne!(status, None, "{path}: connection dropped");
        // 400 (bad page) or 404 (no judgement yet) are both fine; what matters is
        // that the guarded arithmetic answers instead of panicking.
        assert!(
            status.unwrap() < 500,
            "{path}: got {status:?}, expected a non-5xx"
        );
    }

    /// Every paginated list derives its offset through `PageWindow`, so a `page`
    /// whose offset overflows `u64` or exceeds Postgres' `bigint` OFFSET is a 4xx.
    /// This one covers `GET /problems`.
    #[tokio::test]
    async fn list_problems_rejects_overflowing_page() {
        let app = TestApp::spawn().await;
        let token = admin(&app).await;
        for page in [OVERFLOWING_PAGE, BIGINT_OVERFLOW_PAGE] {
            let path = format!("{}?page={page}&per_page=2", routes::PROBLEMS);
            assert_client_error(&path, status_of(&app, &path, &token).await);
        }
    }

    /// `GET /contests` answers an out-of-range `page` with a 4xx.
    #[tokio::test]
    async fn list_contests_rejects_overflowing_page() {
        let app = TestApp::spawn().await;
        let token = admin(&app).await;
        for page in [OVERFLOWING_PAGE, BIGINT_OVERFLOW_PAGE] {
            let path = format!("{}?page={page}&per_page=2", routes::CONTESTS);
            assert_client_error(&path, status_of(&app, &path, &token).await);
        }
    }

    /// `GET /submissions` answers an out-of-range `page` with a 4xx.
    #[tokio::test]
    async fn list_submissions_rejects_overflowing_page() {
        let app = TestApp::spawn().await;
        let token = admin(&app).await;
        for page in [OVERFLOWING_PAGE, BIGINT_OVERFLOW_PAGE] {
            let path = format!("{}?page={page}&per_page=2", routes::SUBMISSIONS);
            assert_client_error(&path, status_of(&app, &path, &token).await);
        }
    }

    /// The contest submissions list answers an out-of-range `page` with a 4xx.
    #[tokio::test]
    async fn list_contest_submissions_rejects_overflowing_page() {
        let app = TestApp::spawn().await;
        let token = admin(&app).await;
        let cid = app.create_contest(&token, "C", true, true).await;
        for page in [OVERFLOWING_PAGE, BIGINT_OVERFLOW_PAGE] {
            let path = format!(
                "{}?page={page}&per_page=2",
                routes::contest_submissions(cid)
            );
            assert_client_error(&path, status_of(&app, &path, &token).await);
        }
    }

    /// The dead-letter queue list answers an out-of-range `page` with a 4xx.
    #[tokio::test]
    async fn list_dlq_rejects_overflowing_page() {
        let app = TestApp::spawn().await;
        let token = admin(&app).await;
        for page in [OVERFLOWING_PAGE, BIGINT_OVERFLOW_PAGE] {
            let path = format!("{}?page={page}&per_page=2", routes::DLQ);
            assert_client_error(&path, status_of(&app, &path, &token).await);
        }
    }
}

mod rejudge_lifecycle {
    use super::*;
    use chrono::Utc;
    use common::{SubmissionStatus, Verdict};
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
    use server::entity::{submission, submission_judgement};

    /// A judged standalone submission at `judge_epoch = 11` with one finalized
    /// current judgement, i.e. the normal state before an admin rejudges.
    async fn seed_judged(
        app: &TestApp,
        token: &str,
        problem_id: i32,
        system_error_retries: i32,
    ) -> i32 {
        let user_id = app.get_with_token(routes::ME, token).await.id();
        let now = Utc::now();
        let sub = submission::ActiveModel {
            files: Set(json!([{"filename": "main.cpp", "content": "int main(){}"}])),
            language: Set("cpp".into()),
            user_id: Set(user_id),
            problem_id: Set(problem_id),
            contest_id: Set(None),
            contest_type: Set("standard".into()),
            status: Set(SubmissionStatus::Judged),
            verdict: Set(Some(Verdict::Accepted)),
            score: Set(Some(100.0)),
            judge_epoch: Set(11),
            max_judge_epoch: Set(11),
            system_error_retries: Set(system_error_retries),
            created_at: Set(now),
            judged_at: Set(Some(now)),
            ..Default::default()
        }
        .insert(&app.db)
        .await
        .expect("insert submission");
        submission_judgement::ActiveModel {
            submission_id: Set(sub.id),
            version: Set(11),
            is_current: Set(true),
            is_finalized: Set(true),
            status: Set(SubmissionStatus::Judged),
            verdict: Set(Some(Verdict::Accepted)),
            score: Set(Some(100.0)),
            judge_epoch: Set(11),
            created_at: Set(now),
            finalized_at: Set(Some(now)),
            ..Default::default()
        }
        .insert(&app.db)
        .await
        .expect("insert judgement");
        sub.id
    }

    /// Every judgement of submission `sid`, oldest version first.
    async fn judgements(app: &TestApp, sid: i32) -> Vec<submission_judgement::Model> {
        submission_judgement::Entity::find()
            .filter(submission_judgement::Column::SubmissionId.eq(sid))
            .order_by_asc(submission_judgement::Column::Version)
            .all(&app.db)
            .await
            .expect("db")
    }

    /// Every judgement of a submission gets its own `judge_epoch`, even when a deferred
    /// rejudge is followed by an immediate one, and applying a judgement never moves the
    /// submission's epoch backwards. The epoch fences late worker results and keys the
    /// `after_judging` hook dedupe, so two judgements must never share one.
    #[tokio::test]
    async fn rejudge_epochs_are_unique() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("rejudge_admin", "password123", "admin")
            .await;
        let pid = app.create_problem(&token, "P").await;
        let sid = seed_judged(&app, &token, pid, 1).await;

        let deferred = app
            .post_with_token(
                &routes::submission_rejudge(sid),
                &json!({"apply_immediately": false}),
                &token,
            )
            .await;
        assert!(deferred.status < 300, "deferred rejudge: {}", deferred.text);
        let immediate = app
            .post_with_token(&routes::submission_rejudge(sid), &json!({}), &token)
            .await;
        assert!(
            immediate.status < 300,
            "immediate rejudge: {}",
            immediate.text
        );

        let rows = judgements(&app, sid).await;
        let epochs: Vec<i32> = rows.iter().map(|j| j.judge_epoch).collect();
        let mut dedup = epochs.clone();
        dedup.sort_unstable();
        dedup.dedup();
        assert_eq!(
            dedup.len(),
            epochs.len(),
            "duplicate judge_epoch across judgements: {epochs:?}"
        );

        // Applying the (finalized) deferred judgement must not move the
        // submission's epoch backwards, and the next rejudge must still get an
        // epoch no other judgement holds.
        let before = submission::Entity::find_by_id(sid)
            .one(&app.db)
            .await
            .expect("db")
            .expect("submission")
            .judge_epoch;
        let deferred_id = rows[1].id;
        assert!(!rows[1].is_current, "control: v2 is the deferred judgement");
        let mut finalize: submission_judgement::ActiveModel = rows[1].clone().into();
        finalize.is_finalized = Set(true);
        finalize.status = Set(SubmissionStatus::Judged);
        finalize.verdict = Set(Some(Verdict::WrongAnswer));
        finalize.finalized_at = Set(Some(Utc::now()));
        finalize.update(&app.db).await.expect("finalize deferred");
        let applied = app
            .post_with_token(
                &routes::submission_judgement_apply(sid, deferred_id),
                &json!({}),
                &token,
            )
            .await;
        assert_eq!(applied.status, 200, "apply: {}", applied.text);
        let after = submission::Entity::find_by_id(sid)
            .one(&app.db)
            .await
            .expect("db")
            .expect("submission");
        assert!(
            after.judge_epoch > before,
            "apply moved judge_epoch from {before} to {}",
            after.judge_epoch
        );
        let again = app
            .post_with_token(&routes::submission_rejudge(sid), &json!({}), &token)
            .await;
        assert!(again.status < 300, "rejudge after apply: {}", again.text);
        let epochs: Vec<i32> = judgements(&app, sid)
            .await
            .iter()
            .map(|j| j.judge_epoch)
            .collect();
        let mut dedup = epochs.clone();
        dedup.sort_unstable();
        dedup.dedup();
        assert_eq!(
            dedup.len(),
            epochs.len(),
            "duplicate judge_epoch after apply + rejudge: {epochs:?}"
        );
    }

    /// Applying a deferred judgement re-stamps it with a fresh epoch, and its
    /// `test_case_result` rows move to that epoch with it. Plugins such as IOI join
    /// results to their judgement on the epoch, so stale result epochs would score the
    /// applied judgement as 0.
    #[tokio::test]
    async fn applied_judgement_results_keep_matching_its_epoch() {
        use sea_orm::{ConnectionTrait, DbBackend, Statement};
        use server::entity::test_case_result;

        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("rejudge_admin", "password123", "admin")
            .await;
        let pid = app.create_problem(&token, "P").await;
        let sid = seed_judged(&app, &token, pid, 1).await;

        let deferred = app
            .post_with_token(
                &routes::submission_rejudge(sid),
                &json!({"apply_immediately": false}),
                &token,
            )
            .await;
        assert!(deferred.status < 300, "deferred rejudge: {}", deferred.text);
        let rows = judgements(&app, sid).await;
        let j2 = rows.last().expect("deferred judgement").clone();
        assert!(!j2.is_current);

        // The deferred run writes its results under its own epoch and finishes.
        for _ in 0..2 {
            test_case_result::ActiveModel {
                submission_id: Set(sid),
                judgement_id: Set(Some(j2.id)),
                judge_epoch: Set(j2.judge_epoch),
                verdict: Set(Verdict::Accepted),
                score: Set(50.0),
                created_at: Set(Utc::now()),
                ..Default::default()
            }
            .insert(&app.db)
            .await
            .expect("result");
        }
        let mut finalize: submission_judgement::ActiveModel = j2.clone().into();
        finalize.is_finalized = Set(true);
        finalize.status = Set(SubmissionStatus::Judged);
        finalize.verdict = Set(Some(Verdict::Accepted));
        finalize.score = Set(Some(100.0));
        finalize.finalized_at = Set(Some(Utc::now()));
        finalize.update(&app.db).await.expect("finalize deferred");

        let applied = app
            .post_with_token(
                &routes::submission_judgement_apply(sid, j2.id),
                &json!({}),
                &token,
            )
            .await;
        assert_eq!(applied.status, 200, "apply: {}", applied.text);
        let current = judgements(&app, sid)
            .await
            .into_iter()
            .find(|j| j.is_current)
            .expect("current judgement");
        assert_eq!(current.id, j2.id);
        assert!(
            current.judge_epoch > j2.judge_epoch,
            "apply takes a fresh epoch"
        );

        // IOI's join (`plugins/ioi/src/score.rs`), minus its contest filter.
        let row = app
            .db
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT COUNT(*) AS n FROM test_case_result tcr \
             JOIN submission_judgement sj \
               ON sj.id = tcr.judgement_id \
              AND sj.submission_id = tcr.submission_id \
              AND sj.judge_epoch = tcr.judge_epoch \
             WHERE tcr.submission_id = $1 AND sj.is_current AND sj.is_finalized",
                [sid.into()],
            ))
            .await
            .expect("db")
            .expect("row");
        let matched: i64 = row.try_get("", "n").expect("n");
        assert_eq!(
            matched, 2,
            "the applied judgement's results do not join on its epoch"
        );
    }

    /// Rejudging an in-flight submission finalizes the judgement it supersedes as a
    /// `SUPERSEDED` system error. A superseded judgement left unfinalized can never
    /// finish, so the stuck detector would re-run it until it lands in the DLQ.
    #[tokio::test]
    async fn rejudge_finalizes_the_superseded_inflight_judgement() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("rejudge_admin", "password123", "admin")
            .await;
        let pid = app.create_problem(&token, "P").await;
        let user_id = app.get_with_token(routes::ME, &token).await.id();
        let now = Utc::now();
        let sub = submission::ActiveModel {
            files: Set(json!([{"filename": "main.cpp", "content": "int main(){}"}])),
            language: Set("cpp".into()),
            user_id: Set(user_id),
            problem_id: Set(pid),
            contest_type: Set("standard".into()),
            status: Set(SubmissionStatus::Running),
            judge_epoch: Set(5),
            max_judge_epoch: Set(5),
            created_at: Set(now),
            ..Default::default()
        }
        .insert(&app.db)
        .await
        .expect("insert submission");
        let j1 = submission_judgement::ActiveModel {
            submission_id: Set(sub.id),
            version: Set(1),
            is_current: Set(true),
            is_finalized: Set(false),
            status: Set(SubmissionStatus::Running),
            judge_epoch: Set(5),
            owner_server_id: Set(Some("server-x".into())),
            lease_heartbeat_at: Set(Some(now)),
            leased_at: Set(Some(now)),
            created_at: Set(now),
            ..Default::default()
        }
        .insert(&app.db)
        .await
        .expect("insert running judgement");

        let res = app
            .post_with_token(&routes::submission_rejudge(sub.id), &json!({}), &token)
            .await;
        if (400..500).contains(&res.status) {
            // Refusing to rejudge an in-flight submission is also acceptable.
            return;
        }
        assert!(res.status < 300, "rejudge: {}", res.text);

        let old = submission_judgement::Entity::find_by_id(j1.id)
            .one(&app.db)
            .await
            .expect("db")
            .expect("j1");
        assert!(!old.is_current, "control: j1 superseded");
        assert!(
            old.is_finalized,
            "superseded judgement left unfinalized in {:?}; stuck detector will re-run it",
            old.status
        );
        assert_eq!(old.status, SubmissionStatus::SystemError);
        assert_eq!(old.error_code.as_deref(), Some("SUPERSEDED"));

        // Finalized, it can now be discarded like any other old version.
        let discard = app
            .post_with_token(
                &routes::submission_judgement_discard(sub.id, j1.id),
                &json!({}),
                &token,
            )
            .await;
        assert_eq!(discard.status, 204, "discard: {}", discard.text);
    }

    /// A rejudge starts a fresh judging attempt: the submission's system-error retry
    /// budget starts over, and the new judgement has its full dispatch-retry budget.
    #[tokio::test]
    async fn rejudge_resets_retry_budgets() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("rejudge_admin", "password123", "admin")
            .await;
        let pid = app.create_problem(&token, "P").await;
        let sid = seed_judged(&app, &token, pid, 5).await;

        let res = app
            .post_with_token(&routes::submission_rejudge(sid), &json!({}), &token)
            .await;
        assert!(res.status < 300, "rejudge: {}", res.text);

        let sub = submission::Entity::find_by_id(sid)
            .one(&app.db)
            .await
            .expect("db")
            .expect("submission");
        assert_eq!(
            sub.system_error_retries, 0,
            "fresh judging attempt inherited the previous attempt's retry budget"
        );
        assert_eq!(app.current_judgement(sid).await.dispatch_retries, 0);
    }
}

mod bulk_rejudge_limits {
    use super::*;

    /// Bulk rejudge rejects more than `MAX_BULK_REJUDGE_IDS` (10,000) ids with
    /// `VALIDATION_ERROR` and accepts exactly the cap. The cap also keeps the `IN (...)`
    /// list under the Postgres bind-parameter limit.
    #[tokio::test]
    async fn bulk_rejudge_enforces_documented_cap() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("bulk_rejudge_admin", "password123", "admin")
            .await;
        let ids: Vec<i32> = (1..=10_001).collect();
        let res = app
            .post_with_token(
                routes::SUBMISSIONS_BULK_REJUDGE,
                &json!({"submission_ids": ids}),
                &token,
            )
            .await;
        assert_eq!(res.status, 400, "{}", res.text);
        assert_eq!(res.body["code"], "VALIDATION_ERROR", "{}", res.text);

        // Control: exactly the cap is accepted (none of these ids exist).
        let ids: Vec<i32> = (1..=10_000).collect();
        let res = app
            .post_with_token(
                routes::SUBMISSIONS_BULK_REJUDGE,
                &json!({"submission_ids": ids}),
                &token,
            )
            .await;
        assert_eq!(res.status, 200, "{}", res.text);
    }
}

mod standalone_submit_to_hidden_contest_problem {
    use super::*;
    use crate::codelink_bracket::visible_problem_ids;
    use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};
    use serde_json::Value;

    /// A cpp submission body, optionally with a caller-chosen `contest_type`.
    fn submission_body(contest_type: Option<&str>) -> Value {
        let mut body = valid_submission_body("cpp");
        if let Some(ct) = contest_type {
            body["contest_type"] = json!(ct);
        }
        body
    }

    /// A cpp code-run body with one custom test case.
    fn code_run_body() -> Value {
        json!({
            "files": [{"filename": "main.cpp", "content": "#include <iostream>\nint main() {}"}],
            "language": "cpp",
            "custom_test_cases": [{"input": "5\n1 2 3 4 5", "expected_output": "15"}],
        })
    }

    /// Contest-less submission and code-run rows stored for `problem_id`. A
    /// denied standalone request must leave both at zero.
    async fn standalone_rows(app: &TestApp, problem_id: i32) -> (u64, u64) {
        use server::entity::{code_run, submission};
        let submissions = submission::Entity::find()
            .filter(submission::Column::ProblemId.eq(problem_id))
            .filter(submission::Column::ContestId.is_null())
            .count(&app.db)
            .await
            .expect("count standalone submissions");
        let code_runs = code_run::Entity::find()
            .filter(code_run::Column::ProblemId.eq(problem_id))
            .filter(code_run::Column::ContestId.is_null())
            .count(&app.db)
            .await
            .expect("count standalone code runs");
        (submissions, code_runs)
    }

    /// Asserts that the standalone submit (with and without a caller-chosen
    /// `contest_type`) and the standalone code run are both a silent 404 for
    /// `token`, and that neither request stored a row.
    async fn assert_standalone_submit_hidden(
        app: &TestApp,
        problem_id: i32,
        token: &str,
        who: &str,
    ) {
        for contest_type in [None, Some("standard")] {
            let res = app
                .post_with_token(
                    &routes::problem_submissions(problem_id),
                    &submission_body(contest_type),
                    token,
                )
                .await;
            assert_eq!(
                res.status, 404,
                "{who}: standalone submit (contest_type {contest_type:?}) to hidden contest problem \
             {problem_id} must be hidden, got {}: {}",
                res.status, res.text
            );
            assert_eq!(res.body["code"], "NOT_FOUND", "{}", res.text);
        }

        let res = app
            .post_with_token(
                &routes::problem_code_runs(problem_id),
                &code_run_body(),
                token,
            )
            .await;
        assert_eq!(
            res.status, 404,
            "{who}: standalone code run on hidden contest problem {problem_id} must be hidden, got \
         {}: {}",
            res.status, res.text
        );
        assert_eq!(res.body["code"], "NOT_FOUND", "{}", res.text);

        assert_eq!(
            standalone_rows(app, problem_id).await,
            (0, 0),
            "{who}: a denied standalone request must not store a contest-less row"
        );
    }

    /// A participant can read a hidden problem through its running private contest, but
    /// can submit or run code on it only through the contest route. The standalone routes
    /// store rows without a `contest_id`, which would skip contest-scoped plugin config,
    /// feedback masking and standings. Problem editors can still submit standalone.
    #[tokio::test]
    async fn hidden_contest_problem_rejects_standalone_submission() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_hidden_private", "pass1234", "admin")
            .await;
        let contestant = app
            .create_authenticated_user("contestant_hidden_private", "pass1234")
            .await;

        let pid = app
            .create_hidden_problem(&admin, "Hidden Contest Problem")
            .await;
        // Private, activated, started, and not yet ended.
        let cid = app
            .create_contest(&admin, "Private Running Contest", false, true)
            .await;
        app.add_problem_to_contest(cid, pid, &admin).await;

        let contestant_id = app.get_with_token(routes::ME, &contestant).await.id();
        let res = app
            .post_with_token(
                &routes::contest_participants(cid),
                &json!({ "user_id": contestant_id }),
                &admin,
            )
            .await;
        assert_eq!(res.status, 201, "enroll contestant failed: {}", res.text);

        // READ admits the participant: they see the problem in the contest's
        // list and can open its statement by id. `GET /problems/{id}` is how the
        // contest UI loads a problem, and it reaches a hidden one only through
        // rule 9.
        assert!(
            visible_problem_ids(&app, cid, &contestant)
                .await
                .contains(&i64::from(pid)),
            "participant should see hidden problem {pid} in the contest problem list"
        );
        let res = app.get_with_token(&routes::problem(pid), &contestant).await;
        assert_eq!(res.status, 200, "standalone read via rule 9: {}", res.text);

        // SUBMIT is not: the standalone route would store a contest-less row.
        assert_standalone_submit_hidden(&app, pid, &contestant, "participant").await;

        // The contest route still accepts the same participant, scoped to the
        // contest.
        let res = app
            .post_with_token(
                &routes::contest_problem_submissions(cid, pid),
                &submission_body(None),
                &contestant,
            )
            .await;
        assert_eq!(res.status, 201, "contest-scoped submit: {}", res.text);
        assert_eq!(res.body["contest_id"], cid);

        // Rule 8 still applies: a problem editor can submit standalone to a
        // hidden problem (to test it before release).
        let res = app
            .post_with_token(
                &routes::problem_submissions(pid),
                &submission_body(None),
                &admin,
            )
            .await;
        assert_eq!(res.status, 201, "editor standalone submit: {}", res.text);
        assert!(res.body["contest_id"].is_null());
    }

    /// Any authenticated user can read a hidden problem in a public running contest, but
    /// an unregistered user cannot submit or run code on it through the standalone
    /// routes, since the contest route refuses them.
    #[tokio::test]
    async fn hidden_problem_in_public_running_contest_rejects_standalone_submission_from_unregistered_user()
     {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_hidden_public", "pass1234", "admin")
            .await;
        let outsider = app
            .create_authenticated_user("outsider_hidden_public", "pass1234")
            .await;

        let pid = app
            .create_hidden_problem(&admin, "Hidden Public-Contest Problem")
            .await;
        // Public, activated, started, and not yet ended.
        let cid = app
            .create_contest(&admin, "Public Running Contest", true, true)
            .await;
        app.add_problem_to_contest(cid, pid, &admin).await;

        // READ admits everyone: any authenticated user can view a hidden problem
        // attached to a public started contest (rule 9), with no registration.
        let res = app.get_with_token(&routes::problem(pid), &outsider).await;
        assert_eq!(res.status, 200, "standalone read via rule 9: {}", res.text);

        // The contest route refuses the unregistered user...
        let res = app
            .post_with_token(
                &routes::contest_problem_submissions(cid, pid),
                &submission_body(None),
                &outsider,
            )
            .await;
        assert_eq!(res.status, 403, "contest-scoped submit: {}", res.text);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");

        // ...so the standalone route must not be a way around it.
        assert_standalone_submit_hidden(&app, pid, &outsider, "unregistered user").await;
    }
}
