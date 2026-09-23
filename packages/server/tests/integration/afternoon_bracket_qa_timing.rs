//! Exploratory QA for the 下午场 (afternoon-session) bracket plugin --
//! Areas 3 (timing/races) and 4 (setup/ordering abuse) of
//! `docs/superpowers/plans/2026-09-22-bracket-exploratory-qa.md`. Areas 1-2
//! (visibility, submission gate) are covered by `afternoon_bracket_qa.rs`;
//! area 5 (web UI) is out of scope for both files.
//!
//! Reuses `afternoon_bracket.rs`'s `pub(crate)` helpers
//! (`spawn_bracket_app`, `player`, `create_all_round_problems`, `get_match`,
//! `submit`, `XIAOJU_SECONDS`) rather than re-deriving them, same rationale
//! as `afternoon_bracket_qa.rs`. This file defines its OWN local `Fixture`
//! (not importable from `afternoon_bracket_qa.rs` -- that one is private to
//! its module) with one difference that matters for this file's scenarios:
//! `setup_fixture_with_timing` lets each test pick its own
//! `xiaoju_seconds` / `round_intermission_seconds` / `escalation_grace_seconds`
//! rather than hard-coding them, since several scenarios here specifically
//! need those knobs (a short escalation grace period so the AwaitingJudge
//! test does not take 120s; a non-zero intermission to probe the round-2
//! start gate; etc).
//!
//! Two classes of `#[ignore]`d tests below encode CONFIRMED defects rather
//! than being softened to pass -- see each test's `#[ignore = "..."]`
//! reason for the full repro and root cause. Passing tests are deliberately
//! kept in the suite too: a probe that confirms a rule IS enforced is
//! exactly as valuable as one that catches a leak.
//!
//! Findings are written up in
//! `.superpowers/sdd/2026-09-19-afternoon-bracket/qa-timing-setup.md`.

use crate::afternoon_bracket::{
    Player, RoundProblems, XIAOJU_SECONDS, bracket_route, create_all_round_problems, get_match,
    player, spawn_bracket_app, submit,
};
use crate::common::{TestApp, TestResponse, routes};
use broccoli_server_sdk::permissions as perm;
use chrono::{Duration as ChronoDuration, Utc};
use common::{SubmissionStatus, Verdict};
use sea_orm::{ActiveModelTrait, Set};
use serde_json::{Value, json};
use server::dispatcher::plugin_timer::{TimerConfig, tick_once};
use server::entity::submission;
use std::time::Duration;

// =====================================================================
// Shared fixture
// =====================================================================

struct Fixture {
    app: TestApp,
    _plugins_tmp: tempfile::TempDir,
    contest_id: i32,
    staff_token: String,
    rounds: Vec<RoundProblems>,
    players: Vec<Player>,
}

fn full_setup_body(
    rounds: &[RoundProblems],
    seeds: &[i32],
    xiaoju_seconds: i64,
    round_intermission_seconds: i64,
    escalation_grace_seconds: i64,
) -> Value {
    json!({
        "rounds": rounds.iter().map(|r| json!({
            "group_a": r.group_a,
            "group_b": r.group_b,
            "tiebreak": [r.tiebreak],
        })).collect::<Vec<_>>(),
        "xiaoju_seconds": xiaoju_seconds,
        "round_intermission_seconds": round_intermission_seconds,
        "escalation_grace_seconds": escalation_grace_seconds,
        "seeds": seeds,
    })
}

async fn setup_fixture_with_timing(
    xiaoju_seconds: i64,
    round_intermission_seconds: i64,
    escalation_grace_seconds: i64,
) -> Fixture {
    let (app, plugins_tmp) = spawn_bracket_app().await;

    let staff_token = app
        .create_user_with_permissions(
            "timing_qa_staff",
            "pass1234",
            &[
                perm::CONTEST_CREATE,
                perm::CONTEST_MANAGE,
                perm::PROBLEM_CREATE,
                perm::PROBLEM_EDIT,
            ],
        )
        .await;

    let mut players = Vec::with_capacity(16);
    for i in 0..16 {
        players.push(player(&app, &format!("timing_qa_player_{i}")).await);
    }

    let res = app
        .post_with_token(
            routes::CONTESTS,
            &json!({
                "title": "Afternoon Bracket Timing/Setup QA",
                "description": "Timing/races + setup-abuse exploratory QA contest",
                "activate_time": "2020-01-01T00:00:00Z",
                "start_time": "2020-01-01T00:00:00Z",
                "end_time": "2099-01-02T00:00:00Z",
                "is_public": true,
                "submissions_visible": true,
                "contest_type": "afternoon-bracket",
            }),
            &staff_token,
        )
        .await;
    assert_eq!(
        res.status, 201,
        "create bracket contest failed: {}",
        res.text
    );
    let contest_id = res.id();

    for p in &players {
        app.register_for_contest(contest_id, &p.token).await;
    }

    // `HookScope::Resource` only fires once an explicit config row enables
    // it -- same reasoning as `afternoon_bracket.rs`/`afternoon_bracket_qa.rs`.
    let res = app
        .put_with_token(
            &routes::contest_config_ns(contest_id, "afternoon-bracket", "before_submission"),
            &json!({"config": {}, "enabled": true, "position": 0}),
            &staff_token,
        )
        .await;
    assert_eq!(
        res.status, 200,
        "enable before_submission hook failed: {}",
        res.text
    );

    let rounds = create_all_round_problems(&app, contest_id, &staff_token).await;
    let seeds: Vec<i32> = players.iter().map(|p| p.id).collect();
    let res = app
        .post_with_token(
            &bracket_route(contest_id, "/setup"),
            &full_setup_body(
                &rounds,
                &seeds,
                xiaoju_seconds,
                round_intermission_seconds,
                escalation_grace_seconds,
            ),
            &staff_token,
        )
        .await;
    assert_eq!(res.status, 200, "bracket /setup failed: {}", res.text);

    Fixture {
        app,
        _plugins_tmp: plugins_tmp,
        contest_id,
        staff_token,
        rounds,
        players,
    }
}

