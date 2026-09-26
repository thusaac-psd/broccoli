use crate::common::{TestApp, routes};
use serde_json::json;

fn valid_contest_body(title: &str, is_public: bool) -> serde_json::Value {
    json!({
        "title": title,
        "description": "A contest description in **Markdown**.",
        "activate_time": "2020-01-01T00:00:00Z",
        "start_time": "2020-01-01T00:00:00Z",
        "end_time": "2099-01-02T00:00:00Z",
        "deactivate_time": null,
        "is_public": is_public,
    })
}

async fn create_contest_as_admin(
    app: &TestApp,
    admin_token: &str,
    title: &str,
    is_public: bool,
) -> i32 {
    let body = valid_contest_body(title, is_public);
    let res = app
        .post_with_token(routes::CONTESTS, &body, admin_token)
        .await;
    assert_eq!(res.status, 201, "create_contest failed: {}", res.text);
    res.id()
}

mod contest_creation {
    use super::*;

    #[tokio::test]
    async fn admin_can_create_a_contest() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;

        let body = valid_contest_body("My Contest", false);
        let res = app.post_with_token(routes::CONTESTS, &body, &token).await;

        assert_eq!(res.status, 201);
        assert_eq!(res.body["title"], "My Contest");
        assert_eq!(res.body["is_public"], false);
        assert!(res.body["id"].as_i64().is_some());
    }

    #[tokio::test]
    async fn creates_contest_without_activate_time() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;

        let mut body = valid_contest_body("No Activate", true);
        body.as_object_mut().unwrap().remove("activate_time");
        let res = app.post_with_token(routes::CONTESTS, &body, &token).await;

        assert_eq!(res.status, 201);
        assert!(res.body["activate_time"].is_null());
    }

    #[tokio::test]
    async fn creates_contest_with_is_public_true() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;

        let body = valid_contest_body("Public Contest", true);
        let res = app.post_with_token(routes::CONTESTS, &body, &token).await;

        assert_eq!(res.status, 201);
        assert_eq!(res.body["is_public"], true);
    }

    #[tokio::test]
    async fn returns_validation_error_for_empty_title() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;

        let mut body = valid_contest_body("", false);
        body["title"] = json!("   ");
        let res = app.post_with_token(routes::CONTESTS, &body, &token).await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn returns_validation_error_when_activate_after_start() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin", "pass1234", "admin")
            .await;

        let mut body = valid_contest_body("Bad Activate", true);
        body["activate_time"] = json!("2025-01-02T00:00:00Z");
        body["start_time"] = json!("2025-01-01T00:00:00Z");

        let res = app.post_with_token(routes::CONTESTS, &body, &token).await;
        assert_eq!(res.status, 400);
        assert!(res.text.contains("activate_time"));
    }

    #[tokio::test]
    async fn returns_validation_error_when_end_before_start() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;

        let body = json!({
            "title": "Bad Times",
            "description": "desc",
            "start_time": "2099-01-02T00:00:00Z",
            "end_time": "2099-01-01T00:00:00Z",
            "is_public": false,
        });
        let res = app.post_with_token(routes::CONTESTS, &body, &token).await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn returns_validation_error_when_deactivate_before_end() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin", "pass1234", "admin")
            .await;

        let mut body = valid_contest_body("Bad Deactivate", true);
        body["end_time"] = json!("2025-01-02T00:00:00Z");
        body["deactivate_time"] = json!("2025-01-01T00:00:00Z");

        let res = app.post_with_token(routes::CONTESTS, &body, &token).await;
        assert_eq!(res.status, 400);
        assert!(res.text.contains("deactivate_time"));
    }

    #[tokio::test]
    async fn contestant_cannot_create_a_contest() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;

        let body = valid_contest_body("Nope", false);
        let res = app.post_with_token(routes::CONTESTS, &body, &token).await;

        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn unauthenticated_user_cannot_create_a_contest() {
        let app = TestApp::spawn().await;

        let body = valid_contest_body("Nope", false);
        let res = app.post_without_token(routes::CONTESTS, &body).await;

        assert_eq!(res.status, 401);
    }

    #[tokio::test]
    async fn returns_validation_error_for_missing_fields() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;

        let body = json!({"title": "partial"});
        let res = app.post_with_token(routes::CONTESTS, &body, &token).await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn rejects_empty_description_on_create_contest() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;

        let body = json!({
            "title": "Valid Title",
            "description": "   ",
            "start_time": "2099-01-01T00:00:00Z",
            "end_time": "2099-01-02T00:00:00Z",
            "is_public": false,
        });
        let res = app.post_with_token(routes::CONTESTS, &body, &token).await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn trims_title_on_create_contest() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;

        let body = json!({
            "title": "  My Contest  ",
            "description": "A contest description.",
            "start_time": "2099-01-01T00:00:00Z",
            "end_time": "2099-01-02T00:00:00Z",
            "is_public": false,
        });
        let res = app.post_with_token(routes::CONTESTS, &body, &token).await;

        assert_eq!(res.status, 201);
        assert_eq!(res.body["title"], "My Contest");
    }
}

mod contest_listing {
    use super::*;

    #[tokio::test]
    async fn admin_sees_all_contests() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;

        create_contest_as_admin(&app, &admin, "Public One", true).await;

        create_contest_as_admin(&app, &admin, "Private One", false).await;

        let never_active_body = json!({
            "title": "Never Active",
            "description": "desc",
            "activate_time": null,
            "start_time": "2020-01-02T00:00:00Z",
            "end_time": "2099-01-03T00:00:00Z",
            "is_public": true,
        });
        app.post_with_token(routes::CONTESTS, &never_active_body, &admin)
            .await;

        let future_body = json!({
            "title": "Future Contest",
            "description": "desc",
            "activate_time": "2099-01-01T00:00:00Z",
            "start_time": "2099-01-02T00:00:00Z",
            "end_time": "2099-01-03T00:00:00Z",
            "is_public": true,
        });
        app.post_with_token(routes::CONTESTS, &future_body, &admin)
            .await;

        let deactivated_body = json!({
            "title": "Deactivated Contest",
            "description": "desc",
            "activate_time": "2020-01-01T00:00:00Z",
            "start_time": "2020-01-02T00:00:00Z",
            "end_time": "2020-01-03T00:00:00Z",
            "deactivate_time": "2020-01-04T00:00:00Z",
            "is_public": true,
        });
        app.post_with_token(routes::CONTESTS, &deactivated_body, &admin)
            .await;

