//! Transitions of a dispatch unit (a judgement or a code run): claim, lease
//! refresh, requeue, exhaust and dispatch failure.

use common::db::{DatabaseConnection, DatabaseTransaction};
use common::{DlqErrorCode, StatusPhase, SubmissionDlqErrorCode, SubmissionStatus};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbBackend, DbErr, EntityTrait, QueryFilter, Statement,
    TransactionTrait, Value,
};

use super::RetryBudget;
use crate::dispatcher::prefer_local_handler;
use crate::dlq::{DlqService, DlqSubject};
use crate::entity::{code_run, submission, submission_judgement};

/// The kind of row the dispatcher judges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    /// A `submission_judgement` row; its submission row caches it while it
    /// is current.
    Judgement,
    /// A `code_run` row.
    CodeRun,
}

impl Unit {
    pub(crate) fn table(self) -> &'static str {
        match self {
            Unit::Judgement => "submission_judgement",
            Unit::CodeRun => "code_run",
        }
    }

    /// `FROM` clause of a scan: the unit as `u`, and for a judgement its
    /// submission as `s`.
    fn scan_from(self) -> &'static str {
        match self {
            Unit::Judgement => "submission_judgement u JOIN submission s ON s.id = u.submission_id",
            Unit::CodeRun => "code_run u",
        }
    }

    /// Row locks of a scan. A judgement scan locks its submission too, so
    /// the statements that follow never wait for either row; the submission
    /// comes first, so a row skipped because its submission is busy leaves
    /// no lock behind.
    fn scan_lock(self) -> &'static str {
        match self {
            Unit::Judgement => "FOR UPDATE OF s, u SKIP LOCKED",
            Unit::CodeRun => "FOR UPDATE OF u SKIP LOCKED",
        }
    }

    fn contest_type(self) -> &'static str {
        match self {
            Unit::Judgement => "s.contest_type",
            Unit::CodeRun => "u.contest_type",
        }
    }

    /// `SET` items that finalize a row of this unit.
    fn finalize_sets(self) -> &'static str {
        match self {
            Unit::Judgement => "is_finalized = TRUE, finalized_at = NOW()",
            Unit::CodeRun => "judged_at = NOW()",
        }
    }

    /// `SET` items that clear every judged-output column, ready for a new
    /// dispatch.
    fn cleared_output_sets(self) -> &'static str {
        match self {
            Unit::Judgement => {
                "verdict = NULL, score = NULL, time_used = NULL, memory_used = NULL, \
                 compile_output = NULL, error_code = NULL, error_message = NULL, \
                 is_finalized = FALSE, finalized_at = NULL"
            }
            Unit::CodeRun => {
                "verdict = NULL, score = NULL, time_used = NULL, memory_used = NULL, \
                 compile_output = NULL, error_code = NULL, error_message = NULL, \
                 judged_at = NULL"
            }
        }
    }

    /// CTE that deletes the results of the rows of CTE `moved`.
    fn wipe_results(self) -> &'static str {
        match self {
            Unit::Judgement => {
                "wiped AS (DELETE FROM test_case_result \
                 WHERE judgement_id IN (SELECT id FROM moved))"
            }
            Unit::CodeRun => {
                "wiped AS (DELETE FROM code_run_result \
                 WHERE code_run_id IN (SELECT id FROM moved))"
            }
        }
    }

    /// CTEs (each with a leading comma) that keep derived rows in step with
    /// the rows of CTE `moved`: a current judgement's submission cache, and
    /// the dead letter of a submission whose current judgement just
    /// recovered. A code run derives nothing.
    fn cascade(self) -> String {
        match self {
            Unit::Judgement => format!(
                ", mirrored AS (UPDATE submission s SET {MIRROR_SETS} FROM moved j \
                 WHERE s.id = j.submission_id AND j.is_current RETURNING s.id, s.status), \
                 recovered AS (UPDATE dead_letter_message d \
                     SET resolved = TRUE, resolved_at = NOW(), resolved_by = NULL \
                     FROM moved j WHERE NOT d.resolved AND j.is_current AND {recovered} \
                       AND d.message_id = {submission})",
                recovered = recovered("j"),
                submission = DlqSubject::Submission(0).sql_message_id("j.submission_id"),
            ),
            Unit::CodeRun => String::new(),
        }
    }
}

