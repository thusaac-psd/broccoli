use serde_json::json;

use crate::common::E2eTestApp;

mod problem_creation {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn admin_can_create_a_problem() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("prob_cr1", "password123", "admin")
            .await;

        let res = app
            .post_with_token(
                "/api/v1/problems",
                &json!({
                    "title": "Two Sum",
                    "content": "Find two numbers that sum to target.",
                    "time_limit": 1000,
                    "memory_limit": 262144,
                    "problem_type": "batch",
                    "default_contest_type": app.contest_type,
                    "checker_format": "exact",
                }),
                &token,
            )
            .await;

        assert_eq!(res.status, 201);
        assert_eq!(res.body["title"], "Two Sum");
        assert!(res.body["id"].is_number());
        assert!(res.body["created_at"].is_string());
        assert!(res.body["updated_at"].is_string());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn contestant_cannot_create_a_problem() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("prob_cr2", "password123", "contestant")
            .await;

        let res = app
            .post_with_token(
                "/api/v1/problems",
                &json!({
                    "title": "Forbidden",
                    "content": "desc",
                    "time_limit": 1000,
                    "memory_limit": 262144,
                    "problem_type": "batch",
                    "default_contest_type": app.contest_type,
                    "checker_format": "exact",
                }),
                &token,
            )
            .await;

        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_title_returns_validation_error() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("prob_cr3", "password123", "admin")
            .await;

        let res = app
            .post_with_token(
                "/api/v1/problems",
                &json!({
                    "title": "   ",
                    "content": "desc",
                    "time_limit": 1000,
                    "memory_limit": 262144,
                    "problem_type": "batch",
                    "default_contest_type": app.contest_type,
                    "checker_format": "exact",
                }),
                &token,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unauthenticated_user_cannot_create_a_problem() {
        let app = E2eTestApp::spawn_without_plugins().await;

        let res = app
            .post_without_token(
                "/api/v1/problems",
                &json!({
                    "title": "Nope",
                    "content": "desc",
                    "time_limit": 1000,
                    "memory_limit": 262144,
                    "problem_type": "batch",
                    "default_contest_type": app.contest_type,
                    "checker_format": "exact",
                }),
            )
            .await;

        assert_eq!(res.status, 401);
    }
}

mod problem_retrieval {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn admin_can_get_problem_by_id() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("prob_get1", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Get Me").await;

        let res = app
            .get_with_token(&format!("/api/v1/problems/{pid}"), &token)
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["id"], pid);
        assert_eq!(res.body["title"], "Get Me");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn nonexistent_problem_returns_404() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("prob_get2", "password123", "admin")
            .await;

        let res = app.get_with_token("/api/v1/problems/99999", &token).await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }
}

mod problem_listing {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn admin_can_list_problems() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("prob_ls1", "password123", "admin")
            .await;

        app.create_problem(&token, "Problem A").await;
        app.create_problem(&token, "Problem B").await;

        let res = app.get_with_token("/api/v1/problems", &token).await;

        assert_eq!(res.status, 200);
        let data = res.body["data"].as_array().unwrap();
        assert!(data.len() >= 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_filters_problems_by_title() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("prob_ls2", "password123", "admin")
            .await;

        app.create_problem(&token, "Unique Alpha Search").await;
        app.create_problem(&token, "Other Problem").await;

        let res = app
            .get_with_token("/api/v1/problems?search=Unique+Alpha", &token)
            .await;

        assert_eq!(res.status, 200);
        let data = res.body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["title"], "Unique Alpha Search");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pagination_works() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("prob_ls3", "password123", "admin")
            .await;

        for i in 0..3 {
            app.create_problem(&token, &format!("Page Problem {i}"))
                .await;
        }

        let res = app
            .get_with_token("/api/v1/problems?page=1&per_page=2", &token)
            .await;

        assert_eq!(res.status, 200);
        let data = res.body["data"].as_array().unwrap();
        assert_eq!(data.len(), 2);
        assert!(res.body["pagination"]["total"].as_i64().unwrap() >= 3);
    }
}

mod problem_update {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn admin_can_update_problem_title() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("prob_up1", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Original Title").await;

        let res = app
            .patch_with_token(
                &format!("/api/v1/problems/{pid}"),
                &json!({"title": "Updated Title"}),
                &token,
            )
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["title"], "Updated Title");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_patch_returns_current_resource() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("prob_up2", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "No Change").await;

