//! Widening-regression coverage for Task 11: replacing the submission
//! `filter_submission_fn` mechanism with kernel decisions.
//!
//! Before this task, `filter_submission_via_plugin`
//! (`packages/server/src/handlers/submission/filter.rs`) adopted a
//! plugin-authored submission JSON wholesale after only a shape check -
//! `Ok(out) => Ok(out.submission)`. A plugin registered against the
//! submission's contest type could therefore alter a verdict, a score, or a
//! displayed user id: a field mask can blank a value, but the old mechanism
//! could also WRITE one.
//!
//! These tests drive the fixture plugin under `tests/fixtures/server-plugin`
//! (which now registers `[[server.queries]] topic = "visibility"`, function
//! `decide_visibility`) through real HTTP handlers and assert the host's own
//! decision always wins:
//!
//! - `plugin_cannot_widen_host_decision`: the fixture plugin answers `Allow`
//!   for every resource by default, including ones the host itself denies.
//!   `Decision::meet` must still produce `Deny` - 404, never 200.
//! - `plugin_cannot_author_a_verdict`: the fixture plugin answers `Redact`
//!   on `result.verdict` / `result.score` for a nominated submission id
//!   (seeded via the fixture's existing KV route). `WireDecision::Redact`
//!   only ever carries field paths, never a replacement value, so the
//!   masked fields must come back `null` - never the plugin's own content,
//!   and never the real value either.
//!
//! Task 14 extends the same fixture with a `visibility_mode` KV switch (see
//! `tests/fixtures/server-plugin/src/lib.rs`) and adds two further groups of
//! tests below, both driving real HTTP handlers:
//!
//! - Failure injection: a plugin that traps, returns non-JSON, returns a
//!   decision vector shorter than the resource batch, returns an unrecognized
//!   decision variant, or answers `Redact` with a field mask over one of the
//!   host's `MAX_MASK_PATH_SEGMENTS` / `MAX_MASK_PATH_BYTES` / `MAX_MASK_FIELDS`
//!   caps must deny the WHOLE batch and surface as 404 `NOT_FOUND` - never a
//!   500, and never a partial result.
//! - A working `decide_visibility` path (`deny_resource_keys` /
//!   `redact_resource_keys` / `redact_resource_fields`), proving the kernel's
//!   headline list-omission and masking guarantees through a real contest ->
//!   problems list and a real problem detail read, not just kernel-internal
//!   unit tests.

use crate::common::{TestApp, routes};

/// Seed an arbitrary fixture-plugin KV key (read by `decide_visibility` in
/// `tests/fixtures/server-plugin/src/lib.rs`) via the plugin's existing
/// `kv_write` route. Generalizes [`seed_redact_submission_id`] to the mode
/// switch and the `deny_resource_keys`/`redact_resource_keys`/
/// `redact_resource_fields` mechanism added for Task 14.
async fn seed_kv(app: &TestApp, key: &str, value: &str) {
    let route = routes::plugin_proxy("server-plugin", &format!("kv/{key}"));
    let res = app
        .post_without_token(&route, &serde_json::json!({ "value": value }))
        .await;
    assert_eq!(res.status, 200, "seeding KV `{key}` failed: {}", res.text);
}

/// Seed the fixture plugin's `redact_submission_ids` KV key (read by
/// `decide_visibility` in `tests/fixtures/server-plugin/src/lib.rs`) with a
/// single submission id, via the plugin's existing `kv_write` route.
async fn seed_redact_submission_id(app: &TestApp, submission_id: i32) {
    seed_kv(app, "redact_submission_ids", &submission_id.to_string()).await;
}