        let res = app.get_with_token(routes::CONTESTS, &admin).await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["data"].as_array().unwrap().len(), 5);
    }

    #[tokio::test]
    async fn contestant_sees_only_public_and_enrolled_contests() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;

        create_contest_as_admin(&app, &admin, "Public", true).await;
        let private_id = create_contest_as_admin(&app, &admin, "Private Enrolled", false).await;
        create_contest_as_admin(&app, &admin, "Private Hidden", false).await;

        let user_id = app.get_with_token(routes::ME, &user).await.id();
        let enroll_body = json!({"user_id": user_id});
        let enroll_res = app
            .post_with_token(
                &routes::contest_participants(private_id),
                &enroll_body,
                &admin,
            )
            .await;
        assert_eq!(enroll_res.status, 201);

        let res = app.get_with_token(routes::CONTESTS, &user).await;
        assert_eq!(res.status, 200);
        let data = res.body["data"].as_array().unwrap();
        assert_eq!(data.len(), 2);
    }

    #[tokio::test]
    async fn contestant_does_not_see_never_activating_contests() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user", "pass1234", "contestant")
            .await;

        let body = json!({
            "title": "Never Active",
            "description": "desc",
            "activate_time": null,
            "start_time": "2020-01-02T00:00:00Z",
            "end_time": "2099-01-03T00:00:00Z",
            "is_public": true,
        });
        app.post_with_token(routes::CONTESTS, &body, &admin).await;

        let res = app.get_with_token(routes::CONTESTS, &user).await;
        let data = res.body["data"].as_array().unwrap();
        assert_eq!(data.len(), 0);
    }

    #[tokio::test]
    async fn contestant_does_not_see_not_yet_activated_contests() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user", "pass1234", "contestant")
            .await;

        let body = json!({
            "title": "Invisible",
            "description": "desc",
            "activate_time": "2099-01-01T00:00:00Z",
            "start_time": "2099-01-02T00:00:00Z",
            "end_time": "2099-01-03T00:00:00Z",
            "is_public": true,
        });
        app.post_with_token(routes::CONTESTS, &body, &admin).await;

        let res = app.get_with_token(routes::CONTESTS, &user).await;
        let data = res.body["data"].as_array().unwrap();
        assert_eq!(data.len(), 0);
    }

    #[tokio::test]
    async fn contestant_does_not_see_deactivated_contests() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user", "pass1234", "contestant")
            .await;

        let body = json!({
            "title": "Archived",
            "description": "desc",
            "activate_time": "2020-01-01T00:00:00Z",
            "start_time": "2020-01-02T00:00:00Z",
            "end_time": "2020-01-03T00:00:00Z",
            "deactivate_time": "2020-01-04T00:00:00Z",
            "is_public": true,
        });
        app.post_with_token(routes::CONTESTS, &body, &admin).await;

        let res = app.get_with_token(routes::CONTESTS, &user).await;
        let data = res.body["data"].as_array().unwrap();
        assert_eq!(data.len(), 0);
    }

    #[tokio::test]
    async fn supports_pagination() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;

        for i in 0..5 {
            create_contest_as_admin(&app, &admin, &format!("Contest {i}"), true).await;
        }

        let res = app
            .get_with_token(&format!("{}?per_page=2&page=1", routes::CONTESTS), &admin)
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["data"].as_array().unwrap().len(), 2);
        assert_eq!(res.body["pagination"]["total"], 5);
        assert_eq!(res.body["pagination"]["total_pages"], 3);
    }

    #[tokio::test]
    async fn supports_search_by_title() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;

        create_contest_as_admin(&app, &admin, "Alpha Contest", true).await;
        create_contest_as_admin(&app, &admin, "Beta Challenge", true).await;

        let res = app
            .get_with_token(&format!("{}?search=alpha", routes::CONTESTS), &admin)
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["data"].as_array().unwrap().len(), 1);
        assert_eq!(res.body["data"][0]["title"], "Alpha Contest");
    }

    #[tokio::test]
    async fn supports_sorting_by_activate_time() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;

        let body_early = json!({
            "title": "Early",
            "description": "desc",
            "activate_time": "2020-01-01T00:00:00Z",
            "start_time": "2020-01-02T00:00:00Z",
            "end_time": "2099-01-03T00:00:00Z",
            "is_public": true,
        });
        app.post_with_token(routes::CONTESTS, &body_early, &admin)
            .await;

        let body_late = json!({
            "title": "Late",
            "description": "desc",
            "activate_time": "2099-01-01T00:00:00Z",
            "start_time": "2099-01-02T00:00:00Z",
            "end_time": "2099-01-03T00:00:00Z",
            "is_public": true,
        });
        app.post_with_token(routes::CONTESTS, &body_late, &admin)
            .await;

        let body_never = json!({
            "title": "Never",
            "description": "desc",
            "activate_time": null,
            "start_time": "2020-01-02T00:00:00Z",
            "end_time": "2099-01-03T00:00:00Z",
            "is_public": true,
        });
        app.post_with_token(routes::CONTESTS, &body_never, &admin)
            .await;

        let res = app
            .get_with_token(
                &format!("{}?sort_by=activate_time&sort_order=asc", routes::CONTESTS),
                &admin,
            )
            .await;
        assert_eq!(res.status, 200);
        let data = res.body["data"].as_array().unwrap();
        assert_eq!(data[0]["title"], "Early");
        assert_eq!(data[1]["title"], "Late");
        assert_eq!(data[2]["title"], "Never");
    }

    #[tokio::test]
    async fn supports_sorting_by_start_time() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;

        let body_early = json!({
            "title": "Early",
            "description": "desc",
            "start_time": "2099-01-01T00:00:00Z",
            "end_time": "2099-01-02T00:00:00Z",
            "is_public": true,
        });
        app.post_with_token(routes::CONTESTS, &body_early, &admin)
            .await;

        let body_late = json!({
            "title": "Late",
            "description": "desc",
            "start_time": "2099-06-01T00:00:00Z",
            "end_time": "2099-06-02T00:00:00Z",
            "is_public": true,
        });
        app.post_with_token(routes::CONTESTS, &body_late, &admin)
            .await;

        let res = app
            .get_with_token(
                &format!("{}?sort_by=start_time&sort_order=asc", routes::CONTESTS),
                &admin,
            )
            .await;
        assert_eq!(res.status, 200);
        let data = res.body["data"].as_array().unwrap();
        assert_eq!(data[0]["title"], "Early");
        assert_eq!(data[1]["title"], "Late");
    }

    #[tokio::test]
    async fn rejects_invalid_sort_by() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;

        let res = app
            .get_with_token(&format!("{}?sort_by=bogus", routes::CONTESTS), &admin)
            .await;
        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn unauthenticated_user_cannot_list_contests() {
        let app = TestApp::spawn().await;
        let res = app.get_without_token(routes::CONTESTS).await;
        assert_eq!(res.status, 401);
    }
}

mod contest_retrieval {
    use super::*;

    #[tokio::test]
    async fn admin_can_get_any_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let id = create_contest_as_admin(&app, &admin, "Private", false).await;

        let res = app.get_with_token(&routes::contest(id), &admin).await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["title"], "Private");
        assert!(res.body["description"].as_str().is_some());
    }

    #[tokio::test]
    async fn participant_can_get_enrolled_private_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let id = create_contest_as_admin(&app, &admin, "Private", false).await;

        let uid = app.get_with_token(routes::ME, &user).await.id();
        app.post_with_token(
            &routes::contest_participants(id),
            &json!({"user_id": uid}),
            &admin,
        )
        .await;

        let res = app.get_with_token(&routes::contest(id), &user).await;
        assert_eq!(res.status, 200);
    }

    #[tokio::test]
    async fn non_participant_cannot_see_private_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let id = create_contest_as_admin(&app, &admin, "Private", false).await;

        let res = app.get_with_token(&routes::contest(id), &user).await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn anyone_can_get_public_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let id = create_contest_as_admin(&app, &admin, "Public", true).await;

        let res = app.get_with_token(&routes::contest(id), &user).await;
        assert_eq!(res.status, 200);
    }

    #[tokio::test]
    async fn contest_my_info_reports_registration_status_for_current_user() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let id = create_contest_as_admin(&app, &admin, "Public", true).await;

        let before = app
            .get_with_token(&routes::contest_my_info(id), &user)
            .await;
        assert_eq!(before.status, 200);
        assert_eq!(before.body["is_registered"], false);

        app.post_with_token(&routes::contest_register(id), &json!({}), &user)
            .await;

        let after = app
            .get_with_token(&routes::contest_my_info(id), &user)
            .await;
        assert_eq!(after.status, 200);
        assert_eq!(after.body["is_registered"], true);
    }
}