async fn setup_fixture() -> Fixture {
    setup_fixture_with_timing(XIAOJU_SECONDS, 0, 120).await
}

/// Rank match `match_id` both directions: `b` (imposed order goes onto `a`)
/// ranks `round.group_a`, `a` ranks `round.group_b`. Caller must pass the
/// ACTUAL `player_a`/`player_b` of that match (see
/// `MatchState::order_a`/`order_b`'s doc comment on this direction being
/// the single most likely bug to get backwards).
async fn order_match(fx: &Fixture, match_id: u8, round: &RoundProblems, a: &Player, b: &Player) {
    let res = fx
        .app
        .post_with_token(
            &bracket_route(fx.contest_id, &format!("/matches/{match_id}/order")),
            &json!({"order": round.group_a}),
            &b.token,
        )
        .await;
    assert_eq!(
        res.status, 200,
        "B's ranking of A's problems (match {match_id}) failed: {}",
        res.text
    );
    let res = fx
        .app
        .post_with_token(
            &bracket_route(fx.contest_id, &format!("/matches/{match_id}/order")),
            &json!({"order": round.group_b}),
            &a.token,
        )
        .await;
    assert_eq!(
        res.status, 200,
        "A's ranking of B's problems (match {match_id}) failed: {}",
        res.text
    );
}

async fn order_match_0(fx: &Fixture) {
    order_match(fx, 0, &fx.rounds[0], &fx.players[0], &fx.players[1]).await;
}

async fn start_match(fx: &Fixture, match_id: u8) -> TestResponse {
    fx.app
        .post_with_token(
            &bracket_route(fx.contest_id, &format!("/matches/{match_id}/start")),
            &json!({}),
            &fx.staff_token,
        )
        .await
}

async fn start_match_0(fx: &Fixture) {
    let res = start_match(fx, 0).await;
    assert_eq!(res.status, 200, "starting match 0 failed: {}", res.text);
}

async fn force_decide(fx: &Fixture, match_id: u8, winner: Option<i32>) -> TestResponse {
    let body = match winner {
        Some(w) => json!({"winner": w}),
        None => json!({}),
    };
    fx.app
        .post_with_token(
            &bracket_route(fx.contest_id, &format!("/matches/{match_id}/force-decide")),
            &body,
            &fx.staff_token,
        )
        .await
}

/// Insert a submission row directly into the `submission` table, bypassing
/// the real HTTP submit path and the real judging pipeline entirely. Used
/// to construct submissions that are deterministically "still in flight"
/// (no real evaluator will ever touch a row it never dispatched) or
/// "already judged" (to control submitted-vs-judged ordering precisely,
/// rather than racing two real submissions through a fast fixture judge).
#[allow(clippy::too_many_arguments)]
async fn insert_submission(
    fx: &Fixture,
    user_id: i32,
    problem_id: i32,
    status: SubmissionStatus,
    verdict: Option<Verdict>,
    created_at: chrono::DateTime<Utc>,
) -> submission::Model {
    submission::ActiveModel {
        files: Set(json!([{"filename": "main.cpp", "content": "ACCEPT"}])),
        language: Set("cpp".into()),
        user_id: Set(user_id),
        problem_id: Set(problem_id),
        contest_id: Set(Some(fx.contest_id)),
        contest_type: Set("afternoon-bracket".into()),
        status: Set(status),
        judged_at: Set(if verdict.is_some() {
            Some(created_at)
        } else {
            None
        }),
        verdict: Set(verdict),
        created_at: Set(created_at),
        ..Default::default()
    }
    .insert(&fx.app.db)
    .await
    .expect("insert directly-constructed submission")
}

