//! A server whose message queue is unreachable reports itself unhealthy and
//! leaves queued work alone: claimed work is dispatched through the queue, so
//! claiming it would only fail it.

use crate::common::{SpawnOptions, TestApp};
use common::SubmissionStatus;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};
use server::dispatcher::claim::{ClaimPass, claim_once};
use server::entity::{submission, user};

fn without_queue() -> SpawnOptions {
    SpawnOptions {
        mq_unreachable: true,
        disable_claim_fiber: true,
        ..Default::default()
    }
}

#[tokio::test]
async fn healthz_is_unavailable_while_redis_is_unreachable() {
    let app = TestApp::spawn_with_options(without_queue()).await;
    let res = app.get_without_token("/healthz").await;
    assert_eq!(res.status, 503, "{}", res.text);
    assert_eq!(res.body["status"], "degraded", "{}", res.text);
    assert_eq!(res.body["mq"], "down", "{}", res.text);
    assert_eq!(res.body["db"], "ok", "{}", res.text);
}

#[tokio::test]
async fn queued_work_is_not_claimed_while_the_queue_is_unreachable() {
    let app = TestApp::spawn_with_options(without_queue()).await;
    let token = app
        .create_user_with_role("queue_outage_admin", "password123", "admin")
        .await;
    let problem_id = app.create_problem(&token, "Queue outage").await;
    let user_id = user::Entity::find()
        .filter(user::Column::Username.eq("queue_outage_admin"))
        .one(&app.db)
        .await
        .unwrap()
        .unwrap()
        .id;
    let row = app
        .insert_submission(submission::ActiveModel {
            files: Set(serde_json::json!([{ "filename": "main.cpp", "content": "int main() {}" }])),
            language: Set("cpp".into()),
            user_id: Set(user_id),
            problem_id: Set(problem_id),
            contest_id: Set(None),
            contest_type: Set("standard".into()),
            status: Set(SubmissionStatus::Queued),
            judge_epoch: Set(0),
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        })
        .await;

    let pass = claim_once(&app.state, "queue-outage", 32, 0).await.unwrap();

    assert_eq!(pass, ClaimPass::QUEUE_DOWN);
    let judgement = app.current_judgement(row.id).await;
    assert_eq!(judgement.status, SubmissionStatus::Queued);
    assert_eq!(judgement.owner_server_id, None);
}
