//! Structured, capability-gated host functions for the plugin core-WRITE path
//! (`host.submission.*` and `host.code_run.*`).
//!
//! Phase 1 of the SQL capability redesign splits the judge's own submission
//! writes off the raw `db_execute`/`db_query` channel. The guest now serializes
//! *structured* input (a `SubmissionUpdate`, a batch of `TestCaseResultRow`s,
//! etc.) and the SERVER owns the SQL. Every write to a
//! judgement's results is fenced by `JUDGEMENT_WRITE_FENCE`. The
//! statements run on the PRIVILEGED pool (the app role) - not the restricted
//! `broccoli_plugin` pool that backs the raw `sql` channel - but still through
//! the SAME NUL-sanitization / arg-parsing / poisoned-connection recovery choke
//! point as raw plugin SQL (`sql::execute_on_pool` / `sql::query_on_pool`).
//!
//! The raw `sql` capability is now restricted to the read-only `broccoli_plugin`
//! role; this module carries the judge's structured core-write path.

use broccoli_server_sdk::types::{
    CodeRunResultRow, CodeRunUpdate, SubmissionUpdate, TestCaseResultRow,
    sanitize_result_text_field,
};
use std::sync::Arc;

use extism::host_fn;
use sea_orm::DatabaseConnection;
use serde_json::{Value as JsonValue, json};

use crate::services::judging;

/// Server-side mirror of the guest SDK's `db::Params`: assigns positional
/// `$1..$N` placeholders (1-based) and collects the bound JSON values, so the
/// SQL built here is byte-identical to what the SDK used to build guest-side.
/// NUL bytes in bound values do not need scrubbing here - every statement is
/// executed through `sql::execute_on_pool` / `sql::query_on_pool`, which strip
/// NUL at the shared choke point exactly as for any other plugin statement.
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
        // JSON null is bound as text by sql::parse_args. Keep non-null values
        // text too: sqlx caches parameter types by SQL, so switching a cached
        // text parameter to binary integer encoding produces a UTF8 0x00 error.
        format!("{}::int", self.bind(json!(value.map(|v| v.to_string()))))
    }

    fn into_args(self) -> Vec<JsonValue> {
        self.args
    }
}

impl broccoli_server_sdk::types::SqlBindSink for Params {
    fn bind_value(&mut self, value: JsonValue) -> String {
        // No NUL scrubbing here: every statement runs through
        // sql::execute_on_pool, which strips NUL at the shared choke point.
        self.bind(value)
    }
}

/// Deserialized view of the `HostDbResponse` envelope `sql::execute_on_pool`
/// returns, for tests that read a statement's row count.
#[cfg(test)]
#[derive(serde::Deserialize)]
struct ExecEnvelope {
    #[serde(default)]
    data: Option<JsonValue>,
    #[serde(default)]
    error: Option<String>,
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

    let Some((sql, args)) = submission_update_statement(&update) else {
        return ok_affected(1);
    };
    // The statement's row count is the number of judgements written, so the
    // envelope is returned as is: 0 tells the guest its epoch is stale.
    let args = serde_json::to_string(&args)?;
    super::sql::execute_on_pool(db, plugin_id, sql, args)
});

host_fn!(pub submission_insert_results(user_data: Arc<(String, DatabaseConnection)>; input: String) -> String {
    let call = super::HostCall::enter(&user_data, "submission_insert_results")?;
    let (plugin_id, db) = &*call;

    let results: Vec<TestCaseResultRow> = serde_json::from_str(&input)
        .map_err(|e| extism::Error::msg(format!("Invalid submission_insert_results input: {e}")))?;

    // The guest early-returns on an empty batch; guard here too so an empty
    // VALUES list can never produce invalid SQL.
    if results.is_empty() {
        return ok_affected(0);
    }

    let mut p = Params::new();
    let mut rows = Vec::with_capacity(results.len());

    for r in &results {
        let score_val = if r.score.is_finite() { r.score } else { 0.0 };
        let message = r.message.as_deref().map(sanitize_result_text_field);
        let stdout = r.stdout.as_deref().map(sanitize_result_text_field);
        let stderr = r.stderr.as_deref().map(sanitize_result_text_field);
        rows.push(format!(
            "({}, {}::int, {}, {}, {}, {}, {}, {}, {}, {}::text, {}::text, {}::text, NOW())",
            p.bind(r.submission_id),
            p.bind(r.judgement_id),
            p.bind(r.judge_epoch),
            p.bind_optional_int(r.test_case_id),
            p.bind_optional_int(r.run_index),
            p.bind(r.verdict.to_db_str()),
            p.bind(score_val),
            p.bind_optional_int(r.time_used),
            p.bind_optional_int(r.memory_used),
            p.bind(json!(message.as_deref())),
            p.bind(json!(stdout.as_deref())),
            p.bind(json!(stderr.as_deref())),
        ));
    }

    let sql = format!(
        "INSERT INTO test_case_result \
         (submission_id, judgement_id, judge_epoch, test_case_id, run_index, verdict, score, \
          time_used, memory_used, checker_output, stdout, stderr, created_at) \
         SELECT v.submission_id, v.judgement_id, v.judge_epoch, v.test_case_id, v.run_index, \
                v.verdict, v.score, v.time_used, v.memory_used, v.checker_output, \
                v.stdout, v.stderr, v.created_at \
         FROM (VALUES {}) AS v( \
             submission_id, judgement_id, judge_epoch, test_case_id, run_index, verdict, score, \
             time_used, memory_used, checker_output, stdout, stderr, created_at \
         ) \
         JOIN submission_judgement j ON {}",
        rows.join(", "),
        judging::write_fence("j", "v.judgement_id", "v.submission_id", "v.judge_epoch"),
    );
    let args = serde_json::to_string(&p.into_args())?;
    super::sql::execute_on_pool(db, plugin_id, sql, args)
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
           AND EXISTS (SELECT 1 FROM submission_judgement j WHERE {})",
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
    let Some((sql, args)) = code_run_update_statement(&update) else {
        return ok_affected(0);
    };
    let args = serde_json::to_string(&args)?;
    super::sql::execute_on_pool(db, plugin_id, sql, args)
});

