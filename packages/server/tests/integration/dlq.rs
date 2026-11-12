use chrono::Utc;
use common::SubmissionStatus;
use sea_orm::{ActiveModelTrait, EntityTrait, Set};
use serde_json::json;

use server::entity::{dead_letter_message, submission};

use crate::common::{SpawnOptions, TestApp, routes};

async fn create_dlq_entry(
    app: &TestApp,
    submission_id: i32,
    message_type: &str,
    resolved: bool,
) -> i32 {
    let now = Utc::now();
    let model = dead_letter_message::ActiveModel {
        message_id: Set(format!("test-msg-{}-{}", message_type, submission_id)),
        message_type: Set(message_type.to_string()),
        submission_id: Set(Some(submission_id)),
        payload: Set(json!({
            "submission_id": submission_id,
            "job_id": format!("test-job-{}", submission_id),
        })),
        error_code: Set("MAX_RETRIES_EXCEEDED".to_string()),
        error_message: Set("Test error message".to_string()),
        retry_count: Set(3),
        retry_history: Set(json!([
            {"attempt": 1, "error": "Error 1", "timestamp": now.to_rfc3339()},
            {"attempt": 2, "error": "Error 2", "timestamp": now.to_rfc3339()},
            {"attempt": 3, "error": "Error 3", "timestamp": now.to_rfc3339()},
        ])),
        first_failed_at: Set(now),
        created_at: Set(now),
        resolved: Set(resolved),
        resolved_at: Set(if resolved { Some(now) } else { None }),
        ..Default::default()
    };

    let result = model
        .insert(&app.db)
        .await
        .expect("Failed to create DLQ entry");
    result.id
}

async fn insert_submission(
    app: &TestApp,
    user_id: i32,
    problem_id: i32,
    status: SubmissionStatus,
) -> i32 {
    let files = json!([{"filename": "main.cpp", "content": "int main() {}"}]);
    let sub = submission::ActiveModel {
        problem_id: Set(problem_id),
        user_id: Set(user_id),
        language: Set("cpp".into()),
        files: Set(files),
        status: Set(status),
        error_code: Set(Some("STUCK_JOB".into())),
        error_message: Set(Some("Job stuck".into())),
        created_at: Set(Utc::now()),
        ..Default::default()
    };
    let result = sub.insert(&app.db).await.expect("insert submission");
    result.id
}

mod dlq_listing {
    use super::*;

    #[tokio::test]
    async fn admin_can_list_dlq_messages() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin1", "password123", "admin")
            .await;

        create_dlq_entry(&app, 1, "operation_task", false).await;
        create_dlq_entry(&app, 2, "stuck_submission", false).await;
        create_dlq_entry(&app, 3, "operation_task", true).await;

        let res = app.get_with_token(routes::DLQ, &admin_token).await;
        assert_eq!(res.status, 200);

        let data = res.body["data"].as_array().expect("data should be array");
        assert_eq!(data.len(), 3);
        assert!(res.body["pagination"]["total"].as_u64().unwrap() >= 3);
    }

    #[tokio::test]
    async fn can_filter_by_message_type() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin2", "password123", "admin")
            .await;

        create_dlq_entry(&app, 10, "operation_task", false).await;
        create_dlq_entry(&app, 11, "stuck_submission", false).await;

        let res = app
            .get_with_token(
                &format!("{}?message_type=operation_task", routes::DLQ),
                &admin_token,
            )
            .await;
        assert_eq!(res.status, 200);

        let data = res.body["data"].as_array().expect("data should be array");
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["message_type"], "operation_task");
    }

    #[tokio::test]
    async fn can_filter_by_resolved_status() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin3", "password123", "admin")
            .await;

        create_dlq_entry(&app, 20, "operation_task", false).await;
        create_dlq_entry(&app, 21, "operation_task", true).await;

        let res = app
            .get_with_token(&format!("{}?resolved=false", routes::DLQ), &admin_token)
            .await;
        assert_eq!(res.status, 200);
        let data = res.body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["resolved"], false);

        let res = app
            .get_with_token(&format!("{}?resolved=true", routes::DLQ), &admin_token)
            .await;
        assert_eq!(res.status, 200);
        let data = res.body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["resolved"], true);
    }

    #[tokio::test]
    async fn returns_pagination_info() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin4", "password123", "admin")
            .await;

        for i in 0..25 {
            create_dlq_entry(&app, 100 + i, "operation_task", false).await;
        }

        let res = app
            .get_with_token(&format!("{}?per_page=10&page=1", routes::DLQ), &admin_token)
            .await;
        assert_eq!(res.status, 200);

        let data = res.body["data"].as_array().unwrap();
        assert_eq!(data.len(), 10);
        assert_eq!(res.body["pagination"]["page"], 1);
        assert_eq!(res.body["pagination"]["per_page"], 10);
        assert!(res.body["pagination"]["total"].as_u64().unwrap() >= 25);
        assert!(res.body["pagination"]["total_pages"].as_u64().unwrap() >= 3);
    }

    #[tokio::test]
    async fn unauthenticated_user_cannot_list_dlq() {
        let app = TestApp::spawn().await;

        let res = app.get_without_token(routes::DLQ).await;
        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "TOKEN_MISSING");
    }

    #[tokio::test]
    async fn contestant_cannot_list_dlq() {
        let app = TestApp::spawn().await;
        let contestant_token = app
            .create_authenticated_user("contestant1", "password123")
            .await;

        let res = app.get_with_token(routes::DLQ, &contestant_token).await;
        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn rejects_invalid_message_type_parameter() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_dir", "password123", "admin")
            .await;

        let res = app
            .get_with_token(
                &format!("{}?message_type=invalid", routes::DLQ),
                &admin_token,
            )
            .await;
        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
        assert!(
            res.body["message"]
                .as_str()
                .unwrap()
                .contains("Invalid message_type")
        );
    }
}