/// Insert a public, standalone-submittable problem directly via the DB.
///
/// `TestApp::create_problem` goes through the real HTTP handler, which
/// validates `problem_type`/`checker_format` against the in-memory evaluator
/// registry. That registry is only ever seeded with the `"__test__"` noop
/// handlers under plain `TestApp::spawn()` - `spawn_with_plugins()` (needed
/// here so the fixture plugin's `decide_visibility` query is actually
/// registered) leaves it empty, so `create_problem` would 400 with
/// `problem_type must be one of: `. None of that validation matters for
/// these tests - only the problem's existence and `is_public` flag do.
async fn insert_public_problem(app: &TestApp, title: &str) -> i32 {
    use sea_orm::{ActiveModelTrait, Set};
    use server::entity::problem;

    let now = chrono::Utc::now();
    let problem = problem::ActiveModel {
        title: Set(title.into()),
        content: Set("## Description\nSolve this.".into()),
        time_limit: Set(1000),
        memory_limit: Set(262144),
        problem_type: Set("standard".into()),
        checker_format: Set("exact".into()),
        default_contest_type: Set("standard".into()),
        show_test_details: Set(false),
        is_public: Set(true),
        submission_format: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        deleted_at: Set(None),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert public problem");
    problem.id
}

/// Insert a `Queued`, verdict-less submission directly via the DB.
///
/// `TestApp::create_submission` goes through the real HTTP handler, which
/// (like `create_problem`) validates the request `language` against the
/// in-memory `language_resolver_registry` - empty under
/// `spawn_with_plugins()` for the same reason documented on
/// [`insert_public_problem`]. Existence and ownership are all these tests
/// need; nothing here exercises the judging pipeline.
async fn insert_queued_submission(app: &TestApp, problem_id: i32, user_id: i32) -> i32 {
    use sea_orm::{ActiveModelTrait, Set};
    use server::entity::submission;

    let submission = submission::ActiveModel {
        files: Set(serde_json::json!([
            { "filename": "main.cpp", "content": "int main() {}" }
        ])),
        language: Set("cpp".into()),
        user_id: Set(user_id),
        problem_id: Set(problem_id),
        contest_id: Set(None),
        contest_type: Set("standard".into()),
        status: Set(common::SubmissionStatus::Queued),
        judge_epoch: Set(0),
        created_at: Set(chrono::Utc::now()),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert queued submission");
    submission.id
}

#[tokio::test]
async fn plugin_cannot_widen_host_decision() {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use server::entity::user;

    let app = TestApp::spawn_with_plugins().await;

    let owner_token = app
        .create_authenticated_user("widen_owner", "pass1234")
        .await;
    let peer_token = app
        .create_authenticated_user("widen_peer", "pass1234")
        .await;

    let problem_id = insert_public_problem(&app, "Widen Test Problem").await;
    let owner = user::Entity::find()
        .filter(user::Column::Username.eq("widen_owner"))
        .one(&app.db)
        .await
        .expect("query owner")
        .expect("owner should exist");

    // A standalone (contest-less) submission: `host_rules::decide_submission`
    // denies any non-owner without `submission:view_all` outright once
    // `sub.contest_id` is `None` - there's no contest window to bypass at
    // all. The fixture plugin's `decide_visibility` answers `Allow` for this
    // resource anyway (its documented default), so a 404 here can only be
    // explained by the host's `Deny` surviving `Decision::meet` regardless
    // of what the plugin says.
    let submission_id = insert_queued_submission(&app, problem_id, owner.id).await;

    // Positive control: the owner themselves must still be able to read it -
    // otherwise a 404 below could just mean the row/route is broken, not that
    // the host's Deny actually held against a widening plugin.
    let owner_res = app
        .get_with_token(&routes::submission(submission_id), &owner_token)
        .await;
    assert_eq!(
        owner_res.status, 200,
        "owner read failed: {}",
        owner_res.text
    );

    let res = app
        .get_with_token(&routes::submission(submission_id), &peer_token)
        .await;

    assert_eq!(
        res.status, 404,
        "a plugin answering Allow must not be able to widen a host Deny; body: {}",
        res.text
    );
    assert_eq!(res.body["code"], "NOT_FOUND");
}

#[tokio::test]
async fn plugin_cannot_author_a_verdict() {
    use chrono::Utc;
    use common::{SubmissionStatus, Verdict};
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
    use server::entity::{submission, user};

    let app = TestApp::spawn_with_plugins().await;

    let owner_token = app
        .create_authenticated_user("verdict_owner", "pass1234")
        .await;
    let problem_id = insert_public_problem(&app, "Verdict Test Problem").await;

    let owner = user::Entity::find()
        .filter(user::Column::Username.eq("verdict_owner"))
        .one(&app.db)
        .await
        .expect("query owner")
        .expect("owner should exist");

    // Insert an already-`Judged` submission directly - `spawn_with_plugins`
    // doesn't register any real evaluator/checker, so there is no judging
    // pipeline to drive a submission to a terminal verdict through the API.
    // The real point of this test is what the RESPONSE does with an
    // already-known, non-null verdict/score once a plugin nominates them for
    // redaction, not how the verdict got there.
    let now = Utc::now();
    let submission = submission::ActiveModel {
        files: Set(serde_json::json!([
            { "filename": "main.cpp", "content": "int main() {}" }
        ])),
        language: Set("cpp".into()),
        user_id: Set(owner.id),
        problem_id: Set(problem_id),
        contest_id: Set(None),
        contest_type: Set("standard".into()),
        status: Set(SubmissionStatus::Judged),
        verdict: Set(Some(Verdict::Accepted)),
        score: Set(Some(100.0)),
        time_used: Set(Some(7)),
        memory_used: Set(Some(128)),
        judge_epoch: Set(1),
        created_at: Set(now),
        judged_at: Set(Some(now)),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert judged submission");
    let submission_id = submission.id;

    // Sanity check on the premise: without any masking, the owner reading
    // their own submission sees the real verdict/score. If this fails, the
    // assertions below would be vacuous (the field could be null for
    // unrelated reasons).
    let baseline = app
        .get_with_token(&routes::submission(submission_id), &owner_token)
        .await;
    assert_eq!(
        baseline.status, 200,
        "baseline read failed: {}",
        baseline.text
    );
    assert_eq!(baseline.body["result"]["verdict"], "Accepted");
    assert_eq!(baseline.body["result"]["score"], 100.0);

    // Nominate this submission's id for redaction in the fixture plugin's
    // `decide_visibility`. It can only answer `Redact { fields: [...] }` -
    // there is no wire channel for it to supply a replacement verdict/score,
    // only paths to blank.
    seed_redact_submission_id(&app, submission_id).await;

    let res = app
        .get_with_token(&routes::submission(submission_id), &owner_token)
        .await;

    assert_eq!(
        res.status, 200,
        "owner read should still be reachable: {}",
        res.text
    );
    assert!(
        res.body["result"]["verdict"].is_null(),
        "plugin-nominated Redact must blank the verdict, got: {}",
        res.body["result"]["verdict"]
    );
    assert!(
        res.body["result"]["score"].is_null(),
        "plugin-nominated Redact must blank the score, got: {}",
        res.body["result"]["score"]
    );
    // The rest of the response is untouched - Redact narrows exactly the
    // nominated fields, nothing else.
    assert_eq!(res.body["id"], submission_id);
    assert_eq!(res.body["status"], "Judged");
}

// == Task 14: failure injection ==========================================
//
// The fixture plugin's `decide_visibility` (`tests/fixtures/server-plugin/src/lib.rs`)
// is switched into each of these failure modes via the `visibility_mode` KV
// key. Every mode must collapse to `Decision::Deny` for the WHOLE batch
// (`decisions_from_output`, `packages/server/src/visibility/plugin_query.rs`)
// - never a partial result, and never a 500: a WASM trap, malformed output,
// or protocol violation from a plugin is an *untrusted extension failing*,
// not a host bug, so it must present to the caller exactly like "this
// resource doesn't exist" (404 `NOT_FOUND`), same as a host-side `Deny`.
//
// The single-resource tests below all reuse the same shape: seed a
// standalone submission owned by `owner`, confirm the owner can read it
// before the failure mode is armed (so the later 404 can only be explained
// by the failure mode, not a broken fixture), then arm the mode and
// re-read.

async fn assert_denies_single_submission_read(mode: &str) {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use server::entity::user;

    let app = TestApp::spawn_with_plugins().await;

    let owner_token = app
        .create_authenticated_user(&format!("mode_{mode}_owner"), "pass1234")
        .await;
    let problem_id = insert_public_problem(&app, "Failure Mode Test Problem").await;
    let owner = user::Entity::find()
        .filter(user::Column::Username.eq(format!("mode_{mode}_owner")))
        .one(&app.db)
        .await
        .expect("query owner")
        .expect("owner should exist");
    let submission_id = insert_queued_submission(&app, problem_id, owner.id).await;

    // Sanity check on the premise: before the failure mode is armed, the
    // owner can read their own submission. If this failed, a 404 below
    // would be meaningless - it could just mean the row/route is broken.
    let baseline = app
        .get_with_token(&routes::submission(submission_id), &owner_token)
        .await;
    assert_eq!(
        baseline.status, 200,
        "baseline read failed before arming mode `{mode}`: {}",
        baseline.text
    );

    seed_kv(&app, "visibility_mode", mode).await;

    let res = app
        .get_with_token(&routes::submission(submission_id), &owner_token)
        .await;

    assert_eq!(
        res.status, 404,
        "plugin failure mode `{mode}` must deny (404), never 500; got {}: {}",
        res.status, res.text
    );
    assert_eq!(
        res.body["code"], "NOT_FOUND",
        "plugin failure mode `{mode}`: {}",
        res.text
    );
}

#[tokio::test]
async fn plugin_trap_denies_and_returns_404_not_500() {
    // `decide_visibility` panics before even parsing its input. On
    // `wasm32-wasip1` with `panic = "abort"` this lowers to a genuine trap -
    // the plugin call itself fails, never returning a Rust panic that could
    // unwind through the host.
    assert_denies_single_submission_read("trap").await;
}

#[tokio::test]
async fn non_json_output_denies() {
    // `decide_visibility` returns `Ok("this is deliberately not JSON")` - a
    // syntactically successful plugin call whose payload fails to
    // deserialize into `VisibilityQueryOutput`.
    assert_denies_single_submission_read("non_json").await;
}

#[tokio::test]
async fn unknown_decision_variant_denies() {
    // `decide_visibility` hand-crafts `{"decisions": [{"mystery": {}}]}`,
    // bypassing the `WireDecisionOut` enum entirely. `WireDecision` has no
    // `#[serde(other)]` catch-all, so this fails to deserialize exactly like
    // `non_json` - a different plugin-side bug producing the same host-side
    // fail-closed outcome.
    assert_denies_single_submission_read("unknown_variant").await;
}

#[tokio::test]
async fn short_decision_vector_denies_whole_batch() {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use server::entity::user;

    let app = TestApp::spawn_with_plugins().await;

    let owner_token = app
        .create_authenticated_user("short_vector_owner", "pass1234")
        .await;
    let problem_id = insert_public_problem(&app, "Short Vector Test Problem").await;
    let owner = user::Entity::find()
        .filter(user::Column::Username.eq("short_vector_owner"))
        .one(&app.db)
        .await
        .expect("query owner")
        .expect("owner should exist");

    // Three standalone submissions in ONE batched list request - proves the
    // failure mode denies the WHOLE batch, not just the one resource the
    // single-resource tests above exercise.
    let submission_ids = [
        insert_queued_submission(&app, problem_id, owner.id).await,
        insert_queued_submission(&app, problem_id, owner.id).await,
        insert_queued_submission(&app, problem_id, owner.id).await,
    ];

    let baseline = app.get_with_token(routes::SUBMISSIONS, &owner_token).await;
    assert_eq!(
        baseline.status, 200,
        "baseline list failed: {}",
        baseline.text
    );
    assert_eq!(
        baseline.body["data"].as_array().map(Vec::len),
        Some(3),
        "baseline should list all 3 owned submissions before arming the mode: {}",
        baseline.text
    );

    // `decide_visibility` builds a full-length `Allow` vector, then pops one
    // element off - one short of the 3-resource batch this list produces.
    seed_kv(&app, "visibility_mode", "short_vector").await;

    let res = app.get_with_token(routes::SUBMISSIONS, &owner_token).await;

    assert_eq!(
        res.status, 200,
        "list endpoint itself must not 500: {}",
        res.text
    );
    assert_eq!(
        res.body["data"].as_array().map(Vec::len),
        Some(0),
        "a wrong-length decision vector must deny EVERY resource in the batch, \
         not just one; got: {}",
        res.text
    );
    // None of the three ids leaked through under a different guise either.
    let leaked: Vec<&serde_json::Value> = res.body["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| submission_ids.contains(&(item["id"].as_i64().unwrap_or(-1) as i32)))
        .collect();
    assert!(
        leaked.is_empty(),
        "a denied submission leaked into the list: {}",
        res.text
    );
}

/// Over-limit `Redact` field masks. The host caps a single decision's field
/// mask at `MAX_MASK_PATH_SEGMENTS` (32 dot-separated segments per path),
/// `MAX_MASK_PATH_BYTES` (256 bytes per path), and `MAX_MASK_FIELDS` (64
/// paths per decision) - see `packages/server/src/visibility/plugin_query.rs`.
/// Those three limits already have unit-level coverage in that file; these
/// tests prove the SAME limits deny the whole batch end-to-end, through a
/// real plugin reached over a real HTTP handler, not just against a
/// hand-built `WireDecision` in a unit test.
#[tokio::test]
async fn over_limit_mask_segments_denies() {
    assert_denies_single_submission_read("over_limit_segments").await;
}

#[tokio::test]
async fn over_limit_mask_bytes_denies() {
    assert_denies_single_submission_read("over_limit_bytes").await;
}

#[tokio::test]
async fn over_limit_mask_fields_denies() {
    assert_denies_single_submission_read("over_limit_fields").await;
}

// == Task 14: a WORKING decide_visibility, through real handlers ==========
//
// Up to this point the kernel's two headline capabilities - omitting a
// denied resource from a list, and masking a redacted one - were proven only
// by kernel-internal unit tests; nothing exercised those contracts through a
// live HTTP handler with a real registered plugin. These four tests drive
// `list_contest_problems` (`GET /api/v1/contests/{id}/problems`) and
// `get_problem` (`GET /api/v1/problems/{id}`) - real handlers, over real
// HTTP - against the fixture plugin's `deny_resource_keys` /
// `redact_resource_keys` / `redact_resource_fields` KV-driven mechanism
// (`tests/fixtures/server-plugin/src/lib.rs`'s normal path).

/// Create a public, already-active contest (via the real handler, which
/// requires `contest:manage`) and attach three problems to it with distinct
/// labels. `TestApp::add_problem_to_contest` hardcodes label `"A"`, so it can
/// only be used once per contest; this posts to the same route directly with
/// a distinct label per problem instead. Returns `(contest_id, p1, p2, p3)`
/// in position order (`add_contest_problem` auto-assigns increasing
/// `position` when omitted, and `list_contest_problems` orders by
/// `position` ascending).
async fn setup_three_problem_contest(app: &TestApp, admin_token: &str) -> (i32, i32, i32, i32) {
    let contest_id = app
        .create_contest(admin_token, "Kernel Working-Path Contest", true, true)
        .await;

    let mut problem_ids = Vec::new();
    for (title, label) in [
        ("Working Path Problem A", "A"),
        ("Working Path Problem B", "B"),
        ("Working Path Problem C", "C"),
    ] {
        let problem_id = insert_public_problem(app, title).await;
        let res = app
            .post_with_token(
                &routes::contest_problems(contest_id),
                &serde_json::json!({ "problem_id": problem_id, "label": label }),
                admin_token,
            )
            .await;
        assert_eq!(
            res.status, 201,
            "add problem `{title}` to contest failed: {}",
            res.text
        );
        problem_ids.push(problem_id);
    }

    (contest_id, problem_ids[0], problem_ids[1], problem_ids[2])
}

#[tokio::test]
async fn decide_visibility_list_partial_omission() {
    let app = TestApp::spawn_with_plugins().await;
    let admin_token = app
        .create_user_with_role("list_omit_admin", "pass1234", "admin")
        .await;
    let viewer_token = app
        .create_authenticated_user("list_omit_viewer", "pass1234")
        .await;

    let (contest_id, p1, p2, p3) = setup_three_problem_contest(&app, &admin_token).await;

    seed_kv(&app, "deny_resource_keys", &format!("problem:{p2}")).await;

    let res = app
        .get_with_token(&routes::contest_problems(contest_id), &viewer_token)
        .await;
    assert_eq!(
        res.status, 200,
        "list_contest_problems failed: {}",
        res.text
    );

    let data = res
        .body
        .as_array()
        .expect("response should be a JSON array");
    assert_eq!(
        data.len(),
        2,
        "exactly the two non-denied problems should be returned: {}",
        res.text
    );
    assert!(
        data.iter().all(|item| item["problem_id"] != p2),
        "the denied problem must be absent ENTIRELY, not a null/placeholder entry: {}",
        res.text
    );

    for (id, label, title) in [
        (p1, "A", "Working Path Problem A"),
        (p3, "C", "Working Path Problem C"),
    ] {
        let survivor = data
            .iter()
            .find(|item| item["problem_id"] == id)
            .unwrap_or_else(|| panic!("survivor problem {id} missing from {}", res.text));
        assert_eq!(survivor["contest_id"], contest_id);
        assert_eq!(survivor["label"], label);
        assert_eq!(survivor["problem_title"], title);
        assert!(
            survivor["position"].is_number(),
            "surviving entries must keep their full field set: {}",
            res.text
        );
    }
}

#[tokio::test]
async fn decide_visibility_list_masking() {
    let app = TestApp::spawn_with_plugins().await;
    let admin_token = app
        .create_user_with_role("list_mask_admin", "pass1234", "admin")
        .await;
    let viewer_token = app
        .create_authenticated_user("list_mask_viewer", "pass1234")
        .await;

    let (contest_id, p1, p2, p3) = setup_three_problem_contest(&app, &admin_token).await;

    seed_kv(&app, "redact_resource_keys", &format!("problem:{p2}")).await;

    let res = app
        .get_with_token(&routes::contest_problems(contest_id), &viewer_token)
        .await;
    assert_eq!(
        res.status, 200,
        "list_contest_problems failed: {}",
        res.text
    );

    let data = res
        .body
        .as_array()
        .expect("response should be a JSON array");
    assert_eq!(
        data.len(),
        3,
        "a Redact decision must still show the problem in the list, only masked: {}",
        res.text
    );

    let redacted = data
        .iter()
        .find(|item| item["problem_id"] == p2)
        .unwrap_or_else(|| panic!("redacted problem {p2} missing from {}", res.text));
    assert!(
        redacted["label"].is_null(),
        "masked field `label` should be blanked: {}",
        res.text
    );
    assert!(
        redacted["problem_title"].is_null(),
        "masked field `problem_title` should be blanked: {}",
        res.text
    );
    // Unmasked fields on the SAME entry are left alone - Redact narrows
    // exactly the nominated fields, nothing else.
    assert_eq!(redacted["contest_id"], contest_id);
    assert_eq!(redacted["problem_id"], p2);
    assert!(redacted["position"].is_number());

    for (id, label, title) in [
        (p1, "A", "Working Path Problem A"),
        (p3, "C", "Working Path Problem C"),
    ] {
        let survivor = data
            .iter()
            .find(|item| item["problem_id"] == id)
            .unwrap_or_else(|| panic!("survivor problem {id} missing from {}", res.text));
        assert_eq!(survivor["label"], label);
        assert_eq!(survivor["problem_title"], title);
    }
}

#[tokio::test]
async fn decide_visibility_detail_deny() {
    let app = TestApp::spawn_with_plugins().await;
    let viewer_token = app
        .create_authenticated_user("detail_deny_viewer", "pass1234")
        .await;
    let problem_id = insert_public_problem(&app, "Detail Deny Problem").await;

    seed_kv(&app, "deny_resource_keys", &format!("problem:{problem_id}")).await;

    let res = app
        .get_with_token(&routes::problem(problem_id), &viewer_token)
        .await;

    assert_eq!(
        res.status, 404,
        "a plugin-authored Deny on a single-resource read must surface as \
         404 NOT_FOUND, never 403 and never 500: {}",
        res.text
    );
    assert_eq!(res.body["code"], "NOT_FOUND");
}

#[tokio::test]
async fn decide_visibility_mixed_batch_lands_on_correct_resources() {
    let app = TestApp::spawn_with_plugins().await;
    let admin_token = app
        .create_user_with_role("mixed_admin", "pass1234", "admin")
        .await;
    let viewer_token = app
        .create_authenticated_user("mixed_viewer", "pass1234")
        .await;

    let (contest_id, p_allow, p_deny, p_redact) =
        setup_three_problem_contest(&app, &admin_token).await;

    seed_kv(&app, "deny_resource_keys", &format!("problem:{p_deny}")).await;
    seed_kv(&app, "redact_resource_keys", &format!("problem:{p_redact}")).await;

    let res = app
        .get_with_token(&routes::contest_problems(contest_id), &viewer_token)
        .await;
    assert_eq!(
        res.status, 200,
        "list_contest_problems failed: {}",
        res.text
    );

    let data = res
        .body
        .as_array()
        .expect("response should be a JSON array");
    assert_eq!(
        data.len(),
        2,
        "the denied problem must be omitted, leaving exactly the allowed and \
         redacted ones: {}",
        res.text
    );

    // Positional check FIRST: `add_contest_problem` assigns positions in
    // insertion order (allow, deny, redact) and `list_contest_problems`
    // orders by `position` ascending, so with the middle problem denied, the
    // allowed problem must land at index 0 and the redacted one at index 1.
    // Asserting by index - not just by searching the array for an id - is
    // what actually catches a decision vector answering correctly for the
    // WRONG resource: positional misalignment between the decision vector
    // and the row vector, the most dangerous defect a handler rewiring can
    // have.
    assert_eq!(
        data[0]["problem_id"], p_allow,
        "index 0 must be the allowed problem: {}",
        res.text
    );
    assert_eq!(
        data[1]["problem_id"], p_redact,
        "index 1 must be the redacted problem: {}",
        res.text
    );

    // The allowed problem is untouched.
    assert_eq!(data[0]["label"], "A");
    assert_eq!(data[0]["problem_title"], "Working Path Problem A");

    // The redacted problem is present but masked, not denied.
    assert!(
        data[1]["label"].is_null(),
        "the redacted problem's label must be blanked: {}",
        res.text
    );
    assert!(
        data[1]["problem_title"].is_null(),
        "the redacted problem's problem_title must be blanked: {}",
        res.text
    );

    // The denied problem is absent entirely, at neither index.
    assert!(
        data.iter().all(|item| item["problem_id"] != p_deny),
        "the denied problem must not appear anywhere in the response: {}",
        res.text
    );
}

/// I1: every contest-level reachability gate must run through the kernel.
///
/// `get_contest`, `get_contest_my_info`, `list_contests`, `list_participants`
/// and `list_contest_submissions`' top gate historically called
/// `check_contest_access` directly instead of
/// `kernel.decide(Action::Read, Resource::Contest(id))`. That is invisible
/// while no plugin narrows `Resource::Contest` - `decide_contest` is a
/// byte-identical port of `check_contest_access` - so a kernel-level unit
/// test cannot detect it: it passes whether or not the handler ever calls
/// the kernel.
///
/// This test detects it at the only layer that can. The viewer is a plain
/// contestant (NOT an admin: `admin_override` short-circuits before
/// `query_plugins`, so an admin viewer would never reach the fixture) reading
/// a PUBLIC, already-active contest - a subject the HOST rules affirmatively
/// ALLOW. The baseline block below proves that: all five endpoints answer
/// 200 before anything is seeded. Only then is `contest:{id}` nominated in
/// the fixture plugin's `deny_resource_keys`, so the sole difference between
/// the two halves is the plugin's answer for `Resource::Contest`. Any
/// endpoint still answering 200 in the second half is bypassing the kernel.
#[tokio::test]
async fn contest_level_gates_all_route_through_the_kernel() {
    let app = TestApp::spawn_with_plugins().await;
    let admin_token = app
        .create_user_with_role("i1_gate_admin", "pass1234", "admin")
        .await;
    let viewer_token = app
        .create_authenticated_user("i1_gate_viewer", "pass1234")
        .await;

    // Public + already active + submissions_visible, so the host rules
    // allow this plain contestant on every one of the five endpoints.
    let contest_id = app
        .create_contest(&admin_token, "I1 Kernel Routing Contest", true, true)
        .await;

    // -- Baseline: the host rules ALLOW this viewer everywhere ------------
    // Without this half, a post-seed 404 would be unattributable: it could
    // just mean the viewer never had access at all.

    let res = app
        .get_with_token(&routes::contest(contest_id), &viewer_token)
        .await;
    assert_eq!(
        res.status, 200,
        "baseline get_contest must be allowed by host rules: {}",
        res.text
    );

    let res = app
        .get_with_token(&routes::contest_my_info(contest_id), &viewer_token)
        .await;
    assert_eq!(
        res.status, 200,
        "baseline get_contest_my_info must be allowed by host rules: {}",
        res.text
    );

    let res = app.get_with_token(routes::CONTESTS, &viewer_token).await;
    assert_eq!(
        res.status, 200,
        "baseline list_contests failed: {}",
        res.text
    );
    assert!(
        res.body["data"]
            .as_array()
            .expect("list_contests returns a data array")
            .iter()
            .any(|c| c["id"] == contest_id),
        "baseline list_contests must include the contest: {}",
        res.text
    );

    let res = app
        .get_with_token(&routes::contest_participants(contest_id), &viewer_token)
        .await;
    assert_eq!(
        res.status, 200,
        "baseline list_participants must be allowed by host rules: {}",
        res.text
    );

    let res = app
        .get_with_token(&routes::contest_submissions(contest_id), &viewer_token)
        .await;
    assert_eq!(
        res.status, 200,
        "baseline list_contest_submissions must be allowed by host rules: {}",
        res.text
    );

    // -- Nominate the contest for a plugin Deny ---------------------------
    seed_kv(&app, "deny_resource_keys", &format!("contest:{contest_id}")).await;

    // Control: `list_contest_problems` already gated on
    // `Resource::Contest` BEFORE I1, so it is not what this test is
    // proving - it is here to prove the plugin Deny is actually LIVE at
    // this moment. The fixture's `read_kv_csv` swallows host errors and
    // returns an empty list (-> Allow), so under heavy DB-pool contention
    // a seeded key can silently fail to read back. If THIS assertion is
    // the one that fails, the fixture was not live and the run is
    // inconclusive - it is not evidence of a handler bypass. If this
    // passes and any assertion below fails, that endpoint is genuinely
    // not consulting the kernel.
    let res = app
        .get_with_token(&routes::contest_problems(contest_id), &viewer_token)
        .await;
    assert_eq!(
        res.status, 404,
        "CONTROL (not the assertion under test): the fixture plugin's Deny \
         on contest:{contest_id} is not live, so this run proves nothing \
         about the five gates below: {}",
        res.text
    );

    // -- Every contest-level gate must now deny ---------------------------

    let res = app
        .get_with_token(&routes::contest(contest_id), &viewer_token)
        .await;
    assert_eq!(
        res.status, 404,
        "get_contest must honour a plugin Deny on Resource::Contest: {}",
        res.text
    );

    let res = app
        .get_with_token(&routes::contest_my_info(contest_id), &viewer_token)
        .await;
    assert_eq!(
        res.status, 404,
        "get_contest_my_info must honour a plugin Deny on Resource::Contest: {}",
        res.text
    );

    let res = app.get_with_token(routes::CONTESTS, &viewer_token).await;
    assert_eq!(res.status, 200, "list_contests failed: {}", res.text);
    assert!(
        res.body["data"]
            .as_array()
            .expect("list_contests returns a data array")
            .iter()
            .all(|c| c["id"] != contest_id),
        "list_contests must omit a contest the plugin denied: {}",
        res.text
    );

    let res = app
        .get_with_token(&routes::contest_participants(contest_id), &viewer_token)
        .await;
    assert_eq!(
        res.status, 404,
        "list_participants must honour a plugin Deny on Resource::Contest: {}",
        res.text
    );

    let res = app
        .get_with_token(&routes::contest_submissions(contest_id), &viewer_token)
        .await;
    assert_eq!(
        res.status, 404,
        "list_contest_submissions' top gate must honour a plugin Deny on \
         Resource::Contest: {}",
        res.text
    );
}

// ---------------------------------------------------------------------
// Task 20 Item 6 (I5): `Decision::is_denied()` treats `Redact` as NOT
// denied by design (see decision.rs's `is_denied_true_only_for_deny`).
// `get_test_case` and `download_attachment` both gate reachability with a
// bare `.is_denied()` check on `Resource::Problem` (and, for the
// attachment, a second check on `Resource::Attachment`) rather than going
// through `Visible<T>`/`into_masked_json` - each site's own doc comment
// says a `Redact` there degenerates to Allow, since there is no masking
// step afterwards. These tests pin that documented behavior against a
// REAL plugin `Redact` answer, not just a doc comment: a plugin nominating
// the problem (or the attachment) via `redact_resource_keys` must not
// cause a 404, and the content that comes back must be the real,
// unmasked content - never null, and never blocked.
// ---------------------------------------------------------------------

#[tokio::test]
async fn get_test_case_redact_on_problem_resource_still_returns_full_content() {
    let app = TestApp::spawn_with_plugins().await;
    // create_test_case requires `problem:edit`, which plain
    // create_authenticated_user does not grant.
    let admin_token = app
        .create_user_with_role("tc_redact_admin", "pass1234", "admin")
        .await;
    let viewer_token = app
        .create_authenticated_user("tc_redact_viewer", "pass1234")
        .await;

    let problem_id = insert_public_problem(&app, "TC Redact Problem").await;
    let tc_id = app.create_test_case(problem_id, &admin_token).await;

    seed_kv(
        &app,
        "redact_resource_keys",
        &format!("problem:{problem_id}"),
    )
    .await;

    let res = app
        .get_with_token(&routes::test_case(problem_id, tc_id), &viewer_token)
        .await;

    assert_eq!(
        res.status, 200,
        "a plugin Redact on Resource::Problem must not deny get_test_case \
         (is_denied() only matches Deny): {}",
        res.text
    );
    assert_eq!(
        res.body["input"], "5\n1 2 3 4 5",
        "the response is built straight from the row with no masking step - \
         content must be the real, unmasked value: {}",
        res.text
    );
    assert_eq!(res.body["expected_output"], "15");
    assert_eq!(res.body["score"], 10);
}

#[tokio::test]
async fn download_attachment_redact_on_attachment_resource_still_returns_full_content() {
    let app = TestApp::spawn_with_plugins().await;
    // upload_attachment requires `problem:edit`, which plain
    // create_authenticated_user does not grant.
    let admin_token = app
        .create_user_with_role("attach_redact_admin", "pass1234", "admin")
        .await;
    let viewer_token = app
        .create_authenticated_user("attach_redact_viewer", "pass1234")
        .await;

    let problem_id = insert_public_problem(&app, "Attachment Redact Problem").await;
    let file_bytes = b"the real attachment content".to_vec();
    let upload_res = app
        .upload_attachment(
            problem_id,
            "notes.txt",
            file_bytes.clone(),
            None,
            &admin_token,
        )
        .await;
    assert_eq!(
        upload_res.status, 201,
        "attachment upload failed: {}",
        upload_res.text
    );
    let ref_id = upload_res.body["id"]
        .as_str()
        .expect("upload response must carry id")
        .to_string();

    // Nominate the ATTACHMENT resource specifically (not the problem), to
    // pin the second, attachment-level gate in `download_attachment` - the
    // problem-level gate above it is left at the fixture's default Allow.
    seed_kv(
        &app,
        "redact_resource_keys",
        &format!("attachment:{ref_id}"),
    )
    .await;

    let res = app
        .download_raw(&routes::attachment(problem_id, &ref_id), &viewer_token)
        .await;

    assert_eq!(
        res.status().as_u16(),
        200,
        "a plugin Redact on Resource::Attachment must not deny download_attachment \
         (is_denied() only matches Deny)"
    );
    let bytes = res.bytes().await.unwrap();
    assert_eq!(
        bytes.as_ref(),
        file_bytes.as_slice(),
        "build_blob_response bypasses Visible<T>/into_masked_json entirely - \
         the streamed bytes must be the real, unmasked content"
    );
}