/// SQL: the unit row `alias` finished as a system error in any shape, a
/// `SystemError` status or a plugin's `SystemError` verdict.
fn system_error(alias: &str) -> String {
    format!(
        "({alias}.status = '{system_error}' \
          OR ({alias}.status = '{judged}' AND {alias}.verdict IS NOT DISTINCT FROM '{verdict}'))",
        system_error = SubmissionStatus::SystemError,
        judged = SubmissionStatus::Judged,
        verdict = common::Verdict::SystemError,
    )
}

/// SQL: the unit row `alias` finished with a verdict that stands, anything
/// terminal but a system error. Its dead letters are resolved.
fn recovered(alias: &str) -> String {
    format!(
        "({alias}.status IN {terminal} AND NOT {system_error})",
        terminal = StatusPhase::Terminal.sql_list(),
        system_error = system_error(alias),
    )
}

/// The submission cache columns, each copied from its current judgement `j`.
const MIRROR_SETS: &str = "status = j.status, judge_epoch = j.judge_epoch, verdict = j.verdict, \
     score = j.score, time_used = j.time_used, memory_used = j.memory_used, \
     compile_output = j.compile_output, error_code = j.error_code, \
     error_message = j.error_message, judged_at = j.finalized_at, \
     target_worker_id = j.target_worker_id";

/// The cascade CTEs of a unit write, for the plugin write path.
pub(super) fn cascade(unit: Unit) -> String {
    unit.cascade()
}

/// Rewrites a submission's cache from its current judgement (resolving its
/// dead letter if the judgement's verdict stands) and returns the submission.
pub(super) async fn mirror_current<C: ConnectionTrait>(
    conn: &C,
    submission_id: i32,
) -> Result<submission::Model, DbErr> {
    conn.execute_raw(Statement::from_sql_and_values(
        DbBackend::Postgres,
        format!(
            "WITH moved AS (SELECT * FROM submission_judgement \
                            WHERE submission_id = $1 AND is_current){cascade} \
             SELECT 1",
            cascade = Unit::Judgement.cascade(),
        ),
        [submission_id.into()],
    ))
    .await?;
    submission::Entity::find_by_id(submission_id)
        .one(conn)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("submission {submission_id}")))
}

/// Positional `$n` parameters of a statement built as text.
#[derive(Default)]
struct Binds(Vec<Value>);

impl Binds {
    fn bind(&mut self, value: impl Into<Value>) -> String {
        self.0.push(value.into());
        format!("${}", self.0.len())
    }

    fn statement(self, sql: String) -> Statement {
        Statement::from_sql_and_values(DbBackend::Postgres, sql, self.0)
    }
}

/// A judgement the claim fiber took, with its submission.
#[derive(Debug, Clone)]
pub(crate) struct ClaimedJudgement {
    pub submission: submission::Model,
    pub judgement: submission_judgement::Model,
}

/// A code run the claim fiber took.
pub(crate) type ClaimedCodeRun = code_run::Model;

/// What one claim statement took.
#[derive(Debug, Clone)]
pub(crate) struct Claimed<T> {
    pub units: Vec<T>,
    /// The `plugin_config` change counter, read by the claiming statement
    /// itself (see [`crate::services::plugin_config_cache`]); `None` when it
    /// claimed nothing.
    pub plugin_config_epoch: Option<i64>,
}

/// `UPDATE` that claims the `Queued` rows picked by `picked` for
/// `server_id`, mirrors current judgements, and returns the claimed rows,
/// each with the `plugin_config` change counter.
fn claim_statement(unit: Unit, server_id: &str, limit: u32, local: &[String]) -> Statement {
    let mut b = Binds::default();
    let limit = b.bind(i64::from(limit));
    let owner = b.bind(server_id);
    let local = b.bind(local.to_vec());
    let order = match unit {
        // Live submissions and immediate rejudges go before deferred ones.
        Unit::Judgement => "u.is_current DESC, u.created_at",
        Unit::CodeRun => "u.created_at",
    };
    b.statement(format!(
        "WITH picked AS (SELECT u.id FROM {from} WHERE u.{queued} AND {handled} \
                         ORDER BY {order} LIMIT {limit} {lock}), \
              moved AS (UPDATE {table} t SET status = '{pending}', owner_server_id = {owner}, \
                            lease_heartbeat_at = NOW(), leased_at = NOW() \
                        FROM picked WHERE t.id = picked.id RETURNING t.*){mirror} \
         SELECT moved.*, (SELECT last_value FROM {epoch}) AS plugin_config_epoch FROM moved",
        from = unit.scan_from(),
        queued = crate::dispatcher::QUEUED_PREDICATE,
        handled = prefer_local_handler(unit.contest_type(), &local),
        lock = unit.scan_lock(),
        table = unit.table(),
        pending = SubmissionStatus::Pending,
        mirror = unit.cascade(),
        epoch = crate::services::plugin_config_cache::EPOCH_SEQUENCE,
    ))
}

