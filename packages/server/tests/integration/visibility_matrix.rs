//! Visibility/reachability pin (SDD 2026-09-15-visibility-kernel, Task 1).
//!
//! This module is a safety net, not a spec. It records WHAT THE SERVER
//! CURRENTLY DOES for a representative slice of the
//! resource x viewer x window x soft-delete cross-product described in
//! `.superpowers/sdd/2026-09-15-visibility-kernel/task-1-brief.md`, so that
//! later tasks which relocate this logic (`utils/contest.rs`,
//! `handlers/submission/filter.rs`, `utils/soft_delete.rs`) can prove they
//! preserved it byte-for-byte. Every test name says which rule it pins;
//! a failure here means a later task changed behaviour, not that this
//! assertion needs "fixing" (unless the assertion itself is provably wrong).
//!
//! Full coverage selection (not the full 256-cell cross product):
//! - all 4 window states x all 4 viewer kinds on `list_contest_problems`
//!   (the densest gate: `check_contest_access` + `require_contest_started`).
//! - anonymous / non-participant / participant / admin on each of the other
//!   7 resources, held at a single representative window (private contest,
//!   inside its activation window).
//! - soft-delete pinning for contest, problem, and (the closest analogue
//!   that exists) submission-via-contest.
//! - a handful of additional cells added because reading the source
//!   surfaced rules the above selection does not exercise (each is called
//!   out with a `// NOTE:` and reported separately).
//!
//! A code-review pass on this file flagged that a PRIVATE contest cannot
//! isolate the activation-window check from the participant check: both
//! produce an identical `404 NOT_FOUND` for a non-participant, so a
//! non-participant cell on a private contest cannot tell the difference
//! between "denied by the window" and "denied because not enrolled" - and
//! would stay green even if the window-gate block in `check_contest_access`
//! were deleted outright. `contest_problem_list_matrix` therefore also has a
//! `window_gate_public_contest_*` group that repeats the 4 window states on
//! a PUBLIC contest with a non-participant viewer, mirroring the existing
//! precedent in `packages/server/src/utils/contest.rs`'s
//! `contest_access_tests` (`public_but_deactivated_contest_is_not_found_for_unprivileged_user`,
//! `public_not_yet_activated_contest_is_not_found_for_unprivileged_user`,
//! `public_in_window_contest_is_accessible_to_any_user`) where `is_public`
//! removes the participant check entirely and the outcome flips purely on
//! the window. The private-contest `non_participant_*` cells are kept
//! because they legitimately pin a different rule (a private contest denies
//! non-members no matter the window) but are named accordingly, not as
//! window coverage.
//!
//! Verifying that fix (per the reviewer's ask: temporarily delete
//! `check_contest_access`'s window-gate block and confirm new cells fail)
//! surfaced a SECOND, deeper masking: `require_contest_started`
//! (`packages/server/src/utils/contest.rs`, right below `check_contest_access`)
//! contains a byte-identical copy of the same window predicate
//! (`activate_time.is_none_or(|at| at > now) || deactivate_time.is_some_and(|dt| dt <= now)`),
//! and `list_contest_problems` calls BOTH `check_contest_access` and
//! `require_contest_started` on the same contest/now. Because the two
//! predicates are identical, they can never diverge for the same request -
//! so no black-box cell on `list_contest_problems`, public contest or not,
//! can isolate `check_contest_access`'s copy specifically; disabling ONLY
//! `check_contest_access`'s block leaves `require_contest_started`'s copy
//! fully enforcing the window and every `list_contest_problems` cell (public
//! or private) keeps passing. The `window_gate_public_contest_*` group in
//! `contest_problem_list_matrix` therefore pins the AGGREGATE window
//! enforcement of that endpoint's full gate chain, not `check_contest_access`
//! in isolation. The cells that genuinely isolate `check_contest_access`
//! live in `contest_detail`'s `window_gate_isolated_public_contest_*` group,
//! because `get_contest` calls `check_contest_access` alone (no
//! `require_contest_started`) - those are the ones verified (see the task
//! report) to flip from 404 to 200 when the block under test is deleted.

use serde_json::json;

use crate::common::{TestApp, routes};

// ---------------------------------------------------------------------
// Shared fixtures
// ---------------------------------------------------------------------

/// One of the 4 canonical contest activation windows from the brief.
/// `hours` in the brief become fixed absolute timestamps here since the
/// integration harness has no clock control - "now" is always real wall
/// time (2026), so anchoring far in the past/future keeps the cell stable.
#[derive(Clone, Copy)]
struct Window {
    activate: Option<&'static str>,
    start: &'static str,
    end: &'static str,
    deactivate: Option<&'static str>,
}

/// `activate_time` is in the future: contest not yet activated.
const BEFORE_ACTIVATION: Window = Window {
    activate: Some("2099-01-01T00:00:00Z"),
    start: "2099-01-01T00:00:00Z",
    end: "2099-01-02T00:00:00Z",
    deactivate: None,
};

/// Activated, started, not deactivated, not ended: the "running" state.
const INSIDE_WINDOW: Window = Window {
    activate: Some("2020-01-01T00:00:00Z"),
    start: "2020-01-01T00:00:00Z",
    end: "2099-01-02T00:00:00Z",
    deactivate: None,
};

/// `deactivate_time` is in the past: contest archived/deactivated.
const AFTER_DEACTIVATION: Window = Window {
    activate: Some("2020-01-01T00:00:00Z"),
    start: "2020-01-01T00:00:00Z",
    end: "2020-01-02T00:00:00Z",
    deactivate: Some("2020-01-03T00:00:00Z"),
};