/// Build a contest + rounds ready for a `/setup` call, without seeding
/// players into the bracket via `/setup` itself -- for tests that probe
/// `/setup`'s OWN validation, where the interesting part is the request
/// body sent to `/setup`, not a fully-running match.
async fn contest_and_rounds_for_setup_probe(
    title: &str,
) -> (
    TestApp,
    tempfile::TempDir,
    String,
    i32,
    Vec<RoundProblems>,
    Vec<Player>,
) {
    let (app, tmp) = spawn_bracket_app().await;
    let staff_token = app
        .create_user_with_permissions(
            "setup_probe_staff",
            "pass1234",
            &[
                perm::CONTEST_CREATE,
                perm::CONTEST_MANAGE,
                perm::PROBLEM_CREATE,
                perm::PROBLEM_EDIT,
            ],
        )
        .await;
    let mut players = Vec::with_capacity(16);
    for i in 0..16 {
        players.push(player(&app, &format!("setup_probe_player_{i}")).await);
    }
    let res = app
        .post_with_token(
            routes::CONTESTS,
            &json!({
                "title": title,
                "description": "setup-abuse QA contest",
                "activate_time": "2020-01-01T00:00:00Z",
                "start_time": "2020-01-01T00:00:00Z",
                "end_time": "2099-01-02T00:00:00Z",
                "is_public": true,
                "submissions_visible": true,
                "contest_type": "afternoon-bracket",
            }),
            &staff_token,
        )
        .await;
    assert_eq!(res.status, 201, "create contest failed: {}", res.text);
    let contest_id = res.id();
    for p in &players {
        app.register_for_contest(contest_id, &p.token).await;
    }
    let rounds = create_all_round_problems(&app, contest_id, &staff_token).await;
    (app, tmp, staff_token, contest_id, rounds, players)
}

// =====================================================================
// Area 3 -- timing and races
// =====================================================================

/// Pins the design's own required "submission-time ordering" test
/// (`docs/superpowers/specs/2026-09-19-afternoon-bracket-design.md`): B's AC
/// lands (already judged) while A's EARLIER submission is still in flight --
/// `decide_xiaoju` must report "not yet", not award B the 小局 just because
/// B's verdict resolved first. Once A's older submission also resolves
/// Accepted, A must win. Uses direct DB inserts (rather than racing two real
/// submissions through the fixture judge) so the submitted-vs-judged
/// ordering is exact and reproducible, not a coin flip against a fast fixture
/// evaluator.
#[tokio::test]
async fn earliest_submitted_wins_and_the_intermediate_awaiting_state_is_pinned() {
    let fx = setup_fixture().await;
    order_match_0(&fx).await;
    start_match_0(&fx).await;

    let a = &fx.players[0];
    let b = &fx.players[1];
    let prob_a = fx.rounds[0].group_a[0];
    let prob_b = fx.rounds[0].group_b[0];

    let t0 = Utc::now();
    let t1 = t0 + ChronoDuration::milliseconds(50);

    // A submits FIRST (t0) but is still being judged.
    let a_sub = insert_submission(&fx, a.id, prob_a, SubmissionStatus::Running, None, t0).await;
    // B submits SECOND (t1) but is ALREADY judged Accepted.
    insert_submission(
        &fx,
        b.id,
        prob_b,
        SubmissionStatus::Judged,
        Some(Verdict::Accepted),
        t1,
    )
    .await;

    // Nudge `advance` well before the deadline: must report the blocked
    // intermediate state, not award B the 小局.
    let res = force_decide(&fx, 0, None).await;
    assert_eq!(
        res.status, 200,
        "advance (pre-deadline) failed: {}",
        res.text
    );

    let view = get_match(&fx.app, fx.contest_id, 0, &fx.staff_token).await;
    assert_eq!(
        view["state"], "in_progress",
        "must stay in_progress/undecided while A's earlier submission is unresolved, even \
         though B's later submission already has a confirmed AC, got {view}"
    );
    assert_eq!(view["score_a"].as_i64(), Some(0));
    assert_eq!(view["score_b"].as_i64(), Some(0));

    // A's verdict now lands: Accepted, still earlier-submitted than B's.
    let mut update: submission::ActiveModel = a_sub.into();
    update.status = Set(SubmissionStatus::Judged);
    update.verdict = Set(Some(Verdict::Accepted));
    update.judged_at = Set(Some(t0));
    update
        .update(&fx.app.db)
        .await
        .expect("update A's submission to Accepted");

    let res = force_decide(&fx, 0, None).await;
    assert_eq!(
        res.status, 200,
        "advance (post-verdict) failed: {}",
        res.text
    );

    let view = get_match(&fx.app, fx.contest_id, 0, &fx.staff_token).await;
    assert_eq!(
        view["score_a"].as_i64(),
        Some(1),
        "A submitted earlier and must win the 小局 despite B's AC landing (being judged) \
         first, got {view}"
    );
    assert_eq!(view["score_b"].as_i64(), Some(0));
}

