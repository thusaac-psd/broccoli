use serde_json::json;

use crate::common::{TestApp, routes};

fn build_zip(files: &[(&str, &str)]) -> Vec<u8> {
    use std::io::Write;
    let buf = Vec::new();
    let cursor = std::io::Cursor::new(buf);
    let mut writer = zip::ZipWriter::new(cursor);
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, content) in files {
        writer.start_file(*name, options).expect("zip start_file");
        writer.write_all(content.as_bytes()).expect("zip write_all");
    }
    let cursor = writer.finish().expect("zip finish");
    cursor.into_inner()
}

async fn setup_problem_with_test_case(app: &TestApp, token: &str, label: &str) -> i32 {
    let pid = app.create_problem(token, "Merge Strategy Test").await;
    let res = app
        .post_with_token(
            &routes::test_cases(pid),
            &json!({
                "input": "original_in",
                "expected_output": "original_out",
                "score": 10,
                "is_sample": false,
                "label": label
            }),
            token,
        )
        .await;
    assert_eq!(res.status, 201);
    assert_eq!(res.body["position"], 0);
    pid
}

async fn insert_submission_for_problem(app: &TestApp, problem_id: i32) {
    use sea_orm::{ActiveModelTrait, Set};
    use server::entity::submission;

    let files = serde_json::json!([{"filename": "main.cpp", "content": "int main() {}"}]);
    let sub = submission::ActiveModel {
        problem_id: Set(problem_id),
        user_id: Set(1),
        language: Set("cpp".into()),
        files: Set(files),
        status: Set(common::SubmissionStatus::Pending),
        created_at: Set(chrono::Utc::now()),
        ..Default::default()
    };
    sub.insert(&app.db).await.expect("insert submission");
}

async fn insert_contest_association_for_problem(app: &TestApp, problem_id: i32) {
    use sea_orm::{ActiveModelTrait, Set};
    use server::entity::{contest, contest_problem};

    let now = chrono::Utc::now();
    let c = contest::ActiveModel {
        title: Set("Test Contest".into()),
        description: Set("A test contest".into()),
        activate_time: Set(Some(now)),
        start_time: Set(now),
        end_time: Set(now + chrono::Duration::hours(3)),
        is_public: Set(false),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    let contest_model = c.insert(&app.db).await.expect("insert contest");

    let cp = contest_problem::ActiveModel {
        contest_id: Set(contest_model.id),
        problem_id: Set(problem_id),
        label: Set("A".into()),
        position: Set(0),
    };
    cp.insert(&app.db).await.expect("insert contest_problem");
}

mod problem_creation {
    use super::*;

    #[tokio::test]
    async fn admin_can_create_a_problem() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin1", "password123", "admin")
            .await;

        let res = app
            .post_with_token(
                routes::PROBLEMS,
                &json!({
                    "title": "Two Sum",
                    "content": "Find two numbers that sum to target.",
                    "time_limit": 1000,
                    "memory_limit": 262144
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

    #[tokio::test]
    async fn problem_setter_can_create_a_problem() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("setter1", "password123", "problem_setter")
            .await;

        let res = app
            .post_with_token(
                routes::PROBLEMS,
                &json!({
                    "title": "Array Max",
                    "content": "Find the maximum.",
                    "time_limit": 2000,
                    "memory_limit": 131072
                }),
                &token,
            )
            .await;

        assert_eq!(res.status, 201);
    }

    #[tokio::test]
    async fn contestant_cannot_create_a_problem() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("contestant1", "password123", "contestant")
            .await;

        let res = app
            .post_with_token(
                routes::PROBLEMS,
                &json!({
                    "title": "Nope",
                    "content": "Should fail.",
                    "time_limit": 1000,
                    "memory_limit": 262144
                }),
                &token,
            )
            .await;

        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn cannot_create_a_problem_with_invalid_data() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin2", "password123", "admin")
            .await;

        let res = app
            .post_with_token(
                routes::PROBLEMS,
                &json!({
                    "title": "   ",
                    "content": "Some content",
                    "time_limit": 1000,
                    "memory_limit": 262144
                }),
                &token,
            )
            .await;
        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");

        let res = app
            .post_with_token(
                routes::PROBLEMS,
                &json!({
                    "title": "Valid",
                    "content": "Some content",
                    "time_limit": 0,
                    "memory_limit": 262144
                }),
                &token,
            )
            .await;
        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn create_problem_trims_title_whitespace() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin47", "password123", "admin")
            .await;

        let res = app
            .post_with_token(
                routes::PROBLEMS,
                &json!({
                    "title": "  Padded Title  ",
                    "content": "Some content",
                    "time_limit": 1000,
                    "memory_limit": 262144
                }),
                &token,
            )
            .await;

        assert_eq!(res.status, 201);
        assert_eq!(res.body["title"], "Padded Title");
    }
}

mod problem_listing {
    use super::*;

    #[tokio::test]
    async fn list_returns_paginated_results() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin3", "password123", "admin")
            .await;

        for i in 0..3 {
            app.post_with_token(
                routes::PROBLEMS,
                &json!({
                    "title": format!("Problem {i}"),
                    "content": "Content",
                    "time_limit": 1000,
                    "memory_limit": 262144
                }),
                &token,
            )
            .await;
        }

        let res = app
            .get_with_token(&format!("{}?per_page=2", routes::PROBLEMS), &token)
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["data"].as_array().unwrap().len(), 2);
        assert_eq!(res.body["pagination"]["total"], 3);
        assert_eq!(res.body["pagination"]["total_pages"], 2);
    }

    #[tokio::test]
    async fn list_can_filter_problems_by_title_search() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin4", "password123", "admin")
            .await;

        app.post_with_token(
            routes::PROBLEMS,
            &json!({
                "title": "Binary Search",
                "content": "Implement binary search.",
                "time_limit": 1000,
                "memory_limit": 262144
            }),
            &token,
        )
        .await;

        app.post_with_token(
            routes::PROBLEMS,
            &json!({
                "title": "Two Sum",
                "content": "Find pairs.",
                "time_limit": 1000,
                "memory_limit": 262144
            }),
            &token,
        )
        .await;

        let res = app
            .get_with_token(&format!("{}?search=binary", routes::PROBLEMS), &token)
            .await;

        assert_eq!(res.status, 200);
        let data = res.body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["title"], "Binary Search");
    }

    #[tokio::test]
    async fn search_escapes_like_wildcard_characters() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin30", "password123", "admin")
            .await;

        app.post_with_token(
            routes::PROBLEMS,
            &json!({
                "title": "100% Done",
                "content": "Content",
                "time_limit": 1000,
                "memory_limit": 262144
            }),
            &token,
        )
        .await;

        app.post_with_token(
            routes::PROBLEMS,
            &json!({
                "title": "Totally Different",
                "content": "Content",
                "time_limit": 1000,
                "memory_limit": 262144
            }),
            &token,
        )
        .await;

        let res = app
            .get_with_token(&format!("{}?search=100%25", routes::PROBLEMS), &token)
            .await;

        assert_eq!(res.status, 200);
        let data = res.body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["title"], "100% Done");
    }

    #[tokio::test]
    async fn list_rejects_invalid_sort_by() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin31", "password123", "admin")
            .await;

        let res = app
            .get_with_token(&format!("{}?sort_by=nonexistent", routes::PROBLEMS), &token)
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn list_can_sort_problems_by_title() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin5", "password123", "admin")
            .await;

        app.post_with_token(
            routes::PROBLEMS,
            &json!({
                "title": "Zebra",
                "content": "Z problem.",
                "time_limit": 1000,
                "memory_limit": 262144
            }),
            &token,
        )
        .await;

        app.post_with_token(
            routes::PROBLEMS,
            &json!({
                "title": "Apple",
                "content": "A problem.",
                "time_limit": 1000,
                "memory_limit": 262144
            }),
            &token,
        )
        .await;

        let res = app
            .get_with_token(
                &format!("{}?sort_by=title&sort_order=asc", routes::PROBLEMS),
                &token,
            )
            .await;

        assert_eq!(res.status, 200);
        let data = res.body["data"].as_array().unwrap();
        assert_eq!(data[0]["title"], "Apple");
        assert_eq!(data[1]["title"], "Zebra");
    }

    #[tokio::test]
    async fn contestant_cannot_list_problems() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("contestant2", "password123", "contestant")
            .await;

        let res = app.get_with_token(routes::PROBLEMS, &token).await;

        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }
}

