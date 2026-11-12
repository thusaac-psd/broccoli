use serde_json::json;

use crate::common::E2eTestApp;

mod submission_creation {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn user_can_create_a_submission() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let admin = app
            .create_user_with_role("sub_cr1", "password123", "admin")
            .await;
        let user = app
            .create_authenticated_user("sub_cr2", "password123")
            .await;

        let pid = app.create_public_problem(&admin, "Submit Problem").await;

        let res = app
            .post_with_token(
                &format!("/api/v1/problems/{pid}/submissions"),
                &json!({
                    "files": [{"filename": "main.cpp", "content": "#include <iostream>\nint main() {}"}],
                    "language": "cpp",
                }),
                &user,
            )
            .await;

        assert_eq!(res.status, 201);
        assert!(res.body["id"].is_number());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_files_returns_validation_error() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let admin = app
            .create_user_with_role("sub_cr3", "password123", "admin")
            .await;
        let user = app
            .create_authenticated_user("sub_cr4", "password123")
            .await;

        let pid = app.create_problem(&admin, "Empty Files Problem").await;

        let res = app
            .post_with_token(
                &format!("/api/v1/problems/{pid}/submissions"),
                &json!({
                    "files": [],
                    "language": "cpp",
                }),
                &user,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_language_returns_validation_error() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let admin = app
            .create_user_with_role("sub_cr5", "password123", "admin")
            .await;
        let user = app
            .create_authenticated_user("sub_cr6", "password123")
            .await;

        let pid = app.create_problem(&admin, "No Lang Problem").await;

        let res = app
            .post_with_token(
                &format!("/api/v1/problems/{pid}/submissions"),
                &json!({
                    "files": [{"filename": "main.cpp", "content": "int main() {}"}],
                    "language": "   ",
                }),
                &user,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_file_content_returns_validation_error() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let admin = app
            .create_user_with_role("sub_cr7", "password123", "admin")
            .await;
        let user = app
            .create_authenticated_user("sub_cr8", "password123")
            .await;

        let pid = app.create_problem(&admin, "Empty Content Problem").await;

        let res = app
            .post_with_token(
                &format!("/api/v1/problems/{pid}/submissions"),
                &json!({
                    "files": [{"filename": "main.cpp", "content": ""}],
                    "language": "cpp",
                }),
                &user,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn path_traversal_in_filename_returns_validation_error() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let admin = app
            .create_user_with_role("sub_cr9", "password123", "admin")
            .await;
        let user = app
            .create_authenticated_user("sub_cr10", "password123")
            .await;

        let pid = app.create_problem(&admin, "Traversal Problem").await;

        let res = app
            .post_with_token(
                &format!("/api/v1/problems/{pid}/submissions"),
                &json!({
                    "files": [{"filename": "../etc/passwd", "content": "int main() {}"}],
                    "language": "cpp",
                }),
                &user,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unauthenticated_user_cannot_submit() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let admin = app
            .create_user_with_role("sub_cr11", "password123", "admin")
            .await;

        let pid = app.create_problem(&admin, "No Auth Submit").await;

        let res = app
            .post_without_token(
                &format!("/api/v1/problems/{pid}/submissions"),
                &json!({
                    "files": [{"filename": "main.cpp", "content": "int main() {}"}],
                    "language": "cpp",
                }),
            )
            .await;

        assert_eq!(res.status, 401);
    }
}

mod standalone_problem_visibility {
    use super::*;

    /// IDOR guard: a non-public problem in zero contests must stay 404 for an
    /// unprivileged user - both reading it and submitting to it - while an
    /// explicitly published problem is reachable. Submitting against hidden
    /// problems would be a stronger oracle than viewing since it runs secret
    /// tests.
    #[tokio::test(flavor = "multi_thread")]
    async fn hidden_standalone_problem_is_404_for_plain_user() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let admin = app
            .create_user_with_role("sub_vis1", "password123", "admin")
            .await;
        let user = app
            .create_authenticated_user("sub_vis2", "password123")
            .await;

        let hidden_pid = app.create_problem(&admin, "Hidden Draft Problem").await;
        let public_pid = app.create_public_problem(&admin, "Published Problem").await;

        let payload = json!({
            "files": [{"filename": "main.cpp", "content": "int main() {}"}],
            "language": "cpp",
        });

        // Hidden draft: not readable, not submittable - indistinguishable
        // from a nonexistent problem.
        let get_hidden = app
            .get_with_token(&format!("/api/v1/problems/{hidden_pid}"), &user)
            .await;
        assert_eq!(
            get_hidden.status, 404,
            "hidden problem must not be readable: {}",
            get_hidden.text
        );
        assert_eq!(get_hidden.body["code"], "NOT_FOUND");

        let submit_hidden = app
            .post_with_token(
                &format!("/api/v1/problems/{hidden_pid}/submissions"),
                &payload,
                &user,
            )
            .await;
        assert_eq!(
            submit_hidden.status, 404,
            "hidden problem must not accept submissions: {}",
            submit_hidden.text
        );
        assert_eq!(submit_hidden.body["code"], "NOT_FOUND");

        // Published problem: same user, same payload - readable and
        // submittable.
        let get_public = app
            .get_with_token(&format!("/api/v1/problems/{public_pid}"), &user)
            .await;
        assert_eq!(
            get_public.status, 200,
            "public problem must be readable: {}",
            get_public.text
        );

        let submit_public = app
            .post_with_token(
                &format!("/api/v1/problems/{public_pid}/submissions"),
                &payload,
                &user,
            )
            .await;
        assert_eq!(
            submit_public.status, 201,
            "public problem must accept submissions: {}",
            submit_public.text
        );
    }
}

mod submission_retrieval {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn owner_can_get_their_submission() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let admin = app
            .create_user_with_role("sub_get1", "password123", "admin")
            .await;
        let user = app
            .create_authenticated_user("sub_get2", "password123")
            .await;

        let pid = app.create_public_problem(&admin, "Get Sub Problem").await;
        let sid = app
            .create_submission(pid, &user, "cpp", "int main() {}")
            .await;

        let res = app
            .get_with_token(&format!("/api/v1/submissions/{sid}"), &user)
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["id"], sid);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn non_owner_cannot_see_submission() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let admin = app
            .create_user_with_role("sub_get3", "password123", "admin")
            .await;
        let user1 = app
            .create_authenticated_user("sub_get4", "password123")
            .await;
        let user2 = app
            .create_authenticated_user("sub_get5", "password123")
            .await;

        // Public so user1 can submit; the 404 below asserts submission
        // ownership, which is independent of problem visibility.
        let pid = app
            .create_public_problem(&admin, "Hidden Sub Problem")
            .await;
        let sid = app
            .create_submission(pid, &user1, "cpp", "int main() {}")
            .await;

        let res = app
            .get_with_token(&format!("/api/v1/submissions/{sid}"), &user2)
            .await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn admin_can_see_any_submission() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let admin = app
            .create_user_with_role("sub_get6", "password123", "admin")
            .await;
        let user = app
            .create_authenticated_user("sub_get7", "password123")
            .await;

        let pid = app
            .create_public_problem(&admin, "Admin View Problem")
            .await;
        let sid = app
            .create_submission(pid, &user, "cpp", "int main() {}")
            .await;

        let res = app
            .get_with_token(&format!("/api/v1/submissions/{sid}"), &admin)
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["id"], sid);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn nonexistent_submission_returns_404() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let user = app
            .create_authenticated_user("sub_get8", "password123")
            .await;

