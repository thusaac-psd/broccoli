use serde_json::json;

use crate::common::E2eTestApp;

fn valid_contest_body(title: &str, is_public: bool) -> serde_json::Value {
    json!({
        "title": title,
        "description": "A contest description in **Markdown**.",
        "activate_time": "2020-01-01T00:00:00Z",
        "start_time": "2020-01-01T00:00:00Z",
        "end_time": "2099-01-02T00:00:00Z",
        "is_public": is_public,
        "contest_type": "standard",
    })
}

mod contest_creation {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn admin_can_create_a_contest() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("con_cr1", "password123", "admin")
            .await;

        let body = valid_contest_body("My Contest", false);
        let res = app.post_with_token("/api/v1/contests", &body, &token).await;

        assert_eq!(res.status, 201);
        assert_eq!(res.body["title"], "My Contest");
        assert_eq!(res.body["is_public"], false);
        assert!(res.body["id"].as_i64().is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn contestant_cannot_create_a_contest() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("con_cr2", "password123", "contestant")
            .await;

        let body = valid_contest_body("Nope", false);
        let res = app.post_with_token("/api/v1/contests", &body, &token).await;

        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_title_returns_validation_error() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("con_cr3", "password123", "admin")
            .await;

        let mut body = valid_contest_body("", false);
        body["title"] = json!("   ");
        let res = app.post_with_token("/api/v1/contests", &body, &token).await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn end_before_start_returns_validation_error() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("con_cr4", "password123", "admin")
            .await;

        let body = json!({
            "title": "Bad Times",
            "description": "desc",
            "start_time": "2099-01-02T00:00:00Z",
            "end_time": "2099-01-01T00:00:00Z",
            "is_public": false,
            "contest_type": app.contest_type,
        });
        let res = app.post_with_token("/api/v1/contests", &body, &token).await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_required_fields_returns_validation_error() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("con_cr5", "password123", "admin")
            .await;

        let body = json!({"title": "partial"});
        let res = app.post_with_token("/api/v1/contests", &body, &token).await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }
}

mod contest_retrieval {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn admin_can_get_contest_by_id() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("con_get1", "password123", "admin")
            .await;

        let cid = app.create_contest(&token, "Get Contest", true, false).await;

        let res = app
            .get_with_token(&format!("/api/v1/contests/{cid}"), &token)
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["id"], cid);
        assert_eq!(res.body["title"], "Get Contest");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn nonexistent_contest_returns_404() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("con_get2", "password123", "admin")
            .await;

        let res = app.get_with_token("/api/v1/contests/99999", &token).await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }
}

mod contest_listing {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn admin_sees_all_contests() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let admin = app
            .create_user_with_role("con_ls1", "password123", "admin")
            .await;

        app.create_contest(&admin, "Public Lst", true, false).await;
        app.create_contest(&admin, "Private Lst", false, false)
            .await;

        let res = app.get_with_token("/api/v1/contests", &admin).await;

        assert_eq!(res.status, 200);
        let data = res.body["data"].as_array().unwrap();
        assert!(data.len() >= 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn contestant_sees_only_public_and_enrolled_contests() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let admin = app
            .create_user_with_role("con_ls2", "password123", "admin")
            .await;
        let user = app
            .create_user_with_role("con_ls3", "password123", "contestant")
            .await;

        app.create_contest(&admin, "Public Vis", true, false).await;
        let private_id = app
            .create_contest(&admin, "Private Enrolled", false, false)
            .await;
        app.create_contest(&admin, "Private Hidden", false, false)
            .await;

        let user_id = app.get_with_token("/api/v1/auth/me", &user).await.id();
        app.post_with_token(
            &format!("/api/v1/contests/{private_id}/participants"),
            &json!({"user_id": user_id}),
            &admin,
        )
        .await;

        let res = app.get_with_token("/api/v1/contests", &user).await;
        assert_eq!(res.status, 200);
        let data = res.body["data"].as_array().unwrap();
        assert_eq!(data.len(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_filters_contests_by_title() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let admin = app
            .create_user_with_role("con_ls4", "password123", "admin")
            .await;

        app.create_contest(&admin, "Unique Zebra Contest", true, false)
            .await;
        app.create_contest(&admin, "Other Contest", true, false)
            .await;

        let res = app
            .get_with_token("/api/v1/contests?search=Unique+Zebra", &admin)
            .await;

        assert_eq!(res.status, 200);
        let data = res.body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["title"], "Unique Zebra Contest");
    }
}

mod contest_update {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn admin_can_update_contest() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("con_up1", "password123", "admin")
            .await;

