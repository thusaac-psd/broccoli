//! End-to-end integration for Task 12 of the 下午场 (afternoon-session)
//! bracket plugin rollout.
//!
//! Every other codelink-bracket test (112 of them, in
//! `plugins/codelink-bracket/src/**`) calls `decide_problem`,
//! `decide_visibility_decisions`, `check_submission`, `advance`, etc.
//! DIRECTLY, in-process, as plain Rust functions. Those tests prove the
//! plugin's own logic is correct in isolation; they prove NOTHING about
//! whether the HOST actually asks the plugin the right question at the
//! right time, or actually honours the answer. That seam, namely
//! `VisibilityKernel` really invoking this plugin's `decide_visibility`
//! query export over the WASM ABI, `create_contest_submission` really
//! dispatching this plugin's `before_submission` hook, `DetachedEval`
//! really driving this plugin's judge, the background dispatcher really
//! firing this plugin's `on_timer`, is what this file exists to prove.
//! Every visibility assertion below therefore goes through
//! `GET /api/v1/contests/{id}/problems` (`list_contest_problems`), the one
//! HTTP path that calls `kernel.fetch_visible_batch`. `GET /matches/{id}`'s
//! own `group_a`/`group_b` masking
//! (`plugins/codelink-bracket/src/routes.rs::mask_group`) is a SEPARATE,
//! plugin-internal re-implementation of the same rule for display purposes;
//! it never leaves the plugin's own process, so it is deliberately never
//! used here to stand in for a kernel round trip.
//!
//! Flow: staff seed 16 players across a 4-round bracket, both players order
//! each other's problems, staff start match 0, players submit through 3
//! 小局 (one AC each, then a scoreless timer-driven timeout), the match
//! resolves 2-0, and the winner lands in round 2's slot. Along the way:
//! opponent-can-see / owner-cannot-see-yet during ordering, current-not-
//! future during play, `submission:view_all` sees everything, a player can
//! never read the opponent's submission, an eliminated player can't read a
//! live match's problems, and a submission to the wrong problem is rejected
//! with the plugin's OWN distinct error code, not a generic one.
//!
//! Requires `start_dispatcher: true` (background judging + timer delivery)
//! and a merged `plugins_dir` combining the real `codelink-bracket`
//! plugin, `standard-checkers` (only so `checker_format: "none"` passes
//! `validate_checker_format` at problem-creation time - never actually
//! invoked), and the test-only `codelink-bracket-judge-fixture` evaluator (stands
//! in for a real sandboxed compiler: `"ACCEPT"` source -> Accepted, anything
//! else -> WrongAnswer). All three `.wasm` files must be freshly built by
//! `./scripts/build-plugins.sh` before this test runs, or it silently
//! exercises stale plugin code.

use crate::common::{SpawnOptions, TestApp, TestResponse, routes};
use serde_json::{Value, json};
use std::time::Duration;

/// How long one 小局 stays open before the plugin timer force-closes it.
/// Small enough that the scoreless-小局-2 timeout (nobody submits, so the
/// ONLY way it resolves is the real background dispatcher's `on_timer` ->
/// `advance` firing after `deadline_ms`) does not make the test slow, large
/// enough to comfortably outlast the dispatcher's own ~1s tick.
pub(crate) const XIAOJU_SECONDS: i64 = 3;

/// One round's 7 real problem ids (3 group_a, 3 group_b, 1 tiebreak),
/// copied identically into every match of that round by the plugin's own
/// `storage::create_match_if_both_slots_filled` - see that function's doc
/// comment in `plugins/codelink-bracket/src/storage.rs`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RoundProblems {
    pub(crate) group_a: [i32; 3],
    pub(crate) group_b: [i32; 3],
    pub(crate) tiebreak: i32,
}

pub(crate) struct Player {
    pub(crate) id: i32,
    pub(crate) token: String,
}

