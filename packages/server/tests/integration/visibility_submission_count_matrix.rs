//! Task 17 / Step 2b (I1), SDD 2026-09-15-visibility-kernel.
//!
//! `handlers/submission/mod.rs::list_contest_submissions` hand-duplicates
//! `visibility::host_rules::decide_submission`'s non-owner branch as a SQL
//! predicate (`can_see_all = can_view_all || (submissions_visible &&
//! is_participant)`) so it can restrict `total`/`total_pages` up front
//! instead of paying for a full per-row kernel decision over the whole
//! table (see that function's own comment above `let total = ...`). Nothing
//! in the type system keeps that hand-written copy in sync with the
//! kernel's real rule if either one is edited later - this file is the
//! regression net for that specific drift.
//!
//! `tests/integration/visibility_matrix.rs` is the FROZEN table for the
//! kernel's per-resource `Decision`s directly; it is not extended here.
//! This file instead cross-checks `list_contest_submissions`'s *aggregate*
//! (`pagination.total` and `data.len()`) against an INDEPENDENT oracle - a
//! per-submission `GET /api/v1/submissions/{id}` (200 vs 404) using the same
//! viewer token - so a bug on either side (the SQL predicate or the kernel's
//! `decide_submission`) shows up as a disagreement between the list
//! endpoint and the detail endpoint, rather than the test merely re-typing
//! the same formula being tested and always agreeing with itself.
//!
//! Matrix: three viewer kinds (`submission:view_all` holder with no contest
//! membership at all; a submission owner, who is necessarily also a
//! registered participant - contest submission creation requires
//! `require_contest_participant`, so "owner, non-participant" is not an
//! API-reachable state; and a pure participant who owns nothing in the
//! contest) crossed with the contest's two independent flags
//! (`is_public`, `submissions_visible`). Every viewer here is either
//! `view_all` (bypasses `check_contest_access` per the handler) or a
//! registered participant (`check_contest_access_decision` allows any
//! member regardless of `is_public` - see `host_rules.rs`), so `is_public`
//! never changes who can even reach the endpoint in this file; it is still
//! exercised on both settings to prove the total-vs-oracle agreement holds
//! in both regimes, not just the one the other suites happen to use.
use serde_json::json;

use crate::common::{TestApp, routes};

async fn user_id(app: &TestApp, token: &str) -> i32 {
    app.get_with_token(routes::ME, token).await.id()
}

async fn enroll(app: &TestApp, admin_token: &str, contest_id: i32, participant_token: &str) {
    let uid = user_id(app, participant_token).await;
    let res = app
        .post_with_token(
            &routes::contest_participants(contest_id),
            &json!({"user_id": uid}),
            admin_token,
        )
        .await;
    assert_eq!(res.status, 201, "enroll failed: {}", res.text);
}

async fn submit(app: &TestApp, contest_id: i32, problem_id: i32, token: &str) -> i32 {
    let res = app
        .post_with_token(
            &routes::contest_problem_submissions(contest_id, problem_id),
            &json!({
                "files": [{"filename": "main.cpp", "content": "#include <iostream>\nint main() {}"}],
                "language": "cpp",
            }),
            token,
        )
        .await;
    assert_eq!(res.status, 201, "submit failed: {}", res.text);
    res.id()
}

/// Counts, via the detail endpoint, how many of `sub_ids` this viewer can
/// actually read. This is the "kernel actually allows" ground truth I1
/// asks for - independent of `list_contest_submissions`'s own SQL, so it
/// can't pass by construction.
async fn oracle_visible_count(app: &TestApp, sub_ids: &[i32], viewer_token: &str) -> usize {
    let mut count = 0;
    for &id in sub_ids {
        let res = app
            .get_with_token(&routes::submission(id), viewer_token)
            .await;
        match res.status {
            200 => count += 1,
            404 => {}
            other => panic!(
                "unexpected status {other} for GET submission {id}: {}",
                res.text
            ),
        }
    }
    count
}

struct ContestConfig {
    is_public: bool,
    submissions_visible: bool,
}

const CONFIGS: &[ContestConfig] = &[
    ContestConfig {
        is_public: true,
        submissions_visible: true,
    },
    ContestConfig {
        is_public: true,
        submissions_visible: false,
    },
    ContestConfig {
        is_public: false,
        submissions_visible: true,
    },
    ContestConfig {
        is_public: false,
        submissions_visible: false,
    },
];

