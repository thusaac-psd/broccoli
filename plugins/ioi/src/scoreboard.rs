// Only used by the wasm32-gated cell loaders below; this file has no
// #[cfg(test)] use of HashMap, the SDK prelude, or serde's Deserialize.
#[cfg(target_arch = "wasm32")]
use std::collections::HashMap;

#[cfg(target_arch = "wasm32")]
use broccoli_server_sdk::prelude::*;
#[cfg(target_arch = "wasm32")]
use serde::Deserialize;

// ContestConfig/ScoringMode/SubtaskDef/resolve_tc_label are only used by the
// wasm32-gated cell loaders below; ScoreboardTiebreaker/ScoreboardVisibility
// are also used directly by the #[cfg(test)] unit tests, so gate the latter
// two more broadly.
#[cfg(target_arch = "wasm32")]
use crate::config::{ContestConfig, ScoringMode, SubtaskDef, resolve_tc_label};
#[cfg(any(target_arch = "wasm32", test))]
use crate::config::{ScoreboardTiebreaker, ScoreboardVisibility};
#[cfg(target_arch = "wasm32")]
use crate::scoring::score_best_tokened_or_last;
#[cfg(target_arch = "wasm32")]
use crate::subtasks::score_all_subtasks;
#[cfg(target_arch = "wasm32")]
use crate::tokens::TokenState;
#[cfg(target_arch = "wasm32")]
use crate::{load_effective_subtasks, load_task_config};

// Used by `scoreboard_entries_tied` (wasm32+test) and the wasm32-only cell
// loaders below; gate broadly enough to cover both.
#[cfg(any(target_arch = "wasm32", test))]
const SCORE_EPSILON: f64 = 1e-9;

// Called from the wasm32-gated `api.rs` scoreboard handler AND directly by
// the #[cfg(test)] unit tests below; gate the same way.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn full_scoreboard_visible_for_phase(
    phase: &str,
    can_view_all: bool,
    scoreboard_visibility: ScoreboardVisibility,
) -> bool {
    can_view_all
        || phase == "after"
        || (phase == "during" && scoreboard_visibility == ScoreboardVisibility::AllContestViewers)
}

// Called from the wasm32-gated `api.rs` scoreboard handler AND directly by the
// #[cfg(test)] unit tests below; gate the same way. Gating this to wasm32 alone
// would make the tiebreaker arithmetic structurally unreachable from any host
// test - the same mistake that once hid `standings_restriction` behind a wasm32
// gate on this branch.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn combined_score_time_seconds(tiebreaker: ScoreboardTiebreaker, times: &[i64]) -> i64 {
    match tiebreaker {
        ScoreboardTiebreaker::EqualRank => 0,
        ScoreboardTiebreaker::SumScoreTime => times.iter().copied().sum(),
        ScoreboardTiebreaker::MaxScoreTime => times.iter().copied().max().unwrap_or(0),
    }
}

// Called from the wasm32-gated `api.rs` scoreboard handler AND directly by
// the #[cfg(test)] unit tests below; gate the same way.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn compare_scoreboard_entries(
    a_score: f64,
    a_time: i64,
    a_username: &str,
    b_score: f64,
    b_time: i64,
    b_username: &str,
    tiebreaker: ScoreboardTiebreaker,
) -> std::cmp::Ordering {
    b_score
        .partial_cmp(&a_score)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| match tiebreaker {
            ScoreboardTiebreaker::EqualRank => std::cmp::Ordering::Equal,
            ScoreboardTiebreaker::SumScoreTime | ScoreboardTiebreaker::MaxScoreTime => {
                a_time.cmp(&b_time)
            }
        })
        .then_with(|| a_username.cmp(b_username))
}

// Called from the wasm32-gated `api.rs` scoreboard handler AND directly by
// the #[cfg(test)] unit tests below; gate the same way.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn scoreboard_entries_tied(
    a_score: f64,
    a_time: i64,
    b_score: f64,
    b_time: i64,
    tiebreaker: ScoreboardTiebreaker,
) -> bool {
    (a_score - b_score).abs() < SCORE_EPSILON
        && match tiebreaker {
            ScoreboardTiebreaker::EqualRank => true,
            ScoreboardTiebreaker::SumScoreTime | ScoreboardTiebreaker::MaxScoreTime => {
                a_time == b_time
            }
        }
}

