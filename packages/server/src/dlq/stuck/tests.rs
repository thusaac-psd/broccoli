use common::DlqMessageType;

use crate::entity::submission_judgement;

use super::recovery::{
    recover_stuck_code_run_without_steal, recover_stuck_judgement_without_steal,
    recover_stuck_submission_without_steal,
};
use super::*;
use sea_orm::{DbBackend, MockDatabase, MockExecResult, TransactionTrait};

fn mock_exec() -> MockExecResult {
    MockExecResult {
        last_insert_id: 0,
        rows_affected: 1,
    }
}

fn base_submission(id: i32, now: chrono::DateTime<Utc>) -> submission::Model {
    submission::Model {
        id,
        files: serde_json::json!({}),
        language: "rust".to_string(),
        user_id: 1,
        problem_id: 2,
        contest_id: None,
        contest_type: "ioi".to_string(),
        status: SubmissionStatus::Pending,
        verdict: None,
        compile_output: None,
        error_code: None,
        error_message: None,
        score: None,
        time_used: None,
        memory_used: None,
        judge_epoch: 4,
        max_judge_epoch: 0,
        target_worker_id: None,
        owner_server_id: None,
        lease_heartbeat_at: None,
        leased_at: None,
        retry_count: 1,
        created_at: now - chrono::Duration::minutes(10),
        judged_at: None,
    }
}

fn base_code_run(id: i32, now: chrono::DateTime<Utc>) -> code_run::Model {
    code_run::Model {
        id,
        files: serde_json::json!({}),
        language: "rust".to_string(),
        user_id: 1,
        problem_id: 2,
        contest_id: None,
        contest_type: "ioi".to_string(),
        status: SubmissionStatus::Pending,
        verdict: None,
        compile_output: None,
        error_code: None,
        error_message: None,
        score: None,
        time_used: None,
        memory_used: None,
        custom_test_cases: serde_json::json!([]),
        owner_server_id: None,
        lease_heartbeat_at: None,
        leased_at: None,
        retry_count: 1,
        judge_epoch: 4,
        created_at: now - chrono::Duration::minutes(10),
        judged_at: None,
    }
}

fn base_judgement(
    id: i32,
    submission_id: i32,
    now: chrono::DateTime<Utc>,
) -> submission_judgement::Model {
    submission_judgement::Model {
        id,
        submission_id,
        version: 2,
        is_current: false,
        is_finalized: false,
        triggered_by_user_id: None,
        target_worker_id: None,
        note: None,
        status: SubmissionStatus::Pending,
        verdict: None,
        score: None,
        time_used: None,
        memory_used: None,
        compile_output: None,
        error_code: None,
        error_message: None,
        judge_epoch: 4,
        owner_server_id: None,
        lease_heartbeat_at: None,
        leased_at: None,
        retry_count: 1,
        created_at: now - chrono::Duration::minutes(10),
        finalized_at: None,
    }
}

fn assert_log_sets_detector_lease(log: &str, table: &str) {
    assert!(
        log.contains(&format!("UPDATE \\\"{table}\\\" SET")),
        "expected update for {table}, got:\n{log}"
    );
    assert!(
        log.contains("\\\"owner_server_id\\\""),
        "expected owner_server_id write, got:\n{log}"
    );
    assert!(
        log.contains("\\\"lease_heartbeat_at\\\""),
        "expected lease_heartbeat_at write, got:\n{log}"
    );
    assert!(
        log.contains("\\\"leased_at\\\""),
        "expected leased_at dispatch-anchor write, got:\n{log}"
    );
    assert!(
        log.contains("server-1"),
        "expected detector server id bind, got:\n{log}"
    );
}

fn assert_retry_judgement_insert_claims_detector_lease(log: &str) {
    assert!(
        log.contains("INSERT INTO \\\"submission_judgement\\\"")
            && log.contains("\\\"owner_server_id\\\"")
            && log.contains("\\\"lease_heartbeat_at\\\""),
        "expected retry judgement insert to claim detector lease, got:\n{log}"
    );
}

#[test]
fn stuck_detector_terminalizes_only_after_retry_threshold() {
    assert!(!stuck_retry_budget_exhausted(4, 5));
    assert!(!stuck_retry_budget_exhausted(5, 5));
    assert!(stuck_retry_budget_exhausted(6, 5));
}

#[test]
fn stuck_detector_clamps_retry_threshold_to_i32() {
    assert!(!stuck_retry_budget_exhausted(
        i32::MAX,
        u32::try_from(i32::MAX).unwrap()
    ));
    assert!(!stuck_retry_budget_exhausted(i32::MAX, u32::MAX));
}

