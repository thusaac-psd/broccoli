//! Transitions of a dispatch unit (a judgement or a code run): claim, lease
//! refresh, requeue, exhaust and dispatch failure.

use crate::error::HostCode;
use common::db::{DatabaseConnection, DatabaseTransaction};
use common::{DlqErrorCode, StatusPhase, SubmissionStatus};
use sea_orm::{ConnectionTrait, DbBackend, DbErr, EntityTrait, Statement, TransactionTrait, Value};

use super::RetryBudget;
use super::dispatch::DispatchedUnit;
use super::fault::{DispatchFailure, SystemErrorClass};
use crate::dlq::{DlqService, DlqSubject};
use crate::entity::{code_run, submission, submission_judgement};
use crate::providers::{Loaded, Provided};

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
    pub(crate) fn scan_from(self) -> &'static str {
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

    /// The keys the unit needs a provider of (`providers::Needs`), each as
    /// SQL over a [`Unit::scan_from`] scan ([`crate::providers::sql_needs`]):
    /// its contest type, its problem's type, its problem's checker format
    /// when the evaluator uses a checker, and its language.
    fn needs(self) -> Vec<(Provided, String)> {
        let row = match self {
            Unit::Judgement => "s",
            Unit::CodeRun => "u",
        };
        crate::providers::sql_needs(
            &format!("{row}.contest_type"),
            &format!("(SELECT pt.problem_type FROM problem pt WHERE pt.id = {row}.problem_id)"),
            &format!("(SELECT pt.checker_format FROM problem pt WHERE pt.id = {row}.problem_id)"),
            &format!("{row}.language"),
            &format!("(SELECT pt.checker_language FROM problem pt WHERE pt.id = {row}.problem_id)"),
        )
    }

    /// SQL over a judgement's [`Unit::scan_from`] scan: each plugin the
    /// judgement is judged by (the fleet-wide provider of each key it needs)
    /// with its inputs epoch (`plugin.inputs_epoch`), as a JSON object. A
    /// plugin-scope judging-input config moves only its plugin's epoch, so
    /// it reaches exactly the judgements that plugin judges.
    pub(crate) fn plugin_epochs() -> String {
        let providers: Vec<String> = Unit::Judgement
            .needs()
            .iter()
            .map(|(kind, key)| crate::providers::provider(*kind, key))
            .collect();
        format!(
            "(SELECT COALESCE(jsonb_object_agg(e.id, e.inputs_epoch), '{{}}'::jsonb) \
               FROM plugin e WHERE e.id IN ({}))",
            providers.join(", ")
        )
    }

    /// SQL over a [`Unit::scan_from`] scan: the unit may run now, every
    /// key it needs having a provider enabled fleet-wide
    /// ([`crate::providers::runnable`]). A unit that may not is parked.
    pub(crate) fn runnable(self) -> String {
        crate::providers::runnable(&self.needs())
    }

    /// SQL over a [`Unit::scan_from`] scan: this replica may take the unit,
    /// having every key it needs loaded on its fleet-wide provider, or no
    /// live replica having one
    /// ([`crate::providers::runs_here`]); `local` is the placeholder of
    /// what this replica has loaded.
    fn runs_here(self, local: &str) -> String {
        crate::providers::runs_here(&self.needs(), local)
    }

    /// `SET` items, each led by `, `, that claiming a row of this unit adds to
    /// its lease columns: a judgement records whether its problem's stored
    /// configs had issues (`problem.config_issues`) when it was dispatched.
    /// The inputs version is stamped where the dispatch reads the inputs
    /// (`judging::stamp_inputs_version`).
    fn claim_sets(self) -> &'static str {
        match self {
            Unit::Judgement => {
                ", config_issues = \
                   (SELECT p.config_issues FROM submission s \
                    JOIN problem p ON p.id = s.problem_id WHERE s.id = t.submission_id)"
            }
            Unit::CodeRun => "",
        }
    }

    /// `SET` items that finalize a row of this unit.
    fn finalize_sets(self) -> &'static str {
        match self {
            Unit::Judgement => "is_finalized = TRUE, finalized_at = NOW()",
            Unit::CodeRun => "judged_at = NOW()",
        }
    }

    /// SQL: the row `alias` is unit `id` at epoch `epoch`, still in the
    /// dispatch that epoch names.
    fn open_dispatch(self, alias: &str, id: &str, epoch: &str) -> String {
        let finalized = match self {
            Unit::Judgement => format!(" AND NOT {alias}.is_finalized"),
            Unit::CodeRun => String::new(),
        };
        format!(
            "{alias}.id = {id} AND {alias}.judge_epoch = {epoch} \
             AND {alias}.status IN {in_flight}{finalized}",
            in_flight = StatusPhase::InFlight.sql_list(),
        )
    }

    /// `UPDATE` (as `t`, without its `WHERE`) that finalizes rows of this
    /// unit as `SystemError` with the error code and message the placeholders
    /// `code` and `message` bind. A judgement records whether its error
    /// stands; a code run is never retried, so its error always does.
    fn finalize_system_error(self, code: &str, message: &str, class: SystemErrorClass) -> String {
        let stands = match self {
            Unit::Judgement => format!(", error_stands = {}", class == SystemErrorClass::Stands),
            Unit::CodeRun => String::new(),
        };
        format!(
            "UPDATE {table} t SET status = '{system_error}', error_code = {code}, \
             error_message = {message}, {finalize}{stands}",
            table = self.table(),
            system_error = SubmissionStatus::SystemError,
            finalize = self.finalize_sets(),
        )
    }

    /// `SET` items that clear every judged-output column, ready for a new
    /// dispatch.
    fn cleared_output_sets(self) -> &'static str {
        match self {
            Unit::Judgement => {
                "verdict = NULL, score = NULL, time_used = NULL, memory_used = NULL, \
                 compile_output = NULL, error_code = NULL, error_message = NULL, \
                 error_stands = FALSE, is_finalized = FALSE, finalized_at = NULL"
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
                 WHERE s.id = j.submission_id AND j.is_current AND NOT {undecided} \
                 RETURNING s.id, s.status), \
                 recovered AS (UPDATE dead_letter_message d \
                     SET resolved = TRUE, resolved_at = NOW(), resolved_by = NULL \
                     FROM moved j WHERE NOT d.resolved AND j.is_current AND {recovered} \
                       AND {settled})",
                undecided = awaits_retry_decision("j", "s"),
                recovered = recovered("j"),
                settled = settled_by_submission("d", "j"),
            ),
            Unit::CodeRun => String::new(),
        }
    }
}