mod contest_update {
    use super::*;

    #[tokio::test]
    async fn admin_can_update_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let id = create_contest_as_admin(&app, &admin, "Original", false).await;

        let patch = json!({"title": "Updated"});
        let res = app
            .patch_with_token(&routes::contest(id), &patch, &admin)
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["title"], "Updated");
    }

    #[tokio::test]
    async fn empty_patch_returns_current_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let id = create_contest_as_admin(&app, &admin, "Stable", false).await;

        let res = app
            .patch_with_token(&routes::contest(id), &json!({}), &admin)
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["title"], "Stable");
    }

    #[tokio::test]
    async fn validates_start_time_against_existing_end_time() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let id = create_contest_as_admin(&app, &admin, "Test", false).await;

        let patch = json!({"start_time": "2099-01-03T00:00:00Z"});
        let res = app
            .patch_with_token(&routes::contest(id), &patch, &admin)
            .await;
        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn validates_activate_time_against_existing_start_time() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin", "pass1234", "admin")
            .await;
        let id = create_contest_as_admin(&app, &admin, "Test", true).await;

        let patch = json!({"activate_time": "2020-01-02T00:00:00Z"});
        let res = app
            .patch_with_token(&routes::contest(id), &patch, &admin)
            .await;
        assert_eq!(res.status, 400);
    }

    #[tokio::test]
    async fn validates_deactivate_time_against_existing_end_time() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin", "pass1234", "admin")
            .await;
        let id = create_contest_as_admin(&app, &admin, "Test", true).await;

        let patch = json!({"deactivate_time": "2099-01-01T00:00:00Z"});
        let res = app
            .patch_with_token(&routes::contest(id), &patch, &admin)
            .await;
        assert_eq!(res.status, 400);
    }

    #[tokio::test]
    async fn can_set_activate_time_to_null() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin", "pass1234", "admin")
            .await;

        let mut body = valid_contest_body("Activate Test", true);
        body["activate_time"] = json!("2020-01-01T00:00:00Z");
        let res = app.post_with_token(routes::CONTESTS, &body, &admin).await;
        let id = res.id();

        let patch = json!({"activate_time": null});
        let res = app
            .patch_with_token(&routes::contest(id), &patch, &admin)
            .await;
        assert_eq!(res.status, 200);
        assert!(res.body["activate_time"].is_null());
    }

    #[tokio::test]
    async fn can_set_deactivate_time_to_null() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin", "pass1234", "admin")
            .await;

        let mut body = valid_contest_body("Deactivate Test", true);
        body["deactivate_time"] = json!("2099-12-31T23:59:59Z");
        let res = app.post_with_token(routes::CONTESTS, &body, &admin).await;
        let id = res.id();

        let patch = json!({"deactivate_time": null});
        let res = app
            .patch_with_token(&routes::contest(id), &patch, &admin)
            .await;
        assert_eq!(res.status, 200);
        assert!(res.body["deactivate_time"].is_null());
    }

    #[tokio::test]
    async fn contestant_cannot_update_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let id = create_contest_as_admin(&app, &admin, "Locked", false).await;

        let patch = json!({"title": "Hacked"});
        let res = app
            .patch_with_token(&routes::contest(id), &patch, &user)
            .await;
        assert_eq!(res.status, 403);
    }

    #[tokio::test]
    async fn returns_not_found_for_nonexistent_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;

        let res = app
            .patch_with_token(&routes::contest(99999), &json!({"title": "X"}), &admin)
            .await;
        assert_eq!(res.status, 404);
    }

    #[tokio::test]
    async fn can_toggle_is_public() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let id = create_contest_as_admin(&app, &admin, "Toggle", false).await;

        let res = app
            .patch_with_token(&routes::contest(id), &json!({"is_public": true}), &admin)
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["is_public"], true);
    }
}

mod contest_deletion {
    use super::*;

    #[tokio::test]
    async fn admin_can_delete_a_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let id = create_contest_as_admin(&app, &admin, "Doomed", false).await;

        let res = app.delete_with_token(&routes::contest(id), &admin).await;
        assert_eq!(res.status, 204);

        let get_res = app.get_with_token(&routes::contest(id), &admin).await;
        assert_eq!(get_res.status, 404);
    }

    #[tokio::test]
    async fn deletes_associated_problems_and_participants() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "Complex", true).await;
        let problem_id = app.create_problem(&admin, "P1").await;

        let add_body = json!({"problem_id": problem_id, "label": "A"});
        app.post_with_token(&routes::contest_problems(contest_id), &add_body, &admin)
            .await;

        app.post_with_token(&routes::contest_register(contest_id), &json!({}), &user)
            .await;

        let res = app
            .delete_with_token(&routes::contest(contest_id), &admin)
            .await;
        assert_eq!(res.status, 204);

        let prob_res = app
            .get_with_token(&routes::problem(problem_id), &admin)
            .await;
        assert_eq!(prob_res.status, 200);
    }

    #[tokio::test]
    async fn contestant_cannot_delete_a_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let id = create_contest_as_admin(&app, &admin, "Protected", false).await;

        let res = app.delete_with_token(&routes::contest(id), &user).await;
        assert_eq!(res.status, 403);
    }

    #[tokio::test]
    async fn returns_not_found_for_nonexistent_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;

        let res = app.delete_with_token(&routes::contest(99999), &admin).await;
        assert_eq!(res.status, 404);
    }

    #[tokio::test]
    async fn soft_deleted_contest_rejects_new_registration() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_softdel_contest_1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("contestant_softdel_1", "pass1234", "contestant")
            .await;
        let id = create_contest_as_admin(&app, &admin, "Doomed Register", true).await;

        let delete_res = app.delete_with_token(&routes::contest(id), &admin).await;
        assert_eq!(delete_res.status, 204);

        let register_res = app
            .post_with_token(&routes::contest_register(id), &json!({}), &user)
            .await;
        assert_eq!(register_res.status, 404);
        assert_eq!(register_res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn soft_deleted_contest_rejects_participant_management() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_softdel_contest_2", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("contestant_softdel_2", "pass1234", "contestant")
            .await;
        let id = create_contest_as_admin(&app, &admin, "Doomed Manage", false).await;
        let user_id = app.get_with_token(routes::ME, &user).await.id();

        let delete_res = app.delete_with_token(&routes::contest(id), &admin).await;
        assert_eq!(delete_res.status, 204);

        let add_res = app
            .post_with_token(
                &routes::contest_participants(id),
                &json!({"user_id": user_id}),
                &admin,
            )
            .await;
        assert_eq!(add_res.status, 404);
        assert_eq!(add_res.body["code"], "NOT_FOUND");
    }
}

