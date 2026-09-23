//! Exploratory QA for the 下午场 (afternoon-session) bracket plugin --
//! Areas 1 (visibility leaks) and 2 (the submission gate) of
//! `docs/superpowers/plans/2026-09-22-bracket-exploratory-qa.md`. Areas 3-5
//! (timing/races, setup abuse, the web UI) are out of scope for this file.
//!
//! Every scenario here is picked to NOT duplicate what
//! `afternoon_bracket.rs`'s single linear end-to-end flow already proves
//! (own-group-during-ordering, current-not-future within one match, another
//! player's submission-by-id, the opponent-problem/own-future-problem gate
//! asymmetry, an eliminated player losing round-2 visibility,
//! `submission:view_all` seeing everything). This file reuses that file's
//! `pub(crate)` helpers (`spawn_bracket_app`, `player`,
//! `create_all_round_problems`, `setup_body`, `bracket_route`, `get_match`,
//! `visible_problem_ids`, `submit`) rather than re-deriving them, per the
//! same "prove the host really asks the plugin" rationale documented there.
//!
//! Findings are written up in
//! `.superpowers/sdd/2026-09-19-afternoon-bracket/qa-visibility-gating.md`.
//! Two classes of `#[ignore]`d tests below encode CONFIRMED defects rather
//! than being softened to pass -- see each test's `#[ignore = "..."]`
//! reason for the full repro and root cause.

use crate::afternoon_bracket::{
    Player, RoundProblems, bracket_route, create_all_round_problems, player, setup_body,
    spawn_bracket_app, submit, visible_problem_ids,
};
use crate::common::{TestApp, routes};
use broccoli_server_sdk::permissions as perm;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde_json::json;
use server::entity::role_permission;

