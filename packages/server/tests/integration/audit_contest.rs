//! Contest-management defects found by the kernel audit (Nov 2026).
//!
//! Each `#[ignore]`d test reproduces a bug and asserts the CORRECT behaviour,
//! so it fails today. Run the whole set with:
//!
//! `cargo test -p server --test integration audit_contest -- --ignored`
//!
//! When fixing a bug, delete its `#[ignore]` so the test becomes a regression
//! guard.

use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};
use serde_json::{Value, json};

use crate::codelink_bracket::visible_problem_ids;
use crate::common::{TestApp, routes};

// -- B1 (#52): `POST /problems/{id}/submissions` and the sibling standalone
// `POST /problems/{id}/code-runs` decide `Action::Submit` on
// `Resource::Problem { contest_id: None }`. They used to be decided by the
// READ rule, whose rule 9 admits a HIDDEN problem when one of its contests is
// open and started and the caller is a participant, or the contest is public.
// The row was then stored with `contest_id = NULL`, which skipped
// contest-scoped plugin config, IOI/ICPC feedback masking and standings, and
// let the caller pick any `contest_type`. Submit now stops at rule 8 (see
// `visibility::host_rules::standalone_reachable_via_contest`). READ is
// unchanged, and each test below checks that first. --

fn submission_body(contest_type: Option<&str>) -> Value {
    let mut body = json!({
        "files": [{"filename": "main.cpp", "content": "#include <iostream>\nint main() {}"}],
        "language": "cpp",
    });
    if let Some(ct) = contest_type {
        body["contest_type"] = json!(ct);
    }
    body
}

fn code_run_body() -> Value {
    json!({
        "files": [{"filename": "main.cpp", "content": "#include <iostream>\nint main() {}"}],
        "language": "cpp",
        "custom_test_cases": [{"input": "5\n1 2 3 4 5", "expected_output": "15"}],
    })
}

/// Contest-less submission and code-run rows stored for `problem_id`. A
/// denied standalone request must leave both at zero.
async fn standalone_rows(app: &TestApp, problem_id: i32) -> (u64, u64) {
    use server::entity::{code_run, submission};
    let submissions = submission::Entity::find()
        .filter(submission::Column::ProblemId.eq(problem_id))
        .filter(submission::Column::ContestId.is_null())
        .count(&app.db)
        .await
        .expect("count standalone submissions");
    let code_runs = code_run::Entity::find()
        .filter(code_run::Column::ProblemId.eq(problem_id))
        .filter(code_run::Column::ContestId.is_null())
        .count(&app.db)
        .await
        .expect("count standalone code runs");
    (submissions, code_runs)
}

/// Asserts that the standalone submit (with and without a caller-chosen
/// `contest_type`) and the standalone code run are both a silent 404 for
/// `token`, and that neither request stored a row.
async fn assert_standalone_submit_hidden(app: &TestApp, problem_id: i32, token: &str, who: &str) {
    for contest_type in [None, Some("standard")] {
        let res = app
            .post_with_token(
                &routes::problem_submissions(problem_id),
                &submission_body(contest_type),
                token,
            )
            .await;
        assert_eq!(
            res.status, 404,
            "{who}: standalone submit (contest_type {contest_type:?}) to hidden contest problem \
             {problem_id} must be hidden, got {}: {}",
            res.status, res.text
        );
        assert_eq!(res.body["code"], "NOT_FOUND", "{}", res.text);
    }

    let res = app
        .post_with_token(
            &routes::problem_code_runs(problem_id),
            &code_run_body(),
            token,
        )
        .await;
    assert_eq!(
        res.status, 404,
        "{who}: standalone code run on hidden contest problem {problem_id} must be hidden, got \
         {}: {}",
        res.status, res.text
    );
    assert_eq!(res.body["code"], "NOT_FOUND", "{}", res.text);

    assert_eq!(
        standalone_rows(app, problem_id).await,
        (0, 0),
        "{who}: a denied standalone request must not store a contest-less row"
    );
}

#[tokio::test]
async fn hidden_contest_problem_rejects_standalone_submission() {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_role("admin_b1_private", "pass1234", "admin")
        .await;
    let contestant = app
        .create_authenticated_user("contestant_b1_private", "pass1234")
        .await;

    let pid = app
        .create_hidden_problem(&admin, "Hidden Contest Problem")
        .await;
    // Private, activated, started, and not yet ended.
    let cid = app
        .create_contest(&admin, "Private Running Contest", false, true)
        .await;
    app.add_problem_to_contest(cid, pid, &admin).await;

    let contestant_id = app.get_with_token(routes::ME, &contestant).await.id();
    let res = app
        .post_with_token(
            &routes::contest_participants(cid),
            &json!({ "user_id": contestant_id }),
            &admin,
        )
        .await;
    assert_eq!(res.status, 201, "enroll contestant failed: {}", res.text);

    // READ is unchanged: the participant sees the problem in the contest's
    // list and can open its statement by id. `GET /problems/{id}` is how the
    // contest UI loads a problem, and it reaches a hidden one only through
    // rule 9.
    assert!(
        visible_problem_ids(&app, cid, &contestant)
            .await
            .contains(&i64::from(pid)),
        "participant should see hidden problem {pid} in the contest problem list"
    );
    let res = app.get_with_token(&routes::problem(pid), &contestant).await;
    assert_eq!(res.status, 200, "standalone read via rule 9: {}", res.text);

    // SUBMIT is not: the standalone route would store a contest-less row.
    assert_standalone_submit_hidden(&app, pid, &contestant, "participant").await;

    // The contest route still accepts the same participant, scoped to the
    // contest.
    let res = app
        .post_with_token(
            &routes::contest_problem_submissions(cid, pid),
            &submission_body(None),
            &contestant,
        )
        .await;
    assert_eq!(res.status, 201, "contest-scoped submit: {}", res.text);
    assert_eq!(res.body["contest_id"], cid);

    // Rule 8 is unchanged: a problem editor can still submit standalone to a
    // hidden problem (to test it before release).
    let res = app
        .post_with_token(
            &routes::problem_submissions(pid),
            &submission_body(None),
            &admin,
        )
        .await;
    assert_eq!(res.status, 201, "editor standalone submit: {}", res.text);
    assert!(res.body["contest_id"].is_null());
}