/// Staff force-decides while A's submission is mid-judge (unresolved); the
/// override must stick even once A's verdict later lands.
#[tokio::test]
async fn force_decide_overrides_a_match_with_a_submission_mid_judge_and_a_later_verdict_does_not_reopen_it()
 {
    let fx = setup_fixture().await;
    order_match_0(&fx).await;
    start_match_0(&fx).await;

    let a = &fx.players[0];
    let b = &fx.players[1];
    let prob_a = fx.rounds[0].group_a[0];

    let a_sub = insert_submission(
        &fx,
        a.id,
        prob_a,
        SubmissionStatus::Running,
        None,
        Utc::now(),
    )
    .await;

    let res = force_decide(&fx, 0, Some(b.id)).await;
    assert_eq!(
        res.status, 200,
        "force-decide while A's submission is mid-judge failed: {}",
        res.text
    );

    let view = get_match(&fx.app, fx.contest_id, 0, &fx.staff_token).await;
    assert_eq!(view["state"], "decided");
    assert_eq!(view["winner"].as_i64(), Some(b.id as i64));

    // A's verdict finally lands, Accepted. The real trigger for a landing
    // verdict is the judging callback; the closest host-wired equivalent
    // reachable over HTTP without touching plugin source is force-decide
    // with no winner (`judge::advance`).
    let mut update: submission::ActiveModel = a_sub.into();
    update.status = Set(SubmissionStatus::Judged);
    update.verdict = Set(Some(Verdict::Accepted));
    update
        .update(&fx.app.db)
        .await
        .expect("land A's late verdict");

    let res = force_decide(&fx, 0, None).await;
    assert_eq!(
        res.status, 200,
        "advance after a late verdict failed: {}",
        res.text
    );

    let view = get_match(&fx.app, fx.contest_id, 0, &fx.staff_token).await;
    assert_eq!(
        view["state"], "decided",
        "a late-landing verdict must not reopen a force-decided match"
    );
    assert_eq!(
        view["winner"].as_i64(),
        Some(b.id as i64),
        "staff's override must not be clobbered by a verdict landing afterwards"
    );
}

/// Two concurrent force-decides for the same match: exactly one must win,
/// the match must never end up corrupted (no winner, or a state neither
/// caller asked for).
#[tokio::test]
async fn two_concurrent_force_decides_never_corrupt_match_state() {
    let fx = setup_fixture().await;
    order_match_0(&fx).await;
    start_match_0(&fx).await;

    let a_id = fx.players[0].id;
    let b_id = fx.players[1].id;

    let (r1, r2) = tokio::join!(
        force_decide(&fx, 0, Some(a_id)),
        force_decide(&fx, 0, Some(b_id)),
    );

    let successes = [r1.status, r2.status]
        .into_iter()
        .filter(|s| *s == 200)
        .count();
    assert_eq!(
        successes, 1,
        "exactly one of two concurrent force-decides must win; got statuses {} / {} \
         (bodies: {} / {})",
        r1.status, r2.status, r1.text, r2.text
    );

    let view = get_match(&fx.app, fx.contest_id, 0, &fx.staff_token).await;
    assert_eq!(view["state"], "decided");
    let winner = view["winner"].as_i64().expect("a winner must be recorded");
    assert!(
        winner == a_id as i64 || winner == b_id as i64,
        "winner must be exactly one of the two participants, got {winner} (view: {view})"
    );
}

/// NUISANCE finding, not a correctness bug: the losing side of a concurrent
/// force-decide gets a bare 500, not a 4xx conflict response.
#[tokio::test]
async fn defect_the_losing_side_of_a_concurrent_force_decide_gets_a_bare_500_not_a_4xx() {
    let fx = setup_fixture().await;
    order_match_0(&fx).await;
    start_match_0(&fx).await;

    let a_id = fx.players[0].id;
    let b_id = fx.players[1].id;

    let (r1, r2) = tokio::join!(
        force_decide(&fx, 0, Some(a_id)),
        force_decide(&fx, 0, Some(b_id)),
    );
    let loser = if r1.status == 200 { &r2 } else { &r1 };
    assert!(
        loser.status < 500,
        "the losing side of a concurrent force-decide should be a 4xx conflict, not a {} \
         (body: {})",
        loser.status,
        loser.text
    );
}

/// Verifies (rather than trusts) the per-slot-key design claim: two sibling
/// round-1 matches decided AT THE SAME TIME both land in the shared round-2
/// slot -- neither winner is lost to a lost CAS retry.
#[tokio::test]
async fn two_sibling_matches_decided_simultaneously_both_land_in_the_shared_round2_slot() {
    let fx = setup_fixture().await;
    order_match_0(&fx).await;
    start_match_0(&fx).await;
    order_match(&fx, 1, &fx.rounds[0], &fx.players[2], &fx.players[3]).await;
    let res = start_match(&fx, 1).await;
    assert_eq!(res.status, 200, "starting match 1 failed: {}", res.text);

    let winner0 = fx.players[0].id;
    let winner1 = fx.players[2].id;
    let (r0, r1) = tokio::join!(
        force_decide(&fx, 0, Some(winner0)),
        force_decide(&fx, 1, Some(winner1)),
    );
    assert_eq!(r0.status, 200, "force-decide match 0 failed: {}", r0.text);
    assert_eq!(r1.status, 200, "force-decide match 1 failed: {}", r1.text);

    // Round 2 match id 8 = storage::match_id_for(2, 0), fed by round-1
    // matches 0 and 1 (pos/2 == 0 for both).
    let round2 = get_match(&fx.app, fx.contest_id, 8, &fx.staff_token).await;
    let a = round2["player_a"].as_i64();
    let b = round2["player_b"].as_i64();
    let present = [a, b];
    assert!(
        present.contains(&Some(winner0 as i64)) && present.contains(&Some(winner1 as i64)),
        "both sibling winners must land in round 2's shared slot; got player_a={a:?} \
         player_b={b:?} (design claim under test: per-slot storage keys make this race \
         structurally impossible, not merely unlikely)"
    );
}