mod problem_detail {
    use super::*;

    #[tokio::test]
    async fn can_retrieve_a_problem_with_full_content() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin7", "password123", "admin")
            .await;

        let id = app.create_problem(&token, "Test Problem").await;

        let res = app.get_with_token(&routes::problem(id), &token).await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["id"], id);
        assert!(
            res.body["content"].is_string(),
            "Detail should include content"
        );
    }

    #[tokio::test]
    async fn cannot_retrieve_a_nonexistent_problem() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin8", "password123", "admin")
            .await;

        let res = app.get_with_token(&routes::problem(99999), &token).await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }
}

mod problem_update {
    use super::*;

    #[tokio::test]
    async fn can_partially_update_a_problem() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin9", "password123", "admin")
            .await;

        let id = app.create_problem(&token, "Test Problem").await;

        let res = app
            .patch_with_token(
                &routes::problem(id),
                &json!({ "title": "Updated Title" }),
                &token,
            )
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["title"], "Updated Title");
        assert_eq!(res.body["time_limit"], 1000);
    }

    #[tokio::test]
    async fn cannot_update_a_nonexistent_problem() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin37", "password123", "admin")
            .await;

        let res = app
            .patch_with_token(
                &routes::problem(99999),
                &json!({ "title": "Ghost" }),
                &token,
            )
            .await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn contestant_cannot_update_a_problem() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin10", "password123", "admin")
            .await;
        let contestant_token = app
            .create_user_with_role("contestant3", "password123", "contestant")
            .await;

        let id = app.create_problem(&admin_token, "Test Problem").await;

        let res = app
            .patch_with_token(
                &routes::problem(id),
                &json!({ "title": "Hacked" }),
                &contestant_token,
            )
            .await;

        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn empty_patch_body_returns_unchanged_problem() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin45", "password123", "admin")
            .await;

        let id = app.create_problem(&token, "Test Problem").await;

        let original = app.get_with_token(&routes::problem(id), &token).await;
        assert_eq!(original.status, 200);

        let res = app
            .patch_with_token(&routes::problem(id), &json!({}), &token)
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["title"], original.body["title"]);
        assert_eq!(res.body["time_limit"], original.body["time_limit"]);
        assert_eq!(res.body["updated_at"], original.body["updated_at"]);
    }
}

mod problem_deletion {
    use super::*;

    #[tokio::test]
    async fn admin_can_delete_a_problem_and_its_test_cases() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin11", "password123", "admin")
            .await;

        let id = app.create_problem(&token, "Test Problem").await;
        app.create_test_case(id, &token).await;

        let res = app.delete_with_token(&routes::problem(id), &token).await;

        assert_eq!(res.status, 204);

        let get_res = app.get_with_token(&routes::problem(id), &token).await;
        assert_eq!(get_res.status, 404);
    }

    #[tokio::test]
    async fn problem_setter_cannot_delete_a_problem() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin12", "password123", "admin")
            .await;
        let setter_token = app
            .create_user_with_role("setter2", "password123", "problem_setter")
            .await;

        let id = app.create_problem(&admin_token, "Test Problem").await;

        let res = app
            .delete_with_token(&routes::problem(id), &setter_token)
            .await;

        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn cannot_delete_a_nonexistent_problem() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin36", "password123", "admin")
            .await;

        let res = app.delete_with_token(&routes::problem(99999), &token).await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn can_soft_delete_a_problem_that_has_submissions() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin13", "password123", "admin")
            .await;

        let id = app.create_problem(&token, "Test Problem").await;
        insert_submission_for_problem(&app, id).await;

        let res = app.delete_with_token(&routes::problem(id), &token).await;

        assert_eq!(res.status, 204);
    }

    #[tokio::test]
    async fn cannot_delete_a_problem_associated_with_a_contest() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin46", "password123", "admin")
            .await;

        let id = app.create_problem(&token, "Test Problem").await;
        insert_contest_association_for_problem(&app, id).await;

        let res = app.delete_with_token(&routes::problem(id), &token).await;

        assert_eq!(res.status, 409);
        assert_eq!(res.body["code"], "CONFLICT");
    }

    #[tokio::test]
    async fn soft_deleted_problem_cannot_create_new_test_case() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin_softdel_problem", "password123", "admin")
            .await;

        let id = app.create_problem(&token, "Soft Deleted Problem").await;

        let delete_res = app.delete_with_token(&routes::problem(id), &token).await;
        assert_eq!(delete_res.status, 204);

        let create_tc_res = app
            .post_with_token(
                &routes::test_cases(id),
                &json!({
                    "input": "1 2",
                    "expected_output": "3",
                    "score": 10,
                    "is_sample": false,
                    "label": "tc_soft_deleted"
                }),
                &token,
            )
            .await;

        assert_eq!(create_tc_res.status, 404);
        assert_eq!(create_tc_res.body["code"], "NOT_FOUND");
    }
}

mod test_case_creation {
    use super::*;

