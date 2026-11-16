use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use common::{SubmissionStatus, Verdict};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter, Set,
};
use serde_json::{Value, json};
use server::entity::{submission, user};
use tokio::sync::Barrier;

use crate::common::E2eTestApp;

async fn seed_counted_submission(
    app: &E2eTestApp,
    username: &str,
    problem_id: i32,
    contest_id: i32,
) -> i32 {
    let user_model = user::Entity::find()
        .filter(user::Column::Username.eq(username))
        .one(&app.db)
        .await
        .expect("query user")
        .expect("user should exist");
    let now = Utc::now();
    submission::ActiveModel {
        files: Set(json!([{ "filename": "main.cpp", "content": "int main() { return 0; }" }])),
        language: Set("cpp".into()),
        user_id: Set(user_model.id),
        problem_id: Set(problem_id),
        contest_id: Set(Some(contest_id)),
        contest_type: Set("icpc".into()),
        status: Set(SubmissionStatus::Judged),
        verdict: Set(Some(Verdict::Accepted)),
        score: Set(Some(0.0)),
        judge_epoch: Set(1),
        max_judge_epoch: Set(1),
        created_at: Set(now),
        judged_at: Set(Some(now)),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert counted submission")
    .id
}

#[tokio::test(flavor = "multi_thread")]
async fn limit_rejects_after_max_submissions() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("sl_admin1", "password", "admin")
        .await;
    let contestant = app.create_authenticated_user("sl_user1", "password").await;

    let problem_id = app.create_problem(&admin, "Limit Problem 1").await;

    let contest_id = app
        .create_typed_contest(&admin, "Limit Contest 1", "icpc", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    app.register_for_contest(contest_id, &contestant).await;

    let config_path = format!(
        "/api/v1/contests/{contest_id}/problems/{problem_id}/config/submission-limit/limits"
    );
    app.put_with_token(
        &config_path,
        &json!({ "config": { "max_submissions": 2 }, "enabled": true }),
        &admin,
    )
    .await;

    // Back-to-back real POSTs inside the grace window: each committed attempt
    // is counted once, through its submission rather than also its reservation.
    let sub_path = format!("/api/v1/contests/{contest_id}/problems/{problem_id}/submissions");
    let submit_body = |n: i32| {
        json!({
            "files": [{"filename": "main.cpp", "content": format!("int main() {{ return {n}; }}")}],
            "language": "cpp",
        })
    };

    let sub1 = app
        .post_with_token(&sub_path, &submit_body(0), &contestant)
        .await;
    assert_eq!(
        sub1.status, 201,
        "First submission should be accepted: {}",
        sub1.text
    );
    let sub2 = app
        .post_with_token(&sub_path, &submit_body(1), &contestant)
        .await;
    assert_eq!(
        sub2.status, 201,
        "Second submission should be accepted: {}",
        sub2.text
    );

    let sub3_res = app
        .post_with_token(&sub_path, &submit_body(2), &contestant)
        .await;
    assert_eq!(
        sub3_res.status, 429,
        "Third submission should be rejected: {}",
        sub3_res.text
    );
    assert_eq!(
        sub3_res.body["code"].as_str().unwrap_or(""),
        "SUBMISSION_LIMIT_EXCEEDED",
        "Error code should be SUBMISSION_LIMIT_EXCEEDED"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn limit_zero_means_unlimited() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("sl_admin2", "password", "admin")
        .await;
    let contestant = app.create_authenticated_user("sl_user2", "password").await;

    let problem_id = app.create_problem(&admin, "Limit Problem 2").await;

    let contest_id = app
        .create_typed_contest(&admin, "Limit Contest 2", "icpc", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    app.register_for_contest(contest_id, &contestant).await;

    let config_path = format!(
        "/api/v1/contests/{contest_id}/problems/{problem_id}/config/submission-limit/limits"
    );
    app.put_with_token(
        &config_path,
        &json!({ "config": { "max_submissions": 0 }, "enabled": true }),
        &admin,
    )
    .await;

    // max = 0 is unlimited: every real submission must be accepted, never 429.
    let sub_path = format!("/api/v1/contests/{contest_id}/problems/{problem_id}/submissions");
    for n in 0..3 {
        let res = app
            .post_with_token(
                &sub_path,
                &json!({
                    "files": [{"filename": "main.cpp", "content": format!("int main() {{ return {n}; }}")}],
                    "language": "cpp",
                }),
                &contestant,
            )
            .await;
        assert_eq!(
            res.status, 201,
            "Submission {n} must be accepted while unlimited: {}",
            res.text
        );
    }

    let status_path = format!(
        "/api/v1/p/submission-limit/api/plugins/submission-limit/contests/{contest_id}/problems/{problem_id}/status"
    );
    let res = app.get_with_token(&status_path, &contestant).await;
    assert_eq!(
        res.status, 200,
        "Limit status endpoint failed: {}",
        res.text
    );
    assert_eq!(res.body["unlimited"].as_bool(), Some(true));
    assert_eq!(res.body["remaining"], serde_json::Value::Null);
    // `submissions_made` counts the user's committed submissions whether or not
    // a cap applies.
    assert_eq!(res.body["submissions_made"].as_u64(), Some(3));
}

#[tokio::test(flavor = "multi_thread")]
async fn limit_status_endpoint_shows_remaining() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("sl_admin3", "password", "admin")
        .await;
    let contestant = app.create_authenticated_user("sl_user3", "password").await;

    let problem_id = app.create_problem(&admin, "Limit Problem 3").await;

    let contest_id = app
        .create_typed_contest(&admin, "Limit Contest 3", "icpc", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    app.register_for_contest(contest_id, &contestant).await;

    let config_path = format!(
        "/api/v1/contests/{contest_id}/problems/{problem_id}/config/submission-limit/limits"
    );
    app.put_with_token(
        &config_path,
        &json!({ "config": { "max_submissions": 5 }, "enabled": true }),
        &admin,
    )
    .await;

    // One real submission takes exactly one claim, so the status endpoint,
    // which reads the same claim counter the gate enforces, reports it.
    let sub_path = format!("/api/v1/contests/{contest_id}/problems/{problem_id}/submissions");
    let sub = app
        .post_with_token(
            &sub_path,
            &json!({
                "files": [{"filename": "main.cpp", "content": "int main() { return 0; }"}],
                "language": "cpp",
            }),
            &contestant,
        )
        .await;
    assert_eq!(
        sub.status, 201,
        "Submission should be accepted: {}",
        sub.text
    );

    let status_path = format!(
        "/api/v1/p/submission-limit/api/plugins/submission-limit/contests/{contest_id}/problems/{problem_id}/status"
    );
    let res = app.get_with_token(&status_path, &contestant).await;
    assert_eq!(
        res.status, 200,
        "Limit status endpoint failed: {}",
        res.text
    );
    assert_eq!(res.body["enabled"].as_bool(), Some(true));
    assert_eq!(res.body["max_submissions"].as_u64(), Some(5));
    assert_eq!(res.body["submissions_made"].as_u64(), Some(1));
    assert_eq!(res.body["remaining"].as_u64(), Some(4));
    assert_eq!(res.body["unlimited"].as_bool(), Some(false));
}

#[tokio::test(flavor = "multi_thread")]
async fn different_problems_have_independent_limits() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("sl_admin4", "password", "admin")
        .await;
    let contestant = app.create_authenticated_user("sl_user4", "password").await;

    let prob_a = app.create_problem(&admin, "Limit ProbA 4").await;
    let prob_b = app.create_problem(&admin, "Limit ProbB 4").await;

    let contest_id = app
        .create_typed_contest(&admin, "Limit Contest 4", "icpc", true, true)
        .await;
    app.add_problem_to_contest_with_label(contest_id, prob_a, "A", &admin)
        .await;
    app.add_problem_to_contest_with_label(contest_id, prob_b, "B", &admin)
        .await;
    app.register_for_contest(contest_id, &contestant).await;

    // Both problems capped at 1; their claim counters are independent.
    for prob in [prob_a, prob_b] {
        let config =
            format!("/api/v1/contests/{contest_id}/problems/{prob}/config/submission-limit/limits");
        app.put_with_token(
            &config,
            &json!({ "config": { "max_submissions": 1 }, "enabled": true }),
            &admin,
        )
        .await;
    }

    let submit = |prob: i32, n: i32| {
        (
            format!("/api/v1/contests/{contest_id}/problems/{prob}/submissions"),
            json!({
                "files": [{"filename": "main.cpp", "content": format!("int main() {{ return {n}; }}")}],
                "language": "cpp",
            }),
        )
    };

    // Problem A: the first real POST fills its cap; the second is rejected.
    let (a1_path, a1_body) = submit(prob_a, 0);
    let a1 = app.post_with_token(&a1_path, &a1_body, &contestant).await;
    assert_eq!(
        a1.status, 201,
        "Problem A first submission should be accepted: {}",
        a1.text
    );

    let (a2_path, a2_body) = submit(prob_a, 1);
    let sub_a2 = app.post_with_token(&a2_path, &a2_body, &contestant).await;
    assert_eq!(
        sub_a2.status, 429,
        "Problem A second submission should be rejected: {}",
        sub_a2.text
    );
    assert_eq!(
        sub_a2.body["code"].as_str().unwrap_or(""),
        "SUBMISSION_LIMIT_EXCEEDED"
    );

    // Problem B has its own counter, so a real POST there is still accepted
    // even though problem A is already at its cap.
    let (b1_path, b1_body) = submit(prob_b, 0);
    let b1 = app.post_with_token(&b1_path, &b1_body, &contestant).await;
    assert_eq!(
        b1.status, 201,
        "Problem B submission should be accepted (independent limit): {}",
        b1.text
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn different_users_have_independent_limits() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("sl_admin5", "password", "admin")
        .await;
    let user_a = app.create_authenticated_user("sl_userA5", "password").await;
    let user_b = app.create_authenticated_user("sl_userB5", "password").await;

    let problem_id = app.create_problem(&admin, "Limit Problem 5").await;

    let contest_id = app
        .create_typed_contest(&admin, "Limit Contest 5", "icpc", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    app.register_for_contest(contest_id, &user_a).await;
    app.register_for_contest(contest_id, &user_b).await;

    let config_path = format!(
        "/api/v1/contests/{contest_id}/problems/{problem_id}/config/submission-limit/limits"
    );
    app.put_with_token(
        &config_path,
        &json!({ "config": { "max_submissions": 1 }, "enabled": true }),
        &admin,
    )
    .await;

    seed_counted_submission(&app, "sl_userA5", problem_id, contest_id).await;

    let status_path = format!(
        "/api/v1/p/submission-limit/api/plugins/submission-limit/contests/{contest_id}/problems/{problem_id}/status"
    );
    let res = app.get_with_token(&status_path, &user_b).await;
    assert_eq!(res.status, 200, "User B status failed: {}", res.text);
    assert_eq!(res.body["submissions_made"].as_u64(), Some(0));
    assert_eq!(res.body["remaining"].as_u64(), Some(1));
}

/// With `max_submissions = 1`, N simultaneous submissions for the same (user,
/// problem, contest) admit exactly one. The server inserts the submission row
/// only after the hook returns, so a count-then-pass check would let every
/// concurrent POST through; the single-statement claim serializes them on the
/// reservation row and rejects the other N-1 with 429
/// SUBMISSION_LIMIT_EXCEEDED.
///
/// The requests are made genuinely concurrent (not sequential): each runs in its
/// own task with its own HTTP client/connection, and an N-party barrier parks
/// every task until all N have built their request, so the release fires the
/// POSTs together and their `check_limit` hooks overlap in flight.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn limit_rejects_concurrent_submissions_race() {
    const N: usize = 8;
    // The submission handler holds its DB transaction open across the
    // `before_submission` hook, and the e2e harness routes the plugin's claim
    // query through the same pool, so each in-flight submission needs two
    // connections at once. Size the pool for 2 x N (plus slack) so the pool
    // itself never becomes the bottleneck - otherwise concurrent requests fail
    // with a "connection pool timed out" 500 unrelated to the gate.
    let app = E2eTestApp::spawn_with_db_pool(2 * N as u32 + 8).await;

    let admin = app
        .create_user_with_role("sl_admin_race", "password", "admin")
        .await;
    let contestant = app
        .create_authenticated_user("sl_user_race", "password")
        .await;

    let problem_id = app.create_problem(&admin, "Limit Problem Race").await;
    let contest_id = app
        .create_typed_contest(&admin, "Limit Contest Race", "icpc", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    app.register_for_contest(contest_id, &contestant).await;

    // Cap at exactly one accepted submission for this problem.
    let config_path = format!(
        "/api/v1/contests/{contest_id}/problems/{problem_id}/config/submission-limit/limits"
    );
    app.put_with_token(
        &config_path,
        &json!({ "config": { "max_submissions": 1 }, "enabled": true }),
        &admin,
    )
    .await;

    let url = format!(
        "{}/api/v1/contests/{contest_id}/problems/{problem_id}/submissions",
        app.base_url
    );

    let barrier = Arc::new(Barrier::new(N));
    let start = Instant::now();
    let mut handles = Vec::with_capacity(N);
    for i in 0..N {
        let url = url.clone();
        let token = contestant.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            // Independent client per task => independent TCP connection, so the
            // POSTs race in parallel instead of multiplexing on one pooled
            // connection.
            let client = reqwest::Client::builder()
                .no_proxy()
                .build()
                .expect("build client");
            let body = json!({
                "files": [{"filename": "main.cpp", "content": format!("int main() {{ return {i}; }}")}],
                "language": "cpp",
            });
            let req = client
                .post(&url)
                .header("Authorization", format!("Bearer {token}"))
                .json(&body);
            // Everyone parks here; the last arrival releases all N at once.
            barrier.wait().await;
            let fired_us = start.elapsed().as_micros();
            let res = req.send().await.expect("send submission");
            let status = res.status().as_u16();
            let text = res.text().await.unwrap_or_default();
            let code = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v["code"].as_str().map(str::to_string));
            (status, code, text, fired_us)
        }));
    }

    let mut accepted = 0usize;
    let mut rejected = 0usize;
    let mut other = Vec::new();
    let mut fired = Vec::with_capacity(N);
    for h in handles {
        let (status, code, text, fired_us) = h.await.expect("join submission task");
        fired.push(fired_us);
        match status {
            201 => accepted += 1,
            429 => {
                assert_eq!(
                    code.as_deref(),
                    Some("SUBMISSION_LIMIT_EXCEEDED"),
                    "429 must carry SUBMISSION_LIMIT_EXCEEDED, got: {text}"
                );
                rejected += 1;
            }
            _ => other.push((status, text)),
        }
    }

    // Evidence that the POSTs really were in flight together, not serialized.
    let spread_us = fired.iter().max().unwrap() - fired.iter().min().unwrap();
    eprintln!(
        "[race] N={N} accepted={accepted} rejected429={rejected} other={} send-window={spread_us}us",
        other.len()
    );

    assert!(
        other.is_empty(),
        "every response must be 201 or 429; got unexpected: {other:?}"
    );
    assert_eq!(
        accepted, 1,
        "the atomic gate must admit EXACTLY ONE submission (accepted={accepted}, rejected={rejected})"
    );
    assert_eq!(
        rejected,
        N - 1,
        "the other {} concurrent submissions must be rejected 429 (rejected={rejected})",
        N - 1
    );

    // Independent confirmation from the database: exactly one submission row was
    // actually persisted for this (user, problem, contest).
    let contestant_id = user::Entity::find()
        .filter(user::Column::Username.eq("sl_user_race"))
        .one(&app.db)
        .await
        .expect("query user")
        .expect("user should exist")
        .id;
    let persisted = submission::Entity::find()
        .filter(submission::Column::UserId.eq(contestant_id))
        .filter(submission::Column::ProblemId.eq(problem_id))
        .filter(submission::Column::ContestId.eq(contest_id))
        .count(&app.db)
        .await
        .expect("count submissions");
    assert_eq!(
        persisted, 1,
        "exactly one submission row must be persisted, found {persisted}"
    );
}