/// Runs a claim statement, parsing each row as `M` and reading the counter.
async fn run_claim<M: sea_orm::FromQueryResult>(
    db: &DatabaseConnection,
    stmt: Statement,
) -> Result<Claimed<M>, DbErr> {
    let rows = db.query_all_raw(stmt).await?;
    let plugin_config_epoch = rows
        .first()
        .map(|row| row.try_get::<i64>("", "plugin_config_epoch"))
        .transpose()?;
    let units = rows
        .iter()
        .map(|row| M::from_query_result(row, ""))
        .collect::<Result<_, _>>()?;
    Ok(Claimed {
        units,
        plugin_config_epoch,
    })
}

/// Claims up to `limit` queued judgements for `server_id`: `Queued` →
/// `Pending`, owned, with a fresh lease. `local` is the contest types this
/// replica handles (see [`crate::dispatcher::prefer_local_handler`]).
pub(crate) async fn claim_judgements(
    db: &DatabaseConnection,
    server_id: &str,
    limit: u32,
    local: &[String],
) -> Result<Claimed<ClaimedJudgement>, DbErr> {
    let nothing = Claimed {
        units: Vec::new(),
        plugin_config_epoch: None,
    };
    if limit == 0 {
        return Ok(nothing);
    }
    let Claimed {
        units: judgements,
        plugin_config_epoch,
    } = run_claim::<submission_judgement::Model>(
        db,
        claim_statement(Unit::Judgement, server_id, limit, local),
    )
    .await?;
    if judgements.is_empty() {
        return Ok(nothing);
    }
    let ids: Vec<i32> = judgements.iter().map(|j| j.submission_id).collect();
    let submissions: std::collections::HashMap<i32, submission::Model> = submission::Entity::find()
        .filter(submission::Column::Id.is_in(ids))
        .all(db)
        .await?
        .into_iter()
        .map(|s| (s.id, s))
        .collect();
    Ok(Claimed {
        units: judgements
            .into_iter()
            .filter_map(|judgement| {
                Some(ClaimedJudgement {
                    submission: submissions.get(&judgement.submission_id)?.clone(),
                    judgement,
                })
            })
            .collect(),
        plugin_config_epoch,
    })
}

/// Claims up to `limit` queued code runs for `server_id`, as
/// [`claim_judgements`] does.
pub(crate) async fn claim_code_runs(
    db: &DatabaseConnection,
    server_id: &str,
    limit: u32,
    local: &[String],
) -> Result<Claimed<ClaimedCodeRun>, DbErr> {
    if limit == 0 {
        return Ok(Claimed {
            units: Vec::new(),
            plugin_config_epoch: None,
        });
    }
    run_claim(db, claim_statement(Unit::CodeRun, server_id, limit, local)).await
}

/// Units (judgements and code runs) `server_id` owns that are in flight:
/// what the claim fiber's in-flight cap counts.
pub async fn count_in_flight(db: &DatabaseConnection, server_id: &str) -> Result<u64, DbErr> {
    let count = |unit: Unit| {
        format!(
            "(SELECT COUNT(*) FROM {table} WHERE owner_server_id = $1 AND status IN {in_flight})",
            table = unit.table(),
            in_flight = StatusPhase::InFlight.sql_list(),
        )
    };
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "SELECT ({} + {})::bigint AS n",
                count(Unit::Judgement),
                count(Unit::CodeRun)
            ),
            [server_id.into()],
        ))
        .await?;
    let n: i64 = row.map(|r| r.try_get("", "n")).transpose()?.unwrap_or(0);
    Ok(u64::try_from(n).unwrap_or(0))
}

/// Rows of each unit a lease operation touched.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnitCounts {
    pub judgements: u64,
    pub code_runs: u64,
}

