//! Contest-management defects found by the kernel audit (Nov 2026).
//!
//! Each `#[ignore]`d test reproduces a bug and asserts the CORRECT behaviour,
//! so it fails today. Run the whole set with:
//!
//! `cargo test -p server --test integration audit_contest -- --ignored`
//!
//! When fixing a bug, delete its `#[ignore]` so the test becomes a regression
//! guard.

use serde_json::json;

use crate::common::{TestApp, routes};

async fn user_id(app: &TestApp, token: &str) -> i32 {
    app.get_with_token(routes::ME, token).await.id()
}

/// BUG (high): `POST /problems/{id}/submissions` (standalone, `contest_id =
/// None`) authorises `Action::Submit` with the READ rule
/// (`host_decide` ignores `Action`). Rule 9 of
/// `decide_standalone_problem_access` lets a hidden problem through when it
/// is attached to a started contest the caller participates in. The row is
/// stored with `contest_id = None`, so contest-scoped plugin config
/// (submission-limit, cooldown) never applies, IOI/ICPC feedback masking is
/// skipped (both only mask when `contest_id.is_some()`), and the attempt is
/// invisible to standings. Only codelink-bracket blocks it, in its own hook.
/// Location: visibility/host_rules.rs (`_action` unused,
/// `decide_standalone_problem_access`), handlers/submission/mod.rs.
#[tokio::test]
#[ignore = "BUG: hidden contest problem can be judged outside the contest via the standalone route"]
async fn hidden_contest_problem_rejects_standalone_submission() {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_role("ac_admin", "password123", "admin")
        .await;
    let contestant = app
        .create_user_with_role("ac_p", "password123", "contestant")
        .await;
    let pid = app.create_hidden_problem(&admin, "Hidden").await;
    let cid = app.create_contest(&admin, "Running", false, false).await;
    app.add_problem_to_contest(cid, pid, &admin).await;
    let uid = user_id(&app, &contestant).await;
    let add = app
        .post_with_token(
            &routes::contest_participants(cid),
            &json!({"user_id": uid}),
            &admin,
        )
        .await;
    assert!(add.status < 300, "enrol failed: {}", add.text);

    // Control: the in-contest route is the sanctioned path.
    let in_contest = app
        .post_with_token(
            &routes::contest_problem_submissions(cid, pid),
            &json!({"language": "cpp", "files": [{"filename": "main.cpp", "content": "int main(){}"}]}),
            &contestant,
        )
        .await;
    assert_eq!(in_contest.status, 201, "control: {}", in_contest.text);

    let standalone = app
        .post_with_token(
            &routes::problem_submissions(pid),
            &json!({"language": "cpp", "files": [{"filename": "main.cpp", "content": "int main(){}"}]}),
            &contestant,
        )
        .await;
    assert_ne!(
        standalone.status, 201,
        "hidden contest problem judged outside the contest: {}",
        standalone.text
    );
}

/// BUG (med): `list_contests` uses `select_only()` and omits
/// `Column::ContestType`; `ContestListItem.contest_type` is `Option<String>`
/// and sea-orm maps the missing column to `None`, so every listed contest
/// shows `contest_type: null` (the admin UI renders a red "Not set").
/// Location: handlers/contest/mod.rs `list_contests`.
#[tokio::test]
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

/// Companion to the test above: clarification lookups go through the same
/// active-parent gate, so `toggle_reply_public` (which has no kernel contest
/// gate of its own) stops working once the contest is soft-deleted.
#[tokio::test]
async fn deleted_contest_rejects_reply_visibility_toggle() {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_role("ac_admin", "password123", "admin")
        .await;
    let contestant = app
        .create_user_with_role("ac_p", "password123", "contestant")
        .await;
    let cid = app.create_contest(&admin, "C", true, false).await;
    let q = app
        .post_with_token(
            &routes::contest_clarifications(cid),
            &json!({"content": "Q?", "clarification_type": "question"}),
            &contestant,
        )
        .await;
    assert!(q.status < 300, "{}", q.text);
    let clar_id = q.id();
    let reply = app
        .post_with_token(
            &routes::contest_clarification_reply(cid, clar_id),
            &json!({"content": "A.", "is_public": false}),
            &admin,
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.text);
    let reply_id = reply.body["replies"][0]["id"].as_i64().unwrap() as i32;

    let del = app.delete_with_token(&routes::contest(cid), &admin).await;
    assert!(del.status < 300, "delete contest: {}", del.text);

    let res = app
        .post_with_token(
            &routes::contest_clarification_toggle(cid, clar_id, reply_id),
            &json!({}),
            &admin,
        )
        .await;
    assert_eq!(res.status, 404, "{}", res.text);
}
