//! Task 14, part D (SDD 2026-09-15-visibility-kernel).
//!
//! `visibility_matrix.rs::contest_clarification_list` (FROZEN) only asserts
//! HTTP status codes and, at most, `data.is_array()` - it never inspects how
//! many rows come back or which ones. A prior task found a commit where
//! `list_clarifications` returned `data: []` for every viewer, including
//! admins, and that whole suite stayed green throughout. This file is the
//! dedicated contents pin that scenario was missing: same private-contest /
//! admin+participant+participant shape as `contest_clarification_list`, but
//! asserting exact row COUNT and IDENTITY per viewer, plus the one masking
//! (`Redact`) path `decide_clarification` has that the status-only suite
//! also never touches.
//!
//! (`tests/integration/clarification.rs::enforces_visibility_rules_for_questions_and_dms`
//! already does something similar and predates the visibility kernel
//! entirely - it would also have caught the `data: []` regression. This file
//! additionally pins the `Redact` masking path, which no other integration
//! test does, and mirrors `contest_clarification_list`'s exact fixture shape
//! so it reads as that suite's missing contents companion.)

use serde_json::json;

use crate::common::{TestApp, routes};

struct Fixture {
    app: TestApp,
    admin: String,
    participant_a: String,
    participant_b: String,
    contest_id: i32,
}

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

async fn setup() -> Fixture {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_role("admin", "pass1234", "admin")
        .await;
    let participant_a = app
        .create_authenticated_user("participant_a", "pass1234")
        .await;
    let participant_b = app
        .create_authenticated_user("participant_b", "pass1234")
        .await;
    // Private contest, same INSIDE_WINDOW shape `create_contest` hardcodes
    // (2020..2099) - contest reachability itself is not what this file
    // pins, so the simplest fixture that gets every viewer past the
    // contest-level gate is used.
    let contest_id = app
        .create_contest(&admin, "Clarification Contents Fixture", false, true)
        .await;
    enroll(&app, &admin, contest_id, &participant_a).await;
    enroll(&app, &admin, contest_id, &participant_b).await;
    Fixture {
        app,
        admin,
        participant_a,
        participant_b,
        contest_id,
    }
}

async fn ask_question(app: &TestApp, contest_id: i32, token: &str, content: &str) -> i32 {
    let res = app
        .post_with_token(
            &routes::contest_clarifications(contest_id),
            &json!({"content": content, "clarification_type": "question"}),
            token,
        )
        .await;
    assert_eq!(res.status, 201, "ask_question failed: {}", res.text);
    res.id()
}

fn contents(data: &serde_json::Value) -> Vec<&str> {
    data.as_array()
        .expect("data must be an array")
        .iter()
        .map(|c| c["content"].as_str().expect("content must be a string"))
        .collect()
}

mod contest_clarification_list_contents {
    use super::*;

