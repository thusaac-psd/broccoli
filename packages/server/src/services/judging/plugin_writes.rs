//! The statements behind a plugin's writes to the judgement or code run it
//! was dispatched with (`host.submission.*`, `host.code_run.*`). The host
//! function layer binds and executes them.

use broccoli_server_sdk::types::{CodeRunUpdate, SqlBindSink, SubmissionUpdate, push_judge_sets};
use common::StatusPhase;

use super::unit::judgement_mirror;

/// The write fence every plugin write to a judgement goes through: the row
/// (`alias`) is the judgement the caller names, of the submission it names,
/// at the epoch the caller was dispatched with, and not finalized. Epochs are
/// unique per submission and a judgement is finalized when it is superseded
/// or requeued under a new epoch, so only the dispatch that owns an epoch can
/// write, whether its judgement is current or a deferred rejudge.
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
        mirror = judgement_mirror(),
    ))
}

/// The write fence every plugin write to a code run goes through: the run
/// (`alias`) the caller names, at the epoch it was dispatched with, still in
/// flight. A requeue gives the run a new epoch, so a lost dispatch cannot
/// write.
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
        "UPDATE code_run t SET {sets} WHERE {fence}",
        sets = sets.join(", "),
        fence = code_run_write_fence("t", &id, &epoch),
    ))
}