/// `code_run_insert_results`' statement: the rows land only while their run
/// is still in flight at their epoch. `None` for an empty batch.
fn code_run_results_statement(results: &[CodeRunResultRow]) -> Option<(String, Vec<JsonValue>)> {
    if results.is_empty() {
        return None;
    }
    let mut p = Params::new();
    let mut rows = Vec::with_capacity(results.len());
    for r in results {
        let score_val = if r.score.is_finite() { r.score } else { 0.0 };
        let message = r.message.as_deref().map(sanitize_result_text_field);
        let stdout = r.stdout.as_deref().map(sanitize_result_text_field);
        let stderr = r.stderr.as_deref().map(sanitize_result_text_field);
        rows.push(format!(
            "({}, {}, {}, {}, {}, {}, {}, {}::text, {}::text, {}::text, NOW())",
            p.bind(r.code_run_id),
            p.bind(r.judge_epoch),
            p.bind(r.run_index),
            p.bind(r.verdict.to_db_str()),
            p.bind(score_val),
            p.bind_optional_int(r.time_used),
            p.bind_optional_int(r.memory_used),
            p.bind(json!(message.as_deref())),
            p.bind(json!(stdout.as_deref())),
            p.bind(json!(stderr.as_deref())),
        ));
    }
    let sql = format!(
        "INSERT INTO code_run_result \
         (code_run_id, judge_epoch, run_index, verdict, score, \
          time_used, memory_used, checker_output, stdout, stderr, created_at) \
         SELECT v.code_run_id, v.judge_epoch, v.run_index, v.verdict, v.score, \
                v.time_used, v.memory_used, v.checker_output, v.stdout, v.stderr, v.created_at \
         FROM (VALUES {}) AS v( \
             code_run_id, judge_epoch, run_index, verdict, score, \
             time_used, memory_used, checker_output, stdout, stderr, created_at \
         ) \
         JOIN code_run cr ON {}",
        rows.join(", "),
        judging::code_run_write_fence("cr", "v.code_run_id", "v.judge_epoch"),
    );
    Some((sql, p.into_args()))
}

host_fn!(pub code_run_insert_results(user_data: Arc<(String, DatabaseConnection)>; input: String) -> String {
    let call = super::HostCall::enter(&user_data, "code_run_insert_results")?;
    let (plugin_id, db) = &*call;

    let results: Vec<CodeRunResultRow> = serde_json::from_str(&input)
        .map_err(|e| extism::Error::msg(format!("Invalid code_run_insert_results input: {e}")))?;
    let Some((sql, args)) = code_run_results_statement(&results) else {
        return ok_affected(0);
    };
    let args = serde_json::to_string(&args)?;
    super::sql::execute_on_pool(db, plugin_id, sql, args)
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::{submission, submission_judgement, test_case_result};
    use crate::services::judging::{OpenJudgement, open_judgement};
    use broccoli_server_sdk::types::{SubmissionStatus, Verdict};
    use chrono::Utc;
    use sea_orm::{
        ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, Set,
        TransactionTrait,
    };

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
            judgement_id: Set(Some(first.id)),
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
                    let (rsql, rargs) = code_run_results_statement(&[result]).expect("rows");
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
}
