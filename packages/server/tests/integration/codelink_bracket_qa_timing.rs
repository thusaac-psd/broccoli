//! Exploratory QA for the 下午场 (afternoon-session) bracket plugin --
//! Areas 3 (timing/races) and 4 (setup/ordering abuse) of
//! `docs/superpowers/plans/2026-09-22-bracket-exploratory-qa.md`. Areas 1-2
//! (visibility, submission gate) are covered by `codelink_bracket_qa.rs`;
//! area 5 (web UI) is out of scope for both files.
//!
//! Reuses `codelink_bracket.rs`'s `pub(crate)` helpers
//! (`spawn_bracket_app`, `player`, `create_all_round_problems`, `get_match`,
//! `submit`, `XIAOJU_SECONDS`) rather than re-deriving them, same rationale
//! as `codelink_bracket_qa.rs`. This file defines its OWN local `Fixture`
//! (not importable from `codelink_bracket_qa.rs` -- that one is private to
//! its module) with one difference that matters for this file's scenarios:
//! `setup_fixture_with_timing` lets each test pick its own
//! `xiaoju_seconds` / `break_before_seconds` / `escalation_grace_seconds`
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

use crate::codelink_bracket::{
    Player, RoundProblems, XIAOJU_SECONDS, bracket_route, create_all_round_problems,
    finish_first_stage, get_match, player, spawn_bracket_app, submit,
};
use crate::common::{TestApp, TestResponse, routes};
use broccoli_server_sdk::permissions as perm;
use chrono::{Duration as ChronoDuration, Utc};
use common::{SubmissionStatus, Verdict};
use sea_orm::{ActiveModelTrait, ConnectionTrait, Set};
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
    break_before_seconds: i64,
    escalation_grace_seconds: i64,
) -> Value {
    json!({
        "rounds": rounds.iter().enumerate().map(|(i, r)| json!({
            "group_a": r.group_a,
            "group_b": r.group_b,
            "tiebreak": [r.tiebreak],
            "xiaoju_seconds": xiaoju_seconds,
            "tiebreak_seconds": xiaoju_seconds,
            "break_before_seconds": if i == 0 { 0 } else { break_before_seconds },
        })).collect::<Vec<_>>(),
        "escalation_grace_seconds": escalation_grace_seconds,
        "seeds": seeds,
    })
}