/// Build a merged `plugins_dir` for `TestApp::spawn_with_plugins_and_options`
/// containing exactly the three plugins this test needs, each populated by
/// copying only `plugin.toml` + its built `.wasm` (+ `i18n/en.toml` for
/// codelink-bracket) - NOT a recursive directory copy, which would also
/// drag in `target/`, `Cargo.lock`, `src/`, etc. Passing a custom
/// `plugins_dir` REPLACES `TestApp`'s default fixture set entirely, so all
/// three real plugins this scenario needs must be assembled here.
fn merged_plugins_dir() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir should be created");

    let workspace_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("server package has a parent")
        .parent()
        .expect("packages/ has a parent")
        .to_path_buf();

    let copy_plugin = |src_dir: std::path::PathBuf, dest_name: &str, wasm_file: &str| {
        let dest_dir = tmp.path().join(dest_name);
        std::fs::create_dir_all(&dest_dir).expect("create plugin subdir");
        std::fs::copy(src_dir.join("plugin.toml"), dest_dir.join("plugin.toml"))
            .unwrap_or_else(|e| panic!("copy {dest_name}/plugin.toml: {e}"));
        std::fs::copy(src_dir.join(wasm_file), dest_dir.join(wasm_file))
            .unwrap_or_else(|e| panic!("copy {dest_name}/{wasm_file}: {e}"));
        let i18n_src = src_dir.join("i18n");
        if i18n_src.is_dir() {
            let i18n_dest = dest_dir.join("i18n");
            std::fs::create_dir_all(&i18n_dest).expect("create i18n subdir");
            for entry in std::fs::read_dir(&i18n_src).expect("read i18n dir") {
                let entry = entry.expect("read i18n entry");
                std::fs::copy(entry.path(), i18n_dest.join(entry.file_name()))
                    .unwrap_or_else(|e| panic!("copy {dest_name}/i18n file: {e}"));
            }
        }
    };

    copy_plugin(
        workspace_root.join("plugins/codelink-bracket"),
        "codelink-bracket",
        "codelink_bracket.wasm",
    );
    copy_plugin(
        workspace_root.join("plugins/standard-checkers"),
        "standard-checkers",
        "standard_checkers.wasm",
    );
    copy_plugin(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/codelink-bracket-judge"),
        "codelink-bracket-judge-fixture",
        "codelink_bracket_judge_fixture.wasm",
    );

    tmp
}

pub(crate) async fn spawn_bracket_app() -> (TestApp, tempfile::TempDir) {
    let tmp = merged_plugins_dir();
    let app = TestApp::spawn_with_plugins_and_options(SpawnOptions {
        plugins_dir: Some(tmp.path().to_path_buf()),
        start_dispatcher: true,
        ..Default::default()
    })
    .await;
    (app, tmp)
}

/// Register `username`, log in, and resolve the real DB-assigned `user_id`
/// (`create_authenticated_user` only ever returns a token).
pub(crate) async fn player(app: &TestApp, username: &str) -> Player {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use server::entity::user;

    let token = app.create_authenticated_user(username, "pass1234").await;
    let id = user::Entity::find()
        .filter(user::Column::Username.eq(username))
        .one(&app.db)
        .await
        .expect("query user")
        .expect("user should exist")
        .id;
    Player { id, token }
}

/// Create a real problem backed by the `codelink-bracket-judge-fixture` evaluator,
/// attach it to the contest under a distinct label (the shared
/// `TestApp::add_problem_to_contest` helper hardcodes label `"A"`, which
/// would collide across the 28 problems this scenario needs), and return
/// its real DB-assigned id.
async fn create_and_attach_problem(
    app: &TestApp,
    contest_id: i32,
    staff_token: &str,
    label: &str,
) -> i32 {
    let res = app
        .post_with_token(
            routes::PROBLEMS,
            &json!({
                "title": format!("Bracket Problem {label}"),
                "content": "## Description\nSubmit `ACCEPT` to solve.",
                "time_limit": 1000,
                "memory_limit": 262144,
                "problem_type": "codelink-bracket-judge-fixture",
                "checker_format": "none",
                "default_contest_type": "codelink-bracket",
                "is_public": true,
            }),
            staff_token,
        )
        .await;
    assert_eq!(
        res.status, 201,
        "create problem {label} failed: {}",
        res.text
    );
    let problem_id = res.id();

    app.create_test_case(problem_id, staff_token).await;

    let res = app
        .post_with_token(
            &routes::contest_problems(contest_id),
            &json!({ "problem_id": problem_id, "label": label }),
            staff_token,
        )
        .await;
    assert_eq!(
        res.status, 201,
        "attach problem {label} to contest failed: {}",
        res.text
    );

    problem_id
}

