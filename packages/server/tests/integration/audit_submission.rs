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
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
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

/// B5 (#56, med, found independently by two reviewers): a new judgement's
/// epoch was `sub.judge_epoch + 1`, but a DEFERRED rejudge (`apply_immediately:false`)
/// creates its judgement at E+1 without advancing `sub.judge_epoch`. A later
/// immediate rejudge therefore also picks E+1. Epoch is the fencing token:
/// `mark_submission_system_error_with_epoch(sub, Some(E+1))` from the deferred
/// run's dispatch failure then finalizes the LIVE immediate judgement as
/// SystemError, and the `after_judging:{id}:{epoch}` hook dedupe collides.
/// Location: handlers/submission/rejudge.rs (epoch = sub.judge_epoch + 1),
/// handlers/submission/dispatch.rs, consumers/mod.rs.
#[tokio::test]
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

    // Applying the (finalized) deferred judgement must not move the
    // submission's epoch backwards, and the next rejudge must still get an
    // epoch no other judgement holds.
    let before = submission::Entity::find_by_id(sid)
        .one(&app.db)
        .await
        .expect("db")
        .expect("submission")
        .judge_epoch;
    let deferred_id = rows[1].id;
    assert!(!rows[1].is_current, "control: v2 is the deferred judgement");
    let mut finalize: submission_judgement::ActiveModel = rows[1].clone().into();
    finalize.is_finalized = Set(true);
    finalize.status = Set(SubmissionStatus::Judged);
    finalize.verdict = Set(Some(Verdict::WrongAnswer));
    finalize.finalized_at = Set(Some(Utc::now()));
    finalize.update(&app.db).await.expect("finalize deferred");
    let applied = app
        .post_with_token(
            &routes::submission_judgement_apply(sid, deferred_id),
            &json!({}),
            &token,
        )
        .await;
    assert_eq!(applied.status, 200, "apply: {}", applied.text);
    let after = submission::Entity::find_by_id(sid)
        .one(&app.db)
        .await
        .expect("db")
        .expect("submission");
    assert!(
        after.judge_epoch > before,
        "apply moved judge_epoch from {before} to {}",
        after.judge_epoch
    );
    let again = app
        .post_with_token(&routes::submission_rejudge(sid), &json!({}), &token)
        .await;
    assert!(again.status < 300, "rejudge after apply: {}", again.text);
    let epochs: Vec<i32> = judgements(&app, sid)
        .await
        .iter()
        .map(|j| j.judge_epoch)
        .collect();
    let mut dedup = epochs.clone();
    dedup.sort_unstable();
    dedup.dedup();
    assert_eq!(
        dedup.len(),
        epochs.len(),
        "duplicate judge_epoch after apply + rejudge: {epochs:?}"
    );
}

/// B6 (#57, med): rejudging an in-flight submission flipped its current
/// judgement to `is_current=false` but left it `is_finalized=false`. Its pending
/// finalize is then rejected (`submission_update` requires is_current or max
/// version), so it never finalizes; the stuck detector re-runs it up to
/// `max_stuck_retries` times and finally DLQs it. The stuck-retry path
/// finalizes the judgement it supersedes; the rejudge path does not.
/// Location: handlers/submission/dispatch.rs `open_rejudge_judgement`.
#[tokio::test]
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
    assert_eq!(old.status, SubmissionStatus::SystemError);
    assert_eq!(old.error_code.as_deref(), Some("SUPERSEDED"));

    // Finalized, it can now be discarded like any other old version.
    let discard = app
        .post_with_token(
            &routes::submission_judgement_discard(sub.id, j1.id),
            &json!({}),
            &token,
        )
        .await;
    assert_eq!(discard.status, 204, "discard: {}", discard.text);
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

/// B23 (#74): the utoipa docs promised "Max 10,000 IDs per request" for bulk
/// rejudge, but `validate_bulk_rejudge` never enforced it; above 65,535 ids
/// the `IN (...)` list exceeded Postgres' bind limit -> 500. Both now come
/// from `models::submission::MAX_BULK_REJUDGE_IDS`.
#[tokio::test]
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
    assert_eq!(res.body["code"], "VALIDATION_ERROR", "{}", res.text);

    // Control: exactly the cap is accepted (none of these ids exist).
    let ids: Vec<i32> = (1..=10_000).collect();
    let res = app
        .post_with_token(
            routes::SUBMISSIONS_BULK_REJUDGE,
            &json!({"submission_ids": ids}),
            &token,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
}

/// BUG (med): the idempotency key stores only (method, path), not a body
/// fingerprint, so reusing a key with a DIFFERENT body silently replays the
/// first response. The web client keeps the key across a failed POST, so an
/// edited resubmission after a network error is dropped and the old code's
/// submission id is returned. A path mismatch already yields 422
/// IDEMPOTENCY_KEY_MISMATCH; a body mismatch should too.
/// Location: middleware/idempotency.rs.
#[tokio::test]
#[ignore = "BUG: Idempotency-Key reuse with a different body silently replays the first response"]
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
    let status = second.status().as_u16();
    let second: serde_json::Value = second.json().await.unwrap_or_default();
    assert!(
        status >= 400 || second["id"] != first["id"],
        "different body under the same key replayed submission {} ({status})",
        first["id"]
    );
}