    #[tokio::test]
    async fn can_create_a_test_case_for_a_problem() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin14", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Test Problem").await;

        let res = app
            .post_with_token(
                &routes::test_cases(pid),
                &json!({
                    "input": "3\n1 2 3",
                    "expected_output": "6",
                    "score": 10,
                    "is_sample": true,
                    "label": "sample_01"
                }),
                &token,
            )
            .await;

        assert_eq!(res.status, 201);
        assert_eq!(res.body["score"], 10);
        assert_eq!(res.body["is_sample"], true);
        assert_eq!(res.body["problem_id"], pid);
    }

    #[tokio::test]
    async fn can_create_test_case_with_large_input_file_contents() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin_large_testcase_input", "password123", "admin")
            .await;

        let pid = app
            .create_problem(&token, "Large Single Test Case Problem")
            .await;
        let large_input = String::from_utf8(vec![b'x'; 1_048_576 + 123])
            .expect("ASCII input should be valid UTF-8");

        let res = app
            .post_with_token(
                &routes::test_cases(pid),
                &json!({
                    "input": large_input,
                    "expected_output": "ok",
                    "score": 10,
                    "is_sample": false,
                    "label": "large_input"
                }),
                &token,
            )
            .await;

        assert_eq!(res.status, 201, "response body: {}", res.body);
        assert_eq!(res.body["label"], "large_input");
        assert_eq!(res.body["input"], large_input);
        assert_eq!(res.body["input_size"], large_input.len());
        // A truncated body's preview is the first 100 chars plus a "..." marker
        // (see `truncate_preview`); the 100-char core is a genuine prefix.
        let input_preview = res.body["input_preview"].as_str().unwrap();
        let core = input_preview
            .strip_suffix("...")
            .expect("truncated preview is marked");
        assert_eq!(core.chars().count(), 100);
        assert!(large_input.starts_with(core));

        let tc_id = res.id();
        let get_res = app
            .get_with_token(&routes::test_case(pid, tc_id), &token)
            .await;
        assert_eq!(get_res.status, 200, "get body: {}", get_res.body);
        assert_eq!(get_res.body["input"], large_input);
        assert_eq!(get_res.body["expected_output"], "ok");

        let list_res = app.get_with_token(&routes::test_cases(pid), &token).await;
        assert_eq!(list_res.status, 200, "list body: {}", list_res.body);
        let list_preview = list_res.body[0]["input_preview"].as_str().unwrap();
        let list_core = list_preview
            .strip_suffix("...")
            .expect("truncated preview is marked");
        assert_eq!(list_core.chars().count(), 100);
        assert!(large_input.starts_with(list_core));
    }

    #[tokio::test]
    async fn create_defaults_label_to_position_when_omitted() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin_tc_default_label", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Default Label Problem").await;

        let res = app
            .post_with_token(
                &routes::test_cases(pid),
                &json!({
                    "input": "3\n1 2 3",
                    "expected_output": "6",
                    "score": 10,
                    "is_sample": true
                }),
                &token,
            )
            .await;

        assert_eq!(res.status, 201);
        assert_eq!(res.body["position"], 0);
        assert_eq!(res.body["label"], "0");
    }

    #[tokio::test]
    async fn cannot_create_a_test_case_for_a_nonexistent_problem() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin17", "password123", "admin")
            .await;

        let res = app
            .post_with_token(
                &routes::test_cases(99999),
                &json!({
                    "input": "1",
                    "expected_output": "1",
                    "score": 0,
                    "is_sample": false,
                    "label": "tc_01"
                }),
                &token,
            )
            .await;

        assert_eq!(res.status, 404);
    }

    #[tokio::test]
    async fn rejects_test_case_with_negative_score() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin40", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Test Problem").await;

        let res = app
            .post_with_token(
                &routes::test_cases(pid),
                &json!({
                    "input": "1",
                    "expected_output": "1",
                    "score": -1,
                    "is_sample": false,
                    "label": "tc_neg"
                }),
                &token,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn rejects_test_case_with_negative_position() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin42", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Test Problem").await;

        let res = app
            .post_with_token(
                &routes::test_cases(pid),
                &json!({
                    "input": "1",
                    "expected_output": "1",
                    "score": 5,
                    "is_sample": false,
                    "position": -1,
                    "label": "tc_pos"
                }),
                &token,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn position_is_auto_assigned_when_omitted() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin24", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Test Problem").await;

        let res1 = app
            .post_with_token(
                &routes::test_cases(pid),
                &json!({
                    "input": "1",
                    "expected_output": "1",
                    "score": 5,
                    "is_sample": true,
                    "label": "tc_01"
                }),
                &token,
            )
            .await;
        let res2 = app
            .post_with_token(
                &routes::test_cases(pid),
                &json!({
                    "input": "2",
                    "expected_output": "2",
                    "score": 5,
                    "is_sample": false,
                    "label": "tc_02"
                }),
                &token,
            )
            .await;

        let pos1 = res1.body["position"].as_i64().unwrap();
        let pos2 = res2.body["position"].as_i64().unwrap();
        assert!(
            pos2 > pos1,
            "Second test case should have a higher position"
        );
    }

    #[tokio::test]
    async fn rejects_duplicate_label_within_problem() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin_tc_dup_create", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Duplicate Label Problem").await;

        let first = app
            .post_with_token(
                &routes::test_cases(pid),
                &json!({
                    "input": "1",
                    "expected_output": "1",
                    "score": 10,
                    "is_sample": false,
                    "label": "dup_label"
                }),
                &token,
            )
            .await;
        assert_eq!(first.status, 201);

        let second = app
            .post_with_token(
                &routes::test_cases(pid),
                &json!({
                    "input": "2",
                    "expected_output": "2",
                    "score": 10,
                    "is_sample": false,
                    "label": "dup_label"
                }),
                &token,
            )
            .await;

        assert_eq!(second.status, 409);
        assert_eq!(second.body["code"], "CONFLICT");
    }
}

mod test_case_listing {
    use super::*;

    #[tokio::test]
    async fn list_returns_previews_without_full_data() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin18", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Test Problem").await;
        app.create_test_case(pid, &token).await;

        let res = app.get_with_token(&routes::test_cases(pid), &token).await;

        assert_eq!(res.status, 200);
        let items = res.body.as_array().unwrap();
        assert_eq!(items.len(), 1);

        let item = &items[0];
        assert!(item.get("input_preview").is_some());
        assert!(item.get("output_preview").is_some());
        assert!(
            item.get("input").is_none(),
            "Full input should not be in list"
        );
        assert!(
            item.get("expected_output").is_none(),
            "Full output should not be in list"
        );
    }

    #[tokio::test]
    async fn list_test_cases_for_nonexistent_problem_returns_404() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin43", "password123", "admin")
            .await;

        let res = app.get_with_token(&routes::test_cases(99999), &token).await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn long_input_is_truncated_in_preview() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin19", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Test Problem").await;

        let long_input = "x".repeat(200);
        app.post_with_token(
            &routes::test_cases(pid),
            &json!({
                "input": long_input,
                "expected_output": "result",
                "score": 5,
                "is_sample": false,
                "label": "tc_long"
            }),
            &token,
        )
        .await;

        let res = app.get_with_token(&routes::test_cases(pid), &token).await;

        assert_eq!(res.status, 200);
        let preview = res.body[0]["input_preview"].as_str().unwrap();
        let core = preview
            .strip_suffix("...")
            .expect("truncated preview is marked");
        assert_eq!(core.chars().count(), 100);
        assert!(long_input.starts_with(core));
    }

    #[tokio::test]
    async fn unicode_input_is_truncated_at_character_boundary() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin51", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Test Problem").await;

        let unicode_input: String = "あ".repeat(200);
        app.post_with_token(
            &routes::test_cases(pid),
            &json!({
                "input": unicode_input,
                "expected_output": "ok",
                "score": 5,
                "is_sample": false,
                "label": "tc_unicode"
            }),
            &token,
        )
        .await;

        let res = app.get_with_token(&routes::test_cases(pid), &token).await;
        assert_eq!(res.status, 200);

        let preview = res.body[0]["input_preview"].as_str().unwrap();
        // Char-boundary-safe: the multibyte body truncates cleanly to 100 chars
        // before the "..." marker, and that core is a genuine prefix.
        let core = preview
            .strip_suffix("...")
            .expect("truncated preview is marked");
        assert_eq!(core.chars().count(), 100);
        assert!(unicode_input.starts_with(core));
    }
}

