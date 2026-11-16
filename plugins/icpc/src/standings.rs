use std::collections::HashMap;

use broccoli_server_sdk::prelude::Verdict;

use crate::config::ProblemState;

#[derive(Debug, Clone)]
pub struct StandingsSubmission {
    pub submission_id: i32,
    pub user_id: i32,
    pub problem_id: i32,
    pub verdict: Option<Verdict>,
    pub status: String,
    pub elapsed_ms: i64,
}

pub fn compute_problem_states(
    submissions: &[StandingsSubmission],
    count_compile_error: bool,
) -> HashMap<(i32, i32), ProblemState> {
    let mut grouped: HashMap<(i32, i32), Vec<&StandingsSubmission>> = HashMap::new();
    for submission in submissions {
        grouped
            .entry((submission.user_id, submission.problem_id))
            .or_default()
            .push(submission);
    }

    grouped
        .into_iter()
        .map(|(key, mut rows)| {
            rows.sort_by_key(|row| (row.elapsed_ms, row.submission_id));

            let accepted_index = rows
                .iter()
                .position(|row| row.verdict == Some(Verdict::Accepted));
            let solved = accepted_index.is_some();
            let solve_time_ms = accepted_index.map(|index| rows[index].elapsed_ms.max(0));
            let count_until = accepted_index.unwrap_or(rows.len());
            let attempts = rows
                .iter()
                .take(count_until)
                .filter(|row| is_penalty_attempt(row, count_compile_error))
                .count() as i32;

            (
                key,
                ProblemState {
                    attempts,
                    solved,
                    solve_time_ms,
                },
            )
        })
        .collect()
}

/// Count PENDING submissions per (user, problem) made during the freeze window,
/// for the frozen contestant view. Only problems NOT already solved before the
/// freeze get pending markers (a solved problem stays solved; a frozen submission
/// on it is irrelevant). SystemError submissions are ignored - they are a judge
/// fault, not the team's attempt, and will be re-judged.
pub fn compute_pending(
    freeze_submissions: &[StandingsSubmission],
    pre_freeze_states: &HashMap<(i32, i32), ProblemState>,
) -> HashMap<(i32, i32), i32> {
    let mut pending: HashMap<(i32, i32), i32> = HashMap::new();
    for s in freeze_submissions {
        if s.status == "SystemError" || s.verdict == Some(Verdict::SystemError) {
            continue;
        }
        let key = (s.user_id, s.problem_id);
        let solved = pre_freeze_states
            .get(&key)
            .map(|st| st.solved)
            .unwrap_or(false);
        if !solved {
            *pending.entry(key).or_insert(0) += 1;
        }
    }
    pending
}

/// Whether a submission counts toward the LIVE (pre-freeze) standings - i.e. is
/// NOT frozen. True if it was made before the freeze window, OR it belongs to the
/// viewer themselves. A contestant always sees their own submissions un-frozen
/// (their own row shows real verdicts); only other teams' during-freeze
/// submissions are hidden as pending. `own_user_id` is None for anonymous /
/// organizer views (no self-exception, though organizers aren't frozen anyway).
pub fn counts_as_live(
    sub: &StandingsSubmission,
    freeze_start_ms: i64,
    own_user_id: Option<i32>,
) -> bool {
    sub.elapsed_ms < freeze_start_ms || Some(sub.user_id) == own_user_id
}

/// Owner of the "first to solve" balloon per problem: the contestant with the
/// earliest accepted solve among submissions made before `freeze_start_ms`
/// (`i64::MAX` when the board is not frozen), ties broken by the smaller
/// `user_id`. Returns `problem_id -> first-solver user_id`.
///
/// The balloon is decided only from what every viewer of the board may see. A
/// contestant's own submissions during the freeze stay live on their row, but
/// another team may have solved the same problem earlier inside the freeze, so
/// an in-freeze solve earns no balloon until the board is revealed.
///
/// `restricted` is the private-standings contestant view, which loaded only the
/// viewer's own submissions. First-solve is a property of every team's data,
/// and announcing "you were first" would itself leak that no one else solved
/// the problem earlier, so the restricted view gets no balloons at all.
pub fn compute_first_solvers(
    submissions: &[StandingsSubmission],
    freeze_start_ms: i64,
    count_compile_error: bool,
    restricted: bool,
) -> HashMap<i32, i32> {
    if restricted {
        return HashMap::new();
    }
    let visible_to_all: Vec<StandingsSubmission> = submissions
        .iter()
        .filter(|s| s.elapsed_ms < freeze_start_ms)
        .cloned()
        .collect();
    let states = compute_problem_states(&visible_to_all, count_compile_error);

    let mut best: HashMap<i32, (i64, i32)> = HashMap::new(); // problem_id -> (solve_ms, user_id)
    for (&(user_id, problem_id), state) in &states {
        if !state.solved {
            continue;
        }
        let solve_ms = state.solve_time_ms.unwrap_or(i64::MAX);
        best.entry(problem_id)
            .and_modify(|current| {
                if (solve_ms, user_id) < *current {
                    *current = (solve_ms, user_id);
                }
            })
            .or_insert((solve_ms, user_id));
    }

    best.into_iter()
        .map(|(problem_id, (_, user_id))| (problem_id, user_id))
        .collect()
}