mod contest_problems {
    use super::*;

    #[tokio::test]
    async fn admin_can_add_problem_to_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", true).await;
        let problem_id = app.create_problem(&admin, "P1").await;

        let body = json!({"problem_id": problem_id, "label": "A"});
        let res = app
            .post_with_token(&routes::contest_problems(contest_id), &body, &admin)
            .await;

        assert_eq!(res.status, 201);
        assert_eq!(res.body["label"], "A");
        assert_eq!(res.body["problem_title"], "P1");
        assert_eq!(res.body["position"], 0);
    }

    #[tokio::test]
    async fn returns_conflict_for_duplicate_label() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", true).await;
        let p1 = app.create_problem(&admin, "P1").await;
        let p2 = app.create_problem(&admin, "P2").await;

        app.post_with_token(
            &routes::contest_problems(contest_id),
            &json!({"problem_id": p1, "label": "A"}),
            &admin,
        )
        .await;

        let res = app
            .post_with_token(
                &routes::contest_problems(contest_id),
                &json!({"problem_id": p2, "label": "A"}),
                &admin,
            )
            .await;
        assert_eq!(res.status, 409);
        assert_eq!(res.body["code"], "CONFLICT");
    }

    #[tokio::test]
    async fn auto_increments_position_when_omitted() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", true).await;
        let p1 = app.create_problem(&admin, "P1").await;
        let p2 = app.create_problem(&admin, "P2").await;

        let r1 = app
            .post_with_token(
                &routes::contest_problems(contest_id),
                &json!({"problem_id": p1, "label": "A"}),
                &admin,
            )
            .await;
        let r2 = app
            .post_with_token(
                &routes::contest_problems(contest_id),
                &json!({"problem_id": p2, "label": "B"}),
                &admin,
            )
            .await;

        assert_eq!(r1.body["position"], 0);
        assert_eq!(r2.body["position"], 1);
    }

    #[tokio::test]
    async fn admin_can_list_contest_problems() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", true).await;
        let p1 = app.create_problem(&admin, "P1").await;
        let p2 = app.create_problem(&admin, "P2").await;

        app.post_with_token(
            &routes::contest_problems(contest_id),
            &json!({"problem_id": p1, "label": "A"}),
            &admin,
        )
        .await;
        app.post_with_token(
            &routes::contest_problems(contest_id),
            &json!({"problem_id": p2, "label": "B"}),
            &admin,
        )
        .await;

        let res = app
            .get_with_token(&routes::contest_problems(contest_id), &admin)
            .await;
        assert_eq!(res.status, 200);
        let data = res.body.as_array().unwrap();
        assert_eq!(data.len(), 2);
        assert_eq!(data[0]["label"], "A");
        assert!(data[0]["problem_title"].as_str().is_some());
    }

    #[tokio::test]
    async fn participant_can_see_problems_of_enrolled_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "Private", false).await;
        let p1 = app.create_problem(&admin, "P1").await;

        app.post_with_token(
            &routes::contest_problems(contest_id),
            &json!({"problem_id": p1, "label": "A"}),
            &admin,
        )
        .await;

        let uid = app.get_with_token(routes::ME, &user).await.id();
        app.post_with_token(
            &routes::contest_participants(contest_id),
            &json!({"user_id": uid}),
            &admin,
        )
        .await;

        let res = app
            .get_with_token(&routes::contest_problems(contest_id), &user)
            .await;
        assert_eq!(res.status, 200);
    }

    #[tokio::test]
    async fn admin_can_update_problem_label_and_position() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", true).await;
        let p1 = app.create_problem(&admin, "P1").await;

        app.post_with_token(
            &routes::contest_problems(contest_id),
            &json!({"problem_id": p1, "label": "A"}),
            &admin,
        )
        .await;

        let patch = json!({"label": "Z", "position": 42});
        let res = app
            .patch_with_token(&routes::contest_problem(contest_id, p1), &patch, &admin)
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["label"], "Z");
        assert_eq!(res.body["position"], 42);
    }

    #[tokio::test]
    async fn empty_patch_returns_current_contest_problem() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", true).await;
        let p1 = app.create_problem(&admin, "P1").await;

        app.post_with_token(
            &routes::contest_problems(contest_id),
            &json!({"problem_id": p1, "label": "A"}),
            &admin,
        )
        .await;

        let res = app
            .patch_with_token(&routes::contest_problem(contest_id, p1), &json!({}), &admin)
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["label"], "A");
        assert_eq!(res.body["problem_title"], "P1");
    }

    #[tokio::test]
    async fn admin_can_remove_problem_from_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", true).await;
        let p1 = app.create_problem(&admin, "P1").await;

        app.post_with_token(
            &routes::contest_problems(contest_id),
            &json!({"problem_id": p1, "label": "A"}),
            &admin,
        )
        .await;

        let res = app
            .delete_with_token(&routes::contest_problem(contest_id, p1), &admin)
            .await;
        assert_eq!(res.status, 204);

        let list = app
            .get_with_token(&routes::contest_problems(contest_id), &admin)
            .await;
        assert_eq!(list.body.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn returns_not_found_for_nonexistent_problem() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", true).await;

        let body = json!({"problem_id": 99999, "label": "X"});
        let res = app
            .post_with_token(&routes::contest_problems(contest_id), &body, &admin)
            .await;
        assert_eq!(res.status, 404);
    }

    #[tokio::test]
    async fn returns_conflict_for_duplicate_problem_id() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", true).await;
        let p1 = app.create_problem(&admin, "P1").await;

        app.post_with_token(
            &routes::contest_problems(contest_id),
            &json!({"problem_id": p1, "label": "A"}),
            &admin,
        )
        .await;

        let res = app
            .post_with_token(
                &routes::contest_problems(contest_id),
                &json!({"problem_id": p1, "label": "B"}),
                &admin,
            )
            .await;
        assert_eq!(res.status, 409);
        assert_eq!(res.body["code"], "CONFLICT");
    }

    #[tokio::test]
    async fn returns_not_found_for_nonexistent_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let p1 = app.create_problem(&admin, "P1").await;

        let body = json!({"problem_id": p1, "label": "A"});
        let res = app
            .post_with_token(&routes::contest_problems(99999), &body, &admin)
            .await;
        assert_eq!(res.status, 404);
    }

    #[tokio::test]
    async fn contestant_cannot_add_problem_to_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", true).await;
        let p1 = app.create_problem(&admin, "P1").await;

        let body = json!({"problem_id": p1, "label": "A"});
        let res = app
            .post_with_token(&routes::contest_problems(contest_id), &body, &user)
            .await;
        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn contestant_cannot_update_contest_problem() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", true).await;
        let p1 = app.create_problem(&admin, "P1").await;

        app.post_with_token(
            &routes::contest_problems(contest_id),
            &json!({"problem_id": p1, "label": "A"}),
            &admin,
        )
        .await;

        let res = app
            .patch_with_token(
                &routes::contest_problem(contest_id, p1),
                &json!({"label": "Z"}),
                &user,
            )
            .await;
        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn contestant_cannot_remove_contest_problem() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", true).await;
        let p1 = app.create_problem(&admin, "P1").await;

        app.post_with_token(
            &routes::contest_problems(contest_id),
            &json!({"problem_id": p1, "label": "A"}),
            &admin,
        )
        .await;

        let res = app
            .delete_with_token(&routes::contest_problem(contest_id, p1), &user)
            .await;
        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn update_returns_conflict_for_duplicate_label() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", true).await;
        let p1 = app.create_problem(&admin, "P1").await;
        let p2 = app.create_problem(&admin, "P2").await;

        app.post_with_token(
            &routes::contest_problems(contest_id),
            &json!({"problem_id": p1, "label": "A"}),
            &admin,
        )
        .await;
        app.post_with_token(
            &routes::contest_problems(contest_id),
            &json!({"problem_id": p2, "label": "B"}),
            &admin,
        )
        .await;

        let res = app
            .patch_with_token(
                &routes::contest_problem(contest_id, p2),
                &json!({"label": "A"}),
                &admin,
            )
            .await;
        assert_eq!(res.status, 409);
        assert_eq!(res.body["code"], "CONFLICT");
    }

    #[tokio::test]
    async fn update_with_same_label_succeeds() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", true).await;
        let p1 = app.create_problem(&admin, "P1").await;

        app.post_with_token(
            &routes::contest_problems(contest_id),
            &json!({"problem_id": p1, "label": "A"}),
            &admin,
        )
        .await;

        let res = app
            .patch_with_token(
                &routes::contest_problem(contest_id, p1),
                &json!({"label": "A"}),
                &admin,
            )
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["label"], "A");
    }

    #[tokio::test]
    async fn rejects_empty_label_on_add_problem() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", true).await;
        let p1 = app.create_problem(&admin, "P1").await;

        let res = app
            .post_with_token(
                &routes::contest_problems(contest_id),
                &json!({"problem_id": p1, "label": "   "}),
                &admin,
            )
            .await;
        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn rejects_label_exceeding_max_length() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", true).await;
        let p1 = app.create_problem(&admin, "P1").await;

        let res = app
            .post_with_token(
                &routes::contest_problems(contest_id),
                &json!({"problem_id": p1, "label": "ABCDEFGHIJK"}),
                &admin,
            )
            .await;
        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn rejects_negative_position_on_add_problem() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", true).await;
        let p1 = app.create_problem(&admin, "P1").await;

        let res = app
            .post_with_token(
                &routes::contest_problems(contest_id),
                &json!({"problem_id": p1, "label": "A", "position": -1}),
                &admin,
            )
            .await;
        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn non_participant_cannot_see_problems_of_private_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "Private", false).await;
        let p1 = app.create_problem(&admin, "P1").await;

        app.post_with_token(
            &routes::contest_problems(contest_id),
            &json!({"problem_id": p1, "label": "A"}),
            &admin,
        )
        .await;

        let res = app
            .get_with_token(&routes::contest_problems(contest_id), &user)
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn admin_can_list_problems_before_contest_starts() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let body = json!({
            "title": "Future Contest",
            "description": "desc",
            "start_time": "2099-01-01T00:00:00Z",
            "end_time": "2099-01-02T00:00:00Z",
            "is_public": true,
        });
        let contest_id = app
            .post_with_token(routes::CONTESTS, &body, &admin)
            .await
            .id();
        let p1 = app.create_problem(&admin, "P1").await;
        app.post_with_token(
            &routes::contest_problems(contest_id),
            &json!({"problem_id": p1, "label": "A"}),
            &admin,
        )
        .await;

        let res = app
            .get_with_token(&routes::contest_problems(contest_id), &admin)
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn participant_cannot_list_problems_before_contest_starts() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let body = json!({
            "title": "Future Contest",
            "description": "desc",
            "activate_time": "2020-01-01T00:00:00Z",
            "start_time": "2099-01-01T00:00:00Z",
            "end_time": "2099-01-02T00:00:00Z",
            "is_public": true,
        });
        let contest_id = app
            .post_with_token(routes::CONTESTS, &body, &admin)
            .await
            .id();

        let res = app
            .get_with_token(&routes::contest_problems(contest_id), &user)
            .await;
        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn participant_can_list_problems_after_contest_starts() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "Active", true).await;
        let p1 = app.create_problem(&admin, "P1").await;
        app.post_with_token(
            &routes::contest_problems(contest_id),
            &json!({"problem_id": p1, "label": "A"}),
            &admin,
        )
        .await;

        let res = app
            .get_with_token(&routes::contest_problems(contest_id), &user)
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body.as_array().unwrap().len(), 1);
    }
}