mod test_case_detail {
    use super::*;

    #[tokio::test]
    async fn can_retrieve_full_test_case_data() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin20", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Test Problem").await;
        let tc_id = app.create_test_case(pid, &token).await;

        let res = app
            .get_with_token(&routes::test_case(pid, tc_id), &token)
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["id"], tc_id);
        assert!(res.body["input"].is_string());
        assert!(res.body["expected_output"].is_string());
    }

    #[tokio::test]
    async fn cannot_access_a_test_case_via_the_wrong_problem() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin21", "password123", "admin")
            .await;

        let pid1 = app.create_problem(&token, "Test Problem").await;
        let pid2 = app.create_problem(&token, "Test Problem").await;
        let tc_id = app.create_test_case(pid1, &token).await;

        let res = app
            .get_with_token(&routes::test_case(pid2, tc_id), &token)
            .await;

        assert_eq!(res.status, 404);
    }

    #[tokio::test]
    async fn contestant_can_access_sample_test_case_via_active_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_tc_sample_1", "password123", "admin")
            .await;
        let contestant = app
            .create_authenticated_user("contestant_tc_sample_1", "password123")
            .await;

        let pid = app.create_problem(&admin, "Sample Access Problem").await;

        let sample_res = app
            .post_with_token(
                &routes::test_cases(pid),
                &json!({
                    "input": "1 2",
                    "expected_output": "3",
                    "score": 10,
                    "is_sample": true,
                    "label": "sample_01",
                }),
                &admin,
            )
            .await;
        assert_eq!(sample_res.status, 201);
        let sample_id = sample_res.id();

        let cid = app
            .create_contest(&admin, "Sample Access Contest", true, true)
            .await;
        app.add_problem_to_contest(cid, pid, &admin).await;

        let res = app
            .get_with_token(&routes::test_case(pid, sample_id), &contestant)
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["id"], sample_id);
        assert_eq!(res.body["is_sample"], true);
    }

    #[tokio::test]
    async fn contestant_cannot_access_non_sample_test_case_via_active_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_tc_sample_2", "password123", "admin")
            .await;
        let contestant = app
            .create_authenticated_user("contestant_tc_sample_2", "password123")
            .await;

        let pid = app.create_problem(&admin, "Hidden Case Problem").await;

        let hidden_res = app
            .post_with_token(
                &routes::test_cases(pid),
                &json!({
                    "input": "secret",
                    "expected_output": "answer",
                    "score": 90,
                    "is_sample": false,
                    "label": "hidden_01",
                }),
                &admin,
            )
            .await;
        assert_eq!(hidden_res.status, 201);
        let hidden_id = hidden_res.id();

        let cid = app
            .create_contest(&admin, "Hidden Case Contest", true, true)
            .await;
        app.add_problem_to_contest(cid, pid, &admin).await;

        let res = app
            .get_with_token(&routes::test_case(pid, hidden_id), &contestant)
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }
}

mod test_case_update {
    use super::*;

    #[tokio::test]
    async fn can_partially_update_a_test_case() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin22", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Test Problem").await;
        let tc_id = app.create_test_case(pid, &token).await;

        let res = app
            .patch_with_token(
                &routes::test_case(pid, tc_id),
                &json!({ "score": 20 }),
                &token,
            )
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["score"], 20);
        assert_eq!(res.body["is_sample"], true);
    }

    #[tokio::test]
    async fn can_patch_large_expected_output_and_round_trip_full_body() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin_large_expected_patch", "password123", "admin")
            .await;

        let pid = app
            .create_problem(&token, "Large Expected Patch Problem")
            .await;
        let tc_id = app.create_test_case(pid, &token).await;
        let large_output = String::from_utf8(vec![b'7'; 1_048_576 + 321])
            .expect("ASCII output should be valid UTF-8");

        let res = app
            .patch_with_token(
                &routes::test_case(pid, tc_id),
                &json!({ "expected_output": large_output }),
                &token,
            )
            .await;

        assert_eq!(res.status, 200, "patch body: {}", res.body);
        assert_eq!(res.body["expected_output"], large_output);
        assert_eq!(res.body["output_size"], large_output.len());
        let output_preview = res.body["output_preview"].as_str().unwrap();
        let core = output_preview
            .strip_suffix("...")
            .expect("truncated preview is marked");
        assert_eq!(core.chars().count(), 100);
        assert!(large_output.starts_with(core));

        let get_res = app
            .get_with_token(&routes::test_case(pid, tc_id), &token)
            .await;
        assert_eq!(get_res.status, 200, "get body: {}", get_res.body);
        assert_eq!(get_res.body["expected_output"], large_output);
    }

    #[tokio::test]
    async fn null_label_in_patch_leaves_label_unchanged() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin_tc_reset_label", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Reset Label Problem").await;
        let tc_id = app
            .post_with_token(
                &routes::test_cases(pid),
                &json!({
                    "input": "1",
                    "expected_output": "1",
                    "score": 10,
                    "is_sample": false,
                    "position": 7,
                    "label": "custom_label"
                }),
                &token,
            )
            .await
            .id();

        let res = app
            .patch_with_token(
                &routes::test_case(pid, tc_id),
                &json!({ "label": null }),
                &token,
            )
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["label"], "custom_label");
    }

    #[tokio::test]
    async fn cannot_update_a_nonexistent_test_case() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin38", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Test Problem").await;

        let res = app
            .patch_with_token(
                &routes::test_case(pid, 99999),
                &json!({ "score": 50 }),
                &token,
            )
            .await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn can_set_description_to_null_via_patch() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin33", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Test Problem").await;

        let res = app
            .post_with_token(
                &routes::test_cases(pid),
                &json!({
                    "input": "1",
                    "expected_output": "1",
                    "score": 5,
                    "is_sample": false,
                    "description": "original desc",
                    "label": "tc_desc"
                }),
                &token,
            )
            .await;
        assert_eq!(res.status, 201);
        let tc_id = res.id();
        assert_eq!(res.body["description"], "original desc");

        let res = app
            .patch_with_token(
                &routes::test_case(pid, tc_id),
                &json!({ "description": null }),
                &token,
            )
            .await;

        assert_eq!(res.status, 200);
        assert!(res.body["description"].is_null());
    }

    #[tokio::test]
    async fn rejects_duplicate_label_on_update() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin_tc_dup_update", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Duplicate Update Problem").await;
        let tc_a = app
            .post_with_token(
                &routes::test_cases(pid),
                &json!({
                    "input": "1",
                    "expected_output": "1",
                    "score": 5,
                    "is_sample": false,
                    "label": "tc_a"
                }),
                &token,
            )
            .await
            .id();
        let tc_b = app
            .post_with_token(
                &routes::test_cases(pid),
                &json!({
                    "input": "2",
                    "expected_output": "2",
                    "score": 5,
                    "is_sample": false,
                    "label": "tc_b"
                }),
                &token,
            )
            .await
            .id();

        let res = app
            .patch_with_token(
                &routes::test_case(pid, tc_b),
                &json!({ "label": " tc_a " }),
                &token,
            )
            .await;

        assert_eq!(res.status, 409);
        assert_eq!(res.body["code"], "CONFLICT");

        let unchanged = app
            .get_with_token(&routes::test_case(pid, tc_a), &token)
            .await;
        assert_eq!(unchanged.status, 200);
        assert_eq!(unchanged.body["label"], "tc_a");
    }
}