/// SQL: the unit row `alias` finished as a system error in any shape: a
/// `SystemError` status (the host's), or a terminal status with a
/// `SystemError` verdict (a plugin's, as `plugin_writes::as_recorded` reads
/// the write that ends it).
fn system_error(alias: &str) -> String {
    format!(
        "({alias}.status = '{system_error}' \
          OR ({alias}.status IN {terminal} AND {alias}.verdict IS NOT DISTINCT FROM '{verdict}'))",
        system_error = SubmissionStatus::SystemError,
        terminal = StatusPhase::Terminal.sql_list(),
        verdict = common::Verdict::SystemError,
    )
}

/// SQL: the judgement `j` of submission `s` finished as a system error that
/// the system-error retry may still re-judge: one that does not stand and
/// that its submission does not already show as its final result (a terminal
/// status at the judgement's epoch): a judgement its submission shows is
/// decided and never reopened. The submission keeps showing the attempt
/// in flight instead of a terminal status that the retry would take back:
/// [`retry_system_errors`], which owns the retry budget, either requeues the
/// judgement or confirms the error.
fn awaits_retry_decision(j: &str, s: &str) -> String {
    format!(
        "({j}.is_finalized AND {system_error} AND NOT {j}.error_stands \
          AND NOT ({s}.status IN {terminal} AND {s}.judge_epoch = {j}.judge_epoch))",
        system_error = system_error(j),
        terminal = StatusPhase::Terminal.sql_list(),
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

/// SQL: the dead letter `d` is one that its submission's current judgement
/// `j` settles once its verdict stands: the submission's own letter, or an
/// operation task that ran for one of its judgements (a lost attempt, or an
/// operation the attempt that stands re-ran).
fn settled_by_submission(d: &str, j: &str) -> String {
    format!(
        "{d}.submission_id = {j}.submission_id \
         AND ({d}.message_id = {submission} OR {d}.direction = '{operation}')",
        submission = DlqSubject::Submission(0).sql_message_id(&format!("{j}.submission_id")),
        operation = common::DlqMessageType::OperationTask.as_str(),
    )
}

/// Resolves dead letter `id` at once if its submission's verdict already
/// stands: a letter filed after the attempt that recovered (an operation's
/// dead letter reaches the database through the queue, after its submission
/// may have been judged again). Returns whether it did.
pub(crate) async fn resolve_if_settled<C: ConnectionTrait>(
    conn: &C,
    id: i32,
) -> Result<bool, DbErr> {
    let n = conn
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "UPDATE dead_letter_message d \
                 SET resolved = TRUE, resolved_at = NOW(), resolved_by = NULL \
                 FROM submission_judgement j \
                 WHERE d.id = $1 AND NOT d.resolved AND j.is_current AND {recovered} \
                   AND {settled}",
                recovered = recovered("j"),
                settled = settled_by_submission("d", "j"),
            ),
            [id.into()],
        ))
        .await?
        .rows_affected();
    Ok(n > 0)
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

/// What dispatch reads from a claimed judgement's submission: the columns
/// that never change once it is created.
#[derive(Debug, Clone, sea_orm::FromQueryResult)]
pub(crate) struct ClaimedSubmission {
    pub id: i32,
    pub user_id: i32,
    pub problem_id: i32,
    pub contest_id: Option<i32>,
    pub contest_type: String,
    pub language: String,
    pub files: serde_json::Value,
}

impl ClaimedSubmission {
    /// Its columns in a claim's result, each `s.<column> AS sub_<column>`.
    const COLUMNS: &'static str = "s.id AS sub_id, s.user_id AS sub_user_id, \
         s.problem_id AS sub_problem_id, s.contest_id AS sub_contest_id, \
         s.contest_type AS sub_contest_type, s.language AS sub_language, \
         s.files AS sub_files";
    const PREFIX: &'static str = "sub_";
}

/// A judgement the claim fiber took, with its submission.
#[derive(Debug, Clone)]
pub(crate) struct ClaimedJudgement {
    pub submission: ClaimedSubmission,
    pub judgement: submission_judgement::Model,
}

