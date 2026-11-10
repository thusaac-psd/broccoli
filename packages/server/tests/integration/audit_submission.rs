//! Submission-lifecycle defects found by the kernel audit (Nov 2026).
//!
//! Each `#[ignore]`d test reproduces a bug and asserts the CORRECT behaviour,
//! so it fails today. Run the whole set with:
//!
//! `cargo test -p server --test integration audit_submission -- --ignored`
//!
//! When fixing a bug, delete its `#[ignore]` so the test becomes a regression
//! guard.

use chrono::Utc;
use common::{SubmissionStatus, Verdict};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, Set,
};
use serde_json::json;
use server::entity::{submission, submission_judgement};

use crate::common::{TestApp, routes};

/// A judged standalone submission at `judge_epoch = 11` with one finalized
/// current judgement, i.e. the normal state before an admin rejudges.
async fn seed_judged(app: &TestApp, token: &str, problem_id: i32, retry_count: i32) -> i32 {
    let user_id = app.get_with_token(routes::ME, token).await.id();
    let now = Utc::now();
    let sub = submission::ActiveModel {
        files: Set(json!([{"filename": "main.cpp", "content": "int main(){}"}])),
        language: Set("cpp".into()),
        user_id: Set(user_id),
        problem_id: Set(problem_id),
        contest_id: Set(None),
        contest_type: Set("standard".into()),
        status: Set(SubmissionStatus::Judged),
        verdict: Set(Some(Verdict::Accepted)),
        score: Set(Some(100.0)),
        judge_epoch: Set(11),
        retry_count: Set(retry_count),
        created_at: Set(now),
        judged_at: Set(Some(now)),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert submission");
    submission_judgement::ActiveModel {
        submission_id: Set(sub.id),
        version: Set(11),
        is_current: Set(true),
        is_finalized: Set(true),
        status: Set(SubmissionStatus::Judged),
        verdict: Set(Some(Verdict::Accepted)),
        score: Set(Some(100.0)),
        judge_epoch: Set(11),
        retry_count: Set(retry_count),
        created_at: Set(now),
        finalized_at: Set(Some(now)),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert judgement");
    sub.id
}

async fn judgements(app: &TestApp, sid: i32) -> Vec<submission_judgement::Model> {
    submission_judgement::Entity::find()
        .filter(submission_judgement::Column::SubmissionId.eq(sid))
        .order_by_asc(submission_judgement::Column::Version)
        .all(&app.db)
        .await
        .expect("db")
}

/// BUG (med, found independently by two reviewers): a new judgement's epoch is
/// `sub.judge_epoch + 1`, but a DEFERRED rejudge (`apply_immediately:false`)
/// creates its judgement at E+1 without advancing `sub.judge_epoch`. A later
/// immediate rejudge therefore also picks E+1. Epoch is the fencing token:
/// `mark_submission_system_error_with_epoch(sub, Some(E+1))` from the deferred
/// run's dispatch failure then finalizes the LIVE immediate judgement as
/// SystemError, and the `after_judging:{id}:{epoch}` hook dedupe collides.
/// Location: handlers/submission/rejudge.rs (epoch = sub.judge_epoch + 1),
/// handlers/submission/dispatch.rs, consumers/mod.rs.
#[tokio::test]
#[ignore = "BUG: deferred + immediate rejudge allocate the same judge_epoch (fencing token collision)"]
async fn rejudge_epochs_are_unique() {
    let app = TestApp::spawn().await;
    let token = app
        .create_user_with_role("as_admin", "password123", "admin")
        .await;
    let pid = app.create_problem(&token, "P").await;
    let sid = seed_judged(&app, &token, pid, 1).await;

    let deferred = app
        .post_with_token(
            &routes::submission_rejudge(sid),
            &json!({"apply_immediately": false}),
            &token,
        )
        .await;
    assert!(deferred.status < 300, "deferred rejudge: {}", deferred.text);
    let immediate = app
        .post_with_token(&routes::submission_rejudge(sid), &json!({}), &token)
        .await;
    assert!(
        immediate.status < 300,
        "immediate rejudge: {}",
        immediate.text
    );

    let rows = judgements(&app, sid).await;
    let epochs: Vec<i32> = rows.iter().map(|j| j.judge_epoch).collect();
    let mut dedup = epochs.clone();
    dedup.sort_unstable();
    dedup.dedup();
    assert_eq!(
        dedup.len(),
        epochs.len(),
        "duplicate judge_epoch across judgements: {epochs:?}"
    );
}

/// BUG (med): rejudging an in-flight submission flips its current judgement
/// to `is_current=false` but leaves it `is_finalized=false`. Its pending
/// finalize is then rejected (`submission_update` requires is_current or max
/// version), so it never finalizes; the stuck detector re-runs it up to
/// `max_stuck_retries` times and finally DLQs it. The stuck-retry path
/// finalizes the judgement it supersedes; the rejudge path does not.
/// Location: handlers/submission/dispatch.rs `open_rejudge_judgement`.
#[tokio::test]
#[ignore = "BUG: rejudging an in-flight submission strands the superseded judgement unfinalized"]
async fn rejudge_finalizes_the_superseded_inflight_judgement() {
    let app = TestApp::spawn().await;
    let token = app
        .create_user_with_role("as_admin", "password123", "admin")
        .await;
    let pid = app.create_problem(&token, "P").await;
    let user_id = app.get_with_token(routes::ME, &token).await.id();
    let now = Utc::now();
    let sub = submission::ActiveModel {
        files: Set(json!([{"filename": "main.cpp", "content": "int main(){}"}])),
        language: Set("cpp".into()),
        user_id: Set(user_id),
        problem_id: Set(pid),
        contest_type: Set("standard".into()),
        status: Set(SubmissionStatus::Running),
        judge_epoch: Set(5),
        retry_count: Set(1),
        owner_server_id: Set(Some("server-x".into())),
        lease_heartbeat_at: Set(Some(now)),
        leased_at: Set(Some(now)),
        created_at: Set(now),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert submission");
    let j1 = submission_judgement::ActiveModel {
        submission_id: Set(sub.id),
        version: Set(1),
        is_current: Set(true),
        is_finalized: Set(false),
        status: Set(SubmissionStatus::Running),
        judge_epoch: Set(5),
        retry_count: Set(1),
        created_at: Set(now),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert running judgement");

    let res = app
        .post_with_token(&routes::submission_rejudge(sub.id), &json!({}), &token)
        .await;
    if (400..500).contains(&res.status) {
        // Refusing to rejudge an in-flight submission is an acceptable fix.
        return;
    }
    assert!(res.status < 300, "rejudge: {}", res.text);

    let old = submission_judgement::Entity::find_by_id(j1.id)
        .one(&app.db)
        .await
        .expect("db")
        .expect("j1");
    assert!(!old.is_current, "control: j1 superseded");
    assert!(
        old.is_finalized,
        "superseded judgement left unfinalized in {:?}; stuck detector will re-run it",
        old.status
    );
}

/// BUG (low): `retry_count` is documented as per-dispatch attempts ("1 after a
/// normal first dispatch") and budgets steal/stuck retries, but no rejudge
/// resets it. After 4 rejudges the first lease steal goes straight to
/// DISPATCH_RETRY_EXHAUSTED + DLQ without a single retry.
/// Location: handlers/submission/rejudge.rs; dispatcher/claim.rs increments.
#[tokio::test]
#[ignore = "BUG: rejudge does not reset retry_count, so retry budgets are lifetime counters"]
async fn rejudge_resets_retry_budget() {
    let app = TestApp::spawn().await;
    let token = app
        .create_user_with_role("as_admin", "password123", "admin")
        .await;
    let pid = app.create_problem(&token, "P").await;
    let sid = seed_judged(&app, &token, pid, 5).await;

    let res = app
        .post_with_token(&routes::submission_rejudge(sid), &json!({}), &token)
        .await;
    assert!(res.status < 300, "rejudge: {}", res.text);

    let sub = submission::Entity::find_by_id(sid)
        .one(&app.db)
        .await
        .expect("db")
        .expect("submission");
    assert_eq!(
        sub.retry_count, 0,
        "fresh judging attempt inherited the previous attempt's retry budget"
    );
}

/// BUG (low): the utoipa docs and handler comment promise "Max 10,000 IDs per
/// request" for bulk rejudge, but `validate_bulk_rejudge` never enforces it;
/// above 65,535 ids the `IN (...)` list exceeds Postgres' bind limit -> 500.
/// Location: models/submission.rs `validate_bulk_rejudge`.
#[tokio::test]
#[ignore = "BUG: documented 10,000-id cap on bulk rejudge is not enforced"]
async fn bulk_rejudge_enforces_documented_cap() {
    let app = TestApp::spawn().await;
    let token = app
        .create_user_with_role("as_admin", "password123", "admin")
        .await;
    let ids: Vec<i32> = (1..=10_001).collect();
    let res = app
        .post_with_token(
            routes::SUBMISSIONS_BULK_REJUDGE,
            &json!({"submission_ids": ids}),
            &token,
        )
        .await;
    assert_eq!(res.status, 400, "{}", res.text);
}

/// Regression (#59): the idempotency key used to store only (method, path),
/// so reusing a key with a DIFFERENT body silently replayed the first
/// response: an edited resubmission after a network error was dropped and the
/// old code's submission id returned. The key is now bound to a body
/// fingerprint and a body mismatch answers 422 IDEMPOTENCY_KEY_MISMATCH, like
/// a path mismatch.
#[tokio::test]
async fn idempotency_key_reuse_with_different_body_is_rejected() {
    let app = TestApp::spawn().await;
    let token = app
        .create_user_with_role("as_admin", "password123", "admin")
        .await;
    let pid = app.create_problem(&token, "P").await;

    let send = |code: &'static str| {
        app.client
            .post(format!(
                "http://{}{}",
                app.addr,
                routes::problem_submissions(pid)
            ))
            .header("Authorization", format!("Bearer {token}"))
            .header("Idempotency-Key", "audit-submit-1")
            .json(&json!({"language": "cpp", "files": [{"filename": "main.cpp", "content": code}]}))
            .send()
    };
    let first = send("int main(){return 0;}").await.expect("send");
    assert_eq!(first.status().as_u16(), 201);
    let first: serde_json::Value = first.json().await.expect("json");

    let second = send("int main(){return 1;}").await.expect("send");
    assert_eq!(second.status().as_u16(), 422);
    let second: serde_json::Value = second.json().await.expect("json");
    assert_eq!(second["code"], "IDEMPOTENCY_KEY_MISMATCH");
    // The mismatch is decided before the handler runs: nothing was submitted.
    let submitted = submission::Entity::find()
        .filter(submission::Column::ProblemId.eq(pid))
        .count(&app.db)
        .await
        .expect("db");
    assert_eq!(submitted, 1, "a mismatched request must not execute");

    // The identical body still replays the original submission.
    let replay = send("int main(){return 0;}").await.expect("send");
    assert_eq!(replay.status().as_u16(), 201);
    let replay: serde_json::Value = replay.json().await.expect("json");
    assert_eq!(replay["id"], first["id"]);
}