        let res = app
            .patch_with_token(&format!("/api/v1/problems/{pid}"), &json!({}), &token)
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["title"], "No Change");
    }
}

mod problem_deletion {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn admin_can_delete_a_problem() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("prob_del1", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "To Delete").await;

        let del = app
            .delete_with_token(&format!("/api/v1/problems/{pid}"), &token)
            .await;
        assert_eq!(del.status, 204);

        let get = app
            .get_with_token(&format!("/api/v1/problems/{pid}"), &token)
            .await;
        assert_eq!(get.status, 404);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn contestant_cannot_delete_a_problem() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let admin = app
            .create_user_with_role("prob_del2", "password123", "admin")
            .await;
        let user = app
            .create_user_with_role("prob_del3", "password123", "contestant")
            .await;

        let pid = app.create_problem(&admin, "Protected").await;

        let res = app
            .delete_with_token(&format!("/api/v1/problems/{pid}"), &user)
            .await;

        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }
}

mod test_case_crud {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn admin_can_create_a_test_case() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("prob_tc1", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "TC Problem").await;

        let res = app
            .post_with_token(
                &format!("/api/v1/problems/{pid}/test-cases"),
                &json!({
                    "input": "1 2 3",
                    "expected_output": "6",
                    "score": 10,
                    "is_sample": true,
                }),
                &token,
            )
            .await;

        assert_eq!(res.status, 201);
        assert!(res.body["id"].is_number());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn admin_can_list_test_cases() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("prob_tc2", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "TC List Problem").await;
        app.create_test_case(pid, &token).await;
        app.create_test_case_with(pid, "10\n1 2", "3", 20, false, &token)
            .await;

        let res = app
            .get_with_token(&format!("/api/v1/problems/{pid}/test-cases"), &token)
            .await;

        assert_eq!(res.status, 200);
        let items = res.body.as_array().unwrap();
        assert_eq!(items.len(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn admin_can_get_single_test_case() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("prob_tc3", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "TC Get Problem").await;
        let tc_id = app.create_test_case(pid, &token).await;

        let res = app
            .get_with_token(
                &format!("/api/v1/problems/{pid}/test-cases/{tc_id}"),
                &token,
            )
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["id"], tc_id);
        assert_eq!(res.body["input"], "5\n1 2 3 4 5");
        assert_eq!(res.body["expected_output"], "15");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn admin_can_update_test_case() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("prob_tc4", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "TC Update Problem").await;
        let tc_id = app.create_test_case(pid, &token).await;

        let res = app
            .patch_with_token(
                &format!("/api/v1/problems/{pid}/test-cases/{tc_id}"),
                &json!({"input": "new input", "expected_output": "new output"}),
                &token,
            )
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["input"], "new input");
        assert_eq!(res.body["expected_output"], "new output");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn admin_can_delete_test_case() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("prob_tc5", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "TC Delete Problem").await;
        let tc_id = app.create_test_case(pid, &token).await;

        let del = app
            .delete_with_token(
                &format!("/api/v1/problems/{pid}/test-cases/{tc_id}"),
                &token,
            )
            .await;
        assert_eq!(del.status, 204);

        let get = app
            .get_with_token(
                &format!("/api/v1/problems/{pid}/test-cases/{tc_id}"),
                &token,
            )
            .await;
        assert_eq!(get.status, 404);
    }
}

mod test_case_reorder {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn admin_can_reorder_test_cases() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("prob_reord1", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Reorder Problem").await;
        let tc1 = app
            .create_test_case_with(pid, "a", "1", 10, false, &token)
            .await;
        let tc2 = app
            .create_test_case_with(pid, "b", "2", 20, false, &token)
            .await;
        let tc3 = app
            .create_test_case_with(pid, "c", "3", 30, false, &token)
            .await;

        let res = app
            .put_with_token(
                &format!("/api/v1/problems/{pid}/test-cases/reorder"),
                &json!({"test_case_ids": [tc3, tc1, tc2]}),
                &token,
            )
            .await;

        assert_eq!(res.status, 204);

        let list = app
            .get_with_token(&format!("/api/v1/problems/{pid}/test-cases"), &token)
            .await;
        assert_eq!(list.status, 200);
        let items = list.body.as_array().unwrap();
        assert_eq!(items[0]["id"], tc3);
        assert_eq!(items[1]["id"], tc1);
        assert_eq!(items[2]["id"], tc2);
    }
}
