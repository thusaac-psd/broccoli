//! Runs [`machine::transition`] against stored state.
//!
//! [`drive`] reads the match, gathers what the transition needs (the server
//! clock, the open game's submissions, an unscheduled match's start time),
//! arms the requested wakeups, writes the new state with a compare-and-set,
//! and only then applies the post-commit effects. A lost write retries from
//! the fresh state; a failed effect returns an error so the triggering timer
//! is redelivered, and redelivery repeats the effects idempotently.

use broccoli_server_sdk::Host;
use broccoli_server_sdk::db::Params;
use broccoli_server_sdk::error::SdkError;
use broccoli_server_sdk::types::*;
use serde::Deserialize;

use crate::decide::{SubmissionLifecycle, SubmissionRecord};
use crate::machine::{self, Effect, Event, Inputs, MatchTimer, Rejection};
use crate::model::{MatchState, Phase, RoundDef, Setup};
use crate::timers::TimerKey;
use crate::{bracket, clock, feedback, stages, storage};

#[derive(Debug)]
pub enum DriveError {
    /// The bracket has not been set up, or the match was never created.
    NotFound,
    Rejected(Rejection),
    Sdk(SdkError),
}

impl From<SdkError> for DriveError {
    fn from(error: SdkError) -> Self {
        DriveError::Sdk(error)
    }
}

impl From<serde_json::Error> for DriveError {
    fn from(error: serde_json::Error) -> Self {
        DriveError::Sdk(error.into())
    }
}

/// Apply `event` to match `match_id` and return the committed state.
pub fn drive(
    host: &Host,
    contest: i32,
    match_id: u8,
    event: Event,
) -> Result<MatchState, DriveError> {
    let setup = storage::load_setup(host, contest)?.ok_or(DriveError::NotFound)?;
    let key = storage::match_key(contest, match_id);
    // Wakeups armed by attempts whose write was lost.
    let mut stray = Vec::new();
    for _ in 0..100 {
        let snapshot = host.storage.snapshot(&key)?.ok_or(DriveError::NotFound)?;
        let current: MatchState = serde_json::from_str(&snapshot.value)?;
        let start_at_ms = match (event, current.phase) {
            (Event::Tick, Phase::Ordering) if !setup.is_scheduled(current.round) => {
                unscheduled_start_at(host, contest, &setup, &current)?
            }
            _ => None,
        };
        let clock = clock::clock(host, contest)?;
        let subs = match (event, current.phase) {
            (Event::Tick, Phase::Playing | Phase::AwaitingJudge { .. }) => {
                match setup.round_for_match(current.round, current.pos) {
                    Some(round_def) => fetch_subs(host, contest, &current, &round_def)?,
                    None => Vec::new(),
                }
            }
            _ => Vec::new(),
        };
        let inputs = Inputs {
            setup: &setup,
            clock,
            subs: &subs,
            start_at_ms,
        };
        let transition =
            machine::transition(&current, event, &inputs).map_err(DriveError::Rejected)?;
        // Arm before writing: if this process dies between the write and the
        // effects, the new state's wakeup already exists. A lost write leaves
        // a spurious wakeup, which only ticks the match again; it is cancelled
        // below once a later attempt commits.
        for wakeup in &transition.wakeups {
            host.timer.schedule(
                wakeup.at_ms,
                &timer_key(contest, match_id, wakeup.timer),
                "",
            )?;
        }
        if transition.next != current
            && !host.storage.compare_and_set_snapshot(
                &key,
                Some(&snapshot),
                &serde_json::to_string(&transition.next)?,
            )?
        {
            stray.extend(transition.wakeups.iter().map(|w| w.timer));
            continue;
        }
        apply(
            host,
            contest,
            match_id,
            &setup,
            &transition.next,
            &transition.effects,
        )?;
        for timer in unneeded(&stray, &transition.wakeups) {
            host.timer.cancel(&timer_key(contest, match_id, timer))?;
        }
        return Ok(transition.next);
    }
    Err(SdkError::Other("Match update CAS retry limit exceeded".into()).into())
}

