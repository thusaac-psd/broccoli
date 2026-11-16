use crate::common::{TestApp, routes};
use serde_json::json;

mod clarification_creation {
    use super::*;

    #[tokio::test]
    async fn contestant_can_create_a_question() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let cid = app.create_contest(&admin, "C1", true, false).await;

        app.register_for_contest(cid, &user).await;

        let body = json!({
            "content": "Is N <= 1000?",
            "clarification_type": "question",
            "is_public": false
        });

        let res = app
            .post_with_token(&routes::contest_clarifications(cid), &body, &user)
            .await;

        assert_eq!(res.status, 201);
        assert_eq!(res.body["content"], "Is N <= 1000?");
        assert_eq!(res.body["author_name"], "user1");
        assert_eq!(res.body["is_public"], false);
    }

    #[tokio::test]
    async fn contestant_cannot_create_announcement_or_dm() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let cid = app.create_contest(&admin, "C1", true, false).await;

        let body_ann = json!({
            "content": "Hack the server",
            "clarification_type": "announcement",
        });
        let res1 = app
            .post_with_token(&routes::contest_clarifications(cid), &body_ann, &user)
            .await;
        assert_eq!(res1.status, 403);

        let body_dm = json!({
            "content": "Psst",
            "clarification_type": "direct_message",
            "recipient_id": 1
        });
        let res2 = app
            .post_with_token(&routes::contest_clarifications(cid), &body_dm, &user)
            .await;
        assert_eq!(res2.status, 403);
    }

    #[tokio::test]
    async fn admin_can_create_announcement() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let cid = app.create_contest(&admin, "C1", true, false).await;

        let body = json!({
            "content": "Contest extended by 10 mins",
            "clarification_type": "announcement"
        });

        let res = app
            .post_with_token(&routes::contest_clarifications(cid), &body, &admin)
            .await;

        assert_eq!(res.status, 201);
        assert_eq!(res.body["is_public"], true);
    }
}

mod clarification_visibility {
    use super::*;

    #[tokio::test]
    async fn enforces_visibility_rules_for_questions_and_dms() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let u1 = app
            .create_user_with_role("u1", "pass1234", "contestant")
            .await;
        let u2 = app
            .create_user_with_role("u2", "pass1234", "contestant")
            .await;
        let cid = app.create_contest(&admin, "C1", true, false).await;

        app.register_for_contest(cid, &u1).await;
        app.register_for_contest(cid, &u2).await;
        let u2_id = app.get_with_token(routes::ME, &u2).await.id();

        app.post_with_token(
            &routes::contest_clarifications(cid),
            &json!({
                "content": "U1 private question",
                "clarification_type": "question"
            }),
            &u1,
        )
        .await;

        app.post_with_token(
            &routes::contest_clarifications(cid),
            &json!({
                "content": "DM to U2",
                "clarification_type": "direct_message",
                "recipient_id": u2_id
            }),
            &admin,
        )
        .await;

        app.post_with_token(
            &routes::contest_clarifications(cid),
            &json!({
                "content": "Public Announcement",
                "clarification_type": "announcement"
            }),
            &admin,
        )
        .await;

        let res_admin = app
            .get_with_token(&routes::contest_clarifications(cid), &admin)
            .await;
        assert_eq!(res_admin.body["data"].as_array().unwrap().len(), 3);

        let res_u1 = app
            .get_with_token(&routes::contest_clarifications(cid), &u1)
            .await;
        let data_u1 = res_u1.body["data"].as_array().unwrap();
        assert_eq!(data_u1.len(), 2);
        assert!(
            data_u1
                .iter()
                .any(|c| c["content"] == "U1 private question")
        );
        assert!(
            data_u1
                .iter()
                .any(|c| c["content"] == "Public Announcement")
        );
        assert!(!data_u1.iter().any(|c| c["content"] == "DM to U2"));

