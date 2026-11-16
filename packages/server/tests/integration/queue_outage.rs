//! A server leaves queued work alone while nothing could run it: its message
//! queue is unreachable (it also reports itself unhealthy), or live workers
//! are known and none of them takes work. Claimed work would only fail or
//! wait unseen at the queue; queued, it visibly waits.

use std::sync::Arc;
use std::time::Duration;

use crate::common::{SpawnOptions, TestApp};
use broccoli_server_sdk::types::OperationTask;
use common::SubmissionStatus;
use common::liveness::NodeRole;
use common::node_identity::NodeClaim;
use common::storage::filesystem::FilesystemBlobStore;
use common::worker::OperationFault;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};
use server::consumers::operation_result::hand_back_while_none_accepting;
use server::dispatcher::claim::{ClaimPass, claim_once};
use server::dispatcher::fanout::WorkerSlots;
use server::entity::{submission, user};
use server::host_funcs::context::OperationHostDeps;
use server::registry::OperationBatches;
use server::services::judging::DispatchFaults;
use server::services::operation_batch::{
    OperationTaskPublisher, next_operation_result_async, start_operation_batch,
};

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

/// Waits until `slots` satisfy `done`. Each wait is for a change the test
/// caused; `ANNOUNCED_WITHIN` only bounds a broken run, well short of the
/// re-read at a key's expiry (`liveness::TTL`) that would find the change
/// without any announcement.
async fn until(slots: &WorkerSlots, done: impl Fn(&WorkerSlots) -> bool) {
    const ANNOUNCED_WITHIN: Duration = Duration::from_secs(5);
    loop {
        let changed = slots.changed();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if done(slots) {
            return;
        }
        tokio::time::timeout(ANNOUNCED_WITHIN, changed)
            .await
            .expect("the fleet change is announced");
    }
}

fn payload(missing_platform_tools: &[&str]) -> serde_json::Value {
    serde_json::to_value(heartbeat(missing_platform_tools)).unwrap()
}

/// The claim gate's view of the fleet follows each worker's announcements:
/// the gate closes the moment the last worker that takes work stops taking
/// it, and opens again when it leaves, without waiting for a periodic read.
#[tokio::test]
async fn the_claim_gate_follows_what_workers_announce() {
    let url = format!(
        "redis://127.0.0.1:{}",
        test_support::shared_redis_port().await
    );
    let namespace = format!("gate-{}", uuid::Uuid::new_v4());
    let client = redis::Client::open(url.as_str()).unwrap();
    let slots = WorkerSlots::default();
    let follow = tokio::spawn(slots.clone().follow(
        Arc::new(client.clone()),
        namespace.clone(),
        Duration::from_secs(3600),
    ));

    let claim = NodeClaim::claim_at_boot(client, &namespace, NodeRole::Worker, "w", payload(&[]))
        .await
        .unwrap();
    let claim = worker::heartbeat::announce_capacity(claim);
    claim.renew(payload(&[])).await.unwrap();
    until(&slots, |s| s.get() == 1).await;

    claim.renew(payload(&[])).await.unwrap();
    claim.renew(payload(&["broccoli-compare"])).await.unwrap();
    until(&slots, WorkerSlots::none_accepting).await;
    assert_eq!(slots.get(), 0);

    claim.release().await.unwrap();
    until(&slots, |s| !s.none_accepting()).await;
    assert_eq!(slots.get(), 0, "no live worker");
    follow.abort();
}

/// Holds every task it is given: a queue no worker reads.
struct UnreadQueue;

#[async_trait::async_trait]
impl OperationTaskPublisher for UnreadQueue {
    async fn publish_operation_task(
        &self,
        _task: &common::worker::Task,
        _pin: Option<&str>,
    ) -> anyhow::Result<mq::Route> {
        Ok(mq::Route::Shared)
    }
}

async fn operation_deps(slots: &WorkerSlots, faults: &DispatchFaults) -> OperationHostDeps {
    let blobs = tempfile::tempdir().unwrap();
    OperationHostDeps {
        plugin_manager: None,
        operation_publisher: Some(Arc::new(UnreadQueue)),
        operation_batches: OperationBatches::default(),
        operation_waiters: Arc::new(dashmap::DashMap::new()),
        server_id: "hand-back".into(),
        blob_store: Arc::new(
            FilesystemBlobStore::new(blobs.keep(), 1024 * 1024)
                .await
                .unwrap(),
        ),
        metrics: None,
        evaluate_batches: Default::default(),
        judgement: None,
        faults: Some(faults.clone()),
        worker_slots: slots.clone(),
    }
}

fn operation() -> OperationTask {
    OperationTask {
        environments: Vec::new(),
        tasks: Vec::new(),
        channels: Vec::new(),
        target_worker_id: None,
        evaluate_batch_id: None,
        test_case_id: None,
    }
}

/// Work already at the queue when the last worker that takes work stops
/// taking it fails at once as `NO_CAPABLE_WORKER`, a fault whose system
/// error is handed back to `Queued` spending no retry, instead of waiting at
/// a queue no worker reads until its result wait runs out. An operation
/// started while it stays so is refused the same way.
#[tokio::test]
async fn work_at_the_queue_is_handed_back_when_no_worker_takes_work() {
    let slots = WorkerSlots::default();
    slots.observe(&[heartbeat(&[])], 1);
    let faults = DispatchFaults::default();
    let deps = operation_deps(&slots, &faults).await;
    let hand_back = tokio::spawn(hand_back_while_none_accepting(
        slots.clone(),
        deps.operation_waiters.clone(),
        None,
    ));
    let batch = start_operation_batch("p".into(), deps.clone(), vec![operation()])
        .await
        .unwrap();
    assert!(faults.get().is_none());

    slots.observe(&[heartbeat(&["broccoli-compare"])], 1);
    let result = next_operation_result_async("p", &deps.operation_batches, &batch, Duration::MAX)
        .await
        .unwrap()
        .expect("the operation is answered");
    assert!(!result.success);
    assert_eq!(result.fault, Some(OperationFault::NoCapableWorker));
    assert_eq!(
        faults.get().map(|f| f.fault),
        Some(OperationFault::NoCapableWorker.into())
    );
    assert!(deps.operation_waiters.is_empty());

    let refused = DispatchFaults::default();
    let deps = operation_deps(&slots, &refused).await;
    start_operation_batch("p".into(), deps.clone(), vec![operation()])
        .await
        .expect_err("nothing can run it");
    assert_eq!(
        refused.get().map(|f| f.fault),
        Some(OperationFault::NoCapableWorker.into())
    );
    assert!(deps.operation_waiters.is_empty());
    hand_back.abort();
}