/// Drives the full `AwaitingJudge` -> escalation -> `NeedsAdjudication` path
/// end to end via `server::dispatcher::plugin_timer::tick_once`, matching
/// `tests/integration/plugin_timer.rs`'s pattern, rather than sleeping and
/// polling for the background dispatcher loop to get around to it. The only
/// sleeps here wait out the UNAVOIDABLE real wall-clock deadlines
/// (`xiaoju_seconds`, `escalation_grace_seconds` are computed from real
/// `NOW()`); delivery itself is always forced deterministically via
/// `tick_once` immediately afterwards.
#[tokio::test]
async fn awaiting_judge_end_to_end_then_escalates_to_needs_adjudication() {
    let fx = setup_fixture_with_timing(3, 0, 2).await;
    order_match_0(&fx).await;
    start_match_0(&fx).await;

    let a = &fx.players[0];
    let b = &fx.players[1];
    let prob_a = fx.rounds[0].group_a[0];
    let prob_b = fx.rounds[0].group_b[0];

    // A's submission never resolves -- inserted directly so no real
    // evaluator is ever dispatched for it.
    let stuck = insert_submission(
        &fx,
        a.id,
        prob_a,
        SubmissionStatus::Running,
        None,
        Utc::now(),
    )
    .await;

    // B gets a REAL, confirmed AC through the actual judging pipeline --
    // this is what makes A's stuck submission a BLOCKER rather than simply
    // the only submission on the board (see the separate defect test for
    // that other case).
    let res = submit(&fx.app, fx.contest_id, prob_b, &b.token, "ACCEPT").await;
    assert_eq!(res.status, 201, "B's submission failed: {}", res.text);

    tokio::time::sleep(Duration::from_millis(500)).await;
    let res = force_decide(&fx, 0, None).await;
    assert_eq!(
        res.status, 200,
        "advance (pre-deadline) failed: {}",
        res.text
    );
    let view = get_match(&fx.app, fx.contest_id, 0, &fx.staff_token).await;
    assert_eq!(
        view["state"], "in_progress",
        "must stay in_progress (blocked, not yet decided) while A's older submission is \
         still in flight and the deadline has not passed yet, got {view}"
    );
    assert_eq!(view["score_a"].as_i64(), Some(0));
    assert_eq!(view["score_b"].as_i64(), Some(0));

    // Wait out the unavoidable real 3s deadline, then force delivery
    // deterministically.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let config = TimerConfig::default();
    tick_once(&fx.app.state, &config)
        .await
        .expect("tick_once (xiaoju deadline) failed");

    let view = get_match(&fx.app, fx.contest_id, 0, &fx.staff_token).await;
    assert_eq!(
        view["state"], "awaiting_judge",
        "the deadline passed while blocked on A's still-in-flight submission with B's AC \
         on record -- must enter awaiting_judge, got {view}"
    );
    assert_eq!(
        view["awaiting_submission_id"].as_i64(),
        Some(stuck.id as i64),
        "the blocking submission id exposed to staff must be A's stuck submission"
    );

    // Wait out the unavoidable real 2s escalation grace period, then force
    // delivery deterministically again.
    tokio::time::sleep(Duration::from_secs(2)).await;
    tick_once(&fx.app.state, &config)
        .await
        .expect("tick_once (escalation) failed");

    let view = get_match(&fx.app, fx.contest_id, 0, &fx.staff_token).await;
    assert_eq!(
        view["state"], "needs_adjudication",
        "a match still blocked after the escalation grace period must escalate for staff, \
         got {view}"
    );
}

/// DEFECT: `decide_xiaoju`'s `best_ac == None` branch never checks for an
/// in-flight submission before declaring the 小局 scoreless.
#[tokio::test]
async fn defect_a_lone_in_flight_submission_with_no_competing_ac_is_silently_scored_scoreless_at_the_deadline()
 {
    let fx = setup_fixture().await;
    order_match_0(&fx).await;
    start_match_0(&fx).await;

    let a = &fx.players[0];
    let prob_a = fx.rounds[0].group_a[0];

    // A submits well before the deadline; still in flight (no verdict at
    // all) when the deadline passes. B never submits anything -- best_ac
    // is None on both sides.
    let stuck = insert_submission(
        &fx,
        a.id,
        prob_a,
        SubmissionStatus::Running,
        None,
        Utc::now(),
    )
    .await;

    tokio::time::sleep(Duration::from_secs(XIAOJU_SECONDS as u64 + 1)).await;
    let config = TimerConfig::default();
    tick_once(&fx.app.state, &config)
        .await
        .expect("tick_once (xiaoju deadline) failed");

    let view = get_match(&fx.app, fx.contest_id, 0, &fx.staff_token).await;
    assert_eq!(
        view["state"], "awaiting_judge",
        "expected awaiting_judge (A's in-flight submission must block a scoreless decision, \
         per the platform-fault policy), got {view}"
    );
    assert_eq!(
        view["awaiting_submission_id"].as_i64(),
        Some(stuck.id as i64)
    );
}

