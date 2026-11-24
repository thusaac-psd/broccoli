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
        app.register_for_contest(cid, &u1).await;

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
        app.register_for_contest(cid, &u1).await;

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
        app.register_for_contest(cid, &u1).await;

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

mod clarification_admission {
    use super::*;

    async fn user_id(app: &TestApp, token: &str) -> i32 {
        app.get_with_token(routes::ME, token).await.id()
    }

    fn question(content: &str) -> serde_json::Value {
        json!({ "content": content, "clarification_type": "question" })
    }

    /// Seeing a public contest is not enough to write into its
    /// clarification channel: a question comes from a participant.
    #[tokio::test]
    async fn a_stranger_to_a_public_contest_cannot_ask() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let stranger = app
            .create_user_with_role("stranger", "pass1234", "contestant")
            .await;
        let cid = app.create_contest(&admin, "C1", true, false).await;

        let res = app
            .post_with_token(
                &routes::contest_clarifications(cid),
                &question("Hello?"),
                &stranger,
            )
            .await;
        assert_eq!(res.status, 403, "{}", res.text);
        assert_eq!(res.body["code"], "CONTEST_NOT_REGISTERED");
    }

    #[tokio::test]
    async fn a_participant_asks_at_most_ten_questions_a_minute() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let cid = app.create_contest(&admin, "C1", true, false).await;
        app.register_for_contest(cid, &user).await;

        for i in 0..10 {
            let res = app
                .post_with_token(
                    &routes::contest_clarifications(cid),
                    &question(&format!("Question {i}")),
                    &user,
                )
                .await;
            assert_eq!(res.status, 201, "question {i}: {}", res.text);
        }
        let res = app
            .post_with_token(
                &routes::contest_clarifications(cid),
                &question("One more"),
                &user,
            )
            .await;
        assert_eq!(res.status, 429, "{}", res.text);
        assert_eq!(res.body["code"], "RATE_LIMITED");
    }

    /// Replies on a participant's own thread share the per-user window with
    /// their questions; staff replies are not counted.
    #[tokio::test]
    async fn a_participants_replies_share_the_question_quota() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let cid = app.create_contest(&admin, "C1", true, false).await;
        app.register_for_contest(cid, &user).await;

        let clar_id = app
            .post_with_token(
                &routes::contest_clarifications(cid),
                &question("Question"),
                &user,
            )
            .await
            .id();
        let reply = json!({ "content": "More detail", "is_public": false });
        for i in 0..9 {
            let res = app
                .post_with_token(
                    &routes::contest_clarification_reply(cid, clar_id),
                    &reply,
                    &user,
                )
                .await;
            assert_eq!(res.status, 200, "reply {i}: {}", res.text);
        }
        let res = app
            .post_with_token(
                &routes::contest_clarification_reply(cid, clar_id),
                &reply,
                &user,
            )
            .await;
        assert_eq!(res.status, 429, "{}", res.text);
        assert_eq!(res.body["code"], "RATE_LIMITED");

        for i in 0..11 {
            let res = app
                .post_with_token(
                    &routes::contest_clarification_reply(cid, clar_id),
                    &reply,
                    &admin,
                )
                .await;
            assert_eq!(res.status, 200, "staff reply {i}: {}", res.text);
        }
    }

    /// A direct message goes to an active participant of the contest, never
    /// to a deleted account or a user outside it.
    #[tokio::test]
    async fn a_direct_message_names_an_active_participant() {
        use sea_orm::ConnectionTrait;
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let participant = app
            .create_user_with_role("participant", "pass1234", "contestant")
            .await;
        let outsider = app
            .create_user_with_role("outsider", "pass1234", "contestant")
            .await;
        let deleted = app
            .create_user_with_role("deleted", "pass1234", "contestant")
            .await;
        let cid = app.create_contest(&admin, "C1", true, false).await;
        app.register_for_contest(cid, &participant).await;
        app.register_for_contest(cid, &deleted).await;
        let deleted_id = user_id(&app, &deleted).await;
        app.db
            .execute_unprepared(&format!(
                "UPDATE \"user\" SET deleted_at = NOW() WHERE id = {deleted_id}"
            ))
            .await
            .unwrap();

        let dm = |recipient_id: i32| {
            json!({
                "content": "A note for you",
                "clarification_type": "direct_message",
                "recipient_id": recipient_id,
            })
        };
        for recipient in [user_id(&app, &outsider).await, deleted_id] {
            let res = app
                .post_with_token(&routes::contest_clarifications(cid), &dm(recipient), &admin)
                .await;
            assert_eq!(res.status, 404, "recipient {recipient}: {}", res.text);
            assert_eq!(res.body["code"], "NOT_FOUND");
        }
        let res = app
            .post_with_token(
                &routes::contest_clarifications(cid),
                &dm(user_id(&app, &participant).await),
                &admin,
            )
            .await;
        assert_eq!(res.status, 201, "{}", res.text);
        assert_eq!(res.body["recipient_name"], "participant");
    }
}

mod clarification_write_results {
    use super::*;

    async fn seed_kv(app: &TestApp, key: &str, value: &str) {
        let route = routes::plugin_proxy("server-plugin", &format!("kv/{key}"));
        let res = app
            .post_without_token(&route, &json!({ "value": value }))
            .await;
        assert_eq!(res.status, 200, "seeding KV `{key}` failed: {}", res.text);
    }

    /// What a write returns is the clarification as the kernel shows it to
    /// the caller: a plugin's field mask on it holds on the reply and resolve
    /// results exactly as on the list.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_plugin_field_mask_holds_on_every_write_result() {
        let app = TestApp::spawn_with_plugins().await;
        let admin = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let user = app
            .create_user_with_role("user1", "pass1234", "contestant")
            .await;
        let cid = app.create_contest(&admin, "C1", true, false).await;
        app.register_for_contest(cid, &user).await;
        let res = app
            .post_with_token(
                &routes::contest_clarifications(cid),
                &json!({ "content": "Is N <= 1000?", "clarification_type": "question" }),
                &user,
            )
            .await;
        assert_eq!(res.status, 201, "{}", res.text);
        let clar_id = res.id();

        seed_kv(
            &app,
            "redact_resource_keys",
            &format!("clarification:{clar_id}"),
        )
        .await;
        seed_kv(&app, "redact_resource_fields", "replies").await;

        let listed = app
            .get_with_token(&routes::contest_clarifications(cid), &user)
            .await;
        assert_eq!(listed.status, 200, "{}", listed.text);
        assert_eq!(listed.body["data"][0]["content"], "Is N <= 1000?");

        let replied = app
            .post_with_token(
                &routes::contest_clarification_reply(cid, clar_id),
                &json!({ "content": "More detail", "is_public": false }),
                &user,
            )
            .await;
        assert_eq!(replied.status, 200, "{}", replied.text);
        assert_eq!(replied.body["id"], clar_id);
        assert_eq!(
            replied.body["replies"],
            serde_json::json!([]),
            "{}",
            replied.text
        );

        let resolved = app
            .post_with_token(
                &routes::contest_clarification_resolve(cid, clar_id),
                &json!({ "resolved": true }),
                &user,
            )
            .await;
        assert_eq!(resolved.status, 200, "{}", resolved.text);
        assert_eq!(resolved.body["resolved"], true);
        assert_eq!(
            resolved.body["replies"],
            serde_json::json!([]),
            "{}",
            resolved.text
        );
    }
}