/// Bumps the lease heartbeat of every in-flight unit `server_id` owns, with
/// the database clock, which every lease-age comparison also uses. Rows are
/// taken in id order and a busy one is skipped: it is mid-transition, and the
/// lease TTL spans many refresh intervals.
pub(crate) async fn refresh_leases(
    db: &DatabaseConnection,
    server_id: &str,
) -> Result<UnitCounts, DbErr> {
    let mut counts = UnitCounts::default();
    for unit in [Unit::Judgement, Unit::CodeRun] {
        let n = db
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "UPDATE {table} SET lease_heartbeat_at = NOW() \
                     WHERE id IN (SELECT id FROM {table} \
                                  WHERE owner_server_id = $1 AND status IN {in_flight} \
                                  ORDER BY id FOR NO KEY UPDATE SKIP LOCKED)",
                    table = unit.table(),
                    in_flight = StatusPhase::InFlight.sql_list(),
                ),
                [server_id.into()],
            ))
            .await?
            .rows_affected();
        match unit {
            Unit::Judgement => counts.judgements = n,
            Unit::CodeRun => counts.code_runs = n,
        }
    }
    Ok(counts)
}

/// A unit a scan locked, with what a decision needs.
struct Picked {
    id: i32,
    submission_id: Option<i32>,
    is_current: bool,
    dispatch_retries: i32,
}

/// Locks up to `limit` rows of `unit` matching `filter` (SQL over `u`, and
/// `s` for a judgement) and returns them.
async fn pick(
    txn: &DatabaseTransaction,
    unit: Unit,
    mut b: Binds,
    filter: String,
    limit: Option<u32>,
) -> Result<Vec<Picked>, DbErr> {
    let limit = match limit {
        Some(n) => format!("LIMIT {}", b.bind(i64::from(n))),
        None => String::new(),
    };
    let columns = match unit {
        Unit::Judgement => "u.id, u.submission_id, u.is_current, u.dispatch_retries",
        Unit::CodeRun => {
            "u.id, NULL::int AS submission_id, FALSE AS is_current, u.dispatch_retries"
        }
    };
    let rows = txn
        .query_all_raw(b.statement(format!(
            "SELECT {columns} FROM {from} WHERE {filter} ORDER BY u.id {limit} {lock}",
            from = unit.scan_from(),
            lock = unit.scan_lock(),
        )))
        .await?;
    rows.iter()
        .map(|r| {
            Ok(Picked {
                id: r.try_get("", "id")?,
                submission_id: r.try_get("", "submission_id")?,
                is_current: r.try_get("", "is_current")?,
                dispatch_retries: r.try_get("", "dispatch_retries")?,
            })
        })
        .collect()
}

/// Why a unit goes back to `Queued`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Requeue {
    /// Its owner gave it up (shutdown, or boot after a crash). No retry is
    /// spent: nothing about the work failed.
    Released,
    /// Its owner was lost or the job is stuck. Spends one dispatch retry.
    Abandoned,
    /// Its judgement finalized as `SystemError`. Spends one of the
    /// submission's system-error retries; the judgement starts with a fresh
    /// dispatch budget.
    SystemErrorRetry,
}

impl Requeue {
    fn dispatch_retries(self) -> &'static str {
        match self {
            Requeue::Released => "t.dispatch_retries",
            Requeue::Abandoned => "t.dispatch_retries + 1",
            Requeue::SystemErrorRetry => "0",
        }
    }
}

