//! Batch A / item 3: `rejudge.rs`'s `apply_submission_judgement` and
//! `rejudge_submission` built their response with a HARDCODED
//! `VisibilityContext { has_view_all: true }` and shipped it straight
//! through `Json(...)`, never through `VisibilityKernel`/`Visible<T>`.
//! `submission:rejudge` is a permission documented as independent of
//! `submission:view_all`/`contest:manage` (see
//! `broccoli-types/src/permissions.rs`), and neither handler filtered its DB
//! fetch by ownership or contest - so ANY user holding only
//! `submission:rejudge` could exfiltrate full source/compile-output/verdict
//! for ANY submission id, in any contest, frozen or not, just by triggering
//! (or re-triggering) a rejudge on it. Confirmed present at the pre-fix
//! commit `3dde5d42` - this predates the visibility-kernel branch.
//!
//! These tests pin the fix: the response now goes through the same
//! `Resource::Submission` kernel Read decision `GET /submissions/{id}`
//! would make for the SAME caller
//! (`apply_filter_to_response_after_mutation`,
//! `handlers/submission/filter.rs`). A `Deny` does NOT fail the request -
//! the mutation is authorised by `submission:rejudge` alone and must still
//! go through - it degrades the response to a minimal `{"id": ...}`
//! acknowledgement instead. A plugin `Redact` blanks exactly the fields a
//! `GET` would blank, never more, never less. A caller who ALSO holds
//! `submission:view_all` still sees everything, same as `GET` would show
//! them - the fix does not over-restrict a real admin.

use broccoli_server_sdk::permissions as perm;
use chrono::Utc;
use common::{SubmissionStatus, Verdict};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use server::entity::{problem, submission, submission_judgement, user};

use crate::common::{TestApp, routes};