/// Shared bracket fixture: 16 seeded players (`players[0]` = A, `players[1]`
/// = B, matched against each other in round 1 match 0 -- same pairing
/// `afternoon_bracket.rs` relies on, see its own comment on `setup.rs`'s
/// round-1 loop) plus one `outsider` who registers for the contest but is
/// never seeded into the bracket at all, staff, all 4 rounds of real
/// problems, `/setup` already run, and the `before_submission` hook already
/// enabled at contest scope. Callers drive match 0 (and, where a test needs
/// it, match 1) through ordering/start/force-decide themselves, since each
/// test here needs a different phase frozen to probe it.
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
            "qa_staff",
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
        players.push(player(&app, &format!("qa_player_{i}")).await);
    }
    let outsider = player(&app, "qa_outsider").await;

    let res = app
        .post_with_token(
            routes::CONTESTS,
            &json!({
                "title": "Afternoon Bracket QA",
                "description": "Visibility/gate exploratory QA contest",
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
    app.register_for_contest(contest_id, &outsider.token).await;

    // Same reasoning as `afternoon_bracket.rs`: a `HookScope::Resource` hook
    // only fires if an explicit config row sets `enabled: true`.
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

/// Rank match 0 (A vs B) both directions -- `/matches/0/start` refuses to
/// start a match missing either ranking.
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

async fn start_match_0(fx: &Fixture) {
    let res = fx
        .app
        .post_with_token(
            &bracket_route(fx.contest_id, "/matches/0/start"),
            &json!({}),
            &fx.staff_token,
        )
        .await;
    assert_eq!(res.status, 200, "starting match 0 failed: {}", res.text);
}

// =====================================================================
// Area 1 -- confirmed leaks: the standalone (contest-id-less) problem
// read paths.
//
// Root cause, common to all three tests below: `GET /problems/{id}`,
// attachment download, and (for a sample) `GET
// /problems/{id}/test-cases/{tc_id}` all build `Resource::Problem{
// contest_id: None, .. }` / `Resource::Attachment{ .. }` -- see
// `packages/server/src/handlers/problem/mod.rs::get_problem` and
// `packages/server/src/handlers/attachment.rs::download_attachment`. Both
// resource kinds are decided ENTIRELY by
// `packages/server/src/visibility/host_rules.rs::decide_standalone_problem_access`
// (is_public, or attached to a public+started contest, or participant of a
// started contest) and NEVER reach the afternoon-bracket plugin's
// phase/ownership-aware `decide_visibility`:
// `plugins/afternoon-bracket/src/visibility.rs`'s `"problem"` arm explicitly
// returns `WireDecision::Allow {}` (no opinion) whenever the query's
// `contest_id` is `None`:
//
// ```
// let (Some(contest_id), Some(problem_id)) =
//     (resource.contest_id, resource.problem_id)
// else {
//     return WireDecision::Allow {};
// };
// ```
//
// `GET /contests/{id}/problems` (`list_contest_problems`), by contrast,
// queries `Resource::Problem{contest_id: Some(cid), ..}`, which DOES reach
// the plugin and correctly hides the same row -- this is a rule "enforced
// on one path and forgotten on another", exactly the defect class the QA
// plan calls the likeliest one. Since every bracket problem in this fixture
// (and, per `create_and_attach_problem`'s own comment, every bracket
// problem the real product creates) is `is_public: true`, the leak fires
// for literally every problem in the bracket, at any phase, to any
// authenticated user -- not just to another contestant.
// =====================================================================

#[tokio::test]
async fn defect_standalone_problem_detail_leaks_own_not_yet_open_bracket_problem() {
    let fx = setup_fixture().await;
    order_match_0(&fx).await;
    start_match_0(&fx).await;

    let a = &fx.players[0];
    let future_problem = fx.rounds[0].group_a[2];

    // Sanity: the CORRECT, contest-scoped path denies it, so the
    // discrepancy asserted below is real, not a fixture bug.
    let ids_for_a = visible_problem_ids(&fx.app, fx.contest_id, &a.token).await;
    assert!(
        !ids_for_a.contains(&(future_problem as i64)),
        "sanity check failed: the contest-scoped list should hide A's own not-yet-open \
         problem {future_problem} -- if this fails the fixture itself is broken, not the \
         standalone route"
    );

    let res = fx
        .app
        .get_with_token(&routes::problem(future_problem), &a.token)
        .await;
    assert_eq!(
        res.status, 404,
        "LEAK: GET /problems/{{id}} returned {} (body: {}) for A's own not-yet-open bracket \
         problem {future_problem} -- the contest-scoped list correctly returns 404-equivalent \
         (absence) for the same resource",
        res.status, res.text
    );
}

#[tokio::test]
async fn defect_standalone_attachment_download_leaks_own_not_yet_open_bracket_problem() {
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
        "LEAK: attachment download returned {} for A's own not-yet-open bracket problem \
         {future_problem}'s attachment -- should be denied like the problem detail is",
        res.status().as_u16()
    );
}

#[tokio::test]
async fn defect_standalone_sample_test_case_leaks_own_not_yet_open_bracket_problem() {
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
        "LEAK: GET /problems/{{id}}/test-cases/{{tc_id}} returned {} (body: {}) for A's own \
         not-yet-open bracket problem {future_problem}'s sample test case",
        res.status, res.text
    );
}

// =====================================================================
// Area 2 -- the single most severe confirmed defect: the standalone
// submission route skips the bracket's gate ENTIRELY, not just its
// visibility narrowing.
// =====================================================================

#[tokio::test]
#[ignore = "DEFECT (host-side residual, confirmed separate from the reachability leak this \
            file's sibling tests document -- see the plugin-side visibility fix's report at \
            .superpowers/sdd/2026-09-19-afternoon-bracket/standalone-visibility-fix-report.md): \
            this test's scenario is A submitting to B's CURRENT (opponent's) problem, which is \
            READABLE by A per the rule table's own 'opponent allowed once ordering reached' rule \
            (see this file's `submitting_during_ordering_phase_is_rejected_as_match_not_started` \
            comment, and `visibility.rs`'s `the_same_problem_is_allowed_to_the_opponent_and_\
            denied_to_its_owner` unit test) -- so `kernel.decide(Action::Submit, Resource::\
            Problem{contest_id: None, problem_id})` correctly resolves Allow, matching the \
            IDENTICAL contest-scoped call in `create_contest_submission` (also Allow, confirmed \
            by this test's own sanity check below expecting 400 NOT_YOUR_PROBLEM, not 404). The \
            standalone visibility fix (this plugin's `decide_problem_for_contest`, shared \
            verbatim by both the contest-scoped and context-free arms of `decide_visibility_\
            decisions`) is therefore proven correct and CANNOT distinguish this case -- it is \
            reachability, not a business rule, by explicit design (`create_submission`'s own \
            code comment: 'REACHABILITY FIRST... same as viewing the problem'). The 400 \
            NOT_YOUR_PROBLEM the contest-scoped route correctly returns comes ENTIRELY from the \
            `before_submission` hook (gate.rs::check_submission_response), a separate, later \
            check. `VisibilityQueryInput.action` IS on the wire (`packages/broccoli-types/src/\
            types/visibility.rs`), so an action-aware plugin rule (stricter for \"submit\" than \
            \"view\") was considered and REJECTED: since `create_contest_submission` gates \
            Action::Submit through the exact same `Resource::Problem{contest_id: Some(cid)}` \
            arm, tightening it there too would flip the SAME currently-passing 400 \
            NOT_YOUR_PROBLEM sanity check below (and likely afternoon_bracket.rs's own gate-probe \
            tests) to 404 -- collapsing the design's deliberate reachability/business-rule \
            separation and regressing already-passing coverage, not fixing this one. The actual \
            root cause remains exactly as originally diagnosed: `hooks::fetch_resource_\
            enablements` (packages/server/src/hooks.rs) only adds its \"contest\"/\"contest_\
            problem\"-scoped SQL conditions inside `if let Some(cid) = contest_id` -- skipped \
            entirely when `create_submission` passes `contest_id: None` -- so the bracket's \
            CONTEST-scoped before_submission enablement row is never found and the hook never \
            dispatches on this path. This is HOST code (packages/server/src/hooks.rs or \
            create_submission's contest resolution), out of scope for a plugin-only fix and \
            explicitly excluded by this task's constraints; fixing it requires either \
            `fetch_resource_enablements` not dropping contest-scoped rows when `contest_id: \
            None`, or `create_submission` resolving the problem's bracket contest (the SAME \
            batched `contest_problem` join this plugin's visibility fix already performs) and \
            passing it through to the hook dispatch, not just the kernel check."]
async fn defect_standalone_submission_route_bypasses_bracket_gate_entirely() {
    let fx = setup_fixture().await;
    order_match_0(&fx).await;
    start_match_0(&fx).await;

    let a = &fx.players[0];
    let opponents_current_problem = fx.rounds[0].group_b[0];

    // Sanity: the CORRECT, contest-scoped route rejects this submission with
    // the plugin's own distinct code -- matches
    // `afternoon_bracket.rs`'s "Gate probe (a)".
    let res = submit(
        &fx.app,
        fx.contest_id,
        opponents_current_problem,
        &a.token,
        "ACCEPT",
    )
    .await;
    assert_eq!(
        res.status, 400,
        "sanity check failed: the contest-scoped submit should reject A submitting to B's \
         current problem: {}",
        res.text
    );
    assert_eq!(res.body["code"], "NOT_YOUR_PROBLEM");

    // The standalone route should reject the identical submission the same
    // way (or, at an absolute minimum, must not silently accept it as a
    // real, judged submission).
    let res = fx
        .app
        .post_with_token(
            &routes::problem_submissions(opponents_current_problem),
            &json!({
                "files": [{"filename": "main.cpp", "content": "ACCEPT"}],
                "language": "cpp",
            }),
            &a.token,
        )
        .await;
    assert_ne!(
        res.status, 201,
        "GATE BYPASS: POST /problems/{{id}}/submissions (standalone) accepted A's submission \
         to B's current (opponent's) problem {opponents_current_problem} with status {} (body: \
         {}) -- the bracket's before_submission gate was never invoked on this path at all",
        res.status, res.text
    );
}

// =====================================================================
// Area 2 -- passing probes: confirm real enforcement, and its distinct
// codes, through the CORRECT (contest-scoped) route.
// =====================================================================

/// New, not covered by `afternoon_bracket.rs`: submitting during the
/// `Ordering` phase (match created, both rankings not necessarily in yet,
/// `/start` never called) reaches a DIFFERENT rejection code than either of
/// `afternoon_bracket.rs`'s two gate probes. Once B has ranked A's group
/// (submitter == B writes `order_a`, see `ordering.rs`'s module doc
/// comment), the kernel's own `decide_problem` grants B (the opponent) read
/// access as soon as `state != Pending` -- `Ordering` qualifies -- so this
/// request reaches the plugin's `before_submission` hook, which then
/// rejects it as `MATCH_NOT_STARTED` (`gate.rs::resolve_rejection`: `m.state
/// != InProgress && m.state != Tiebreak`). Unlike the leaks above, this is a
/// confirmation that the gate works correctly and reachably through the
/// real HTTP path.
#[tokio::test]
async fn submitting_during_ordering_phase_is_rejected_as_match_not_started() {
    let fx = setup_fixture().await;

    let a = &fx.players[0];
    let b = &fx.players[1];

    // Only B ranks A's group -- enough to grant B (opponent) kernel-level
    // read access to it, per `decide_problem`'s "reached ordering or later"
    // rule -- but `/matches/0/start` is deliberately never called.
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

/// New, not covered by `afternoon_bracket.rs`: an eliminated player (B, who
/// lost round-1 match 0) probing a round they were never part of at all
/// (round 2, where A now plays C). `find_players_match` finds no match for B
/// in round 2, so BOTH the kernel's own reachability check AND (were it ever
/// reached) `gate.rs`'s pure `resolve_rejection` would say "not this
/// viewer's round" -- but per `decide_problem`, the kernel denies first,
/// generically, before the hook is ever dispatched. This documents the
/// SAME architectural asymmetry `afternoon_bracket.rs`'s "Gate probe (b)"
/// already reports as accepted (kernel-level denial pre-empts the plugin's
/// own reasoned rejection code) -- not a new finding, but confirms it holds
/// for the eliminated-player case specifically, which that file never
/// drives a match to.
#[tokio::test]
async fn eliminated_player_cannot_submit_to_a_round_they_have_no_match_in() {
    let fx = setup_fixture().await;
    order_match_0(&fx).await;
    start_match_0(&fx).await;

    let a = &fx.players[0];
    let b = &fx.players[1];
    let c = &fx.players[2];

    // Force-decide match 0 (A beats B) and match 1 (C beats D) so round 2's
    // match (id 8, A vs C -- see `afternoon_bracket.rs`'s comment on
    // `storage::match_id_for`'s slot scheme) exists.
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
         the kernel (matching the accepted own-future-problem asymmetry), got: {}",
        res.text
    );
    assert_eq!(res.body["code"], "NOT_FOUND");
}

/// New, not covered by `afternoon_bracket.rs`: a user who registered for the
/// contest (so contest-level participation checks pass) but was never
/// seeded into the bracket at all -- `find_players_match` never finds them
/// in ANY round. Same asymmetry as the eliminated-player probe above:
/// denied at the kernel, generically, before the gate's own `NOT_IN_MATCH`
/// code is ever produced.
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
// Area 1 -- `submission:view_all` revocation mid-contest.
//
// `afternoon_bracket.rs` already confirms the "sees everything" half; this
// adds the "revoked mid-contest" half the QA plan separately calls out.
// =====================================================================

/// `AuthUser` (`packages/server/src/extractors/auth.rs`) is stateless: its
/// `permissions` come straight from the JWT's claims, baked in at
/// login/registration time, and are never re-checked against the database
/// on a read/submit path (only `FreshAuthUser`, reserved for high-value
/// mutation handlers, re-checks `credentials_changed_at`). So revoking a
/// role's permission mid-contest does NOT retroactively narrow an
/// already-issued access token's visibility -- only a freshly minted token
/// (re-login) picks up the change. This is a documented, deliberate
/// tradeoff elsewhere in this codebase (stateless reads/submits stay fast;
/// only mutations pay for freshness), not a bracket-specific bug, but the
/// QA plan asks to confirm it explicitly for this permission in this
/// plugin's context -- this test nails down exactly where the line falls:
/// the STALE token keeps the spectator's view; a FRESH token (re-login)
/// loses it.
#[tokio::test]
async fn revoking_view_all_mid_contest_does_not_narrow_an_already_issued_token() {
    let fx = setup_fixture().await;
    order_match_0(&fx).await;
    start_match_0(&fx).await;

    let spectator_username = "qa_spectator";
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

    // Revoke the permission directly in the DB -- what an admin's "remove
    // role permission" action ultimately does.
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

    // The STALE token (minted before the revocation) still carries the old
    // permission -- it keeps seeing everything.
    let ids = visible_problem_ids(&fx.app, fx.contest_id, &stale_token).await;
    assert!(
        ids.contains(&(future_problem as i64)),
        "the stale token should still see everything: stateless AuthUser reads permissions \
         from the JWT's claims, not the database, on this path"
    );

    // A FRESH token (re-login) picks up the revocation.
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