mod test_case_deletion {
    use super::*;

    #[tokio::test]
    async fn can_delete_a_test_case() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin23", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Test Problem").await;
        let tc_id = app.create_test_case(pid, &token).await;

        let res = app
            .delete_with_token(&routes::test_case(pid, tc_id), &token)
            .await;

        assert_eq!(res.status, 204);

        let get_res = app
            .get_with_token(&routes::test_case(pid, tc_id), &token)
            .await;
        assert_eq!(get_res.status, 404);
    }

    #[tokio::test]
    async fn delete_is_blocked_by_test_case_results() {
        use common::{SubmissionStatus, Verdict};
        use sea_orm::{ActiveModelTrait, Set};
        use server::entity::{submission, test_case_result};

        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin_del_blocked", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Test Problem").await;
        let tc_id = app.create_test_case(pid, &token).await;

        let me = app.get_with_token(routes::ME, &token).await;
        let user_id = me.id();

        let now = chrono::Utc::now();

        let files = serde_json::json!([{"filename": "main.rs", "content": "fn main() {}"}]);
        let sub = submission::ActiveModel {
            files: Set(files),
            language: Set("rust".into()),
            status: Set(SubmissionStatus::Judged),
            verdict: Set(Some(Verdict::Accepted)),
            score: Set(Some(100.0)),
            time_used: Set(Some(50)),
            memory_used: Set(Some(1024)),
            user_id: Set(user_id),
            problem_id: Set(pid),
            created_at: Set(now),
            judged_at: Set(Some(now)),
            ..Default::default()
        };
        let sub_model = sub
            .insert(&app.db)
            .await
            .expect("Failed to insert submission");

        let tcr = test_case_result::ActiveModel {
            submission_id: Set(sub_model.id),
            test_case_id: Set(Some(tc_id)),
            verdict: Set(Verdict::Accepted),
            score: Set(10.0),
            time_used: Set(Some(50)),
            memory_used: Set(Some(1024)),
            created_at: Set(now),
            ..Default::default()
        };
        tcr.insert(&app.db)
            .await
            .expect("Failed to insert test case result");

        let res = app
            .delete_with_token(&routes::test_case(pid, tc_id), &token)
            .await;
        assert_eq!(res.status, 409);
        assert_eq!(res.body["code"], "CONFLICT");
    }

    #[tokio::test]
    async fn cannot_delete_a_test_case_via_the_wrong_problem() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin34", "password123", "admin")
            .await;

        let pid1 = app.create_problem(&token, "Test Problem").await;
        let pid2 = app.create_problem(&token, "Test Problem").await;
        let tc_id = app.create_test_case(pid1, &token).await;

        let res = app
            .delete_with_token(&routes::test_case(pid2, tc_id), &token)
            .await;

        assert_eq!(res.status, 404);

        let get_res = app
            .get_with_token(&routes::test_case(pid1, tc_id), &token)
            .await;
        assert_eq!(get_res.status, 200);
    }
}

mod test_case_reorder {
    use super::*;

    #[tokio::test]
    async fn can_reorder_test_cases() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin52", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Test Problem").await;
        let tc1 = app.create_test_case(pid, &token).await;
        let tc2 = app.create_test_case(pid, &token).await;
        let tc3 = app.create_test_case(pid, &token).await;

        let body = json!({"test_case_ids": [tc3, tc1, tc2]});
        let res = app
            .put_with_token(&routes::test_cases_reorder(pid), &body, &token)
            .await;
        assert_eq!(res.status, 204);

        let list = app.get_with_token(&routes::test_cases(pid), &token).await;
        assert_eq!(list.status, 200);
        let data = list.body.as_array().unwrap();
        assert_eq!(data.len(), 3);
        assert_eq!(data[0]["id"], tc3);
        assert_eq!(data[0]["position"], 0);
        assert_eq!(data[1]["id"], tc1);
        assert_eq!(data[1]["position"], 1);
        assert_eq!(data[2]["id"], tc2);
        assert_eq!(data[2]["position"], 2);
    }

    #[tokio::test]
    async fn reorder_rejects_missing_test_case_ids() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin53", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Test Problem").await;
        let tc1 = app.create_test_case(pid, &token).await;
        let _tc2 = app.create_test_case(pid, &token).await;

        let body = json!({"test_case_ids": [tc1]});
        let res = app
            .put_with_token(&routes::test_cases_reorder(pid), &body, &token)
            .await;
        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn reorder_rejects_extra_test_case_ids() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin54", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Test Problem").await;
        let tc1 = app.create_test_case(pid, &token).await;

        let body = json!({"test_case_ids": [tc1, 99999]});
        let res = app
            .put_with_token(&routes::test_cases_reorder(pid), &body, &token)
            .await;
        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn reorder_rejects_duplicate_ids() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin55", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Test Problem").await;
        let tc1 = app.create_test_case(pid, &token).await;

        let body = json!({"test_case_ids": [tc1, tc1]});
        let res = app
            .put_with_token(&routes::test_cases_reorder(pid), &body, &token)
            .await;
        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn reorder_rejects_empty_list() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin56", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Test Problem").await;

        let body = json!({"test_case_ids": []});
        let res = app
            .put_with_token(&routes::test_cases_reorder(pid), &body, &token)
            .await;
        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn reorder_returns_not_found_for_nonexistent_problem() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin57", "password123", "admin")
            .await;

        let body = json!({"test_case_ids": [1]});
        let res = app
            .put_with_token(&routes::test_cases_reorder(99999), &body, &token)
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn contestant_cannot_reorder_test_cases() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin58", "password123", "admin")
            .await;
        let contestant_token = app
            .create_user_with_role("contestant58", "password123", "contestant")
            .await;

        let pid = app.create_problem(&admin_token, "Test Problem").await;
        let tc1 = app.create_test_case(pid, &admin_token).await;

        let body = json!({"test_case_ids": [tc1]});
        let res = app
            .put_with_token(&routes::test_cases_reorder(pid), &body, &contestant_token)
            .await;
        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }
}

