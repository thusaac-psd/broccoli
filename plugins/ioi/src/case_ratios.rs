//! What live judging scored a judgement with: the 0..1 ratio each judged
//! case scored, and the subtask results those ratios gave.
//!
//! Live judging scores every subtask from the ratios and stores both here
//! beside the judgement. A reading of one judgement's subtask scores (the
//! subtask-scores route) shows the stored subtask results, so it shows what
//! the judgement was scored with even after the problem's subtasks change.
//! The sum-best-subtask task score and its scoreboard cells combine the
//! ratios of every submission under the problem's current subtasks. The
//! weighted points the host keeps per case cannot stand in for the ratio: a
//! zero-point member stores 0 whatever it scored, and rounding to points
//! drifts a partial ratio.

use std::collections::HashMap;

use broccoli_server_sdk::evaluator::CaseOutcome;
use broccoli_server_sdk::prelude::*;

use crate::score::SubtaskScoreDetail;

/// Test case id to the ratio the case scored. A case with no entry scored 0.
pub type CaseRatios = HashMap<i32, f64>;

/// The ratios subtask scoring reads from a judgement's outcomes: every case
/// that ran, at the score the judging policy gave it. Skipped and cancelled
/// cases never ran, so they have no entry.
pub fn judged_ratios(outcomes: &[CaseOutcome]) -> CaseRatios {
    outcomes
        .iter()
        .filter(|outcome| !outcome.verdict.is_skipped_or_cancelled())
        .map(|outcome| (outcome.test_case_id, outcome.score))
        .collect()
}

/// A row of `case_ratio` read back by a scoring query.
#[cfg(target_arch = "wasm32")]
#[derive(serde::Deserialize)]
pub struct JudgedRatios {
    pub ratios: CaseRatios,
}

/// The subtask results of a `case_ratio` row, read back by the subtask-scores
/// route.
#[cfg(target_arch = "wasm32")]
#[derive(serde::Deserialize)]
pub struct JudgedSubtasks {
    pub subtasks: Vec<SubtaskScoreDetail>,
}

/// The join every scoring query reads a judgement's ratios through: the row
/// of judgement `sj` that its current attempt stored. An attempt the host
/// fenced out may have stored ratios the judgement's standing result was
/// never scored with (a later attempt standing as a system error stores
/// none), so the row is keyed by the judgement's epoch as well as its id.
#[cfg(target_arch = "wasm32")]
pub const JOIN_JUDGED_RATIOS: &str =
    "JOIN case_ratio cr ON cr.judgement_id = sj.id AND cr.judge_epoch = sj.judge_epoch";

/// The plugin-owned table: one row per judgement, its ratios as a JSON object
/// keyed by test case id and its subtask results as a JSON array in the
/// order of the subtasks it was scored by.
#[cfg(target_arch = "wasm32")]
const STATEMENTS: &[&str] = &[r#"
    CREATE TABLE IF NOT EXISTS case_ratio (
        judgement_id INTEGER PRIMARY KEY,
        judge_epoch  INTEGER NOT NULL,
        ratios       JSONB   NOT NULL,
        subtasks     JSONB   NOT NULL
    )
    "#];

#[cfg(target_arch = "wasm32")]
pub fn create_tables(host: &Host) -> Result<(), SdkError> {
    host.db.ensure_schema("ioi", STATEMENTS)
}

/// Store what a judgement was scored with before its result is persisted, so
/// a finalized judgement always has it. An attempt the host fenced out may
/// still reach this, so a write never replaces the row of a later attempt;
/// the persisted result of a fenced attempt is refused on its own.
pub fn store(
    host: &Host,
    judgement_id: i32,
    judge_epoch: i32,
    ratios: &CaseRatios,
    subtasks: &[SubtaskScoreDetail],
) -> Result<(), SdkError> {
    let mut p = Params::new();
    let sql = format!(
        "INSERT INTO case_ratio (judgement_id, judge_epoch, ratios, subtasks) \
         VALUES ({}, {}, {}::jsonb, {}::jsonb) \
         ON CONFLICT (judgement_id) DO UPDATE \
            SET judge_epoch = EXCLUDED.judge_epoch, ratios = EXCLUDED.ratios, \
                subtasks = EXCLUDED.subtasks \
          WHERE case_ratio.judge_epoch <= EXCLUDED.judge_epoch",
        p.bind(judgement_id),
        p.bind(judge_epoch),
        p.bind(serde_json::to_value(ratios)?),
        p.bind(serde_json::to_value(subtasks)?),
    );
    host.db.execute_with_args(&sql, &p.into_args())?;
    Ok(())
}