/// Requeues the locked `ids` of `unit`: a fresh epoch, `Queued`, no owner or
/// lease, output cleared and results deleted. The caller's scan locked them
/// and checked the from-state.
async fn requeue(
    txn: &DatabaseTransaction,
    unit: Unit,
    ids: &[i32],
    why: Requeue,
) -> Result<u64, DbErr> {
    if ids.is_empty() {
        return Ok(0);
    }
    let sets = format!(
        "status = '{queued}', queued_at = NOW(), owner_server_id = NULL, \
         lease_heartbeat_at = NULL, leased_at = NULL, dispatch_retries = {retries}, {cleared}",
        queued = SubmissionStatus::Queued,
        retries = why.dispatch_retries(),
        cleared = unit.cleared_output_sets(),
    );
    let statement = match unit {
        Unit::CodeRun => Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "WITH moved AS (UPDATE code_run t SET {sets}, judge_epoch = t.judge_epoch + 1 \
                                WHERE t.id = ANY($1) RETURNING t.id), {wipe} \
                 SELECT id FROM moved",
                wipe = unit.wipe_results(),
            ),
            [ids.to_vec().into()],
        ),
        Unit::Judgement => {
            // One allocation per judgement: a submission with several
            // judgements in the batch takes that many consecutive epochs.
            let se_retry = if why == Requeue::SystemErrorRetry {
                ", system_error_retries = s.system_error_retries + 1"
            } else {
                ""
            };
            let rows = txn
                .query_all_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    format!(
                        "WITH picked AS (SELECT id, submission_id FROM submission_judgement \
                                         WHERE id = ANY($1)), \
                              counts AS (SELECT submission_id, COUNT(*)::int AS n FROM picked \
                                         GROUP BY submission_id), \
                              alloc AS (UPDATE submission s \
                                        SET max_judge_epoch = s.max_judge_epoch + c.n{se_retry} \
                                        FROM counts c WHERE s.id = c.submission_id \
                                        RETURNING s.id, s.max_judge_epoch, c.n) \
                         SELECT p.id, (a.max_judge_epoch - a.n + ROW_NUMBER() OVER \
                                 (PARTITION BY p.submission_id ORDER BY p.id))::int AS epoch \
                         FROM picked p JOIN alloc a ON a.id = p.submission_id"
                    ),
                    [ids.to_vec().into()],
                ))
                .await?;
            let mut moved = Vec::with_capacity(rows.len());
            let mut epochs = Vec::with_capacity(rows.len());
            for row in rows {
                moved.push(row.try_get::<i32>("", "id")?);
                epochs.push(row.try_get::<i32>("", "epoch")?);
            }
            Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "WITH v AS (SELECT * FROM unnest($1::int[], $2::int[]) AS v(id, epoch)), \
                          moved AS (UPDATE submission_judgement t \
                                    SET {sets}, judge_epoch = v.epoch \
                                    FROM v WHERE t.id = v.id RETURNING t.*), {wipe}{mirror} \
                     SELECT id FROM moved",
                    wipe = unit.wipe_results(),
                    mirror = unit.cascade(),
                ),
                [moved.into(), epochs.into()],
            )
        }
    };
    Ok(txn.execute_raw(statement).await?.rows_affected())
}

/// Requeues every in-flight unit `server_id` owns without spending a retry,
/// so the fleet (or this server once it is back) dispatches it again. Run at
/// boot, for work a previous instance of this server id left behind, and on
/// graceful shutdown, after the dispatcher stopped. The new epoch fences any
/// write still arriving from the old dispatch.
pub async fn release_leases(db: &DatabaseConnection, server_id: &str) -> Result<UnitCounts, DbErr> {
    let txn = db.begin().await?;
    let mut counts = UnitCounts::default();
    for unit in [Unit::Judgement, Unit::CodeRun] {
        let mut b = Binds::default();
        let filter = format!(
            "u.owner_server_id = {} AND u.status IN {}",
            b.bind(server_id),
            StatusPhase::InFlight.sql_list()
        );
        let ids: Vec<i32> = pick(&txn, unit, b, filter, None)
            .await?
            .into_iter()
            .map(|p| p.id)
            .collect();
        let n = requeue(&txn, unit, &ids, Requeue::Released).await?;
        match unit {
            Unit::Judgement => counts.judgements = n,
            Unit::CodeRun => counts.code_runs = n,
        }
    }
    txn.commit().await?;
    Ok(counts)
}

/// How a detector decides that an in-flight unit's owner no longer drives
/// it. Both compare timestamps stamped with the database clock against the
/// database clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Abandoned {
    /// The lease steal: the owner stopped refreshing the lease for
    /// `lease_ttl_secs`. Needs every live server to refresh its leases.
    LeaseLapsed { lease_ttl_secs: u64 },
    /// The stuck-job detector: no heartbeat for `timeout_secs`, or (when set)
    /// dispatched more than `max_inflight_secs` ago, which also catches a
    /// wedged job whose live server keeps its lease fresh.
    Stuck {
        timeout_secs: u64,
        max_inflight_secs: Option<u64>,
    },
}

