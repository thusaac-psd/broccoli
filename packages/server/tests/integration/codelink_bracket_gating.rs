//! Visibility and submission-gate tests for the 下午场 (afternoon-session)
//! bracket plugin. Timing, race and setup-ordering scenarios live in
//! `codelink_bracket_timing.rs`.
//!
//! These scenarios complement `codelink_bracket.rs`'s single linear
//! end-to-end flow, which covers own-group visibility during ordering,
//! current-not-future within one match, another player's submission by id,
//! the opponent-problem/own-future-problem gate asymmetry, an eliminated
//! player losing round-2 visibility, and `submission:view_all` seeing
//! everything. This file reuses that file's `pub(crate)` helpers
//! (`spawn_bracket_app`, `player`, `create_all_round_problems`,
//! `setup_body`, `bracket_route`, `get_match`, `visible_problem_ids`,
//! `submit`), so every check goes through the same real host-to-plugin path
//! documented there.

use crate::codelink_bracket::{
    Player, RoundProblems, bracket_route, create_all_round_problems, player, setup_body,
    spawn_bracket_app, submit, visible_problem_ids,
};
use crate::common::{TestApp, routes};
use broccoli_server_sdk::permissions as perm;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde_json::json;
use server::entity::role_permission;

/// Shared bracket fixture: 16 seeded players (`players[0]` = A, `players[1]`
/// = B, matched against each other in round 1 match 0; this is the same
/// pairing `codelink_bracket.rs` relies on, see its comment on `setup.rs`'s
/// round-1 loop), one `outsider` who registers for the contest but is never
/// seeded into the bracket, staff, all 4 rounds of real problems, `/setup`
/// already run, and the `before_submission` hook enabled at contest scope.
/// Callers drive match 0 (and, where a test needs it, match 1) through
/// ordering, start and force-decide themselves, since each test needs a
/// different phase frozen to check it.
struct Fixture {
    app: TestApp,
    _plugins_tmp: tempfile::TempDir,
    contest_id: i32,
    staff_token: String,
    rounds: Vec<RoundProblems>,
    players: Vec<Player>,
    outsider: Player,
}

