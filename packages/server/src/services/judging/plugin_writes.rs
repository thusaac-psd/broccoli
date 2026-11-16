//! The statements behind a plugin's writes to the judgement or code run it
//! was dispatched with (`host.submission.*`, `host.code_run.*`). The host
//! function layer binds and executes them.

use broccoli_server_sdk::types::{
    CodeRunUpdate, SubmissionStatus, SubmissionUpdate, Verdict, sanitize_result_text_field,
};
use common::StatusPhase;

use super::fault::{Recorded, SystemErrorClass};
use super::unit::{Unit, cascade};

/// A plugin's error code as the host records it on a judgement or code run:
/// `<plugin_id>.<code>`. A plugin id holds no `.`, so the UI reads the raising
/// plugin off the code and looks its translation up in that plugin's pack
/// (`<plugin_id>.apiError.<code>`), and two plugins' codes never collide. A
/// recorded code without a plugin id is the host's own: a plugin never writes
/// one, so it cannot pass for a platform fault or a code that stands (see
/// [`super::fault`]).
pub(crate) fn plugin_error_code(plugin_id: &str, code: &str) -> String {
    format!("{plugin_id}.{code}")
}

fn attributed(plugin_id: &str, code: Option<Option<String>>) -> Option<Option<String>> {
    code.map(|code| code.map(|code| plugin_error_code(plugin_id, &code)))
}

/// `update` from `plugin_id` as the host records it, the one rule for every
/// error code a judgement write carries:
///
/// - the plugin's own code is recorded under its id ([`plugin_error_code`]);
/// - an update that would finalize an attempt the platform lost (a fault of
///   the hand-back class, see [`super::DispatchFaults::lost`]) is `None`, a
///   write fenced out as a stale dispatch's is, whatever verdict the plugin
///   computed from the lost operations: the dispatch hands the judgement back
///   when it ends;
/// - under any other fault, an update that ends the judgement as a system
///   error carries the fault's bare host code and description, whatever the
///   plugin reported, so the system-error retry treats it by its cause.
///
/// Any other update is unchanged.
pub(crate) fn as_recorded(
    plugin_id: &str,
    mut update: SubmissionUpdate,
    fault: Option<Recorded>,
) -> Option<SubmissionUpdate> {
    update.error_code = attributed(plugin_id, update.error_code);
    let Some(fault) = fault else {
        return Some(update);
    };
    let finalizes = update.status.is_some_and(|s| s.is_terminal());
    if SystemErrorClass::of_fault(fault.fault) == SystemErrorClass::HandBack {
        return (!finalizes).then_some(update);
    }
    let ends_in_system_error =
        finalizes && matches!(update.verdict, Some(Some(Verdict::SystemError)));
    if ends_in_system_error {
        update.error_code = Some(Some(fault.fault.code().to_string()));
        update.error_message = Some(Some(fault.message));
    }
    Some(update)
}

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

/// One statement for a plugin's progress or result update, as the host
/// records it ([`as_recorded`]): writes the judged columns onto the caller's
/// judgement behind [`write_fence`],
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

