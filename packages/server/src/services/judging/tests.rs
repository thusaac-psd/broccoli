//! Real-Postgres tests of the judging transitions.

use chrono::Utc;
use common::{DlqMessageType, SubmissionStatus, Verdict};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait,
    PaginatorTrait, QueryFilter, Set, Statement, TransactionTrait,
};

use super::unit::Reclaimed;
use super::*;
use crate::entity::{
    code_run, dead_letter_message, submission, submission_judgement, test_case_result,
};

const NO_TYPES: &[String] = &[];

/// The test user and problem, seeded on first use.
async fn owner_ids(db: &DatabaseConnection) -> (i32, i32) {
    use crate::entity::{problem, user};
    match (
        user::Entity::find().one(db).await.unwrap(),
        problem::Entity::find().one(db).await.unwrap(),
    ) {
        (Some(u), Some(p)) => (u.id, p.id),
        _ => crate::test_support::seed_user_and_problem(db).await,
    }
}

async fn new_submission(
    db: &DatabaseConnection,
) -> (submission::Model, submission_judgement::Model) {
    let (user_id, problem_id) = owner_ids(db).await;
    let txn = db.begin().await.unwrap();
    let created = create_submission(
        &txn,
        submission::ActiveModel {
            files: Set(serde_json::json!([])),
            language: Set("cpp".into()),
            user_id: Set(user_id),
            problem_id: Set(problem_id),
            created_at: Set(Utc::now()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    created
}

async fn sub(db: &DatabaseConnection, id: i32) -> submission::Model {
    submission::Entity::find_by_id(id)
        .one(db)
        .await
        .unwrap()
        .unwrap()
}

async fn judgement(db: &DatabaseConnection, id: i32) -> submission_judgement::Model {
    submission_judgement::Entity::find_by_id(id)
        .one(db)
        .await
        .unwrap()
        .unwrap()
}

async fn exec(db: &DatabaseConnection, sql: &str) {
    db.execute_raw(Statement::from_string(DbBackend::Postgres, sql.to_string()))
        .await
        .unwrap();
}

async fn claim_one(db: &DatabaseConnection, server: &str) -> ClaimedJudgement {
    let mut claimed = claim_judgements(db, server, 10, NO_TYPES).await.unwrap();
    assert_eq!(claimed.len(), 1, "exactly one judgement is claimable");
    claimed.remove(0)
}

async fn add_result(db: &DatabaseConnection, j: &submission_judgement::Model) {
    test_case_result::ActiveModel {
        submission_id: Set(j.submission_id),
        judgement_id: Set(Some(j.id)),
        judge_epoch: Set(j.judge_epoch),
        verdict: Set(Verdict::Accepted),
        score: Set(1.0),
        created_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap();
}

async fn results(db: &DatabaseConnection, judgement_id: i32) -> u64 {
    test_case_result::Entity::find()
        .filter(test_case_result::Column::JudgementId.eq(judgement_id))
        .count(db)
        .await
        .unwrap()
}

async fn rejudge(
    db: &DatabaseConnection,
    sid: i32,
    immediate: bool,
) -> submission_judgement::Model {
    let txn = db.begin().await.unwrap();
    let open = if immediate {
        OpenJudgement::Rejudge {
            triggered_by_user_id: 1,
            target_worker_id: None,
        }
    } else {
        OpenJudgement::DeferredRejudge {
            triggered_by_user_id: 1,
            target_worker_id: None,
        }
    };
    let (_, j) = open_judgement(&txn, sid, open).await.unwrap().unwrap();
    txn.commit().await.unwrap();
    j
}

async fn finish(db: &DatabaseConnection, j: &submission_judgement::Model, verdict: Verdict) {
    exec(
        db,
        &format!(
            "UPDATE submission_judgement SET status = 'Judged', verdict = '{verdict}', \
             is_finalized = TRUE, finalized_at = NOW() WHERE id = {}",
            j.id
        ),
    )
    .await;
}

#[tokio::test]
async fn a_new_submission_is_queued_with_its_first_judgement() {
    let (_pg, db) = crate::test_support::postgres().await;
    let (s, j) = new_submission(&db).await;
    assert_eq!(s.status, SubmissionStatus::Queued);
    assert!(j.is_current);
    assert_eq!(j.version, 1);
    assert_eq!(j.status, SubmissionStatus::Queued);
    assert_eq!(j.judge_epoch, s.judge_epoch);
    assert_eq!(j.owner_server_id, None);
}

#[tokio::test]
async fn claiming_puts_the_judgement_in_flight_and_the_submission_follows() {
    let (_pg, db) = crate::test_support::postgres().await;
    let (s, j) = new_submission(&db).await;

    let claimed = claim_one(&db, "srv-a").await;
    assert_eq!(claimed.judgement.id, j.id);
    assert_eq!(claimed.submission.id, s.id);
    assert_eq!(claimed.judgement.status, SubmissionStatus::Pending);
    assert_eq!(claimed.judgement.owner_server_id.as_deref(), Some("srv-a"));
    assert!(claimed.judgement.lease_heartbeat_at.is_some());
    assert!(claimed.judgement.leased_at.is_some());
    assert_eq!(claimed.submission.status, SubmissionStatus::Pending);
    assert!(
        claim_judgements(&db, "srv-b", 10, NO_TYPES)
            .await
            .unwrap()
            .is_empty(),
        "a claimed judgement is not claimed again"
    );
    assert_eq!(count_in_flight(&db, "srv-a").await.unwrap(), 1);
}

#[tokio::test]
async fn a_deferred_rejudge_is_claimed_without_touching_the_submission() {
    let (_pg, db) = crate::test_support::postgres().await;
    let (s, j1) = new_submission(&db).await;
    claim_one(&db, "srv-a").await;
    finish(&db, &j1, Verdict::Accepted).await;
    mirror_status(&db, s.id).await;

    let deferred = rejudge(&db, s.id, false).await;
    assert!(!deferred.is_current);
    assert_ne!(deferred.judge_epoch, j1.judge_epoch, "epochs are unique");
    let claimed = claim_one(&db, "srv-a").await;
    assert_eq!(claimed.judgement.id, deferred.id);
    assert_eq!(claimed.submission.status, SubmissionStatus::Judged);
}

/// Rewrites the submission cache from its current judgement, as every
/// transition does; for tests that move a judgement by hand.
async fn mirror_status(db: &DatabaseConnection, sid: i32) {
    super::unit::mirror_current(db, sid).await.unwrap();
}

#[tokio::test]
async fn an_immediate_rejudge_queues_both_rows_with_no_owner() {
    let (_pg, db) = crate::test_support::postgres().await;
    let (s, j1) = new_submission(&db).await;
    claim_one(&db, "srv-a").await;

    let j2 = rejudge(&db, s.id, true).await;
    let old = judgement(&db, j1.id).await;
    assert!(!old.is_current);
    assert!(
        old.is_finalized,
        "the in-flight judgement it replaces is finalized"
    );
    assert_eq!(old.status, SubmissionStatus::SystemError);

    let now = sub(&db, s.id).await;
    assert_eq!(now.status, SubmissionStatus::Queued);
    assert_eq!(now.judge_epoch, j2.judge_epoch);
    assert_eq!(j2.status, SubmissionStatus::Queued);
    assert_eq!(j2.owner_server_id, None);
    // Nothing in flight is left for the steal to take: the queued pair is
    // only ever dispatched by a claim.
    let reclaimed = reclaim_abandoned(
        &db,
        Unit::Judgement,
        Abandoned::LeaseLapsed { lease_ttl_secs: 0 },
        RetryBudget::new(5),
        10,
        NO_TYPES,
    )
    .await
    .unwrap();
    assert_eq!(reclaimed, Reclaimed::default());
    assert_eq!(sub(&db, s.id).await.status, SubmissionStatus::Queued);
}

#[tokio::test]
async fn lease_refresh_covers_a_current_judgement_beside_a_newer_deferred_one() {
    let (_pg, db) = crate::test_support::postgres().await;
    let (s, j1) = new_submission(&db).await;
    claim_one(&db, "srv-a").await;
    // A deferred rejudge (a higher version) exists while the current
    // judgement is judging.
    rejudge(&db, s.id, false).await;
    exec(
        &db,
        &format!(
            "UPDATE submission_judgement SET lease_heartbeat_at = NOW() - INTERVAL '1 hour' \
             WHERE id = {}",
            j1.id
        ),
    )
    .await;

    let counts = refresh_leases(&db, "srv-a").await.unwrap();
    assert_eq!(counts.judgements, 1);
    let reclaimed = reclaim_abandoned(
        &db,
        Unit::Judgement,
        Abandoned::LeaseLapsed { lease_ttl_secs: 60 },
        RetryBudget::new(5),
        10,
        NO_TYPES,
    )
    .await
    .unwrap();
    assert_eq!(reclaimed.requeued, 0, "a refreshed lease is not stolen");
}

#[tokio::test]
async fn a_lapsed_lease_requeues_under_a_new_epoch_and_spends_a_retry() {
    let (_pg, db) = crate::test_support::postgres().await;
    let (s, _) = new_submission(&db).await;
    let claimed = claim_one(&db, "dead").await;
    add_result(&db, &claimed.judgement).await;
    exec(
        &db,
        "UPDATE submission_judgement SET lease_heartbeat_at = NOW() - INTERVAL '2 minutes'",
    )
    .await;

    let reclaimed = reclaim_abandoned(
        &db,
        Unit::Judgement,
        Abandoned::LeaseLapsed { lease_ttl_secs: 60 },
        RetryBudget::new(5),
        10,
        NO_TYPES,
    )
    .await
    .unwrap();
    assert_eq!(reclaimed.requeued, 1);

    let j = judgement(&db, claimed.judgement.id).await;
    assert_eq!(j.status, SubmissionStatus::Queued);
    assert_eq!(j.owner_server_id, None);
    assert_eq!(j.lease_heartbeat_at, None);
    assert_eq!(j.dispatch_retries, 1);
    assert!(j.judge_epoch > claimed.judgement.judge_epoch);
    assert_eq!(
        results(&db, j.id).await,
        0,
        "the lost attempt's results are wiped"
    );
    let s = sub(&db, s.id).await;
    assert_eq!(s.status, SubmissionStatus::Queued);
    assert_eq!(s.judge_epoch, j.judge_epoch);
    // The old dispatch can no longer finish.
    assert!(
        !fail_dispatch(
            &db,
            Unit::Judgement,
            j.id,
            claimed.judgement.judge_epoch,
            "X",
            "late"
        )
        .await
        .unwrap()
    );
}

#[tokio::test]
async fn a_live_lease_is_left_alone() {
    let (_pg, db) = crate::test_support::postgres().await;
    new_submission(&db).await;
    claim_one(&db, "live").await;
    let reclaimed = reclaim_abandoned(
        &db,
        Unit::Judgement,
        Abandoned::LeaseLapsed { lease_ttl_secs: 60 },
        RetryBudget::new(5),
        10,
        NO_TYPES,
    )
    .await
    .unwrap();
    assert_eq!(reclaimed, Reclaimed::default());
}

#[tokio::test]
async fn a_spent_dispatch_budget_finalizes_and_dead_letters_once_per_submission() {
    let (_pg, db) = crate::test_support::postgres().await;
    let (s, _) = new_submission(&db).await;
    let claimed = claim_one(&db, "dead").await;
    exec(
        &db,
        "UPDATE submission_judgement SET retry_count = 2, \
         lease_heartbeat_at = NOW() - INTERVAL '2 minutes'",
    )
    .await;

    let reclaimed = reclaim_abandoned(
        &db,
        Unit::Judgement,
        Abandoned::LeaseLapsed { lease_ttl_secs: 60 },
        RetryBudget::new(2),
        10,
        NO_TYPES,
    )
    .await
    .unwrap();
    assert_eq!(
        reclaimed,
        Reclaimed {
            requeued: 0,
            exhausted: 1
        }
    );
    let j = judgement(&db, claimed.judgement.id).await;
    assert!(j.is_finalized);
    assert_eq!(j.status, SubmissionStatus::SystemError);
    assert_eq!(j.error_code.as_deref(), Some("DISPATCH_RETRY_EXHAUSTED"));
    let s = sub(&db, s.id).await;
    assert_eq!(
        s.status,
        SubmissionStatus::SystemError,
        "the submission follows"
    );
    let dlq = dead_letter_message::Entity::find()
        .filter(dead_letter_message::Column::SubmissionId.eq(s.id))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(dlq.len(), 1);
    assert_eq!(
        dlq[0].message_type,
        DlqMessageType::StuckSubmission.as_str()
    );
}

#[tokio::test]
async fn a_dispatch_past_the_in_flight_cap_is_stuck_even_with_a_fresh_lease() {
    let (_pg, db) = crate::test_support::postgres().await;
    new_submission(&db).await;
    let claimed = claim_one(&db, "wedged").await;
    exec(
        &db,
        "UPDATE submission_judgement SET leased_at = NOW() - INTERVAL '2 hours'",
    )
    .await;
    let stuck = |cap| Abandoned::Stuck {
        timeout_secs: 21_600,
        max_inflight_secs: cap,
    };

    let ignored = reclaim_abandoned(
        &db,
        Unit::Judgement,
        stuck(None),
        RetryBudget::new(5),
        10,
        NO_TYPES,
    )
    .await
    .unwrap();
    assert_eq!(ignored, Reclaimed::default(), "no cap, fresh heartbeat");

    let reclaimed = reclaim_abandoned(
        &db,
        Unit::Judgement,
        stuck(Some(3600)),
        RetryBudget::new(5),
        10,
        NO_TYPES,
    )
    .await
    .unwrap();
    assert_eq!(reclaimed.requeued, 1);
    assert_eq!(
        judgement(&db, claimed.judgement.id).await.status,
        SubmissionStatus::Queued
    );
}

#[tokio::test]
async fn releasing_requeues_own_work_without_spending_a_retry() {
    let (_pg, db) = crate::test_support::postgres().await;
    let (mine, _) = new_submission(&db).await;
    let claimed = claim_one(&db, "me").await;
    let (theirs, _) = new_submission(&db).await;
    claim_one(&db, "peer").await;

    let counts = release_leases(&db, "me").await.unwrap();
    assert_eq!(counts.judgements, 1);
    let j = judgement(&db, claimed.judgement.id).await;
    assert_eq!(j.status, SubmissionStatus::Queued);
    assert_eq!(j.dispatch_retries, 0);
    assert!(j.judge_epoch > claimed.judgement.judge_epoch);
    assert_eq!(sub(&db, mine.id).await.status, SubmissionStatus::Queued);
    assert_eq!(sub(&db, theirs.id).await.status, SubmissionStatus::Pending);
}

#[tokio::test]
async fn a_system_error_is_retried_within_the_submission_budget() {
    let (_pg, db) = crate::test_support::postgres().await;
    let (s, _) = new_submission(&db).await;
    let claimed = claim_one(&db, "srv").await;
    exec(&db, "UPDATE submission_judgement SET retry_count = 3").await;
    assert!(
        fail_dispatch(
            &db,
            Unit::Judgement,
            claimed.judgement.id,
            claimed.judgement.judge_epoch,
            "PLUGIN_ERROR",
            "boom"
        )
        .await
        .unwrap()
    );
    assert_eq!(sub(&db, s.id).await.status, SubmissionStatus::SystemError);

    let budget = RetryBudget::new(1);
    assert_eq!(
        retry_system_errors(&db, budget, 10, NO_TYPES)
            .await
            .unwrap(),
        1
    );
    let j = judgement(&db, claimed.judgement.id).await;
    assert_eq!(j.status, SubmissionStatus::Queued);
    assert!(!j.is_finalized);
    assert_eq!(j.error_code, None);
    assert_eq!(
        j.dispatch_retries, 0,
        "the retry gets a fresh dispatch budget"
    );
    let s = sub(&db, s.id).await;
    assert_eq!(s.status, SubmissionStatus::Queued);
    assert_eq!(s.system_error_retries, 1);

    // The budget is spent: the next SystemError stands.
    let again = claim_one(&db, "srv").await;
    fail_dispatch(
        &db,
        Unit::Judgement,
        again.judgement.id,
        again.judgement.judge_epoch,
        "PLUGIN_ERROR",
        "boom",
    )
    .await
    .unwrap();
    assert_eq!(
        retry_system_errors(&db, budget, 10, NO_TYPES)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn a_contestant_verdict_or_a_plugin_refusal_is_never_retried() {
    let (_pg, db) = crate::test_support::postgres().await;
    let (_, _) = new_submission(&db).await;
    let wrong = claim_one(&db, "srv").await;
    finish(&db, &wrong.judgement, Verdict::WrongAnswer).await;
    let (_, _) = new_submission(&db).await;
    let refused = claim_one(&db, "srv").await;
    fail_dispatch(
        &db,
        Unit::Judgement,
        refused.judgement.id,
        refused.judgement.judge_epoch,
        common::SubmissionDlqErrorCode::PLUGIN_REFUSED,
        "no",
    )
    .await
    .unwrap();

    let n = retry_system_errors(&db, RetryBudget::new(5), 10, NO_TYPES)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn applying_a_deferred_judgement_moves_its_results_to_a_fresh_epoch() {
    let (_pg, db) = crate::test_support::postgres().await;
    let (s, j1) = new_submission(&db).await;
    claim_one(&db, "srv").await;
    finish(&db, &j1, Verdict::WrongAnswer).await;
    let deferred = rejudge(&db, s.id, false).await;
    add_result(&db, &deferred).await;
    finish(&db, &deferred, Verdict::Accepted).await;

    let txn = db.begin().await.unwrap();
    let ApplyJudgement::Applied(applied) = apply_judgement(&txn, s.id, deferred.id).await.unwrap()
    else {
        panic!("applied");
    };
    txn.commit().await.unwrap();
    let j = judgement(&db, deferred.id).await;
    assert!(j.is_current);
    assert!(j.judge_epoch > deferred.judge_epoch);
    assert_eq!(applied.judge_epoch, j.judge_epoch);
    assert_eq!(applied.status, SubmissionStatus::Judged);
    assert_eq!(applied.verdict, Some(Verdict::Accepted));
    let moved = test_case_result::Entity::find()
        .filter(test_case_result::Column::JudgementId.eq(deferred.id))
        .filter(test_case_result::Column::JudgeEpoch.eq(j.judge_epoch))
        .count(&db)
        .await
        .unwrap();
    assert_eq!(moved, 1);
}

async fn new_code_run(db: &DatabaseConnection) -> code_run::Model {
    let (user_id, problem_id) = owner_ids(db).await;
    code_run::ActiveModel {
        files: Set(serde_json::json!([])),
        language: Set("cpp".into()),
        user_id: Set(user_id),
        problem_id: Set(problem_id),
        status: Set(SubmissionStatus::Queued),
        custom_test_cases: Set(serde_json::json!([])),
        created_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap()
}

#[tokio::test]
async fn a_code_run_follows_the_same_claim_and_requeue_rules() {
    let (_pg, db) = crate::test_support::postgres().await;
    let run = new_code_run(&db).await;
    let claimed = claim_code_runs(&db, "dead", 10, NO_TYPES).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].status, SubmissionStatus::Pending);
    assert_eq!(claimed[0].owner_server_id.as_deref(), Some("dead"));
    exec(
        &db,
        "UPDATE code_run SET lease_heartbeat_at = NOW() - INTERVAL '2 minutes'",
    )
    .await;

    let reclaimed = reclaim_abandoned(
        &db,
        Unit::CodeRun,
        Abandoned::LeaseLapsed { lease_ttl_secs: 60 },
        RetryBudget::new(5),
        10,
        NO_TYPES,
    )
    .await
    .unwrap();
    assert_eq!(reclaimed.requeued, 1);
    let after = code_run::Entity::find_by_id(run.id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.status, SubmissionStatus::Queued);
    assert_eq!(after.judge_epoch, run.judge_epoch + 1);
    assert_eq!(after.dispatch_retries, 1);
    assert!(
        !fail_dispatch(&db, Unit::CodeRun, run.id, run.judge_epoch, "X", "late")
            .await
            .unwrap(),
        "the lost dispatch is fenced"
    );
}

async fn abandon(db: &DatabaseConnection, retries: i32) {
    exec(
        db,
        &format!(
            "UPDATE submission_judgement SET retry_count = {retries}, status = 'Running', \
             is_finalized = FALSE, finalized_at = NULL, owner_server_id = 'dead', \
             lease_heartbeat_at = NOW() - INTERVAL '2 minutes'"
        ),
    )
    .await;
}

async fn steal(db: &DatabaseConnection) -> Reclaimed {
    reclaim_abandoned(
        db,
        Unit::Judgement,
        Abandoned::LeaseLapsed { lease_ttl_secs: 60 },
        RetryBudget::new(0),
        10,
        NO_TYPES,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn a_recurring_exhaustion_reopens_the_one_dead_letter_of_its_submission() {
    let (_pg, db) = crate::test_support::postgres().await;
    let (s, _) = new_submission(&db).await;
    abandon(&db, 0).await;
    assert_eq!(steal(&db).await.exhausted, 1);
    let first = dead_letter_message::Entity::find()
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    crate::dlq::DlqService::new(&db)
        .resolve(first.id, 1)
        .await
        .unwrap();

    abandon(&db, 0).await;
    assert_eq!(steal(&db).await.exhausted, 1);
    let rows = dead_letter_message::Entity::find().all(&db).await.unwrap();
    assert_eq!(rows.len(), 1, "one row per subject");
    let row = &rows[0];
    assert_eq!(row.id, first.id);
    assert_eq!(row.submission_id, Some(s.id));
    assert!(!row.resolved, "the recurrence reopens it");
    assert_eq!(row.resolved_by, None);
    assert_eq!(row.retry_count, 2, "both occurrences are counted");
    assert_eq!(row.retry_history.as_array().map(Vec::len), Some(2));
}

/// A rejudge holds its submission's row lock from the epoch allocation to
/// commit, so a concurrent rejudge waits for it and a claim skips the
/// submission: versions stay unique and exactly one judgement is current.
#[tokio::test]
async fn a_rejudge_serializes_with_another_rejudge_and_a_claim() {
    let (_pg, db) = crate::test_support::postgres().await;
    let (s, _) = new_submission(&db).await;
    let sid = s.id;
    let rejudge_in = move |txn: sea_orm::DatabaseTransaction| async move {
        open_judgement(
            &txn,
            sid,
            OpenJudgement::Rejudge {
                triggered_by_user_id: 1,
                target_worker_id: None,
            },
        )
        .await
        .unwrap()
        .unwrap();
        txn
    };

    let first = rejudge_in(db.begin().await.unwrap()).await;
    // While the first rejudge is open, a claim passes over the submission.
    assert!(
        claim_judgements(&db, "srv", 10, NO_TYPES)
            .await
            .unwrap()
            .is_empty()
    );
    // A second rejudge blocks on the row lock until the first commits.
    let second_txn = db.begin().await.unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let second = tokio::spawn(async move {
        started_tx.send(()).unwrap();
        rejudge_in(second_txn).await.commit().await.unwrap();
    });
    started_rx.await.unwrap();
    first.commit().await.unwrap();
    second.await.unwrap();

    let all = submission_judgement::Entity::find()
        .filter(submission_judgement::Column::SubmissionId.eq(sid))
        .all(&db)
        .await
        .unwrap();
    let mut versions: Vec<i32> = all.iter().map(|j| j.version).collect();
    versions.sort_unstable();
    assert_eq!(versions, vec![1, 2, 3]);
    assert_eq!(all.iter().filter(|j| j.is_current).count(), 1);
    let current = claim_one(&db, "srv").await;
    assert_eq!(current.judgement.version, 3);
}