/// A unit the claim fiber takes, as its dispatch carries it.
pub(crate) trait ClaimedUnit {
    fn dispatched(&self) -> DispatchedUnit;
    /// The contest type it is judged under.
    fn contest_type(&self) -> &str;
    /// The language it is in.
    fn language(&self) -> &str;
}

impl ClaimedUnit for ClaimedJudgement {
    fn contest_type(&self) -> &str {
        &self.submission.contest_type
    }

    fn language(&self) -> &str {
        &self.submission.language
    }

    fn dispatched(&self) -> DispatchedUnit {
        DispatchedUnit::Judgement {
            submission_id: self.judgement.submission_id,
            judgement_id: self.judgement.id,
            judge_epoch: self.judgement.judge_epoch,
        }
    }
}

impl ClaimedUnit for code_run::Model {
    fn contest_type(&self) -> &str {
        &self.contest_type
    }

    fn language(&self) -> &str {
        &self.language
    }

    fn dispatched(&self) -> DispatchedUnit {
        DispatchedUnit::CodeRun {
            code_run_id: self.id,
            judge_epoch: self.judge_epoch,
        }
    }
}

/// One unit a claim took, with what the claim's metrics record.
#[derive(Debug, Clone)]
pub(crate) struct Claim<T> {
    pub unit: T,
    /// Its problem's type (`problem.problem_type`).
    pub problem_type: String,
    /// Its problem's checker format (`problem.checker_format`).
    pub checker_format: String,
    /// Its problem's checker language (`problem.checker_language`).
    pub checker_language: Option<String>,
    /// Seconds from its creation to the claim, on the database clock.
    pub queued_secs: f64,
}

impl<T: ClaimedUnit> Claim<T> {
    /// What the unit needs a provider of, its problem as the claim read it.
    pub(crate) fn needs<'a>(
        &'a self,
        lifecycle: &crate::plugin_lifecycle::PluginLifecycle,
    ) -> crate::providers::Needs<'a> {
        lifecycle.needs(
            self.unit.contest_type(),
            &self.problem_type,
            &self.checker_format,
            Some(self.unit.language()),
            self.checker_language.as_deref(),
        )
    }
}

/// What one claim statement took.
#[derive(Debug, Clone)]
pub(crate) struct Claimed<T> {
    pub units: Vec<Claim<T>>,
    /// The `plugin_config` change counter, read by the claiming statement
    /// itself (see [`crate::services::plugin_config_cache`]); `None` when it
    /// claimed nothing.
    pub plugin_config_epoch: Option<i64>,
    /// Under an in-flight cap: the room the cap left before this claim (the
    /// cap less the units the server had in flight, at least 0).
    pub cap_room: Option<u32>,
}

/// How many rows one claim statement may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClaimLimit {
    /// At most this many rows.
    pub rows: u32,
    /// And no more than leave the server with this many units (judgements
    /// and code runs) in flight; `None` for no cap.
    pub in_flight_cap: Option<u32>,
}

/// SQL: the units (judgements and code runs) the server bound at `owner`
/// has in flight, what an in-flight cap counts.
fn in_flight_count(owner: &str) -> String {
    let count = |unit: Unit| {
        format!(
            "(SELECT COUNT(*) FROM {table} WHERE owner_server_id = {owner} AND status IN {in_flight})",
            table = unit.table(),
            in_flight = StatusPhase::InFlight.sql_list(),
        )
    };
    format!("({} + {})", count(Unit::Judgement), count(Unit::CodeRun))
}

/// The one statement of a claim: within `limit`, `UPDATE`s the `Queued` rows
/// it picks to `Pending` owned by `server_id` with a fresh lease, mirrors
/// current judgements, and returns every row dispatch and the claim's
/// metrics need, so nothing fallible runs between the claim committing and
/// its dispatch starting. The result always has a first row with the room
/// the cap left (`claim_room`) and the `plugin_config` change counter
/// (`claim_epoch`); with nothing claimed it is that row alone, its unit
/// columns NULL.
fn claim_statement(unit: Unit, server_id: &str, limit: ClaimLimit, local: &Loaded) -> Statement {
    let mut b = Binds::default();
    let rows = b.bind(i64::from(limit.rows));
    let owner = b.bind(server_id);
    let local = b.bind(serde_json::to_value(local).expect("loaded keys serialize"));
    let (room, take) = match limit.in_flight_cap {
        Some(cap) => (
            format!(
                "GREATEST({cap} - {count}, 0)",
                cap = b.bind(i64::from(cap)),
                count = in_flight_count(&owner),
            ),
            format!("LEAST({rows}, (SELECT n FROM room))"),
        ),
        None => ("NULL".to_string(), rows),
    };
    let order = match unit {
        // Live submissions and immediate rejudges go before deferred ones.
        Unit::Judgement => "u.is_current DESC, u.created_at",
        Unit::CodeRun => "u.created_at",
    };
    let (columns, joins) = match unit {
        Unit::Judgement => (
            format!(", {}", ClaimedSubmission::COLUMNS),
            "LEFT JOIN submission s ON s.id = moved.submission_id \
             LEFT JOIN problem p ON p.id = s.problem_id",
        ),
        Unit::CodeRun => (
            String::new(),
            "LEFT JOIN problem p ON p.id = moved.problem_id",
        ),
    };
    b.statement(format!(
        "WITH room AS (SELECT ({room})::bigint AS n), \
              picked AS (SELECT u.id FROM {from} \
                         WHERE u.{queued} AND u.queued_at <= NOW() AND {handled} AND {open} \
                         ORDER BY {order} LIMIT {take} {lock}), \
              moved AS (UPDATE {table} t SET status = '{pending}', owner_server_id = {owner}, \
                            lease_heartbeat_at = NOW(), leased_at = NOW(){claim_sets} \
                        FROM picked WHERE t.id = picked.id RETURNING t.*){mirror} \
         SELECT room.n AS claim_room, (SELECT last_value FROM {epoch}) AS claim_epoch, \
                p.problem_type AS claim_problem_type, p.checker_format AS claim_checker_format, \
                p.checker_language AS claim_checker_language, \
                GREATEST(EXTRACT(EPOCH FROM NOW() - moved.created_at), 0)::float8 \
                    AS claim_queued_secs, \
                moved.*{columns} \
         FROM room LEFT JOIN moved ON TRUE {joins}",
        from = unit.scan_from(),
        queued = crate::dispatcher::QUEUED_PREDICATE,
        handled = unit.runs_here(&local),
        open = unit.runnable(),
        lock = unit.scan_lock(),
        table = unit.table(),
        pending = SubmissionStatus::Pending,
        claim_sets = unit.claim_sets(),
        mirror = unit.cascade(),
        epoch = crate::services::plugin_config_cache::EPOCH_SEQUENCE,
    ))
}

