//! A plugin writes only the judgement or code run its own call was
//! dispatched with: a write naming any other unit, or made outside a
//! dispatch, is refused before it reaches the database.

use crate::common::{TestApp, routes};
use common::SubmissionStatus;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use serde_json::json;
use server::entity::{code_run, problem, submission, user};

/// A problem to write against; nothing judges it.
async fn problem(app: &TestApp, title: &str) -> i32 {
    problem::ActiveModel {
        title: Set(title.into()),
        content: Set(String::new()),
        time_limit: Set(1000),
        memory_limit: Set(262144),
        problem_type: Set("standard".into()),
        checker_format: Set("exact".into()),
        default_contest_type: Set("standard".into()),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert problem")
    .id
}

async fn admin(app: &TestApp, name: &str) -> (String, i32) {
    let token = app
        .create_user_with_role(name, "password123", "admin")
        .await;
    let id = user::Entity::find()
        .filter(user::Column::Username.eq(name))
        .one(&app.db)
        .await
        .unwrap()
        .unwrap()
        .id;
    (token, id)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_cannot_finalize_a_judgement_it_was_not_dispatched_with() {
    let app = TestApp::spawn_with_plugins().await;
    let (_, user_id) = admin(&app, "foreign_write_admin").await;
    let problem_id = problem(&app, "Foreign write").await;
    let sub = app
        .insert_submission(submission::ActiveModel {
            files: Set(json!([{ "filename": "main.cpp", "content": "int main() {}" }])),
            language: Set("cpp".into()),
            user_id: Set(user_id),
            problem_id: Set(problem_id),
            contest_type: Set("standard".into()),
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        })
        .await;
    app.put_in_flight(sub.id, SubmissionStatus::Running, "some-server", None, None)
        .await;
    let judgement = app.current_judgement(sub.id).await;

    let res = app
        .post_without_token(
            &routes::plugin_proxy("server-plugin", "write/submission_update"),
            &json!({
                "submission_id": sub.id,
                "judgement_id": judgement.id,
                "judge_epoch": judgement.judge_epoch,
                "status": "Judged",
                "verdict": "Accepted",
                "score": 100.0,
            }),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert!(res.body["error"].is_string(), "refused: {}", res.text);
    assert!(res.body["data"].is_null(), "{}", res.text);

    let after = app.current_judgement(sub.id).await;
    assert_eq!(after.status, SubmissionStatus::Running);
    assert!(!after.is_finalized);
    assert_eq!(after.verdict, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_cannot_finish_a_code_run_it_was_not_dispatched_with() {
    let app = TestApp::spawn_with_plugins().await;
    let (_, user_id) = admin(&app, "foreign_run_admin").await;
    let problem_id = problem(&app, "Foreign run").await;
    let run = code_run::ActiveModel {
        files: Set(json!([{ "filename": "main.cpp", "content": "int main() {}" }])),
        language: Set("cpp".into()),
        user_id: Set(user_id),
        problem_id: Set(problem_id),
        contest_type: Set("standard".into()),
        status: Set(SubmissionStatus::Running),
        custom_test_cases: Set(json!([])),
        created_at: Set(chrono::Utc::now()),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert code run");

    let res = app
        .post_without_token(
            &routes::plugin_proxy("server-plugin", "write/code_run_update"),
            &json!({
                "code_run_id": run.id,
                "judge_epoch": run.judge_epoch,
                "status": "Judged",
                "verdict": "Accepted",
            }),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert!(res.body["error"].is_string(), "refused: {}", res.text);

    let after = code_run::Entity::find_by_id(run.id)
        .one(&app.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.status, SubmissionStatus::Running);
    assert_eq!(after.verdict, None);
}
