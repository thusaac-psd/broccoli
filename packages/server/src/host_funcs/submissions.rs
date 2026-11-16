//! Structured, capability-gated host functions through which a plugin
//! writes the judgement or code run it was dispatched with
//! (`host.submission.*` and `host.code_run.*`). The guest sends structured
//! input; the host builds every statement (see `services::judging`), fenced on
//! the unit and its epoch. The statements run on the host function pool, through
//! the same argument parsing and statement typing as raw plugin SQL
//! (`sql::execute_on_pool`).

use broccoli_server_sdk::types::{
    CodeRunResultRow, CodeRunUpdate, SubmissionUpdate, TestCaseResultRow,
    sanitize_result_text_field,
};
use std::sync::Arc;

use common::db::DatabaseConnection;
use extism::host_fn;
use serde_json::{Value as JsonValue, json};

use crate::services::judging;

/// Positional parameters of the statements built here: assigns `$1..$N`
/// placeholders (1-based) and collects the bound JSON values.
struct Params {
    args: Vec<JsonValue>,
    idx: usize,
}

impl Params {
    fn new() -> Self {
        Self {
            args: Vec::new(),
            idx: 1,
        }
    }

    fn bind(&mut self, value: impl Into<JsonValue>) -> String {
        let placeholder = format!("${}", self.idx);
        self.args.push(value.into());
        self.idx += 1;
        placeholder
    }

    fn bind_optional_int(&mut self, value: Option<i32>) -> String {
        // JSON null binds as text (sql::parse_args), so the cast types the
        // parameter for an integer column either way.
        format!("{}::int", self.bind(json!(value)))
    }

    fn into_args(self) -> Vec<JsonValue> {
        self.args
    }
}

impl judging::SqlBindSink for Params {
    fn bind_value(&mut self, value: JsonValue) -> String {
        self.bind(value)
    }
}

fn ok_affected(n: u64) -> Result<String, extism::Error> {
    Ok(json!({ "data": n }).to_string())
}

/// The statement behind `submission_update`, with its bound arguments.
/// `None` when the update sets nothing.
fn submission_update_statement(update: &SubmissionUpdate) -> Option<(String, Vec<JsonValue>)> {
    let mut p = Params::new();
    let sql = judging::judgement_write(&mut p, update)?;
    Some((sql, p.into_args()))
}

host_fn!(pub submission_update(user_data: Arc<(String, DatabaseConnection)>; input: String) -> String {
    let call = super::HostCall::enter(&user_data, "submission_update")?;
    let (plugin_id, db) = &*call;

    let update: SubmissionUpdate = serde_json::from_str(&input)
        .map_err(|e| extism::Error::msg(format!("Invalid submission_update input: {e}")))?;

    // An update that sets nothing is not a fenced-out write.
    let Some((sql, args)) = submission_update_statement(&update) else {
        return ok_affected(1);
    };
    // The statement's row count is the number of judgements written, so the
    // envelope is returned as is: 0 tells the guest its epoch is stale.
    let args = serde_json::to_string(&args)?;
    super::sql::execute_on_pool(db, plugin_id, sql, args)
});

/// Postgres accepts at most this many bound parameters in one statement.
const BIND_PARAMETER_LIMIT: usize = 65_535;

/// The judged columns every result row carries, after the columns that name
/// its unit.
const JUDGED_RESULT_COLUMNS: [&str; 8] = [
    "verdict",
    "score",
    "time_used",
    "memory_used",
    "checker_output",
    "stdout",
    "stderr",
    "created_at",
];

/// The judged part of one result row.
struct JudgedResult<'a> {
    verdict: String,
    score: f64,
    time_used: Option<i32>,
    memory_used: Option<i32>,
    message: Option<&'a str>,
    stdout: Option<&'a str>,
    stderr: Option<&'a str>,
}