#[test]
fn stuck_detector_error_message_names_retry_threshold() {
    assert_eq!(stuck_retries_exceeded_message(5), "Exceeded 5 retries");
}

#[test]
fn stuck_detector_uses_stable_code_run_dlq_message_id() {
    assert_eq!(stuck_code_run_message_id(42), "stuck-code-run-42");
}

#[test]
fn stuck_detector_uses_stable_judgement_dlq_message_id() {
    assert_eq!(
        stuck_submission_judgement_message_id(42),
        "stuck-submission-judgement-42"
    );
}

#[test]
fn stuck_detector_new_dlq_message_types_are_non_retryable_by_default() {
    assert_eq!(DlqMessageType::StuckCodeRun.as_str(), "stuck_code_run");
    assert_eq!(
        DlqMessageType::StuckSubmissionJudgement.as_str(),
        "stuck_submission_judgement"
    );
}

#[test]
fn old_queued_rows_are_observed_not_recovered() {
    let now = Utc::now();
    let queued_threshold = now - chrono::Duration::seconds(300);
    let orphan_pending_threshold = now - chrono::Duration::seconds(300);
    let lease_threshold = now - chrono::Duration::hours(6);

    assert_eq!(
        stuck_disposition(
            &SubmissionStatus::Queued,
            None,
            now - chrono::Duration::minutes(10),
            None,
            None,
            queued_threshold,
            orphan_pending_threshold,
            lease_threshold,
            None,
        ),
        StuckDisposition::ObserveQueued
    );
}

#[test]
fn old_pending_without_owner_is_recovered_after_orphan_timeout() {
    let now = Utc::now();
    let queued_threshold = now - chrono::Duration::seconds(300);
    let orphan_pending_threshold = now - chrono::Duration::seconds(300);
    let lease_threshold = now - chrono::Duration::hours(6);

    assert_eq!(
        stuck_disposition(
            &SubmissionStatus::Pending,
            None,
            now - chrono::Duration::minutes(10),
            None,
            None,
            queued_threshold,
            orphan_pending_threshold,
            lease_threshold,
            None,
        ),
        StuckDisposition::Recover
    );
}

#[test]
fn owned_execution_state_with_fresh_lease_is_ignored_even_when_old() {
    let now = Utc::now();
    let queued_threshold = now - chrono::Duration::seconds(300);
    let orphan_pending_threshold = now - chrono::Duration::seconds(300);
    let lease_threshold = now - chrono::Duration::hours(6);

    assert_eq!(
        stuck_disposition(
            &SubmissionStatus::Running,
            Some("server-1"),
            now - chrono::Duration::hours(12),
            Some(now - chrono::Duration::seconds(10)),
            None,
            queued_threshold,
            orphan_pending_threshold,
            lease_threshold,
            None,
        ),
        StuckDisposition::Ignore
    );
}

#[test]
fn owned_rows_with_stale_or_missing_lease_are_recovered() {
    let now = Utc::now();
    let queued_threshold = now - chrono::Duration::seconds(300);
    let orphan_pending_threshold = now - chrono::Duration::seconds(300);
    let lease_threshold = now - chrono::Duration::hours(6);

    assert_eq!(
        stuck_disposition(
            &SubmissionStatus::Compiling,
            Some("server-1"),
            now - chrono::Duration::seconds(30),
            Some(now - chrono::Duration::hours(7)),
            None,
            queued_threshold,
            orphan_pending_threshold,
            lease_threshold,
            None,
        ),
        StuckDisposition::Recover
    );
    assert_eq!(
        stuck_disposition(
            &SubmissionStatus::Pending,
            Some("server-1"),
            now - chrono::Duration::seconds(30),
            None,
            None,
            queued_threshold,
            orphan_pending_threshold,
            lease_threshold,
            None,
        ),
        StuckDisposition::Recover
    );
}