    /// The regression this whole file exists to catch: if `list_clarifications`
    /// collapsed to `data: []` for everyone, EVERY assertion below on non-empty
    /// counts would fail, for every viewer, including the admin.
    #[tokio::test]
    async fn each_viewer_sees_exactly_their_own_rows_by_count_and_identity() {
        let f = setup().await;

        let qa = ask_question(
            &f.app,
            f.contest_id,
            &f.participant_a,
            "A's private question",
        )
        .await;
        let qb = ask_question(
            &f.app,
            f.contest_id,
            &f.participant_b,
            "B's private question",
        )
        .await;

        let ann_res = f
            .app
            .post_with_token(
                &routes::contest_clarifications(f.contest_id),
                &json!({"content": "Public announcement to all", "clarification_type": "announcement"}),
                &f.admin,
            )
            .await;
        assert_eq!(
            ann_res.status, 201,
            "announcement create failed: {}",
            ann_res.text
        );

        let b_id = user_id(&f.app, &f.participant_b).await;
        let dm_res = f
            .app
            .post_with_token(
                &routes::contest_clarifications(f.contest_id),
                &json!({
                    "content": "DM to B",
                    "clarification_type": "direct_message",
                    "recipient_id": b_id,
                }),
                &f.admin,
            )
            .await;
        assert_eq!(dm_res.status, 201, "dm create failed: {}", dm_res.text);

        // A public question with a PRIVATE reply: reachable by everyone (it's
        // public) but its legacy reply_* fields should be redacted for anyone
        // who is neither the author nor a contest:manage admin.
        let masked_res = f
            .app
            .post_with_token(
                &routes::contest_clarifications(f.contest_id),
                &json!({
                    "content": "Public Q from admin needing reply",
                    "clarification_type": "question",
                    "is_public": true,
                }),
                &f.admin,
            )
            .await;
        assert_eq!(
            masked_res.status, 201,
            "masked question create failed: {}",
            masked_res.text
        );
        let masked_id = masked_res.id();
        let reply_res = f
            .app
            .post_with_token(
                &routes::contest_clarification_reply(f.contest_id, masked_id),
                &json!({"content": "shh", "is_public": false}),
                &f.admin,
            )
            .await;
        assert_eq!(reply_res.status, 200, "reply failed: {}", reply_res.text);

        // --- admin: contest:manage bypasses every per-row rule, sees all 5 ---
        let res_admin = f
            .app
            .get_with_token(&routes::contest_clarifications(f.contest_id), &f.admin)
            .await;
        assert_eq!(res_admin.status, 200);
        let admin_contents = contents(&res_admin.body["data"]);
        assert_eq!(
            admin_contents.len(),
            5,
            "admin must see all 5 rows, got: {admin_contents:?}"
        );
        for expected in [
            "A's private question",
            "B's private question",
            "Public announcement to all",
            "DM to B",
            "Public Q from admin needing reply",
        ] {
            assert!(
                admin_contents.contains(&expected),
                "admin missing row {expected:?}, got: {admin_contents:?}"
            );
        }
        // Admin is a participant (contest:manage), so the masked reply is
        // shown in full to them, not redacted.
        let admin_masked_row = res_admin.body["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["content"] == "Public Q from admin needing reply")
            .expect("admin must see the masked-for-others row");
        assert_eq!(admin_masked_row["reply_content"], "shh");

        // --- participant_a: own question + the public announcement + the
        // public-but-reply-masked question (redacted). Never B's private
        // question, never the DM to B. ---
        let res_a = f
            .app
            .get_with_token(
                &routes::contest_clarifications(f.contest_id),
                &f.participant_a,
            )
            .await;
        assert_eq!(res_a.status, 200);
        let a_contents = contents(&res_a.body["data"]);
        assert_eq!(
            a_contents.len(),
            3,
            "participant_a row count mismatch, got: {a_contents:?}"
        );
        assert!(a_contents.contains(&"A's private question"));
        assert!(a_contents.contains(&"Public announcement to all"));
        assert!(a_contents.contains(&"Public Q from admin needing reply"));
        assert!(
            !a_contents.contains(&"B's private question"),
            "participant_a must never see B's private question, got: {a_contents:?}"
        );
        assert!(
            !a_contents.contains(&"DM to B"),
            "participant_a must never see a DM addressed to B, got: {a_contents:?}"
        );
        let a_masked_row = res_a.body["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["content"] == "Public Q from admin needing reply")
            .expect("participant_a must see the public question itself");
        assert_eq!(
            a_masked_row["reply_content"],
            serde_json::Value::Null,
            "non-participant on a private reply must have reply_content redacted, not leaked"
        );
        assert_eq!(a_masked_row["reply_author_id"], serde_json::Value::Null);
        assert_eq!(a_masked_row["reply_author_name"], serde_json::Value::Null);
        assert_eq!(a_masked_row["replied_at"], serde_json::Value::Null);
        // Redact only blanks the reply_* fields - the question content itself,
        // which is what makes this row reachable at all, must survive intact.
        assert_eq!(a_masked_row["content"], "Public Q from admin needing reply");
        assert_eq!(a_masked_row["is_public"], true);

        // --- participant_b: own question + announcement + the DM addressed
        // to them + the same masked-reply row. Never A's private question. ---
        let res_b = f
            .app
            .get_with_token(
                &routes::contest_clarifications(f.contest_id),
                &f.participant_b,
            )
            .await;
        assert_eq!(res_b.status, 200);
        let b_contents = contents(&res_b.body["data"]);
        assert_eq!(
            b_contents.len(),
            4,
            "participant_b row count mismatch, got: {b_contents:?}"
        );
        assert!(b_contents.contains(&"B's private question"));
        assert!(b_contents.contains(&"Public announcement to all"));
        assert!(b_contents.contains(&"DM to B"));
        assert!(b_contents.contains(&"Public Q from admin needing reply"));
        assert!(
            !b_contents.contains(&"A's private question"),
            "participant_b must never see A's private question, got: {b_contents:?}"
        );
        let b_masked_row = res_b.body["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["content"] == "Public Q from admin needing reply")
            .expect("participant_b must see the public question itself");
        assert_eq!(b_masked_row["reply_content"], serde_json::Value::Null);

        // Sanity check on the fixture itself: if this ever tripped, qa/qb
        // wouldn't be distinct clarification ids and the whole test would be
        // meaningless.
        assert_ne!(qa, qb);
    }
}