mod test_case_zip_upload {
    use super::*;

    #[tokio::test]
    async fn can_upload_test_cases_from_a_flat_zip() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin25", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Test Problem").await;

        let zip_data = build_zip(&[
            ("01.in", "1 2\n"),
            ("01.ans", "3\n"),
            ("02.in", "10 20\n"),
            ("02.ans", "30\n"),
        ]);

        let res = app
            .upload_with_token(
                &routes::test_cases_upload(pid),
                "tests.zip",
                zip_data,
                Some("*.in"),
                Some("*.ans"),
                Some("replace"),
                &token,
            )
            .await;

        assert_eq!(res.status, 201);
        assert_eq!(res.body["created"], 2);
        let tcs = res.body["test_cases"].as_array().unwrap();
        assert_eq!(tcs.len(), 2);
        assert_eq!(tcs[0]["is_sample"], false);
        assert_eq!(tcs[0]["score"], 50);
        assert_eq!(tcs[1]["score"], 50);
    }

    #[tokio::test]
    async fn contestant_cannot_upload_test_cases() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_upload_perm", "password123", "admin")
            .await;
        let contestant = app
            .create_user_with_role("contestant_upload_perm", "password123", "contestant")
            .await;

        let pid = app.create_problem(&admin, "Test Problem").await;

        let zip_data = build_zip(&[("01.in", "1 2\n"), ("01.ans", "3\n")]);

        let res = app
            .upload_with_token(
                &routes::test_cases_upload(pid),
                "tests.zip",
                zip_data,
                Some("*.in"),
                Some("*.ans"),
                Some("replace"),
                &contestant,
            )
            .await;

        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn can_upload_with_sample_and_main_directories() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin26", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Test Problem").await;

        let zip_data = build_zip(&[
            ("sample/sample-01.in", "sample input\n"),
            ("sample/sample-01.ans", "sample output\n"),
            ("main/main-01.in", "main input\n"),
            ("main/main-01.ans", "main output\n"),
        ]);

        let res = app
            .upload_with_token(
                &routes::test_cases_upload(pid),
                "tests.zip",
                zip_data,
                Some("*.in"),
                Some("*.ans"),
                Some("replace"),
                &token,
            )
            .await;

        assert_eq!(res.status, 201, "{}", res.body["message"]);
        assert_eq!(res.body["created"], 2);

        let tcs = res.body["test_cases"].as_array().unwrap();
        let sample = tcs.iter().find(|tc| tc["is_sample"] == true);
        let main_tc = tcs.iter().find(|tc| tc["is_sample"] == false);
        assert!(sample.is_some(), "Should have a sample test case");
        assert!(main_tc.is_some(), "Should have a main test case");
        assert_eq!(sample.unwrap()["score"], 0);
        assert_eq!(main_tc.unwrap()["score"], 100);
    }

    #[tokio::test]
    async fn upload_rejects_zip_with_unmatched_files() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin27", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Test Problem").await;

        let zip_data = build_zip(&[("01.in", "input\n")]);

        let res = app
            .upload_with_token(
                &routes::test_cases_upload(pid),
                "tests.zip",
                zip_data,
                Some("*.in"),
                Some("*.ans"),
                Some("replace"),
                &token,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn can_upload_test_cases_with_custom_wildcard_formats() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin_wildcard", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Wildcard Problem").await;

        let zip_data = build_zip(&[
            ("input_01_data.txt", "10 20\n"),
            ("answer_01_result.ans", "30\n"),
        ]);

        let res = app
            .upload_with_token(
                &routes::test_cases_upload(pid),
                "tests.zip",
                zip_data,
                Some("input_*_data.txt"),
                Some("answer_*_result.ans"),
                Some("replace"),
                &token,
            )
            .await;

        assert_eq!(res.status, 201);
        assert_eq!(res.body["created"], 1);
        let tcs = res.body["test_cases"].as_array().unwrap();
        assert_eq!(tcs[0]["label"], "01");
    }

    #[tokio::test]
    async fn upload_rejects_non_zip_file() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin28", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Test Problem").await;

        let res = app
            .upload_with_token(
                &routes::test_cases_upload(pid),
                "not-a-zip.txt",
                b"this is not a zip file".to_vec(),
                Some("*.in"),
                Some("*.ans"),
                Some("replace"),
                &token,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn cannot_upload_test_cases_to_a_nonexistent_problem() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin29", "password123", "admin")
            .await;

        let zip_data = build_zip(&[("01.in", "input\n"), ("01.ans", "output\n")]);

        let res = app
            .upload_with_token(
                &routes::test_cases_upload(99999),
                "tests.zip",
                zip_data,
                Some("*.in"),
                Some("*.ans"),
                Some("replace"),
                &token,
            )
            .await;

        assert_eq!(res.status, 404);
    }

    #[tokio::test]
    async fn upload_rejects_missing_format_fields() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin50", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Test Problem").await;

        let zip_data = build_zip(&[("01.in", "input\n"), ("01.ans", "answer\n")]);

        let res = app
            .upload_with_token(
                &routes::test_cases_upload(pid),
                "tests.zip",
                zip_data,
                None,
                None,
                Some("replace"),
                &token,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn upload_rejects_invalid_filename_formats() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin51", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Test Problem").await;

        let zip_data = build_zip(&[("01.in", "input\n"), ("01.ans", "answer\n")]);

        let res = app
            .upload_with_token(
                &routes::test_cases_upload(pid),
                "tests.zip",
                zip_data,
                Some("*_input_*.txt"),
                Some("*_answer_*.ans"),
                Some("replace"),
                &token,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn upload_rejects_missing_merge_strategy() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin52", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Test Problem").await;

        let zip_data = build_zip(&[("01.in", "input\n"), ("01.ans", "answer\n")]);

        let res = app
            .upload_with_token(
                &routes::test_cases_upload(pid),
                "tests.zip",
                zip_data,
                Some("*.in"),
                Some("*.ans"),
                None,
                &token,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn upload_rejects_conflicting_existing_label() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin_upload_dup_label", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Upload Label Conflict").await;
        let existing = app
            .post_with_token(
                &routes::test_cases(pid),
                &json!({
                    "input": "seed",
                    "expected_output": "seed",
                    "score": 1,
                    "is_sample": false,
                    "label": "01"
                }),
                &token,
            )
            .await;
        assert_eq!(existing.status, 201);

        let zip_data = build_zip(&[("01.in", "input\n"), ("01.ans", "answer\n")]);

        let res = app
            .upload_with_token(
                &routes::test_cases_upload(pid),
                "tests.zip",
                zip_data,
                Some("*.in"),
                Some("*.ans"),
                Some("abort"),
                &token,
            )
            .await;

        assert_eq!(res.status, 409);
        assert_eq!(res.body["code"], "CONFLICT");
    }

    #[tokio::test]
    async fn upload_rejects_empty_extracted_label() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin_upload_empty_label", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Upload Empty Label").await;
        let zip_data = build_zip(&[("input_.txt", "1\n"), ("output_.txt", "2\n")]);

        let res = app
            .upload_with_token(
                &routes::test_cases_upload(pid),
                "tests.zip",
                zip_data,
                Some("input_*.txt"),
                Some("output_*.txt"),
                Some("replace"),
                &token,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn abort_strategy_fails_if_any_label_exists() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin53", "password123", "admin")
            .await;

        let pid = setup_problem_with_test_case(&app, &token, "01").await;

        let zip_data = build_zip(&[("01.in", "new input\n"), ("01.ans", "new output\n")]);

        let res = app
            .upload_with_token(
                &routes::test_cases_upload(pid),
                "tests.zip",
                zip_data,
                Some("*.in"),
                Some("*.ans"),
                Some("abort"),
                &token,
            )
            .await;

        assert_eq!(res.status, 409);
        assert_eq!(res.body["code"], "CONFLICT");
    }

    #[tokio::test]
    async fn skip_strategy_skips_existing_labels_and_creates_new_ones() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin54", "password123", "admin")
            .await;

        let pid = setup_problem_with_test_case(&app, &token, "01").await;

        let zip_data = build_zip(&[
            ("01.in", "new input\n"),
            ("01.ans", "new output\n"),
            ("02.in", "second input\n"),
            ("02.ans", "second output\n"),
        ]);

        let res = app
            .upload_with_token(
                &routes::test_cases_upload(pid),
                "tests.zip",
                zip_data,
                Some("*.in"),
                Some("*.ans"),
                Some("skip"),
                &token,
            )
            .await;

        assert_eq!(res.status, 201);
        assert_eq!(res.body["created"], 1);
        assert_eq!(res.body["updated"], 0);

        let list = app.get_with_token(&routes::test_cases(pid), &token).await;
        let cases = list.body.as_array().unwrap();
        assert_eq!(cases.len(), 2);
        let tc1 = cases.iter().find(|tc| tc["label"] == "01").unwrap();
        assert_ne!(tc1["input_preview"], "new input\n");
        let tc2 = cases.iter().find(|tc| tc["label"] == "02").unwrap();
        assert_eq!(tc2["position"], 1);
        assert_eq!(tc2["score"], 50);
    }

    #[tokio::test]
    async fn overwrite_strategy_updates_existing_and_creates_new_ones() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin55", "password123", "admin")
            .await;

        let pid = setup_problem_with_test_case(&app, &token, "01").await;

        let zip_data = build_zip(&[
            ("01.in", "updated input\n"),
            ("01.ans", "updated output\n"),
            ("02.in", "second input\n"),
            ("02.ans", "second output\n"),
        ]);

        let res = app
            .upload_with_token(
                &routes::test_cases_upload(pid),
                "tests.zip",
                zip_data,
                Some("*.in"),
                Some("*.ans"),
                Some("overwrite"),
                &token,
            )
            .await;

        assert_eq!(res.status, 201, "{}", res.body["message"]);
        assert_eq!(res.body["created"], 1);
        assert_eq!(res.body["updated"], 1);

        let list = app.get_with_token(&routes::test_cases(pid), &token).await;
        let cases = list.body.as_array().unwrap();
        assert_eq!(cases.len(), 2);
        let tc1 = cases.iter().find(|tc| tc["label"] == "01").unwrap();
        assert_eq!(tc1["input_preview"], "updated input\n");
        assert_eq!(tc1["score"], 50);
        let tc2 = cases.iter().find(|tc| tc["label"] == "02").unwrap();
        assert_eq!(tc2["position"], 1);
        assert_eq!(tc2["score"], 50);
    }

    #[tokio::test]
    async fn replace_strategy_wipes_all_existing_test_cases() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin56", "password123", "admin")
            .await;

        let pid = setup_problem_with_test_case(&app, &token, "01").await;

        let zip_data = build_zip(&[("02.in", "second input\n"), ("02.ans", "second output\n")]);

        let res = app
            .upload_with_token(
                &routes::test_cases_upload(pid),
                "tests.zip",
                zip_data,
                Some("*.in"),
                Some("*.ans"),
                Some("replace"),
                &token,
            )
            .await;

        assert_eq!(res.status, 201);
        assert_eq!(res.body["created"], 1);
        assert_eq!(res.body["updated"], 0);

        let list = app.get_with_token(&routes::test_cases(pid), &token).await;
        let cases = list.body.as_array().unwrap();
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0]["label"], "02");
        assert_eq!(cases[0]["position"], 0);
    }
}