#[test]
fn owned_fresh_lease_but_over_inflight_cap_is_recovered() {
    // The silent-wedge case: a live server keeps refreshing lease_heartbeat_at,
    // so the stale-lease branch never fires, but the immutable leased_at anchor
    // is older than the in-flight cap. The cap term must still recover the row.
    let now = Utc::now();
    let queued_threshold = now - chrono::Duration::seconds(300);
    let orphan_pending_threshold = now - chrono::Duration::seconds(300);
    let lease_threshold = now - chrono::Duration::hours(6);
    let inflight_cap = Some(now - chrono::Duration::hours(1));

    assert_eq!(
        stuck_disposition(
            &SubmissionStatus::Running,
            Some("server-1"),
            now - chrono::Duration::hours(2),
            // Fresh heartbeat: the stale-lease branch is vacuous here.
            Some(now - chrono::Duration::seconds(5)),
            // Anchor older than the cap: the cap branch fires.
            Some(now - chrono::Duration::hours(2)),
            queued_threshold,
            orphan_pending_threshold,
            lease_threshold,
            inflight_cap,
        ),
        StuckDisposition::Recover
    );
}

#[test]
fn owned_fresh_lease_within_inflight_cap_is_ignored() {
    let now = Utc::now();
    let queued_threshold = now - chrono::Duration::seconds(300);
    let orphan_pending_threshold = now - chrono::Duration::seconds(300);
    let lease_threshold = now - chrono::Duration::hours(6);
    let inflight_cap = Some(now - chrono::Duration::hours(1));

    assert_eq!(
        stuck_disposition(
            &SubmissionStatus::Running,
            Some("server-1"),
            now - chrono::Duration::minutes(2),
            Some(now - chrono::Duration::seconds(5)),
            // Anchor newer than the cap: still legitimately in flight.
            Some(now - chrono::Duration::seconds(30)),
            queued_threshold,
            orphan_pending_threshold,
            lease_threshold,
            inflight_cap,
        ),
        StuckDisposition::Ignore
    );
}

#[test]
fn disabled_inflight_cap_never_recovers_on_dispatch_age_alone() {
    // max_inflight_secs = 0 -> inflight_cap_threshold = None. A very old anchor
    // under a fresh heartbeat must NOT recover: the cap is opt-in.
    let now = Utc::now();
    let queued_threshold = now - chrono::Duration::seconds(300);
    let orphan_pending_threshold = now - chrono::Duration::seconds(300);
    let lease_threshold = now - chrono::Duration::hours(6);

    assert_eq!(
        stuck_disposition(
            &SubmissionStatus::Running,
            Some("server-1"),
            now - chrono::Duration::hours(5),
            Some(now - chrono::Duration::seconds(5)),
            Some(now - chrono::Duration::hours(5)),
            queued_threshold,
            orphan_pending_threshold,
            lease_threshold,
            None,
        ),
        StuckDisposition::Ignore
    );
}

#[test]
fn inflight_capped_is_null_safe_on_both_sides() {
    let now = Utc::now();
    let cap = now - chrono::Duration::hours(1);
    // Both present, anchor older than cap -> capped.
    assert!(inflight_capped(
        Some(now - chrono::Duration::hours(2)),
        Some(cap)
    ));
    // Both present, anchor newer than cap -> not capped.
    assert!(!inflight_capped(
        Some(now - chrono::Duration::seconds(30)),
        Some(cap)
    ));
    // Legacy / pre-dispatch row with no anchor -> never capped.
    assert!(!inflight_capped(None, Some(cap)));
    // Cap disabled -> never capped, however old the anchor.
    assert!(!inflight_capped(
        Some(now - chrono::Duration::hours(9)),
        None
    ));
    assert!(!inflight_capped(None, None));
}

#[test]
fn detector_owned_redispatch_rows_get_fresh_owner_lease() {
    let now = Utc::now();
    let (owner_server_id, lease_heartbeat_at) = detector_retry_lease("server-1", now);

    assert_eq!(owner_server_id.as_deref(), Some("server-1"));
    assert_eq!(lease_heartbeat_at, Some(now));
}

#[test]
fn direct_recovery_policy_keeps_current_rows_on_lease_steal_but_handles_deferred_judgements() {
    assert!(!should_recover_directly(true, None));
    assert!(!should_recover_directly(true, Some(true)));
    assert!(should_recover_directly(true, Some(false)));
    assert!(should_recover_directly(false, None));
    assert!(should_recover_directly(false, Some(true)));
}