/// A stale xiaoju-N deadline timer firing after that 小局 already resolved
/// early (on a real AC) must be a no-op -- the module doc comment's "stale
/// key, discarded index" contract, not a premature decision of the NEXT
/// 小局. Uses a wide xiaoju_seconds window and an artificial delay before
/// submitting so the stale timer's original fire time sits comfortably
/// before the next 小局's own fresh deadline.
#[tokio::test]
async fn a_stale_xiaoju_deadline_timer_after_early_resolution_is_a_noop() {
    let fx = setup_fixture_with_timing(6, 0, 120).await;
    order_match_0(&fx).await;
    start_match_0(&fx).await;

    let a = &fx.players[0];
    let prob_a = fx.rounds[0].group_a[0];

    // ~2s in, 小局 0 resolves early via a real AC -- well ahead of its
    // original 6s deadline. The ORIGINAL xiaoju-0 timer still fires at
    // (match start + 6s); xiaoju-1 (opened at ~2s) has its own fresh
    // deadline at ~(2s + 6s) = ~8s, a ~2s margin after the stale fire.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let res = submit(&fx.app, fx.contest_id, prob_a, &a.token, "ACCEPT").await;
    assert_eq!(res.status, 201, "A's submission failed: {}", res.text);

    tokio::time::sleep(Duration::from_millis(500)).await;
    let after_early_decision = get_match(&fx.app, fx.contest_id, 0, &fx.staff_token).await;
    assert_eq!(after_early_decision["score_a"].as_i64(), Some(1));
    assert_eq!(after_early_decision["state"], "in_progress");
    assert_eq!(
        after_early_decision["current_xiaoju_index"].as_i64(),
        Some(1),
        "xiaoju 1 should now be open, got {after_early_decision}"
    );

    // Wait until just past the ORIGINAL (now stale) xiaoju-0 deadline
    // (match start + 6s) and force delivery deterministically.
    tokio::time::sleep(Duration::from_millis(3_800)).await; // 2s + 0.5s already elapsed -> land at ~6.3s
    let config = TimerConfig::default();
    tick_once(&fx.app.state, &config)
        .await
        .expect("tick_once (stale xiaoju-0 timer) failed");

    let after_stale_fire = get_match(&fx.app, fx.contest_id, 0, &fx.staff_token).await;
    assert_eq!(
        after_stale_fire["state"], "in_progress",
        "a stale xiaoju-0 timer firing after xiaoju-0 already resolved early must be a \
         no-op, not decide xiaoju-1 prematurely, got {after_stale_fire}"
    );
    assert_eq!(after_stale_fire["score_a"].as_i64(), Some(1));
    assert_eq!(after_stale_fire["score_b"].as_i64(), Some(0));
    assert_eq!(
        after_stale_fire["current_xiaoju_index"].as_i64(),
        Some(1),
        "xiaoju 1 must still be the open one, undecided"
    );
    assert_eq!(
        after_stale_fire["current_xiaoju_deadline_ms"],
        after_early_decision["current_xiaoju_deadline_ms"],
        "xiaoju 1's own deadline must be unaffected by the stale xiaoju-0 timer"
    );
}

// =====================================================================
// Area 4 -- setup and ordering abuse
// =====================================================================

/// DEFECT: `/setup` called a second time, mid-contest, silently
/// reconfigures an already-in-progress match's pacing.
#[tokio::test]
async fn defect_setup_called_twice_silently_reconfigures_an_in_progress_matchs_timing() {
    let fx = setup_fixture_with_timing(3, 0, 120).await;
    order_match_0(&fx).await;
    start_match_0(&fx).await;

    let seeds: Vec<i32> = fx.players.iter().map(|p| p.id).collect();
    let res = fx
        .app
        .post_with_token(
            &bracket_route(fx.contest_id, "/setup"),
            &full_setup_body(&fx.rounds, &seeds, 300, 0, 120),
            &fx.staff_token,
        )
        .await;
    assert_eq!(res.status, 200, "second /setup call failed: {}", res.text);

    let a = &fx.players[0];
    let prob_a = fx.rounds[0].group_a[0];
    let res = submit(&fx.app, fx.contest_id, prob_a, &a.token, "ACCEPT").await;
    assert_eq!(res.status, 201, "A's submission failed: {}", res.text);
    tokio::time::sleep(Duration::from_millis(500)).await;

    let view = get_match(&fx.app, fx.contest_id, 0, &fx.staff_token).await;
    assert_eq!(
        view["current_xiaoju_index"].as_i64(),
        Some(1),
        "xiaoju 1 should now be open, got {view}"
    );
    let opened_at = view["current_xiaoju_opened_at_ms"]
        .as_i64()
        .expect("xiaoju 1 should have an opened_at");
    let deadline = view["current_xiaoju_deadline_ms"]
        .as_i64()
        .expect("xiaoju 1 should have a deadline");
    assert_eq!(
        deadline - opened_at,
        3_000,
        "expected 小局 1 (opened WITHIN the same already-started match) to keep the \
         xiaoju_seconds=3 pacing the match actually started under -- got a {}-ms window, \
         meaning the second /setup call's xiaoju_seconds=300 silently applied to a live \
         match mid-play (view: {view})",
        deadline - opened_at
    );
}