mod dlq_stats {
    use super::*;

    #[tokio::test]
    async fn admin_can_get_dlq_stats() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin5", "password123", "admin")
            .await;

        create_dlq_entry(&app, 200, "operation_task", false).await;
        create_dlq_entry(&app, 201, "stuck_submission", false).await;
        create_dlq_entry(&app, 202, "operation_task", true).await;

        let res = app.get_with_token(routes::DLQ_STATS, &admin_token).await;
        assert_eq!(res.status, 200);

        assert!(res.body["total_unresolved"].as_u64().unwrap() >= 2);
        assert!(res.body["total_resolved"].as_u64().unwrap() >= 1);

        let by_type = &res.body["unresolved_by_message_type"];
        assert!(by_type.is_object());
        assert!(by_type["operation_task"].as_u64().unwrap() >= 1);
        assert!(by_type["stuck_submission"].as_u64().unwrap() >= 1);
    }

    #[tokio::test]
    async fn unauthenticated_user_cannot_get_stats() {
        let app = TestApp::spawn().await;

        let res = app.get_without_token(routes::DLQ_STATS).await;
        assert_eq!(res.status, 401);
    }

    #[tokio::test]
    async fn contestant_cannot_get_stats() {
        let app = TestApp::spawn().await;
        let contestant_token = app
            .create_authenticated_user("contestant2", "password123")
            .await;

        let res = app
            .get_with_token(routes::DLQ_STATS, &contestant_token)
            .await;
        assert_eq!(res.status, 403);
    }
}

mod dlq_detail {
    use super::*;

    #[tokio::test]
    async fn admin_can_get_dlq_message_detail() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin6", "password123", "admin")
            .await;

        let dlq_id = create_dlq_entry(&app, 300, "operation_task", false).await;

        let res = app
            .get_with_token(&routes::dlq_message(dlq_id), &admin_token)
            .await;
        assert_eq!(res.status, 200);

        assert_eq!(res.body["id"], dlq_id);
        assert_eq!(res.body["submission_id"], 300);
        assert_eq!(res.body["message_type"], "operation_task");
        assert!(res.body["payload"].is_object());
        assert!(res.body["retry_history"].is_array());
    }

    #[tokio::test]
    async fn returns_404_for_nonexistent_message() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin7", "password123", "admin")
            .await;

        let res = app
            .get_with_token(&routes::dlq_message(99999), &admin_token)
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn contestant_cannot_get_dlq_detail() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin8", "password123", "admin")
            .await;
        let contestant_token = app
            .create_authenticated_user("contestant3", "password123")
            .await;

        let dlq_id = create_dlq_entry(&app, 301, "operation_task", false).await;

        let res = app
            .get_with_token(&routes::dlq_message(dlq_id), &admin_token)
            .await;
        assert_eq!(res.status, 200);

        let res = app
            .get_with_token(&routes::dlq_message(dlq_id), &contestant_token)
            .await;
        assert_eq!(res.status, 403);
    }
}

mod dlq_resolve {
    use super::*;

    #[tokio::test]
    async fn admin_can_resolve_dlq_message() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin9", "password123", "admin")
            .await;

        let dlq_id = create_dlq_entry(&app, 400, "operation_task", false).await;

