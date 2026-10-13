//! Per-game penalties and public feedback. Judge results stay unchanged.
use broccoli_server_sdk::prelude::*;
use serde::{Deserialize, Serialize};

use crate::decide::{SubmissionLifecycle, SubmissionRecord, submission_in_flight};
use crate::model::{MatchPhase, MatchState};
use crate::storage;

pub const PENALTY_SECONDS: i64 = 20;

pub fn game_scope(contest: i32, match_id: u8, index: u8) -> String {
    format!("game:{contest}:{match_id}:{index}")
}

#[derive(Debug, Default, Deserialize, Serialize, PartialEq, Clone)]
pub struct PlayerFeedback {
    pub penalty_count: i64,
    pub pending_submission_id: Option<i32>,
    pub effective_at_us: Option<i64>,
}

/// Only WA/TLE/MLE/RE count. Count submissions, rather than callbacks, so
/// retries cannot apply a penalty twice. CE and infrastructure failures do not.
pub fn player_feedback(
    host: &Host,
    contest: i32,
    m: &MatchState,
    user: i32,
    problem: i32,
) -> Result<PlayerFeedback, SdkError> {
    let Some(game) = m.xiaoju.last() else {
        return Ok(PlayerFeedback::default());
    };
    let mut p = Params::new();
    let contest_param = p.bind(contest);
    let user_param = p.bind(user);
    let sql = format!(
        "SELECT COUNT(*) FILTER (WHERE s.status::text = 'Judged' AND s.verdict IN \
            ('WrongAnswer', 'TimeLimitExceeded', 'MemoryLimitExceeded', 'RuntimeError')) AS penalty_count, \
         (SELECT MIN(submission_id) FROM submission_feedback WHERE contest_id = {contest_param} AND user_id = {user_param} AND outcome = 'pending') AS pending_submission_id, \
         (SELECT MIN(effective_at_us) FROM submission_feedback WHERE contest_id = {contest_param} AND user_id = {user_param} AND outcome = 'pending') AS effective_at_us \
         FROM submission s \
         WHERE s.contest_id = {} AND s.user_id = {} AND s.problem_id = {} \
           AND s.created_at >= to_timestamp({}::double precision / 1000)",
        contest_param,
        user_param,
        p.bind(problem),
        p.bind(game.opened_at_ms)
    );
    Ok(host
        .db
        .query_one_with_args(&sql, &p.into_args())?
        .unwrap_or_default())
}

#[derive(Deserialize)]
struct FeedbackRow {
    submission_id: i32,
    user_id: i32,
    problem_id: i32,
    submitted_at_us: i64,
    effective_at_us: i64,
    scope_key: String,
    verdict: Option<Verdict>,
    status: SubmissionLifecycle,
}

/// Called after the match CAS commits, including on duplicate callbacks. Only
/// Pending admissions in a closed match are retired even if judging is stuck.
/// Retired results remain withheld until judging finishes, but no longer own
/// the contestant's admission slot. Published outcomes never transition again.
pub fn publish(host: &Host, contest: i32, match_id: u8, m: &MatchState) -> Result<(), SdkError> {
    let mut p = Params::new();
    let sql = format!(
        "SELECT s.id AS submission_id, s.user_id, s.problem_id, \
         (EXTRACT(EPOCH FROM s.created_at) * 1000000)::bigint AS submitted_at_us, \
         f.effective_at_us, f.scope_key, s.verdict, s.status::text AS status \
         FROM submission_feedback f JOIN submission s ON s.id = f.submission_id \
         WHERE f.plugin_id = 'codelink-bracket' AND f.state_key = {} AND f.outcome IN ('pending', 'retired')",
        p.bind(storage::match_key(contest, match_id))
    );
    let rows: Vec<FeedbackRow> = host.db.query_with_args(&sql, &p.into_args())?;
    for row in rows {
        let game = m
            .xiaoju
            .iter()
            .find(|g| game_scope(contest, match_id, g.index) == row.scope_key);
        if let Some(game) = game.filter(|g| m.state == MatchPhase::Decided || g.decided) {
            host.timer.cancel(&crate::judge::effective_timer_key(
                contest,
                match_id,
                game.index,
                row.submission_id,
            ))?;
        }
        let record = SubmissionRecord {
            submission_id: row.submission_id,
            user_id: row.user_id,
            problem_id: row.problem_id,
            submitted_at_us: row.submitted_at_us,
            effective_at_us: row.effective_at_us,
            verdict: row.verdict,
            status: row.status,
        };
        let outcome = if submission_in_flight(&record) {
            if m.state != MatchPhase::Decided {
                continue;
            }
            "retired"
        } else if !record.verdict.as_ref().is_some_and(Verdict::is_accepted) {
            "released"
        } else {
            match game {
                Some(game) if game.decided => {
                    if game.winner == Some(record.user_id) {
                        "released"
                    } else {
                        "too_late"
                    }
                }
                _ if m.state == MatchPhase::Decided => {
                    if m.winner == Some(record.user_id) {
                        "released"
                    } else {
                        "too_late"
                    }
                }
                _ => continue,
            }
        };
        let mut p = Params::new();
        host.db.execute_with_args(&format!(
            "UPDATE submission_feedback SET outcome = {} WHERE submission_id = {} AND outcome IN ('pending', 'retired')",
            p.bind(outcome), p.bind(record.submission_id)), &p.into_args())?;
    }
    Ok(())
}