        let res = app.get_with_token("/api/v1/submissions/99999", &user).await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }
}

mod submission_listing {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn user_sees_only_own_submissions() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let admin = app
            .create_user_with_role("sub_ls1", "password123", "admin")
            .await;
        let user1 = app
            .create_authenticated_user("sub_ls2", "password123")
            .await;
        let user2 = app
            .create_authenticated_user("sub_ls3", "password123")
            .await;

        let pid = app.create_public_problem(&admin, "List Sub Problem").await;
        app.create_submission(pid, &user1, "cpp", "int main() { return 0; }")
            .await;
        app.create_submission(pid, &user2, "cpp", "int main() { return 1; }")
            .await;

        let res = app.get_with_token("/api/v1/submissions", &user1).await;

        assert_eq!(res.status, 200);
        let data = res.body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn admin_sees_all_submissions() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let admin = app
            .create_user_with_role("sub_ls4", "password123", "admin")
            .await;
        let user = app
            .create_authenticated_user("sub_ls5", "password123")
            .await;

        let pid = app
            .create_public_problem(&admin, "Admin List Problem")
            .await;
        app.create_submission(pid, &user, "cpp", "int main() { return 0; }")
            .await;
        app.create_submission(pid, &admin, "cpp", "int main() { return 1; }")
            .await;

        let res = app.get_with_token("/api/v1/submissions", &admin).await;

        assert_eq!(res.status, 200);
        let data = res.body["data"].as_array().unwrap();
        assert!(data.len() >= 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn filter_by_problem_id() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let admin = app
            .create_user_with_role("sub_ls6", "password123", "admin")
            .await;

        let p1 = app.create_problem(&admin, "Filter P1").await;
        let p2 = app.create_problem(&admin, "Filter P2").await;
        app.create_submission(p1, &admin, "cpp", "int main() { return 0; }")
            .await;
        app.create_submission(p2, &admin, "cpp", "int main() { return 1; }")
            .await;

        let res = app
            .get_with_token(&format!("/api/v1/submissions?problem_id={p1}"), &admin)
            .await;

        assert_eq!(res.status, 200);
        let data = res.body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["problem_id"], p1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn filter_by_language() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let admin = app
            .create_user_with_role("sub_ls7", "password123", "admin")
            .await;

        let pid = app.create_problem(&admin, "Filter Lang Problem").await;
        app.create_submission(pid, &admin, "cpp", "int main() { return 0; }")
            .await;
        app.create_submission(pid, &admin, "python3", "print('hello')")
            .await;

        let res = app
            .get_with_token("/api/v1/submissions?language=cpp", &admin)
            .await;

        assert_eq!(res.status, 200);
        let data = res.body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["language"], "cpp");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pagination_works() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let admin = app
            .create_user_with_role("sub_ls8", "password123", "admin")
            .await;

        let pid = app.create_problem(&admin, "Paginate Problem").await;
        for _ in 0..3 {
            app.create_submission(pid, &admin, "cpp", "int main() { return 0; }")
                .await;
        }

        let res = app
            .get_with_token("/api/v1/submissions?per_page=2", &admin)
            .await;

        assert_eq!(res.status, 200);
        let data = res.body["data"].as_array().unwrap();
        assert_eq!(data.len(), 2);
        assert!(res.body["pagination"]["total"].as_i64().unwrap() >= 3);
        let cursor = res.body["pagination"]["next_cursor"]
            .as_str()
            .expect("a third submission is on the next page");

        let next = app
            .get_with_token(
                &format!("/api/v1/submissions?per_page=2&cursor={cursor}"),
                &admin,
            )
            .await;
        assert_eq!(next.status, 200);
        let next_data = next.body["data"].as_array().unwrap();
        assert!(!next_data.is_empty());
        assert!(
            next_data.iter().all(|s| !data.contains(s)),
            "the next page continues after the first"
        );
    }
}