mod bulk_delete_test_cases {
    use super::*;

    #[tokio::test]
    async fn admin_can_bulk_delete_test_cases() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin_bulk1", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Bulk Delete TC").await;
        let tc1 = app.create_test_case(pid, &token).await;
        let tc2 = app.create_test_case(pid, &token).await;
        let tc3 = app.create_test_case(pid, &token).await;

        let res = app
            .delete_with_body_and_token(
                &routes::test_cases_bulk(pid),
                &json!({"test_case_ids": [tc1, tc2]}),
                &token,
            )
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["deleted"], 2);

        let list = app.get_with_token(&routes::test_cases(pid), &token).await;
        assert_eq!(list.status, 200);
        let data = list.body.as_array().expect("response should be array");
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["id"], tc3);
    }

    #[tokio::test]
    async fn returns_validation_error_for_empty_ids() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin_bulk2", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Bulk Delete TC").await;

        let res = app
            .delete_with_body_and_token(
                &routes::test_cases_bulk(pid),
                &json!({"test_case_ids": []}),
                &token,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn returns_validation_error_for_duplicate_ids() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin_bulk3", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Bulk Delete TC").await;
        let tc1 = app.create_test_case(pid, &token).await;

        let res = app
            .delete_with_body_and_token(
                &routes::test_cases_bulk(pid),
                &json!({"test_case_ids": [tc1, tc1]}),
                &token,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn returns_not_found_for_nonexistent_problem() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin_bulk4", "password123", "admin")
            .await;

        let res = app
            .delete_with_body_and_token(
                &routes::test_cases_bulk(99999),
                &json!({"test_case_ids": [1]}),
                &token,
            )
            .await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn returns_not_found_for_ids_not_in_problem() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin_bulk5", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Bulk Delete TC").await;
        let tc1 = app.create_test_case(pid, &token).await;

        let res = app
            .delete_with_body_and_token(
                &routes::test_cases_bulk(pid),
                &json!({"test_case_ids": [tc1, 99999]}),
                &token,
            )
            .await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn returns_conflict_when_test_cases_have_results() {
        use common::{SubmissionStatus, Verdict};
        use sea_orm::{ActiveModelTrait, Set};
        use server::entity::{submission, test_case_result};

        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin_bulk6", "password123", "admin")
            .await;

        let pid = app.create_problem(&token, "Bulk Delete TC").await;
        let tc1 = app.create_test_case(pid, &token).await;
        let tc2 = app.create_test_case(pid, &token).await;

        let me = app.get_with_token(routes::ME, &token).await;
        let user_id = me.id();
        let now = chrono::Utc::now();

        let files = serde_json::json!([{"filename": "main.rs", "content": "fn main() {}"}]);
        let sub = submission::ActiveModel {
            files: Set(files),
            language: Set("rust".into()),
            status: Set(SubmissionStatus::Judged),
            verdict: Set(Some(Verdict::Accepted)),
            score: Set(Some(100.0)),
            time_used: Set(Some(50)),
            memory_used: Set(Some(1024)),
            user_id: Set(user_id),
            problem_id: Set(pid),
            created_at: Set(now),
            judged_at: Set(Some(now)),
            ..Default::default()
        };
        let sub_model = sub.insert(&app.db).await.expect("insert submission");

        let tcr = test_case_result::ActiveModel {
            submission_id: Set(sub_model.id),
            test_case_id: Set(Some(tc1)),
            verdict: Set(Verdict::Accepted),
            score: Set(10.0),
            time_used: Set(Some(50)),
            memory_used: Set(Some(1024)),
            created_at: Set(now),
            ..Default::default()
        };
        tcr.insert(&app.db).await.expect("insert test case result");

        let res = app
            .delete_with_body_and_token(
                &routes::test_cases_bulk(pid),
                &json!({"test_case_ids": [tc1, tc2]}),
                &token,
            )
            .await;

        assert_eq!(res.status, 409);
        assert_eq!(res.body["code"], "CONFLICT");
    }

    #[tokio::test]
    async fn contestant_cannot_bulk_delete_test_cases() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_bulk7", "password123", "admin")
            .await;
        let contestant_token = app
            .create_authenticated_user("contestant_bulk7", "password123")
            .await;

        let pid = app.create_problem(&admin_token, "Bulk Delete TC").await;
        let tc1 = app.create_test_case(pid, &admin_token).await;

        let res = app
            .delete_with_body_and_token(
                &routes::test_cases_bulk(pid),
                &json!({"test_case_ids": [tc1]}),
                &contestant_token,
            )
            .await;

        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }
}