        let cid = app
            .create_contest(&token, "Original Contest", true, false)
            .await;

        let res = app
            .patch_with_token(
                &format!("/api/v1/contests/{cid}"),
                &json!({"title": "Updated Contest"}),
                &token,
            )
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["title"], "Updated Contest");
    }
}

mod contest_deletion {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn admin_can_delete_a_contest() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("con_del1", "password123", "admin")
            .await;

        let cid = app.create_contest(&token, "To Delete", true, false).await;

        let del = app
            .delete_with_token(&format!("/api/v1/contests/{cid}"), &token)
            .await;
        assert_eq!(del.status, 204);

        let get = app
            .get_with_token(&format!("/api/v1/contests/{cid}"), &token)
            .await;
        assert_eq!(get.status, 404);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn contestant_cannot_delete_a_contest() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let admin = app
            .create_user_with_role("con_del2", "password123", "admin")
            .await;
        let user = app
            .create_user_with_role("con_del3", "password123", "contestant")
            .await;

        let cid = app
            .create_contest(&admin, "Protected Contest", true, false)
            .await;

        let res = app
            .delete_with_token(&format!("/api/v1/contests/{cid}"), &user)
            .await;

        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }
}

mod contest_problems {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn admin_can_add_problem_to_contest() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("con_cp1", "password123", "admin")
            .await;

        let cid = app.create_contest(&token, "CP Contest", true, false).await;
        let pid = app.create_problem(&token, "CP Problem").await;

        let res = app
            .post_with_token(
                &format!("/api/v1/contests/{cid}/problems"),
                &json!({"problem_id": pid, "label": "A"}),
                &token,
            )
            .await;

        assert_eq!(res.status, 201);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn admin_can_list_contest_problems() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("con_cp2", "password123", "admin")
            .await;

        let cid = app
            .create_contest(&token, "CP List Contest", true, false)
            .await;
        let p1 = app.create_problem(&token, "CP P1").await;
        let p2 = app.create_problem(&token, "CP P2").await;
        app.add_problem_to_contest_with_label(cid, p1, "A", &token)
            .await;
        app.add_problem_to_contest_with_label(cid, p2, "B", &token)
            .await;

        let res = app
            .get_with_token(&format!("/api/v1/contests/{cid}/problems"), &token)
            .await;