mod contest_participants {
    use super::*;

    #[tokio::test]
    async fn admin_can_add_participant() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", false).await;

        let uid = app.get_with_token(routes::ME, &user).await.id();
        let body = json!({"user_id": uid});
        let res = app
            .post_with_token(&routes::contest_participants(contest_id), &body, &admin)
            .await;

        assert_eq!(res.status, 201);
        assert_eq!(res.body["user_id"], uid);
        assert_eq!(res.body["username"], "user1");
    }

    #[tokio::test]
    async fn returns_conflict_for_duplicate_participant() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", false).await;

        let uid = app.get_with_token(routes::ME, &user).await.id();
        let body = json!({"user_id": uid});
        app.post_with_token(&routes::contest_participants(contest_id), &body, &admin)
            .await;

        let res = app
            .post_with_token(&routes::contest_participants(contest_id), &body, &admin)
            .await;
        assert_eq!(res.status, 409);
        assert_eq!(res.body["code"], "CONFLICT");
    }

    #[tokio::test]
    async fn returns_not_found_for_nonexistent_user() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", false).await;

        let body = json!({"user_id": 99999});
        let res = app
            .post_with_token(&routes::contest_participants(contest_id), &body, &admin)
            .await;
        assert_eq!(res.status, 404);
    }

    #[tokio::test]
    async fn admin_can_list_participants() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", true).await;

        let uid = app.get_with_token(routes::ME, &user).await.id();
        app.post_with_token(
            &routes::contest_participants(contest_id),
            &json!({"user_id": uid}),
            &admin,
        )
        .await;

        let res = app
            .get_with_token(&routes::contest_participants(contest_id), &admin)
            .await;
        assert_eq!(res.status, 200);
        let data = res.body.as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["username"], "user1");
    }

    #[tokio::test]
    async fn hidden_participant_list_denies_non_manager_participant() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_hide_list", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user_hide_list", "pass1234", "contestant")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "Hidden list", true).await;

        let patch_res = app
            .patch_with_token(
                &routes::contest(contest_id),
                &json!({"show_participants_list": false}),
                &admin,
            )
            .await;
        assert_eq!(patch_res.status, 200, "patch failed: {}", patch_res.text);

        app.post_with_token(&routes::contest_register(contest_id), &json!({}), &user)
            .await;

        let res = app
            .get_with_token(&routes::contest_participants(contest_id), &user)
            .await;
        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn hidden_participant_list_still_readable_by_contest_manager() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_hide_list2", "pass1234", "admin")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "Hidden list 2", true).await;

        let patch_res = app
            .patch_with_token(
                &routes::contest(contest_id),
                &json!({"show_participants_list": false}),
                &admin,
            )
            .await;
        assert_eq!(patch_res.status, 200, "patch failed: {}", patch_res.text);

        let res = app
            .get_with_token(&routes::contest_participants(contest_id), &admin)
            .await;
        assert_eq!(res.status, 200);
    }

    #[tokio::test]
    async fn admin_can_remove_participant() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", false).await;

        let uid = app.get_with_token(routes::ME, &user).await.id();
        app.post_with_token(
            &routes::contest_participants(contest_id),
            &json!({"user_id": uid}),
            &admin,
        )
        .await;

        let res = app
            .delete_with_token(&routes::contest_participant(contest_id, uid), &admin)
            .await;
        assert_eq!(res.status, 204);

        let list = app
            .get_with_token(&routes::contest_participants(contest_id), &admin)
            .await;
        assert_eq!(list.body.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn contestant_cannot_add_participant() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", false).await;

        let uid = app.get_with_token(routes::ME, &user).await.id();
        let res = app
            .post_with_token(
                &routes::contest_participants(contest_id),
                &json!({"user_id": uid}),
                &user,
            )
            .await;
        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn contestant_cannot_remove_participant() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", true).await;

        let uid = app.get_with_token(routes::ME, &user).await.id();
        app.post_with_token(
            &routes::contest_participants(contest_id),
            &json!({"user_id": uid}),
            &admin,
        )
        .await;

        let res = app
            .delete_with_token(&routes::contest_participant(contest_id, uid), &user)
            .await;
        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn participant_can_see_participant_list() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", true).await;

        app.post_with_token(&routes::contest_register(contest_id), &json!({}), &user)
            .await;

        let res = app
            .get_with_token(&routes::contest_participants(contest_id), &user)
            .await;
        assert_eq!(res.status, 200);
    }

    #[tokio::test]
    async fn list_marks_soft_deleted_participants_as_deleted() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_participant_soft_delete", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("participant_soft_deleted", "pass1234", "contestant")
            .await;
        let create_res = app
            .post_with_token(
                routes::CONTESTS,
                &json!({
                    "title": "C-soft-delete",
                    "description": "desc",
                    "activate_time": "2020-01-01T00:00:00Z",
                    "start_time": "2020-01-01T00:00:00Z",
                    "end_time": "2020-01-02T00:00:00Z",
                    "deactivate_time": "2020-01-03T00:00:00Z",
                    "is_public": true,
                }),
                &admin,
            )
            .await;
        assert_eq!(
            create_res.status, 201,
            "create contest failed: {}",
            create_res.text
        );
        let contest_id = create_res.id();

        let user_id = app.get_with_token(routes::ME, &user).await.id();
        let add_res = app
            .post_with_token(
                &routes::contest_participants(contest_id),
                &json!({"user_id": user_id}),
                &admin,
            )
            .await;
        assert_eq!(
            add_res.status, 201,
            "add participant failed: {}",
            add_res.text
        );

        let delete_res = app
            .delete_with_token(&format!("{}/{}", routes::USERS, user_id), &admin)
            .await;
        assert_eq!(
            delete_res.status, 204,
            "delete user failed: {}",
            delete_res.text
        );

        let list_res = app
            .get_with_token(&routes::contest_participants(contest_id), &admin)
            .await;
        assert_eq!(
            list_res.status, 200,
            "list participants failed: {}",
            list_res.text
        );

        let participants = list_res
            .body
            .as_array()
            .expect("participants response should be array");
        let deleted_user = participants
            .iter()
            .find(|p| p["user_id"] == json!(user_id))
            .expect("deleted participant should remain in list");
        assert_eq!(deleted_user["username"], "participant_soft_deleted");
        assert_eq!(deleted_user["is_deleted"], true);
    }
}