mod problem_contest_access {
    use super::*;

    #[tokio::test]
    async fn contestant_can_view_problem_via_active_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_pca1", "password123", "admin")
            .await;
        let contestant = app
            .create_authenticated_user("contestant_pca1", "password123")
            .await;

        let pid = app.create_problem(&admin, "Contest Problem").await;
        let cid = app
            .create_contest(&admin, "Active Contest", true, true)
            .await;
        app.add_problem_to_contest(cid, pid, &admin).await;

        let res = app.get_with_token(&routes::problem(pid), &contestant).await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["id"], pid);
        assert!(res.body["content"].is_string());
        assert!(res.body["samples"].is_array());
    }

    #[tokio::test]
    async fn contestant_cannot_view_problem_before_contest_starts() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_pca2", "password123", "admin")
            .await;
        let contestant = app
            .create_authenticated_user("contestant_pca2", "password123")
            .await;

        let pid = app.create_hidden_problem(&admin, "Future Problem").await;

        let body = json!({
            "title": "Future Contest",
            "description": "desc",
            "start_time": "2099-01-01T00:00:00Z",
            "end_time": "2099-01-02T00:00:00Z",
            "is_public": true,
        });
        let cid = app
            .post_with_token(routes::CONTESTS, &body, &admin)
            .await
            .id();
        app.add_problem_to_contest(cid, pid, &admin).await;

        let res = app.get_with_token(&routes::problem(pid), &contestant).await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn non_participant_cannot_view_problem_via_private_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_pca3", "password123", "admin")
            .await;
        let outsider = app
            .create_authenticated_user("outsider_pca3", "password123")
            .await;

        let pid = app.create_hidden_problem(&admin, "Private Problem").await;
        let cid = app
            .create_contest(&admin, "Private Contest", false, true)
            .await;
        app.add_problem_to_contest(cid, pid, &admin).await;

        let res = app.get_with_token(&routes::problem(pid), &outsider).await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn problem_response_includes_only_sample_test_case_metadata() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_pca4", "password123", "admin")
            .await;
        let contestant = app
            .create_authenticated_user("contestant_pca4", "password123")
            .await;

        let pid = app.create_problem(&admin, "Samples Problem").await;

        let sample_res = app
            .post_with_token(
                &routes::test_cases(pid),
                &json!({
                    "input": "sample input",
                    "expected_output": "sample output",
                    "score": 10,
                    "is_sample": true,
                    "label": "sample_01",
                    "description": "Try choosing the first available pair.",
                }),
                &admin,
            )
            .await;
        assert_eq!(sample_res.status, 201);

        let hidden_res = app
            .post_with_token(
                &routes::test_cases(pid),
                &json!({
                    "input": "hidden input",
                    "expected_output": "hidden output",
                    "score": 90,
                    "is_sample": false,
                    "label": "hidden_01",
                }),
                &admin,
            )
            .await;
        assert_eq!(hidden_res.status, 201);

        let cid = app
            .create_contest(&admin, "Samples Contest", true, true)
            .await;
        app.add_problem_to_contest(cid, pid, &admin).await;

        let res = app.get_with_token(&routes::problem(pid), &contestant).await;
        assert_eq!(res.status, 200);

        let samples = res.body["samples"].as_array().expect("samples array");
        assert_eq!(samples.len(), 1);
        assert!(samples[0]["id"].is_number());
        assert_eq!(samples[0]["input_size"], "sample input".len());
        assert_eq!(samples[0]["output_size"], "sample output".len());
        assert_eq!(
            samples[0]["description"],
            "Try choosing the first available pair."
        );
        assert!(samples[0].get("input").is_none());
        assert!(samples[0].get("expected_output").is_none());
    }

    #[tokio::test]
    async fn admin_can_view_problem_before_contest_starts() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_pca5", "password123", "admin")
            .await;

        let pid = app.create_problem(&admin, "Admin View Problem").await;

        let res = app.get_with_token(&routes::problem(pid), &admin).await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["id"], pid);
    }
}
