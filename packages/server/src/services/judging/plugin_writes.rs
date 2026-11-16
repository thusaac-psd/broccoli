//! The statements behind a plugin's writes to the judgement or code run it
//! was dispatched with (`host.submission.*`, `host.code_run.*`). The host
//! function layer binds and executes them.

use broccoli_server_sdk::types::{
    CodeRunUpdate, SubmissionStatus, SubmissionUpdate, Verdict, sanitize_result_text_field,
};
use common::StatusPhase;

use super::fault::{Recorded, SystemErrorClass};
use super::unit::{Unit, cascade};
use crate::error::HostCode;
use crate::services::plugin_output::plugin_code;

/// A plugin's write to the unit it was dispatched with: the parts of it
/// [`as_recorded`] rules on, alike for a judgement and a code run.
pub(crate) trait PluginWrite {
    /// The write ends its unit.
    fn finalizes(&self) -> bool;
    /// The write ends its unit as a system error.
    fn ends_in_system_error(&self) -> bool;
    fn error_code(&mut self) -> &mut Option<Option<String>>;
    fn error_message(&mut self) -> &mut Option<Option<String>>;
    /// Records whether a system error the write ends its unit with stands. A
    /// code run's always does: it is never retried.
    fn set_stands(&mut self, stands: bool);
    /// The plugin's word that its system error stands.
    fn stands(&self) -> bool;
}

/// `status` and `verdict` end a unit as a system error. A plugin writes no
/// `SystemError` status, so its system error is a terminal write with a
/// `SystemError` verdict, whichever terminal status; the judging scans read
/// a finished row by the same rule (`unit::system_error`).
fn ends_in_system_error(
    status: Option<SubmissionStatus>,
    verdict: &Option<Option<Verdict>>,
) -> bool {
    status.is_some_and(|s| s.is_terminal()) && matches!(verdict, Some(Some(Verdict::SystemError)))
}

impl PluginWrite for SubmissionUpdate {
    fn finalizes(&self) -> bool {
        self.status.is_some_and(|s| s.is_terminal())
    }
    fn ends_in_system_error(&self) -> bool {
        ends_in_system_error(self.status, &self.verdict)
    }
    fn error_code(&mut self) -> &mut Option<Option<String>> {
        &mut self.error_code
    }
    fn error_message(&mut self) -> &mut Option<Option<String>> {
        &mut self.error_message
    }
    fn set_stands(&mut self, stands: bool) {
        self.stands = stands;
    }
    fn stands(&self) -> bool {
        self.stands
    }
}

impl PluginWrite for CodeRunUpdate {
    fn finalizes(&self) -> bool {
        self.status.is_some_and(|s| s.is_terminal())
    }
    fn ends_in_system_error(&self) -> bool {
        ends_in_system_error(self.status, &self.verdict)
    }
    fn error_code(&mut self) -> &mut Option<Option<String>> {
        &mut self.error_code
    }
    fn error_message(&mut self) -> &mut Option<Option<String>> {
        &mut self.error_message
    }
    fn set_stands(&mut self, _: bool) {}
    fn stands(&self) -> bool {
        true
    }
}

/// `update` from `plugin_id` as the host records it, the one rule for every
/// error code a judgement or code run write carries:
///
/// - the plugin's own code is recorded under its id ([`plugin_code`]);
/// - an update that ends the unit as a system error states its code: one
///   that names none is recorded as `PLUGIN_ERROR`, so no system error, one
///   that stands least of all, is shown without a code;
/// - the plugin's word that its system error stands
///   ([`SubmissionUpdate::stands`]) counts only on the update that ends the
///   unit in one;
/// - an update that would finalize an attempt the platform lost (a fault of
///   the hand-back class, see [`super::DispatchFaults::lost`]) is `None`, a
///   write fenced out as a stale dispatch's is, whatever verdict the plugin
///   computed from the lost operations: the dispatch hands the unit back
///   when it ends;
/// - under any other fault, an update that ends the unit as a system error
///   carries the fault's bare host code and description, and stands as the
///   fault's class says, whatever the plugin reported, so the system-error
///   retry treats it by its cause.
///
/// Any other update is unchanged.
pub(crate) fn as_recorded<W: PluginWrite>(
    plugin_id: &str,
    mut update: W,
    fault: Option<Recorded>,
) -> Option<W> {
    let code = update.error_code();
    *code = code
        .take()
        .map(|code| code.map(|code| plugin_code(plugin_id, &code)));
    let ends_in_system_error = update.ends_in_system_error();
    if ends_in_system_error && !matches!(update.error_code(), Some(Some(_))) {
        *update.error_code() = Some(Some(HostCode::PluginError.as_str().to_string()));
    }
    update.set_stands(update.stands() && ends_in_system_error);
    let Some(fault) = fault else {
        return Some(update);
    };
    let class = SystemErrorClass::of_fault(fault.fault);
    if class == SystemErrorClass::HandBack {
        return (!update.finalizes()).then_some(update);
    }
    if ends_in_system_error {
        *update.error_code() = Some(Some(fault.fault.code().to_string()));
        *update.error_message() = Some(Some(fault.message));
        update.set_stands(class == SystemErrorClass::Stands);
    }
    Some(update)
}