async fn setup_fixture_with_timing(
    xiaoju_seconds: i64,
    break_before_seconds: i64,
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
                "contest_type": "codelink-bracket",
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

    let rounds = create_all_round_problems(&app, contest_id, &staff_token).await;
    let seeds: Vec<i32> = players.iter().map(|p| p.id).collect();
    let res = app
        .post_with_token(
            &bracket_route(contest_id, "/setup"),
            &full_setup_body(
                &rounds,
                &seeds,
                xiaoju_seconds,
                break_before_seconds,
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

/// Matches start by themselves once both players have ranked: assert that
/// happened (formerly staff had to POST /start).
async fn start_match_0(fx: &Fixture) {
    expect_auto_started(fx, 0).await;
}

async fn expect_auto_started(fx: &Fixture, match_id: u8) {
    let res = fx
        .app
        .get_with_token(
            &bracket_route(fx.contest_id, &format!("/matches/{match_id}")),
            &fx.staff_token,
        )
        .await;
    assert_eq!(
        res.status, 200,
        "reading match {match_id} failed: {}",
        res.text
    );
    assert_eq!(
        res.body["state"], "in_progress",
        "match {match_id} should start by itself once both players have ranked"
    );
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
        contest_type: Set("codelink-bracket".into()),
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
                "contest_type": "codelink-bracket",
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
    expect_auto_started(&fx, 1).await;

    let winner0 = fx.players[0].id;
    let winner1 = fx.players[2].id;
    let (r0, r1) = tokio::join!(
        force_decide(&fx, 0, Some(winner0)),
        force_decide(&fx, 1, Some(winner1)),
    );
    assert_eq!(r0.status, 200, "force-decide match 0 failed: {}", r0.text);
    assert_eq!(r1.status, 200, "force-decide match 1 failed: {}", r1.text);

    finish_first_stage(&fx.app, fx.contest_id, &fx.staff_token, &fx.players, 2).await;

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
/// `NOW()`), and they are derived from the deadline the server reports, not
/// guessed; delivery itself is always forced deterministically via
/// `tick_once` immediately afterwards.
///
/// Both submissions are inserted directly with fixed verdicts. An earlier
/// version sent B's AC through the real judging pipeline with a short game,
/// and failed whenever a loaded test run judged it late or pushed the
/// pre-deadline check past a 3 s deadline.
#[tokio::test]
async fn awaiting_judge_end_to_end_then_escalates_to_needs_adjudication() {
    const GAME_SECONDS: i64 = 8;
    const GRACE_SECONDS: i64 = 2;
    let fx = setup_fixture_with_timing(GAME_SECONDS, 0, GRACE_SECONDS).await;
    order_match_0(&fx).await;
    start_match_0(&fx).await;

    let a = &fx.players[0];
    let b = &fx.players[1];
    let prob_a = fx.rounds[0].group_a[0];
    let prob_b = fx.rounds[0].group_b[0];

    // A's submission never resolves; B's, submitted later, is already an
    // AC. That makes A's submission a BLOCKER rather than simply the only
    // submission on the board (see the separate defect test for that case).
    let t0 = Utc::now();
    let stuck = insert_submission(&fx, a.id, prob_a, SubmissionStatus::Running, None, t0).await;
    insert_submission(
        &fx,
        b.id,
        prob_b,
        SubmissionStatus::Judged,
        Some(Verdict::Accepted),
        t0 + ChronoDuration::seconds(1),
    )
    .await;

    let res = force_decide(&fx, 0, None).await;
    assert_eq!(
        res.status, 200,
        "advance (pre-deadline) failed: {}",
        res.text
    );
    let view = get_match(&fx.app, fx.contest_id, 0, &fx.staff_token).await;
    let deadline_ms = view["current_xiaoju_deadline_ms"]
        .as_i64()
        .expect("game 1 is open and has a deadline");
    assert!(
        Utc::now().timestamp_millis() < deadline_ms,
        "the pre-deadline check ran after the {GAME_SECONDS}s deadline; the run is too \
         slow for this test to say anything"
    );
    assert_eq!(
        view["state"], "in_progress",
        "must stay in_progress (blocked, not yet decided) while A's older submission is \
         still in flight and the deadline has not passed yet, got {view}"
    );
    assert_eq!(view["score_a"].as_i64(), Some(0));
    assert_eq!(view["score_b"].as_i64(), Some(0));

    // The first AC wakeup cannot decide while A is unresolved. The independent
    // deadline timer must remain armed after that wakeup is delivered.
    let config = TimerConfig::default();
    sleep_until_ms(t0.timestamp_millis() + 1_250).await;
    tick_once(&fx.app.state, &config)
        .await
        .expect("early AC wakeup");

    // Wait out the real deadline, then force delivery deterministically.
    sleep_until_ms(deadline_ms + 250).await;
    tick_once(&fx.app.state, &config)
        .await
        .expect("tick_once (xiaoju deadline) failed");

    // The background timer loop may have claimed the row just before our
    // explicit tick. Wait for its delivery rather than inspecting mid-call.
    let view = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let view = get_match(&fx.app, fx.contest_id, 0, &fx.staff_token).await;
            if view["state"] != "in_progress" {
                return view;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("deadline callback did not complete");
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

    // The escalation timer was armed during the tick above, so the grace
    // period is over GRACE_SECONDS after it returned.
    sleep_until_ms(Utc::now().timestamp_millis() + GRACE_SECONDS * 1_000 + 250).await;
    tick_once(&fx.app.state, &config)
        .await
        .expect("tick_once (escalation) failed");

    let view = get_match(&fx.app, fx.contest_id, 0, &fx.staff_token).await;
    assert_eq!(
        view["state"], "needs_adjudication",
        "a match still blocked after the escalation grace period must escalate for staff, \
         got {view}"
    );
    assert_eq!(view["adjudication_reason"], "stuck_judge");
}

/// The contest ending mid-game: nobody can submit any more, so the game
/// must not be scored as if both players had failed. It goes to staff with
/// the reason, and neither the auto-start nor a staff start can begin
/// another match afterwards.
#[tokio::test]
async fn a_game_cut_off_by_the_contest_end_goes_to_staff() {
    let fx = setup_fixture_with_timing(30, 0, 2).await;
    order_match_0(&fx).await;
    start_match_0(&fx).await;

    // Staff pull the end forward to well before game 1's deadline.
    fx.app
        .db
        .execute_unprepared(&format!(
            "UPDATE contest SET end_time = NOW() + INTERVAL '1 second' WHERE id = {}",
            fx.contest_id
        ))
        .await
        .expect("move the contest end");
    sleep_until_ms(Utc::now().timestamp_millis() + 1_500).await;

    // Anything that looks at the match after the end (a verdict landing, a
    // timer, this nudge) must see that the game can no longer be played.
    let res = force_decide(&fx, 0, None).await;
    assert_eq!(
        res.status, 200,
        "advance after the end failed: {}",
        res.text
    );

    let view = get_match(&fx.app, fx.contest_id, 0, &fx.staff_token).await;
    assert_eq!(view["state"], "needs_adjudication", "got {view}");
    assert_eq!(view["adjudication_reason"], "contest_ended");
    assert_eq!(view["score_a"].as_i64(), Some(0));
    assert_eq!(view["score_b"].as_i64(), Some(0));
    assert_eq!(
        view["games"][0]["decided"], false,
        "the cut-off game is left unscored for staff, got {view}"
    );

    // A second match ranked after the end neither starts by itself nor on
    // a staff start.
    order_match(&fx, 1, &fx.rounds[0], &fx.players[2], &fx.players[3]).await;
    let view = get_match(&fx.app, fx.contest_id, 1, &fx.staff_token).await;
    assert_eq!(view["state"], "ordering", "got {view}");
    let res = start_match(&fx, 1).await;
    assert_eq!(res.status, 400, "staff start after the end: {}", res.text);
}

async fn sleep_until_ms(at_ms: i64) {
    let wait = at_ms - Utc::now().timestamp_millis();
    if wait > 0 {
        tokio::time::sleep(Duration::from_millis(wait as u64)).await;
    }
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
    let seeds: Vec<i32> = fx.players.iter().map(|p| p.id).collect();
    let mut initial = full_setup_body(&fx.rounds, &seeds, 3, 0, 120);
    initial["rounds"][0]["tiebreak_seconds"] = json!(7);
    let configured = fx
        .app
        .post_with_token(
            &bracket_route(fx.contest_id, "/setup"),
            &initial,
            &fx.staff_token,
        )
        .await;
    assert_eq!(configured.status, 200, "{}", configured.text);
    order_match_0(&fx).await;
    start_match_0(&fx).await;

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
    wait_published(&fx, res.id(), &a.token).await;

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

    let b = &fx.players[1];
    let reply = submit(
        &fx.app,
        fx.contest_id,
        fx.rounds[0].group_b[1],
        &b.token,
        "ACCEPT",
    )
    .await;
    assert_eq!(reply.status, 201, "{}", reply.text);
    wait_published(&fx, reply.id(), &b.token).await;
    let third = get_match(&fx.app, fx.contest_id, 0, &fx.staff_token).await;
    assert_eq!(third["current_xiaoju_index"], 2);
    sleep_until_ms(third["current_xiaoju_deadline_ms"].as_i64().unwrap() + 150).await;
    assert_eq!(force_decide(&fx, 0, None).await.status, 200);
    let tiebreak = get_match(&fx.app, fx.contest_id, 0, &fx.staff_token).await;
    assert_eq!(tiebreak["state"], "tiebreak", "{tiebreak}");
    assert_eq!(tiebreak["current_xiaoju_index"], 3);
    assert_eq!(
        tiebreak["current_xiaoju_deadline_ms"].as_i64().unwrap()
            - tiebreak["current_xiaoju_opened_at_ms"].as_i64().unwrap(),
        7_000,
        "the tiebreak duration is pinned when the match starts, before the setup correction",
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

/// A player who never ranks must not stall the bracket: the staff start
/// override fills the missing ranking with the setup's listed order.
#[tokio::test]
async fn staff_start_fills_a_missing_ranking_with_the_listed_order() {
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
    // A never ranks B's problems, so the match cannot start by itself.
    let view = get_match(&fx.app, fx.contest_id, 0, &fx.staff_token).await;
    assert_eq!(
        view["state"], "ordering",
        "must wait for A's ranking, got {view}"
    );
    assert!(
        view["starts_at_ms"].is_null(),
        "no start time without both rankings"
    );

    let res = start_match(&fx, 0).await;
    assert_eq!(res.status, 200, "staff override failed: {}", res.text);
    let view = get_match(&fx.app, fx.contest_id, 0, &fx.staff_token).await;
    assert_eq!(view["state"], "in_progress", "got {view}");
    // Orders are masked for this staff user (no submission:view_all), so
    // read B's own view: B plays order_b and sees only its current, first
    // entry - which must be the first problem of the listed group.
    let b_view = get_match(&fx.app, fx.contest_id, 0, &b.token).await;
    assert_eq!(
        b_view["order_b"][0],
        json!(fx.rounds[0].group_b[0]),
        "A's missing ranking of B's problems becomes the listed order: {b_view}"
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
            &full_setup_body(&rounds, &seeds, 3, 0, 120),
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
    let mut body = full_setup_body(&rounds, &seeds, 3, 0, 120);
    body["rounds"][0]["tiebreak"] = json!([]);
    let res = app
        .post_with_token(&bracket_route(contest_id, "/setup"), &body, &staff_token)
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

/// Round 2 opens only after every first-stage match and the intermission.
/// Ordering and staff starts remain unavailable until the opening timer fires.
#[tokio::test]
async fn round2_waits_for_every_match_and_stage_opening_before_ordering() {
    let fx = setup_fixture_with_timing(3, 5, 120).await;
    let seeds: Vec<_> = fx.players.iter().map(|p| p.id).collect();
    let mut body = full_setup_body(&fx.rounds, &seeds, 3, 5, 120);
    body["rounds"][1]["xiaoju_seconds"] = json!(11);
    body["rounds"][1]["tiebreak_seconds"] = json!(19);
    body["rounds"][2]["break_before_seconds"] = json!(60);
    let updated = fx
        .app
        .post_with_token(
            &bracket_route(fx.contest_id, "/setup"),
            &body,
            &fx.staff_token,
        )
        .await;
    assert_eq!(updated.status, 200, "{}", updated.text);
    order_match(&fx, 0, &fx.rounds[0], &fx.players[0], &fx.players[1]).await;
    let first = get_match(&fx.app, fx.contest_id, 0, &fx.staff_token).await;
    assert_eq!(
        first["current_xiaoju_deadline_ms"].as_i64().unwrap()
            - first["current_xiaoju_opened_at_ms"].as_i64().unwrap(),
        3_000
    );

    for pos in 0u8..2 {
        let res = force_decide(&fx, pos, Some(fx.players[usize::from(pos) * 2].id)).await;
        assert_eq!(res.status, 200, "{}", res.text);
    }
    let route = bracket_route(fx.contest_id, "/matches/8");
    let waiting = fx.app.get_with_token(&route, &fx.staff_token).await;
    assert_eq!(
        waiting.status, 404,
        "remaining matches must block stage creation"
    );
    finish_first_stage(&fx.app, fx.contest_id, &fx.staff_token, &fx.players, 2).await;
    let waiting = fx.app.get_with_token(&route, &fx.staff_token).await;
    assert_eq!(waiting.status, 404, "stage must wait for the intermission");
    let ranking = fx
        .app
        .post_with_token(
            &format!("{route}/order"),
            &json!({"order": fx.rounds[1].group_b}),
            &fx.players[0].token,
        )
        .await;
    assert_eq!(ranking.status, 404, "ordering cannot precede stage opening");
    assert_eq!(
        start_match(&fx, 8).await.status,
        404,
        "staff cannot bypass opening"
    );

    tokio::time::sleep(Duration::from_millis(5_500)).await;
    tick_once(&fx.app.state, &TimerConfig::default())
        .await
        .expect("stage timer failed");
    for id in 8..12 {
        let view = get_match(&fx.app, fx.contest_id, id, &fx.staff_token).await;
        assert_eq!(view["state"], "ordering", "stage match {id}: {view}");
    }
    order_match(&fx, 8, &fx.rounds[1], &fx.players[0], &fx.players[2]).await;
    expect_auto_started(&fx, 8).await;
    let second = get_match(&fx.app, fx.contest_id, 8, &fx.staff_token).await;
    assert_eq!(
        second["current_xiaoju_deadline_ms"].as_i64().unwrap()
            - second["current_xiaoju_opened_at_ms"].as_i64().unwrap(),
        11_000
    );
}

// Penalties and feedback use real HTTP submissions and the real WASM plugin.
async fn wait_raw_judgement(fx: &Fixture, id: i32) -> submission::Model {
    use sea_orm::EntityTrait;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let row = submission::Entity::find_by_id(id)
                .one(&fx.app.db)
                .await
                .unwrap()
                .unwrap();
            if row.status.is_terminal() {
                return row;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("submission did not finish judging")
}

async fn wait_published(fx: &Fixture, id: i32, token: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let response = fx.app.get_with_token(&routes::submission(id), token).await;
            assert_eq!(response.status, 200, "{}", response.text);
            if matches!(
                response.body["status"].as_str(),
                Some("Judged" | "CompilationError")
            ) {
                return response.body;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("submission feedback did not release")
}

async fn assert_feedback_withheld(fx: &Fixture, id: i32, token: &str) {
    let detail = fx.app.get_with_token(&routes::submission(id), token).await;
    assert_eq!(detail.status, 200, "{}", detail.text);
    assert_eq!(detail.body["status"], "Running");
    assert!(detail.body["result"].is_null(), "{}", detail.text);
    for list in [
        routes::SUBMISSIONS.to_string(),
        routes::contest_submissions(fx.contest_id),
    ] {
        for (status, present) in [("Running", true), ("Judged", false)] {
            let response = fx
                .app
                .get_with_token(&format!("{list}?status={status}&sort_by=status"), token)
                .await;
            assert_eq!(response.status, 200, "{}", response.text);
            let found = response.body["data"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["id"] == id);
            assert_eq!(found, present, "{}", response.text);
        }
    }
    let history = fx
        .app
        .get_with_token(&routes::submission_judgements(id), token)
        .await;
    assert_eq!(history.status, 200, "{}", history.text);
    for judgement in history.body.as_array().unwrap() {
        assert_eq!(judgement["status"], "Running");
        assert_eq!(judgement["is_finalized"], false);
        assert!(judgement["verdict"].is_null() && judgement["score"].is_null());
        assert!(judgement["finalized_at"].is_null());
        assert!(
            judgement["test_case_results"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn configured_penalties_control_admission_and_player_feedback_including_zero() {
    for seconds in [30, 0] {
        let fx = setup_fixture_with_timing(120, 0, 120).await;
        let seeds: Vec<i32> = fx.players.iter().map(|p| p.id).collect();
        let mut body = full_setup_body(&fx.rounds, &seeds, 120, 0, 120);
        body["penalty_seconds"] = json!(seconds);
        for invalid_grace in [-1, i64::MAX] {
            body["escalation_grace_seconds"] = json!(invalid_grace);
            let invalid = fx
                .app
                .post_with_token(
                    &bracket_route(fx.contest_id, "/setup"),
                    &body,
                    &fx.staff_token,
                )
                .await;
            assert_eq!(invalid.status, 400, "{}", invalid.text);
        }
        body["escalation_grace_seconds"] = json!(120);
        let configured = fx
            .app
            .post_with_token(
                &bracket_route(fx.contest_id, "/setup"),
                &body,
                &fx.staff_token,
            )
            .await;
        assert_eq!(configured.status, 200, "{}", configured.text);
        order_match_0(&fx).await;
        let a = &fx.players[0];
        let wrong = submit(
            &fx.app,
            fx.contest_id,
            fx.rounds[0].group_a[0],
            &a.token,
            "WRONG",
        )
        .await;
        assert_eq!(wrong.status, 201, "{}", wrong.text);
        assert_eq!(
            wait_published(&fx, wrong.id(), &a.token).await["result"]["verdict"],
            "WrongAnswer"
        );
        let view = get_match(&fx.app, fx.contest_id, 0, &a.token).await;
        assert_eq!(view["own_feedback"]["penalty_count"], 1);
        assert_eq!(view["own_feedback"]["delay_seconds"], seconds);

        let accepted = submit(
            &fx.app,
            fx.contest_id,
            fx.rounds[0].group_a[0],
            &a.token,
            "ACCEPT",
        )
        .await;
        assert_eq!(accepted.status, 201, "{}", accepted.text);
        let raw = wait_raw_judgement(&fx, accepted.id()).await;
        let feedback = fx.app.db.query_one_raw(sea_orm::Statement::from_sql_and_values(
            sea_orm::DbBackend::Postgres,
            "SELECT (f.metadata->>'delay_seconds')::bigint AS delay_seconds,
            COALESCE((f.metadata->>'effective_at_us')::bigint, (EXTRACT(EPOCH FROM s.created_at)*1000000)::bigint + (f.metadata->>'delay_seconds')::bigint*1000000) AS effective_at_us
            FROM submission_feedback f JOIN submission s ON s.id=f.submission_id WHERE submission_id = $1",
            [accepted.id().into()],
        )).await.unwrap().unwrap();
        assert_eq!(
            feedback.try_get::<i64>("", "delay_seconds").unwrap(),
            seconds
        );
        assert_eq!(
            feedback.try_get::<i64>("", "effective_at_us").unwrap()
                - raw.created_at.timestamp_micros(),
            seconds * 1_000_000
        );
        if seconds == 0 {
            assert_eq!(
                wait_published(&fx, accepted.id(), &a.token).await["result"]["verdict"],
                "Accepted"
            );
        } else {
            assert_feedback_withheld(&fx, accepted.id(), &a.token).await;
        }
    }
}

#[tokio::test]
async fn penalty_delays_ac_allows_opponent_to_win_and_resets_next_game() {
    let fx = setup_fixture_with_timing(120, 0, 120).await;
    order_match_0(&fx).await;
    let a = &fx.players[0];
    let b = &fx.players[1];
    let wrong = submit(
        &fx.app,
        fx.contest_id,
        fx.rounds[0].group_a[0],
        &a.token,
        "WRONG",
    )
    .await;
    assert_eq!(wrong.status, 201, "{}", wrong.text);
    let wrong_result = wait_published(&fx, wrong.id(), &a.token).await;
    assert_eq!(wrong_result["result"]["verdict"], "WrongAnswer");
    let view = get_match(&fx.app, fx.contest_id, 0, &a.token).await;
    assert_eq!(view["own_feedback"]["penalty_count"], 1);

    let accepted = submit(
        &fx.app,
        fx.contest_id,
        fx.rounds[0].group_a[0],
        &a.token,
        "ACCEPT",
    )
    .await;
    assert_eq!(accepted.status, 201, "{}", accepted.text);
    let raw = wait_raw_judgement(&fx, accepted.id()).await;
    assert_eq!(raw.verdict, Some(Verdict::Accepted));
    assert_feedback_withheld(&fx, accepted.id(), &a.token).await;
    let feedback = fx.app.db.query_one_raw(sea_orm::Statement::from_sql_and_values(
        sea_orm::DbBackend::Postgres,
        "SELECT (f.metadata->>'delay_seconds')::bigint AS delay_seconds,
            COALESCE((f.metadata->>'effective_at_us')::bigint, (EXTRACT(EPOCH FROM s.created_at)*1000000)::bigint + (f.metadata->>'delay_seconds')::bigint*1000000) AS effective_at_us
            FROM submission_feedback f JOIN submission s ON s.id=f.submission_id WHERE submission_id = $1",
        [accepted.id().into()],
    )).await.unwrap().unwrap();
    assert_eq!(feedback.try_get::<i64>("", "delay_seconds").unwrap(), 20);
    assert_eq!(
        feedback.try_get::<i64>("", "effective_at_us").unwrap() - raw.created_at.timestamp_micros(),
        20_000_000
    );
    let again = submit(
        &fx.app,
        fx.contest_id,
        fx.rounds[0].group_a[0],
        &a.token,
        "ACCEPT",
    )
    .await;
    assert_eq!(again.body["code"], "SUBMISSION_IN_FLIGHT", "{}", again.text);

    let opponent = submit(
        &fx.app,
        fx.contest_id,
        fx.rounds[0].group_b[0],
        &b.token,
        "ACCEPT",
    )
    .await;
    assert_eq!(opponent.status, 201, "{}", opponent.text);
    assert_eq!(
        wait_published(&fx, opponent.id(), &b.token).await["result"]["verdict"],
        "Accepted"
    );
    assert_eq!(
        wait_published(&fx, accepted.id(), &a.token).await["result"]["verdict"],
        "TooLate"
    );
    assert_eq!(
        wait_raw_judgement(&fx, accepted.id()).await.verdict,
        Some(Verdict::Accepted)
    );
    let view = get_match(&fx.app, fx.contest_id, 0, &a.token).await;
    assert_eq!(view["score_b"], 1);
    assert_eq!(view["current_xiaoju_index"], 1);
    assert_eq!(view["own_feedback"]["penalty_count"], 0);
    let next = submit(
        &fx.app,
        fx.contest_id,
        fx.rounds[0].group_a[1],
        &a.token,
        "ACCEPT",
    )
    .await;
    assert_eq!(next.status, 201, "{}", next.text);
    assert_eq!(
        wait_published(&fx, next.id(), &a.token).await["result"]["verdict"],
        "Accepted"
    );
}

#[tokio::test]
async fn slow_earlier_judge_withholds_only_correct_opponent_results() {
    for earlier_correct in [true, false] {
        let fx = setup_fixture_with_timing(120, 0, 120).await;
        order_match_0(&fx).await;
        let a = &fx.players[0];
        let b = &fx.players[1];
        let earlier = insert_submission(
            &fx,
            a.id,
            fx.rounds[0].group_a[0],
            SubmissionStatus::Running,
            None,
            Utc::now(),
        )
        .await;
        let wrong = submit(
            &fx.app,
            fx.contest_id,
            fx.rounds[0].group_b[0],
            &b.token,
            "WRONG",
        )
        .await;
        assert_eq!(wrong.status, 201, "{}", wrong.text);
        assert_eq!(
            wait_published(&fx, wrong.id(), &b.token).await["result"]["verdict"],
            "WrongAnswer"
        );
        let accepted = submit(
            &fx.app,
            fx.contest_id,
            fx.rounds[0].group_b[0],
            &b.token,
            "ACCEPT",
        )
        .await;
        assert_eq!(accepted.status, 201, "{}", accepted.text);
        wait_raw_judgement(&fx, accepted.id()).await;
        // Remove B's test-induced penalty while keeping A as an earlier blocker.
        fx.app.db.execute_raw(sea_orm::Statement::from_sql_and_values(sea_orm::DbBackend::Postgres,
            "UPDATE submission_feedback SET metadata = jsonb_set(metadata,'{effective_at_us}',to_jsonb((SELECT (EXTRACT(EPOCH FROM created_at) * 1000000)::bigint FROM submission WHERE id = $1))) WHERE submission_id = $1",
            [accepted.id().into()],
        )).await.unwrap();
        assert_eq!(force_decide(&fx, 0, None).await.status, 200);
        assert_feedback_withheld(&fx, accepted.id(), &b.token).await;
        let mut earlier: submission::ActiveModel = earlier.into();
        earlier.status = Set(SubmissionStatus::Judged);
        earlier.verdict = Set(Some(if earlier_correct {
            Verdict::Accepted
        } else {
            Verdict::WrongAnswer
        }));
        earlier.update(&fx.app.db).await.unwrap();
        assert_eq!(force_decide(&fx, 0, None).await.status, 200);
        let public = wait_published(&fx, accepted.id(), &b.token).await;
        assert_eq!(
            public["result"]["verdict"],
            if earlier_correct {
                "TooLate"
            } else {
                "Accepted"
            }
        );
    }
}

#[tokio::test]
async fn four_errors_accumulate_and_compile_errors_do_not_add_penalty() {
    let fx = setup_fixture_with_timing(120, 0, 120).await;
    order_match_0(&fx).await;
    let a = &fx.players[0];
    let ce = submit(
        &fx.app,
        fx.contest_id,
        fx.rounds[0].group_a[0],
        &a.token,
        "COMPILE_ERROR",
    )
    .await;
    assert_eq!(ce.status, 201, "{}", ce.text);
    assert_eq!(
        wait_published(&fx, ce.id(), &a.token).await["status"],
        "CompilationError"
    );
    assert_eq!(
        get_match(&fx.app, fx.contest_id, 0, &a.token).await["own_feedback"]["penalty_count"],
        0
    );
    for (index, source) in ["WRONG", "TIME_LIMIT", "MEMORY_LIMIT", "RUNTIME_ERROR"]
        .into_iter()
        .enumerate()
    {
        let wrong = submit(
            &fx.app,
            fx.contest_id,
            fx.rounds[0].group_a[0],
            &a.token,
            source,
        )
        .await;
        assert_eq!(wrong.status, 201, "{}", wrong.text);
        wait_published(&fx, wrong.id(), &a.token).await;
        assert_eq!(
            get_match(&fx.app, fx.contest_id, 0, &a.token).await["own_feedback"]["penalty_count"],
            index + 1
        );
    }
    let accepted = submit(
        &fx.app,
        fx.contest_id,
        fx.rounds[0].group_a[0],
        &a.token,
        "ACCEPT",
    )
    .await;
    assert_eq!(accepted.status, 201, "{}", accepted.text);
    wait_raw_judgement(&fx, accepted.id()).await;
    assert_feedback_withheld(&fx, accepted.id(), &a.token).await;
    let row = fx
        .app
        .db
        .query_one_raw(sea_orm::Statement::from_sql_and_values(
            sea_orm::DbBackend::Postgres,
            "SELECT (metadata->>'delay_seconds')::bigint AS delay_seconds FROM submission_feedback WHERE submission_id = $1",
            [accepted.id().into()],
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.try_get::<i64>("", "delay_seconds").unwrap(), 80);
}

#[tokio::test]
async fn atomic_admission_accepts_only_one_concurrent_submission_and_checks_cutoff() {
    use broccoli_server_sdk::types::SubmissionAdmission;
    use sea_orm::EntityTrait;
    use server::entity::plugin_storage;
    let fx = setup_fixture_with_timing(120, 0, 120).await;
    order_match_0(&fx).await;
    let state_key = format!("match:{}:0", fx.contest_id);
    let guard = plugin_storage::Entity::find_by_id((
        "codelink-bracket".to_string(),
        "default".to_string(),
        state_key.clone(),
    ))
    .one(&fx.app.db)
    .await
    .unwrap()
    .unwrap();
    let admission = SubmissionAdmission {
        guard: broccoli_server_sdk::types::SubmissionStorageGuard {
            key: state_key.clone(),
            expected_value: guard.data.as_str().unwrap().to_string(),
        },
        feedback_key: state_key,
        metadata: json!({"scope_key":format!("game:{}:0:0",fx.contest_id),"delay_seconds":0,"effective_at_us":null}),
        slot: Some(broccoli_server_sdk::types::SubmissionSlot {
            key: format!("{}:{}", fx.contest_id, fx.players[0].id),
            generation: None,
        }),
        latest_arrival_ms: Some(Utc::now().timestamp_millis() + 120_000),
    };
    let new = submission::ActiveModel {
        user_id: Set(fx.players[0].id),
        problem_id: Set(fx.rounds[0].group_a[0]),
        contest_id: Set(Some(fx.contest_id)),
        contest_type: Set("codelink-bracket".into()),
        files: Set(json!([])),
        language: Set("cpp".into()),
        status: Set(SubmissionStatus::Running),
        created_at: Set(Utc::now()),
        ..Default::default()
    };
    let mut too_late = admission.clone();
    too_late.latest_arrival_ms = Some(Utc::now().timestamp_millis() - 10_000);
    let rejected = server::services::submission_feedback::insert_admitted(
        &fx.app.state,
        new.clone(),
        &too_late,
        "codelink-bracket",
    )
    .await;
    assert!(
        matches!(rejected, Err(server::error::AppError::PluginRejection { ref code, .. }) if code == "SUBMISSION_TOO_LATE")
    );
    let outcomes = futures::future::join_all((0..8).map(|_| {
        server::services::submission_feedback::insert_admitted(
            &fx.app.state,
            new.clone(),
            &admission,
            "codelink-bracket",
        )
    }))
    .await;
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    for error in outcomes.into_iter().filter_map(Result::err) {
        assert!(
            matches!(error, server::error::AppError::PluginRejection { ref code, .. } if code == "SUBMISSION_IN_FLIGHT"),
            "{error:?}"
        );
    }

    // Simulate the first request finishing before another stale request writes.
    // The unique pending index alone cannot catch this case: hooks must rerun
    // to include the newly earned penalty, even though the slot is now free.
    fx.app
        .db
        .execute_raw(sea_orm::Statement::from_sql_and_values(
            sea_orm::DbBackend::Postgres,
            "UPDATE submission_feedback SET outcome = 'released' WHERE contest_id = $1",
            [fx.contest_id.into()],
        ))
        .await
        .unwrap();
    let stale = server::services::submission_feedback::insert_admitted(
        &fx.app.state,
        new,
        &admission,
        "codelink-bracket",
    )
    .await
    .unwrap_err();
    assert!(
        matches!(stale, server::error::AppError::PluginRejection { code, .. } if code == "SUBMISSION_IN_FLIGHT")
    );
}

#[tokio::test]
async fn penalty_ac_releases_at_effective_time_without_another_submission() {
    let fx = setup_fixture_with_timing(120, 0, 120).await;
    order_match_0(&fx).await;
    let a = &fx.players[0];
    let problem = fx.rounds[0].group_a[0];
    let wrong = submit(&fx.app, fx.contest_id, problem, &a.token, "WRONG").await;
    assert_eq!(wrong.status, 201, "{}", wrong.text);
    wait_published(&fx, wrong.id(), &a.token).await;
    let accepted = submit(&fx.app, fx.contest_id, problem, &a.token, "ACCEPT").await;
    assert_eq!(accepted.status, 201, "{}", accepted.text);
    let raw = wait_raw_judgement(&fx, accepted.id()).await;
    assert_feedback_withheld(&fx, accepted.id(), &a.token).await;
    sleep_until_ms(raw.created_at.timestamp_millis() + 20_250).await;
    assert_eq!(
        wait_published(&fx, accepted.id(), &a.token).await["result"]["verdict"],
        "Accepted"
    );
    let view = get_match(&fx.app, fx.contest_id, 0, &a.token).await;
    assert_eq!(view["score_a"], 1);
    assert_eq!(view["current_xiaoju_index"], 1);
}

#[tokio::test]
async fn system_errors_remain_pending_without_penalizing_the_player() {
    let fx = setup_fixture_with_timing(120, 0, 120).await;
    order_match_0(&fx).await;
    let a = &fx.players[0];
    let problem = fx.rounds[0].group_a[0];
    let failed = submit(&fx.app, fx.contest_id, problem, &a.token, "SYSTEM_ERROR").await;
    assert_eq!(failed.status, 201, "{}", failed.text);
    assert_eq!(
        wait_raw_judgement(&fx, failed.id()).await.verdict,
        Some(Verdict::SystemError)
    );
    assert_feedback_withheld(&fx, failed.id(), &a.token).await;
    let view = get_match(&fx.app, fx.contest_id, 0, &a.token).await;
    assert_eq!(view["own_feedback"]["penalty_count"], 0);
    let next = submit(&fx.app, fx.contest_id, problem, &a.token, "ACCEPT").await;
    assert_eq!(next.body["code"], "SUBMISSION_IN_FLIGHT", "{}", next.text);
}

#[tokio::test]
async fn staff_force_decision_retires_stuck_admissions_and_unblocks_the_next_stage() {
    let fx = setup_fixture_with_timing(120, 0, 120).await;
    order_match_0(&fx).await;
    let a = &fx.players[0];
    let b = &fx.players[1];
    let mut stuck = Vec::new();
    for (player, problem) in [(a, fx.rounds[0].group_a[0]), (b, fx.rounds[0].group_b[0])] {
        let response = submit(
            &fx.app,
            fx.contest_id,
            problem,
            &player.token,
            "SYSTEM_ERROR",
        )
        .await;
        assert_eq!(response.status, 201, "{}", response.text);
        assert_eq!(
            wait_raw_judgement(&fx, response.id()).await.verdict,
            Some(Verdict::SystemError)
        );
        assert_feedback_withheld(&fx, response.id(), &player.token).await;
        stuck.push(response.id());
    }
    assert_eq!(force_decide(&fx, 0, Some(a.id)).await.status, 200);
    for (id, player) in stuck.iter().zip([a, b]) {
        assert_feedback_withheld(&fx, *id, &player.token).await;
        let row = fx
            .app
            .db
            .query_one_raw(sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                "SELECT outcome FROM submission_feedback WHERE submission_id = $1",
                [(*id).into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.try_get::<String>("", "outcome").unwrap(), "retired");
        assert_eq!(
            wait_raw_judgement(&fx, *id).await.verdict,
            Some(Verdict::SystemError)
        );
    }

    finish_first_stage(&fx.app, fx.contest_id, &fx.staff_token, &fx.players, 1).await;
    order_match(&fx, 8, &fx.rounds[1], a, &fx.players[2]).await;
    expect_auto_started(&fx, 8).await;
    let view = get_match(&fx.app, fx.contest_id, 8, &a.token).await;
    assert!(
        view["own_feedback"]["pending_submission_id"].is_null(),
        "{view}"
    );
    let next = submit(
        &fx.app,
        fx.contest_id,
        fx.rounds[1].group_a[0],
        &a.token,
        "WRONG",
    )
    .await;
    assert_eq!(next.status, 201, "{}", next.text);
    let judged = wait_raw_judgement(&fx, next.id()).await;
    assert_eq!(judged.verdict, Some(Verdict::WrongAnswer), "{judged:?}");
    wait_published(&fx, next.id(), &a.token).await;

    // A late repaired result must still use the closed match's outcome, and
    // neither change the staff decision nor reacquire the admission slot.
    for id in &stuck {
        fx.app
            .db
            .execute_raw(sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                "UPDATE submission SET status = 'Judged', verdict = 'Accepted' WHERE id = $1",
                [(*id).into()],
            ))
            .await
            .unwrap();
    }
    assert_eq!(force_decide(&fx, 0, None).await.status, 200);
    assert_eq!(
        wait_published(&fx, stuck[1], &b.token).await["result"]["verdict"],
        "TooLate"
    );
    assert_eq!(
        wait_published(&fx, stuck[0], &a.token).await["result"]["verdict"],
        "Accepted"
    );
    let closed = get_match(&fx.app, fx.contest_id, 0, &a.token).await;
    assert_eq!(closed["winner"], a.id);
}

#[tokio::test]
async fn feedback_publication_failure_retries_after_committing_the_match() {
    let fx = setup_fixture_with_timing(120, 0, 120).await;
    let a = &fx.players[0];
    let seeds: Vec<_> = fx.players.iter().map(|p| p.id).collect();
    let mut body = full_setup_body(&fx.rounds, &seeds, 120, 0, 120);
    body["penalty_seconds"] = json!(3);
    assert_eq!(
        fx.app
            .post_with_token(
                &bracket_route(fx.contest_id, "/setup"),
                &body,
                &fx.staff_token
            )
            .await
            .status,
        200
    );
    order_match_0(&fx).await;
    for index in 0..2 {
        let accepted = submit(
            &fx.app,
            fx.contest_id,
            fx.rounds[0].group_a[index],
            &a.token,
            "ACCEPT",
        )
        .await;
        assert_eq!(accepted.status, 201, "{}", accepted.text);
        wait_published(&fx, accepted.id(), &a.token).await;
    }
    finish_first_stage(&fx.app, fx.contest_id, &fx.staff_token, &fx.players, 1).await;
    let wrong = submit(
        &fx.app,
        fx.contest_id,
        fx.rounds[0].group_a[2],
        &a.token,
        "WRONG",
    )
    .await;
    assert_eq!(wrong.status, 201, "{}", wrong.text);
    wait_published(&fx, wrong.id(), &a.token).await;
    let accepted = submit(
        &fx.app,
        fx.contest_id,
        fx.rounds[0].group_a[2],
        &a.token,
        "ACCEPT",
    )
    .await;
    assert_eq!(accepted.status, 201, "{}", accepted.text);
    let raw = wait_raw_judgement(&fx, accepted.id()).await;
    assert_eq!(raw.verdict, Some(Verdict::Accepted));
    let effective_at_ms = raw.created_at.timestamp_millis() + 3_000;
    fx.app
        .db
        .execute_unprepared(&format!(
            "CREATE FUNCTION reject_feedback_publication() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN IF NEW.submission_id = {} AND NEW.outcome <> 'pending' THEN
             RAISE EXCEPTION 'injected publication failure';
         END IF; RETURN NEW; END $$;
         CREATE TRIGGER reject_feedback_publication BEFORE UPDATE OF outcome ON submission_feedback
         FOR EACH ROW EXECUTE FUNCTION reject_feedback_publication()",
            accepted.id()
        ))
        .await
        .unwrap();
    // A single failure would exhaust this dispatcher's delivery budget without
    // the replacement wakeup and claim-aware cleanup.
    let config = TimerConfig {
        max_attempts: 1,
        ..Default::default()
    };
    let key = format!("xiaoju:{}:0:2:effective:{}", fx.contest_id, accepted.id());
    for attempt in 0..2 {
        sleep_until_ms(effective_at_ms + attempt * 1_000 + 150).await;
        tick_once(&fx.app.state, &config).await.unwrap();
        let committed = get_match(&fx.app, fx.contest_id, 0, &fx.staff_token).await;
        assert_eq!(committed["state"], "decided", "{committed}");
        assert_eq!(committed["winner"], a.id);
        assert_eq!(committed["score_a"], 3);
        let public = fx
            .app
            .get_with_token(&routes::submission(accepted.id()), &a.token)
            .await;
        assert_eq!(public.body["status"], "Running", "{}", public.text);
        let retry = fx.app.db.query_one_raw(sea_orm::Statement::from_sql_and_values(
            sea_orm::DbBackend::Postgres,
            "SELECT attempts, claimed_at FROM plugin_timer WHERE plugin_id = 'codelink-bracket' AND key = $1",
            [key.clone().into()],
        )).await.unwrap().expect("publication retry must remain durable");
        assert_eq!(retry.try_get::<i32>("", "attempts").unwrap(), 0);
        assert!(
            retry
                .try_get::<Option<chrono::DateTime<Utc>>>("", "claimed_at")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            fx.app
                .get_with_token(&bracket_route(fx.contest_id, "/matches/8"), &fx.staff_token)
                .await
                .status,
            404
        );
    }
    fx.app.db.execute_unprepared(
        "DROP TRIGGER reject_feedback_publication ON submission_feedback; DROP FUNCTION reject_feedback_publication()"
    ).await.unwrap();
    sleep_until_ms(effective_at_ms + 2_150).await;
    tick_once(&fx.app.state, &config).await.unwrap();
    assert_eq!(
        wait_published(&fx, accepted.id(), &a.token).await["result"]["verdict"],
        "Accepted"
    );
    let next = get_match(&fx.app, fx.contest_id, 8, &fx.staff_token).await;
    assert_eq!(next["state"], "ordering", "{next}");
    order_match(&fx, 8, &fx.rounds[1], a, &fx.players[2]).await;
    let next_submission = submit(
        &fx.app,
        fx.contest_id,
        fx.rounds[1].group_a[0],
        &a.token,
        "ACCEPT",
    )
    .await;
    assert_eq!(
        next_submission.status, 201,
        "publication retry must release admission: {}",
        next_submission.text
    );
}

#[tokio::test]
async fn plugin_reload_upgrades_its_own_old_storage_without_a_core_data_migration() {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use server::entity::plugin_storage;
    let fx = setup_fixture_with_timing(120, 0, 120).await;
    let setup_key = format!("setup:{}", fx.contest_id);
    let match_key = format!("match:{}:0", fx.contest_id);
    for key in [&setup_key, &match_key] {
        let row = plugin_storage::Entity::find_by_id((
            "codelink-bracket".to_string(),
            "default".to_string(),
            key.clone(),
        ))
        .one(&fx.app.db)
        .await
        .unwrap()
        .unwrap();
        let mut document: Value = serde_json::from_str(row.data.as_str().unwrap()).unwrap();
        if key == &setup_key {
            document["xiaoju_seconds"] = json!(120);
            document["round_intermission_seconds"] = json!(45);
            for stage in document["rounds"].as_array_mut().unwrap() {
                for field in ["xiaoju_seconds", "tiebreak_seconds", "break_before_seconds"] {
                    stage.as_object_mut().unwrap().remove(field);
                }
            }
        } else {
            document.as_object_mut().unwrap().remove("game_durations");
            document["xiaoju_seconds"] = json!(0);
            document["admission_revision"] = json!(3);
            document["admission_by_user"] = json!({});
        }
        let mut old: plugin_storage::ActiveModel = row.into();
        // Both representations supported by the storage host API must migrate.
        old.data = Set(if key == &setup_key {
            json!(document.to_string())
        } else {
            document
        });
        old.update(&fx.app.db).await.unwrap();
    }
    plugin_storage::Entity::delete_many()
        .filter(plugin_storage::Column::PluginId.eq("codelink-bracket"))
        .filter(plugin_storage::Column::Key.eq("schema_version"))
        .exec(&fx.app.db)
        .await
        .unwrap();
    let admin = fx
        .app
        .create_user_with_role("migration_admin", "pass1234", "admin")
        .await;
    let reloaded = fx
        .app
        .post_with_token(
            &format!(
                "{}/reload",
                routes::admin_plugin_details("codelink-bracket")
            ),
            &json!({}),
            &admin,
        )
        .await;
    assert_eq!(reloaded.status, 200, "{}", reloaded.text);
    let setup = plugin_storage::Entity::find_by_id((
        "codelink-bracket".to_string(),
        "default".to_string(),
        setup_key,
    ))
    .one(&fx.app.db)
    .await
    .unwrap()
    .unwrap();
    let setup: Value = serde_json::from_str(setup.data.as_str().unwrap()).unwrap();
    assert_eq!(setup["rounds"][0]["break_before_seconds"], 0);
    assert_eq!(setup["rounds"][1]["break_before_seconds"], 45);
    assert_eq!(setup["rounds"][0]["xiaoju_seconds"], 120);
    let migrated = get_match(&fx.app, fx.contest_id, 0, &fx.staff_token).await;
    assert_eq!(migrated["state"], "ordering");
    order_match_0(&fx).await;
    assert_eq!(
        get_match(&fx.app, fx.contest_id, 0, &fx.staff_token).await["state"],
        "in_progress"
    );
}
