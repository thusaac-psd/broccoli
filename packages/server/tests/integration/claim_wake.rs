//! The claim fiber claims new work when it is queued, not on its poll tick.

use std::time::Duration;

use crate::common::TestApp;
use common::SubmissionStatus;
use sea_orm::{ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter, Set, Statement};
use server::entity::{submission, user};

/// Waits until a session of the app's database is listening for claim
/// notifications.
async fn wait_until_listening(app: &TestApp) {
    let sql = format!(
        "SELECT COUNT(*)::bigint AS n FROM pg_stat_activity \
         WHERE datname = current_database() AND query ILIKE 'LISTEN%{}%'",
        server::services::judging::CLAIM_CHANNEL
    );
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let row = app
                .db
                .query_one_raw(Statement::from_string(DbBackend::Postgres, sql.clone()))
                .await
                .unwrap()
                .unwrap();
            if row.try_get::<i64>("", "n").unwrap() > 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the claim fiber listens");
}

/// Waits until the submission's current judgement has left `Queued`.
async fn wait_until_claimed(app: &TestApp, submission_id: i32) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while app.current_judgement(submission_id).await.status == SubmissionStatus::Queued {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the submission is claimed");
}

#[tokio::test]
async fn new_work_is_claimed_without_waiting_for_the_poll_tick() {
    let app = TestApp::spawn().await;
    let token = app
        .create_user_with_role("claim_wake_admin", "password123", "admin")
        .await;
    let user_id = user::Entity::find()
        .filter(user::Column::Username.eq("claim_wake_admin"))
        .one(&app.db)
        .await
        .unwrap()
        .unwrap()
        .id;
    let problem_id = app.create_problem(&token, "Claim wake").await;
    let new = || submission::ActiveModel {
        files: Set(serde_json::json!([{ "filename": "main.cpp", "content": "int main() {}" }])),
        language: Set("cpp".into()),
        user_id: Set(user_id),
        problem_id: Set(problem_id),
        contest_type: Set("standard".into()),
        created_at: Set(chrono::Utc::now()),
        ..Default::default()
    };

    // A poll tick that never comes within the test.
    let (cancel, cancelled) = tokio::sync::watch::channel(false);
    let fiber = tokio::spawn(server::dispatcher::claim::run_with_fallback_tick(
        app.state.clone(),
        format!("claim-wake-{}", uuid::Uuid::new_v4()),
        Duration::from_secs(3_600),
        cancelled,
    ));
    wait_until_listening(&app).await;

    // Besides notifications, the fiber passes once at start and once when it
    // starts listening; each claims what is queued then. So by the third
    // submission, queued only after the second was claimed, only that
    // submission's own notification can have woken it.
    for _ in 0..3 {
        let s = app.insert_submission(new()).await;
        wait_until_claimed(&app, s.id).await;
    }

    cancel.send(true).unwrap();
    fiber.await.unwrap();
}