#[tokio::test]
async fn direct_submission_recovery_sql_claims_detector_lease() {
    let now = Utc::now();
    let submission = base_submission(42, now);
    let inserted_judgement = submission_judgement::Model {
        id: 101,
        submission_id: submission.id,
        version: 3,
        is_current: true,
        is_finalized: false,
        triggered_by_user_id: None,
        target_worker_id: None,
        note: None,
        status: SubmissionStatus::Pending,
        verdict: None,
        score: None,
        time_used: None,
        memory_used: None,
        compile_output: None,
        error_code: None,
        error_message: None,
        judge_epoch: submission.judge_epoch + 1,
        owner_server_id: None,
        lease_heartbeat_at: None,
        leased_at: None,
        retry_count: 0,
        created_at: now,
        finalized_at: None,
    };

    // The guarded submission UPDATE ... RETURNING, as the database answers it.
    let mut redispatched = submission.clone();
    redispatched.status = SubmissionStatus::Pending;
    redispatched.retry_count = submission.retry_count + 1;
    redispatched.judge_epoch = submission.judge_epoch + 1;
    redispatched.max_judge_epoch = submission.judge_epoch + 1;
    redispatched.owner_server_id = Some("server-1".to_string());
    redispatched.lease_heartbeat_at = Some(now);
    redispatched.leased_at = Some(now);

    // Queries: submission UPDATE ... RETURNING, latest-version lookup,
    // judgement INSERT ... RETURNING. Exec: the supersede of the abandoned
    // judgement.
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([vec![redispatched]])
        .append_query_results([vec![inserted_judgement.clone()], vec![inserted_judgement]])
        .append_exec_results([mock_exec()])
        .into_connection();
    let txn = db.begin().await.expect("begin mock transaction");

    let recovery = recover_stuck_submission_without_steal(&txn, &submission, "server-1")
        .await
        .expect("recover submission");

    txn.commit().await.expect("commit mock transaction");

    let StuckRecovery::RedispatchSubmission { model, retry_count } = recovery else {
        panic!("expected submission redispatch");
    };
    assert_eq!(retry_count, submission.retry_count + 1);
    assert_eq!(model.owner_server_id.as_deref(), Some("server-1"));
    assert!(model.lease_heartbeat_at.is_some());
    assert!(
        model.leased_at.is_some(),
        "redispatch stamps leased_at anchor"
    );

    let log = format!("{:?}", db.into_transaction_log());
    assert_log_sets_detector_lease(&log, "submission");
    assert_retry_judgement_insert_claims_detector_lease(&log);
}

#[tokio::test]
async fn direct_code_run_recovery_sql_claims_detector_lease() {
    let now = Utc::now();
    let code_run = base_code_run(77, now);
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_exec_results([mock_exec(), mock_exec()])
        .into_connection();
    let txn = db.begin().await.expect("begin mock transaction");

    let recovery = recover_stuck_code_run_without_steal(&txn, &code_run, "server-1")
        .await
        .expect("recover code run");

    txn.commit().await.expect("commit mock transaction");

    let StuckRecovery::RedispatchCodeRun { model, retry_count } = recovery else {
        panic!("expected code run redispatch");
    };
    assert_eq!(retry_count, code_run.retry_count + 1);
    assert_eq!(model.owner_server_id.as_deref(), Some("server-1"));
    assert!(model.lease_heartbeat_at.is_some());
    assert!(
        model.leased_at.is_some(),
        "redispatch stamps leased_at anchor"
    );

    let log = format!("{:?}", db.into_transaction_log());
    assert_log_sets_detector_lease(&log, "code_run");
}

#[tokio::test]
async fn code_run_recovery_skips_result_delete_when_claim_is_lost() {
    // Regression: two stuck-recovery detectors can race the same code run. The
    // loser's epoch+status-guarded `code_run` update matches 0 rows and returns
    // `Skip`, but the caller (`handle_stuck_code_run`) commits the transaction
    // unconditionally. If the per-run `code_run_result` delete ran BEFORE that
    // guard, a lost race would still delete + commit, wiping the results the
    // winning epoch's re-dispatch is (re)producing. The guarded claim must come
    // first so a lost race performs no destructive write.
    let now = Utc::now();
    let code_run = base_code_run(77, now);
    // A single programmed exec with rows_affected == 0 models the guarded update
    // losing the claim. Only one exec is available: the winning path (update
    // first, then delete) consumes exactly this one and returns before the
    // delete.
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 0,
        }])
        .into_connection();
    let txn = db.begin().await.expect("begin mock transaction");

    let recovery = recover_stuck_code_run_without_steal(&txn, &code_run, "server-1")
        .await
        .expect("recover code run");

    txn.commit().await.expect("commit mock transaction");

    assert!(
        matches!(recovery, StuckRecovery::Skip),
        "a lost claim race must Skip, not redispatch"
    );

    let log = format!("{:?}", db.into_transaction_log());
    assert!(
        log.contains("UPDATE \\\"code_run\\\" SET"),
        "the epoch-guarded claim update must run first:\n{log}"
    );
    assert!(
        !log.contains("DELETE FROM \\\"code_run_result\\\""),
        "a lost claim must not delete code_run_result:\n{log}"
    );
}