        assert_eq!(res.status, 200);
        let items = res.body.as_array().unwrap();
        assert_eq!(items.len(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn admin_can_reorder_contest_problems() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("con_cp3", "password123", "admin")
            .await;

        let cid = app
            .create_contest(&token, "Reorder Contest", true, false)
            .await;
        let p1 = app.create_problem(&token, "Reord P1").await;
        let p2 = app.create_problem(&token, "Reord P2").await;
        let p3 = app.create_problem(&token, "Reord P3").await;
        app.add_problem_to_contest_with_label(cid, p1, "A", &token)
            .await;
        app.add_problem_to_contest_with_label(cid, p2, "B", &token)
            .await;
        app.add_problem_to_contest_with_label(cid, p3, "C", &token)
            .await;

        let res = app
            .put_with_token(
                &format!("/api/v1/contests/{cid}/problems/reorder"),
                &json!({"problem_ids": [p3, p1, p2]}),
                &token,
            )
            .await;

        assert_eq!(res.status, 204);

        let list = app
            .get_with_token(&format!("/api/v1/contests/{cid}/problems"), &token)
            .await;
        let items = list.body.as_array().unwrap();
        assert_eq!(items[0]["problem_id"], p3);
        assert_eq!(items[1]["problem_id"], p1);
        assert_eq!(items[2]["problem_id"], p2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn admin_can_remove_problem_from_contest() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("con_cp4", "password123", "admin")
            .await;

        let cid = app
            .create_contest(&token, "Remove CP Contest", true, false)
            .await;
        let pid = app.create_problem(&token, "Remove CP Problem").await;
        app.add_problem_to_contest(cid, pid, &token).await;

        let del = app
            .delete_with_token(&format!("/api/v1/contests/{cid}/problems/{pid}"), &token)
            .await;
        assert_eq!(del.status, 204);

        let list = app
            .get_with_token(&format!("/api/v1/contests/{cid}/problems"), &token)
            .await;
        let items = list.body.as_array().unwrap();
        assert_eq!(items.len(), 0);
    }
}

mod contest_participants {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn admin_can_add_participant() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let admin = app
            .create_user_with_role("con_part1", "password123", "admin")
            .await;
        let user = app
            .create_authenticated_user("con_part2", "password123")
            .await;

        let cid = app
            .create_contest(&admin, "Participant Contest", true, false)
            .await;
        let user_id = app.get_with_token("/api/v1/auth/me", &user).await.id();

        let res = app
            .post_with_token(
                &format!("/api/v1/contests/{cid}/participants"),
                &json!({"user_id": user_id}),
                &admin,
            )
            .await;

        assert_eq!(res.status, 201);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn admin_can_list_participants() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let admin = app
            .create_user_with_role("con_part3", "password123", "admin")
            .await;
        let user = app
            .create_authenticated_user("con_part4", "password123")
            .await;

        let cid = app
            .create_contest(&admin, "List Parts Contest", true, false)
            .await;
        let user_id = app.get_with_token("/api/v1/auth/me", &user).await.id();
        app.post_with_token(
            &format!("/api/v1/contests/{cid}/participants"),
            &json!({"user_id": user_id}),
            &admin,
        )
        .await;

        let res = app
            .get_with_token(&format!("/api/v1/contests/{cid}/participants"), &admin)
            .await;

        assert_eq!(res.status, 200);
        let items = res.body.as_array().unwrap();
        assert!(!items.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn admin_can_remove_participant() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let admin = app
            .create_user_with_role("con_part5", "password123", "admin")
            .await;
        let user = app
            .create_authenticated_user("con_part6", "password123")
            .await;

        let cid = app
            .create_contest(&admin, "Remove Part Contest", true, false)
            .await;
        let user_id = app.get_with_token("/api/v1/auth/me", &user).await.id();
        app.post_with_token(
            &format!("/api/v1/contests/{cid}/participants"),
            &json!({"user_id": user_id}),
            &admin,
        )
        .await;

        let del = app
            .delete_with_token(
                &format!("/api/v1/contests/{cid}/participants/{user_id}"),
                &admin,
            )
            .await;

        assert_eq!(del.status, 204);
    }
}

mod self_registration {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn user_can_self_register_for_public_contest() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let admin = app
            .create_user_with_role("con_sr1", "password123", "admin")
            .await;
        let user = app
            .create_authenticated_user("con_sr2", "password123")
            .await;

        let cid = app
            .create_contest(&admin, "Self Reg Contest", true, false)
            .await;

        let res = app
            .post_with_token(
                &format!("/api/v1/contests/{cid}/register"),
                &json!({}),
                &user,
            )
            .await;

        assert_eq!(res.status, 201);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn user_can_unregister_from_contest() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let admin = app
            .create_user_with_role("con_sr3", "password123", "admin")
            .await;
        let user = app
            .create_authenticated_user("con_sr4", "password123")
            .await;

        let cid = app
            .create_contest(&admin, "Unreg Contest", true, false)
            .await;
        // Unregistering is only allowed before the contest starts.
        let res = app
            .patch_with_token(
                &format!("/api/v1/contests/{cid}"),
                &serde_json::json!({"start_time": "2098-01-01T00:00:00Z"}),
                &admin,
            )
            .await;
        assert_eq!(res.status, 200, "{}", res.text);
        app.register_for_contest(cid, &user).await;

        let res = app
            .delete_with_token(&format!("/api/v1/contests/{cid}/register"), &user)
            .await;

        assert_eq!(res.status, 204);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn private_contest_not_visible_to_non_participant() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let admin = app
            .create_user_with_role("con_sr5", "password123", "admin")
            .await;
        let user = app
            .create_authenticated_user("con_sr6", "password123")
            .await;

        let cid = app
            .create_contest(&admin, "Invisible Contest", false, false)
            .await;

        let res = app
            .get_with_token(&format!("/api/v1/contests/{cid}"), &user)
            .await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }
}