/// The three viewer archetypes I1 names, plus each cell's expected
/// `pagination.total` (== expected `data.len()` == expected oracle count,
/// since this fixture registers no visibility plugin - see Task 17 / Step
/// 2a's `into_masked_json_caps_oversized_detail_text_with_no_mask_and_no_plugin`
/// for the same "no plugin" premise). Two submissions exist in the contest
/// (`owner_a`'s own, `owner_b`'s own):
///
/// - `view_all`: `submission:view_all`, never enrolled -> always both (host
///   permission bypass in `decide_submission`, unconditional).
/// - `owner_a` / `owner_b`: owns exactly one of the two rows -> always sees
///   its own (owner bypass, independent of `submissions_visible` - a
///   deliberate cross-check that the SQL's `can_see_all` gating the OTHER
///   row never accidentally also gates the owner's own row, which stays
///   reachable purely through the `!can_see_all` branch's
///   `UserId.eq(auth_user.user_id)` filter) plus the peer row iff
///   `submissions_visible` (both are always participants).
/// - `participant`: owns neither -> sees both iff `submissions_visible`,
///   else neither.
#[tokio::test]
async fn list_contest_submissions_total_matches_kernel_truth_across_matrix() {
    for cfg in CONFIGS {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin", "pass1234", "admin")
            .await;
        let view_all_viewer = app
            .create_user_with_role("viewall", "pass1234", "problem_setter")
            .await;
        let owner_a = app.create_authenticated_user("ownera", "pass1234").await;
        let owner_b = app.create_authenticated_user("ownerb", "pass1234").await;
        let participant = app
            .create_authenticated_user("participant", "pass1234")
            .await;

        let problem_id = app.create_problem(&admin, "Matrix Problem").await;
        let contest_id = app
            .create_contest(
                &admin,
                "Submission Count Matrix Fixture",
                cfg.is_public,
                cfg.submissions_visible,
            )
            .await;
        app.add_problem_to_contest(contest_id, problem_id, &admin)
            .await;

        // `view_all_viewer` is deliberately NEVER enrolled: I1's `view_all`
        // row must hold under the permission bypass alone, with zero
        // contest membership.
        enroll(&app, &admin, contest_id, &owner_a).await;
        enroll(&app, &admin, contest_id, &owner_b).await;
        enroll(&app, &admin, contest_id, &participant).await;

        let sub_a = submit(&app, contest_id, problem_id, &owner_a).await;
        let sub_b = submit(&app, contest_id, problem_id, &owner_b).await;
        let sub_ids = [sub_a, sub_b];

        let peer_visible = cfg.submissions_visible;
        let cases: [(&str, &str, usize); 4] = [
            ("view_all", &view_all_viewer, 2),
            ("owner_a", &owner_a, 1 + peer_visible as usize),
            ("owner_b", &owner_b, 1 + peer_visible as usize),
            (
                "participant",
                &participant,
                if peer_visible { 2 } else { 0 },
            ),
        ];

        for (label, token, expected) in cases {
            let list_res = app
                .get_with_token(&routes::contest_submissions(contest_id), token)
                .await;
            assert_eq!(
                list_res.status, 200,
                "[is_public={}, submissions_visible={}, viewer={label}] list failed: {}",
                cfg.is_public, cfg.submissions_visible, list_res.text
            );

            let total = list_res.body["pagination"]["total"]
                .as_u64()
                .expect("pagination.total must be a number") as usize;
            let data_len = list_res.body["data"]
                .as_array()
                .expect("data must be an array")
                .len();
            let oracle = oracle_visible_count(&app, &sub_ids, token).await;

            assert_eq!(
                total, expected,
                "[is_public={}, submissions_visible={}, viewer={label}] pagination.total diverged from expected",
                cfg.is_public, cfg.submissions_visible
            );
            assert_eq!(
                data_len, expected,
                "[is_public={}, submissions_visible={}, viewer={label}] data.len() diverged from expected",
                cfg.is_public, cfg.submissions_visible
            );
            assert_eq!(
                oracle, expected,
                "[is_public={}, submissions_visible={}, viewer={label}] independent per-submission GET oracle diverged from expected - the SQL predicate and the kernel disagree",
                cfg.is_public, cfg.submissions_visible
            );
        }
    }
}