        let res_u2 = app
            .get_with_token(&routes::contest_clarifications(cid), &u2)
            .await;
        let data_u2 = res_u2.body["data"].as_array().unwrap();
        assert_eq!(data_u2.len(), 2);
        assert!(data_u2.iter().any(|c| c["content"] == "DM to U2"));
        assert!(
            !data_u2
                .iter()
                .any(|c| c["content"] == "U1 private question")
        );
    }

    /// A private reply keeps a private question hidden from other contestants.
    /// Publishing the reply publishes the thread: a reply is only ever shown
    /// under its question, so a public reply under a private question would be
    /// visible to nobody new.
    #[tokio::test]
    async fn publishing_a_reply_publishes_its_question() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let u1 = app
            .create_user_with_role("u1", "pass1234", "contestant")
            .await;
        let u2 = app
            .create_user_with_role("u2", "pass1234", "contestant")
            .await;
        let cid = app.create_contest(&admin, "C1", true, false).await;

        let q_res = app
            .post_with_token(
                &routes::contest_clarifications(cid),
                &json!({
                    "content": "Is the input sorted?",
                    "clarification_type": "question"
                }),
                &u1,
            )
            .await;
        let clar_id = q_res.id();

        let rep_res = app
            .post_with_token(
                &routes::contest_clarification_reply(cid, clar_id),
                &json!({
                    "content": "Yes.",
                    "is_public": false
                }),
                &admin,
            )
            .await;
        assert_eq!(rep_res.status, 200);
        let reply_id = rep_res.body["replies"][0]["id"].as_i64().unwrap() as i32;

        let before = app
            .get_with_token(&routes::contest_clarifications(cid), &u2)
            .await;
        assert_eq!(
            before.body["data"].as_array().unwrap().len(),
            0,
            "a private reply must not reveal a private question"
        );

        let toggled = app
            .post_with_token(
                &routes::contest_clarification_toggle(cid, clar_id, reply_id),
                &json!({}),
                &admin,
            )
            .await;
        assert_eq!(toggled.status, 200);
        assert_eq!(toggled.body["is_public"], true);

        let after = app
            .get_with_token(&routes::contest_clarifications(cid), &u2)
            .await;
        let data = after.body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1, "the published thread is visible to U2");
        assert_eq!(data[0]["content"], "Is the input sorted?");
        assert_eq!(data[0]["is_public"], true);
        assert_eq!(data[0]["replies"][0]["content"], "Yes.");

        // Making the reply private again hides it from U2 but leaves the
        // question public: it may carry other public replies.
        app.post_with_token(
            &routes::contest_clarification_toggle(cid, clar_id, reply_id),
            &json!({}),
            &admin,
        )
        .await;
        let reverted = app
            .get_with_token(&routes::contest_clarifications(cid), &u2)
            .await;
        let data = reverted.body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["replies"].as_array().unwrap().len(), 0);
    }
}

mod clarification_actions {
    use super::*;

    #[tokio::test]
    async fn author_can_resolve_and_reopen_thread() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let u1 = app
            .create_user_with_role("u1", "pass1234", "contestant")
            .await;
        let cid = app.create_contest(&admin, "C1", true, false).await;

        let q_res = app
            .post_with_token(
                &routes::contest_clarifications(cid),
                &json!({
                    "content": "Help?",
                    "clarification_type": "question"
                }),
                &u1,
            )
            .await;
        let clar_id = q_res.id();

        let res = app
            .post_with_token(
                &routes::contest_clarification_resolve(cid, clar_id),
                &json!({"resolved": true}),
                &u1,
            )
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["resolved"], true);
        assert_eq!(res.body["resolved_by_name"], "u1");

        let res = app
            .post_with_token(
                &routes::contest_clarification_resolve(cid, clar_id),
                &json!({"resolved": false}),
                &u1,
            )
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["resolved"], false);
        assert_eq!(res.body["resolved_by_name"], json!(null));
    }

    #[tokio::test]
    async fn non_author_cannot_reply_or_resolve() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let u1 = app
            .create_user_with_role("u1", "pass1234", "contestant")
            .await;
        let u2 = app
            .create_user_with_role("u2", "pass1234", "contestant")
            .await;
        let cid = app.create_contest(&admin, "C1", true, false).await;

        let q_res = app
            .post_with_token(
                &routes::contest_clarifications(cid),
                &json!({
                    "content": "Help?",
                    "clarification_type": "question"
                }),
                &u1,
            )
            .await;
        let clar_id = q_res.id();

        let rep_res = app
            .post_with_token(
                &routes::contest_clarification_reply(cid, clar_id),
                &json!({
                    "content": "I know!",
                    "is_public": false
                }),
                &u2,
            )
            .await;
        assert_eq!(rep_res.status, 403);

        let res_res = app
            .post_with_token(
                &routes::contest_clarification_resolve(cid, clar_id),
                &json!({"resolved": true}),
                &u2,
            )
            .await;
        assert_eq!(res_res.status, 403);
    }

    /// A stranger to a *private* contest (never registered, no `contest:manage`)
    /// must get an identical 404 for `reply`/`resolve` whether the target
    /// clarification id exists or not. `reply_clarification`/
    /// `resolve_clarification` check contest reachability before fetching the
    /// row; fetching first would let an existing id fall through to the
    /// admin/author/recipient check and return 403 PermissionDenied while a
    /// missing id returns 404 NotFound - letting the stranger confirm a
    /// clarification exists in a contest they cannot otherwise reach at all.
    #[tokio::test]
    async fn stranger_to_private_contest_cannot_distinguish_existing_from_missing_clarification() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let u1 = app
            .create_user_with_role("u1", "pass1234", "contestant")
            .await;
        let stranger = app
            .create_user_with_role("stranger1", "pass1234", "contestant")
            .await;
        // Private contest: `is_public: false`. `stranger` is never registered
        // and has no `contest:manage` permission, so `check_contest_access_decision`/
        // the kernel's `Resource::Contest` decision denies them outright.
        // `register_for_contest` is self-service and only works on public
        // contests, so `u1` is enrolled via the admin `add_participant`
        // endpoint instead.
        let cid = app.create_contest(&admin, "Private C1", false, false).await;
        let u1_id = app.get_with_token(routes::ME, &u1).await.id();
        let add_res = app
            .post_with_token(
                &routes::contest_participants(cid),
                &json!({ "user_id": u1_id }),
                &admin,
            )
            .await;
        assert_eq!(add_res.status, 201);

        // admin can create in a private contest without being a registered
        // participant (`contest:manage` bypasses the reachability gate).
        let q_res = app
            .post_with_token(
                &routes::contest_clarifications(cid),
                &json!({
                    "content": "Existing question",
                    "clarification_type": "question"
                }),
                &admin,
            )
            .await;
        assert_eq!(q_res.status, 201);
        let existing_clar_id = q_res.id();
        let missing_clar_id = existing_clar_id + 999_000;

        for clar_id in [existing_clar_id, missing_clar_id] {
            let reply_res = app
                .post_with_token(
                    &routes::contest_clarification_reply(cid, clar_id),
                    &json!({
                        "content": "Trying to peek",
                        "is_public": false
                    }),
                    &stranger,
                )
                .await;
            assert_eq!(
                reply_res.status, 404,
                "reply: clarification_id={clar_id} must be indistinguishable from a missing one"
            );
            assert_eq!(reply_res.body["code"], "NOT_FOUND");

            let resolve_res = app
                .post_with_token(
                    &routes::contest_clarification_resolve(cid, clar_id),
                    &json!({"resolved": true}),
                    &stranger,
                )
                .await;
            assert_eq!(
                resolve_res.status, 404,
                "resolve: clarification_id={clar_id} must be indistinguishable from a missing one"
            );
            assert_eq!(resolve_res.body["code"], "NOT_FOUND");
        }
    }
}