/// The database fence every plugin write to a judgement goes through, after
/// the host checked that the judgement is the one the calling plugin's call
/// was dispatched with ([`super::Dispatch::owned`]): the row (`alias`) is
/// that judgement, of its submission, at the epoch of the dispatch, and not
/// finalized. Epochs are unique per submission and a judgement is finalized
/// when it is superseded or requeued under a new epoch, so a dispatch whose
/// epoch has passed writes nothing, whether its judgement is current or a
/// deferred rejudge.
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
        sets.push(format!("error_stands = {}", update.stands));
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

/// The database fence every plugin write to a code run goes through, after
/// the host checked that the run is the one the calling plugin's call was
/// dispatched with: the run (`alias`) at the epoch of the dispatch, still in
/// flight. A requeue gives the run a new epoch, so a lost dispatch cannot
/// write. A result insert locks the run `FOR SHARE`, as for [`write_fence`].
pub(crate) fn code_run_write_fence(alias: &str, code_run: &str, epoch: &str) -> String {
    format!(
        "{alias}.id = {code_run} AND {alias}.judge_epoch = {epoch} \
         AND {alias}.status IN {in_flight}",
        in_flight = StatusPhase::InFlight.sql_list(),
    )
}

/// The statement for a plugin's code-run progress or result update, as the
/// host records it ([`as_recorded`]), behind [`code_run_write_fence`]; a
/// terminal status stamps `judged_at`. Its row count is the number of runs
/// written. `None` when the update sets nothing.
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

    /// A plugin's standing system error is recorded as standing; the word
    /// counts on nothing else it writes. A fault that stands decides it
    /// whatever the plugin said.
    #[test]
    fn a_system_error_stands_only_when_its_plugin_or_its_fault_says_so() {
        let standing = SubmissionUpdate::standing_system_error(1, 1, 0, "NO_TEST_CASES", "none");
        let recorded = as_recorded("judge", standing, None).unwrap();
        assert!(recorded.stands);
        assert_eq!(
            recorded.error_code,
            Some(Some("judge.NO_TEST_CASES".into()))
        );
        for u in [
            update(SubmissionStatus::Judged, Some(Verdict::WrongAnswer)),
            update(SubmissionStatus::Running, Some(Verdict::SystemError)),
        ] {
            let u = SubmissionUpdate { stands: true, ..u };
            assert!(!as_recorded("judge", u, None).unwrap().stands);
        }
        let retried = update(SubmissionStatus::Judged, Some(Verdict::SystemError));
        assert!(!as_recorded("judge", retried.clone(), None).unwrap().stands);
        assert!(
            as_recorded("judge", retried, checker_failed())
                .unwrap()
                .stands
        );
    }

    /// A system error a plugin writes without a code is recorded with the
    /// host's `PLUGIN_ERROR`, a judgement's and a code run's alike.
    #[test]
    fn a_system_error_without_a_code_is_recorded_as_a_plugin_error() {
        for code in [None, Some(None)] {
            let u = SubmissionUpdate {
                error_code: code.clone(),
                ..update(SubmissionStatus::Judged, Some(Verdict::SystemError))
            };
            assert_eq!(
                as_recorded("judge", u, None).unwrap().error_code,
                Some(Some("PLUGIN_ERROR".into()))
            );
            let run = CodeRunUpdate {
                status: Some(SubmissionStatus::Judged),
                verdict: Some(Some(Verdict::SystemError)),
                error_code: code,
                ..Default::default()
            };
            assert_eq!(
                as_recorded("judge", run, None).unwrap().error_code,
                Some(Some("PLUGIN_ERROR".into()))
            );
        }
        let progress = SubmissionUpdate {
            error_code: None,
            ..update(SubmissionStatus::Running, Some(Verdict::SystemError))
        };
        assert_eq!(
            as_recorded("judge", progress, None).unwrap().error_code,
            None
        );
    }

    /// A plugin's write has no `SystemError` status to send: its system
    /// error is a terminal status with a `SystemError` verdict, the one shape
    /// [`PluginWrite::ends_in_system_error`] reads.
    #[test]
    fn a_plugin_cannot_write_a_system_error_status() {
        let refused = serde_json::from_value::<SubmissionUpdate>(serde_json::json!({
            "submission_id": 1,
            "judgement_id": 1,
            "judge_epoch": 1,
            "status": "SystemError",
        }));
        assert!(refused.is_err());
        let refused = serde_json::from_value::<CodeRunUpdate>(serde_json::json!({
            "code_run_id": 1,
            "judge_epoch": 1,
            "status": "SystemError",
        }));
        assert!(refused.is_err());
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
        assert_eq!(
            as_recorded("judge", run, None).unwrap().error_code,
            Some(Some("judge.NO_TEST_CASES".into()))
        );
    }

    /// A code run's write follows the judgement's rule: a lost attempt's
    /// result is fenced out, and a system error under a fault carries the
    /// fault's code.
    #[test]
    fn a_code_run_write_follows_the_same_fault_rule() {
        let run = |verdict| CodeRunUpdate {
            status: Some(SubmissionStatus::Judged),
            verdict: Some(Some(verdict)),
            error_code: Some(Some("PLUGIN_SAYS".into())),
            ..Default::default()
        };
        assert!(as_recorded("judge", run(Verdict::Accepted), lost()).is_none());
        let recorded = as_recorded("judge", run(Verdict::SystemError), checker_failed()).unwrap();
        assert_eq!(recorded.error_code, Some(Some("CHECKER_FAILED".into())));
        let progress = CodeRunUpdate {
            status: Some(SubmissionStatus::Running),
            ..Default::default()
        };
        assert!(as_recorded("judge", progress, lost()).is_some());
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