/// The wakeups lost attempts armed that the committed transition does not
/// need. A match only moves forward through its games, and an accepted
/// submission's settlement time only moves earlier as opponent errors are
/// judged, so a wakeup the committed state does not arm is not needed by any
/// later state either. `Closing` is kept: another process that closed the
/// match may still be retrying its closing work under that key.
fn unneeded(stray: &[MatchTimer], armed: &[machine::Wakeup]) -> Vec<MatchTimer> {
    let mut unneeded: Vec<MatchTimer> = Vec::new();
    for &timer in stray {
        if !matches!(timer, MatchTimer::Closing(_))
            && !armed.iter().any(|w| w.timer == timer)
            && !unneeded.contains(&timer)
        {
            unneeded.push(timer);
        }
    }
    unneeded
}

/// Tick match `match_id`: what every timer, judging result and stage
/// opening does. A match that does not exist has nothing to tick.
pub fn tick(host: &Host, contest: i32, match_id: u8) -> Result<(), SdkError> {
    match drive(host, contest, match_id, Event::Tick) {
        Ok(_) | Err(DriveError::NotFound) => Ok(()),
        Err(DriveError::Rejected(rejection)) => Err(SdkError::Other(rejection.to_string())),
        Err(DriveError::Sdk(error)) => Err(error),
    }
}

fn timer_key(contest: i32, match_id: u8, timer: MatchTimer) -> String {
    TimerKey::Match {
        contest,
        match_id,
        timer,
    }
    .encode()
}

/// Apply post-commit effects in order. Effective-time wakeups of settled
/// games are cancelled last, after everything else has succeeded.
fn apply(
    host: &Host,
    contest: i32,
    match_id: u8,
    setup: &Setup,
    m: &MatchState,
    effects: &[Effect],
) -> Result<(), SdkError> {
    let mut settled = Vec::new();
    for effect in effects {
        match *effect {
            Effect::PublishFeedback => settled = feedback::publish(host, contest, match_id, m)?,
            Effect::AdvanceBracket { winner } => {
                bracket::write_next_round_slots(host, contest, setup, m, winner)?;
                stages::try_open_stage(host, contest, m.round + 1)?;
            }
            Effect::Cancel(timer) => host.timer.cancel(&timer_key(contest, match_id, timer))?,
        }
    }
    for timer in settled {
        host.timer.cancel(&timer_key(contest, match_id, timer))?;
    }
    Ok(())
}

/// When a match in an unscheduled stage may start (see
/// [`bracket::start_at_ms`]).
fn unscheduled_start_at(
    host: &Host,
    contest: i32,
    setup: &Setup,
    m: &MatchState,
) -> Result<Option<i64>, SdkError> {
    if m.order_a.is_none() || m.order_b.is_none() {
        return Ok(None);
    }
    let matches = storage::load_all_matches(host, contest)?;
    let contest_start = clock::contest_times(host, contest)?.start_ms;
    Ok(bracket::start_at_ms(setup, &matches, m, contest_start))
}

#[derive(Debug, Deserialize)]
struct SubRow {
    id: i32,
    user_id: i32,
    problem_id: i32,
    submitted_at_us: i64,
    verdict: Option<Verdict>,
    status: SubmissionLifecycle,
}

