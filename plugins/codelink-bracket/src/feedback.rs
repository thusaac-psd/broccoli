//! Per-game penalties and public feedback. Judge results stay unchanged.
use broccoli_server_sdk::prelude::*;
use serde::{Deserialize, Serialize};

use crate::decide::{SubmissionRecord, submission_in_flight};
use crate::model::{MatchPhase, MatchState};
use crate::storage;

const PENALTY_VERDICTS: &[Verdict] = &[
    Verdict::WrongAnswer,
    Verdict::TimeLimitExceeded,
    Verdict::MemoryLimitExceeded,
    Verdict::RuntimeError,
];

pub(crate) fn counts_as_penalty(sub: &SubmissionRecord) -> bool {
    sub.status == SubmissionLifecycle::Judged
        && sub
            .verdict
            .as_ref()
            .is_some_and(|verdict| PENALTY_VERDICTS.contains(verdict))
}

pub(crate) fn score_delay_seconds(m: &MatchState, count: i64, penalty_seconds: i64) -> i64 {
    if m.xiaoju
        .last()
        .is_some_and(|game| m.is_regular_game(game.index))
    {
        count.saturating_mul(penalty_seconds)
    } else {
        0
    }
}

pub fn game_scope(contest: i32, match_id: u8, index: u8) -> String {
    format!("game:{contest}:{match_id}:{index}")
}

#[derive(Debug, Default, Deserialize, Serialize, PartialEq, Clone)]
pub struct PlayerFeedback {
    pub penalty_count: i64,
    #[serde(default)]
    pub delay_seconds: i64,
    pub pending_submission_id: Option<i32>,
    pub effective_at_us: Option<i64>,
    #[serde(skip)]
    pub slot_token: Option<SubmissionSlotToken>,
}

/// Only WA/TLE/MLE/RE in regular games add score penalties. Tiebreaks keep
/// their error count for display but never add a delay. Count submissions,
/// rather than callbacks, so retries cannot apply a penalty twice.
pub fn player_feedback(
    host: &Host,
    contest: i32,
    m: &MatchState,
    user: i32,
    problem: i32,
    penalty_seconds: i64,
) -> Result<PlayerFeedback, SdkError> {
    let Some(game) = m.xiaoju.last() else {
        return Ok(PlayerFeedback::default());
    };
    // Observe the slot before reading verdicts. If it has been released, the
    // prior verdict is already committed and the following count includes it.
    // Reading verdicts first could admit a request with a stale penalty count.
    let slot = host.submission.observe_slot(&player_slot(contest, user))?;
    let mut p = Params::new();
    let contest_param = p.bind(contest);
    let user_param = p.bind(user);
    let penalty_verdicts = PENALTY_VERDICTS
        .iter()
        .map(|verdict| p.bind(verdict.to_db_str()))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT COUNT(*) FILTER (WHERE s.status::text = 'Judged' AND s.verdict IN \
            ({penalty_verdicts})) AS penalty_count \
         FROM submission s \
         WHERE s.contest_id = {} AND s.user_id = {} AND s.problem_id = {} \
           AND s.created_at >= to_timestamp({}::double precision / 1000)",
        contest_param,
        user_param,
        p.bind(problem),
        p.bind(game.opened_at_ms)
    );
    let mut feedback: PlayerFeedback = host
        .db
        .query_one_with_args(&sql, &p.into_args())?
        .unwrap_or_default();
    feedback.slot_token = Some(slot.token);
    if let Some(pending) = slot.pending {
        feedback.pending_submission_id = Some(pending.submission_id);
        feedback.effective_at_us = Some(effective_time(&pending)?);
    }
    feedback.delay_seconds = score_delay_seconds(m, feedback.penalty_count, penalty_seconds);
    Ok(feedback)
}

pub fn player_slot(contest: i32, user: i32) -> String {
    format!("{contest}:{user}")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdmissionMetadata {
    pub scope_key: String,
    pub delay_seconds: i64,
    pub effective_at_us: Option<i64>,
}

pub fn effective_time(row: &SubmissionFeedbackRecord) -> Result<i64, SdkError> {
    let metadata: AdmissionMetadata = serde_json::from_value(row.metadata.clone())?;
    Ok(metadata.effective_at_us.unwrap_or_else(|| {
        row.submitted_at_us
            .saturating_add(metadata.delay_seconds.saturating_mul(1_000_000))
    }))
}

/// Called after the match CAS commits, including on duplicate callbacks. Only
/// Pending admissions in a closed match are retired even if judging is stuck.
/// Retired results remain withheld until judging finishes, but no longer own
/// the contestant's admission slot. Published outcomes never transition again.
/// Returns effective timer keys to cancel only after all post-commit work succeeds.
pub fn publish(
    host: &Host,
    contest: i32,
    match_id: u8,
    m: &MatchState,
) -> Result<Vec<String>, SdkError> {
    let rows = host.submission.feedback_all(&SubmissionFeedbackQuery {
        key: Some(storage::match_key(contest, match_id)),
        unpublished_only: true,
        ..Default::default()
    })?;
    let mut completed_timers = Vec::new();
    for row in rows {
        let metadata: AdmissionMetadata = serde_json::from_value(row.metadata.clone())?;
        let effective_at_us = effective_time(&row)?;
        let game = m
            .xiaoju
            .iter()
            .find(|g| game_scope(contest, match_id, g.index) == metadata.scope_key);
        if let Some(game) = game.filter(|g| m.state == MatchPhase::Decided || g.decided) {
            completed_timers.push(crate::judge::effective_timer_key(
                contest,
                match_id,
                game.index,
                row.submission_id,
            ));
        }
        let record = SubmissionRecord {
            submission_id: row.submission_id,
            user_id: row.user_id,
            problem_id: row.problem_id,
            submitted_at_us: row.submitted_at_us,
            effective_at_us,
            verdict: row.verdict,
            status: row.status,
        };
        if submission_in_flight(&record) {
            if m.state != MatchPhase::Decided {
                continue;
            }
            host.submission.release_slot(record.submission_id)?;
        } else {
            let losing_ac = if !record.verdict.as_ref().is_some_and(Verdict::is_accepted) {
                false
            } else {
                match game {
                    Some(game) if game.decided => game.winner != Some(record.user_id),
                    _ if m.state == MatchPhase::Decided => m.winner != Some(record.user_id),
                    _ => continue,
                }
            };
            let release = if losing_ac {
                ReleaseResult::Override {
                    verdict: Verdict::Other("TooLate".into()),
                    score: 0.0,
                }
            } else {
                ReleaseResult::Original
            };
            host.submission
                .release_feedback(record.submission_id, release)?;
        }
    }
    Ok(completed_timers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiebreak_feedback_has_no_penalty_even_while_awaiting_judging() {
        for state in [MatchPhase::Tiebreak, MatchPhase::AwaitingJudge] {
            let host = Host::mock();
            let m = MatchState {
                state,
                group_a: vec![101, 102, 103],
                xiaoju: vec![crate::model::XiaojuState {
                    index: 3,
                    ..Default::default()
                }],
                ..Default::default()
            };
            host.db
                .queue_query_result(serde_json::json!([{ "penalty_count": 4 }]));
            let feedback = player_feedback(&host, 7, &m, 10, 319, 20).unwrap();
            assert_eq!(feedback.penalty_count, 4);
            assert_eq!(feedback.delay_seconds, 0);
        }
    }
}