async fn user_id_of(app: &E2eTestApp, username: &str) -> i32 {
    user::Entity::find()
        .filter(user::Column::Username.eq(username))
        .one(&app.db)
        .await
        .expect("query user")
        .expect("user should exist")
        .id
}
/// A contest problem with `max_submissions = max` for one registered
/// contestant. Returns (contestant token, user id, problem id, contest id).
async fn limited_problem(app: &E2eTestApp, tag: &str, max: u32) -> (String, i32, i32, i32) {
    let admin = app
        .create_user_with_role(&format!("sl_admin_{tag}"), "password", "admin")
        .await;
    let username = format!("sl_user_{tag}");
    let contestant = app.create_authenticated_user(&username, "password").await;
    let problem_id = app.create_problem(&admin, &format!("{tag} Problem")).await;
    let contest_id = app
        .create_typed_contest(&admin, &format!("{tag} Contest"), "icpc", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    app.register_for_contest(contest_id, &contestant).await;
    let config_path = format!(
        "/api/v1/contests/{contest_id}/problems/{problem_id}/config/submission-limit/limits"
    );
    app.put_with_token(
        &config_path,
        &json!({ "config": { "max_submissions": max }, "enabled": true }),
        &admin,
    )
    .await;
    let uid = user_id_of(app, &username).await;
    (contestant, uid, problem_id, contest_id)
}

/// Store one reservation for `attempt_id` that expires `expires_in_secs` from
/// now (negative: already expired).
/// The table lives in the plugin's own schema.
async fn seed_reservation(
    app: &E2eTestApp,
    (uid, problem_id, contest_id): (i32, i32, i32),
    attempt_id: &str,
    expires_in_secs: i64,
) {
    app.db
        .execute_unprepared(&format!(
            "INSERT INTO \"plugin_submission-limit\".submission_limit_claim (user_id, problem_id, contest_id, reservations) \
             VALUES ({uid}, {problem_id}, {contest_id}, \
               jsonb_build_object('{attempt_id}', EXTRACT(EPOCH FROM NOW()) + {expires_in_secs}))"
        ))
        .await
        .expect("seed reservation");
}

async fn submit(app: &E2eTestApp, token: &str, problem_id: i32, contest_id: i32) -> u16 {
    let sub_path = format!("/api/v1/contests/{contest_id}/problems/{problem_id}/submissions");
    let body = json!({
        "files": [{"filename": "main.cpp", "content": "int main() { return 0; }"}],
        "language": "cpp",
    });
    app.post_with_token(&sub_path, &body, token).await.status
}

/// A submission that reserved a slot but was then rejected by a later hook (or
/// failed to insert) leaves a reservation no submission carries. Once the
/// attempt timeout has passed it frees its slot.
#[tokio::test(flavor = "multi_thread")]
async fn a_reservation_without_a_submission_frees_its_slot_once_it_expires() {
    let app = E2eTestApp::spawn().await;
    let (token, uid, problem_id, contest_id) = limited_problem(&app, "expired", 1).await;
    let attempt = "00000000-0000-4000-8000-000000000001";
    seed_reservation(&app, (uid, problem_id, contest_id), attempt, -1).await;
    assert_eq!(submit(&app, &token, problem_id, contest_id).await, 201);
}

/// An unexpired reservation holds its slot while its submission has not
/// committed, however long the create takes.
#[tokio::test(flavor = "multi_thread")]
async fn a_live_reservation_holds_its_slot() {
    let app = E2eTestApp::spawn().await;
    let (token, uid, problem_id, contest_id) = limited_problem(&app, "live", 1).await;
    let attempt = "00000000-0000-4000-8000-000000000002";
    seed_reservation(&app, (uid, problem_id, contest_id), attempt, 60).await;
    assert_eq!(submit(&app, &token, problem_id, contest_id).await, 429);
}

/// Committed submissions count from the submission table, so a submission is
/// counted even when no reservation of it survives.
#[tokio::test(flavor = "multi_thread")]
async fn a_committed_submission_consumes_a_slot() {
    let app = E2eTestApp::spawn().await;
    let (token, _uid, problem_id, contest_id) = limited_problem(&app, "committed", 1).await;
    seed_counted_submission(&app, "sl_user_committed", problem_id, contest_id).await;
    assert_eq!(submit(&app, &token, problem_id, contest_id).await, 429);
}