/// Fetch every submission relevant to deciding `m`'s open 小局: both
/// players' submissions to their OWN current problem
/// ([`MatchState::current_problem`]) inside the game's window, scoped to
/// this contest.
///
/// Reads `status` alongside `verdict`: a submission whose judging failed
/// with a platform fault (`SystemError`) writes NO verdict, so
/// `verdict.is_none()` alone cannot tell "still being judged" apart from
/// "terminally failed" -- see `decide::submission_in_flight`. `status::text
/// AS status` reads the Postgres enum column as plain text, which
/// deserializes into [`SubmissionLifecycle`]. The effective time comes from
/// the admission metadata recorded when the submission was accepted.
fn fetch_subs(
    host: &Host,
    contest: i32,
    m: &MatchState,
    round_def: &RoundDef,
) -> Result<Vec<SubmissionRecord>, SdkError> {
    let Some(game) = m.xiaoju.last() else {
        return Ok(Vec::new());
    };
    let mut p = Params::new();
    let mut clauses = Vec::new();
    for player in [m.player_a, m.player_b] {
        if let Some(problem) = m.current_problem(round_def, player) {
            clauses.push(format!(
                "(s.user_id = {} AND s.problem_id = {})",
                p.bind(player),
                p.bind(problem)
            ));
        }
    }
    if clauses.is_empty() {
        return Ok(Vec::new());
    }

    let sql = format!(
        "SELECT s.id, s.user_id, s.problem_id, \
         (EXTRACT(EPOCH FROM s.created_at) * 1000000)::bigint AS submitted_at_us, verdict, \
         s.status::text AS status \
         FROM submission s \
         WHERE s.contest_id = {} AND ({}) AND s.created_at >= to_timestamp({}::double precision / 1000)",
        p.bind(contest),
        clauses.join(" OR "),
        p.bind(game.opened_at_ms)
    );
    let rows: Vec<SubRow> = host.db.query_with_args(&sql, &p.into_args())?;
    let feedback = host.submission.feedback_all(&SubmissionFeedbackQuery {
        submission_ids: Some(rows.iter().map(|r| r.id).collect()),
        ..Default::default()
    })?;
    let effective: std::collections::HashMap<i32, i64> = feedback
        .iter()
        .map(|r| feedback::effective_time(r).map(|time| (r.submission_id, time)))
        .collect::<Result<_, _>>()?;
    Ok(rows
        .into_iter()
        .map(|r| SubmissionRecord {
            submission_id: r.id,
            user_id: r.user_id,
            problem_id: r.problem_id,
            submitted_at_us: r.submitted_at_us,
            effective_at_us: effective.get(&r.id).copied().unwrap_or(r.submitted_at_us),
            verdict: r.verdict,
            status: r.status,
        })
        .filter(|s| game.contains_submission_at_us(s.submitted_at_us))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::Clock;
    use crate::machine::Closure;
    use crate::model::{Adjudication, XiaojuState};
    use crate::test_support::{queue_now, seed, setup_two_rounds};
    use crate::timers::handle_timer;

    fn key(timer: MatchTimer) -> String {
        timer_key(7, 0, timer)
    }

    fn match_in_progress() -> MatchState {
        MatchState {
            round: 1,
            pos: 0,
            player_a: 10,
            player_b: 20,
            group_a: vec![101, 102, 103],
            group_b: vec![201, 202, 203],
            order_a: Some(vec![103, 101, 102]),
            order_b: Some(vec![203, 201, 202]),
            phase: Phase::Playing,
            xiaoju: vec![XiaojuState {
                index: 0,
                deadline_ms: 1_000_000,
                base_deadline_ms: 1_000_000,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn queue_subs(host: &Host, rows: Vec<serde_json::Value>) {
        host.db.queue_query_result(serde_json::Value::Array(rows));
    }

    fn ac_row(user_id: i32, submitted_at_ms: i64) -> serde_json::Value {
        serde_json::json!({
            "id": user_id,
            "user_id": user_id,
            "problem_id": 0,
            "submitted_at_us": submitted_at_ms * 1000,
            "verdict": "Accepted",
            "status": "Judged",
        })
    }

    /// Player 10's accepted submission 10 in game `index`, admitted with an
    /// effective time of 30 s.
    fn feedback_record(index: u8) -> SubmissionFeedbackRecord {
        SubmissionFeedbackRecord {
            submission_id: 10,
            contest_id: 7,
            user_id: 10,
            problem_id: 103,
            submitted_at_us: 10_000_000,
            status: SubmissionLifecycle::Judged,
            verdict: Some(Verdict::Accepted),
            key: storage::match_key(7, 0),
            metadata: serde_json::json!({"scope_key":feedback::game_scope(7,0,index),"delay_seconds":20,"effective_at_us":null}),
            withheld: true,
            slot_held: true,
            slot_key: Some(feedback::player_slot(7, 10)),
            result: None,
        }
    }

    #[test]
    fn a_missing_setup_or_match_is_not_found_and_creates_nothing() {
        let host = Host::mock();
        assert!(matches!(
            drive(&host, 7, 0, Event::Tick),
            Err(DriveError::NotFound)
        ));
        crate::test_support::seed(&host, 7, &setup_two_rounds(), 1, &match_in_progress());
        assert!(matches!(
            drive(&host, 7, 0, Event::Tick),
            Err(DriveError::NotFound)
        ));
        assert!(storage::load_match(&host, 7, 0).unwrap().is_none());
        assert!(tick(&host, 7, 0).is_ok());
    }

    #[test]
    fn a_failed_publication_keeps_the_committed_decision_and_its_wakeups_until_a_retry_succeeds() {
        let host = Host::mock();
        let mut setup = setup_two_rounds();
        setup.rounds[1].break_before_seconds = 0;
        let mut m = match_in_progress();
        m.score_a = 1;
        m.xiaoju = (0..3)
            .map(|index| XiaojuState {
                index,
                decided: index < 2,
                winner: (index == 0).then_some(10),
                ..m.xiaoju[0].clone()
            })
            .collect();
        seed(&host, 7, &setup, 0, &m);
        for id in 1..8 {
            let mut other = m.clone();
            other.pos = id;
            other.player_a = 100 + i32::from(id) * 2;
            other.player_b = other.player_a + 1;
            other.phase = Phase::Decided {
                winner: other.player_a,
                decided_at_ms: 20_000,
            };
            seed(&host, 7, &setup, id, &other);
            bracket::write_next_round_slots(&host, 7, &setup, &other, other.player_a).unwrap();
        }
        let deadline = key(MatchTimer::Deadline { game: 2 });
        let effective = key(MatchTimer::Effective {
            game: 2,
            submission_id: 10,
            at_us: None,
        });
        host.timer.schedule(1_000_000, &deadline, "").unwrap();
        host.timer.schedule(30_000, &effective, "").unwrap();
        queue_now(&host, 30_000);
        queue_subs(&host, vec![ac_row(10, 10_000)]);
        host.submission.queue_feedback_query_success();
        host.submission
            .queue_feedback_query_error(SdkError::Other("publication unavailable".into()));
        assert!(handle_timer(&host, &effective, "", 30_000).is_err());
        let committed = storage::load_match(&host, 7, 0).unwrap().unwrap();
        assert_eq!(
            committed.phase,
            Phase::Decided {
                winner: 10,
                decided_at_ms: 30_000
            }
        );
        // The closing wakeup, armed before the decision was written, now
        // stands in for the game's deadline.
        assert!(!host.timer.is_scheduled(&deadline));
        assert_eq!(
            host.timer
                .scheduled_at(&key(MatchTimer::Closing(Closure::Decision))),
            Some(31_000)
        );
        assert_eq!(host.timer.scheduled_at(&effective), Some(31_000));
        assert!(
            host.storage
                .get_one(&storage::slot_key(7, 2, 0))
                .unwrap()
                .is_none()
        );
        assert!(storage::load_match(&host, 7, 8).unwrap().is_none());

        // Retries outlast the dispatcher's own delivery budget.
        for attempt in 1..=10 {
            host.submission
                .queue_feedback_query_error(SdkError::Other("still unavailable".into()));
            queue_now(&host, 30_000 + attempt * 1_000);
            let payload = format!(r#"{{"retry":{attempt}}}"#);
            assert!(handle_timer(&host, &effective, &payload, 30_000 + attempt * 1_000).is_err());
            assert!(
                host.timer
                    .is_scheduled(&key(MatchTimer::Closing(Closure::Decision)))
            );
            assert_eq!(
                host.timer.scheduled_at(&effective),
                Some(31_000 + attempt * 1_000)
            );
        }
        host.submission.set_feedback(vec![feedback_record(2)]);
        queue_now(&host, 41_000);
        host.db
            .queue_query_result(serde_json::json!([{ "start_ms": 0.0 }]));
        queue_now(&host, 41_000);
        handle_timer(&host, &effective, r#"{"retry":11}"#, 41_000).unwrap();
        for timer in [
            effective,
            deadline,
            key(MatchTimer::Closing(Closure::Decision)),
        ] {
            assert!(!host.timer.is_scheduled(&timer), "{timer}");
        }
        assert_eq!(
            storage::load_match(&host, 7, 0).unwrap().unwrap(),
            committed
        );
        assert_eq!(host.submission.feedback_actions().len(), 1);
        let next = storage::load_match(&host, 7, 8).unwrap().unwrap();
        assert_eq!((next.player_a, next.player_b), (10, 102));
    }

    #[test]
    fn a_staff_award_without_a_live_game_retries_its_closing_work() {
        let host = Host::mock();
        let mut m = match_in_progress();
        m.phase = Phase::NeedsAdjudication {
            adjudication: Adjudication::TiebreakExhausted,
        };
        seed(&host, 7, &setup_two_rounds(), 0, &m);
        queue_now(&host, 30_000);
        host.submission
            .queue_feedback_query_error(SdkError::Other("publication failed".into()));
        assert!(drive(&host, 7, 0, Event::Award { winner: 10 }).is_err());
        let closing = key(MatchTimer::Closing(Closure::Decision));
        assert_eq!(host.timer.scheduled_at(&closing), Some(31_000));
        assert_eq!(
            storage::load_match(&host, 7, 0).unwrap().unwrap().winner(),
            Some(10)
        );

        queue_now(&host, 31_000);
        handle_timer(&host, &closing, "", 31_000).unwrap();
        assert!(!host.timer.is_scheduled(&closing));
        assert_eq!(
            host.storage
                .get_one(&storage::slot_key(7, 2, 0))
                .unwrap()
                .as_deref(),
            Some("10")
        );
    }

    #[test]
    fn a_failed_feedback_update_keeps_the_effective_wakeup_and_does_not_double_score() {
        let host = Host::mock();
        seed(&host, 7, &setup_two_rounds(), 0, &match_in_progress());
        let effective = key(MatchTimer::Effective {
            game: 0,
            submission_id: 10,
            at_us: None,
        });
        host.timer.schedule(30_000, &effective, "").unwrap();
        queue_now(&host, 30_000);
        queue_subs(&host, vec![ac_row(10, 10_000)]);
        host.submission.set_feedback(vec![feedback_record(0)]);
        host.submission
            .queue_feedback_error(SdkError::Other("feedback update failed".into()));
        assert!(handle_timer(&host, &effective, "", 30_000).is_err());
        let committed = storage::load_match(&host, 7, 0).unwrap().unwrap();
        assert_eq!(committed.score_a, 1);
        assert_eq!(committed.xiaoju.len(), 2);
        assert!(host.timer.is_scheduled(&effective));
        let next_deadline = committed.xiaoju[1].deadline_ms;
        assert_eq!(
            host.timer
                .scheduled_at(&key(MatchTimer::Deadline { game: 1 })),
            Some(next_deadline)
        );

        queue_now(&host, 31_000);
        queue_subs(&host, vec![]);
        host.submission.set_feedback(vec![feedback_record(0)]);
        handle_timer(&host, &effective, r#"{"retry":1}"#, 31_000).unwrap();
        assert!(!host.timer.is_scheduled(&effective));
        assert_eq!(
            host.timer
                .scheduled_at(&key(MatchTimer::Deadline { game: 1 })),
            Some(next_deadline)
        );
        assert_eq!(
            storage::load_match(&host, 7, 0).unwrap().unwrap(),
            committed
        );
    }

    #[test]
    fn a_ranked_unscheduled_match_starts_once_the_contest_is_running() {
        let host = Host::mock();
        let mut m = match_in_progress();
        m.phase = Phase::Ordering;
        m.xiaoju.clear();
        seed(&host, 7, &setup_two_rounds(), 0, &m);

        host.db
            .queue_query_result(serde_json::json!([{ "start_ms": 60_000.0 }]));
        queue_now(&host, 5_000);
        let waiting = drive(&host, 7, 0, Event::Tick).unwrap();
        assert_eq!(waiting.phase, Phase::Ordering);
        assert_eq!(
            host.timer.scheduled_at(&key(MatchTimer::Start)),
            Some(60_000)
        );

        host.db
            .queue_query_result(serde_json::json!([{ "start_ms": 60_000.0 }]));
        queue_now(&host, 60_000);
        let started = drive(&host, 7, 0, Event::Tick).unwrap();
        assert_eq!(started.phase, Phase::Playing);
        assert_eq!(started.xiaoju[0].opened_at_ms, 60_000);
        assert!(!host.timer.is_scheduled(&key(MatchTimer::Start)));
        assert_eq!(
            host.timer
                .scheduled_at(&key(MatchTimer::Deadline { game: 0 })),
            Some(started.xiaoju[0].deadline_ms)
        );
    }

    #[test]
    fn a_match_missing_a_ranking_waits_without_a_wakeup_or_extra_reads() {
        let host = Host::mock();
        let mut m = match_in_progress();
        m.phase = Phase::Ordering;
        m.order_b = None;
        m.xiaoju.clear();
        seed(&host, 7, &setup_two_rounds(), 0, &m);
        queue_now(&host, 5_000);
        assert_eq!(drive(&host, 7, 0, Event::Tick).unwrap(), m);
        assert!(!host.timer.is_scheduled(&key(MatchTimer::Start)));
        assert_eq!(host.db.queries().len(), 1, "only the clock is read");
    }

    #[test]
    fn a_stale_cancel_from_an_older_commit_keeps_the_next_games_deadline() {
        let host = Host::mock();
        let setup = setup_two_rounds();
        let m = match_in_progress();
        seed(&host, 7, &setup, 0, &m);
        let deadline = m.xiaoju[0].deadline_ms;
        let judging = SubmissionRecord {
            submission_id: 5,
            user_id: 20,
            problem_id: 203,
            submitted_at_us: 5_000_000,
            effective_at_us: 5_000_000,
            verdict: None,
            status: SubmissionLifecycle::Running,
        };

        // X: the deadline tick sees the submission still judging and commits
        // the wait, but has not applied its effects yet.
        let key_x = storage::match_key(7, 0);
        let snapshot = host.storage.snapshot(&key_x).unwrap().unwrap();
        let x = machine::transition(
            &m,
            Event::Tick,
            &Inputs {
                setup: &setup,
                clock: Clock::at(deadline),
                subs: std::slice::from_ref(&judging),
                start_at_ms: None,
            },
        )
        .unwrap();
        assert!(matches!(x.next.phase, Phase::AwaitingJudge { .. }));
        assert!(
            x.effects
                .contains(&Effect::Cancel(MatchTimer::Deadline { game: 0 }))
        );
        assert!(
            host.storage
                .compare_and_set_snapshot(
                    &key_x,
                    Some(&snapshot),
                    &serde_json::to_string(&x.next).unwrap()
                )
                .unwrap()
        );

        // Y: the verdict lands, the game settles scoreless and the next game
        // opens with its own deadline wakeup.
        queue_now(&host, deadline + 1_000);
        queue_subs(
            &host,
            vec![serde_json::json!({
                "id": 5, "user_id": 20, "problem_id": 203, "submitted_at_us": 5_000_000,
                "verdict": "WrongAnswer", "status": "Judged",
            })],
        );
        let y = drive(&host, 7, 0, Event::Tick).unwrap();
        assert_eq!(y.xiaoju.len(), 2);
        let next_deadline = key(MatchTimer::Deadline { game: 1 });
        assert_eq!(
            host.timer.scheduled_at(&next_deadline),
            Some(y.xiaoju[1].deadline_ms)
        );

        // X's effects arrive last and touch only game 0's wakeup.
        apply(&host, 7, 0, &setup, &x.next, &x.effects).unwrap();
        assert_eq!(
            host.timer.scheduled_at(&next_deadline),
            Some(y.xiaoju[1].deadline_ms)
        );
    }

    #[test]
    fn a_closing_retry_that_keeps_failing_stops_after_its_retry_budget() {
        let host = Host::mock();
        let mut m = match_in_progress();
        m.phase = Phase::Decided {
            winner: 10,
            decided_at_ms: 1_000,
        };
        seed(&host, 7, &setup_two_rounds(), 0, &m);
        let closing = key(MatchTimer::Closing(Closure::Decision));
        let mut at = 2_000;
        host.timer.schedule(at, &closing, "").unwrap();
        for retry in 0..crate::timers::MAX_RETRIES {
            queue_now(&host, at);
            host.submission
                .queue_feedback_query_error(SdkError::Other("publication unavailable".into()));
            let payload = format!(r#"{{"retry":{retry}}}"#);
            assert!(handle_timer(&host, &closing, &payload, at).is_err());
            at += 1_000;
            assert_eq!(host.timer.scheduled_at(&closing), Some(at), "retry {retry}");
        }
        host.timer.cancel(&closing).unwrap();
        queue_now(&host, at);
        host.submission
            .queue_feedback_query_error(SdkError::Other("publication unavailable".into()));
        let payload = format!(r#"{{"retry":{}}}"#, crate::timers::MAX_RETRIES);
        assert!(handle_timer(&host, &closing, &payload, at).is_err());
        assert!(!host.timer.is_scheduled(&closing));
    }

    #[test]
    fn wakeups_armed_by_lost_writes_are_released_unless_the_commit_needs_them() {
        use machine::Wakeup;
        let deadline = |game| MatchTimer::Deadline { game };
        let closing = MatchTimer::Closing(Closure::Decision);
        let stray = [deadline(1), deadline(2), deadline(1), closing];
        let armed = [Wakeup {
            timer: deadline(2),
            at_ms: 5,
        }];
        assert_eq!(unneeded(&stray, &armed), [deadline(1)]);
    }
}