#[tokio::test]
async fn direct_deferred_judgement_recovery_sql_claims_detector_lease() {
    let now = Utc::now();
    let submission = base_submission(42, now);
    let judgement = base_judgement(88, submission.id, now);
    let mut reserved = submission.clone();
    reserved.max_judge_epoch = judgement.judge_epoch + 1;
    let mut redispatched = judgement.clone();
    redispatched.judge_epoch = judgement.judge_epoch + 1;
    // Queries: the parent's epoch reservation and the judgement update, both
    // UPDATE ... RETURNING. Exec: the test_case_result wipe.
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([vec![reserved]])
        .append_query_results([vec![redispatched.clone()]])
        .append_exec_results([mock_exec()])
        .into_connection();
    let txn = db.begin().await.expect("begin mock transaction");

    let recovery = recover_stuck_judgement_without_steal(&txn, &judgement, "server-1")
        .await
        .expect("recover deferred judgement");

    txn.commit().await.expect("commit mock transaction");

    let StuckRecovery::RedispatchJudgement {
        submission: model,
        judgement_id,
        fire_after_judging,
        retry_count,
    } = recovery
    else {
        panic!("expected judgement redispatch");
    };
    assert_eq!(judgement_id, judgement.id);
    assert!(!fire_after_judging);
    assert_eq!(retry_count, judgement.retry_count + 1);
    assert_eq!(model.owner_server_id.as_deref(), Some("server-1"));
    assert!(model.lease_heartbeat_at.is_some());
    assert!(
        model.leased_at.is_some(),
        "redispatch stamps leased_at anchor"
    );

    assert_eq!(
        model.judge_epoch, redispatched.judge_epoch,
        "the dispatch is fenced by the judgement's freshly allocated epoch"
    );

    let log = format!("{:?}", db.into_transaction_log());
    let parent_update = log
        .split("UPDATE \\\"submission\\\" SET ")
        .nth(1)
        .and_then(|rest| rest.split(" WHERE ").next())
        .unwrap_or_else(|| panic!("expected the parent epoch reservation:\n{log}"));
    assert!(
        parent_update.starts_with("\\\"max_judge_epoch\\\" = GREATEST")
            && parent_update.matches("\\\" = ").count() == 1,
        "deferred judgement recovery may only reserve an epoch on the parent \
         submission, never touch its status or current epoch: {parent_update}"
    );
    assert_log_sets_detector_lease(&log, "submission_judgement");
}

/// In-place recovery of a stuck CURRENT judgement never lowers the
/// submission's dispatch-retry count. The judgement's own count is usually
/// lower (a judgement opened by a rejudge starts at 0); copying it over used
/// to hand a flapping submission a fresh steal/stuck budget.
#[tokio::test]
async fn current_judgement_recovery_never_lowers_submission_retry_count() {
    use sea_orm::{ActiveModelTrait, EntityTrait, Set};

    let (_pg, db) = crate::test_support::postgres().await;
    let (user_id, problem_id) = crate::test_support::seed_user_and_problem(&db).await;
    let now = Utc::now();
    let sub = submission::ActiveModel {
        files: Set(serde_json::json!({})),
        language: Set("cpp".into()),
        user_id: Set(user_id),
        problem_id: Set(problem_id),
        status: Set(SubmissionStatus::Running),
        judge_epoch: Set(5),
        max_judge_epoch: Set(5),
        retry_count: Set(4),
        created_at: Set(now),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("submission");
    let judgement = submission_judgement::ActiveModel {
        submission_id: Set(sub.id),
        version: Set(1),
        is_current: Set(true),
        is_finalized: Set(false),
        status: Set(SubmissionStatus::Running),
        judge_epoch: Set(5),
        retry_count: Set(0),
        created_at: Set(now),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("judgement");

    let txn = db.begin().await.expect("begin");
    let recovery = recover_stuck_judgement_without_steal(&txn, &judgement, "server-1")
        .await
        .expect("recover");
    txn.commit().await.expect("commit");
    assert!(matches!(
        recovery,
        StuckRecovery::RedispatchJudgement { .. }
    ));

    let sub = submission::Entity::find_by_id(sub.id)
        .one(&db)
        .await
        .expect("db")
        .expect("submission");
    assert_eq!(sub.retry_count, 4, "submission retry budget kept");
    assert_eq!(sub.judge_epoch, 6, "re-dispatched at a fresh epoch");
}