// Only constructed by the wasm32-gated `load_max_submission_scoreboard_cells`
// below.
#[cfg(target_arch = "wasm32")]
#[derive(Deserialize)]
struct MaxSubmissionScoreboardRow {
    user_id: i32,
    problem_id: i32,
    score: f64,
    score_time_seconds: i64,
}

// Only constructed by the wasm32-gated cell loaders below.
#[cfg(target_arch = "wasm32")]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ScoreboardCell {
    pub(crate) score: f64,
    pub(crate) score_time_seconds: i64,
}

// Only constructed by the wasm32-gated
// `load_best_tokened_or_last_scoreboard_cells` below.
#[cfg(target_arch = "wasm32")]
#[derive(Deserialize)]
struct ScoreboardSubmissionRow {
    user_id: i32,
    problem_id: i32,
    score: f64,
    elapsed_seconds: i64,
}

// Only constructed by the wasm32-gated `load_sum_best_subtask_scoreboard_cells`
// below.
#[cfg(target_arch = "wasm32")]
#[derive(Deserialize)]
struct ScoreboardTcScoreRow {
    user_id: i32,
    problem_id: i32,
    submission_id: i32,
    test_case_id: i32,
    score: f64,
    verdict: Verdict,
    elapsed_seconds: i64,
}

#[cfg(target_arch = "wasm32")]
fn load_max_submission_scoreboard_cells(
    host: &Host,
    contest_id: i32,
    user_ids: &[i32],
    problem_ids: &[i32],
) -> Result<HashMap<(i32, i32), ScoreboardCell>, SdkError> {
    if user_ids.is_empty() || problem_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut p = Params::new();
    let contest_placeholder = p.bind(contest_id);
    let user_placeholders: Vec<String> = user_ids.iter().map(|id| p.bind(*id)).collect();
    let problem_placeholders: Vec<String> = problem_ids.iter().map(|id| p.bind(*id)).collect();
    let score_epsilon_placeholder = p.bind(SCORE_EPSILON);
    let sql = format!(
        "WITH scored AS ( \
             SELECT s.user_id, s.problem_id, sj.score as score, \
                    GREATEST(EXTRACT(EPOCH FROM (s.created_at - c.start_time))::bigint, 0) \
                      as elapsed_seconds \
             FROM submission s \
             JOIN contest c ON c.id = s.contest_id \
             JOIN submission_judgement sj \
               ON sj.submission_id = s.id \
              AND sj.is_current = TRUE \
              AND sj.judge_epoch = s.judge_epoch \
             WHERE s.contest_id = {} \
               AND s.user_id IN ({}) \
               AND s.problem_id IN ({}) \
               AND sj.score IS NOT NULL \
         ), maxes AS ( \
             SELECT user_id, problem_id, MAX(score) as score \
             FROM scored \
             GROUP BY user_id, problem_id \
         ) \
         SELECT m.user_id, m.problem_id, m.score, \
                COALESCE(MIN(s.elapsed_seconds) FILTER \
                    (WHERE m.score > 0.0 AND s.score >= m.score - {}), 0) \
                    as score_time_seconds \
         FROM maxes m \
         JOIN scored s ON s.user_id = m.user_id AND s.problem_id = m.problem_id \
         GROUP BY m.user_id, m.problem_id, m.score",
        contest_placeholder,
        user_placeholders.join(","),
        problem_placeholders.join(","),
        score_epsilon_placeholder,
    );
    let rows: Vec<MaxSubmissionScoreboardRow> = host.db.query_with_args(&sql, &p.into_args())?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                (r.user_id, r.problem_id),
                ScoreboardCell {
                    score: r.score,
                    score_time_seconds: r.score_time_seconds,
                },
            )
        })
        .collect())
}