mod contest_registration {
    use super::*;

    #[tokio::test]
    async fn user_can_register_for_public_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let id = create_contest_as_admin(&app, &admin, "Public", true).await;

        let res = app
            .post_with_token(&routes::contest_register(id), &json!({}), &user)
            .await;
        assert_eq!(res.status, 201);
    }

    #[tokio::test]
    async fn user_cannot_register_for_private_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let id = create_contest_as_admin(&app, &admin, "Private", false).await;

        let res = app
            .post_with_token(&routes::contest_register(id), &json!({}), &user)
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn user_cannot_register_for_never_activating_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user", "pass1234", "contestant")
            .await;

        let body = json!({
            "title": "Never",
            "description": "desc",
            "start_time": "2020-01-01T00:00:00Z",
            "end_time": "2020-01-02T00:00:00Z",
            "is_public": true,
        });
        let res = app.post_with_token(routes::CONTESTS, &body, &admin).await;
        let id = res.id();

        let res = app
            .post_with_token(&routes::contest_register(id), &json!({}), &user)
            .await;
        assert_eq!(res.status, 404);
    }

    #[tokio::test]
    async fn user_cannot_register_for_not_yet_activated_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user", "pass1234", "contestant")
            .await;

        let body = json!({
            "title": "Future",
            "description": "desc",
            "activate_time": "2099-01-01T00:00:00Z",
            "start_time": "2099-01-02T00:00:00Z",
            "end_time": "2099-01-03T00:00:00Z",
            "is_public": true,
        });
        let res = app.post_with_token(routes::CONTESTS, &body, &admin).await;
        let id = res.id();

        let res = app
            .post_with_token(&routes::contest_register(id), &json!({}), &user)
            .await;
        assert_eq!(res.status, 404);
    }

    #[tokio::test]
    async fn user_cannot_register_for_deactivated_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user", "pass1234", "contestant")
            .await;

        let body = json!({
            "title": "Dead",
            "description": "desc",
            "activate_time": "2020-01-01T00:00:00Z",
            "start_time": "2020-01-02T00:00:00Z",
            "end_time": "2020-01-03T00:00:00Z",
            "deactivate_time": "2020-01-04T00:00:00Z",
            "is_public": true,
        });
        let res = app.post_with_token(routes::CONTESTS, &body, &admin).await;
        let id = res.id();

        let res = app
            .post_with_token(&routes::contest_register(id), &json!({}), &user)
            .await;
        assert_eq!(res.status, 404);
    }

    #[tokio::test]
    async fn user_cannot_register_for_ended_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;

        let body = json!({
            "title": "Ended",
            "description": "desc",
            "activate_time": "2020-01-01T00:00:00Z",
            "start_time": "2020-01-01T00:00:00Z",
            "end_time": "2020-01-02T00:00:00Z",
            "is_public": true,
        });
        let create_res = app.post_with_token(routes::CONTESTS, &body, &admin).await;
        let id = create_res.id();

        let res = app
            .post_with_token(&routes::contest_register(id), &json!({}), &user)
            .await;
        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn duplicate_registration_returns_conflict() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let id = create_contest_as_admin(&app, &admin, "Public", true).await;

        app.post_with_token(&routes::contest_register(id), &json!({}), &user)
            .await;
        let res = app
            .post_with_token(&routes::contest_register(id), &json!({}), &user)
            .await;
        assert_eq!(res.status, 409);
        assert_eq!(res.body["code"], "CONFLICT");
    }

    #[tokio::test]
    async fn user_can_unregister_from_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let id = create_contest_as_admin(&app, &admin, "Public", true).await;

        app.post_with_token(&routes::contest_register(id), &json!({}), &user)
            .await;
        let res = app
            .delete_with_token(&routes::contest_register(id), &user)
            .await;
        assert_eq!(res.status, 204);
    }

    #[tokio::test]
    async fn unregister_returns_not_found_if_not_registered() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let id = create_contest_as_admin(&app, &admin, "Public", true).await;

        let res = app
            .delete_with_token(&routes::contest_register(id), &user)
            .await;
        assert_eq!(res.status, 404);
    }
}