/// Runs a claim statement of `unit`, parsing each claimed row with `parse`.
/// A row that fails to parse hands every row of the claim back to `Queued`,
/// spending no retry, before the error is returned: the claim committed, and
/// nothing would dispatch them.
async fn run_claim<T>(
    db: &DatabaseConnection,
    unit: Unit,
    server_id: &str,
    limit: ClaimLimit,
    local: &Loaded,
    parse: impl Fn(&sea_orm::QueryResult) -> Result<T, DbErr>,
) -> Result<Claimed<T>, DbErr> {
    let none = Claimed {
        units: Vec::new(),
        plugin_config_epoch: None,
        cap_room: None,
    };
    if limit.rows == 0 {
        return Ok(none);
    }
    let rows = db
        .query_all_raw(claim_statement(unit, server_id, limit, local))
        .await?;
    let ids: Vec<i32> = rows
        .iter()
        .filter_map(|row| row.try_get::<Option<i32>>("", "id").ok().flatten())
        .collect();
    let parsed = (|| -> Result<Claimed<T>, DbErr> {
        let Some(first) = rows.first() else {
            return Ok(none);
        };
        let cap_room = first
            .try_get::<Option<i64>>("", "claim_room")?
            .map(|n| u32::try_from(n).unwrap_or(u32::MAX));
        if ids.is_empty() {
            return Ok(Claimed { cap_room, ..none });
        }
        let units = rows
            .iter()
            .map(|row| {
                Ok(Claim {
                    unit: parse(row)?,
                    problem_type: row
                        .try_get::<Option<String>>("", "claim_problem_type")?
                        .unwrap_or_else(|| "unknown".into()),
                    checker_format: row
                        .try_get::<Option<String>>("", "claim_checker_format")?
                        .unwrap_or_default(),
                    checker_language: row.try_get("", "claim_checker_language")?,
                    queued_secs: row.try_get("", "claim_queued_secs")?,
                })
            })
            .collect::<Result<_, DbErr>>()?;
        Ok(Claimed {
            units,
            plugin_config_epoch: Some(first.try_get("", "claim_epoch")?),
            cap_room,
        })
    })();
    if parsed.is_err()
        && let Err(e) = release_claimed(db, unit, server_id, &ids).await
    {
        tracing::error!(error = %e, ?unit, ?ids, "Handing back a claim that failed to parse failed");
    }
    parsed
}

/// Claims queued judgements for `server_id` within `limit`: `Queued` →
/// `Pending`, owned, with a fresh lease. `local` is what this replica has
/// loaded: it takes only work it can run (see [`crate::providers::runs_here`]).
pub(crate) async fn claim_judgements(
    db: &DatabaseConnection,
    server_id: &str,
    limit: ClaimLimit,
    local: &Loaded,
) -> Result<Claimed<ClaimedJudgement>, DbErr> {
    use sea_orm::FromQueryResult;
    run_claim(db, Unit::Judgement, server_id, limit, local, |row| {
        Ok(ClaimedJudgement {
            submission: ClaimedSubmission::from_query_result(row, ClaimedSubmission::PREFIX)?,
            judgement: submission_judgement::Model::from_query_result(row, "")?,
        })
    })
    .await
}

/// Claims queued code runs for `server_id` within `limit`, as
/// [`claim_judgements`] does.
pub(crate) async fn claim_code_runs(
    db: &DatabaseConnection,
    server_id: &str,
    limit: ClaimLimit,
    local: &Loaded,
) -> Result<Claimed<code_run::Model>, DbErr> {
    use sea_orm::FromQueryResult;
    run_claim(db, Unit::CodeRun, server_id, limit, local, |row| {
        code_run::Model::from_query_result(row, "")
    })
    .await
}