#[cfg(target_arch = "wasm32")]
fn load_best_tokened_or_last_scoreboard_cells(
    host: &Host,
    contest_id: i32,
    user_ids: &[i32],
    problem_ids: &[i32],
) -> Result<HashMap<(i32, i32), ScoreboardCell>, SdkError> {
    if user_ids.is_empty() || problem_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let token_keys: Vec<String> = user_ids
        .iter()
        .map(|user_id| format!("tokens:{contest_id}:{user_id}"))
        .collect();
    let token_key_refs: Vec<&str> = token_keys.iter().map(|key| key.as_str()).collect();
    let raw_token_states = host.storage.get(&token_key_refs)?;
    let mut tokened_submission_ids = Vec::new();
    for key in &token_keys {
        if let Some(raw) = raw_token_states.get(key) {
            let state: TokenState = serde_json::from_str(raw).unwrap_or_default();
            tokened_submission_ids.extend(state.tokened_submission_ids);
        }
    }
    tokened_submission_ids.sort_unstable();
    tokened_submission_ids.dedup();

    let mut p = Params::new();
    let contest_placeholder = p.bind(contest_id);
    let user_placeholders: Vec<String> = user_ids.iter().map(|id| p.bind(*id)).collect();
    let problem_placeholders: Vec<String> = problem_ids.iter().map(|id| p.bind(*id)).collect();
    let sql = format!(
        // Gate `is_finalized = TRUE`: a dispatched-but-unjudged submission carries
        // a current judgement with score NULL, which COALESCE would read as 0.0.
        // Without the gate, DISTINCT ON picks that in-flight submission as "last"
        // and the cell flips to 0 the instant a contestant submits again, then
        // restores when judging completes -- a live-standings flicker that wrongly
        // drops their score. Gated, "last" is the last FINALIZED submission, so a
        // cell holds its known score until the newer one actually finishes. This
        // also covers a rejudge whose new current judgement may carry a stale
        // non-NULL score before finalizing. Mirrors the SumBestSubtask gate and
        // the MaxSubmission `score IS NOT NULL` gate. `s.id DESC` breaks a
        // same-created_at tie deterministically (highest id = truly last).
        "SELECT DISTINCT ON (s.user_id, s.problem_id) \
                s.user_id, s.problem_id, \
                COALESCE(sj.score, 0.0) as score, \
                GREATEST(EXTRACT(EPOCH FROM (s.created_at - c.start_time))::bigint, 0) \
                  as elapsed_seconds \
         FROM submission s \
         JOIN contest c ON c.id = s.contest_id \
         JOIN submission_judgement sj \
           ON sj.submission_id = s.id \
          AND sj.is_current = TRUE \
          AND sj.is_finalized = TRUE \
          AND sj.judge_epoch = s.judge_epoch \
         WHERE s.contest_id = {} \
           AND s.user_id IN ({}) \
           AND s.problem_id IN ({}) \
         ORDER BY s.user_id, s.problem_id, s.created_at DESC, s.id DESC",
        contest_placeholder,
        user_placeholders.join(","),
        problem_placeholders.join(","),
    );
    let last_rows: Vec<ScoreboardSubmissionRow> = host.db.query_with_args(&sql, &p.into_args())?;

    let tokened_rows = if tokened_submission_ids.is_empty() {
        Vec::new()
    } else {
        let mut p = Params::new();
        let contest_placeholder = p.bind(contest_id);
        let user_placeholders: Vec<String> = user_ids.iter().map(|id| p.bind(*id)).collect();
        let problem_placeholders: Vec<String> = problem_ids.iter().map(|id| p.bind(*id)).collect();
        let tokened_placeholders: Vec<String> = tokened_submission_ids
            .iter()
            .map(|id| p.bind(*id))
            .collect();
        let sql = format!(
            // Same `is_finalized = TRUE` gate as the "last" query above: a tokened
            // submission that is in-flight (fresh) or rejudging must not fold a
            // COALESCE-0 / stale score into the Rust-side max below. Only its
            // finalized score should count toward the best tokened result.
            "SELECT s.user_id, s.problem_id, \
                    COALESCE(sj.score, 0.0) as score, \
                    GREATEST(EXTRACT(EPOCH FROM (s.created_at - c.start_time))::bigint, 0) \
                      as elapsed_seconds \
             FROM submission s \
             JOIN contest c ON c.id = s.contest_id \
             JOIN submission_judgement sj \
               ON sj.submission_id = s.id \
              AND sj.is_current = TRUE \
              AND sj.is_finalized = TRUE \
              AND sj.judge_epoch = s.judge_epoch \
             WHERE s.contest_id = {} \
               AND s.user_id IN ({}) \
               AND s.problem_id IN ({}) \
               AND s.id IN ({})",
            contest_placeholder,
            user_placeholders.join(","),
            problem_placeholders.join(","),
            tokened_placeholders.join(","),
        );
        host.db
            .query_with_args::<ScoreboardSubmissionRow>(&sql, &p.into_args())?
    };

    let mut last_by_cell: HashMap<(i32, i32), ScoreboardSubmissionRow> = HashMap::new();
    for row in last_rows {
        last_by_cell.insert((row.user_id, row.problem_id), row);
    }

    let mut tokened_by_cell: HashMap<(i32, i32), Vec<ScoreboardSubmissionRow>> = HashMap::new();
    for row in tokened_rows {
        tokened_by_cell
            .entry((row.user_id, row.problem_id))
            .or_default()
            .push(row);
    }

    let mut cells = HashMap::new();
    for &user_id in user_ids {
        for &problem_id in problem_ids {
            let key = (user_id, problem_id);
            let tokened_best = tokened_by_cell
                .get(&key)
                .and_then(|rows| rows.iter().map(|row| row.score).reduce(f64::max))
                .unwrap_or(0.0);
            let last_score = last_by_cell.get(&key).map(|row| row.score).unwrap_or(0.0);
            let score = score_best_tokened_or_last(tokened_best, last_score);
            let mut score_time_seconds = 0;
            if score > 0.0 {
                let mut eligible_times = Vec::new();
                if let Some(rows) = tokened_by_cell.get(&key) {
                    eligible_times.extend(
                        rows.iter()
                            .filter(|row| row.score >= score - SCORE_EPSILON)
                            .map(|row| row.elapsed_seconds),
                    );
                }
                if let Some(row) = last_by_cell.get(&key)
                    && row.score >= score - SCORE_EPSILON
                {
                    eligible_times.push(row.elapsed_seconds);
                }
                score_time_seconds = eligible_times.into_iter().min().unwrap_or(0);
            }
            cells.insert(
                key,
                ScoreboardCell {
                    score,
                    score_time_seconds,
                },
            );
        }
    }

    Ok(cells)
}

