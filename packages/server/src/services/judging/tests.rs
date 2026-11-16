//! Real-Postgres tests of the judging transitions.

use chrono::Utc;
use common::db::DatabaseConnection;
use common::worker::OperationFault;
use common::{DlqMessageType, SubmissionStatus, Verdict};

use crate::error::HostCode;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, PaginatorTrait,
    QueryFilter, Set, Statement, TransactionTrait,
};

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::unit::Reclaimed;
use super::*;
use crate::entity::{
    code_run, dead_letter_message, submission, submission_judgement, test_case_result,
};

const NO_TYPES: &[String] = &[];

/// Ten rows, no in-flight cap.
const TEN: ClaimLimit = ClaimLimit {
    rows: 10,
    in_flight_cap: None,
};

/// The units `server` has in flight.
async fn in_flight(db: &DatabaseConnection, server: &str) -> i64 {
    let count = |table: &str| {
        format!(
            "(SELECT COUNT(*) FROM {table} WHERE owner_server_id = $1 AND status IN {})",
            common::StatusPhase::InFlight.sql_list()
        )
    };
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "SELECT ({} + {})::bigint AS n",
                count("submission_judgement"),
                count("code_run")
            ),
            [server.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    row.try_get("", "n").unwrap()
}

/// A handle on `db`'s pool that counts the statements run through it.
fn counting(db: &DatabaseConnection) -> (DatabaseConnection, Arc<AtomicUsize>) {
    let statements = Arc::new(AtomicUsize::new(0));
    let mut counted = sea_orm::SqlxPostgresConnector::from_sqlx_postgres_pool(
        db.as_sea_orm().get_postgres_connection_pool().clone(),
    );
    let seen = Arc::clone(&statements);
    counted.set_metric_callback(move |_| {
        seen.fetch_add(1, Ordering::Relaxed);
    });
    (counted.into(), statements)
}

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
            contest_type: Set("standard".into()),
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
    let mut claimed = claim_judgements(db, server, TEN, NO_TYPES)
        .await
        .unwrap()
        .units;
    assert_eq!(claimed.len(), 1, "exactly one judgement is claimable");
    claimed.remove(0).unit
}

/// A dispatch of judgement `j`, live in `live` while it is held.
fn dispatch_of(live: &LiveDispatches, j: &submission_judgement::Model) -> Dispatch {
    Dispatch::new(
        "p",
        live.enter(DispatchedUnit::Judgement {
            submission_id: j.submission_id,
            judgement_id: j.id,
            judge_epoch: j.judge_epoch,
        }),
        DispatchFaults::default(),
    )
}