mod clarification_reply_publishing {
    use super::*;

    /// An admin replying with `is_public: true` publishes the reply in one call -
    /// no separate toggle-public round-trip.
    #[tokio::test]
    async fn admin_reply_publishes_without_a_separate_toggle() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let u1 = app
            .create_user_with_role("u1", "pass1234", "contestant")
            .await;
        let cid = app.create_contest(&admin, "C1", true, false).await;
        app.register_for_contest(cid, &u1).await;

        let q_res = app
            .post_with_token(
                &routes::contest_clarifications(cid),
                &json!({
                    "content": "Time limit?",
                    "clarification_type": "question"
                }),
                &u1,
            )
            .await;
        let clar_id = q_res.id();

        let rep_res = app
            .post_with_token(
                &routes::contest_clarification_reply(cid, clar_id),
                &json!({
                    "content": "2 seconds.",
                    "is_public": true
                }),
                &admin,
            )
            .await;
        assert_eq!(rep_res.status, 200);
        assert_eq!(
            rep_res.body["replies"][0]["is_public"], true,
            "admin reply with is_public:true must be public at reply time"
        );
        assert_eq!(
            rep_res.body["is_public"], true,
            "a public reply publishes its question"
        );

        let u2 = app
            .create_user_with_role("u2", "pass1234", "contestant")
            .await;
        app.register_for_contest(cid, &u2).await;
        let seen = app
            .get_with_token(&routes::contest_clarifications(cid), &u2)
            .await;
        let data = seen.body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1, "every participant sees the published thread");
        assert_eq!(data[0]["replies"][0]["content"], "2 seconds.");
    }

    /// A non-admin (here the question's own author) may reply, but their
    /// `is_public: true` is forced false - only admins can broadcast a reply to
    /// every participant, the same gate the create path and toggle enforce.
    #[tokio::test]
    async fn non_admin_reply_cannot_publish() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let u1 = app
            .create_user_with_role("u1", "pass1234", "contestant")
            .await;
        let cid = app.create_contest(&admin, "C1", true, false).await;
        app.register_for_contest(cid, &u1).await;

        let q_res = app
            .post_with_token(
                &routes::contest_clarifications(cid),
                &json!({
                    "content": "Can I get a hint?",
                    "clarification_type": "question"
                }),
                &u1,
            )
            .await;
        let clar_id = q_res.id();

        let rep_res = app
            .post_with_token(
                &routes::contest_clarification_reply(cid, clar_id),
                &json!({
                    "content": "Publishing this to everyone!",
                    "is_public": true
                }),
                &u1,
            )
            .await;
        assert_eq!(rep_res.status, 200);
        assert_eq!(
            rep_res.body["replies"][0]["is_public"], false,
            "a non-admin's reply is_public:true must be forced private"
        );
    }
}