/// `activate_time` is NULL. `check_contest_access` treats this as "never
/// activated" (`is_none_or` on `None` is `true`), i.e. out of window,
/// regardless of how far in the past `start_time`/`end_time` are.
const NULL_ACTIVATE_TIME: Window = Window {
    activate: None,
    start: "2020-01-01T00:00:00Z",
    end: "2099-01-02T00:00:00Z",
    deactivate: None,
};

/// Extra window (not one of the brief's 4) used only to pin
/// `require_contest_started`, a rule the 4 canonical windows above never
/// independently exercise (in all 4, activate<=now iff start<=now).
const ACTIVATED_BUT_NOT_STARTED: Window = Window {
    activate: Some("2020-01-01T00:00:00Z"),
    start: "2099-01-01T00:00:00Z",
    end: "2099-01-02T00:00:00Z",
    deactivate: None,
};

async fn create_contest_window(
    app: &TestApp,
    admin_token: &str,
    title: &str,
    is_public: bool,
    window: Window,
) -> i32 {
    let body = json!({
        "title": title,
        "description": "Visibility matrix fixture contest",
        "activate_time": window.activate,
        "start_time": window.start,
        "end_time": window.end,
        "deactivate_time": window.deactivate,
        "is_public": is_public,
        "submissions_visible": true,
    });
    let res = app.post_with_token(routes::CONTESTS, &body, admin_token).await;
    assert_eq!(
        res.status, 201,
        "create_contest_window failed: {}",
        res.text
    );
    res.id()
}

async fn user_id(app: &TestApp, token: &str) -> i32 {
    app.get_with_token(routes::ME, token).await.id()
}

/// Enrolls `participant_token`'s user into `contest_id` via the
/// `contest:manage`-gated admin endpoint. Unlike self-registration
/// (`register_for_contest`), `add_participant` has no activation-window or
/// `is_public` check, so it can seed a participant into a contest in ANY
/// window state - required for the before/after/null-activate cells of the
/// 16-cell matrix below.
async fn enroll_participant(app: &TestApp, admin_token: &str, contest_id: i32, participant_token: &str) {
    let uid = user_id(app, participant_token).await;
    let res = app
        .post_with_token(
            &routes::contest_participants(contest_id),
            &json!({"user_id": uid}),
            admin_token,
        )
        .await;
    assert_eq!(res.status, 201, "enroll_participant failed: {}", res.text);
}

fn contest_problem_samples_path(contest_id: i32, problem_id: i32) -> String {
    format!("/api/v1/contests/{contest_id}/problems/{problem_id}/samples")
}

async fn submit_to_contest_problem(
    app: &TestApp,
    contest_id: i32,
    problem_id: i32,
    token: &str,
) -> i32 {
    let res = app
        .post_with_token(
            &routes::contest_problem_submissions(contest_id, problem_id),
            &json!({
                "files": [{"filename": "main.cpp", "content": "int main(){return 0;}"}],
                "language": "cpp",
            }),
            token,
        )
        .await;
    assert_eq!(
        res.status, 201,
        "submit_to_contest_problem failed: {}",
        res.text
    );
    res.id()
}

// ---------------------------------------------------------------------
// Densest gate: list_contest_problems x {4 windows} x {4 viewer kinds}
// Gate = check_contest_access + require_contest_started.
// The primary fixture contest is private (is_public=false) so non-participant
// and participant are meaningfully different for the participant/admin cells.
// A SEPARATE public-contest fixture (`setup_public`, below) isolates the
// window check itself from the participant check - see the
// `window_gate_public_contest_*` group.
// ---------------------------------------------------------------------
mod contest_problem_list_matrix {
    use super::*;

    async fn setup(window: Window) -> (TestApp, String, String, String, i32) {
        let app = TestApp::spawn().await;
        let admin = app.create_user_with_role("admin", "pass1234", "admin").await;
        let non_participant = app.create_authenticated_user("outsider", "pass1234").await;
        let participant = app.create_authenticated_user("participant", "pass1234").await;
        let contest_id = create_contest_window(&app, &admin, "Matrix Contest", false, window).await;
        enroll_participant(&app, &admin, contest_id, &participant).await;
        (app, admin, non_participant, participant, contest_id)
    }

    /// Public-contest fixture used ONLY to isolate the window check
    /// (`check_contest_access`'s window-gate block) from the participant
    /// check. For a public contest, once the window-gate block is passed,
    /// `check_contest_access` returns `Ok` unconditionally on `is_public`
    /// without ever looking up `contest_user` - so a non-participant's
    /// outcome flips purely on the window state, matching the precedent unit
    /// tests in `packages/server/src/utils/contest.rs::contest_access_tests`.
    async fn setup_public(window: Window) -> (TestApp, String, i32) {
        let app = TestApp::spawn().await;
        let admin = app.create_user_with_role("admin", "pass1234", "admin").await;
        let non_participant = app.create_authenticated_user("outsider", "pass1234").await;
        let contest_id =
            create_contest_window(&app, &admin, "Public Window Gate Contest", true, window).await;
        (app, non_participant, contest_id)
    }

    // --- anonymous: these pin that authentication is mandatory, NOT the
    // window. `AuthUser` is a required axum extractor that rejects with
    // TOKEN_MISSING before the handler body - and therefore before
    // check_contest_access - ever runs, so the outcome is identical in every
    // window state. Four near-identical cells are kept (one per window) only
    // to document that the mandatory-auth extractor really does run first in
    // every window, not to claim window coverage. ---