async fn add_result(db: &DatabaseConnection, j: &submission_judgement::Model) {
    test_case_result::ActiveModel {
        submission_id: Set(j.submission_id),
        judgement_id: Set(j.id),
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
    assert_eq!(sub(&db, s.id).await.status, SubmissionStatus::Pending);
    assert!(
        claim_judgements(&db, "srv-b", TEN, NO_TYPES)
            .await
            .unwrap()
            .units
            .is_empty(),
        "a claimed judgement is not claimed again"
    );
    assert_eq!(in_flight(&db, "srv-a").await, 1);
}

/// A claim is one statement: it returns each judgement with what its
/// dispatch reads of the submission and what the claim's metrics record, so
/// no second statement can fail between the claim committing and the
/// dispatch starting.
#[tokio::test]
async fn a_claim_is_one_statement_that_returns_what_dispatch_needs() {
    let (_pg, db) = crate::test_support::postgres().await;
    let (s, j) = new_submission(&db).await;
    let (counted, statements) = counting(&db);

    let claimed = claim_judgements(&counted, "srv", TEN, NO_TYPES)
        .await
        .unwrap();
    assert_eq!(statements.load(Ordering::Relaxed), 1);
    let [claim] = &claimed.units[..] else {
        panic!("one judgement claimed: {claimed:?}");
    };
    assert_eq!(claim.unit.judgement.id, j.id);
    assert_eq!(claim.unit.submission.id, s.id);
    assert_eq!(claim.unit.submission.problem_id, s.problem_id);
    assert_eq!(claim.unit.submission.language, "cpp");
    assert_eq!(claim.unit.submission.contest_type, "standard");
    assert_eq!(claim.problem_type, "standard");
    assert!(claim.queued_secs >= 0.0);
    assert!(claimed.plugin_config_epoch.is_some());

    let (counted, statements) = counting(&db);
    let none = claim_judgements(&counted, "srv", TEN, NO_TYPES)
        .await
        .unwrap();
    assert!(none.units.is_empty() && none.plugin_config_epoch.is_none());
    assert_eq!(statements.load(Ordering::Relaxed), 1);
}

/// Under an in-flight cap the claim counts what the server has in flight in
/// the same statement, takes no more than the room left, and says what room
/// that was.
#[tokio::test]
async fn a_capped_claim_takes_only_the_room_the_cap_leaves() {
    let (_pg, db) = crate::test_support::postgres().await;
    for _ in 0..3 {
        new_submission(&db).await;
    }
    new_code_run(&db).await;
    let capped = |cap| ClaimLimit {
        rows: 10,
        in_flight_cap: Some(cap),
    };
    assert_eq!(
        claim_code_runs(&db, "srv", TEN, NO_TYPES)
            .await
            .unwrap()
            .units
            .len(),
        1
    );

    let claimed = claim_judgements(&db, "srv", capped(3), NO_TYPES)
        .await
        .unwrap();
    assert_eq!(claimed.cap_room, Some(2), "the code run counts");
    assert_eq!(claimed.units.len(), 2);
    let full = claim_judgements(&db, "srv", capped(3), NO_TYPES)
        .await
        .unwrap();
    assert_eq!(full.cap_room, Some(0));
    assert!(full.units.is_empty());
    assert_eq!(
        claim_judgements(&db, "other", capped(3), NO_TYPES)
            .await
            .unwrap()
            .cap_room,
        Some(3),
        "another server's work does not count"
    );
}

/// A claim whose dispatch never starts goes back to `Queued` spending no
/// retry, and only the given rows this server owns move.
#[tokio::test]
async fn a_claim_that_never_dispatched_is_handed_back_without_spending_a_retry() {
    let (_pg, db) = crate::test_support::postgres().await;
    let (s, _) = new_submission(&db).await;
    let claimed = claim_one(&db, "srv").await;
    new_submission(&db).await;
    let other = claim_one(&db, "srv").await;

    assert_eq!(
        super::unit::release_claimed(&db, Unit::Judgement, "elsewhere", &[claimed.judgement.id])
            .await
            .unwrap(),
        0,
        "another server's claim is not released"
    );
    assert_eq!(
        super::unit::release_claimed(&db, Unit::Judgement, "srv", &[claimed.judgement.id])
            .await
            .unwrap(),
        1
    );
    let j = judgement(&db, claimed.judgement.id).await;
    assert_eq!(j.status, SubmissionStatus::Queued);
    assert_eq!(j.owner_server_id, None);
    assert_eq!(j.dispatch_retries, claimed.judgement.dispatch_retries);
    assert_eq!(sub(&db, s.id).await.status, SubmissionStatus::Queued);
    assert_eq!(
        judgement(&db, other.judgement.id).await.status,
        SubmissionStatus::Pending
    );
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
    assert_eq!(sub(&db, s.id).await.status, SubmissionStatus::Judged);
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
    assert_eq!(old.status, SubmissionStatus::Superseded);

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
    let live = LiveDispatches::default();
    let _dispatch = dispatch_of(&live, &claim_one(&db, "srv-a").await.judgement);
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

    let counts = refresh_leases(&db, "srv-a", &live).await.unwrap();
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

/// A judgement this server owns but no dispatch of it works on any more (its
/// task was lost) keeps its old heartbeat, so its lease lapses and it is
/// stolen; one a dispatch still holds is refreshed and kept.
#[tokio::test]
async fn lease_refresh_covers_only_live_dispatches() {
    let (_pg, db) = crate::test_support::postgres().await;
    let live = LiveDispatches::default();
    new_submission(&db).await;
    let kept = claim_one(&db, "srv").await.judgement;
    let _kept = dispatch_of(&live, &kept);
    new_submission(&db).await;
    let lost = claim_one(&db, "srv").await.judgement;
    drop(dispatch_of(&live, &lost));
    // A dispatch of the kept judgement's earlier epoch is not its dispatch.
    let _stale = dispatch_of(
        &live,
        &submission_judgement::Model {
            judge_epoch: kept.judge_epoch - 1,
            ..kept.clone()
        },
    );
    exec(
        &db,
        "UPDATE submission_judgement SET lease_heartbeat_at = NOW() - INTERVAL '1 hour'",
    )
    .await;

    let counts = refresh_leases(&db, "srv", &live).await.unwrap();
    assert_eq!(counts.judgements, 1, "only the live dispatch's lease");
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
    assert_eq!(reclaimed.requeued, 1, "the lost judgement is stolen");
    assert_eq!(
        judgement(&db, lost.id).await.status,
        SubmissionStatus::Queued
    );
    assert_ne!(
        judgement(&db, kept.id).await.status,
        SubmissionStatus::Queued
    );
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
    assert_eq!(
        fail_dispatch(
            &db,
            Unit::Judgement,
            j.id,
            claimed.judgement.judge_epoch,
            &DispatchFailure::judging(HostCode::PluginError, "late"),
        )
        .await
        .unwrap(),
        DispatchFailed::Stale
    );
}

#[tokio::test]
async fn one_reclaim_takes_over_every_lapsed_unit_whatever_the_batch_size() {
    let (_pg, db) = crate::test_support::postgres().await;
    for _ in 0..7 {
        new_submission(&db).await;
    }
    claim_judgements(&db, "dead", TEN, NO_TYPES).await.unwrap();
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
        3,
        NO_TYPES,
    )
    .await
    .unwrap();
    assert_eq!(
        reclaimed.requeued, 7,
        "batches repeat until one comes back short"
    );
    assert_eq!(in_flight(&db, "dead").await, 0);
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
        "UPDATE submission_judgement SET dispatch_retries = 2, \
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
        SubmissionStatus::Pending,
        "the submission shows the attempt until the system-error retry decides"
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

/// A system error the retry will re-judge never shows on the submission:
/// it keeps showing the attempt in flight until the reaper requeues it, so
/// the status a client sees never goes from terminal back to judging. Once
/// the budget is spent the reaper confirms the error, once, and only then
/// does the submission show it.
#[tokio::test]
async fn a_system_error_is_retried_within_the_submission_budget() {
    let (_pg, db) = crate::test_support::postgres().await;
    let (s, _) = new_submission(&db).await;
    let claimed = claim_one(&db, "srv").await;
    exec(&db, "UPDATE submission_judgement SET dispatch_retries = 3").await;
    assert_eq!(
        fail_dispatch(
            &db,
            Unit::Judgement,
            claimed.judgement.id,
            claimed.judgement.judge_epoch,
            &DispatchFailure::judging(HostCode::PluginError, "boom"),
        )
        .await
        .unwrap(),
        DispatchFailed::SystemError
    );
    assert_eq!(
        sub(&db, s.id).await.status,
        SubmissionStatus::Pending,
        "the submission still shows the attempt"
    );

    let budget = RetryBudget::new(1);
    let pass = retry_system_errors(&db, budget, 10, NO_TYPES)
        .await
        .unwrap();
    assert_eq!(pass.requeued, 1);
    assert!(pass.confirmed.is_empty());
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

    // The budget is spent: the next SystemError stands, shown once confirmed.
    let again = claim_one(&db, "srv").await;
    fail_dispatch(
        &db,
        Unit::Judgement,
        again.judgement.id,
        again.judgement.judge_epoch,
        &DispatchFailure::judging(HostCode::PluginError, "boom"),
    )
    .await
    .unwrap();
    assert_eq!(sub(&db, s.id).await.status, SubmissionStatus::Pending);
    let pass = retry_system_errors(&db, budget, 10, NO_TYPES)
        .await
        .unwrap();
    assert_eq!(pass.requeued, 0);
    assert_eq!(
        pass.confirmed
            .iter()
            .map(|c| c.submission_id)
            .collect::<Vec<_>>(),
        [s.id]
    );
    let shown = sub(&db, s.id).await;
    assert_eq!(shown.status, SubmissionStatus::SystemError);
    assert_eq!(shown.error_code.as_deref(), Some("PLUGIN_ERROR"));
    let pass = retry_system_errors(&db, budget, 10, NO_TYPES)
        .await
        .unwrap();
    assert!(pass.confirmed.is_empty(), "confirmed once");
}

/// A system error its submission already shows as its final result (written
/// before `error_stands` existed, or by a build that never set it) was
/// decided: the retry never reopens it.
#[tokio::test]
async fn a_system_error_its_submission_already_shows_is_never_reopened() {
    let (_pg, db) = crate::test_support::postgres().await;
    let (s, _) = new_submission(&db).await;
    let claimed = claim_one(&db, "srv").await;
    exec(
        &db,
        &format!(
            "UPDATE submission_judgement SET status = 'SystemError', verdict = 'SystemError', \
             error_code = 'PLUGIN_ERROR', is_finalized = TRUE, finalized_at = NOW(), \
             error_stands = FALSE WHERE id = {}",
            claimed.judgement.id,
        ),
    )
    .await;
    exec(
        &db,
        &format!(
            "UPDATE submission SET status = 'SystemError', verdict = 'SystemError', \
             error_code = 'PLUGIN_ERROR' WHERE id = {}",
            s.id
        ),
    )
    .await;

    let pass = retry_system_errors(&db, RetryBudget::new(5), 10, NO_TYPES)
        .await
        .unwrap();
    assert_eq!(pass, SystemErrorPass::default());
    assert_eq!(sub(&db, s.id).await.status, SubmissionStatus::SystemError);
    assert_eq!(
        judgement(&db, claimed.judgement.id).await.status,
        SubmissionStatus::SystemError
    );
}

/// A contestant's verdict is never retried, and neither is a failure every
/// attempt repeats: a plugin's refusal or the problem's checker failing
/// shows at once.
#[tokio::test]
async fn a_contestant_verdict_or_a_failure_that_stands_is_never_retried() {
    let (_pg, db) = crate::test_support::postgres().await;
    let (_, _) = new_submission(&db).await;
    let wrong = claim_one(&db, "srv").await;
    finish(&db, &wrong.judgement, Verdict::WrongAnswer).await;
    let mut standing = Vec::new();
    for failure in [
        DispatchFailure::refused("no"),
        DispatchFailure::from(super::fault::Recorded {
            fault: DispatchFault::CheckerFailed,
            message: "no".into(),
        }),
    ] {
        let code = failure.code;
        let (_, _) = new_submission(&db).await;
        let claimed = claim_one(&db, "srv").await;
        assert_eq!(
            fail_dispatch(
                &db,
                Unit::Judgement,
                claimed.judgement.id,
                claimed.judgement.judge_epoch,
                &failure,
            )
            .await
            .unwrap(),
            DispatchFailed::SystemError
        );
        standing.push((code, claimed.submission.id));
    }

    let pass = retry_system_errors(&db, RetryBudget::new(5), 10, NO_TYPES)
        .await
        .unwrap();
    assert_eq!(pass, SystemErrorPass::default());
    for (code, id) in standing {
        let shown = sub(&db, id).await;
        assert_eq!(
            shown.status,
            SubmissionStatus::SystemError,
            "{code} shows at once"
        );
        assert_eq!(shown.error_code.as_deref(), Some(code.as_str()));
    }
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
        contest_type: Set("standard".into()),
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
    let claimed = claim_code_runs(&db, "dead", TEN, NO_TYPES)
        .await
        .unwrap()
        .units;
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].unit.status, SubmissionStatus::Pending);
    assert_eq!(claimed[0].unit.owner_server_id.as_deref(), Some("dead"));
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
    assert_eq!(
        fail_dispatch(
            &db,
            Unit::CodeRun,
            run.id,
            run.judge_epoch,
            &DispatchFailure::judging(HostCode::PluginError, "late")
        )
        .await
        .unwrap(),
        DispatchFailed::Stale,
        "the lost dispatch is fenced"
    );
}