/// Create all 28 real problems (4 rounds x 7: 3 group_a + 3 group_b + 1
/// tiebreak) and attach them to the contest, returning one [`RoundProblems`]
/// per round in order.
pub(crate) async fn create_all_round_problems(
    app: &TestApp,
    contest_id: i32,
    staff_token: &str,
) -> Vec<RoundProblems> {
    let mut rounds = Vec::with_capacity(4);
    for round in 0..4u8 {
        let group_a = [
            create_and_attach_problem(app, contest_id, staff_token, &format!("R{round}A0")).await,
            create_and_attach_problem(app, contest_id, staff_token, &format!("R{round}A1")).await,
            create_and_attach_problem(app, contest_id, staff_token, &format!("R{round}A2")).await,
        ];
        let group_b = [
            create_and_attach_problem(app, contest_id, staff_token, &format!("R{round}B0")).await,
            create_and_attach_problem(app, contest_id, staff_token, &format!("R{round}B1")).await,
            create_and_attach_problem(app, contest_id, staff_token, &format!("R{round}B2")).await,
        ];
        let tiebreak =
            create_and_attach_problem(app, contest_id, staff_token, &format!("R{round}T")).await;
        rounds.push(RoundProblems {
            group_a,
            group_b,
            tiebreak,
        });
    }
    rounds
}

pub(crate) fn setup_body(rounds: &[RoundProblems], seeds: &[i32]) -> Value {
    json!({
        "rounds": rounds.iter().map(|r| json!({
            "group_a": r.group_a,
            "group_b": r.group_b,
            "tiebreak": [r.tiebreak],
        })).collect::<Vec<_>>(),
        "xiaoju_seconds": XIAOJU_SECONDS,
        "round_intermission_seconds": 0,
        "seeds": seeds,
    })
}

pub(crate) fn bracket_route(contest_id: i32, sub_path: &str) -> String {
    routes::plugin_proxy(
        "codelink-bracket",
        &format!("/api/plugins/codelink-bracket/contests/{contest_id}{sub_path}"),
    )
}

pub(crate) async fn get_match(app: &TestApp, contest_id: i32, match_id: u8, token: &str) -> Value {
    let res = app
        .get_with_token(
            &bracket_route(contest_id, &format!("/matches/{match_id}")),
            token,
        )
        .await;
    assert_eq!(
        res.status, 200,
        "get_match({match_id}) failed: {}",
        res.text
    );
    res.body
}

