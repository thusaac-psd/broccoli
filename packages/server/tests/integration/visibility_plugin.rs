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

use crate::common::{TestApp, routes};

/// Seed the fixture plugin's `redact_submission_ids` KV key (read by
/// `decide_visibility` in `tests/fixtures/server-plugin/src/lib.rs`) with a
/// single submission id, via the plugin's existing `kv_write` route.
async fn seed_redact_submission_id(app: &TestApp, submission_id: i32) {
    let route = routes::plugin_proxy("server-plugin", "kv/redact_submission_ids");
    let res = app
        .post_without_token(&route, &serde_json::json!({ "value": submission_id.to_string() }))
        .await;
    assert_eq!(res.status, 200, "seeding redact KV failed: {}", res.text);
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

    let owner_token = app.create_authenticated_user("widen_owner", "pass1234").await;
    let peer_token = app.create_authenticated_user("widen_peer", "pass1234").await;

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
    assert_eq!(owner_res.status, 200, "owner read failed: {}", owner_res.text);

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
    assert_eq!(baseline.status, 200, "baseline read failed: {}", baseline.text);
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

    assert_eq!(res.status, 200, "owner read should still be reachable: {}", res.text);
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