mod contest_problem_reorder {
    use super::*;

    #[tokio::test]
    async fn admin_can_reorder_contest_problems() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", true).await;
        let p1 = app.create_problem(&admin, "P1").await;
        let p2 = app.create_problem(&admin, "P2").await;
        let p3 = app.create_problem(&admin, "P3").await;

        app.post_with_token(
            &routes::contest_problems(contest_id),
            &json!({"problem_id": p1, "label": "A"}),
            &admin,
        )
        .await;
        app.post_with_token(
            &routes::contest_problems(contest_id),
            &json!({"problem_id": p2, "label": "B"}),
            &admin,
        )
        .await;
        app.post_with_token(
            &routes::contest_problems(contest_id),
            &json!({"problem_id": p3, "label": "C"}),
            &admin,
        )
        .await;

        let body = json!({"problem_ids": [p3, p1, p2]});
        let res = app
            .put_with_token(&routes::contest_problems_reorder(contest_id), &body, &admin)
            .await;
        assert_eq!(res.status, 204);

        let list = app
            .get_with_token(&routes::contest_problems(contest_id), &admin)
            .await;
        assert_eq!(list.status, 200);
        let data = list.body.as_array().unwrap();
        assert_eq!(data[0]["label"], "C");
        assert_eq!(data[0]["position"], 0);
        assert_eq!(data[1]["label"], "A");
        assert_eq!(data[1]["position"], 1);
        assert_eq!(data[2]["label"], "B");
        assert_eq!(data[2]["position"], 2);
    }

    #[tokio::test]
    async fn reorder_rejects_missing_problem_ids() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", true).await;
        let p1 = app.create_problem(&admin, "P1").await;
        let p2 = app.create_problem(&admin, "P2").await;

        app.post_with_token(
            &routes::contest_problems(contest_id),
            &json!({"problem_id": p1, "label": "A"}),
            &admin,
        )
        .await;
        app.post_with_token(
            &routes::contest_problems(contest_id),
            &json!({"problem_id": p2, "label": "B"}),
            &admin,
        )
        .await;

        let body = json!({"problem_ids": [p1]});
        let res = app
            .put_with_token(&routes::contest_problems_reorder(contest_id), &body, &admin)
            .await;
        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn reorder_rejects_extra_problem_ids() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", true).await;
        let p1 = app.create_problem(&admin, "P1").await;

        app.post_with_token(
            &routes::contest_problems(contest_id),
            &json!({"problem_id": p1, "label": "A"}),
            &admin,
        )
        .await;

        let body = json!({"problem_ids": [p1, 99999]});
        let res = app
            .put_with_token(&routes::contest_problems_reorder(contest_id), &body, &admin)
            .await;
        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn reorder_rejects_duplicate_problem_ids() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", true).await;
        let p1 = app.create_problem(&admin, "P1").await;

        app.post_with_token(
            &routes::contest_problems(contest_id),
            &json!({"problem_id": p1, "label": "A"}),
            &admin,
        )
        .await;

        let body = json!({"problem_ids": [p1, p1]});
        let res = app
            .put_with_token(&routes::contest_problems_reorder(contest_id), &body, &admin)
            .await;
        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn reorder_rejects_empty_list() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", true).await;

        let body = json!({"problem_ids": []});
        let res = app
            .put_with_token(&routes::contest_problems_reorder(contest_id), &body, &admin)
            .await;
        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn contestant_cannot_reorder_contest_problems() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let contest_id = create_contest_as_admin(&app, &admin, "C1", true).await;
        let p1 = app.create_problem(&admin, "P1").await;

        app.post_with_token(
            &routes::contest_problems(contest_id),
            &json!({"problem_id": p1, "label": "A"}),
            &admin,
        )
        .await;

        let body = json!({"problem_ids": [p1]});
        let res = app
            .put_with_token(&routes::contest_problems_reorder(contest_id), &body, &user)
            .await;
        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }
}

mod bulk_delete_contest_problems {
    use super::*;

    #[tokio::test]
    async fn admin_can_bulk_delete_contest_problems() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_bdc1", "pass1234", "admin")
            .await;

        let cid = create_contest_as_admin(&app, &admin, "Bulk Del CP", false).await;
        let p1 = app.create_problem(&admin, "P1").await;
        let p2 = app.create_problem(&admin, "P2").await;
        let p3 = app.create_problem(&admin, "P3").await;

        app.post_with_token(
            &routes::contest_problems(cid),
            &json!({"problem_id": p1, "label": "A"}),
            &admin,
        )
        .await;
        app.post_with_token(
            &routes::contest_problems(cid),
            &json!({"problem_id": p2, "label": "B"}),
            &admin,
        )
        .await;
        app.post_with_token(
            &routes::contest_problems(cid),
            &json!({"problem_id": p3, "label": "C"}),
            &admin,
        )
        .await;

        let res = app
            .delete_with_body_and_token(
                &routes::contest_problems_bulk(cid),
                &json!({"problem_ids": [p1, p2]}),
                &admin,
            )
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["removed"], 2);

        let list = app
            .get_with_token(&routes::contest_problems(cid), &admin)
            .await;
        assert_eq!(list.status, 200);
        let data = list.body.as_array().expect("response should be array");
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["problem_id"], p3);
    }

    #[tokio::test]
    async fn returns_validation_error_for_empty_ids() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_bdc2", "pass1234", "admin")
            .await;

        let cid = create_contest_as_admin(&app, &admin, "Bulk Del CP", false).await;

        let res = app
            .delete_with_body_and_token(
                &routes::contest_problems_bulk(cid),
                &json!({"problem_ids": []}),
                &admin,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn returns_validation_error_for_duplicate_ids() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_bdc3", "pass1234", "admin")
            .await;

        let cid = create_contest_as_admin(&app, &admin, "Bulk Del CP", false).await;
        let p1 = app.create_problem(&admin, "P1").await;

        app.post_with_token(
            &routes::contest_problems(cid),
            &json!({"problem_id": p1, "label": "A"}),
            &admin,
        )
        .await;

        let res = app
            .delete_with_body_and_token(
                &routes::contest_problems_bulk(cid),
                &json!({"problem_ids": [p1, p1]}),
                &admin,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn returns_not_found_for_nonexistent_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_bdc4", "pass1234", "admin")
            .await;

        let res = app
            .delete_with_body_and_token(
                &routes::contest_problems_bulk(99999),
                &json!({"problem_ids": [1]}),
                &admin,
            )
            .await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn returns_not_found_for_ids_not_in_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_bdc5", "pass1234", "admin")
            .await;

        let cid = create_contest_as_admin(&app, &admin, "Bulk Del CP", false).await;
        let p1 = app.create_problem(&admin, "P1").await;
        let p_other = app.create_problem(&admin, "P Other").await;

        app.post_with_token(
            &routes::contest_problems(cid),
            &json!({"problem_id": p1, "label": "A"}),
            &admin,
        )
        .await;

        let res = app
            .delete_with_body_and_token(
                &routes::contest_problems_bulk(cid),
                &json!({"problem_ids": [p1, p_other]}),
                &admin,
            )
            .await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn contestant_cannot_bulk_delete_contest_problems() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_bdc6", "pass1234", "admin")
            .await;
        let contestant = app
            .create_authenticated_user("contestant_bdc6", "pass1234")
            .await;

        let cid = create_contest_as_admin(&app, &admin, "Bulk Del CP", false).await;
        let p1 = app.create_problem(&admin, "P1").await;

        app.post_with_token(
            &routes::contest_problems(cid),
            &json!({"problem_id": p1, "label": "A"}),
            &admin,
        )
        .await;

        let res = app
            .delete_with_body_and_token(
                &routes::contest_problems_bulk(cid),
                &json!({"problem_ids": [p1]}),
                &contestant,
            )
            .await;

        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }
}