/// The fenced `INSERT ... SELECT` both result host functions run: each row
/// is `key_columns` (bound by `bind_key`) then [`JUDGED_RESULT_COLUMNS`], and
/// lands only through `join`, which names the unit row the fence locks.
/// `Ok(None)` for an empty batch; `Err` when the batch would exceed the bind
/// parameter limit, so the plugin can send it in smaller batches.
fn results_statement<R>(
    results: &[R],
    table: &str,
    key_columns: &[&str],
    bind_key: impl Fn(&mut Params, &R) -> Vec<String>,
    judged: impl Fn(&R) -> JudgedResult<'_>,
    join: &str,
) -> Result<Option<(String, Vec<JsonValue>)>, String> {
    if results.is_empty() {
        return Ok(None);
    }
    // `created_at` is `NOW()`, not bound.
    let per_row = key_columns.len() + JUDGED_RESULT_COLUMNS.len() - 1;
    let max_rows = BIND_PARAMETER_LIMIT / per_row;
    if results.len() > max_rows {
        return Err(format!(
            "{} result rows in one call exceed the limit of {max_rows}; send them in smaller batches",
            results.len()
        ));
    }
    let mut p = Params::new();
    let mut rows = Vec::with_capacity(results.len());
    for r in results {
        let mut values = bind_key(&mut p, r);
        let j = judged(r);
        let score = if j.score.is_finite() { j.score } else { 0.0 };
        values.extend([
            p.bind(j.verdict),
            p.bind(score),
            p.bind_optional_int(j.time_used),
            p.bind_optional_int(j.memory_used),
            format!(
                "{}::text",
                p.bind(json!(j.message.map(sanitize_result_text_field)))
            ),
            format!(
                "{}::text",
                p.bind(json!(j.stdout.map(sanitize_result_text_field)))
            ),
            format!(
                "{}::text",
                p.bind(json!(j.stderr.map(sanitize_result_text_field)))
            ),
            "NOW()".to_string(),
        ]);
        rows.push(format!("({})", values.join(", ")));
    }
    let all_columns = || key_columns.iter().copied().chain(JUDGED_RESULT_COLUMNS);
    let columns = all_columns().collect::<Vec<_>>().join(", ");
    let selected = all_columns()
        .map(|c| format!("v.{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT INTO {table} ({columns}) SELECT {selected} \
         FROM (VALUES {rows}) AS v({columns}) JOIN {join}",
        rows = rows.join(", "),
    );
    Ok(Some((sql, p.into_args())))
}

/// Runs a result insert built by [`results_statement`], or reports its error
/// to the guest in the standard envelope.
fn execute_results(
    db: &DatabaseConnection,
    plugin_id: &str,
    statement: Result<Option<(String, Vec<JsonValue>)>, String>,
) -> Result<String, extism::Error> {
    match statement {
        Ok(None) => ok_affected(0),
        Ok(Some((sql, args))) => {
            let args = serde_json::to_string(&args)?;
            super::sql::execute_on_pool(db, plugin_id, sql, args)
        }
        Err(e) => super::sql::HostDbResponse::err(e).to_json_string(),
    }
}

/// `submission_insert_results`' statement: the rows land only while their
/// judgement is unfinalized at their epoch, behind a `FOR SHARE` lock (see
/// [`judging::write_fence`]).
fn submission_results_statement(
    results: &[TestCaseResultRow],
) -> Result<Option<(String, Vec<JsonValue>)>, String> {
    results_statement(
        results,
        "test_case_result",
        &[
            "submission_id",
            "judgement_id",
            "judge_epoch",
            "test_case_id",
            "run_index",
        ],
        |p, r| {
            vec![
                p.bind(r.submission_id),
                format!("{}::int", p.bind(r.judgement_id)),
                p.bind(r.judge_epoch),
                p.bind_optional_int(r.test_case_id),
                p.bind_optional_int(r.run_index),
            ]
        },
        |r| JudgedResult {
            verdict: r.verdict.to_db_str().to_string(),
            score: r.score,
            time_used: r.time_used,
            memory_used: r.memory_used,
            message: r.message.as_deref(),
            stdout: r.stdout.as_deref(),
            stderr: r.stderr.as_deref(),
        },
        &format!(
            "submission_judgement j ON {} FOR SHARE OF j",
            judging::write_fence("j", "v.judgement_id", "v.submission_id", "v.judge_epoch")
        ),
    )
}

host_fn!(pub submission_insert_results(user_data: Arc<(String, DatabaseConnection)>; input: String) -> String {
    let call = super::HostCall::enter(&user_data, "submission_insert_results")?;
    let (plugin_id, db) = &*call;

    let results: Vec<TestCaseResultRow> = serde_json::from_str(&input)
        .map_err(|e| extism::Error::msg(format!("Invalid submission_insert_results input: {e}")))?;
    execute_results(db, plugin_id, submission_results_statement(&results))
});

/// `submission_delete_results`' statement. Same fence as the writes: a
/// superseded or finalized judgement's late worker cannot wipe the results
/// its judgement is kept with as version history.
fn delete_results_statement(
    submission_id: i32,
    judgement_id: i32,
    judge_epoch: i32,
) -> (String, Vec<JsonValue>) {
    let mut p = Params::new();
    let sql = format!(
        "DELETE FROM test_case_result t \
         WHERE t.submission_id = {} AND t.judgement_id = {} AND t.judge_epoch = {} \
           AND EXISTS (SELECT 1 FROM submission_judgement j WHERE {} FOR SHARE)",
        p.bind(submission_id),
        p.bind(judgement_id),
        p.bind(judge_epoch),
        judging::write_fence("j", "t.judgement_id", "t.submission_id", "t.judge_epoch"),
    );
    (sql, p.into_args())
}

host_fn!(pub submission_delete_results(user_data: Arc<(String, DatabaseConnection)>; input: String) -> String {
    let call = super::HostCall::enter(&user_data, "submission_delete_results")?;
    let (plugin_id, db) = &*call;

    let (submission_id, judgement_id, judge_epoch): (i32, i32, i32) = serde_json::from_str(&input)
        .map_err(|e| extism::Error::msg(format!("Invalid submission_delete_results input: {e}")))?;

    let (sql, args) = delete_results_statement(submission_id, judgement_id, judge_epoch);
    let args = serde_json::to_string(&args)?;
    super::sql::execute_on_pool(db, plugin_id, sql, args)
});

host_fn!(pub submission_query_test_cases(user_data: Arc<(String, DatabaseConnection)>; input: String) -> String {
    let call = super::HostCall::enter(&user_data, "submission_query_test_cases")?;
    let (plugin_id, db) = &*call;

    let problem_id: i32 = serde_json::from_str(&input)
        .map_err(|e| extism::Error::msg(format!("Invalid submission_query_test_cases input: {e}")))?;

    let mut p = Params::new();
    let sql = format!(
        "SELECT id, score, is_sample, position, description, label \
         FROM test_case WHERE problem_id = {} ORDER BY position ASC",
        p.bind(problem_id)
    );
    let args = serde_json::to_string(&p.into_args())?;
    super::sql::query_on_pool(db, plugin_id, sql, args)
});

/// The write fence against real Postgres, driven through the same builders and
/// in the same order as `submission_update`.
/// The statement behind `code_run_update`, with its bound arguments.
/// `None` when the update sets nothing.
fn code_run_update_statement(update: &CodeRunUpdate) -> Option<(String, Vec<JsonValue>)> {
    let mut p = Params::new();
    let sql = judging::code_run_write(&mut p, update)?;
    Some((sql, p.into_args()))
}

host_fn!(pub code_run_update(user_data: Arc<(String, DatabaseConnection)>; input: String) -> String {
    let call = super::HostCall::enter(&user_data, "code_run_update")?;
    let (plugin_id, db) = &*call;

    let update: CodeRunUpdate = serde_json::from_str(&input)
        .map_err(|e| extism::Error::msg(format!("Invalid code_run_update input: {e}")))?;
    // An update that sets nothing is not a fenced-out write.
    let Some((sql, args)) = code_run_update_statement(&update) else {
        return ok_affected(1);
    };
    let args = serde_json::to_string(&args)?;
    super::sql::execute_on_pool(db, plugin_id, sql, args)
});

/// `code_run_insert_results`' statement: the rows land only while their run
/// is in flight at their epoch, behind a `FOR SHARE` lock.
fn code_run_results_statement(
    results: &[CodeRunResultRow],
) -> Result<Option<(String, Vec<JsonValue>)>, String> {
    results_statement(
        results,
        "code_run_result",
        &["code_run_id", "judge_epoch", "run_index"],
        |p, r| {
            vec![
                p.bind(r.code_run_id),
                p.bind(r.judge_epoch),
                p.bind(r.run_index),
            ]
        },
        |r| JudgedResult {
            verdict: r.verdict.to_db_str().to_string(),
            score: r.score,
            time_used: r.time_used,
            memory_used: r.memory_used,
            message: r.message.as_deref(),
            stdout: r.stdout.as_deref(),
            stderr: r.stderr.as_deref(),
        },
        &format!(
            "code_run cr ON {} FOR SHARE OF cr",
            judging::code_run_write_fence("cr", "v.code_run_id", "v.judge_epoch")
        ),
    )
}

host_fn!(pub code_run_insert_results(user_data: Arc<(String, DatabaseConnection)>; input: String) -> String {
    let call = super::HostCall::enter(&user_data, "code_run_insert_results")?;
    let (plugin_id, db) = &*call;

    let results: Vec<CodeRunResultRow> = serde_json::from_str(&input)
        .map_err(|e| extism::Error::msg(format!("Invalid code_run_insert_results input: {e}")))?;
    execute_results(db, plugin_id, code_run_results_statement(&results))
});

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Fixtures are stamped with real time.
mod tests {
    use super::*;
    use crate::entity::{submission, submission_judgement, test_case_result};
    use crate::services::judging::{OpenJudgement, open_judgement};
    use broccoli_server_sdk::types::{SubmissionStatus, Verdict};
    use chrono::Utc;
    use sea_orm::{
        ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter,
        Set, TransactionTrait,
    };

    /// Deserialized view of the `HostDbResponse` envelope `sql::execute_on_pool`
    /// returns, for tests that read a statement's row count.
    #[derive(serde::Deserialize)]
    struct ExecEnvelope {
        #[serde(default)]
        data: Option<JsonValue>,
        #[serde(default)]
        error: Option<String>,
    }

    fn execute(db: &DatabaseConnection, sql: String, args: Vec<JsonValue>) -> u64 {
        let args = serde_json::to_string(&args).expect("args");
        let resp = super::super::sql::execute_on_pool(db, "fence-test", sql, args).expect("exec");
        let env: ExecEnvelope = serde_json::from_str(&resp).expect("envelope");
        assert!(env.error.is_none(), "db error: {:?}", env.error);
        env.data.and_then(|v| v.as_u64()).unwrap_or(0)
    }

    /// `submission_update`'s statement sequence: the fenced judgement write,
    /// then the cache row only if the judgement accepted it. Returns the
    /// guest-visible rows-affected.
    async fn submission_update(db: &DatabaseConnection, update: SubmissionUpdate) -> u64 {
        let db = db.clone();
        tokio::task::spawn_blocking(move || {
            let (sql, args) = submission_update_statement(&update).expect("sets");
            execute(&db, sql, args)
        })
        .await
        .expect("join")
    }

    async fn delete_results(db: &DatabaseConnection, judgement: &submission_judgement::Model) {
        let db = db.clone();
        let (sid, jid, epoch) = (judgement.submission_id, judgement.id, judgement.judge_epoch);
        tokio::task::spawn_blocking(move || {
            let (sql, args) = delete_results_statement(sid, jid, epoch);
            execute(&db, sql, args);
        })
        .await
        .expect("join");
    }

    fn finish(j: &submission_judgement::Model, verdict: Verdict, score: f64) -> SubmissionUpdate {
        SubmissionUpdate {
            submission_id: j.submission_id,
            judgement_id: j.id,
            judge_epoch: j.judge_epoch,
            status: Some(SubmissionStatus::Judged),
            verdict: Some(Some(verdict)),
            score: Some(score),
            ..Default::default()
        }
    }

    async fn judgement(db: &DatabaseConnection, id: i32) -> submission_judgement::Model {
        submission_judgement::Entity::find_by_id(id)
            .one(db)
            .await
            .expect("db")
            .expect("judgement")
    }

    async fn submission_row(db: &DatabaseConnection, id: i32) -> submission::Model {
        submission::Entity::find_by_id(id)
            .one(db)
            .await
            .expect("db")
            .expect("submission")
    }

    /// A judged submission with a finalized current v1 at epoch 3.
    async fn seed_judged(db: &DatabaseConnection) -> (submission::Model, i32) {
        let now = Utc::now();
        let (user_id, problem_id) = crate::test_support::seed_user_and_problem(db).await;
        let sub = submission::ActiveModel {
            files: Set(serde_json::json!({})),
            language: Set("cpp".into()),
            user_id: Set(user_id),
            problem_id: Set(problem_id),
            status: Set(common::SubmissionStatus::Judged),
            verdict: Set(Some(common::Verdict::Accepted)),
            score: Set(Some(100.0)),
            judge_epoch: Set(3),
            max_judge_epoch: Set(3),
            created_at: Set(now),
            judged_at: Set(Some(now)),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("submission");
        let v1 = submission_judgement::ActiveModel {
            submission_id: Set(sub.id),
            version: Set(1),
            is_current: Set(true),
            is_finalized: Set(true),
            status: Set(common::SubmissionStatus::Judged),
            verdict: Set(Some(common::Verdict::Accepted)),
            score: Set(Some(100.0)),
            judge_epoch: Set(3),
            created_at: Set(now),
            finalized_at: Set(Some(now)),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("v1");
        (sub, v1.id)
    }

    async fn open(
        db: &DatabaseConnection,
        sid: i32,
        open: OpenJudgement,
    ) -> submission_judgement::Model {
        let txn = db.begin().await.expect("begin");
        let (_, j) = open_judgement(&txn, sid, open)
            .await
            .expect("open")
            .expect("submission exists");
        txn.commit().await.expect("commit");
        j
    }

    /// A deferred rejudge is still running when an immediate rejudge opens a
    /// higher version. Each finalizes its OWN judgement; only the current one
    /// drives the submission's visible verdict. Under the old
    /// `is_current OR version = MAX(version)` fence the deferred judgement was
    /// neither, so it could never finalize.
    #[tokio::test(flavor = "multi_thread")]
    async fn deferred_and_immediate_rejudges_each_finalize_their_own_judgement() {
        let (_pg, db) = crate::test_support::postgres().await;
        let (sub, _v1) = seed_judged(&db).await;

        let deferred = open(
            &db,
            sub.id,
            OpenJudgement::DeferredRejudge {
                triggered_by_user_id: sub.user_id,
                target_worker_id: None,
            },
        )
        .await;
        let immediate = open(
            &db,
            sub.id,
            OpenJudgement::Rejudge {
                triggered_by_user_id: sub.user_id,
                target_worker_id: None,
            },
        )
        .await;
        assert!(immediate.version > deferred.version);
        assert_ne!(immediate.judge_epoch, deferred.judge_epoch);

        // The deferred run finishes first, while the immediate one is live.
        let rows = submission_update(&db, finish(&deferred, Verdict::WrongAnswer, 0.0)).await;
        assert_eq!(rows, 1, "the deferred judgement accepts its own result");
        let d = judgement(&db, deferred.id).await;
        assert!(d.is_finalized);
        assert!(
            !d.is_current,
            "finishing never makes a deferred judgement current"
        );
        assert_eq!(d.verdict, Some(common::Verdict::WrongAnswer));
        let s = submission_row(&db, sub.id).await;
        assert_eq!(
            s.status,
            common::SubmissionStatus::Queued,
            "visible state untouched"
        );
        assert_eq!(s.verdict, None);
        assert_eq!(s.judge_epoch, immediate.judge_epoch);

        // The immediate (current) run finishes and drives the submission.
        let rows = submission_update(&db, finish(&immediate, Verdict::Accepted, 100.0)).await;
        assert_eq!(rows, 1);
        let i = judgement(&db, immediate.id).await;
        assert!(i.is_finalized && i.is_current);
        let s = submission_row(&db, sub.id).await;
        assert_eq!(s.status, common::SubmissionStatus::Judged);
        assert_eq!(s.verdict, Some(common::Verdict::Accepted));
        assert_eq!(s.score, Some(100.0));
    }

    /// Nothing can write to a superseded or finalized judgement, or to a
    /// judgement through another submission's id, and its kept results
    /// cannot be deleted.
    #[tokio::test(flavor = "multi_thread")]
    async fn superseded_or_finalized_judgements_reject_writes() {
        let (_pg, db) = crate::test_support::postgres().await;
        let (sub, _v1) = seed_judged(&db).await;

        let first = open(
            &db,
            sub.id,
            OpenJudgement::Rejudge {
                triggered_by_user_id: sub.user_id,
                target_worker_id: None,
            },
        )
        .await;
        test_case_result::ActiveModel {
            submission_id: Set(sub.id),
            judgement_id: Set(first.id),
            judge_epoch: Set(first.judge_epoch),
            verdict: Set(common::Verdict::Accepted),
            score: Set(1.0),
            created_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(&db)
        .await
        .expect("partial result");
        // A second rejudge supersedes the in-flight first one.
        let second = open(
            &db,
            sub.id,
            OpenJudgement::Rejudge {
                triggered_by_user_id: sub.user_id,
                target_worker_id: None,
            },
        )
        .await;

        let late = submission_update(&db, finish(&first, Verdict::WrongAnswer, 0.0)).await;
        assert_eq!(late, 0, "a superseded judgement's late result is rejected");
        assert_eq!(
            judgement(&db, first.id).await.error_code.as_deref(),
            Some("SUPERSEDED"),
            "it keeps the supersede outcome"
        );
        delete_results(&db, &first).await;
        let kept = test_case_result::Entity::find()
            .filter(test_case_result::Column::JudgementId.eq(first.id))
            .count(&db)
            .await
            .expect("count");
        assert_eq!(kept, 1, "a superseded judgement's results are kept");

        let mut wrong_sub = finish(&second, Verdict::WrongAnswer, 0.0);
        wrong_sub.submission_id += 1;
        assert_eq!(submission_update(&db, wrong_sub).await, 0);

        assert_eq!(
            submission_update(&db, finish(&second, Verdict::Accepted, 100.0)).await,
            1
        );
        let again = submission_update(&db, finish(&second, Verdict::WrongAnswer, 0.0)).await;
        assert_eq!(again, 0, "a finalized judgement accepts no further writes");
        let s = submission_row(&db, sub.id).await;
        assert_eq!(s.verdict, Some(common::Verdict::Accepted));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_code_run_write_lands_only_at_its_in_flight_epoch() {
        use crate::entity::{code_run, code_run_result};
        use broccoli_server_sdk::types::{CodeRunResultRow, CodeRunUpdate};

        let (_pg, db) = crate::test_support::postgres().await;
        let (user_id, problem_id) = crate::test_support::seed_user_and_problem(&db).await;
        let run = code_run::ActiveModel {
            files: Set(serde_json::json!([])),
            language: Set("cpp".into()),
            user_id: Set(user_id),
            problem_id: Set(problem_id),
            status: Set(common::SubmissionStatus::Running),
            judge_epoch: Set(2),
            custom_test_cases: Set(serde_json::json!([])),
            created_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(&db)
        .await
        .expect("code run");

        let write = |epoch: i32| {
            let db = db.clone();
            let id = run.id;
            async move {
                tokio::task::spawn_blocking(move || {
                    let result = CodeRunResultRow {
                        code_run_id: id,
                        judge_epoch: epoch,
                        run_index: 0,
                        verdict: Verdict::Accepted,
                        score: 1.0,
                        time_used: None,
                        memory_used: None,
                        message: None,
                        stdout: None,
                        stderr: None,
                    };
                    let (rsql, rargs) = code_run_results_statement(&[result])
                        .expect("fits")
                        .expect("rows");
                    let inserted = execute(&db, rsql, rargs);
                    let update = CodeRunUpdate {
                        code_run_id: id,
                        judge_epoch: epoch,
                        status: Some(SubmissionStatus::Judged),
                        verdict: Some(Some(Verdict::Accepted)),
                        ..Default::default()
                    };
                    let (sql, args) = code_run_update_statement(&update).expect("sets");
                    (inserted, execute(&db, sql, args))
                })
                .await
                .expect("join")
            }
        };

        assert_eq!(write(1).await, (0, 0), "a stale epoch writes nothing");
        assert_eq!(write(2).await, (1, 1));
        assert_eq!(write(2).await, (0, 0), "a finished run accepts no writes");
        let after = code_run::Entity::find_by_id(run.id)
            .one(&db)
            .await
            .expect("db")
            .expect("run");
        assert_eq!(after.status, common::SubmissionStatus::Judged);
        assert!(after.judged_at.is_some());
        let results = code_run_result::Entity::find()
            .filter(code_run_result::Column::CodeRunId.eq(run.id))
            .count(&db)
            .await
            .expect("count");
        assert_eq!(results, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_standing_verdict_resolves_the_submissions_dead_letter() {
        use crate::dlq::{DlqService, DlqSubject};
        use crate::entity::dead_letter_message;

        let (_pg, db) = crate::test_support::postgres().await;
        let (sub, _) = seed_judged(&db).await;
        let dead_letter = || async {
            DlqService::new(&db)
                .dead_letter(
                    DlqSubject::Submission(sub.id),
                    Some(sub.id),
                    serde_json::json!({}),
                    common::DlqErrorCode::StuckJob,
                    "stuck",
                )
                .await
                .expect("dead letter");
            dead_letter_message::Entity::find()
                .one(&db)
                .await
                .expect("db")
                .expect("row")
        };
        // A system-error verdict leaves it open.
        dead_letter().await;
        let j = open(
            &db,
            sub.id,
            OpenJudgement::Rejudge {
                triggered_by_user_id: sub.user_id,
                target_worker_id: None,
            },
        )
        .await;
        let mut update = finish(&j, Verdict::SystemError, 0.0);
        update.verdict = Some(Some(Verdict::SystemError));
        assert_eq!(submission_update(&db, update).await, 1);
        let row = dead_letter_message::Entity::find()
            .one(&db)
            .await
            .expect("db")
            .expect("row");
        assert!(!row.resolved, "a system error does not resolve it");

        // A verdict that stands resolves it, as the system rather than an operator.
        dead_letter().await;
        let j = open(
            &db,
            sub.id,
            OpenJudgement::Rejudge {
                triggered_by_user_id: sub.user_id,
                target_worker_id: None,
            },
        )
        .await;
        assert_eq!(
            submission_update(&db, finish(&j, Verdict::WrongAnswer, 0.0)).await,
            1
        );
        let row = dead_letter_message::Entity::find()
            .one(&db)
            .await
            .expect("db")
            .expect("row");
        assert!(row.resolved);
        assert!(row.resolved_at.is_some());
        assert_eq!(row.resolved_by, None);
    }

    /// A result insert from a dispatch whose judgement is being requeued waits
    /// for the requeue and then re-checks its fence against the committed
    /// row, so the stale rows never land.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_result_insert_racing_a_requeue_writes_nothing() {
        let (_pg, db) = crate::test_support::postgres().await;
        let (sub, _) = seed_judged(&db).await;
        let j = open(
            &db,
            sub.id,
            OpenJudgement::Rejudge {
                triggered_by_user_id: sub.user_id,
                target_worker_id: None,
            },
        )
        .await;

        let requeue = db.begin().await.expect("begin");
        requeue
            .execute_unprepared(&format!(
                "UPDATE submission_judgement SET judge_epoch = judge_epoch + 1 WHERE id = {}",
                j.id
            ))
            .await
            .expect("requeue");
        let insert = {
            let db = db.clone();
            let row = TestCaseResultRow {
                submission_id: j.submission_id,
                judgement_id: j.id,
                judge_epoch: j.judge_epoch,
                test_case_id: None,
                run_index: Some(0),
                verdict: Verdict::Accepted,
                score: 1.0,
                time_used: None,
                memory_used: None,
                message: None,
                stdout: None,
                stderr: None,
            };
            tokio::task::spawn_blocking(move || {
                let (sql, args) = submission_results_statement(&[row])
                    .expect("fits")
                    .expect("rows");
                execute(&db, sql, args)
            })
        };
        crate::test_support::wait_until_blocked_on_a_lock(&db).await;
        requeue.commit().await.expect("commit");

        assert_eq!(
            insert.await.expect("join"),
            0,
            "the stale insert writes nothing"
        );
        let rows = test_case_result::Entity::find()
            .filter(test_case_result::Column::JudgementId.eq(j.id))
            .count(&db)
            .await
            .expect("count");
        assert_eq!(rows, 0);
    }

    #[test]
    fn a_result_batch_past_the_bind_limit_is_refused_whole() {
        let row = CodeRunResultRow {
            code_run_id: 1,
            judge_epoch: 1,
            run_index: 0,
            verdict: Verdict::Accepted,
            score: 1.0,
            time_used: None,
            memory_used: None,
            message: None,
            stdout: None,
            stderr: None,
        };
        // A code-run row binds 10 parameters.
        let fits = vec![row.clone(); BIND_PARAMETER_LIMIT / 10];
        assert!(code_run_results_statement(&fits).expect("fits").is_some());
        let too_many = vec![row; BIND_PARAMETER_LIMIT / 10 + 1];
        let err = code_run_results_statement(&too_many).expect_err("over the limit");
        assert!(err.contains("smaller batches"), "{err}");
    }
}