async fn abandon(db: &DatabaseConnection, retries: i32) {
    exec(
        db,
        &format!(
            "UPDATE submission_judgement SET dispatch_retries = {retries}, status = 'Running', \
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
    exec(
        &db,
        "UPDATE dead_letter_message SET last_failed_at = NOW() - INTERVAL '1 day'",
    )
    .await;

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
    assert!(
        row.last_failed_at > Utc::now() - chrono::Duration::hours(1),
        "the recurrence restamps when it last failed"
    );
}

/// A rejudge holds its submission's row lock from the epoch allocation to
/// commit, so a concurrent rejudge waits for it and a claim skips the
/// submission: versions stay unique and exactly one judgement is current.
#[tokio::test]
async fn a_rejudge_serializes_with_another_rejudge_and_a_claim() {
    let (_pg, db) = crate::test_support::postgres().await;
    let (s, _) = new_submission(&db).await;
    let sid = s.id;
    let rejudge_in = move |txn: common::db::DatabaseTransaction| async move {
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
        claim_judgements(&db, "srv", TEN, NO_TYPES)
            .await
            .unwrap()
            .units
            .is_empty()
    );
    // A second rejudge blocks on the row lock until the first commits.
    let second_txn = db.begin().await.unwrap();
    let second = tokio::spawn(async move {
        rejudge_in(second_txn).await.commit().await.unwrap();
    });
    crate::test_support::wait_until_blocked_on_a_lock(&db).await;
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

/// The lease refresh takes rows in id order and skips one a transition
/// holds, so a bulk rejudge holding submission and judgement locks can never
/// deadlock with it.
#[tokio::test]
async fn lease_refresh_skips_a_judgement_a_rejudge_holds() {
    let (_pg, db) = crate::test_support::postgres().await;
    let live = LiveDispatches::default();
    let (held, _) = new_submission(&db).await;
    let _held = dispatch_of(&live, &claim_one(&db, "srv").await.judgement);
    new_submission(&db).await;
    let _other = dispatch_of(&live, &claim_one(&db, "srv").await.judgement);

    let txn = db.begin().await.unwrap();
    open_judgement(
        &txn,
        held.id,
        OpenJudgement::Rejudge {
            triggered_by_user_id: 1,
            target_worker_id: None,
        },
    )
    .await
    .unwrap()
    .unwrap();
    let counts = refresh_leases(&db, "srv", &live).await.unwrap();
    assert_eq!(counts.judgements, 1, "the held judgement is skipped");
    txn.commit().await.unwrap();
}

#[tokio::test]
async fn discarding_a_dead_lettered_judgement_resolves_its_dead_letter() {
    let (_pg, db) = crate::test_support::postgres().await;
    let (s, j1) = new_submission(&db).await;
    claim_one(&db, "srv").await;
    finish(&db, &j1, Verdict::Accepted).await;
    mirror_status(&db, s.id).await;
    let deferred = rejudge(&db, s.id, false).await;
    claim_one(&db, "dead").await;
    exec(
        &db,
        &format!(
            "UPDATE submission_judgement SET lease_heartbeat_at = NOW() - INTERVAL '2 minutes' \
             WHERE id = {}",
            deferred.id
        ),
    )
    .await;
    assert_eq!(steal(&db).await.exhausted, 1);

    let txn = db.begin().await.unwrap();
    assert!(matches!(
        discard_judgement(&txn, s.id, deferred.id, 7).await.unwrap(),
        DiscardJudgement::Discarded
    ));
    txn.commit().await.unwrap();
    let row = dead_letter_message::Entity::find()
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.message_type,
        DlqMessageType::StuckSubmissionJudgement.as_str()
    );
    assert!(row.resolved);
    assert_eq!(row.resolved_by, Some(7));
}

#[tokio::test]
async fn the_in_flight_cap_counts_judgements_and_code_runs() {
    let (_pg, db) = crate::test_support::postgres().await;
    new_submission(&db).await;
    claim_one(&db, "srv").await;
    new_code_run(&db).await;
    assert_eq!(
        claim_code_runs(&db, "srv", TEN, NO_TYPES)
            .await
            .unwrap()
            .units
            .len(),
        1
    );
    assert_eq!(in_flight(&db, "srv").await, 2);
}

#[tokio::test]
async fn a_requeue_restarts_the_queue_age_but_keeps_the_arrival_time() {
    let (_pg, db) = crate::test_support::postgres().await;
    let (_, j) = new_submission(&db).await;
    claim_one(&db, "srv").await;
    exec(
        &db,
        "UPDATE submission_judgement SET created_at = NOW() - INTERVAL '1 day', \
         queued_at = NOW() - INTERVAL '1 day'",
    )
    .await;
    release_leases(&db, "srv").await.unwrap();
    let row = db
        .query_one_raw(Statement::from_string(
            DbBackend::Postgres,
            format!(
                "SELECT queued_at > NOW() - INTERVAL '1 minute' AS fresh, \
                        created_at < NOW() - INTERVAL '1 hour' AS old \
                 FROM submission_judgement WHERE id = {}",
                j.id
            ),
        ))
        .await
        .unwrap()
        .unwrap();
    assert!(row.try_get::<bool>("", "fresh").unwrap());
    assert!(row.try_get::<bool>("", "old").unwrap());
}

/// The next claim notification on `listener`.
async fn next_claim_wake(listener: &mut sea_orm::sqlx::postgres::PgListener) -> String {
    tokio::time::timeout(std::time::Duration::from_secs(30), listener.recv())
        .await
        .expect("a claim notification")
        .expect("listener connection")
        .payload()
        .to_string()
}

#[tokio::test]
async fn queuing_a_unit_or_freeing_its_room_notifies_the_claim_fibers() {
    let (_pg, db, url) = crate::test_support::postgres_with_url().await;
    // The migrations installed it; installing it again changes nothing.
    install_claim_notify(&db).await.unwrap();
    let mut listener = sea_orm::sqlx::postgres::PgListener::connect(&url)
        .await
        .unwrap();
    listener.listen(CLAIM_CHANNEL).await.unwrap();

    let (s, _) = new_submission(&db).await;
    assert_eq!(
        ClaimWake::parse(&next_claim_wake(&mut listener).await),
        Some(ClaimWake::Queued)
    );

    // Claiming and moving within flight say nothing: the next notification
    // is the one that frees the owner's room.
    let claimed = claim_one(&db, "srv").await;
    exec(
        &db,
        &format!(
            "UPDATE submission_judgement SET status = 'Running' WHERE id = {}",
            claimed.judgement.id
        ),
    )
    .await;
    finish(&db, &claimed.judgement, Verdict::Accepted).await;
    assert_eq!(
        ClaimWake::parse(&next_claim_wake(&mut listener).await),
        Some(ClaimWake::Freed { server_id: "srv" })
    );

    // A rejudge queues a new judgement.
    rejudge(&db, s.id, true).await;
    assert_eq!(
        ClaimWake::parse(&next_claim_wake(&mut listener).await),
        Some(ClaimWake::Queued)
    );

    // A code run is a unit too.
    new_code_run(&db).await;
    assert_eq!(
        ClaimWake::parse(&next_claim_wake(&mut listener).await),
        Some(ClaimWake::Queued)
    );
}

fn operation_letter(id: &str, submission_id: i32) -> common::DlqEnvelope {
    common::DlqEnvelope {
        message_id: id.into(),
        message_type: DlqMessageType::OperationTask,
        submission_id: Some(submission_id),
        payload: serde_json::json!({}),
        error_code: common::DlqErrorCode::MaxRetriesExceeded,
        error_message: "boom".into(),
        retry_history: vec![],
    }
}

/// An operation's dead letter is filed under the submission it ran for and
/// is resolved by the verdict that stands, whether it arrives before or
/// after that verdict.
#[tokio::test]
async fn a_standing_verdict_resolves_its_submissions_operation_letters() {
    let (_pg, db) = crate::test_support::postgres().await;
    let (s, j) = new_submission(&db).await;
    claim_one(&db, "srv").await;
    let dlq = crate::dlq::DlqService::new(&db);
    let before = dlq
        .send_to_dlq(&operation_letter("op-1", s.id))
        .await
        .unwrap();
    assert!(!before.resolved, "the submission has no verdict yet");

    finish(&db, &j, Verdict::Accepted).await;
    mirror_status(&db, s.id).await;
    let before = dead_letter_message::Entity::find_by_id(before.id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert!(before.resolved, "the verdict resolves the earlier letter");

    let after = dlq
        .send_to_dlq(&operation_letter("op-2", s.id))
        .await
        .unwrap();
    assert!(
        after.resolved,
        "a letter filed after the verdict is settled at once"
    );
}

/// A contest of type `contest_type`, as raw SQL: the tests below race its
/// type against a submission's insert.
async fn typed_contest(db: &DatabaseConnection, contest_type: &str) -> i32 {
    db.query_one_raw(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO contest (title, description, start_time, end_time, contest_type, \
                              created_at, updated_at) \
         VALUES ('race', 'race', now(), now() + interval '1 hour', $1, now(), now()) \
         RETURNING id",
        [contest_type.into()],
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get("", "id")
    .unwrap()
}

fn contest_submission(user_id: i32, problem_id: i32, contest_id: i32) -> submission::ActiveModel {
    submission::ActiveModel {
        files: Set(serde_json::json!([])),
        language: Set("cpp".into()),
        user_id: Set(user_id),
        problem_id: Set(problem_id),
        contest_id: Set(Some(contest_id)),
        contest_type: Set("ioi".into()),
        created_at: Set(Utc::now()),
        ..Default::default()
    }
}

/// A change of the contest's type that holds the contest row when a
/// submission made under the old type is inserted: the insert waits for it
/// and is refused once it commits, whichever of the two reaches the row
/// first, so no submission lands under a type its contest no longer has.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_submission_is_refused_once_its_contest_type_changed_under_it() {
    let (_pg, db) = crate::test_support::postgres().await;
    let (user_id, problem_id) = owner_ids(&db).await;
    let contest_id = typed_contest(&db, "ioi").await;

    // The change: the contest row held FOR UPDATE, as `update_contest` holds
    // it, with no submission yet and the new type written.
    let change = db.begin().await.unwrap();
    change
        .execute_unprepared(&format!(
            "SELECT id FROM contest WHERE id = {contest_id} FOR UPDATE; \
             UPDATE contest SET contest_type = 'icpc' WHERE id = {contest_id}"
        ))
        .await
        .unwrap();

    let insert = tokio::spawn({
        let db = db.clone();
        async move {
            let txn = db.begin().await.unwrap();
            let created =
                create_submission(&txn, contest_submission(user_id, problem_id, contest_id)).await;
            txn.commit().await.unwrap();
            created.map(|(s, _)| s.id)
        }
    });
    change.commit().await.unwrap();
    let refused = insert.await.unwrap().unwrap_err();
    assert!(
        matches!(refused, crate::error::AppError::Conflict(_)),
        "{refused:?}"
    );
    assert_eq!(
        submission::Entity::find()
            .filter(submission::Column::ContestId.eq(contest_id))
            .count(&db)
            .await
            .unwrap(),
        0
    );
}

/// A submission inserted, not yet committed, holds its contest row: a change
/// of the type that locks the row afterwards sees the submission once it has
/// the row, so the type stays.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_type_change_after_a_pending_submission_finds_it() {
    let (_pg, db) = crate::test_support::postgres().await;
    let (user_id, problem_id) = owner_ids(&db).await;
    let contest_id = typed_contest(&db, "ioi").await;

    let insert = db.begin().await.unwrap();
    create_submission(&insert, contest_submission(user_id, problem_id, contest_id))
        .await
        .unwrap();

    let change = tokio::spawn({
        let db = db.clone();
        async move {
            let txn = db.begin().await.unwrap();
            txn.execute_unprepared(&format!(
                "SELECT id FROM contest WHERE id = {contest_id} FOR UPDATE"
            ))
            .await
            .unwrap();
            let locked = crate::utils::contest::contest_has_submissions(&txn, contest_id)
                .await
                .unwrap();
            txn.rollback().await.unwrap();
            locked
        }
    });
    insert.commit().await.unwrap();
    assert!(change.await.unwrap(), "the change sees the submission");
}

/// An attempt the platform lost (the queue was unreachable or lost an
/// operation, its worker lacks a platform tool, or no worker takes work)
/// goes back to the queue however often it happens, never finalized and with
/// neither retry budget spent: only failures judging the submission spend
/// its system-error budget.
#[tokio::test]
async fn a_judgement_the_platform_lost_is_handed_back_without_a_verdict_or_a_retry() {
    let (_pg, db) = crate::test_support::postgres().await;
    let (s, _) = new_submission(&db).await;
    for fault in OperationFault::ALL
        .into_iter()
        .chain([OperationFault::QueueUnreachable])
    {
        let claimed = claim_one(&db, "srv").await;
        assert_eq!(
            fail_dispatch(
                &db,
                Unit::Judgement,
                claimed.judgement.id,
                claimed.judgement.judge_epoch,
                &DispatchFailure::from(super::fault::Recorded {
                    fault: fault.into(),
                    message: "the queue".into(),
                }),
            )
            .await
            .unwrap(),
            DispatchFailed::HandedBack,
            "{fault:?}"
        );
        let j = judgement(&db, claimed.judgement.id).await;
        assert_eq!(j.status, SubmissionStatus::Queued);
        assert_eq!((j.error_code, j.is_finalized), (None, false));
        assert!(j.judge_epoch > claimed.judgement.judge_epoch);
        assert_eq!(j.dispatch_retries, claimed.judgement.dispatch_retries);
        let shown = sub(&db, s.id).await;
        assert_eq!(shown.status, SubmissionStatus::Queued, "{fault:?}");
        assert_eq!(shown.system_error_retries, 0, "{fault:?}");
        let pass = retry_system_errors(&db, RetryBudget::new(1), 10, NO_TYPES)
            .await
            .unwrap();
        assert_eq!(pass, SystemErrorPass::default(), "nothing awaits a retry");
    }
}

/// Records `standard` (the type [`new_submission`] uses) as provided by one
/// plugin, enabled or not.
async fn provider(db: &DatabaseConnection, enabled: bool) {
    exec(
        db,
        &format!(
            "INSERT INTO plugin (id, is_enabled, governs) \
             VALUES ('std', {enabled}, '{{\"visibility\": false, \"contest_types\": [\"standard\"], \"problem_types\": []}}') \
             ON CONFLICT (id) DO UPDATE SET is_enabled = EXCLUDED.is_enabled"
        ),
    )
    .await;
}

async fn claimable(db: &DatabaseConnection) -> usize {
    claim_judgements(db, "srv", TEN, NO_TYPES)
        .await
        .unwrap()
        .units
        .len()
}

/// A judgement cut by its contest plugin's disable goes straight back to
/// `Queued`, on whichever replica its session ran: it never shows a
/// `SystemError`, spends neither budget, and waits queued while every
/// provider of its type is disabled: nothing claims it and no retry is spent
/// on it. It is claimed again once a provider is enabled.
#[tokio::test]
async fn work_of_a_disabled_contest_plugin_is_parked_until_it_is_enabled() {
    let (_pg, db) = crate::test_support::postgres().await;
    provider(&db, true).await;
    let (cut, _) = new_submission(&db).await;
    let claimed = claim_one(&db, "srv").await;

    provider(&db, false).await;
    assert_eq!(
        fail_dispatch(
            &db,
            Unit::Judgement,
            claimed.judgement.id,
            claimed.judgement.judge_epoch,
            &DispatchFailure::judging(HostCode::PluginSessionAborted, "the callback is gone"),
        )
        .await
        .unwrap(),
        DispatchFailed::HandedBack
    );
    let j = judgement(&db, claimed.judgement.id).await;
    assert_eq!(j.status, SubmissionStatus::Queued);
    assert_eq!((j.error_code, j.is_finalized), (None, false));
    assert!(
        j.judge_epoch > claimed.judgement.judge_epoch,
        "the cut session is fenced"
    );
    assert_eq!(j.dispatch_retries, claimed.judgement.dispatch_retries);
    let s = sub(&db, cut.id).await;
    assert_eq!(s.status, SubmissionStatus::Queued, "the submission follows");
    assert_eq!(s.system_error_retries, 0);
    let pass = retry_system_errors(&db, RetryBudget::new(1), 10, NO_TYPES)
        .await
        .unwrap();
    assert_eq!(pass, SystemErrorPass::default(), "no system error to retry");

    // A new submission waits with it.
    let (waiting, _) = new_submission(&db).await;
    assert_eq!(claimable(&db).await, 0, "a parked type is not claimed");
    assert_eq!(sub(&db, waiting.id).await.status, SubmissionStatus::Queued);

    provider(&db, true).await;
    assert_eq!(claimable(&db).await, 2, "both resume");
}

/// Records `standard` (the problem type [`new_submission`] uses) as
/// evaluated by one plugin, enabled or not.
async fn evaluator(db: &DatabaseConnection, enabled: bool) {
    exec(
        db,
        &format!(
            "INSERT INTO plugin (id, is_enabled, governs) \
             VALUES ('eval', {enabled}, \
                     '{{\"visibility\": false, \"contest_types\": [], \"problem_types\": [\"standard\"]}}') \
             ON CONFLICT (id) DO UPDATE SET is_enabled = EXCLUDED.is_enabled"
        ),
    )
    .await;
}

/// A unit needs its problem type's evaluator as much as its contest type's
/// plugin: while every evaluator of the problem type is disabled, its work
/// is parked exactly as for a disabled contest plugin. A failure the disable
/// causes goes back to `Queued` spending nothing, a system error awaiting
/// its retry is handed back instead of burning the budget, and nothing is
/// claimed until an evaluator is enabled.
#[tokio::test]
async fn work_whose_evaluator_plugin_is_disabled_is_parked_until_it_is_enabled() {
    let (_pg, db) = crate::test_support::postgres().await;
    evaluator(&db, true).await;
    let (cut, _) = new_submission(&db).await;
    let claimed = claim_one(&db, "srv").await;
    let (erred, _) = new_submission(&db).await;
    let failed = claim_one(&db, "srv").await;
    finish(&db, &failed.judgement, Verdict::SystemError).await;

    evaluator(&db, false).await;
    assert_eq!(
        fail_dispatch(
            &db,
            Unit::Judgement,
            claimed.judgement.id,
            claimed.judgement.judge_epoch,
            &DispatchFailure::judging(HostCode::PluginExecutionError, "no evaluator"),
        )
        .await
        .unwrap(),
        DispatchFailed::HandedBack
    );
    let pass = retry_system_errors(&db, RetryBudget::new(1), 10, NO_TYPES)
        .await
        .unwrap();
    assert_eq!(pass.handed_back, 1, "{pass:?}");
    assert_eq!(pass.requeued, 0, "{pass:?}");
    for id in [cut.id, erred.id] {
        let s = sub(&db, id).await;
        assert_eq!(s.status, SubmissionStatus::Queued);
        assert_eq!(s.system_error_retries, 0, "no budget is spent");
    }
    assert_eq!(claimable(&db).await, 0, "parked work is not claimed");

    evaluator(&db, true).await;
    assert_eq!(claimable(&db).await, 2, "both resume");
}

/// A failure judging the submission that is still awaiting its retry when
/// the provider is disabled goes back to the queue with its budget kept, so
/// its history shows no finalized attempt the submission does not: both wait
/// parked, queued. Once the provider is back it is judged again; an error
/// with its budget spent is not confirmed while the type is parked either.
#[tokio::test]
async fn a_system_error_awaiting_its_retry_is_handed_back_when_its_type_is_parked() {
    let (_pg, db) = crate::test_support::postgres().await;
    provider(&db, true).await;
    let budget = RetryBudget::new(1);
    for spent in [0, 1] {
        let (s, _) = new_submission(&db).await;
        exec(
            &db,
            &format!(
                "UPDATE submission SET system_error_retries = {spent} WHERE id = {}",
                s.id
            ),
        )
        .await;
        let claimed = claim_one(&db, "srv").await;
        fail_dispatch(
            &db,
            Unit::Judgement,
            claimed.judgement.id,
            claimed.judgement.judge_epoch,
            &DispatchFailure::judging(HostCode::PluginError, "boom"),
        )
        .await
        .unwrap();

        provider(&db, false).await;
        let pass = retry_system_errors(&db, budget, 10, NO_TYPES)
            .await
            .unwrap();
        assert_eq!((pass.handed_back, pass.requeued), (1, 0), "spent {spent}");
        assert!(pass.confirmed.is_empty(), "spent {spent}");
        let j = judgement(&db, claimed.judgement.id).await;
        assert_eq!(
            (j.status, j.is_finalized),
            (SubmissionStatus::Queued, false)
        );
        let shown = sub(&db, s.id).await;
        assert_eq!(shown.status, SubmissionStatus::Queued);
        assert_eq!(shown.system_error_retries, spent, "no budget spent");
        assert_eq!(claimable(&db).await, 0, "it waits parked");

        provider(&db, true).await;
        assert_eq!(claimable(&db).await, 1, "judged again once enabled");
        finish(
            &db,
            &judgement(&db, claimed.judgement.id).await,
            Verdict::Accepted,
        )
        .await;
    }
}

/// A system error that stands stays what the submission shows when its
/// contest type is parked: parking holds back only work still undecided, so
/// a decided error is never taken back. A plugin's standing error, a refusal
/// and an error the retry confirmed alike.
#[tokio::test]
async fn a_system_error_that_stands_stays_shown_when_its_type_is_parked() {
    let (_pg, db) = crate::test_support::postgres().await;
    provider(&db, true).await;
    let budget = RetryBudget::new(0);
    let (_, refused) = new_submission(&db).await;
    let claimed = claim_one(&db, "srv").await;
    fail_dispatch(
        &db,
        Unit::Judgement,
        claimed.judgement.id,
        claimed.judgement.judge_epoch,
        &DispatchFailure::refused("no"),
    )
    .await
    .unwrap();
    let (_, confirmed) = new_submission(&db).await;
    let claimed = claim_one(&db, "srv").await;
    fail_dispatch(
        &db,
        Unit::Judgement,
        claimed.judgement.id,
        claimed.judgement.judge_epoch,
        &DispatchFailure::judging(HostCode::PluginError, "boom"),
    )
    .await
    .unwrap();
    let pass = retry_system_errors(&db, budget, 10, NO_TYPES)
        .await
        .unwrap();
    assert_eq!(pass.confirmed.len(), 1);

    provider(&db, false).await;
    let pass = retry_system_errors(&db, budget, 10, NO_TYPES)
        .await
        .unwrap();
    assert_eq!(pass, SystemErrorPass::default(), "nothing is taken back");
    for j in [refused, confirmed] {
        let shown = sub(&db, j.submission_id).await;
        assert_eq!(shown.status, SubmissionStatus::SystemError);
        let j = judgement(&db, j.id).await;
        assert!(j.error_stands && j.is_finalized);
    }
}

/// While its type is parked no system error spends budget, however many
/// await a decision and however small the batch: one pick decides each.
#[tokio::test]
async fn no_system_error_retry_is_spent_while_its_type_is_parked() {
    let (_pg, db) = crate::test_support::postgres().await;
    provider(&db, true).await;
    let mut failed = Vec::new();
    for _ in 0..3 {
        let (s, _) = new_submission(&db).await;
        let claimed = claim_one(&db, "srv").await;
        fail_dispatch(
            &db,
            Unit::Judgement,
            claimed.judgement.id,
            claimed.judgement.judge_epoch,
            &DispatchFailure::judging(HostCode::PluginError, "boom"),
        )
        .await
        .unwrap();
        failed.push(s.id);
    }
    provider(&db, false).await;
    let pass = retry_system_errors(&db, RetryBudget::new(5), 2, NO_TYPES)
        .await
        .unwrap();
    assert_eq!((pass.handed_back, pass.requeued), (2, 0));
    let pass = retry_system_errors(&db, RetryBudget::new(5), 2, NO_TYPES)
        .await
        .unwrap();
    assert_eq!((pass.handed_back, pass.requeued), (1, 0));
    for id in failed {
        let s = sub(&db, id).await;
        assert_eq!(s.status, SubmissionStatus::Queued);
        assert_eq!(s.system_error_retries, 0, "no budget spent while parked");
    }
}

/// A code run ends by the judgement's rule: an attempt the platform lost
/// goes back to the queue with no retry spent, so does one whose type is
/// parked, and anything else is finalized as a system error.
#[tokio::test]
async fn a_failed_code_run_is_handed_back_or_finalized_by_the_judgements_rule() {
    let (_pg, db) = crate::test_support::postgres().await;
    provider(&db, true).await;
    let claim = |db: DatabaseConnection| async move {
        let mut runs = claim_code_runs(&db, "srv", TEN, NO_TYPES)
            .await
            .unwrap()
            .units;
        assert_eq!(runs.len(), 1, "exactly one code run is claimable");
        runs.remove(0).unit
    };
    let run = new_code_run(&db).await;
    let lost = DispatchFailure::from(super::fault::Recorded {
        fault: OperationFault::OperationLost.into(),
        message: "lost".into(),
    });
    let judging = DispatchFailure::judging(HostCode::PluginError, "boom");
    let find = |db: DatabaseConnection, id| async move {
        code_run::Entity::find_by_id(id)
            .one(&db)
            .await
            .unwrap()
            .unwrap()
    };

    let claimed = claim(db.clone()).await;
    let outcome = fail_dispatch(&db, Unit::CodeRun, run.id, claimed.judge_epoch, &lost)
        .await
        .unwrap();
    assert_eq!(outcome, DispatchFailed::HandedBack);
    let after = find(db.clone(), run.id).await;
    assert_eq!(after.status, SubmissionStatus::Queued);
    assert_eq!(after.dispatch_retries, 0);
    assert!(
        after.judge_epoch > claimed.judge_epoch,
        "the lost dispatch is fenced"
    );

    let claimed = claim(db.clone()).await;
    provider(&db, false).await;
    let outcome = fail_dispatch(&db, Unit::CodeRun, run.id, claimed.judge_epoch, &judging)
        .await
        .unwrap();
    assert_eq!(outcome, DispatchFailed::HandedBack, "parked");
    assert_eq!(
        find(db.clone(), run.id).await.status,
        SubmissionStatus::Queued
    );

    provider(&db, true).await;
    let claimed = claim(db.clone()).await;
    let outcome = fail_dispatch(&db, Unit::CodeRun, run.id, claimed.judge_epoch, &judging)
        .await
        .unwrap();
    assert_eq!(outcome, DispatchFailed::SystemError);
    let after = find(db.clone(), run.id).await;
    assert_eq!(after.status, SubmissionStatus::SystemError);
    assert_eq!(after.error_code.as_deref(), Some("PLUGIN_ERROR"));
}

/// A judgement superseded before it finished never got a verdict: it ends
/// `Superseded`, history and not a system error, which no retry takes and
/// which cannot be applied back.
#[tokio::test]
async fn a_judgement_superseded_unfinished_ends_superseded() {
    let (_pg, db) = crate::test_support::postgres().await;
    let (s, first) = new_submission(&db).await;
    claim_one(&db, "srv").await;
    rejudge(&db, s.id, true).await;
    let superseded = judgement(&db, first.id).await;
    assert_eq!(superseded.status, SubmissionStatus::Superseded);
    assert!(superseded.is_finalized && !superseded.is_current);
    let pass = retry_system_errors(&db, RetryBudget::new(5), 10, NO_TYPES)
        .await
        .unwrap();
    assert_eq!(pass, SystemErrorPass::default(), "nothing to re-judge");

    let txn = db.begin().await.unwrap();
    assert!(matches!(
        apply_judgement(&txn, s.id, first.id).await.unwrap(),
        ApplyJudgement::Superseded
    ));
}

/// In-flight work of a parked type whose owner was lost goes back without
/// spending a dispatch retry, even with the budget spent.
#[tokio::test]
async fn lapsed_work_of_a_parked_type_is_handed_back() {
    let (_pg, db) = crate::test_support::postgres().await;
    provider(&db, true).await;
    let (s, j) = new_submission(&db).await;
    provider(&db, false).await;
    abandon(&db, 0).await;
    let reclaimed = steal(&db).await;
    assert_eq!((reclaimed.requeued, reclaimed.exhausted), (1, 0));
    let after = judgement(&db, j.id).await;
    assert_eq!(after.status, SubmissionStatus::Queued);
    assert_eq!(after.dispatch_retries, 0);
    assert_eq!(sub(&db, s.id).await.status, SubmissionStatus::Queued);
}