    #[tokio::test]
    async fn anonymous_rejected_by_mandatory_auth_before_activation() {
        let (app, _, _, _, contest_id) = setup(BEFORE_ACTIVATION).await;
        let res = app.get_without_token(&routes::contest_problems(contest_id)).await;
        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "TOKEN_MISSING");
    }

    #[tokio::test]
    async fn anonymous_rejected_by_mandatory_auth_inside_window() {
        let (app, _, _, _, contest_id) = setup(INSIDE_WINDOW).await;
        let res = app.get_without_token(&routes::contest_problems(contest_id)).await;
        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "TOKEN_MISSING");
    }

    #[tokio::test]
    async fn anonymous_rejected_by_mandatory_auth_after_deactivation() {
        let (app, _, _, _, contest_id) = setup(AFTER_DEACTIVATION).await;
        let res = app.get_without_token(&routes::contest_problems(contest_id)).await;
        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "TOKEN_MISSING");
    }

    #[tokio::test]
    async fn anonymous_rejected_by_mandatory_auth_null_activate_time() {
        let (app, _, _, _, contest_id) = setup(NULL_ACTIVATE_TIME).await;
        let res = app.get_without_token(&routes::contest_problems(contest_id)).await;
        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "TOKEN_MISSING");
    }

    // --- non-participant on a PRIVATE contest: NOT_FOUND in every window.
    // IMPORTANT: this does NOT pin the window gate. `check_contest_access`
    // has two independent paths that both produce this exact 404 for a
    // private contest - the window-gate block, and (once inside the window)
    // the participant-check fallback - and a black-box HTTP test cannot tell
    // which path fired. These cells legitimately pin "a private contest
    // denies a non-member no matter the window state," a real and
    // independent rule, but they would ALL stay green even if the
    // window-gate block were deleted outright (the participant-check
    // fallback masks it). The cells that actually isolate and pin the window
    // arithmetic are the `window_gate_public_contest_*` group below. ---

    #[tokio::test]
    async fn private_contest_denies_non_participant_before_activation() {
        let (app, _, non_participant, _, contest_id) = setup(BEFORE_ACTIVATION).await;
        let res = app
            .get_with_token(&routes::contest_problems(contest_id), &non_participant)
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn private_contest_denies_non_participant_inside_window() {
        let (app, _, non_participant, _, contest_id) = setup(INSIDE_WINDOW).await;
        let res = app
            .get_with_token(&routes::contest_problems(contest_id), &non_participant)
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn private_contest_denies_non_participant_after_deactivation() {
        let (app, _, non_participant, _, contest_id) = setup(AFTER_DEACTIVATION).await;
        let res = app
            .get_with_token(&routes::contest_problems(contest_id), &non_participant)
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn private_contest_denies_non_participant_null_activate_time() {
        let (app, _, non_participant, _, contest_id) = setup(NULL_ACTIVATE_TIME).await;
        let res = app
            .get_with_token(&routes::contest_problems(contest_id), &non_participant)
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    // --- window_gate_public_contest_*: a public contest removes the
    // participant-check fallback (`is_public` short-circuits before the
    // `contest_user` lookup), so these pin the AGGREGATE window enforcement
    // of this endpoint's full gate chain (`check_contest_access` +
    // `require_contest_started`) purely on window state, with no
    // participant-membership confound.
    //
    // CAVEAT (found while verifying this fix bites, see task-1-report.md):
    // these do NOT isolate `check_contest_access`'s window-gate block from
    // `require_contest_started`'s block. Both functions contain the exact
    // same window predicate applied to the exact same contest/now, and
    // `list_contest_problems` calls both, so the two can never diverge for
    // one request - deleting ONLY `check_contest_access`'s copy leaves
    // `require_contest_started`'s copy still enforcing the window and these
    // 4 cells keep passing unchanged. For a genuine isolation of
    // `check_contest_access` alone, see `contest_detail`'s
    // `window_gate_isolated_public_contest_*` group, which hits `get_contest`
    // - an endpoint that calls `check_contest_access` only. ---

    #[tokio::test]
    async fn window_gate_public_contest_before_activation_is_404() {
        let (app, non_participant, contest_id) = setup_public(BEFORE_ACTIVATION).await;
        let res = app
            .get_with_token(&routes::contest_problems(contest_id), &non_participant)
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn window_gate_public_contest_inside_window_is_200() {
        let (app, non_participant, contest_id) = setup_public(INSIDE_WINDOW).await;
        let res = app
            .get_with_token(&routes::contest_problems(contest_id), &non_participant)
            .await;
        assert_eq!(
            res.status, 200,
            "positive control: a non-participant IS let in on a public in-window contest"
        );
        assert!(res.body.is_array());
    }

    #[tokio::test]
    async fn window_gate_public_contest_after_deactivation_is_404() {
        let (app, non_participant, contest_id) = setup_public(AFTER_DEACTIVATION).await;
        let res = app
            .get_with_token(&routes::contest_problems(contest_id), &non_participant)
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn window_gate_public_contest_null_activate_time_is_404() {
        let (app, non_participant, contest_id) = setup_public(NULL_ACTIVATE_TIME).await;
        let res = app
            .get_with_token(&routes::contest_problems(contest_id), &non_participant)
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    // --- participant: NOT_FOUND outside the window (the window gate in
    // check_contest_access runs BEFORE the participant/is_public check, so
    // being enrolled does not help outside the window), 200 inside it ---

    #[tokio::test]
    async fn participant_before_activation_is_404() {
        let (app, _, _, participant, contest_id) = setup(BEFORE_ACTIVATION).await;
        let res = app
            .get_with_token(&routes::contest_problems(contest_id), &participant)
            .await;
        assert_eq!(
            res.status, 404,
            "out-of-window contest must 404 even for an enrolled participant"
        );
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn participant_inside_window_is_200() {
        let (app, _, _, participant, contest_id) = setup(INSIDE_WINDOW).await;
        let res = app
            .get_with_token(&routes::contest_problems(contest_id), &participant)
            .await;
        assert_eq!(res.status, 200);
        assert!(res.body.is_array(), "body shape: array of contest problems");
    }

    #[tokio::test]
    async fn participant_after_deactivation_is_404() {
        let (app, _, _, participant, contest_id) = setup(AFTER_DEACTIVATION).await;
        let res = app
            .get_with_token(&routes::contest_problems(contest_id), &participant)
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn participant_null_activate_time_is_404() {
        let (app, _, _, participant, contest_id) = setup(NULL_ACTIVATE_TIME).await;
        let res = app
            .get_with_token(&routes::contest_problems(contest_id), &participant)
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    // --- admin (contest:manage): bypasses the window entirely, in every
    // cell, including a not-yet-activated or already-deactivated private
    // contest ---

    #[tokio::test]
    async fn admin_before_activation_is_200() {
        let (app, admin, _, _, contest_id) = setup(BEFORE_ACTIVATION).await;
        let res = app
            .get_with_token(&routes::contest_problems(contest_id), &admin)
            .await;
        assert_eq!(res.status, 200);
        assert!(res.body.is_array());
    }

    #[tokio::test]
    async fn admin_inside_window_is_200() {
        let (app, admin, _, _, contest_id) = setup(INSIDE_WINDOW).await;
        let res = app
            .get_with_token(&routes::contest_problems(contest_id), &admin)
            .await;
        assert_eq!(res.status, 200);
        assert!(res.body.is_array());
    }

    #[tokio::test]
    async fn admin_after_deactivation_is_200() {
        let (app, admin, _, _, contest_id) = setup(AFTER_DEACTIVATION).await;
        let res = app
            .get_with_token(&routes::contest_problems(contest_id), &admin)
            .await;
        assert_eq!(res.status, 200);
        assert!(res.body.is_array());
    }

    #[tokio::test]
    async fn admin_null_activate_time_is_200() {
        let (app, admin, _, _, contest_id) = setup(NULL_ACTIVATE_TIME).await;
        let res = app
            .get_with_token(&routes::contest_problems(contest_id), &admin)
            .await;
        assert_eq!(res.status, 200);
        assert!(res.body.is_array());
    }

    // --- extra rule not covered by the 4 canonical windows:
    // require_contest_started, layered on top of check_contest_access ---

    /// NOTE: activated-but-not-yet-started is a DIFFERENT denial than the
    /// window gate: it is a 400 VALIDATION_ERROR ("Contest has not started
    /// yet"), not a 404. A participant can tell the difference between "this
    /// contest does not exist / isn't active yet" (404) and "this contest
    /// exists, is active, but hasn't started" (400) - the latter leaks
    /// existence by design (list_contests already shows it).
    #[tokio::test]
    async fn participant_activated_but_not_started_is_400_validation_error() {
        let (app, _, _, participant, contest_id) = setup(ACTIVATED_BUT_NOT_STARTED).await;
        let res = app
            .get_with_token(&routes::contest_problems(contest_id), &participant)
            .await;
        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn admin_activated_but_not_started_bypasses_and_is_200() {
        let (app, admin, _, _, contest_id) = setup(ACTIVATED_BUT_NOT_STARTED).await;
        let res = app
            .get_with_token(&routes::contest_problems(contest_id), &admin)
            .await;
        assert_eq!(
            res.status, 200,
            "contest:manage bypasses require_contest_started too"
        );
    }
}

// ---------------------------------------------------------------------
// Remaining resources: anonymous / non-participant / participant / admin,
// held at a single window (private contest, inside its activation window).
// ---------------------------------------------------------------------

mod contest_detail {
    use super::*;

    struct Fixture {
        app: TestApp,
        admin: String,
        non_participant: String,
        participant: String,
        contest_id: i32,
    }

    async fn setup() -> Fixture {
        let app = TestApp::spawn().await;
        let admin = app.create_user_with_role("admin", "pass1234", "admin").await;
        let non_participant = app.create_authenticated_user("outsider", "pass1234").await;
        let participant = app.create_authenticated_user("participant", "pass1234").await;
        let contest_id =
            create_contest_window(&app, &admin, "Contest Detail Fixture", false, INSIDE_WINDOW).await;
        enroll_participant(&app, &admin, contest_id, &participant).await;
        Fixture { app, admin, non_participant, participant, contest_id }
    }

    #[tokio::test]
    async fn anonymous_is_401() {
        let f = setup().await;
        let res = f.app.get_without_token(&routes::contest(f.contest_id)).await;
        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "TOKEN_MISSING");
    }

    #[tokio::test]
    async fn non_participant_is_404() {
        let f = setup().await;
        let res = f
            .app
            .get_with_token(&routes::contest(f.contest_id), &f.non_participant)
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn participant_is_200() {
        let f = setup().await;
        let res = f
            .app
            .get_with_token(&routes::contest(f.contest_id), &f.participant)
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["id"], f.contest_id);
    }

    #[tokio::test]
    async fn admin_is_200() {
        let f = setup().await;
        let res = f
            .app
            .get_with_token(&routes::contest(f.contest_id), &f.admin)
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["id"], f.contest_id);
    }

    /// Public-contest fixture used ONLY to isolate `check_contest_access`'s
    /// window-gate block from everything else. `get_contest` calls
    /// `find_contest` + `check_contest_access` and NOTHING else - no
    /// `require_contest_started`, so (unlike `list_contest_problems`, see
    /// `contest_problem_list_matrix`'s `window_gate_public_contest_*`
    /// comment) there is no second, duplicate window check downstream to
    /// mask a broken `check_contest_access`. Combined with `is_public`
    /// removing the participant-check fallback, a non-participant's outcome
    /// here depends on NOTHING but the window-gate block under test.
    async fn setup_public(window: Window) -> (TestApp, String, i32) {
        let app = TestApp::spawn().await;
        let admin = app.create_user_with_role("admin", "pass1234", "admin").await;
        let non_participant = app.create_authenticated_user("outsider", "pass1234").await;
        let contest_id =
            create_contest_window(&app, &admin, "Isolated Window Gate Contest", true, window).await;
        (app, non_participant, contest_id)
    }

    // --- window_gate_isolated_public_contest_*: THIS is the group that
    // genuinely isolates and pins `check_contest_access`'s window-gate block
    // in isolation, verified as follows (see task-1-report.md for the actual
    // numbers): with `packages/server/src/utils/contest.rs`'s window-gate
    // block (the one inside `check_contest_access`) temporarily deleted, the
    // 3 out-of-window cells below flip from 404 to 200 and FAIL, while
    // restoring the block byte-identical makes the whole suite pass again.
    // Mirrors the precedent in
    // `packages/server/src/utils/contest.rs::contest_access_tests`
    // (`public_but_deactivated_contest_is_not_found_for_unprivileged_user`,
    // `public_not_yet_activated_contest_is_not_found_for_unprivileged_user`,
    // `public_in_window_contest_is_accessible_to_any_user`). ---

    #[tokio::test]
    async fn window_gate_isolated_public_contest_before_activation_is_404() {
        let (app, non_participant, contest_id) = setup_public(BEFORE_ACTIVATION).await;
        let res = app.get_with_token(&routes::contest(contest_id), &non_participant).await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn window_gate_isolated_public_contest_inside_window_is_200() {
        let (app, non_participant, contest_id) = setup_public(INSIDE_WINDOW).await;
        let res = app.get_with_token(&routes::contest(contest_id), &non_participant).await;
        assert_eq!(
            res.status, 200,
            "positive control: a non-participant IS let in on a public in-window contest"
        );
        assert_eq!(res.body["id"], contest_id);
    }

    #[tokio::test]
    async fn window_gate_isolated_public_contest_after_deactivation_is_404() {
        let (app, non_participant, contest_id) = setup_public(AFTER_DEACTIVATION).await;
        let res = app.get_with_token(&routes::contest(contest_id), &non_participant).await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn window_gate_isolated_public_contest_null_activate_time_is_404() {
        let (app, non_participant, contest_id) = setup_public(NULL_ACTIVATE_TIME).await;
        let res = app.get_with_token(&routes::contest(contest_id), &non_participant).await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }
}

mod contest_problem_detail {
    //! There is no `GET /contests/{id}/problems/{problem_id}` single-item
    //! endpoint (only PATCH/DELETE exist there). The read path for a single
    //! contest problem's statement is the standalone `GET /problems/{id}`,
    //! gated by `require_problem_read_access` -> `can_access_problem_via_contest`
    //! for a non-public problem.
    use super::*;

    struct Fixture {
        app: TestApp,
        admin: String,
        non_participant: String,
        participant: String,
        problem_id: i32,
    }

    async fn setup() -> Fixture {
        let app = TestApp::spawn().await;
        let admin = app.create_user_with_role("admin", "pass1234", "admin").await;
        let non_participant = app.create_authenticated_user("outsider", "pass1234").await;
        let participant = app.create_authenticated_user("participant", "pass1234").await;
        let contest_id =
            create_contest_window(&app, &admin, "Problem Detail Fixture", false, INSIDE_WINDOW).await;
        let problem_id = app.create_hidden_problem(&admin, "Hidden Problem").await;
        app.add_problem_to_contest(contest_id, problem_id, &admin).await;
        enroll_participant(&app, &admin, contest_id, &participant).await;
        Fixture { app, admin, non_participant, participant, problem_id }
    }

    #[tokio::test]
    async fn anonymous_is_401() {
        let f = setup().await;
        let res = f.app.get_without_token(&routes::problem(f.problem_id)).await;
        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "TOKEN_MISSING");
    }

    #[tokio::test]
    async fn non_participant_is_404() {
        let f = setup().await;
        let res = f
            .app
            .get_with_token(&routes::problem(f.problem_id), &f.non_participant)
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn participant_is_200() {
        let f = setup().await;
        let res = f
            .app
            .get_with_token(&routes::problem(f.problem_id), &f.participant)
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["id"], f.problem_id);
    }

    #[tokio::test]
    async fn admin_is_200() {
        let f = setup().await;
        let res = f
            .app
            .get_with_token(&routes::problem(f.problem_id), &f.admin)
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["id"], f.problem_id);
    }
}

mod contest_problem_sample {
    use super::*;

    struct Fixture {
        app: TestApp,
        admin: String,
        non_participant: String,
        participant: String,
        contest_id: i32,
        problem_id: i32,
    }

    async fn setup(is_public: bool) -> Fixture {
        let app = TestApp::spawn().await;
        let admin = app.create_user_with_role("admin", "pass1234", "admin").await;
        let non_participant = app.create_authenticated_user("outsider", "pass1234").await;
        let participant = app.create_authenticated_user("participant", "pass1234").await;
        let contest_id =
            create_contest_window(&app, &admin, "Sample Fixture", is_public, INSIDE_WINDOW).await;
        let problem_id = app.create_problem(&admin, "Sample Problem").await;
        app.add_problem_to_contest(contest_id, problem_id, &admin).await;
        app.create_test_case(problem_id, &admin).await;
        enroll_participant(&app, &admin, contest_id, &participant).await;
        Fixture { app, admin, non_participant, participant, contest_id, problem_id }
    }

    #[tokio::test]
    async fn anonymous_is_401() {
        let f = setup(false).await;
        let res = f
            .app
            .get_without_token(&contest_problem_samples_path(f.contest_id, f.problem_id))
            .await;
        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "TOKEN_MISSING");
    }

    #[tokio::test]
    async fn non_participant_is_404() {
        let f = setup(false).await;
        let res = f
            .app
            .get_with_token(
                &contest_problem_samples_path(f.contest_id, f.problem_id),
                &f.non_participant,
            )
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn participant_is_200() {
        let f = setup(false).await;
        let res = f
            .app
            .get_with_token(
                &contest_problem_samples_path(f.contest_id, f.problem_id),
                &f.participant,
            )
            .await;
        assert_eq!(res.status, 200);
        assert!(res.body["samples"].is_array());
        assert_eq!(res.body["samples"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn admin_is_200() {
        let f = setup(false).await;
        let res = f
            .app
            .get_with_token(&contest_problem_samples_path(f.contest_id, f.problem_id), &f.admin)
            .await;
        assert_eq!(res.status, 200);
    }

    /// NOTE (docstring/behaviour mismatch): `getContestProblemSamples`'s
    /// OpenAPI description says "Requires the user to be a participant or
    /// have contest:manage permission", but the handler only calls
    /// `check_contest_access` (no `is_contest_participant`/
    /// `require_contest_participant` call). For a PUBLIC contest,
    /// `check_contest_access` grants any authenticated user - enrolled or
    /// not - as soon as the window is open. Samples for a public running
    /// contest are therefore readable by a logged-in non-participant.
    #[tokio::test]
    async fn public_contest_samples_readable_by_non_participant_despite_docstring() {
        let f = setup(true).await;
        let res = f
            .app
            .get_with_token(
                &contest_problem_samples_path(f.contest_id, f.problem_id),
                &f.non_participant,
            )
            .await;
        assert_eq!(
            res.status, 200,
            "check_contest_access does not require participation for a public contest"
        );
    }
}

mod problem_attachment_list {
    use super::*;

    struct Fixture {
        app: TestApp,
        admin: String,
        non_participant: String,
        participant: String,
        problem_id: i32,
    }

    async fn setup() -> Fixture {
        let app = TestApp::spawn().await;
        let admin = app.create_user_with_role("admin", "pass1234", "admin").await;
        let non_participant = app.create_authenticated_user("outsider", "pass1234").await;
        let participant = app.create_authenticated_user("participant", "pass1234").await;
        let contest_id =
            create_contest_window(&app, &admin, "Attachment Fixture", false, INSIDE_WINDOW).await;
        let problem_id = app.create_hidden_problem(&admin, "Attachment Problem").await;
        app.add_problem_to_contest(contest_id, problem_id, &admin).await;
        let upload = app
            .upload_attachment(problem_id, "notes.txt", b"hello".to_vec(), None, &admin)
            .await;
        assert_eq!(upload.status, 201, "attachment upload failed: {}", upload.text);
        enroll_participant(&app, &admin, contest_id, &participant).await;
        Fixture { app, admin, non_participant, participant, problem_id }
    }

    #[tokio::test]
    async fn anonymous_is_401() {
        let f = setup().await;
        let res = f.app.get_without_token(&routes::attachments(f.problem_id)).await;
        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "TOKEN_MISSING");
    }

    #[tokio::test]
    async fn non_participant_is_404() {
        let f = setup().await;
        let res = f
            .app
            .get_with_token(&routes::attachments(f.problem_id), &f.non_participant)
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn participant_is_200() {
        let f = setup().await;
        let res = f
            .app
            .get_with_token(&routes::attachments(f.problem_id), &f.participant)
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["total"], 1);
        assert_eq!(res.body["attachments"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn admin_is_200() {
        let f = setup().await;
        let res = f
            .app
            .get_with_token(&routes::attachments(f.problem_id), &f.admin)
            .await;
        assert_eq!(res.status, 200);
    }
}

mod contest_submission_list {
    use super::*;

    struct Fixture {
        app: TestApp,
        admin: String,
        non_participant: String,
        participant: String,
        contest_id: i32,
    }

    async fn setup() -> Fixture {
        let app = TestApp::spawn().await;
        let admin = app.create_user_with_role("admin", "pass1234", "admin").await;
        let non_participant = app.create_authenticated_user("outsider", "pass1234").await;
        let participant = app.create_authenticated_user("participant", "pass1234").await;
        let contest_id =
            create_contest_window(&app, &admin, "Submission List Fixture", false, INSIDE_WINDOW).await;
        let problem_id = app.create_problem(&admin, "Submission List Problem").await;
        app.add_problem_to_contest(contest_id, problem_id, &admin).await;
        enroll_participant(&app, &admin, contest_id, &participant).await;
        submit_to_contest_problem(&app, contest_id, problem_id, &participant).await;
        Fixture { app, admin, non_participant, participant, contest_id }
    }

    #[tokio::test]
    async fn anonymous_is_401() {
        let f = setup().await;
        let res = f
            .app
            .get_without_token(&routes::contest_submissions(f.contest_id))
            .await;
        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "TOKEN_MISSING");
    }

    #[tokio::test]
    async fn non_participant_is_404() {
        let f = setup().await;
        let res = f
            .app
            .get_with_token(&routes::contest_submissions(f.contest_id), &f.non_participant)
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn participant_is_200() {
        let f = setup().await;
        let res = f
            .app
            .get_with_token(&routes::contest_submissions(f.contest_id), &f.participant)
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["data"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn admin_is_200() {
        let f = setup().await;
        let res = f
            .app
            .get_with_token(&routes::contest_submissions(f.contest_id), &f.admin)
            .await;
        assert_eq!(res.status, 200);
    }

    /// NOTE (permission-key asymmetry): every other contest read path in
    /// this file gates its admin bypass on `contest:manage`. This endpoint
    /// instead bypasses `check_contest_access` entirely for anyone holding
    /// `submission:view_all` (`can_view_all`) - a DIFFERENT permission. The
    /// `problem_setter` role has `submission:view_all` but NOT
    /// `contest:manage`, so a problem_setter can list a private, DEACTIVATED
    /// contest's submissions even though the same user would get 404 from
    /// `getContest` on that same contest. A future kernel that unifies
    /// "does this viewer bypass the window" on `contest:manage` alone would
    /// silently change this endpoint's behaviour for problem_setters.
    #[tokio::test]
    async fn submission_view_all_without_contest_manage_bypasses_deactivated_window_is_200() {
        let app = TestApp::spawn().await;
        let admin = app.create_user_with_role("admin", "pass1234", "admin").await;
        let submitter = app.create_authenticated_user("submitter", "pass1234").await;
        let viewer = app
            .create_user_with_role("viewer_view_all", "pass1234", "problem_setter")
            .await;

        let contest_id =
            create_contest_window(&app, &admin, "Bypass Fixture", false, INSIDE_WINDOW).await;
        let problem_id = app.create_problem(&admin, "Bypass Problem").await;
        app.add_problem_to_contest(contest_id, problem_id, &admin).await;
        enroll_participant(&app, &admin, contest_id, &submitter).await;
        submit_to_contest_problem(&app, contest_id, problem_id, &submitter).await;

        // Move the contest into the past: now deactivated/archived.
        let patch = app
            .patch_with_token(
                &routes::contest(contest_id),
                &json!({
                    "end_time": "2020-01-02T00:00:00Z",
                    "deactivate_time": "2020-01-03T00:00:00Z",
                }),
                &admin,
            )
            .await;
        assert_eq!(patch.status, 200, "patch to deactivate failed: {}", patch.text);

        // Sanity/baseline: check_contest_access denies EVERYONE once the
        // window closes, even an enrolled participant with no special
        // permission - participation only matters INSIDE the window (see
        // contest_problem_list_matrix::participant_after_deactivation_is_404).
        // This is the baseline the submission_view_all bypass below deviates
        // from.
        let baseline = app
            .get_with_token(&routes::contest(contest_id), &submitter)
            .await;
        assert_eq!(
            baseline.status, 404,
            "check_contest_access has no participant carve-out once the window is closed"
        );
        assert_eq!(baseline.body["code"], "NOT_FOUND");

        let res = app
            .get_with_token(&routes::contest_submissions(contest_id), &viewer)
            .await;
        assert_eq!(
            res.status, 200,
            "submission:view_all bypasses check_contest_access even without contest:manage"
        );
        assert_eq!(res.body["data"].as_array().unwrap().len(), 1);
    }
}

mod submission_detail {
    use super::*;

    struct Fixture {
        app: TestApp,
        admin: String,
        outsider: String,
        owner: String,
        submission_id: i32,
    }

    async fn setup() -> Fixture {
        let app = TestApp::spawn().await;
        let admin = app.create_user_with_role("admin", "pass1234", "admin").await;
        let outsider = app.create_authenticated_user("outsider", "pass1234").await;
        let owner = app.create_authenticated_user("owner", "pass1234").await;
        let contest_id =
            create_contest_window(&app, &admin, "Submission Detail Fixture", false, INSIDE_WINDOW)
                .await;
        let problem_id = app.create_problem(&admin, "Submission Detail Problem").await;
        app.add_problem_to_contest(contest_id, problem_id, &admin).await;
        enroll_participant(&app, &admin, contest_id, &owner).await;
        let submission_id = submit_to_contest_problem(&app, contest_id, problem_id, &owner).await;
        Fixture { app, admin, outsider, owner, submission_id }
    }

    #[tokio::test]
    async fn anonymous_is_401() {
        let f = setup().await;
        let res = f.app.get_without_token(&routes::submission(f.submission_id)).await;
        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "TOKEN_MISSING");
    }

    #[tokio::test]
    async fn non_participant_is_404() {
        let f = setup().await;
        let res = f
            .app
            .get_with_token(&routes::submission(f.submission_id), &f.outsider)
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn owner_participant_is_200() {
        let f = setup().await;
        let res = f
            .app
            .get_with_token(&routes::submission(f.submission_id), &f.owner)
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["id"], f.submission_id);
    }

    #[tokio::test]
    async fn admin_is_200() {
        let f = setup().await;
        let res = f
            .app
            .get_with_token(&routes::submission(f.submission_id), &f.admin)
            .await;
        assert_eq!(res.status, 200);
    }
}

mod contest_clarification_list {
    use super::*;

    struct Fixture {
        app: TestApp,
        admin: String,
        non_participant: String,
        participant: String,
        contest_id: i32,
    }

    async fn setup() -> Fixture {
        let app = TestApp::spawn().await;
        let admin = app.create_user_with_role("admin", "pass1234", "admin").await;
        let non_participant = app.create_authenticated_user("outsider", "pass1234").await;
        let participant = app.create_authenticated_user("participant", "pass1234").await;
        let contest_id =
            create_contest_window(&app, &admin, "Clarification Fixture", false, INSIDE_WINDOW).await;
        enroll_participant(&app, &admin, contest_id, &participant).await;
        Fixture { app, admin, non_participant, participant, contest_id }
    }

    #[tokio::test]
    async fn anonymous_is_401() {
        let f = setup().await;
        let res = f
            .app
            .get_without_token(&routes::contest_clarifications(f.contest_id))
            .await;
        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "TOKEN_MISSING");
    }

    #[tokio::test]
    async fn non_participant_is_404() {
        let f = setup().await;
        let res = f
            .app
            .get_with_token(&routes::contest_clarifications(f.contest_id), &f.non_participant)
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn participant_is_200() {
        let f = setup().await;
        let res = f
            .app
            .get_with_token(&routes::contest_clarifications(f.contest_id), &f.participant)
            .await;
        assert_eq!(res.status, 200);
        assert!(res.body["data"].is_array());
    }

    #[tokio::test]
    async fn admin_is_200() {
        let f = setup().await;
        let res = f
            .app
            .get_with_token(&routes::contest_clarifications(f.contest_id), &f.admin)
            .await;
        assert_eq!(res.status, 200);
    }
}

// ---------------------------------------------------------------------
// Soft-delete: contest, problem, and (the closest analogue that exists)
// submission-via-soft-deleted-contest.
// ---------------------------------------------------------------------
mod soft_delete {
    use super::*;

    /// NOTE: `find_contest` (used by `get_contest`) resolves the row via
    /// `find_active_by_id` BEFORE `check_contest_access` runs its
    /// `contest:manage` short-circuit. A soft-deleted contest is therefore
    /// NOT_FOUND even for a `contest:manage` admin - soft-delete is not
    /// bypassable by the same permission that bypasses the activation
    /// window.
    #[tokio::test]
    async fn contest_soft_deleted_is_not_found_even_for_contest_manage_admin() {
        let app = TestApp::spawn().await;
        let admin = app.create_user_with_role("admin", "pass1234", "admin").await;
        let contest_id =
            create_contest_window(&app, &admin, "Doomed Contest", true, INSIDE_WINDOW).await;

        let delete_res = app.delete_with_token(&routes::contest(contest_id), &admin).await;
        assert_eq!(delete_res.status, 204);

        let res = app.get_with_token(&routes::contest(contest_id), &admin).await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    /// NOTE: same shape of surprise as above, for problems -
    /// `require_problem_read_access` fetches via `find_active_by_id` before
    /// checking `problem:create`/`problem:edit`, so a soft-deleted problem
    /// is NOT_FOUND even for its own editor.
    #[tokio::test]
    async fn problem_soft_deleted_is_not_found_even_for_problem_edit_admin() {
        let app = TestApp::spawn().await;
        let admin = app.create_user_with_role("admin", "pass1234", "admin").await;
        let problem_id = app.create_problem(&admin, "Doomed Problem").await;

        let delete_res = app.delete_with_token(&routes::problem(problem_id), &admin).await;
        assert_eq!(delete_res.status, 204);

        let res = app.get_with_token(&routes::problem(problem_id), &admin).await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    /// NOTE: `submission` has NO `deleted_at` column / `SoftDeletable` impl
    /// at all - `find_submission` is a raw `find_by_id`. There is no way to
    /// soft-delete a submission directly. The closest real analogue is
    /// soft-deleting the CONTEST a submission belongs to, which is
    /// meaningful because `require_submission_visible`'s non-owner branch
    /// re-resolves the contest via `find_contest` (soft-delete-aware) on
    /// every read.
    ///
    /// The owner-bypass in `require_submission_visible`
    /// (`sub.user_id != auth_user.user_id`) means this contest lookup is
    /// SKIPPED ENTIRELY when the viewer owns the submission. The result: a
    /// participant can still read their own submission from a contest an
    /// admin has since soft-deleted, forever, while every other viewer
    /// (including one who was legitimately enrolled) is immediately
    /// NOT_FOUND. This asymmetry is a strong candidate for a "will this
    /// still be true" pin across the refactor.
    #[tokio::test]
    async fn submission_owner_can_still_view_own_submission_after_contest_soft_deleted() {
        let app = TestApp::spawn().await;
        let admin = app.create_user_with_role("admin", "pass1234", "admin").await;
        let owner = app.create_authenticated_user("owner", "pass1234").await;
        let contest_id =
            create_contest_window(&app, &admin, "Doomed Submission Contest", false, INSIDE_WINDOW)
                .await;
        let problem_id = app.create_problem(&admin, "Doomed Submission Problem").await;
        app.add_problem_to_contest(contest_id, problem_id, &admin).await;
        enroll_participant(&app, &admin, contest_id, &owner).await;
        let submission_id = submit_to_contest_problem(&app, contest_id, problem_id, &owner).await;

        let delete_res = app.delete_with_token(&routes::contest(contest_id), &admin).await;
        assert_eq!(delete_res.status, 204);

        let res = app
            .get_with_token(&routes::submission(submission_id), &owner)
            .await;
        assert_eq!(
            res.status, 200,
            "owner bypass in require_submission_visible skips the contest lookup entirely"
        );
        assert_eq!(res.body["id"], submission_id);
    }

    /// The mirror image of the above: a non-owner who WAS legitimately
    /// enrolled in the same (now soft-deleted) contest loses access to the
    /// owner's submission immediately, because the non-owner branch of
    /// `require_submission_visible` re-resolves the contest via
    /// `find_contest`.
    #[tokio::test]
    async fn submission_enrolled_non_owner_is_not_found_after_contest_soft_deleted() {
        let app = TestApp::spawn().await;
        let admin = app.create_user_with_role("admin", "pass1234", "admin").await;
        let owner = app.create_authenticated_user("owner", "pass1234").await;
        let peer = app.create_authenticated_user("peer", "pass1234").await;
        let contest_id =
            create_contest_window(&app, &admin, "Doomed Peer Contest", false, INSIDE_WINDOW).await;
        let problem_id = app.create_problem(&admin, "Doomed Peer Problem").await;
        app.add_problem_to_contest(contest_id, problem_id, &admin).await;
        enroll_participant(&app, &admin, contest_id, &owner).await;
        enroll_participant(&app, &admin, contest_id, &peer).await;
        let submission_id = submit_to_contest_problem(&app, contest_id, problem_id, &owner).await;

        let delete_res = app.delete_with_token(&routes::contest(contest_id), &admin).await;
        assert_eq!(delete_res.status, 204);

        let res = app
            .get_with_token(&routes::submission(submission_id), &peer)
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }
}