#[tokio::test]
async fn hidden_problem_in_public_running_contest_rejects_standalone_submission_from_unregistered_user()
 {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_role("admin_b1_public", "pass1234", "admin")
        .await;
    let outsider = app
        .create_authenticated_user("outsider_b1_public", "pass1234")
        .await;

    let pid = app
        .create_hidden_problem(&admin, "Hidden Public-Contest Problem")
        .await;
    // Public, activated, started, and not yet ended.
    let cid = app
        .create_contest(&admin, "Public Running Contest", true, true)
        .await;
    app.add_problem_to_contest(cid, pid, &admin).await;

    // READ is unchanged: any authenticated user can view a hidden problem
    // attached to a public started contest (rule 9), with no registration.
    let res = app.get_with_token(&routes::problem(pid), &outsider).await;
    assert_eq!(res.status, 200, "standalone read via rule 9: {}", res.text);

    // The contest route refuses the unregistered user...
    let res = app
        .post_with_token(
            &routes::contest_problem_submissions(cid, pid),
            &submission_body(None),
            &outsider,
        )
        .await;
    assert_eq!(res.status, 403, "contest-scoped submit: {}", res.text);
    assert_eq!(res.body["code"], "PERMISSION_DENIED");

    // ...so the standalone route must not be a way around it.
    assert_standalone_submit_hidden(&app, pid, &outsider, "unregistered user").await;
}

/// BUG (med): `list_contests` uses `select_only()` and omits
/// `Column::ContestType`; `ContestListItem.contest_type` is `Option<String>`
/// and sea-orm maps the missing column to `None`, so every listed contest
/// shows `contest_type: null` (the admin UI renders a red "Not set").
/// Location: handlers/contest/mod.rs `list_contests`.
#[tokio::test]
#[ignore = "BUG: GET /contests always returns contest_type: null (column not selected)"]
async fn list_contests_includes_contest_type() {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_role("ac_admin", "password123", "admin")
        .await;
    let cid = app.create_contest(&admin, "C", true, true).await;

    let one = app.get_with_token(&routes::contest(cid), &admin).await;
    assert_eq!(one.body["contest_type"], "standard", "control: detail view");

    let list = app.get_with_token(routes::CONTESTS, &admin).await;
    let row = list.body["data"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["id"] == cid))
        .expect("contest listed");
    assert_eq!(row["contest_type"], "standard", "list row: {row}");
}

/// BUG (med): `unregister_from_contest` has no window guard, unlike
/// `register_for_contest` (blocks after end) and `delete_user` (refuses to
/// drop registrations in started contests). A participant can erase
/// themselves from the final standings after the contest ends; every
/// standings implementation builds rows from `contest_user`.
/// Location: handlers/contest/participants.rs `unregister_from_contest`.
#[tokio::test]
#[ignore = "BUG: participant can unregister after the contest ended (vanishes from standings)"]
async fn cannot_unregister_after_contest_end() {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_role("ac_admin", "password123", "admin")
        .await;
    let contestant = app
        .create_user_with_role("ac_p", "password123", "contestant")
        .await;
    let cid = app.create_contest(&admin, "C", true, true).await;
    app.register_for_contest(cid, &contestant).await;

    let end = app
        .patch_with_token(
            &routes::contest(cid),
            &json!({"end_time": "2021-01-01T00:00:00Z"}),
            &admin,
        )
        .await;
    assert!(end.status < 300, "end contest: {}", end.text);

    let res = app
        .delete_with_token(&routes::contest_register(cid), &contestant)
        .await;
    assert!(
        (400..500).contains(&res.status),
        "unregister after end returned {}: {}",
        res.status,
        res.text
    );
}

/// BUG (low): `find_contest_problem` only checks that the `contest_problem`
/// row exists, not that the contest is active. After `delete_contest`
/// soft-deletes the contest (and purges its contest-problem config), the
/// contest-problem config endpoints keep working, and an upsert re-creates
/// exactly the rows the delete purged. Contest-scope config correctly 404s.
/// Location: utils/contest.rs `find_contest_problem`.
#[tokio::test]
#[ignore = "BUG: contest-problem config can still be written after the contest is soft-deleted"]
async fn deleted_contest_rejects_contest_problem_config_writes() {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_role("ac_admin", "password123", "admin")
        .await;
    let cid = app.create_contest(&admin, "C", true, true).await;
    let pid = app.create_problem(&admin, "P").await;
    app.add_problem_to_contest(cid, pid, &admin).await;

    let del = app.delete_with_token(&routes::contest(cid), &admin).await;
    assert!(del.status < 300, "delete contest: {}", del.text);

    let body = json!({"config": {"time_limit": 2000}});
    let contest_scope = app
        .put_with_token(
            &routes::contest_config_ns(cid, "test-plugin", "limits"),
            &body,
            &admin,
        )
        .await;
    assert_eq!(contest_scope.status, 404, "control: contest scope 404s");

    let res = app
        .put_with_token(
            &routes::contest_problem_config_ns(cid, pid, "test-plugin", "limits"),
            &body,
            &admin,
        )
        .await;
    assert_eq!(
        res.status, 404,
        "wrote config for a deleted contest: {}",
        res.text
    );
}