/// The statement for `plugin_id`'s code-run progress or result update, behind
/// [`code_run_write_fence`], its error code the plugin's
/// ([`plugin_error_code`]); a terminal status stamps `judged_at`. Its row
/// count is the number of runs written. `None` when the update sets nothing.
pub(crate) fn code_run_write<P: SqlBindSink>(
    p: &mut P,
    plugin_id: &str,
    mut update: CodeRunUpdate,
) -> Option<String> {
    update.error_code = attributed(plugin_id, update.error_code);
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
    use crate::error::HostCode;
    use crate::services::judging::fault::DispatchFault;
    use common::worker::OperationFault;

    fn update(status: SubmissionStatus, verdict: Option<Verdict>) -> SubmissionUpdate {
        SubmissionUpdate {
            submission_id: 1,
            judgement_id: 1,
            judge_epoch: 1,
            status: Some(status),
            verdict: Some(verdict),
            error_code: Some(Some("PLUGIN_SAYS".into())),
            ..Default::default()
        }
    }

    fn lost() -> Option<Recorded> {
        Some(Recorded {
            fault: OperationFault::NoCapableWorker.into(),
            message: "none accepting".into(),
        })
    }

    #[test]
    fn no_verdict_finalizes_a_lost_attempt() {
        for verdict in [Some(Verdict::SystemError), Some(Verdict::Accepted), None] {
            let u = update(SubmissionStatus::Judged, verdict.clone());
            assert!(as_recorded("judge", u, lost()).is_none(), "{verdict:?}");
        }
        let u = update(SubmissionStatus::CompilationError, None);
        assert!(as_recorded("judge", u, lost()).is_none());
    }

    #[test]
    fn progress_of_a_lost_attempt_and_every_write_of_a_sound_one_stand() {
        let u = update(SubmissionStatus::Running, None);
        let recorded = as_recorded("judge", u.clone(), lost()).expect("progress stands");
        assert_eq!(recorded.status, u.status);
        assert_eq!(recorded.error_code, Some(Some("judge.PLUGIN_SAYS".into())));
        let u = update(SubmissionStatus::Judged, Some(Verdict::SystemError));
        let recorded = as_recorded("judge", u, None).expect("a sound write stands");
        assert_eq!(recorded.error_code, Some(Some("judge.PLUGIN_SAYS".into())));
    }

    fn checker_failed() -> Option<Recorded> {
        Some(Recorded {
            fault: DispatchFault::CheckerFailed,
            message: "signal 6".into(),
        })
    }

    /// Under a fault that stands, a system error carries the fault's bare
    /// host code, not the plugin's: the class reads the host's cause.
    #[test]
    fn a_system_error_under_a_fault_that_stands_carries_the_faults_code() {
        let u = as_recorded(
            "judge",
            update(SubmissionStatus::Judged, Some(Verdict::SystemError)),
            checker_failed(),
        )
        .unwrap();
        assert_eq!(u.error_code, Some(Some("CHECKER_FAILED".into())));
        assert_eq!(u.error_message, Some(Some("signal 6".into())));
    }

    #[test]
    fn other_updates_record_the_plugins_code_under_its_id() {
        for u in [
            update(SubmissionStatus::Judged, Some(Verdict::WrongAnswer)),
            update(SubmissionStatus::Running, Some(Verdict::SystemError)),
        ] {
            assert_eq!(
                as_recorded("judge", u, checker_failed())
                    .unwrap()
                    .error_code,
                Some(Some("judge.PLUGIN_SAYS".into()))
            );
        }
    }

    /// A plugin cannot record a host code: one that names a platform fault
    /// or a refusal is recorded under the plugin's id, so it never reads as
    /// one of the host's classes.
    #[test]
    fn a_plugin_writing_a_host_code_records_it_as_its_own() {
        for code in [HostCode::PluginRefused, HostCode::OperationLost] {
            let mut u = update(SubmissionStatus::Judged, Some(Verdict::SystemError));
            u.error_code = Some(Some(code.as_str().into()));
            assert_eq!(
                as_recorded("judge", u, None).unwrap().error_code,
                Some(Some(format!("judge.{code}")))
            );
        }
        let cleared = SubmissionUpdate {
            error_code: Some(None),
            ..update(SubmissionStatus::Judged, Some(Verdict::Accepted))
        };
        assert_eq!(
            as_recorded("judge", cleared, None).unwrap().error_code,
            Some(None)
        );
        let run = CodeRunUpdate {
            error_code: Some(Some("NO_TEST_CASES".into())),
            ..Default::default()
        };
        let mut p = RecordingSink { args: Vec::new() };
        code_run_write(&mut p, "judge", run).expect("sets");
        assert_eq!(p.args[0], serde_json::json!("judge.NO_TEST_CASES"));
    }

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