        let res = app
            .delete_with_token(&routes::dlq_message(dlq_id), &admin_token)
            .await;
        assert_eq!(res.status, 204);

        let msg = dead_letter_message::Entity::find_by_id(dlq_id)
            .one(&app.db)
            .await
            .expect("DB query failed")
            .expect("Message should exist");
        assert!(msg.resolved);
        assert!(msg.resolved_at.is_some());
    }

    #[tokio::test]
    async fn resolving_already_resolved_message_is_idempotent() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin10", "password123", "admin")
            .await;

        let dlq_id = create_dlq_entry(&app, 401, "operation_task", true).await;

        let res = app
            .delete_with_token(&routes::dlq_message(dlq_id), &admin_token)
            .await;
        assert_eq!(res.status, 204);
    }

    #[tokio::test]
    async fn returns_404_for_nonexistent_message() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin11", "password123", "admin")
            .await;

        let res = app
            .delete_with_token(&routes::dlq_message(99998), &admin_token)
            .await;
        assert_eq!(res.status, 404);
    }

    #[tokio::test]
    async fn contestant_cannot_resolve_dlq_message() {
        let app = TestApp::spawn().await;
        let contestant_token = app
            .create_authenticated_user("contestant4", "password123")
            .await;

        let dlq_id = create_dlq_entry(&app, 402, "operation_task", false).await;

        let res = app
            .delete_with_token(&routes::dlq_message(dlq_id), &contestant_token)
            .await;
        assert_eq!(res.status, 403);
    }
}

mod dlq_retry {
    use super::*;

    #[tokio::test]
    async fn admin_can_retry_stuck_submission() {
        let app = TestApp::spawn_with_options(SpawnOptions {
            disable_claim_fiber: true,
            ..Default::default()
        })
        .await;
        let admin_token = app
            .create_user_with_role("admin_retry1", "password123", "admin")
            .await;

        let problem_id = app.create_problem(&admin_token, "Retry Test Problem").await;

        let sub_id = insert_submission(&app, 1, problem_id, SubmissionStatus::SystemError).await;
        // Its system-error retries are spent, so the SystemError stands.
        submission::ActiveModel {
            id: Set(sub_id),
            system_error_retries: Set(6),
            ..Default::default()
        }
        .update(&app.db)
        .await
        .expect("spend retry budget");

        let dlq_id = create_dlq_entry(&app, sub_id, "stuck_submission", false).await;

        let res = app
            .post_with_token(&routes::dlq_retry(dlq_id), &json!({}), &admin_token)
            .await;
        assert_eq!(res.status, 200);

        let sub = submission::Entity::find_by_id(sub_id)
            .one(&app.db)
            .await
            .expect("DB query failed")
            .expect("Submission should exist");
        assert_eq!(sub.status, SubmissionStatus::Queued);
        assert!(sub.error_code.is_none());
        assert!(sub.error_message.is_none());
        assert_eq!(
            sub.system_error_retries, 0,
            "a DLQ retry starts a new judging attempt with a fresh retry budget"
        );
        let judgement = app.current_judgement(sub_id).await;
        assert_eq!(judgement.status, SubmissionStatus::Queued);
        assert_eq!(judgement.dispatch_retries, 0);

        let msg = dead_letter_message::Entity::find_by_id(dlq_id)
            .one(&app.db)
            .await
            .expect("DB query failed")
            .expect("DLQ message should exist");
        assert!(msg.resolved);
        assert!(msg.resolved_at.is_some());
    }

    #[tokio::test]
    async fn cannot_retry_already_resolved_message() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin12", "password123", "admin")
            .await;

        let dlq_id = create_dlq_entry(&app, 500, "stuck_submission", true).await;