/// DEFECT: a ranking can be resubmitted after the match has left the
/// Ordering phase.
#[tokio::test]
async fn defect_ranking_can_be_resubmitted_after_the_match_has_started() {
    let fx = setup_fixture().await;
    order_match_0(&fx).await;
    start_match_0(&fx).await;

    let before = get_match(&fx.app, fx.contest_id, 0, &fx.staff_token).await;
    assert_eq!(before["state"], "in_progress");

    let b = &fx.players[1];
    let original = fx.rounds[0].group_a;
    let swapped = [original[2], original[0], original[1]];
    let res = fx
        .app
        .post_with_token(
            &bracket_route(fx.contest_id, "/matches/0/order"),
            &json!({"order": swapped}),
            &b.token,
        )
        .await;
    assert_ne!(
        res.status, 200,
        "expected a resubmitted ranking to be rejected once the match has left the Ordering \
         phase, but it was accepted: {}",
        res.text
    );
}

#[tokio::test]
async fn ranking_rejects_a_repeated_problem_id() {
    let fx = setup_fixture().await;
    let b = &fx.players[1];
    let g = fx.rounds[0].group_a;
    let res = fx
        .app
        .post_with_token(
            &bracket_route(fx.contest_id, "/matches/0/order"),
            &json!({"order": [g[0], g[0], g[1]]}),
            &b.token,
        )
        .await;
    assert_eq!(
        res.status, 400,
        "a repeated problem id must be rejected: {}",
        res.text
    );
}

#[tokio::test]
async fn ranking_rejects_a_foreign_problem_id() {
    let fx = setup_fixture().await;
    let b = &fx.players[1];
    let g = fx.rounds[0].group_a;
    let foreign = fx.rounds[1].group_a[0]; // real problem, but not this match's group
    let res = fx
        .app
        .post_with_token(
            &bracket_route(fx.contest_id, "/matches/0/order"),
            &json!({"order": [g[0], g[1], foreign]}),
            &b.token,
        )
        .await;
    assert_eq!(
        res.status, 400,
        "a foreign problem id must be rejected: {}",
        res.text
    );
}

#[tokio::test]
async fn ranking_rejects_the_wrong_length_via_deserialization() {
    let fx = setup_fixture().await;
    let b = &fx.players[1];
    let g = fx.rounds[0].group_a;
    let res = fx
        .app
        .post_with_token(
            &bracket_route(fx.contest_id, "/matches/0/order"),
            &json!({"order": [g[0], g[1]]}),
            &b.token,
        )
        .await;
    assert_eq!(
        res.status, 400,
        "a 2-element order (OrderRequest.order is a fixed [i32; 3]) must be rejected: {}",
        res.text
    );
}

#[tokio::test]
async fn ranking_rejects_a_submitter_not_in_the_match() {
    let fx = setup_fixture().await;
    // Seeded elsewhere in the bracket (round-1 match 2), not a participant
    // of match 0.
    let not_in_match_0 = &fx.players[5];
    let g = fx.rounds[0].group_a;
    let res = fx
        .app
        .post_with_token(
            &bracket_route(fx.contest_id, "/matches/0/order"),
            &json!({"order": g}),
            &not_in_match_0.token,
        )
        .await;
    assert_eq!(
        res.status, 400,
        "a ranking from a non-participant must be rejected: {}",
        res.text
    );
}

#[tokio::test]
async fn start_rejects_a_match_missing_one_ranking() {
    let fx = setup_fixture().await;
    let b = &fx.players[1];
    let res = fx
        .app
        .post_with_token(
            &bracket_route(fx.contest_id, "/matches/0/order"),
            &json!({"order": fx.rounds[0].group_a}),
            &b.token,
        )
        .await;
    assert_eq!(res.status, 200, "B's ranking failed: {}", res.text);
    // A never ranks B's problems.
    let res = start_match(&fx, 0).await;
    assert_eq!(
        res.status, 400,
        "starting a match missing one ranking must be rejected: {}",
        res.text
    );
    assert!(
        res.text.contains("both players"),
        "unexpected rejection message: {}",
        res.text
    );
}

#[tokio::test]
async fn setup_rejects_fewer_than_16_seeds() {
    let (app, _tmp, staff_token, contest_id, rounds, players) =
        contest_and_rounds_for_setup_probe("Short Seeds QA").await;
    let seeds: Vec<i32> = players.iter().take(15).map(|p| p.id).collect();
    let res = app
        .post_with_token(
            &bracket_route(contest_id, "/setup"),
            &json!({
                "rounds": rounds.iter().map(|r| json!({
                    "group_a": r.group_a,
                    "group_b": r.group_b,
                    "tiebreak": [r.tiebreak],
                })).collect::<Vec<_>>(),
                "xiaoju_seconds": 3,
                "round_intermission_seconds": 0,
                "seeds": seeds,
            }),
            &staff_token,
        )
        .await;
    assert_eq!(
        res.status, 400,
        "a 15-element seeds array (SetupRequest.seeds is a fixed [i32; 16]) must be \
         rejected: {}",
        res.text
    );
}