/// Rows of each unit a lease operation touched.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnitCounts {
    pub judgements: u64,
    pub code_runs: u64,
}

/// Bumps the lease heartbeat of every in-flight unit `server_id` owns that
/// one of its dispatches still works on (`live`, at the epoch it was claimed
/// under), with the database clock, which every lease-age comparison also
/// uses. A unit it owns with no live dispatch keeps its old heartbeat, so its
/// lease lapses and another server steals it. Rows are taken in id order and
/// a busy one is skipped: it is mid-transition, and the lease TTL spans many
/// refresh intervals.
pub async fn refresh_leases(
    db: &DatabaseConnection,
    server_id: &str,
    live: &super::LiveDispatches,
) -> Result<UnitCounts, DbErr> {
    let mut counts = UnitCounts::default();
    for unit in [Unit::Judgement, Unit::CodeRun] {
        let (ids, epochs) = live.leases(unit);
        let n = db
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "UPDATE {table} SET lease_heartbeat_at = NOW() \
                     WHERE id IN (SELECT id FROM {table} \
                                  WHERE owner_server_id = $1 AND status IN {in_flight} \
                                    AND (id, judge_epoch) IN \
                                        (SELECT * FROM unnest($2::int[], $3::int[])) \
                                  ORDER BY id FOR NO KEY UPDATE SKIP LOCKED)",
                    table = unit.table(),
                    in_flight = StatusPhase::InFlight.sql_list(),
                ),
                [server_id.into(), ids.into(), epochs.into()],
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
    /// Its submission's spent system-error retries (0 for a code run).
    system_error_retries: i32,
    /// It may run now ([`Unit::runnable`]).
    open: bool,
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
        Unit::Judgement => {
            "u.id, u.submission_id, u.is_current, u.dispatch_retries, s.system_error_retries"
        }
        Unit::CodeRun => {
            "u.id, NULL::int AS submission_id, FALSE AS is_current, u.dispatch_retries, \
             0 AS system_error_retries"
        }
    };
    let rows = txn
        .query_all_raw(b.statement(format!(
            "SELECT {columns}, {open} AS open FROM {from} WHERE {filter} ORDER BY u.id {limit} {lock}",
            open = unit.runnable(),
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
                system_error_retries: r.try_get("", "system_error_retries")?,
                open: r.try_get("", "open")?,
            })
        })
        .collect()
}

/// How long work handed back for a database outage waits before a claim may
/// take it again ([`Requeue::HandedBackAfterOutage`]): long enough that an
/// outage, real or a plugin's own failure, cannot hand one unit back in a
/// hot loop, short enough that recovered work runs at once.
pub(crate) const OUTAGE_HANDBACK_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

/// Why a unit goes back to `Queued`: the one place a retry budget is
/// charged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Requeue {
    /// Nothing about the work failed: its owner gave it up (shutdown, or boot
    /// after a crash), or the platform lost the attempt (a system error of
    /// [`SystemErrorClass::HandBack`]). No retry is spent.
    HandedBack,
    /// The platform lost the attempt to a database outage (the host
    /// database lost, or no plugin SQL connection free): handed back like
    /// [`Self::HandedBack`], but queued [`OUTAGE_HANDBACK_DELAY`] ahead, so
    /// an outage that persists never hands the same work back and forth.
    HandedBackAfterOutage,
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
            Requeue::HandedBack | Requeue::HandedBackAfterOutage => "t.dispatch_retries",
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
    let queued_at = match why {
        Requeue::HandedBackAfterOutage => format!(
            "NOW() + make_interval(secs => {})",
            OUTAGE_HANDBACK_DELAY.as_secs_f64()
        ),
        _ => "NOW()".to_string(),
    };
    let sets = format!(
        "status = '{queued}', queued_at = {queued_at}, owner_server_id = NULL, \
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
        let n = release(&txn, unit, server_id, None).await?;
        match unit {
            Unit::Judgement => counts.judgements = n,
            Unit::CodeRun => counts.code_runs = n,
        }
    }
    txn.commit().await?;
    Ok(counts)
}

/// Requeues the in-flight units of `ids` that `server_id` owns, spending no
/// retry: a claim whose dispatch never started.
pub(crate) async fn release_claimed(
    db: &DatabaseConnection,
    unit: Unit,
    server_id: &str,
    ids: &[i32],
) -> Result<u64, DbErr> {
    if ids.is_empty() {
        return Ok(0);
    }
    let txn = db.begin().await?;
    let n = release(&txn, unit, server_id, Some(ids)).await?;
    txn.commit().await?;
    Ok(n)
}