impl Abandoned {
    fn predicate(self, b: &mut Binds) -> String {
        let lapsed = |b: &mut Binds, secs: u64| {
            format!(
                "(u.lease_heartbeat_at IS NULL \
                  OR u.lease_heartbeat_at < NOW() - make_interval(secs => {}))",
                b.bind(secs as f64)
            )
        };
        match self {
            Abandoned::LeaseLapsed { lease_ttl_secs } => lapsed(b, lease_ttl_secs),
            Abandoned::Stuck {
                timeout_secs,
                max_inflight_secs,
            } => {
                let lapsed = lapsed(b, timeout_secs);
                match max_inflight_secs {
                    Some(cap) => format!(
                        "({lapsed} OR u.leased_at < NOW() - make_interval(secs => {}))",
                        b.bind(cap as f64)
                    ),
                    None => lapsed,
                }
            }
        }
    }

    /// The submission-facing error code, DLQ code and message a unit is
    /// finalized with when its dispatch-retry budget is spent.
    fn exhausted(self, budget: RetryBudget) -> (&'static str, DlqErrorCode, String) {
        match self {
            Abandoned::LeaseLapsed { .. } => (
                SubmissionDlqErrorCode::DISPATCH_RETRY_EXHAUSTED,
                DlqErrorCode::DispatchRetryExhausted,
                format!(
                    "Dispatch retry limit exhausted after {} retries",
                    budget.max()
                ),
            ),
            Abandoned::Stuck { .. } => (
                SubmissionDlqErrorCode::STUCK_JOB,
                DlqErrorCode::StuckJob,
                format!("Exceeded {} retries", budget.max()),
            ),
        }
    }
}

/// What a reclaim scan did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Reclaimed {
    pub requeued: u64,
    pub exhausted: u64,
}

/// Takes over up to `limit` in-flight units of `unit` that `abandoned`
/// selects and this replica can judge: each with dispatch retries left is
/// requeued (spending one), each without is finalized as `SystemError` and
/// dead-lettered.
pub(crate) async fn reclaim_abandoned(
    db: &DatabaseConnection,
    unit: Unit,
    abandoned: Abandoned,
    budget: RetryBudget,
    limit: u32,
    local: &[String],
) -> Result<Reclaimed, DbErr> {
    let txn = db.begin().await?;
    let mut b = Binds::default();
    let predicate = abandoned.predicate(&mut b);
    let local = b.bind(local.to_vec());
    let filter = format!(
        "u.status IN {in_flight} AND {predicate} AND {handled}",
        in_flight = StatusPhase::InFlight.sql_list(),
        handled = prefer_local_handler(unit.contest_type(), &local),
    );
    let picked = pick(&txn, unit, b, filter, Some(limit)).await?;
    let (retry, exhaust): (Vec<Picked>, Vec<Picked>) = picked
        .into_iter()
        .partition(|p| budget.allows(p.dispatch_retries));

    let retry_ids: Vec<i32> = retry.iter().map(|p| p.id).collect();
    let requeued = requeue(&txn, unit, &retry_ids, Requeue::Abandoned).await?;
    let (error_code, dlq_code, message) = abandoned.exhausted(budget);
    let exhausted = finalize_system_error(&txn, unit, &exhaust, error_code, &message).await?;
    for p in &exhaust {
        dead_letter(&txn, unit, p, dlq_code, &message).await?;
    }
    txn.commit().await?;
    Ok(Reclaimed {
        requeued,
        exhausted,
    })
}

/// Finalizes the locked `rows` as `SystemError` with `error_code`.
async fn finalize_system_error(
    txn: &DatabaseTransaction,
    unit: Unit,
    rows: &[Picked],
    error_code: &str,
    message: &str,
) -> Result<u64, DbErr> {
    if rows.is_empty() {
        return Ok(0);
    }
    let ids: Vec<i32> = rows.iter().map(|p| p.id).collect();
    let n = txn
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "WITH moved AS (UPDATE {table} t SET status = '{system_error}', \
                                    error_code = $2, error_message = $3, {finalize} \
                                WHERE t.id = ANY($1) RETURNING t.*){mirror} \
                 SELECT id FROM moved",
                table = unit.table(),
                system_error = SubmissionStatus::SystemError,
                finalize = unit.finalize_sets(),
                mirror = unit.cascade(),
            ),
            [ids.into(), error_code.into(), message.into()],
        ))
        .await?
        .rows_affected();
    Ok(n)
}