async fn setup_fixture() -> Fixture {
    let (app, plugins_tmp) = spawn_bracket_app().await;

    let staff_token = app
        .create_user_with_permissions(
            "gating_staff",
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
        players.push(player(&app, &format!("gating_player_{i}")).await);
    }
    let outsider = player(&app, "gating_outsider").await;

    let res = app
        .post_with_token(
            routes::CONTESTS,
            &json!({
                "title": "Afternoon Bracket Gating",
                "description": "Bracket visibility and submission gate contest",
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
    app.register_for_contest(contest_id, &outsider.token).await;

    let rounds = create_all_round_problems(&app, contest_id, &staff_token).await;

    let seeds: Vec<i32> = players.iter().map(|p| p.id).collect();
    let res = app
        .post_with_token(
            &bracket_route(contest_id, "/setup"),
            &setup_body(&rounds, &seeds),
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
        outsider,
    }
}

/// Rank match 0 (A vs B) in both directions. A match cannot start while
/// either ranking is missing.
async fn order_match_0(fx: &Fixture) {
    let a = &fx.players[0];
    let b = &fx.players[1];
    let res = fx
        .app
        .post_with_token(
            &bracket_route(fx.contest_id, "/matches/0/order"),
            &json!({"order": fx.rounds[0].group_a}),
            &b.token,
        )
        .await;
    assert_eq!(
        res.status, 200,
        "B's ranking of A's problems failed: {}",
        res.text
    );
    let res = fx
        .app
        .post_with_token(
            &bracket_route(fx.contest_id, "/matches/0/order"),
            &json!({"order": fx.rounds[0].group_b}),
            &a.token,
        )
        .await;
    assert_eq!(
        res.status, 200,
        "A's ranking of B's problems failed: {}",
        res.text
    );
}

/// Matches start by themselves once both players have ranked: assert that
/// match 0 is now in progress.
async fn start_match_0(fx: &Fixture) {
    let res = fx
        .app
        .get_with_token(&bracket_route(fx.contest_id, "/matches/0"), &fx.staff_token)
        .await;
    assert_eq!(res.status, 200, "reading match 0 failed: {}", res.text);
    assert_eq!(
        res.body["state"], "in_progress",
        "match 0 should start by itself once both players have ranked"
    );
}

// =====================================================================
// Standalone (contest-id-less) problem read paths.
//
// `GET /problems/{id}`, attachment download, and (for a sample) `GET
// /problems/{id}/test-cases/{tc_id}` ask the visibility kernel about
// `Resource::Problem { contest_id: None, .. }` (see
// `packages/server/src/handlers/problem/mod.rs::get_problem` and
// `packages/server/src/handlers/attachment.rs::download_attachment`). The
// host side, `decide_standalone_problem_access` in
// `packages/server/src/visibility/host_rules.rs`, allows any `is_public`
// problem, and every bracket problem is `is_public: true` (see
// `create_and_attach_problem` in `codelink_bracket.rs`). The bracket's
// phase and ownership rules therefore hold on these paths only because
// `plugins/codelink-bracket/src/visibility.rs` resolves a context-free
// problem to the bracket contests it belongs to and decides it through the
// same `decide_problem_for_contest` that the contest-scoped path (`GET
// /contests/{id}/problems`) uses. A rule enforced on one path must hold on
// every other path to the same resource: without it, every problem in the
// bracket would be readable by any authenticated user at any phase.
// =====================================================================

#[tokio::test]
async fn standalone_problem_detail_hides_own_not_yet_open_bracket_problem() {
    let fx = setup_fixture().await;
    order_match_0(&fx).await;
    start_match_0(&fx).await;

    let a = &fx.players[0];
    let future_problem = fx.rounds[0].group_a[2];

    // Premise: the contest-scoped path hides it, so the 404 asserted below
    // checks the standalone route rather than the fixture.
    let ids_for_a = visible_problem_ids(&fx.app, fx.contest_id, &a.token).await;
    assert!(
        !ids_for_a.contains(&(future_problem as i64)),
        "sanity check failed: the contest-scoped list should hide A's own not-yet-open \
         problem {future_problem}; if this fails, the fixture is broken, not the \
         standalone route"
    );

    let res = fx
        .app
        .get_with_token(&routes::problem(future_problem), &a.token)
        .await;
    assert_eq!(
        res.status, 404,
        "GET /problems/{{id}} returned {} (body: {}) for A's own not-yet-open bracket \
         problem {future_problem}; it must be hidden, as the contest-scoped list hides the \
         same resource",
        res.status, res.text
    );
}

#[tokio::test]
async fn standalone_attachment_download_hides_own_not_yet_open_bracket_problem() {
    let fx = setup_fixture().await;
    order_match_0(&fx).await;
    start_match_0(&fx).await;

    let a = &fx.players[0];
    let future_problem = fx.rounds[0].group_a[2];

    let upload = fx
        .app
        .upload_attachment(
            future_problem,
            "hint.txt",
            "do not leak this to A before 小局 2 opens"
                .as_bytes()
                .to_vec(),
            None,
            &fx.staff_token,
        )
        .await;
    assert_eq!(
        upload.status, 201,
        "attaching a file to the future problem failed: {}",
        upload.text
    );
    let ref_id = upload.body["id"]
        .as_str()
        .expect("attachment response should carry an id")
        .to_string();

    let res = fx
        .app
        .download_raw(&routes::attachment(future_problem, &ref_id), &a.token)
        .await;
    assert_eq!(
        res.status().as_u16(),
        404,
        "attachment download returned {} for A's own not-yet-open bracket problem \
         {future_problem}'s attachment; it must be denied like the problem detail is",
        res.status().as_u16()
    );
}

#[tokio::test]
async fn standalone_sample_test_case_hides_own_not_yet_open_bracket_problem() {
    let fx = setup_fixture().await;
    order_match_0(&fx).await;
    start_match_0(&fx).await;

    let a = &fx.players[0];
    let future_problem = fx.rounds[0].group_a[2];

    let tc_id = fx
        .app
        .create_test_case(future_problem, &fx.staff_token)
        .await;

    let res = fx
        .app
        .get_with_token(&routes::test_case(future_problem, tc_id), &a.token)
        .await;
    assert_eq!(
        res.status, 404,
        "GET /problems/{{id}}/test-cases/{{tc_id}} returned {} (body: {}) for A's own \
         not-yet-open bracket problem {future_problem}'s sample test case; it must be denied",
        res.status, res.text
    );
}

// =====================================================================
// Standalone submission route (`POST /problems/{id}/submissions`).
//
// It must not let a player reach a bracket problem around the contest
// route: hidden problems stay hidden, and readable ones still require
// their contest context, even when the request names a contest type that
// skips admission.
// =====================================================================

#[tokio::test]
async fn standalone_submission_cannot_bypass_bracket_admission() {
    let fx = setup_fixture().await;
    fx.app
        .state
        .registries
        .contest_type_registry
        .write()
        .await
        .insert(
            "practice".into(),
            server::registry::ContestTypeHandlers {
                plugin_id: "codelink-bracket-judge-fixture".into(),
                submission_fn: "unused".into(),
                code_run_fn: "unused".into(),
                submission_admission: false,
            },
        );
    order_match_0(&fx).await;
    start_match_0(&fx).await;

    let a = &fx.players[0];
    let opponents_current_problem = fx.rounds[0].group_b[0];

    // The contest-scoped route hides opponent problems once ordering ends.
    let res = submit(
        &fx.app,
        fx.contest_id,
        opponents_current_problem,
        &a.token,
        "ACCEPT",
    )
    .await;
    assert_eq!(
        res.status, 404,
        "sanity check failed: the contest-scoped submit should reject A submitting to B's \
         current problem: {}",
        res.text
    );
    assert_eq!(res.body["code"], "NOT_FOUND");

    // Hidden opponent problems stay hidden on standalone submission routes;
    // the readable current problem still requires its contest context.
    for problem in [opponents_current_problem, fx.rounds[0].group_a[0]] {
        for contest_type in [None, Some("practice")] {
            let mut body = json!({
                "files": [{"filename": "main.cpp", "content": "ACCEPT"}],
                "language": "cpp",
            });
            if let Some(contest_type) = contest_type {
                body["contest_type"] = json!(contest_type);
            }
            let res = fx
                .app
                .post_with_token(&routes::problem_submissions(problem), &body, &a.token)
                .await;
            let hidden = problem == opponents_current_problem;
            assert_eq!(res.status, if hidden { 404 } else { 400 }, "{}", res.text);
            assert_eq!(
                res.body["code"],
                if hidden {
                    "NOT_FOUND"
                } else {
                    "CONTEST_SUBMISSION_REQUIRED"
                },
                "{}",
                res.text
            );
        }
    }
}

/// Submitting to one's own not-yet-open problem would defeat the format: a
/// player could read 小局 3's problem during 小局 1, develop against the
/// real judge, then submit the moment it opens. The rules'
/// 「无法提前看到自己组后面的题目」 exists to prevent exactly that.
///
/// Visibility closes this, not the hook: `create_submission` gates
/// `Action::Submit` on the same `Resource::Problem { contest_id: None }`
/// decision that hides the problem from `GET /problems/{id}`, so the request
/// fails with 404 before hook dispatch. A change to either layer must keep
/// this closed.
#[tokio::test]
async fn own_not_yet_open_problem_cannot_be_submitted_to_via_the_standalone_route() {
    let fx = setup_fixture().await;
    order_match_0(&fx).await;
    start_match_0(&fx).await;

    let a = &fx.players[0];
    // A's own 小局-2 problem: theirs, but not yet open while 小局 0 runs.
    let own_future_problem = fx.rounds[0].group_a[2];

    // Premise: the contest-scoped list hides it, so the denial asserted
    // below is the bracket's rule and not a broken fixture.
    let ids_for_a = visible_problem_ids(&fx.app, fx.contest_id, &a.token).await;
    assert!(
        !ids_for_a.contains(&(own_future_problem as i64)),
        "premise failed: A's own not-yet-open problem should be hidden from the \
         contest-scoped list"
    );

    let res = fx
        .app
        .post_with_token(
            &routes::problem_submissions(own_future_problem),
            &json!({
                "files": [{"filename": "main.cpp", "content": "ACCEPT"}],
                "language": "cpp",
            }),
            &a.token,
        )
        .await;

    assert_eq!(
        res.status, 404,
        "A submitted to their own not-yet-open problem via the standalone route (status {}, \
         body {}); a player must not be able to judge-test a later 小局's solution ahead of \
         time",
        res.status, res.text
    );
}

// =====================================================================
// Contest-scoped submission gate: each rejection reaches the right layer
// and carries its own code.
// =====================================================================

/// Submitting during the `Ordering` phase (match created, rankings not all
/// in, match not started) is rejected with a code distinct from both gate
/// probes in `codelink_bracket.rs`. Once B has ranked A's group (submitter
/// B writes `order_a`, see `ordering.rs`'s module doc comment), the
/// kernel's `decide_problem` grants B (the opponent) read access because
/// `state != Pending` (`Ordering` qualifies). The request therefore reaches
/// the plugin's `before_submission` hook, which rejects it as
/// `MATCH_NOT_STARTED` (`gate.rs::resolve_rejection`: `m.state !=
/// InProgress && m.state != Tiebreak`) through the real HTTP path.
#[tokio::test]
async fn submitting_during_ordering_phase_is_rejected_as_match_not_started() {
    let fx = setup_fixture().await;

    let a = &fx.players[0];
    let b = &fx.players[1];

    // Only B ranks A's group. That grants B (opponent) kernel-level read
    // access to it under `decide_problem`'s "reached ordering or later"
    // rule, but A never ranks B's group, so the match does not start.
    let res = fx
        .app
        .post_with_token(
            &bracket_route(fx.contest_id, "/matches/0/order"),
            &json!({"order": fx.rounds[0].group_a}),
            &b.token,
        )
        .await;
    assert_eq!(
        res.status, 200,
        "B's ranking of A's problems failed: {}",
        res.text
    );

    let ids_for_b = visible_problem_ids(&fx.app, fx.contest_id, &b.token).await;
    assert!(
        ids_for_b.contains(&(fx.rounds[0].group_a[0] as i64)),
        "sanity check failed: B (opponent) should already see A's group_a during ordering, \
         which is what puts this submission through to the gate rather than being denied at \
         the kernel first"
    );

    let res = submit(
        &fx.app,
        fx.contest_id,
        fx.rounds[0].group_a[0],
        &b.token,
        "ACCEPT",
    )
    .await;
    assert_eq!(
        res.status, 400,
        "submitting during the Ordering phase (match never started) should be rejected: {}",
        res.text
    );
    assert_eq!(
        res.body["code"], "MATCH_NOT_STARTED",
        "ordering-phase submission must carry its own distinct code, got: {}",
        res.text
    );

    let _ = a; // only used to name the pairing for readers; B drives this probe.
}

/// An eliminated player (B, who lost round-1 match 0) cannot submit to a
/// round they have no match in (round 2, where A plays C).
/// `find_players_match` finds no match for B in round 2, so both the
/// kernel's reachability check and `gate.rs`'s pure `resolve_rejection`
/// would answer "not this viewer's round". The kernel decides first (see
/// `decide_problem`), so the response is a generic 404 and the hook is never
/// dispatched. This is the same ordering `codelink_bracket.rs`'s
/// own-future-problem gate probe pins (a kernel-level denial pre-empts the
/// plugin's own rejection code), applied to an eliminated player, a state
/// that file never drives a match into.
#[tokio::test]
async fn eliminated_player_cannot_submit_to_a_round_they_have_no_match_in() {
    let fx = setup_fixture().await;
    order_match_0(&fx).await;
    start_match_0(&fx).await;

    let a = &fx.players[0];
    let b = &fx.players[1];
    let c = &fx.players[2];

    // Force-decide match 0 (A beats B) and match 1 (C beats D) so round 2's
    // match exists (id 8, A vs C; see `codelink_bracket.rs`'s comment on
    // `storage::match_id_for`'s slot scheme).
    let res = fx
        .app
        .post_with_token(
            &bracket_route(fx.contest_id, "/matches/0/force-decide"),
            &json!({"winner": a.id}),
            &fx.staff_token,
        )
        .await;
    assert_eq!(
        res.status, 200,
        "force-deciding match 0 failed: {}",
        res.text
    );

    let res = fx
        .app
        .post_with_token(
            &bracket_route(fx.contest_id, "/matches/1/force-decide"),
            &json!({"winner": c.id}),
            &fx.staff_token,
        )
        .await;
    assert_eq!(
        res.status, 200,
        "force-deciding match 1 failed: {}",
        res.text
    );

    // B is now eliminated. B has never had a match in round 2 at all.
    let res = submit(
        &fx.app,
        fx.contest_id,
        fx.rounds[1].group_a[0],
        &b.token,
        "ACCEPT",
    )
    .await;
    assert_eq!(
        res.status, 404,
        "an eliminated player submitting to a round they have no match in should be denied at \
         the kernel (the same ordering as the own-future-problem case), got: {}",
        res.text
    );
    assert_eq!(res.body["code"], "NOT_FOUND");
}

/// A user who registered for the contest (so contest-level participation
/// checks pass) but was never seeded into the bracket has no match in any
/// round (`find_players_match` never finds them). As with the eliminated
/// player above, the kernel denies the request generically before the gate
/// can produce its own `NOT_IN_MATCH` code.
#[tokio::test]
async fn registered_but_not_in_bracket_user_cannot_submit() {
    let fx = setup_fixture().await;
    order_match_0(&fx).await;
    start_match_0(&fx).await;

    let res = submit(
        &fx.app,
        fx.contest_id,
        fx.rounds[0].group_a[0],
        &fx.outsider.token,
        "ACCEPT",
    )
    .await;
    assert_eq!(
        res.status, 404,
        "a contest participant who was never seeded into the bracket should be denied at the \
         kernel, got: {}",
        res.text
    );
    assert_eq!(res.body["code"], "NOT_FOUND");
}

// =====================================================================
// `submission:view_all` revocation mid-contest.
//
// `codelink_bracket.rs` covers a `submission:view_all` holder seeing
// everything; this section covers the permission being revoked mid-contest.
// =====================================================================

/// `AuthUser` (`packages/server/src/extractors/auth.rs`) is stateless: its
/// `permissions` come from the JWT's claims, fixed at login or registration
/// time, and read/submit paths never re-check them against the database
/// (only `FreshAuthUser`, reserved for high-value mutation handlers,
/// re-checks `credentials_changed_at`). Revoking a role's permission
/// mid-contest therefore does not narrow an already-issued access token's
/// visibility; only a freshly minted token (re-login) picks up the change.
/// This is the codebase-wide tradeoff (stateless reads and submits stay
/// fast; only mutations pay for freshness), not a bracket-specific rule.
/// This test pins where the line falls for this permission in this plugin:
/// the stale token keeps the spectator's view, and a fresh token (re-login)
/// loses it.
#[tokio::test]
async fn revoking_view_all_mid_contest_does_not_narrow_an_already_issued_token() {
    let fx = setup_fixture().await;
    order_match_0(&fx).await;
    start_match_0(&fx).await;

    let spectator_username = "gating_spectator";
    let spectator_password = "pass1234";
    let stale_token = fx
        .app
        .create_user_with_permissions(
            spectator_username,
            spectator_password,
            &[perm::SUBMISSION_VIEW_ALL],
        )
        .await;
    let role_name = format!("{spectator_username}_role");

    let future_problem = fx.rounds[0].group_a[2];
    let ids = visible_problem_ids(&fx.app, fx.contest_id, &stale_token).await;
    assert!(
        ids.contains(&(future_problem as i64)),
        "sanity check failed: submission:view_all should see every problem live, including \
         A's own not-yet-open one"
    );

    // Revoke the permission directly in the DB, which is what an admin's
    // "remove role permission" action ultimately does.
    let deleted = role_permission::Entity::delete_many()
        .filter(role_permission::Column::Role.eq(role_name))
        .filter(role_permission::Column::Permission.eq(perm::SUBMISSION_VIEW_ALL))
        .exec(&fx.app.db)
        .await
        .expect("revoke submission:view_all");
    assert_eq!(
        deleted.rows_affected, 1,
        "expected exactly one row to be revoked"
    );

    // The stale token (minted before the revocation) still carries the old
    // permission, so it keeps seeing everything.
    let ids = visible_problem_ids(&fx.app, fx.contest_id, &stale_token).await;
    assert!(
        ids.contains(&(future_problem as i64)),
        "the stale token should still see everything: stateless AuthUser reads permissions \
         from the JWT's claims, not the database, on this path"
    );

    // A fresh token (re-login) picks up the revocation.
    let login = fx
        .app
        .post_without_token(
            routes::LOGIN,
            &json!({"username": spectator_username, "password": spectator_password}),
        )
        .await;
    assert_eq!(login.status, 200, "re-login failed: {}", login.text);
    let fresh_token = login.body["token"]
        .as_str()
        .expect("login response should contain a token")
        .to_string();

    let ids = visible_problem_ids(&fx.app, fx.contest_id, &fresh_token).await;
    assert!(
        !ids.contains(&(future_problem as i64)),
        "the fresh (post-revocation) token should have lost submission:view_all and now see \
         only what an ordinary registered-but-not-in-bracket viewer sees"
    );
}
