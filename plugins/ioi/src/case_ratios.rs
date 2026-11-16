//! The 0..1 ratio each judged case scored, as live judging scored it.
//!
//! Live judging scores every subtask from these ratios and stores them here
//! beside the judgement. Every later reading of a submission's subtask scores
//! (the subtask-scores route, the sum-best-subtask task score and its
//! scoreboard cells) reads the same ratios back, so a recomputed score is the
//! judged score by construction. The weighted points the host keeps per case
//! cannot stand in for the ratio: a zero-point member stores 0 whatever it
//! scored, and rounding to points drifts a partial ratio.

use std::collections::HashMap;

use broccoli_server_sdk::evaluator::CaseOutcome;
use broccoli_server_sdk::prelude::*;

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

/// The plugin-owned table: one row per judgement, its ratios as a JSON object
/// keyed by test case id.
#[cfg(target_arch = "wasm32")]
const STATEMENTS: &[&str] = &[r#"
    CREATE TABLE IF NOT EXISTS case_ratio (
        judgement_id INTEGER PRIMARY KEY,
        judge_epoch  INTEGER NOT NULL,
        ratios       JSONB   NOT NULL
    )
    "#];

#[cfg(target_arch = "wasm32")]
pub fn create_tables(host: &Host) -> Result<(), SdkError> {
    host.db.ensure_schema("ioi", STATEMENTS)
}

/// Store a judgement's ratios before its result is persisted, so a finalized
/// judgement always has them. An attempt the host fenced out may still reach
/// this, so a write never replaces the ratios of a later attempt; the
/// persisted result of a fenced attempt is refused on its own.
pub fn store(
    host: &Host,
    judgement_id: i32,
    judge_epoch: i32,
    ratios: &CaseRatios,
) -> Result<(), SdkError> {
    let mut p = Params::new();
    let sql = format!(
        "INSERT INTO case_ratio (judgement_id, judge_epoch, ratios) \
         VALUES ({}, {}, {}::jsonb) \
         ON CONFLICT (judgement_id) DO UPDATE \
            SET judge_epoch = EXCLUDED.judge_epoch, ratios = EXCLUDED.ratios \
          WHERE case_ratio.judge_epoch <= EXCLUDED.judge_epoch",
        p.bind(judgement_id),
        p.bind(judge_epoch),
        p.bind(serde_json::to_value(ratios)?),
    );
    host.db.execute_with_args(&sql, &p.into_args())?;
    Ok(())
}