/// Records an exhausted unit in the dead letter queue: a current judgement
/// under its submission, any other unit under itself.
async fn dead_letter(
    txn: &DatabaseTransaction,
    unit: Unit,
    picked: &Picked,
    error_code: DlqErrorCode,
    message: &str,
) -> Result<(), DbErr> {
    let subject = match (unit, picked.submission_id) {
        (Unit::Judgement, Some(submission_id)) if picked.is_current => {
            DlqSubject::Submission(submission_id)
        }
        (Unit::Judgement, _) => DlqSubject::Judgement(picked.id),
        (Unit::CodeRun, _) => DlqSubject::CodeRun(picked.id),
    };
    let payload = serde_json::json!({
        "unit": unit.table(),
        "id": picked.id,
        "submission_id": picked.submission_id,
        "dispatch_retries": picked.dispatch_retries,
    });
    DlqService::new(txn)
        .dead_letter(subject, picked.submission_id, payload, error_code, message)
        .await
}

/// Requeues up to `limit` current judgements that finalized as a
/// `SystemError` in any shape (a plugin's `SystemError` verdict, or a
/// `SystemError` status from a dispatch failure or exhaustion), while their
/// submission has system-error retries left. A deterministic plugin refusal
/// (`PLUGIN_REFUSED`) can only fail again and is left standing.
pub(crate) async fn retry_system_errors(
    db: &DatabaseConnection,
    budget: RetryBudget,
    limit: u32,
    local: &[String],
) -> Result<u64, DbErr> {
    let txn = db.begin().await?;
    let mut b = Binds::default();
    let filter = format!(
        "u.is_current AND u.is_finalized \
         AND {system_error} \
         AND u.error_code IS DISTINCT FROM {refused} \
         AND s.system_error_retries < {max} AND {handled}",
        system_error = system_error("u"),
        refused = b.bind(SubmissionDlqErrorCode::PLUGIN_REFUSED),
        max = b.bind(budget.max()),
        handled = {
            let local = b.bind(local.to_vec());
            prefer_local_handler("s.contest_type", &local)
        },
    );
    let ids: Vec<i32> = pick(&txn, Unit::Judgement, b, filter, Some(limit))
        .await?
        .into_iter()
        .map(|p| p.id)
        .collect();
    let n = requeue(&txn, Unit::Judgement, &ids, Requeue::SystemErrorRetry).await?;
    txn.commit().await?;
    Ok(n)
}

/// Finalizes a unit whose dispatch failed as `SystemError`, fenced on its
/// epoch: a failure reported by a dispatch that was since requeued or
/// superseded changes nothing. Returns whether the unit was finalized.
pub(crate) async fn fail_dispatch(
    db: &DatabaseConnection,
    unit: Unit,
    id: i32,
    judge_epoch: i32,
    error_code: &str,
    error_message: &str,
) -> Result<bool, DbErr> {
    // A judgement locks its submission first, as every judgement transition
    // does; the UPDATE joins the lock so it is taken before the row's.
    let (lock, from, guard) = match unit {
        Unit::Judgement => (
            "WITH locked AS (SELECT s.id FROM submission s JOIN submission_judgement j \
                             ON j.submission_id = s.id WHERE j.id = $1 FOR NO KEY UPDATE OF s), ",
            "FROM locked",
            "AND t.submission_id = locked.id AND NOT t.is_finalized",
        ),
        Unit::CodeRun => ("WITH ", "", ""),
    };
    let n = db
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "{lock}moved AS (UPDATE {table} t SET status = '{system_error}', \
                                     error_code = $3, error_message = $4, {finalize} {from} \
                                 WHERE t.id = $1 AND t.judge_epoch = $2 \
                                   AND t.status IN {in_flight} {guard} \
                                 RETURNING t.*){mirror} \
                 SELECT id FROM moved",
                table = unit.table(),
                system_error = SubmissionStatus::SystemError,
                finalize = unit.finalize_sets(),
                in_flight = StatusPhase::InFlight.sql_list(),
                mirror = unit.cascade(),
            ),
            [
                id.into(),
                judge_epoch.into(),
                error_code.into(),
                error_message.into(),
            ],
        ))
        .await?
        .rows_affected();
    Ok(n > 0)
}