/// Requeues the in-flight units of `unit` that `server_id` owns (only those
/// of `ids` when given) without spending a retry.
async fn release(
    txn: &DatabaseTransaction,
    unit: Unit,
    server_id: &str,
    ids: Option<&[i32]>,
) -> Result<u64, DbErr> {
    let mut b = Binds::default();
    let mut filter = format!(
        "u.owner_server_id = {} AND u.status IN {}",
        b.bind(server_id),
        StatusPhase::InFlight.sql_list()
    );
    if let Some(ids) = ids {
        filter.push_str(&format!(" AND u.id = ANY({})", b.bind(ids.to_vec())));
    }
    let ids: Vec<i32> = pick(txn, unit, b, filter, None)
        .await?
        .into_iter()
        .map(|p| p.id)
        .collect();
    requeue(txn, unit, &ids, Requeue::HandedBack).await
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
                HostCode::DispatchRetryExhausted.as_str(),
                DlqErrorCode::DispatchRetryExhausted,
                format!(
                    "Dispatch retry limit exhausted after {} retries",
                    budget.max()
                ),
            ),
            Abandoned::Stuck { .. } => (
                HostCode::StuckJob.as_str(),
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

impl std::ops::AddAssign for Reclaimed {
    fn add_assign(&mut self, other: Self) {
        self.requeued += other.requeued;
        self.exhausted += other.exhausted;
    }
}

/// Takes over every in-flight unit of `unit` that `abandoned` selects and
/// this replica can judge, `batch` rows per transaction: each with dispatch
/// retries left is requeued (spending one), each without is finalized as
/// `SystemError` and dead-lettered. Batches repeat until one comes back
/// short, so a dead server's whole in-flight set is recovered in one scan
/// however large it is. Every batch takes its rows out of flight, so the
/// loop ends.
pub(crate) async fn reclaim_abandoned(
    db: &DatabaseConnection,
    unit: Unit,
    abandoned: Abandoned,
    budget: RetryBudget,
    batch: u32,
    local: &Loaded,
) -> Result<Reclaimed, DbErr> {
    let batch = batch.max(1);
    let mut total = Reclaimed::default();
    loop {
        let (taken, reclaimed) = reclaim_batch(db, unit, abandoned, budget, batch, local).await?;
        total += reclaimed;
        if taken < batch as usize {
            return Ok(total);
        }
    }
}

/// One batch of [`reclaim_abandoned`]: returns how many rows it took.
async fn reclaim_batch(
    db: &DatabaseConnection,
    unit: Unit,
    abandoned: Abandoned,
    budget: RetryBudget,
    limit: u32,
    local: &Loaded,
) -> Result<(usize, Reclaimed), DbErr> {
    let txn = db.begin().await?;
    let mut b = Binds::default();
    let predicate = abandoned.predicate(&mut b);
    let local = b.bind(serde_json::to_value(local).expect("loaded keys serialize"));
    let filter = format!(
        "u.status IN {in_flight} AND {predicate} AND {handled}",
        in_flight = StatusPhase::InFlight.sql_list(),
        handled = unit.runs_here(&local),
    );
    let picked = pick(&txn, unit, b, filter, Some(limit)).await?;
    let taken = picked.len();
    // Work of a parked contest type lost its attempt to the disable: it goes
    // back to wait for its provider without spending a retry.
    let (parked, picked): (Vec<Picked>, Vec<Picked>) = picked.into_iter().partition(|p| !p.open);
    let parked_ids: Vec<i32> = parked.iter().map(|p| p.id).collect();
    let handed_back = requeue(&txn, unit, &parked_ids, Requeue::HandedBack).await?;
    let (retry, exhaust): (Vec<Picked>, Vec<Picked>) = picked
        .into_iter()
        .partition(|p| budget.allows(p.dispatch_retries));

    let retry_ids: Vec<i32> = retry.iter().map(|p| p.id).collect();
    let requeued = handed_back + requeue(&txn, unit, &retry_ids, Requeue::Abandoned).await?;
    let (error_code, dlq_code, message) = abandoned.exhausted(budget);
    let exhausted = finalize_system_error(&txn, unit, &exhaust, error_code, &message).await?;
    for p in &exhaust {
        dead_letter(&txn, unit, p, dlq_code, &message).await?;
    }
    txn.commit().await?;
    Ok((
        taken,
        Reclaimed {
            requeued,
            exhausted,
        },
    ))
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
                "WITH moved AS ({finalize} WHERE t.id = ANY($1) RETURNING t.*){mirror} \
                 SELECT id FROM moved",
                finalize = unit.finalize_system_error("$2", "$3", SystemErrorClass::Judging),
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

/// A submission whose system error now stands, as the reaper confirmed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConfirmedSystemError {
    pub submission_id: i32,
    pub user_id: i32,
    pub problem_id: i32,
    pub contest_id: Option<i32>,
}

/// What one system-error retry pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SystemErrorPass {
    /// Judgements the platform lost, or of a parked contest type, requeued
    /// with no retry spent.
    pub handed_back: u64,
    /// Judgements requeued for another attempt, each spending one of its
    /// submission's system-error retries.
    pub requeued: u64,
    /// Submissions whose system error stands: their budget is spent.
    pub confirmed: Vec<ConfirmedSystemError>,
}

/// What the system-error retry does with a judgement awaiting its decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RetryDecision {
    /// Its contest type is parked: a retry would only spend budget on an
    /// attempt nothing can judge, and confirming the error would make the
    /// disable's outage a verdict. It goes back to the queue spending
    /// nothing, parked with its submission, and is judged again once a
    /// provider is enabled.
    HandBack,
    /// Requeued for another attempt, spending one of its submission's
    /// system-error retries.
    Retry,
    /// Its budget is spent: the error stands, and the submission shows it.
    Confirm,
}

impl RetryDecision {
    fn of(picked: &Picked, budget: RetryBudget) -> Self {
        if !picked.open {
            Self::HandBack
        } else if budget.allows(picked.system_error_retries) {
            Self::Retry
        } else {
            Self::Confirm
        }
    }
}