fn is_penalty_attempt(row: &StandingsSubmission, count_compile_error: bool) -> bool {
    if row.status == "SystemError" || row.verdict == Some(Verdict::SystemError) {
        return false;
    }

    if row.status == "CompilationError" || row.verdict == Some(Verdict::CompileError) {
        return count_compile_error;
    }

    // Cancelled joins Skipped as a non-attempt: neither was a real judged
    // attempt, so neither incurs an ICPC penalty (consistent with
    // Verdict::is_skipped_or_cancelled used elsewhere). A submission-level
    // Cancelled is not written on the current ICPC path, so this is hardening
    // against a future change rather than a live bug.
    !matches!(
        row.verdict,
        Some(Verdict::Accepted) | Some(Verdict::Skipped) | Some(Verdict::Cancelled)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_wrong_judgement_overrides_stale_solved_storage_shape() {
        let states = compute_problem_states(
            &[StandingsSubmission {
                submission_id: 1,
                user_id: 1,
                problem_id: 100,
                verdict: Some(Verdict::WrongAnswer),
                status: "Judged".into(),
                elapsed_ms: 10 * 60_000,
            }],
            false,
        );

        let state = states.get(&(1, 100)).expect("state should exist");
        assert!(!state.solved);
        assert_eq!(state.attempts, 1);
        assert_eq!(state.solve_time_ms, None);
    }

    #[test]
    fn accepted_penalty_uses_submission_elapsed_time_and_prior_attempts() {
        let states = compute_problem_states(
            &[
                StandingsSubmission {
                    submission_id: 1,
                    user_id: 1,
                    problem_id: 100,
                    verdict: Some(Verdict::WrongAnswer),
                    status: "Judged".into(),
                    elapsed_ms: 3 * 60_000,
                },
                StandingsSubmission {
                    submission_id: 2,
                    user_id: 1,
                    problem_id: 100,
                    verdict: Some(Verdict::Accepted),
                    status: "Judged".into(),
                    elapsed_ms: 10 * 60_000,
                },
                StandingsSubmission {
                    submission_id: 3,
                    user_id: 1,
                    problem_id: 100,
                    verdict: Some(Verdict::WrongAnswer),
                    status: "Judged".into(),
                    elapsed_ms: 20 * 60_000,
                },
            ],
            false,
        );

        let state = states.get(&(1, 100)).expect("state should exist");
        assert!(state.solved);
        assert_eq!(state.attempts, 1);
        assert_eq!(state.solve_time_ms, Some(10 * 60_000));
    }

    #[test]
    fn compile_errors_follow_contest_counting_config() {
        let row = StandingsSubmission {
            submission_id: 1,
            user_id: 1,
            problem_id: 100,
            verdict: None,
            status: "CompilationError".into(),
            elapsed_ms: 60_000,
        };

        let ignored = compute_problem_states(std::slice::from_ref(&row), false);
        assert_eq!(ignored.get(&(1, 100)).unwrap().attempts, 0);

        let counted = compute_problem_states(&[row], true);
        assert_eq!(counted.get(&(1, 100)).unwrap().attempts, 1);
    }

    #[test]
    fn pending_counts_only_unsolved_and_ignores_system_error() {
        // pre-freeze: user 1 already solved problem 100.
        let pre = compute_problem_states(
            &[StandingsSubmission {
                submission_id: 1,
                user_id: 1,
                problem_id: 100,
                verdict: Some(Verdict::Accepted),
                status: "Judged".into(),
                elapsed_ms: 5 * 60_000,
            }],
            false,
        );
        // during freeze: one sub on the solved 100 (ignored), and on unsolved 200 a
        // WA (counts) + a SystemError (ignored).
        let freeze = vec![
            StandingsSubmission {
                submission_id: 2,
                user_id: 1,
                problem_id: 100,
                verdict: Some(Verdict::WrongAnswer),
                status: "Judged".into(),
                elapsed_ms: 290 * 60_000,
            },
            StandingsSubmission {
                submission_id: 3,
                user_id: 1,
                problem_id: 200,
                verdict: Some(Verdict::WrongAnswer),
                status: "Judged".into(),
                elapsed_ms: 291 * 60_000,
            },
            StandingsSubmission {
                submission_id: 4,
                user_id: 1,
                problem_id: 200,
                verdict: Some(Verdict::SystemError),
                status: "SystemError".into(),
                elapsed_ms: 292 * 60_000,
            },
        ];
        let pending = compute_pending(&freeze, &pre);
        assert_eq!(pending.get(&(1, 100)), None, "solved problem: no pending");
        assert_eq!(
            pending.get(&(1, 200)),
            Some(&1),
            "one pending (WA counts, SystemError ignored)"
        );
    }

    #[test]
    fn own_submissions_are_never_frozen() {
        let freeze_start = 240 * 60_000;
        let during = |uid: i32| StandingsSubmission {
            submission_id: uid,
            user_id: uid,
            problem_id: 1,
            verdict: Some(Verdict::Accepted),
            status: "Judged".into(),
            elapsed_ms: 250 * 60_000, // inside the freeze window
        };
        // Viewer = user 7: their own during-freeze submission stays live...
        assert!(counts_as_live(&during(7), freeze_start, Some(7)));
        // ...but another team's during-freeze submission is frozen.
        assert!(!counts_as_live(&during(8), freeze_start, Some(7)));
        // A pre-freeze submission is live for everyone.
        let pre = StandingsSubmission {
            elapsed_ms: 100 * 60_000,
            ..during(8)
        };
        assert!(counts_as_live(&pre, freeze_start, Some(7)));
        // Anonymous / organizer viewer: no self-exception.
        assert!(!counts_as_live(&during(7), freeze_start, None));
    }

    #[test]
    fn first_solver_is_the_globally_earliest_accepted() {
        // Two teams solved problem 100; user 8 solved it earlier than user 7.
        let first = compute_first_solvers(
            &[
                StandingsSubmission {
                    submission_id: 1,
                    user_id: 7,
                    problem_id: 100,
                    verdict: Some(Verdict::Accepted),
                    status: "Judged".into(),
                    elapsed_ms: 30 * 60_000,
                },
                StandingsSubmission {
                    submission_id: 2,
                    user_id: 8,
                    problem_id: 100,
                    verdict: Some(Verdict::Accepted),
                    status: "Judged".into(),
                    elapsed_ms: 10 * 60_000,
                },
                // user 7 also solved problem 200 (only solver) -> first there.
                StandingsSubmission {
                    submission_id: 3,
                    user_id: 7,
                    problem_id: 200,
                    verdict: Some(Verdict::Accepted),
                    status: "Judged".into(),
                    elapsed_ms: 40 * 60_000,
                },
            ],
            i64::MAX,
            false,
            false,
        );

        assert_eq!(first.get(&100), Some(&8), "user 8 solved 100 first");
        assert_eq!(
            first.get(&200),
            Some(&7),
            "user 7 is the only solver of 200"
        );
    }

    #[test]
    fn restricted_view_never_flags_a_first_solve() {
        // The private-standings restricted view loaded only the viewer's (user 7)
        // own submissions. Even though user 7 solved problem 100, first-solve is a
        // global property that someone else may already own - so the restricted
        // path must NOT crown the viewer. Suppressed entirely -> empty map.
        let first = compute_first_solvers(
            &[StandingsSubmission {
                submission_id: 1,
                user_id: 7,
                problem_id: 100,
                verdict: Some(Verdict::Accepted),
                status: "Judged".into(),
                elapsed_ms: 30 * 60_000,
            }],
            i64::MAX,
            false,
            true,
        );
        assert!(
            first.is_empty(),
            "restricted view must not flag any first solve, got {first:?}"
        );
    }

    #[test]
    fn an_in_freeze_solve_earns_no_balloon_while_frozen() {
        let accepted = |submission_id, user_id, elapsed_ms| StandingsSubmission {
            submission_id,
            user_id,
            problem_id: 100,
            verdict: Some(Verdict::Accepted),
            status: "Judged".into(),
            elapsed_ms,
        };
        let freeze_start = 240 * 60_000;
        // The viewer (user 7) solved inside the freeze, and its own solve is in
        // its live set; user 8 solved earlier, also inside the freeze.
        let viewers_live = [accepted(2, 7, 250 * 60_000)];
        let frozen = compute_first_solvers(&viewers_live, freeze_start, false, false);
        assert!(frozen.is_empty(), "no balloon while frozen: {frozen:?}");

        // Revealed: everyone's solves count and the true first solver wins.
        let all = [accepted(1, 8, 245 * 60_000), accepted(2, 7, 250 * 60_000)];
        let revealed = compute_first_solvers(&all, i64::MAX, false, false);
        assert_eq!(revealed.get(&100), Some(&8));
    }
}