#[tokio::test]
async fn setup_rejects_a_duplicate_seed() {
    let (app, _tmp, staff_token, contest_id, rounds, players) =
        contest_and_rounds_for_setup_probe("Duplicate Seed QA").await;
    let mut seeds: Vec<i32> = players.iter().map(|p| p.id).collect();
    seeds[15] = seeds[0]; // 16 elements, only 15 distinct ids
    let res = app
        .post_with_token(
            &bracket_route(contest_id, "/setup"),
            &full_setup_body(&rounds, &seeds, 3, 0, 120),
            &staff_token,
        )
        .await;
    assert_eq!(
        res.status, 400,
        "a duplicate seed must be rejected: {}",
        res.text
    );
    assert!(
        res.text.contains("distinct"),
        "unexpected rejection message: {}",
        res.text
    );
}

#[tokio::test]
async fn setup_rejects_a_problem_reused_across_two_groups() {
    let (app, _tmp, staff_token, contest_id, mut rounds, players) =
        contest_and_rounds_for_setup_probe("Reused Problem QA").await;
    rounds[1].group_a[0] = rounds[0].group_a[0];
    let seeds: Vec<i32> = players.iter().map(|p| p.id).collect();
    let res = app
        .post_with_token(
            &bracket_route(contest_id, "/setup"),
            &full_setup_body(&rounds, &seeds, 3, 0, 120),
            &staff_token,
        )
        .await;
    assert_eq!(
        res.status, 400,
        "a problem reused across two groups/rounds must be rejected: {}",
        res.text
    );
    assert!(
        res.text.contains("more than one"),
        "unexpected rejection message: {}",
        res.text
    );
}

#[tokio::test]
async fn setup_rejects_an_empty_tiebreak_list() {
    let (app, _tmp, staff_token, contest_id, rounds, players) =
        contest_and_rounds_for_setup_probe("Empty Tiebreak QA").await;
    let seeds: Vec<i32> = players.iter().map(|p| p.id).collect();
    let mut rounds_json: Vec<Value> = rounds
        .iter()
        .map(|r| json!({"group_a": r.group_a, "group_b": r.group_b, "tiebreak": [r.tiebreak]}))
        .collect();
    rounds_json[0]["tiebreak"] = json!([]);
    let res = app
        .post_with_token(
            &bracket_route(contest_id, "/setup"),
            &json!({
                "rounds": rounds_json,
                "xiaoju_seconds": 3,
                "round_intermission_seconds": 0,
                "seeds": seeds,
            }),
            &staff_token,
        )
        .await;
    assert_eq!(
        res.status, 400,
        "an empty tiebreak list must be rejected: {}",
        res.text
    );
    assert!(
        res.text.contains("tiebreak"),
        "unexpected rejection message: {}",
        res.text
    );
}

/// Decides every round-1 match (8 of them, required for
/// `round_ended_at_ms` to be `Some`), then attempts to start the round-2
/// match immediately -- before the configured inter-round intermission has
/// elapsed.
#[tokio::test]
async fn round2_match_cannot_start_before_the_intermission_elapses() {
    let fx = setup_fixture_with_timing(3, 5, 120).await; // 5s intermission

    for pos in 0u8..8 {
        let a = &fx.players[(pos * 2) as usize];
        let b = &fx.players[(pos * 2 + 1) as usize];
        order_match(&fx, pos, &fx.rounds[0], a, b).await;
        let res = start_match(&fx, pos).await;
        assert_eq!(res.status, 200, "starting match {pos} failed: {}", res.text);
        let res = force_decide(&fx, pos, Some(a.id)).await;
        assert_eq!(
            res.status, 200,
            "force-deciding match {pos} failed: {}",
            res.text
        );
    }

    // Round 2 match 8 (fed by round-1 matches 0 and 1) now exists.
    let round2 = get_match(&fx.app, fx.contest_id, 8, &fx.staff_token).await;
    assert_eq!(
        round2["state"], "ordering",
        "round 2 match should exist and be awaiting ordering, got {round2}"
    );
    let pa_id = round2["player_a"].as_i64().expect("player_a") as i32;
    let pb_id = round2["player_b"].as_i64().expect("player_b") as i32;
    let pa = fx
        .players
        .iter()
        .find(|p| p.id == pa_id)
        .expect("player_a should be a known player");
    let pb = fx
        .players
        .iter()
        .find(|p| p.id == pb_id)
        .expect("player_b should be a known player");
    order_match(&fx, 8, &fx.rounds[1], pa, pb).await;

    let res = start_match(&fx, 8).await;
    assert_eq!(
        res.status, 400,
        "starting round 2 before the 5s inter-round intermission has elapsed must be \
         rejected: {}",
        res.text
    );
    assert!(
        res.text.contains("intermission"),
        "unexpected rejection message: {}",
        res.text
    );
}