/// Poll `GET /matches/{id}` until `predicate` holds or `timeout` elapses.
/// Mirrors `plugin_timer.rs`'s `await_deliveries` polling pattern - the
/// background dispatcher ticks roughly every second, so this drives both
/// real judging progression and real timer-driven advancement.
async fn poll_match(
    app: &TestApp,
    contest_id: i32,
    match_id: u8,
    token: &str,
    timeout: Duration,
    mut predicate: impl FnMut(&Value) -> bool,
) -> Value {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let view = get_match(app, contest_id, match_id, token).await;
        if predicate(&view) {
            return view;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for match {match_id}'s predicate; last view: {view}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The real problem ids visible to `token` in the contest's problem list -
/// the ONE HTTP path (`list_contest_problems`) that calls
/// `kernel.fetch_visible_batch`, i.e. the one path that actually proves the
/// host asked the plugin's `decide_visibility` and honoured the answer. A
/// denied problem is OMITTED from the response array entirely (no
/// placeholder), so absence from this list IS the denial signal.
pub(crate) async fn visible_problem_ids(app: &TestApp, contest_id: i32, token: &str) -> Vec<i64> {
    let res = app
        .get_with_token(&routes::contest_problems(contest_id), token)
        .await;
    assert_eq!(
        res.status, 200,
        "list_contest_problems failed: {}",
        res.text
    );
    res.body
        .as_array()
        .expect("contest problems response should be an array")
        .iter()
        .map(|p| {
            p["problem_id"]
                .as_i64()
                .expect("each entry should carry problem_id")
        })
        .collect()
}

pub(crate) async fn submit(
    app: &TestApp,
    contest_id: i32,
    problem_id: i32,
    token: &str,
    source: &str,
) -> TestResponse {
    app.post_with_token(
        &routes::contest_problem_submissions(contest_id, problem_id),
        &json!({
            "files": [{"filename": "main.cpp", "content": source}],
            "language": "cpp",
        }),
        token,
    )
    .await
}

#[tokio::test]
async fn codelink_bracket_end_to_end_through_the_real_host() {
    use broccoli_server_sdk::permissions as perm;

    let (app, _plugins_tmp) = spawn_bracket_app().await;

    // === Fixture setup ===============================================
    let staff_token = app
        .create_user_with_permissions(
            "bracket_staff",
            "pass1234",
            &[
                perm::CONTEST_CREATE,
                perm::CONTEST_MANAGE,
                perm::PROBLEM_CREATE,
                perm::PROBLEM_EDIT,
            ],
        )
        .await;
    let view_all_token = app
        .create_user_with_permissions("bracket_view_all", "pass1234", &[perm::SUBMISSION_VIEW_ALL])
        .await;

    let mut players = Vec::with_capacity(16);
    for i in 0..16 {
        players.push(player(&app, &format!("bracket_player_{i}")).await);
    }

    let res = app
        .post_with_token(
            routes::CONTESTS,
            &json!({
                "title": "Afternoon Bracket E2E",
                "description": "Afternoon bracket integration contest",
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

    // Every player must be a registered participant: public-contest READS
    // (`check_contest_access`) need no registration, but the submission-gate
    // probes below reach `require_contest_participant` (SUBMIT, not READ)
    // once the kernel allows them through - without registration those
    // probes would 403 `PermissionDenied` instead of exercising the
    // intended plugin-level rejection.
    for p in &players {
        app.register_for_contest(contest_id, &p.token).await;
    }

    // Enable the `before_submission` hook (`HookScope::Resource`): per
    // `packages/server/src/hooks.rs::merge_resource_enablements`, a
    // resource-scoped hook only fires if an explicit config row sets
    // `enabled: true` - absent that, `create_contest_submission` would
    // never dispatch it at all, and the submission-gate probes below would
    // silently pass through unchecked. The namespace string itself is
    // cosmetic; `extract_plugin_id` only reads the `plugin_id:` prefix the
    // host itself composes.
    let res = app
        .put_with_token(
            &routes::contest_config_ns(contest_id, "codelink-bracket", "before_submission"),
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
    assert_eq!(res.body["ok"], true);

    // Round 1, match 0 = seeds[0] (A) vs seeds[1] (B); match 1 = seeds[2]
    // (C) vs seeds[3] (D) - see `setup.rs::handle_setup`'s round-1 loop,
    // which pairs `slot_key(1, 2p)`/`slot_key(1, 2p+1)` into match `p`.
    let a = &players[0];
    let b = &players[1];
    let c = &players[2];

    // === Phase 1: ordering - opponent can see, owner cannot yet =========
    // B ranks A's group_a (the direction is load-bearing: submitter == B
    // writes order_a - see `ordering.rs`'s module doc comment).
    let res = app
        .post_with_token(
            &bracket_route(contest_id, "/matches/0/order"),
            &json!({"order": rounds[0].group_a}),
            &b.token,
        )
        .await;
    assert_eq!(
        res.status, 200,
        "B's ranking of A's problems failed: {}",
        res.text
    );

    let res = app
        .post_with_token(
            &bracket_route(contest_id, "/matches/0/order"),
            &json!({"order": rounds[0].group_b}),
            &a.token,
        )
        .await;
    assert_eq!(
        res.status, 200,
        "A's ranking of B's problems failed: {}",
        res.text
    );

    let ids_for_b = visible_problem_ids(&app, contest_id, &b.token).await;
    for pid in rounds[0].group_a {
        assert!(
            ids_for_b.contains(&(pid as i64)),
            "B (opponent) must see A's group_a problem {pid} during ordering - if this fails, \
             the kernel never honoured the plugin's opponent-can-see-during-ordering answer"
        );
    }

    let ids_for_a = visible_problem_ids(&app, contest_id, &a.token).await;
    for pid in rounds[0].group_a {
        assert!(
            !ids_for_a.contains(&(pid as i64)),
            "A (owner) must NOT see own group_a problem {pid} before any 小局 has opened"
        );
    }
    for pid in rounds[0].group_b {
        assert!(
            ids_for_a.contains(&(pid as i64)),
            "A (opponent) must see B's group_b problem {pid} during ordering"
        );
    }
    let ids_for_b = visible_problem_ids(&app, contest_id, &b.token).await;
    for pid in rounds[0].group_b {
        assert!(
            !ids_for_b.contains(&(pid as i64)),
            "B (owner) must NOT see own group_b problem {pid} before any 小局 has opened"
        );
    }

    // === Phase 2: start the match ========================================
    let res = app
        .post_with_token(
            &bracket_route(contest_id, "/matches/0/start"),
            &json!({}),
            &staff_token,
        )
        .await;
    assert_eq!(res.status, 200, "starting match 0 failed: {}", res.text);

    // === Phase 3: 小局 0 - current-not-future, and the submission gate ===
    let ids_for_a = visible_problem_ids(&app, contest_id, &a.token).await;
    assert!(
        ids_for_a.contains(&(rounds[0].group_a[0] as i64)),
        "A must see 小局 0's own current problem"
    );
    assert!(
        !ids_for_a.contains(&(rounds[0].group_a[1] as i64)),
        "A must NOT see 小局 1's not-yet-open problem while 小局 0 is open"
    );
    assert!(
        !ids_for_a.contains(&(rounds[0].group_a[2] as i64)),
        "A must NOT see 小局 2's not-yet-open problem while 小局 0 is open"
    );

    // Gate probe (a): A submits to B's current problem (the OPPONENT's
    // problem, not A's own). The kernel's OWN base decision allows this
    // read (viewer == opponent, state != Pending), so the request reaches
    // the plugin's `before_submission` hook, which rejects it with its own
    // distinct code.
    let res = submit(&app, contest_id, rounds[0].group_b[0], &a.token, "ACCEPT").await;
    assert_eq!(
        res.status, 400,
        "submitting to the opponent's current problem should be rejected: {}",
        res.text
    );
    assert_eq!(
        res.body["code"], "NOT_YOUR_PROBLEM",
        "opponent-problem submission must be rejected with the plugin's distinct code, got: {}",
        res.text
    );

    // Gate probe (b): A submits to A's OWN 小局-2 (index 2) problem while
    // 小局 0 is open. Unlike probe (a), the kernel's OWN visibility check on
    // this problem denies it outright (owner rule: position > current
    // index) BEFORE the hook is ever reached - a structurally different
    // rejection (generic 404, not the plugin's code) from probe (a) above.
    // This asymmetry is a genuine architectural finding, reported as such,
    // not normalized away.
    let res = submit(&app, contest_id, rounds[0].group_a[2], &a.token, "ACCEPT").await;
    assert_eq!(
        res.status, 404,
        "submitting to own not-yet-open problem should be rejected by the kernel itself: {}",
        res.text
    );
    assert_eq!(
        res.body["code"], "NOT_FOUND",
        "unexpected rejection code: {}",
        res.text
    );

    // A's real 小局 0 submission: AC.
    let res = submit(&app, contest_id, rounds[0].group_a[0], &a.token, "ACCEPT").await;
    assert_eq!(
        res.status, 201,
        "A's real 小局 0 submission failed: {}",
        res.text
    );
    let submission_0_id = res.id();

    let view = poll_match(
        &app,
        contest_id,
        0,
        &staff_token,
        Duration::from_secs(15),
        |v| v["score_a"].as_i64() == Some(1),
    )
    .await;
    assert_eq!(view["score_b"].as_i64(), Some(0));

    // === Submission-read assertions ======================================
    let res = app
        .get_with_token(&routes::submission(submission_0_id), &b.token)
        .await;
    assert_eq!(
        res.status, 404,
        "B must not be able to read A's submission: {}",
        res.text
    );

    let res = app
        .get_with_token(&routes::submission(submission_0_id), &a.token)
        .await;
    assert_eq!(
        res.status, 200,
        "A (owner) should read own submission: {}",
        res.text
    );

    let res = app
        .get_with_token(&routes::submission(submission_0_id), &view_all_token)
        .await;
    assert_eq!(
        res.status, 200,
        "submission:view_all holder should read any submission live: {}",
        res.text
    );

    // === Phase 4: 小局 1 - current-not-future again ======================
    let ids_for_a = visible_problem_ids(&app, contest_id, &a.token).await;
    assert!(
        ids_for_a.contains(&(rounds[0].group_a[0] as i64)),
        "A should still see the already-decided 小局 0 problem"
    );
    assert!(
        ids_for_a.contains(&(rounds[0].group_a[1] as i64)),
        "A must see 小局 1's own current problem"
    );
    assert!(
        !ids_for_a.contains(&(rounds[0].group_a[2] as i64)),
        "A must NOT see 小局 2's not-yet-open problem while 小局 1 is open"
    );

    let res = submit(&app, contest_id, rounds[0].group_a[1], &a.token, "ACCEPT").await;
    assert_eq!(
        res.status, 201,
        "A's real 小局 1 submission failed: {}",
        res.text
    );

    let view = poll_match(
        &app,
        contest_id,
        0,
        &staff_token,
        Duration::from_secs(15),
        |v| v["score_a"].as_i64() == Some(2),
    )
    .await;
    assert_eq!(view["score_b"].as_i64(), Some(0));

    // === Phase 5: 小局 2 - nobody submits; the real plugin timer decides =
    let view = poll_match(
        &app,
        contest_id,
        0,
        &staff_token,
        Duration::from_secs(XIAOJU_SECONDS as u64 + 25),
        |v| v["state"] == "decided",
    )
    .await;
    assert_eq!(
        view["winner"].as_i64(),
        Some(a.id as i64),
        "match 0 should resolve to A (2-0) once 小局 2 times out scoreless"
    );
    assert_eq!(view["score_a"].as_i64(), Some(2));
    assert_eq!(view["score_b"].as_i64(), Some(0));

    // === Phase 6: force-decide match 1, then check the next-round slot ===
    let res = app
        .post_with_token(
            &bracket_route(contest_id, "/matches/1/force-decide"),
            &json!({"winner": c.id}),
            &staff_token,
        )
        .await;
    assert_eq!(
        res.status, 200,
        "force-deciding match 1 failed: {}",
        res.text
    );

    // Round 1 match 0's `pos` (0) writes round 2 slot 0 (-> player_a);
    // match 1's `pos` (1) writes round 2 slot 1 (-> player_b) - see
    // `judge.rs::write_next_round_slot` / `storage.rs`'s slot-index scheme.
    // Round 2's single match at pos 0 is match id 8
    // (`storage::match_id_for`).
    let match8 = get_match(&app, contest_id, 8, &staff_token).await;
    assert_eq!(match8["state"], "ordering");
    assert_eq!(
        match8["player_a"].as_i64(),
        Some(a.id as i64),
        "round 2's match should seed player_a from round-1 match 0's winner"
    );
    assert_eq!(
        match8["player_b"].as_i64(),
        Some(c.id as i64),
        "round 2's match should seed player_b from round-1 match 1's winner"
    );

    // === Visibility after elimination and with submission:view_all ======
    let round2_ids: Vec<i64> = rounds[1]
        .group_a
        .iter()
        .chain(rounds[1].group_b.iter())
        .chain(std::iter::once(&rounds[1].tiebreak))
        .map(|&id| id as i64)
        .collect();

    let ids_for_b = visible_problem_ids(&app, contest_id, &b.token).await;
    for pid in &round2_ids {
        assert!(
            !ids_for_b.contains(pid),
            "eliminated player B must not see live round-2 problem {pid} - \
             B has no match in round 2 at all (find_players_match returns None)"
        );
    }

    // Sanity check on the same mechanism from the other side: A, who DID
    // advance, sees round 2's opponent group (C's group_b) but not A's own
    // round-2 group_a yet (no 小局 open in round 2).
    let ids_for_a = visible_problem_ids(&app, contest_id, &a.token).await;
    for pid in rounds[1].group_b {
        assert!(
            ids_for_a.contains(&(pid as i64)),
            "A should see round 2's opponent group_b problem {pid} once match 8 reaches ordering"
        );
    }
    for pid in rounds[1].group_a {
        assert!(
            !ids_for_a.contains(&(pid as i64)),
            "A must not see A's own round-2 group_a problem {pid} before any 小局 opens"
        );
    }

    let all_problem_ids: Vec<i64> = rounds
        .iter()
        .flat_map(|r| {
            r.group_a
                .into_iter()
                .chain(r.group_b)
                .chain(std::iter::once(r.tiebreak))
        })
        .map(|id| id as i64)
        .collect();
    let ids_for_view_all = visible_problem_ids(&app, contest_id, &view_all_token).await;
    for pid in &all_problem_ids {
        assert!(
            ids_for_view_all.contains(pid),
            "submission:view_all holder must see problem {pid} live, regardless of match phase"
        );
    }
}