mod bulk_add_participants {
    use super::*;

    #[tokio::test]
    async fn admin_can_add_existing_users_by_username() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_bap1", "pass1234", "admin")
            .await;

        let cid = app.create_contest(&admin, "BAP Contest", true, false).await;

        app.create_authenticated_user("existuser1", "pass1234")
            .await;
        app.create_authenticated_user("existuser2", "pass1234")
            .await;

        let res = app
            .post_with_token(
                &routes::contest_participants_bulk(cid),
                &json!({"usernames": ["existuser1", "existuser2"]}),
                &admin,
            )
            .await;

        assert_eq!(res.status, 200);
        let added = res.body["added"].as_array().expect("added array");
        assert_eq!(added.len(), 2);
        assert!(res.body["not_found"].as_array().unwrap().is_empty());
        assert!(res.body["created"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn reports_not_found_for_nonexistent_usernames() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_bap2", "pass1234", "admin")
            .await;

        let cid = app.create_contest(&admin, "BAP Contest", true, false).await;

        let res = app
            .post_with_token(
                &routes::contest_participants_bulk(cid),
                &json!({"usernames": ["zzzfake_nonexist"]}),
                &admin,
            )
            .await;

        assert_eq!(res.status, 200);
        let not_found = res.body["not_found"].as_array().expect("not_found array");
        assert_eq!(not_found.len(), 1);
        assert_eq!(not_found[0], "zzzfake_nonexist");
        assert!(res.body["added"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn admin_can_create_and_enroll_new_users() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_bap3", "pass1234", "admin")
            .await;

        let cid = app.create_contest(&admin, "BAP Contest", true, false).await;

        let res = app
            .post_with_token(
                &routes::contest_participants_bulk(cid),
                &json!({
                    "create_users": [
                        {"username": "newuser_bap3"}
                    ]
                }),
                &admin,
            )
            .await;

        assert_eq!(res.status, 200);
        let created = res.body["created"].as_array().expect("created array");
        assert_eq!(created.len(), 1);
        assert_eq!(created[0]["username"], "newuser_bap3");
        assert!(created[0]["password"].as_str().is_some());
        assert!(created[0]["user_id"].as_i64().is_some());
    }

    #[tokio::test]
    async fn created_user_can_login_with_returned_password() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_bap4", "pass1234", "admin")
            .await;

        let cid = app.create_contest(&admin, "BAP Contest", true, false).await;

        let res = app
            .post_with_token(
                &routes::contest_participants_bulk(cid),
                &json!({
                    "create_users": [
                        {"username": "logintest_bap4"}
                    ]
                }),
                &admin,
            )
            .await;

        assert_eq!(res.status, 200);
        let created = res.body["created"].as_array().expect("created array");
        let password = created[0]["password"].as_str().expect("password");

        let login_res = app
            .post_without_token(
                routes::LOGIN,
                &json!({"username": "logintest_bap4", "password": password}),
            )
            .await;

        assert_eq!(login_res.status, 200);
        assert!(login_res.body["token"].as_str().is_some());
    }

    #[tokio::test]
    async fn handles_already_enrolled_users() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_bap5", "pass1234", "admin")
            .await;

        let cid = app.create_contest(&admin, "BAP Contest", true, false).await;

        let user_token = app
            .create_authenticated_user("enrolled_bap5", "pass1234")
            .await;
        app.register_for_contest(cid, &user_token).await;

        let res = app
            .post_with_token(
                &routes::contest_participants_bulk(cid),
                &json!({"usernames": ["enrolled_bap5"]}),
                &admin,
            )
            .await;

        assert_eq!(res.status, 200);
        let already = res.body["already_enrolled"]
            .as_array()
            .expect("already_enrolled");
        assert_eq!(already.len(), 1);
        assert_eq!(already[0]["username"], "enrolled_bap5");
        assert!(res.body["added"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn mixed_request_with_all_categories() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_bap6", "pass1234", "admin")
            .await;

        let cid = app.create_contest(&admin, "BAP Contest", true, false).await;

        let enrolled_token = app
            .create_authenticated_user("enrolled_bap6", "pass1234")
            .await;
        app.register_for_contest(cid, &enrolled_token).await;

        app.create_authenticated_user("notenrolled_bap6", "pass1234")
            .await;

        let res = app
            .post_with_token(
                &routes::contest_participants_bulk(cid),
                &json!({
                    "usernames": ["enrolled_bap6", "notenrolled_bap6", "fakename_bap6"],
                    "create_users": [{"username": "brandnew_bap6"}]
                }),
                &admin,
            )
            .await;

        assert_eq!(res.status, 200);

        let added = res.body["added"].as_array().expect("added");
        let created = res.body["created"].as_array().expect("created");
        let already = res.body["already_enrolled"]
            .as_array()
            .expect("already_enrolled");
        let not_found = res.body["not_found"].as_array().expect("not_found");

        assert_eq!(added.len(), 1);
        assert_eq!(added[0]["username"], "notenrolled_bap6");

        assert_eq!(created.len(), 1);
        assert_eq!(created[0]["username"], "brandnew_bap6");

        assert_eq!(already.len(), 1);
        assert_eq!(already[0]["username"], "enrolled_bap6");

        assert_eq!(not_found.len(), 1);
        assert_eq!(not_found[0], "fakename_bap6");
    }

    #[tokio::test]
    async fn returns_validation_error_for_empty_request() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_bap7", "pass1234", "admin")
            .await;

        let cid = app.create_contest(&admin, "BAP Contest", true, false).await;

        let res = app
            .post_with_token(&routes::contest_participants_bulk(cid), &json!({}), &admin)
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn contestant_cannot_bulk_add_participants() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_bap8", "pass1234", "admin")
            .await;
        let contestant = app
            .create_authenticated_user("contestant_bap8", "pass1234")
            .await;

        let cid = app.create_contest(&admin, "BAP Contest", true, false).await;

        let res = app
            .post_with_token(
                &routes::contest_participants_bulk(cid),
                &json!({"usernames": ["someone"]}),
                &contestant,
            )
            .await;

        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }
}