        let res = app
            .post_with_token(&routes::dlq_retry(dlq_id), &json!({}), &admin_token)
            .await;
        assert_eq!(res.status, 409);
        assert_eq!(res.body["code"], "CONFLICT");
    }

    #[tokio::test]
    async fn cannot_retry_operation_task_messages() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin13", "password123", "admin")
            .await;

        let dlq_id = create_dlq_entry(&app, 501, "operation_task", false).await;

        let res = app
            .post_with_token(&routes::dlq_retry(dlq_id), &json!({}), &admin_token)
            .await;
        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
        assert!(
            res.body["message"]
                .as_str()
                .unwrap()
                .contains("stuck_submission")
        );
    }

    #[tokio::test]
    async fn cannot_retry_submission_in_judged_state() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_retry2", "password123", "admin")
            .await;

        let problem_id = app
            .create_problem(&admin_token, "Retry State Problem")
            .await;

        let sub_id = insert_submission(&app, 1, problem_id, SubmissionStatus::Judged).await;
        let dlq_id = create_dlq_entry(&app, sub_id, "stuck_submission", false).await;

        let res = app
            .post_with_token(&routes::dlq_retry(dlq_id), &json!({}), &admin_token)
            .await;
        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
        assert!(
            res.body["message"]
                .as_str()
                .unwrap()
                .contains("cannot be retried")
        );
    }

    #[tokio::test]
    async fn returns_404_for_nonexistent_message() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin14", "password123", "admin")
            .await;

        let res = app
            .post_with_token(&routes::dlq_retry(99997), &json!({}), &admin_token)
            .await;
        assert_eq!(res.status, 404);
    }

    #[tokio::test]
    async fn contestant_cannot_retry_dlq_message() {
        let app = TestApp::spawn().await;
        let contestant_token = app
            .create_authenticated_user("contestant5", "password123")
            .await;

        let dlq_id = create_dlq_entry(&app, 502, "stuck_submission", false).await;

        let res = app
            .post_with_token(&routes::dlq_retry(dlq_id), &json!({}), &contestant_token)
            .await;
        assert_eq!(res.status, 403);
    }

    #[tokio::test]
    async fn retry_returns_404_when_submission_not_found() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin15", "password123", "admin")
            .await;

        let dlq_id = create_dlq_entry(&app, 99999, "stuck_submission", false).await;

        let res = app
            .post_with_token(&routes::dlq_retry(dlq_id), &json!({}), &admin_token)
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }
}

mod bulk_retry_dlq {
    use super::*;

    #[tokio::test]
    async fn admin_can_bulk_retry_stuck_submissions() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_br1", "password123", "admin")
            .await;

        let problem_id = app.create_problem(&admin_token, "Bulk Retry Problem").await;

        let sub1 = insert_submission(&app, 1, problem_id, SubmissionStatus::SystemError).await;
        let sub2 = insert_submission(&app, 1, problem_id, SubmissionStatus::SystemError).await;

        let dlq1 = create_dlq_entry(&app, sub1, "stuck_submission", false).await;
        let dlq2 = create_dlq_entry(&app, sub2, "stuck_submission", false).await;