/// Decides up to `limit` current judgements that finalized as a system error
/// in any shape (a plugin's `SystemError` verdict, or a `SystemError` status
/// from a dispatch failure or exhaustion) that does not stand yet, which the
/// submission row has not shown (see [`awaits_retry_decision`]), each by
/// [`RetryDecision::of`]. One pick takes every judgement awaiting a decision
/// that this replica may decide: one it can judge, or one to confirm, which
/// any replica may, whatever it can judge.
pub(crate) async fn retry_system_errors(
    db: &DatabaseConnection,
    budget: RetryBudget,
    limit: u32,
    local: &Loaded,
) -> Result<SystemErrorPass, DbErr> {
    let txn = db.begin().await?;
    let mut b = Binds::default();
    let max = b.bind(budget.max());
    let local = b.bind(serde_json::to_value(local).expect("loaded keys serialize"));
    let filter = format!(
        "u.is_current AND {undecided} \
         AND ({handled} OR ({open} AND s.system_error_retries >= {max}))",
        undecided = awaits_retry_decision("u", "s"),
        handled = Unit::Judgement.runs_here(&local),
        open = Unit::Judgement.runnable(),
    );
    let picked = pick(&txn, Unit::Judgement, b, filter, Some(limit)).await?;
    let decided = |decision: RetryDecision| -> Vec<i32> {
        picked
            .iter()
            .filter(|p| RetryDecision::of(p, budget) == decision)
            .map(|p| p.id)
            .collect()
    };
    let handed_back = requeue(
        &txn,
        Unit::Judgement,
        &decided(RetryDecision::HandBack),
        Requeue::HandedBack,
    )
    .await?;
    let requeued = requeue(
        &txn,
        Unit::Judgement,
        &decided(RetryDecision::Retry),
        Requeue::SystemErrorRetry,
    )
    .await?;
    let confirmed = confirm_system_errors(&txn, &decided(RetryDecision::Confirm)).await?;
    txn.commit().await?;
    Ok(SystemErrorPass {
        handed_back,
        requeued,
        confirmed,
    })
}

/// Marks the locked current judgements `ids` as standing and shows each on
/// its submission.
async fn confirm_system_errors(
    txn: &DatabaseTransaction,
    ids: &[i32],
) -> Result<Vec<ConfirmedSystemError>, DbErr> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = txn
        .query_all_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "WITH moved AS (UPDATE submission_judgement t SET error_stands = TRUE \
                                WHERE t.id = ANY($1) RETURNING t.*){mirror} \
                 SELECT s.id, s.user_id, s.problem_id, s.contest_id \
                 FROM submission s JOIN moved ON moved.submission_id = s.id",
                mirror = Unit::Judgement.cascade(),
            ),
            [ids.to_vec().into()],
        ))
        .await?;
    rows.iter()
        .map(|r| {
            Ok(ConfirmedSystemError {
                submission_id: r.try_get("", "id")?,
                user_id: r.try_get("", "user_id")?,
                problem_id: r.try_get("", "problem_id")?,
                contest_id: r.try_get("", "contest_id")?,
            })
        })
        .collect()
}

/// What [`fail_dispatch`] did with the unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DispatchFailed {
    /// Finalized as `SystemError`: a judgement's error stands or awaits the
    /// system-error retry, as its class says; a code run's stands.
    SystemError,
    /// Back to `Queued` without spending a retry: the platform lost the
    /// attempt, or a plugin the unit needs (its contest type's, its
    /// evaluator, its checker when the evaluator uses one, its language's)
    /// has no provider enabled fleet-wide (`Unit::runnable`), so the failure
    /// is the disable's (a judging session the unload cut), on whichever
    /// replica it ran, and the work waits parked for a provider.
    HandedBack,
    /// Nothing: the dispatch was since requeued, superseded or finished.
    Stale,
}

