//! The claim fiber's in-flight cap counts, per server, the judgements it has
//! claimed and not finished. Uses a unique owner id so parallel tests that
//! share the database (and the fixture's own server id) cannot interfere.

use crate::common::TestApp;
use common::SubmissionStatus;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use server::entity::{submission, submission_judgement, user};

async fn insert(
    app: &TestApp,
    problem_id: i32,
    user_id: i32,
    owner: Option<&str>,
    status: SubmissionStatus,
) {
    let sub = submission::ActiveModel {
        files: Set(serde_json::json!([{ "filename": "main.cpp", "content": "int main() {}" }])),
        language: Set("cpp".into()),
        user_id: Set(user_id),
        problem_id: Set(problem_id),
        contest_id: Set(None),
        contest_type: Set("standard".into()),
        status: Set(status),
        judge_epoch: Set(0),
        created_at: Set(chrono::Utc::now()),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert submission");
    submission_judgement::ActiveModel {
        submission_id: Set(sub.id),
        version: Set(1),
        is_current: Set(true),
        is_finalized: Set(status.is_terminal()),
        status: Set(status),
        owner_server_id: Set(owner.map(str::to_string)),
        created_at: Set(chrono::Utc::now()),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert judgement");
}

#[tokio::test]
async fn in_flight_counts_only_this_servers_unfinished_judgements() {
    let app = TestApp::spawn().await;
    let token = app
        .create_user_with_role("claim_cap_admin", "password123", "admin")
        .await;
    let user_id = user::Entity::find()
        .filter(user::Column::Username.eq("claim_cap_admin"))
        .one(&app.db)
        .await
        .unwrap()
        .unwrap()
        .id;
    let problem = app.create_problem(&token, "Claim cap").await;
    let me = format!("claim-cap-{}", uuid::Uuid::new_v4());
    let other = format!("claim-cap-other-{}", uuid::Uuid::new_v4());

    // Mine and still judging: counted.
    for status in [
        SubmissionStatus::Pending,
        SubmissionStatus::Compiling,
        SubmissionStatus::Running,
    ] {
        insert(&app, problem, user_id, Some(&me), status).await;
    }
    // Mine but finished, another server's, and not yet claimed: not counted.
    insert(&app, problem, user_id, Some(&me), SubmissionStatus::Judged).await;
    insert(
        &app,
        problem,
        user_id,
        Some(&me),
        SubmissionStatus::SystemError,
    )
    .await;
    insert(
        &app,
        problem,
        user_id,
        Some(&other),
        SubmissionStatus::Running,
    )
    .await;
    insert(&app, problem, user_id, None, SubmissionStatus::Queued).await;

    let n = server::services::judging::count_in_flight(&app.db, &me)
        .await
        .expect("count in flight");
    assert_eq!(n, 3);
}