        let res = app
            .post_with_token(
                routes::DLQ_BULK_RETRY,
                &json!({"message_ids": [dlq1, dlq2]}),
                &admin_token,
            )
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["retried"], 2);
        assert_eq!(res.body["skipped"], 0);

        // Handler spawns dispatch_to_plugin after commit; status races and
        // can flip back to SystemError before we read. Stable observables:
        // error_code is no longer "STUCK_JOB", and the DLQ entry is resolved.
        let s1 = submission::Entity::find_by_id(sub1)
            .one(&app.db)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(s1.error_code.as_deref(), Some("STUCK_JOB"));

        let s2 = submission::Entity::find_by_id(sub2)
            .one(&app.db)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(s2.error_code.as_deref(), Some("STUCK_JOB"));

        let m1 = dead_letter_message::Entity::find_by_id(dlq1)
            .one(&app.db)
            .await
            .unwrap()
            .unwrap();
        assert!(m1.resolved);
        let m2 = dead_letter_message::Entity::find_by_id(dlq2)
            .one(&app.db)
            .await
            .unwrap()
            .unwrap();
        assert!(m2.resolved);
    }

    #[tokio::test]
    async fn bulk_retry_skips_operation_task_messages() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_br1b", "password123", "admin")
            .await;

        let dlq1 = create_dlq_entry(&app, 600, "operation_task", false).await;
        let dlq2 = create_dlq_entry(&app, 601, "operation_task", false).await;

        let res = app
            .post_with_token(
                routes::DLQ_BULK_RETRY,
                &json!({"message_ids": [dlq1, dlq2]}),
                &admin_token,
            )
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["retried"], 0);
        assert_eq!(res.body["skipped"], 2);
    }

    #[tokio::test]
    async fn returns_validation_error_for_empty_ids() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_br2", "password123", "admin")
            .await;

        let res = app
            .post_with_token(
                routes::DLQ_BULK_RETRY,
                &json!({"message_ids": []}),
                &admin_token,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn returns_validation_error_for_duplicate_ids() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_br3", "password123", "admin")
            .await;

        let res = app
            .post_with_token(
                routes::DLQ_BULK_RETRY,
                &json!({"message_ids": [1, 1]}),
                &admin_token,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn returns_validation_error_when_both_modes_provided() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_br4", "password123", "admin")
            .await;

        let res = app
            .post_with_token(
                routes::DLQ_BULK_RETRY,
                &json!({"message_ids": [1], "message_type": "stuck_submission"}),
                &admin_token,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn returns_validation_error_when_neither_mode_provided() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_br5", "password123", "admin")
            .await;

        let res = app
            .post_with_token(routes::DLQ_BULK_RETRY, &json!({}), &admin_token)
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn contestant_cannot_bulk_retry() {
        let app = TestApp::spawn().await;
        let contestant_token = app
            .create_authenticated_user("contestant_br6", "password123")
            .await;

        let res = app
            .post_with_token(
                routes::DLQ_BULK_RETRY,
                &json!({"message_ids": [1]}),
                &contestant_token,
            )
            .await;

        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn filter_mode_retries_matching_stuck_submissions() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_br7", "password123", "admin")
            .await;

        let problem_id = app
            .create_problem(&admin_token, "Filter Retry Problem")
            .await;

        let sub1 = insert_submission(&app, 1, problem_id, SubmissionStatus::SystemError).await;
        create_dlq_entry(&app, sub1, "stuck_submission", false).await;

        create_dlq_entry(&app, 9999, "operation_task", false).await;

        let res = app
            .post_with_token(
                routes::DLQ_BULK_RETRY,
                &json!({"message_type": "stuck_submission"}),
                &admin_token,
            )
            .await;

        assert_eq!(res.status, 200);
        assert!(res.body["retried"].as_u64().unwrap() >= 1);
    }
}

mod bulk_delete_dlq {
    use super::*;

    #[tokio::test]
    async fn admin_can_bulk_delete_dlq_messages() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_bd1", "password123", "admin")
            .await;

        let dlq1 = create_dlq_entry(&app, 700, "operation_task", false).await;
        let dlq2 = create_dlq_entry(&app, 701, "stuck_submission", false).await;
        let dlq3 = create_dlq_entry(&app, 702, "operation_task", false).await;

        let res = app
            .delete_with_body_and_token(
                routes::DLQ_BULK,
                &json!({"message_ids": [dlq1, dlq2]}),
                &admin_token,
            )
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["deleted"], 2);

        let msg1 = dead_letter_message::Entity::find_by_id(dlq1)
            .one(&app.db)
            .await
            .expect("DB query failed")
            .expect("msg1 should exist");
        assert!(msg1.resolved);

        let msg2 = dead_letter_message::Entity::find_by_id(dlq2)
            .one(&app.db)
            .await
            .expect("DB query failed")
            .expect("msg2 should exist");
        assert!(msg2.resolved);

        let msg3 = dead_letter_message::Entity::find_by_id(dlq3)
            .one(&app.db)
            .await
            .expect("DB query failed")
            .expect("msg3 should exist");
        assert!(!msg3.resolved);
    }

    #[tokio::test]
    async fn skips_already_resolved_messages() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_bd2", "password123", "admin")
            .await;

        let resolved_id = create_dlq_entry(&app, 710, "operation_task", true).await;
        let unresolved_id = create_dlq_entry(&app, 711, "operation_task", false).await;

        let res = app
            .delete_with_body_and_token(
                routes::DLQ_BULK,
                &json!({"message_ids": [resolved_id, unresolved_id]}),
                &admin_token,
            )
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["deleted"], 1);
    }

    #[tokio::test]
    async fn returns_validation_error_for_empty_ids() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_bd3", "password123", "admin")
            .await;

        let res = app
            .delete_with_body_and_token(routes::DLQ_BULK, &json!({"message_ids": []}), &admin_token)
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn returns_validation_error_for_duplicate_ids() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_bd4", "password123", "admin")
            .await;

        let res = app
            .delete_with_body_and_token(
                routes::DLQ_BULK,
                &json!({"message_ids": [1, 1]}),
                &admin_token,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn returns_zero_for_nonexistent_ids() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_bd5", "password123", "admin")
            .await;

        let res = app
            .delete_with_body_and_token(
                routes::DLQ_BULK,
                &json!({"message_ids": [99998, 99999]}),
                &admin_token,
            )
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["deleted"], 0);
    }

    #[tokio::test]
    async fn contestant_cannot_bulk_delete() {
        let app = TestApp::spawn().await;
        let contestant_token = app
            .create_authenticated_user("contestant_bd6", "password123")
            .await;

        let res = app
            .delete_with_body_and_token(
                routes::DLQ_BULK,
                &json!({"message_ids": [1]}),
                &contestant_token,
            )
            .await;

        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }
}