#[cfg(target_arch = "wasm32")]
fn load_sum_best_subtask_scoreboard_cells(
    host: &Host,
    contest_id: i32,
    user_ids: &[i32],
    problem_ids: &[i32],
) -> Result<HashMap<(i32, i32), ScoreboardCell>, SdkError> {
    if user_ids.is_empty() || problem_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut problem_subtasks: HashMap<i32, (Vec<TestCaseRow>, Vec<SubtaskDef>)> = HashMap::new();
    let mut test_case_meta: HashMap<(i32, i32), (String, f64)> = HashMap::new();
    for &problem_id in problem_ids {
        let task_config = load_task_config(host, contest_id, problem_id)?;
        let (test_cases, subtask_defs) = load_effective_subtasks(host, problem_id, &task_config)?;
        for test_case in &test_cases {
            test_case_meta.insert(
                (problem_id, test_case.id),
                (resolve_tc_label(test_case), test_case.score),
            );
        }
        problem_subtasks.insert(problem_id, (test_cases, subtask_defs));
    }

    let mut p = Params::new();
    let contest_placeholder = p.bind(contest_id);
    let user_placeholders: Vec<String> = user_ids.iter().map(|id| p.bind(*id)).collect();
    let problem_placeholders: Vec<String> = problem_ids.iter().map(|id| p.bind(*id)).collect();
    let sql = format!(
        "SELECT s.user_id, s.problem_id, s.id as submission_id, \
                tcr.test_case_id, tcr.score, tcr.verdict, \
                GREATEST(EXTRACT(EPOCH FROM (s.created_at - c.start_time))::bigint, 0) \
                  as elapsed_seconds \
         FROM submission s \
         JOIN contest c ON c.id = s.contest_id \
         JOIN test_case_result tcr ON tcr.submission_id = s.id \
         JOIN submission_judgement sj \
           ON sj.id = tcr.judgement_id \
          AND sj.submission_id = tcr.submission_id \
          AND sj.judge_epoch = tcr.judge_epoch \
         WHERE s.contest_id = {} \
           AND s.user_id IN ({}) \
           AND s.problem_id IN ({}) \
           AND tcr.test_case_id IS NOT NULL \
           AND sj.is_current = TRUE AND sj.is_finalized = TRUE \
         ORDER BY s.created_at ASC",
        contest_placeholder,
        user_placeholders.join(","),
        problem_placeholders.join(","),
    );
    let rows: Vec<ScoreboardTcScoreRow> = host.db.query_with_args(&sql, &p.into_args())?;

    let mut by_submission: HashMap<(i32, i32, i32), (i64, HashMap<String, f64>)> = HashMap::new();
    for row in rows {
        let Some((label, max_score)) = test_case_meta.get(&(row.problem_id, row.test_case_id))
        else {
            continue;
        };
        let raw_score = crate::score::normalized_raw_score(&row.verdict, row.score, *max_score);
        let (elapsed, scores) = by_submission
            .entry((row.user_id, row.problem_id, row.submission_id))
            .or_insert_with(|| (row.elapsed_seconds, HashMap::new()));
        *elapsed = (*elapsed).min(row.elapsed_seconds);
        scores.insert(label.clone(), raw_score);
    }

    let mut submissions_by_cell: HashMap<(i32, i32), Vec<(i64, HashMap<String, f64>)>> =
        HashMap::new();
    for ((user_id, problem_id, _submission_id), submission_scores) in by_submission {
        submissions_by_cell
            .entry((user_id, problem_id))
            .or_default()
            .push(submission_scores);
    }

    let mut cells = HashMap::new();
    for &user_id in user_ids {
        for &problem_id in problem_ids {
            let Some((test_cases, subtask_defs)) = problem_subtasks.get(&problem_id) else {
                continue;
            };
            let submissions = submissions_by_cell
                .get(&(user_id, problem_id))
                .cloned()
                .unwrap_or_default();
            // Single source of truth for the score with score.rs's task-score
            // recompute (`sum_best_subtask_score`); the loop below only ADDS
            // score-time tracking (WHEN each subtask's best was reached), which
            // the task-score path does not compute.
            let score = crate::score::sum_best_subtask_score(
                subtask_defs,
                test_cases,
                submissions.iter().map(|(_, tc_scores)| tc_scores),
            );

            let mut best_by_subtask: Vec<(f64, Option<i64>)> = Vec::new();
            for (elapsed_seconds, tc_scores) in submissions {
                let subtask_scores = score_all_subtasks(subtask_defs, test_cases, &tc_scores);
                for (idx, subtask) in subtask_scores.iter().enumerate() {
                    if best_by_subtask.len() <= idx {
                        best_by_subtask.resize(idx + 1, (0.0, None));
                    }
                    let (best_score, best_time) = &mut best_by_subtask[idx];
                    if subtask.score > *best_score + SCORE_EPSILON {
                        *best_score = subtask.score;
                        *best_time = Some(elapsed_seconds);
                    } else if (subtask.score - *best_score).abs() < SCORE_EPSILON
                        && subtask.score > 0.0
                        && best_time
                            .map(|elapsed| elapsed_seconds < elapsed)
                            .unwrap_or(true)
                    {
                        *best_time = Some(elapsed_seconds);
                    }
                }
            }
            let score_time_seconds = if score > 0.0 {
                best_by_subtask
                    .iter()
                    .filter(|(score, _)| *score > 0.0)
                    .filter_map(|(_, elapsed)| *elapsed)
                    .max()
                    .unwrap_or(0)
            } else {
                0
            };
            cells.insert(
                (user_id, problem_id),
                ScoreboardCell {
                    score,
                    score_time_seconds,
                },
            );
        }
    }

    Ok(cells)
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn load_scoreboard_cells(
    host: &Host,
    config: &ContestConfig,
    contest_id: i32,
    user_ids: &[i32],
    problem_ids: &[i32],
) -> Result<HashMap<(i32, i32), ScoreboardCell>, SdkError> {
    match config.scoring_mode {
        ScoringMode::MaxSubmission => {
            load_max_submission_scoreboard_cells(host, contest_id, user_ids, problem_ids)
        }
        ScoringMode::SumBestSubtask => {
            load_sum_best_subtask_scoreboard_cells(host, contest_id, user_ids, problem_ids)
        }
        ScoringMode::BestTokenedOrLast => {
            load_best_tokened_or_last_scoreboard_cells(host, contest_id, user_ids, problem_ids)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admins_can_view_full_scoreboard_in_any_phase() {
        for phase in ["before", "during", "after"] {
            assert!(full_scoreboard_visible_for_phase(
                phase,
                true,
                ScoreboardVisibility::AdminsOnly,
            ));
        }
    }

    #[test]
    fn combined_score_time_collapses_to_zero_under_equal_rank() {
        // EqualRank deliberately discards timing entirely, so every contestant
        // ties on the tiebreaker and ranking falls back to score alone.
        assert_eq!(
            combined_score_time_seconds(ScoreboardTiebreaker::EqualRank, &[10, 20, 30]),
            0
        );
    }

    #[test]
    fn combined_score_time_sums_or_maxes_according_to_the_tiebreaker() {
        let times = [10, 40, 25];
        assert_eq!(
            combined_score_time_seconds(ScoreboardTiebreaker::SumScoreTime, &times),
            75,
            "SumScoreTime must add every problem's score-time"
        );
        assert_eq!(
            combined_score_time_seconds(ScoreboardTiebreaker::MaxScoreTime, &times),
            40,
            "MaxScoreTime must take the single slowest, not the total"
        );
    }

    #[test]
    fn combined_score_time_of_no_solves_is_zero_for_every_tiebreaker() {
        // A contestant who has solved nothing has no score-times at all.
        // `MaxScoreTime` is the interesting one: `max()` on an empty slice is
        // `None`, and anything other than 0 here would rank a no-solve
        // contestant against solvers on a fabricated time.
        for tiebreaker in [
            ScoreboardTiebreaker::EqualRank,
            ScoreboardTiebreaker::SumScoreTime,
            ScoreboardTiebreaker::MaxScoreTime,
        ] {
            assert_eq!(combined_score_time_seconds(tiebreaker, &[]), 0);
        }
    }

    #[test]
    fn phase_after_reveals_full_scoreboard_even_without_view_all_or_all_contest_viewers() {
        assert!(full_scoreboard_visible_for_phase(
            "after",
            false,
            ScoreboardVisibility::AdminsOnly,
        ));
    }

    #[test]
    fn phase_before_and_during_stay_hidden_without_view_all_or_all_contest_viewers() {
        for phase in ["before", "during"] {
            assert!(!full_scoreboard_visible_for_phase(
                phase,
                false,
                ScoreboardVisibility::AdminsOnly,
            ));
        }
    }

    #[test]
    fn equal_rank_tiebreaker_ignores_score_time() {
        assert_eq!(
            compare_scoreboard_entries(
                100.0,
                600,
                "slow",
                100.0,
                60,
                "fast",
                ScoreboardTiebreaker::EqualRank
            ),
            std::cmp::Ordering::Greater
        );
        assert!(scoreboard_entries_tied(
            100.0,
            600,
            100.0,
            60,
            ScoreboardTiebreaker::EqualRank
        ));
    }

    #[test]
    fn time_tiebreakers_rank_faster_total_first() {
        assert_eq!(
            compare_scoreboard_entries(
                100.0,
                60,
                "fast",
                100.0,
                600,
                "slow",
                ScoreboardTiebreaker::MaxScoreTime
            ),
            std::cmp::Ordering::Less
        );
        assert!(!scoreboard_entries_tied(
            100.0,
            60,
            100.0,
            600,
            ScoreboardTiebreaker::MaxScoreTime
        ));
    }
}
