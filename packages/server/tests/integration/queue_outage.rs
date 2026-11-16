//! A server leaves queued work alone while nothing could run it: its message
//! queue is unreachable (it also reports itself unhealthy), or live workers
//! are known and none of them takes work. Claimed work would only fail or
//! wait unseen at the queue; queued, it visibly waits.

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

async fn queued_submission(app: &TestApp, admin: &str) -> submission::Model {
    let token = app
        .create_user_with_role(admin, "password123", "admin")
        .await;
    let problem_id = app.create_problem(&token, "Queue outage").await;
    let user_id = user::Entity::find()
        .filter(user::Column::Username.eq(admin))
        .one(&app.db)
        .await
        .unwrap()
        .unwrap()
        .id;
    app.insert_submission(submission::ActiveModel {
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
    .await
}

#[tokio::test]
async fn queued_work_is_not_claimed_while_the_queue_is_unreachable() {
    let app = TestApp::spawn_with_options(without_queue()).await;
    let row = queued_submission(&app, "queue_outage_admin").await;

    let pass = claim_once(&app.state, "queue-outage", 32, 0).await.unwrap();

    assert_eq!(pass, ClaimPass::CANNOT_RUN);
    let judgement = app.current_judgement(row.id).await;
    assert_eq!(judgement.status, SubmissionStatus::Queued);
    assert_eq!(judgement.owner_server_id, None);
}

fn heartbeat(missing_platform_tools: &[&str]) -> common::worker::HeartbeatPayload {
    common::worker::HeartbeatPayload {
        id: "w".into(),
        started_at: chrono::Utc::now(),
        last_seen: chrono::Utc::now(),
        in_flight: 0,
        max_concurrency: Some(1),
        fairness_mode: "unknown".into(),
        sandbox_backend: "test".into(),
        version: "test".into(),
        hostname: None,
        ip_addresses: Vec::new(),
        os: None,
        arch: None,
        cpu_count: None,
        pid: None,
        missing_platform_tools: missing_platform_tools
            .iter()
            .map(|t| t.to_string())
            .collect(),
    }
}

#[tokio::test]
async fn queued_work_waits_while_no_live_worker_takes_work() {
    let app = TestApp::spawn_with_options(SpawnOptions {
        disable_claim_fiber: true,
        ..Default::default()
    })
    .await;
    let row = queued_submission(&app, "no_worker_admin").await;

    app.state
        .worker_slots
        .observe(&[heartbeat(&["broccoli-compare"])], 1);
    let pass = claim_once(&app.state, "no-worker", 32, 0).await.unwrap();
    assert_eq!(pass, ClaimPass::CANNOT_RUN);
    let judgement = app.current_judgement(row.id).await;
    assert_eq!(judgement.status, SubmissionStatus::Queued);
    assert_eq!(judgement.owner_server_id, None);

    app.state
        .worker_slots
        .observe(&[heartbeat(&["broccoli-compare"]), heartbeat(&[])], 1);
    let pass = claim_once(&app.state, "no-worker", 32, 0).await.unwrap();
    assert_eq!(pass.judgements.0, 1, "{pass:?}");
}
