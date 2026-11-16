//! The statements behind a plugin's writes to the judgement or code run it
//! was dispatched with (`host.submission.*`, `host.code_run.*`). The host
//! function layer binds and executes them.

use broccoli_server_sdk::types::{
    CodeRunUpdate, SubmissionStatus, SubmissionUpdate, Verdict, sanitize_result_text_field,
};
use common::StatusPhase;

use super::unit::{Unit, cascade};

/// The write fence every plugin write to a judgement goes through: the row
/// (`alias`) is the judgement the caller names, of the submission it names,
/// at the epoch the caller was dispatched with, and not finalized. Epochs are
/// unique per submission and a judgement is finalized when it is superseded
/// or requeued under a new epoch, so only the dispatch that owns an epoch can
/// write, whether its judgement is current or a deferred rejudge.
///
/// A statement that only reads the fenced row (a result insert or delete)
/// must lock it `FOR SHARE`: the lock waits out a concurrent requeue and
/// re-checks the fence against the committed row, where a plain read would
/// check its own snapshot and let a stale dispatch's rows in.
pub(crate) fn write_fence(alias: &str, judgement: &str, submission: &str, epoch: &str) -> String {
    format!(
        "{alias}.id = {judgement} AND {alias}.submission_id = {submission} \
         AND {alias}.judge_epoch = {epoch} AND {alias}.is_finalized = FALSE"
    )
}

/// One statement for a plugin's progress or result update: writes the
/// judged columns onto the caller's judgement behind [`write_fence`],
/// finalizes it on a terminal status, and rewrites the submission cache when
/// the judgement is current. Its row count is the number of judgements
/// written (0 when the fence rejected the write). `None` when the update
/// sets nothing.
pub(crate) fn judgement_write<P: SqlBindSink>(
    p: &mut P,
    update: &SubmissionUpdate,
) -> Option<String> {
    let mut sets = Vec::new();
    push_judge_sets(
        p,
        &mut sets,
        &update.status,
        &update.verdict,
        &update.score,
        &update.time_used,
        &update.memory_used,
        &update.compile_output,
        &update.error_code,
        &update.error_message,
    );
    // A judgement's terminal stamp is `finalized_at`; the submission's
    // `judged_at` is mirrored from it.
    sets.retain(|set| set != "judged_at = NOW()");
    if sets.is_empty() {
        return None;
    }
    if update.status.as_ref().is_some_and(|s| s.is_terminal()) {
        sets.push("is_finalized = TRUE".to_string());
        sets.push("finalized_at = NOW()".to_string());
    }
    let judgement = p.bind_value(update.judgement_id.into());
    let submission = p.bind_value(update.submission_id.into());
    let epoch = p.bind_value(update.judge_epoch.into());
    // The submission row is locked before the judgement, as in every
    // judgement transition; the UPDATE joins the lock so it is taken first.
    Some(format!(
        "WITH locked AS (SELECT id FROM submission WHERE id = {submission} FOR NO KEY UPDATE), \
              moved AS (UPDATE submission_judgement t SET {sets} FROM locked \
                        WHERE t.submission_id = locked.id AND {fence} RETURNING t.*){mirror} \
         SELECT 1 FROM moved",
        sets = sets.join(", "),
        fence = write_fence("t", &judgement, &submission, &epoch),
        mirror = cascade(Unit::Judgement),
    ))
}

/// The write fence every plugin write to a code run goes through: the run
/// (`alias`) the caller names, at the epoch it was dispatched with, still in
/// flight. A requeue gives the run a new epoch, so a lost dispatch cannot
/// write. A result insert locks the run `FOR SHARE`, as for [`write_fence`].
pub(crate) fn code_run_write_fence(alias: &str, code_run: &str, epoch: &str) -> String {
    format!(
        "{alias}.id = {code_run} AND {alias}.judge_epoch = {epoch} \
         AND {alias}.status IN {in_flight}",
        in_flight = StatusPhase::InFlight.sql_list(),
    )
}

/// The statement for a plugin's code-run progress or result update, behind
/// [`code_run_write_fence`]; a terminal status stamps `judged_at`. Its row
/// count is the number of runs written. `None` when the update sets nothing.
pub(crate) fn code_run_write<P: SqlBindSink>(p: &mut P, update: &CodeRunUpdate) -> Option<String> {
    let mut sets = Vec::new();
    push_judge_sets(
        p,
        &mut sets,
        &update.status,
        &update.verdict,
        &update.score,
        &update.time_used,
        &update.memory_used,
        &update.compile_output,
        &update.error_code,
        &update.error_message,
    );
    if sets.is_empty() {
        return None;
    }
    let id = p.bind_value(update.code_run_id.into());
    let epoch = p.bind_value(update.judge_epoch.into());
    Some(format!(
        "WITH moved AS (UPDATE code_run t SET {sets} WHERE {fence} RETURNING t.*){cascade} \
         SELECT 1 FROM moved",
        sets = sets.join(", "),
        fence = code_run_write_fence("t", &id, &epoch),
        cascade = cascade(Unit::CodeRun),
    ))
}