/// Insert a public, standalone-submittable problem directly via the DB -
/// mirrors the identical helper (and its doc comment explaining why this
/// bypasses `TestApp::create_problem`) in `visibility_plugin.rs`.
async fn insert_public_problem(app: &TestApp, title: &str) -> i32 {
    let now = Utc::now();
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

async fn find_user_id(app: &TestApp, username: &str) -> i32 {
    user::Entity::find()
        .filter(user::Column::Username.eq(username))
        .one(&app.db)
        .await
        .expect("query user")
        .expect("user should exist")
        .id
}

/// Insert an already-`Judged`, standalone (contest-less) submission owned by
/// `owner_id`, with a real verdict/score/compile-output so a leak (or its
/// absence) is observable.
async fn insert_judged_submission(app: &TestApp, problem_id: i32, owner_id: i32) -> i32 {
    let now = Utc::now();
    let submission = submission::ActiveModel {
        files: Set(serde_json::json!([
            { "filename": "main.cpp", "content": "int main() { return 0; }" }
        ])),
        language: Set("cpp".into()),
        user_id: Set(owner_id),
        problem_id: Set(problem_id),
        contest_id: Set(None),
        contest_type: Set("standard".into()),
        status: Set(SubmissionStatus::Judged),
        verdict: Set(Some(Verdict::Accepted)),
        score: Set(Some(100.0)),
        time_used: Set(Some(7)),
        memory_used: Set(Some(128)),
        compile_output: Set(Some("g++ main.cpp -o main\n".into())),
        judge_epoch: Set(1),
        created_at: Set(now),
        judged_at: Set(Some(now)),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert judged submission");
    submission.id
}

// == Regression: a `submission:rejudge`-only caller must not see content a
// == `GET` would deny them ===================================================

#[tokio::test]
async fn rejudge_only_user_triggers_rejudge_without_leaking_unreachable_submission() {
    let app = TestApp::spawn().await;

    app.create_authenticated_user("rjv_owner", "pass1234").await;
    let owner_id = find_user_id(&app, "rjv_owner").await;
    let problem_id = insert_public_problem(&app, "Rejudge Visibility Problem").await;
    let submission_id = insert_judged_submission(&app, problem_id, owner_id).await;

    let operator_token = app
        .create_user_with_permissions("rjv_operator", "pass1234", &[perm::SUBMISSION_REJUDGE])
        .await;

    // Premise check: this operator cannot read the submission via the normal
    // read path at all - not the owner, no `submission:view_all`, and the
    // submission is standalone (no contest to be a participant of). If this
    // isn't a 404, the assertions below are vacuous.
    let baseline_get = app
        .get_with_token(&routes::submission(submission_id), &operator_token)
        .await;
    assert_eq!(
        baseline_get.status, 404,
        "premise broken: rejudge-only operator should not be able to GET this submission: {}",
        baseline_get.text
    );

    let res = app
        .post_with_token(
            &routes::submission_rejudge(submission_id),
            &serde_json::json!({}),
            &operator_token,
        )
        .await;

    // The mutation must still succeed - `submission:rejudge` alone
    // authorises the write, independent of Read visibility.
    assert_eq!(
        res.status, 200,
        "rejudge must succeed for a submission:rejudge holder even when they can't read it: {}",
        res.text
    );

    // But the response must carry nothing beyond the id the caller already
    // supplied in the URL - no files, no verdict, no compile output. This is
    // the exact shape a denied Read degrades to.
    let obj = res.body.as_object().expect("response should be an object");
    assert_eq!(
        obj.len(),
        1,
        "a denied-read response must contain nothing but the id, got: {}",
        res.body
    );
    assert_eq!(res.body["id"], submission_id);

    // Prove the mutation genuinely happened despite the suppressed response:
    // the row must have flipped to Queued.
    let updated = submission::Entity::find_by_id(submission_id)
        .one(&app.db)
        .await
        .expect("query submission")
        .expect("submission should still exist");
    assert_eq!(
        updated.status,
        SubmissionStatus::Queued,
        "rejudge mutation should have happened even though the response was suppressed"
    );
}

#[tokio::test]
async fn rejudge_only_user_applies_judgement_without_leaking_unreachable_submission() {
    let app = TestApp::spawn().await;

    app.create_authenticated_user("rjv_apply_owner", "pass1234")
        .await;
    let owner_id = find_user_id(&app, "rjv_apply_owner").await;
    let problem_id = insert_public_problem(&app, "Apply Visibility Problem").await;
    let submission_id = insert_judged_submission(&app, problem_id, owner_id).await;

    // A second, finalized, non-current judgement candidate - as if an
    // earlier `apply_immediately=false` rejudge had produced one - with a
    // DIFFERENT verdict, so applying it is an observable mutation.
    let now = Utc::now();
    let candidate = submission_judgement::ActiveModel {
        submission_id: Set(submission_id),
        version: Set(2),
        is_current: Set(false),
        is_finalized: Set(true),
        status: Set(SubmissionStatus::Judged),
        verdict: Set(Some(Verdict::WrongAnswer)),
        score: Set(Some(0.0)),
        judge_epoch: Set(2),
        created_at: Set(now),
        finalized_at: Set(Some(now)),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert candidate judgement");

    let operator_token = app
        .create_user_with_permissions(
            "rjv_apply_operator",
            "pass1234",
            &[perm::SUBMISSION_REJUDGE],
        )
        .await;

    let baseline_get = app
        .get_with_token(&routes::submission(submission_id), &operator_token)
        .await;
    assert_eq!(
        baseline_get.status, 404,
        "premise broken: rejudge-only operator should not be able to GET this submission: {}",
        baseline_get.text
    );

    let res = app
        .post_with_token(
            &routes::submission_judgement_apply(submission_id, candidate.id),
            &serde_json::json!({}),
            &operator_token,
        )
        .await;

    assert_eq!(
        res.status, 200,
        "apply-judgement must succeed for a submission:rejudge holder: {}",
        res.text
    );
    let obj = res.body.as_object().expect("response should be an object");
    assert_eq!(
        obj.len(),
        1,
        "a denied-read response must contain nothing but the id, got: {}",
        res.body
    );
    assert_eq!(res.body["id"], submission_id);

    let updated = submission::Entity::find_by_id(submission_id)
        .one(&app.db)
        .await
        .expect("query submission")
        .expect("submission should still exist");
    assert_eq!(
        updated.verdict,
        Some(Verdict::WrongAnswer),
        "apply mutation should have happened even though the response was suppressed"
    );
}

// == Converse: a caller who ALSO holds submission:view_all is not
// == over-restricted =========================================================

#[tokio::test]
async fn rejudge_with_view_all_still_sees_full_response() {
    let app = TestApp::spawn().await;

    app.create_authenticated_user("rjv_va_owner", "pass1234")
        .await;
    let owner_id = find_user_id(&app, "rjv_va_owner").await;
    let problem_id = insert_public_problem(&app, "View All Visibility Problem").await;
    let submission_id = insert_judged_submission(&app, problem_id, owner_id).await;

    let admin_token = app
        .create_user_with_permissions(
            "rjv_va_operator",
            "pass1234",
            &[perm::SUBMISSION_REJUDGE, perm::SUBMISSION_VIEW_ALL],
        )
        .await;

    // GET already works for this caller - positive control.
    let baseline_get = app
        .get_with_token(&routes::submission(submission_id), &admin_token)
        .await;
    assert_eq!(baseline_get.status, 200, "baseline GET failed");

    // `apply_immediately=false` so the submission's cached verdict/score
    // survive the call - a genuinely-cleared-by-the-mutation field would be
    // indistinguishable from a masked one, which would make this assertion
    // vacuous.
    let res = app
        .post_with_token(
            &routes::submission_rejudge(submission_id),
            &serde_json::json!({ "apply_immediately": false }),
            &admin_token,
        )
        .await;

    assert_eq!(res.status, 200, "rejudge failed: {}", res.text);
    assert_eq!(
        res.body["user_id"], owner_id,
        "a submission:view_all holder should see the real owner, got: {}",
        res.body
    );
    assert_eq!(res.body["language"], "cpp");
    assert!(
        res.body["files"].as_array().is_some_and(|f| !f.is_empty()),
        "a submission:view_all holder should see the source files, got: {}",
        res.body
    );
}

// == Redact: a plugin decision must shape the response the same way it
// == would shape a GET ========================================================

/// Seed the fixture plugin's `redact_submission_ids` KV key (read by
/// `decide_visibility` in `tests/fixtures/server-plugin/src/lib.rs`) - same
/// mechanism `visibility_plugin.rs`'s `plugin_cannot_author_a_verdict` uses
/// to prove a plugin can only blank `result.verdict`/`result.score`, never
/// author one. Reused here as a stand-in for a frozen ICPC contest / a
/// restricted IOI feedback level: both are, from the kernel's perspective,
/// just a `Redact { fields: ["result.verdict", "result.score"] }` decision
/// on top of a host `Allow` - this test doesn't need real freeze/feedback
/// plugin logic to pin that the REJUDGE response respects whatever decision
/// comes back, exactly like a `GET` would.
async fn seed_redact_submission_id(app: &TestApp, submission_id: i32) {
    let route = routes::plugin_proxy("server-plugin", "kv/redact_submission_ids");
    let res = app
        .post_without_token(
            &route,
            &serde_json::json!({ "value": submission_id.to_string() }),
        )
        .await;
    assert_eq!(res.status, 200, "seeding KV failed: {}", res.text);
}

#[tokio::test]
async fn rejudge_response_is_redacted_like_a_read_would_be() {
    let app = TestApp::spawn_with_plugins().await;

    // Self-owned submission, so the host-level decision is Allow (owner
    // bypass) without needing a contest/participant fixture - only the
    // PLUGIN's Redact is under test here.
    let owner_token = app
        .create_user_with_permissions(
            "rjv_redact_operator",
            "pass1234",
            &[perm::SUBMISSION_REJUDGE],
        )
        .await;
    let owner_id = find_user_id(&app, "rjv_redact_operator").await;
    let problem_id = insert_public_problem(&app, "Redact Visibility Problem").await;
    let submission_id = insert_judged_submission(&app, problem_id, owner_id).await;

    // Unmasked baseline, before the plugin is armed.
    let baseline_get = app
        .get_with_token(&routes::submission(submission_id), &owner_token)
        .await;
    assert_eq!(baseline_get.status, 200, "baseline GET failed");
    assert_eq!(baseline_get.body["result"]["verdict"], "Accepted");
    assert_eq!(baseline_get.body["result"]["score"], 100.0);

    seed_redact_submission_id(&app, submission_id).await;

    // Confirm a GET now sees the redacted shape - this is the "ground
    // truth" the rejudge response must match.
    let redacted_get = app
        .get_with_token(&routes::submission(submission_id), &owner_token)
        .await;
    assert_eq!(redacted_get.status, 200);
    assert!(redacted_get.body["result"]["verdict"].is_null());
    assert!(redacted_get.body["result"]["score"].is_null());

    let res = app
        .post_with_token(
            &routes::submission_rejudge(submission_id),
            &serde_json::json!({ "apply_immediately": false }),
            &owner_token,
        )
        .await;

    assert_eq!(res.status, 200, "rejudge failed: {}", res.text);
    assert!(
        res.body["result"]["verdict"].is_null(),
        "rejudge response must be redacted the same way a GET is, got: {}",
        res.body
    );
    assert!(
        res.body["result"]["score"].is_null(),
        "rejudge response must be redacted the same way a GET is, got: {}",
        res.body
    );
    // Redact narrows exactly the nominated fields - unrelated fields (id,
    // status) stay visible, unlike the full-Deny case above.
    assert_eq!(res.body["id"], submission_id);
    assert_eq!(res.body["status"], "Judged");
}