/// Ends a unit (a judgement or a code run alike) whose dispatch failed,
/// fenced on its epoch: a failure reported by a dispatch that was since
/// requeued or superseded changes nothing. The unit goes straight back to
/// `Queued`, so it never shows a `SystemError` it does not have, when the
/// platform lost the attempt ([`SystemErrorClass::HandBack`]) or its contest
/// type is parked; anything else is finalized as `SystemError`, a
/// judgement's standing at once when its class is
/// [`SystemErrorClass::Stands`].
///
/// Every judging session ends here, most after their plugin finalized the
/// unit, so the common case is one statement: it finalizes a unit whose type
/// is open, or finds nothing to do. Only a unit to hand back takes a second,
/// transactional pass.
pub(crate) async fn fail_dispatch(
    db: &DatabaseConnection,
    unit: Unit,
    id: i32,
    judge_epoch: i32,
    failure: &DispatchFailure,
) -> Result<DispatchFailed, DbErr> {
    let outage = matches!(
        failure.code,
        HostCode::DatabaseUnavailable | HostCode::DatabaseBusy
    );
    if failure.class == SystemErrorClass::HandBack {
        return hand_back(db, unit, id, judge_epoch, None, outage).await;
    }
    let past_cap;
    let failure = if failure.class == SystemErrorClass::Contended {
        if counted_contention(db, unit, id, judge_epoch).await? {
            return hand_back(db, unit, id, judge_epoch, None, outage).await;
        }
        // The unit used its contention hand-backs: the failure is the
        // submission's to spend a retry on.
        past_cap = DispatchFailure {
            class: SystemErrorClass::Judging,
            ..failure.clone()
        };
        &past_cap
    } else {
        failure
    };
    let open_now = unit.open_dispatch("t", "$1", "$2");
    let sql = match unit {
        // A judgement locks its submission first, as every judgement
        // transition does; the UPDATE joins the lock so it is taken before
        // the row's.
        Unit::Judgement => format!(
            "WITH locked AS (SELECT s.id, {open} AS open FROM submission s \
                             JOIN submission_judgement u ON u.submission_id = s.id \
                             WHERE u.id = $1 FOR NO KEY UPDATE OF s), \
                  moved AS ({finalize} FROM locked \
                            WHERE {open_now} AND t.submission_id = locked.id AND locked.open \
                            RETURNING t.*){mirror}, \
                  parked AS (SELECT t.id FROM submission_judgement t, locked \
                             WHERE {open_now} AND NOT locked.open) \
             SELECT (SELECT COUNT(*) FROM moved) AS moved, (SELECT COUNT(*) FROM parked) AS parked",
            open = unit.runnable(),
            finalize = unit.finalize_system_error("$3", "$4", failure.class),
            mirror = unit.cascade(),
        ),
        Unit::CodeRun => format!(
            "WITH locked AS (SELECT u.id, {open} AS open FROM code_run u WHERE u.id = $1), \
                  moved AS ({finalize} FROM locked \
                            WHERE {open_now} AND t.id = locked.id AND locked.open RETURNING t.id), \
                  parked AS (SELECT t.id FROM code_run t, locked \
                             WHERE {open_now} AND NOT locked.open) \
             SELECT (SELECT COUNT(*) FROM moved) AS moved, (SELECT COUNT(*) FROM parked) AS parked",
            open = unit.runnable(),
            finalize = unit.finalize_system_error("$3", "$4", failure.class),
        ),
    };
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            [
                id.into(),
                judge_epoch.into(),
                failure.code.as_str().into(),
                failure.message.as_str().into(),
            ],
        ))
        .await?
        .ok_or_else(|| DbErr::Custom("fail_dispatch returned no row".into()))?;
    if row.try_get::<i64>("", "moved")? > 0 {
        return Ok(DispatchFailed::SystemError);
    }
    if row.try_get::<i64>("", "parked")? == 0 {
        return Ok(DispatchFailed::Stale);
    }
    hand_back(db, unit, id, judge_epoch, Some(failure), outage).await
}

/// The second pass of [`fail_dispatch`]: under the unit's locks, hands back
/// the in-flight unit. With `unless_open`, the failure is the disable's only
/// while the unit's type is still parked: if a provider was enabled since, it
/// is finalized with that failure instead.
/// Counts one contention hand-back on unit `id` at `judge_epoch`, if it has
/// any left (`super::fault::CONTENTION_HANDBACKS`): whether it was counted,
/// so the attempt is handed back.
async fn counted_contention(
    db: &DatabaseConnection,
    unit: Unit,
    id: i32,
    judge_epoch: i32,
) -> Result<bool, DbErr> {
    Ok(db
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "UPDATE {table} SET contended_handbacks = contended_handbacks + 1 \
                  WHERE id = $1 AND judge_epoch = $2 AND contended_handbacks < $3",
                table = unit.table(),
            ),
            [
                id.into(),
                judge_epoch.into(),
                super::fault::CONTENTION_HANDBACKS.into(),
            ],
        ))
        .await?
        .rows_affected()
        == 1)
}

async fn hand_back(
    db: &DatabaseConnection,
    unit: Unit,
    id: i32,
    judge_epoch: i32,
    unless_open: Option<&DispatchFailure>,
    outage: bool,
) -> Result<DispatchFailed, DbErr> {
    let txn = db.begin().await?;
    let row = txn
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "SELECT {open} AS open FROM {from} WHERE {open_now} {lock}",
                open = unit.runnable(),
                from = unit.scan_from(),
                open_now = unit.open_dispatch("u", "$1", "$2"),
                lock = match unit {
                    Unit::Judgement => "FOR NO KEY UPDATE OF s",
                    Unit::CodeRun => "FOR NO KEY UPDATE OF u",
                },
            ),
            [id.into(), judge_epoch.into()],
        ))
        .await?;
    let outcome = match (row, unless_open) {
        (None, _) => DispatchFailed::Stale,
        (Some(row), Some(failure)) if row.try_get::<bool>("", "open")? => {
            txn.execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "WITH moved AS ({finalize} WHERE t.id = $1 RETURNING t.*){mirror} \
                     SELECT id FROM moved",
                    finalize = unit.finalize_system_error("$2", "$3", failure.class),
                    mirror = unit.cascade(),
                ),
                [
                    id.into(),
                    failure.code.as_str().into(),
                    failure.message.as_str().into(),
                ],
            ))
            .await?;
            DispatchFailed::SystemError
        }
        (Some(_), _) => {
            let why = if outage {
                Requeue::HandedBackAfterOutage
            } else {
                Requeue::HandedBack
            };
            requeue(&txn, unit, &[id], why).await?;
            DispatchFailed::HandedBack
        }
    };
    txn.commit().await?;
    Ok(outcome)
}