/// Sink for positional SQL bind parameters: binds a value and returns its
/// placeholder (e.g. `$3`).
pub(crate) trait SqlBindSink {
    fn bind_value(&mut self, value: serde_json::Value) -> String;
}

/// Build the `SET` column list shared by the `submission`/`code_run` cache row
/// and its mirrored judgement row from a judged update.
///
/// The tri-state `Option<Option<_>>` fields distinguish "leave untouched"
/// (`None`) from "set NULL" (`Some(None)`); a terminal status also stamps
/// `judged_at = NOW()`, and a non-finite score is clamped to 0.
#[allow(clippy::too_many_arguments)]
fn push_judge_sets<P: SqlBindSink>(
    p: &mut P,
    sets: &mut Vec<String>,
    status: &Option<SubmissionStatus>,
    verdict: &Option<Option<Verdict>>,
    score: &Option<f64>,
    time_used: &Option<Option<i32>>,
    memory_used: &Option<Option<i32>>,
    compile_output: &Option<Option<String>>,
    error_code: &Option<Option<String>>,
    error_message: &Option<Option<String>>,
) {
    if let Some(status) = status {
        sets.push(format!("status = {}", p.bind_value(status.as_str().into())));
        if status.is_terminal() {
            sets.push("judged_at = NOW()".into());
        }
    }

    match verdict {
        Some(Some(v)) => sets.push(format!("verdict = {}", p.bind_value(v.to_db_str().into()))),
        Some(None) => sets.push("verdict = NULL".into()),
        None => {}
    }

    if let Some(score) = score {
        let val = if score.is_finite() { *score } else { 0.0 };
        sets.push(format!("score = {}", p.bind_value(val.into())));
    }

    push_double_opt(p, sets, "time_used", time_used);
    push_double_opt(p, sets, "memory_used", memory_used);
    push_double_opt_str(p, sets, "compile_output", compile_output);
    push_double_opt_str(p, sets, "error_code", error_code);
    push_double_opt_str(p, sets, "error_message", error_message);
}

fn push_double_opt<P: SqlBindSink>(
    p: &mut P,
    sets: &mut Vec<String>,
    col: &str,
    val: &Option<Option<i32>>,
) {
    match val {
        Some(Some(v)) => sets.push(format!("{col} = {}", p.bind_value((*v).into()))),
        Some(None) => sets.push(format!("{col} = NULL")),
        None => {}
    }
}

fn push_double_opt_str<P: SqlBindSink>(
    p: &mut P,
    sets: &mut Vec<String>,
    col: &str,
    val: &Option<Option<String>>,
) {
    match val {
        Some(Some(v)) => sets.push(format!(
            "{col} = {}",
            p.bind_value(sanitize_result_text_field(v).as_ref().into())
        )),
        Some(None) => sets.push(format!("{col} = NULL")),
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records bound values and hands back sequential `$N` placeholders, so a
    /// test can assert exactly what SQL `push_judge_sets` builds.
    struct RecordingSink {
        args: Vec<serde_json::Value>,
    }

    impl SqlBindSink for RecordingSink {
        fn bind_value(&mut self, value: serde_json::Value) -> String {
            self.args.push(value);
            format!("${}", self.args.len())
        }
    }

    #[test]
    fn push_judge_sets_builds_expected_set_list_and_args() {
        let mut p = RecordingSink { args: Vec::new() };
        let mut sets = Vec::new();
        push_judge_sets(
            &mut p,
            &mut sets,
            &Some(SubmissionStatus::Judged), // terminal -> also stamps judged_at
            &Some(Some(Verdict::Accepted)),  // Some(Some) -> bound
            &Some(f64::NAN),                 // non-finite -> clamped to 0
            &Some(Some(120)),                // Some(Some) -> bound
            &Some(None),                     // Some(None) -> = NULL, no bind
            &Some(Some("compile log".to_string())), // Some(Some) -> bound (sanitized)
            &None,                           // None -> column untouched
            &Some(None),                     // Some(None) -> = NULL, no bind
        );

        let expected: Vec<String> = [
            "status = $1",
            "judged_at = NOW()",
            "verdict = $2",
            "score = $3",
            "time_used = $4",
            "memory_used = NULL",
            "compile_output = $5",
            "error_message = NULL",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(sets, expected);
        assert_eq!(
            p.args,
            vec![
                serde_json::json!("Judged"),
                serde_json::json!("Accepted"),
                serde_json::json!(0.0),
                serde_json::json!(120),
                serde_json::json!("compile log"),
            ]
        );
    }

    #[test]
    fn push_judge_sets_all_none_writes_nothing() {
        let mut p = RecordingSink { args: Vec::new() };
        let mut sets = Vec::new();
        push_judge_sets(
            &mut p, &mut sets, &None, &None, &None, &None, &None, &None, &None, &None,
        );
        assert!(sets.is_empty());
        assert!(p.args.is_empty());
    }
}
