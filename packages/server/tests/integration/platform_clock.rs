//! Handlers keep wall time by the database clock: moving the test database
//! clock moves contest phases, token expiry and the `Date` header on every
//! replica at once.

use std::time::Duration;

use serde_json::json;

use crate::common::{SpawnOptions, TestApp, routes};

fn rfc3339(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .expect("valid instant")
        .to_rfc3339()
}

#[tokio::test]
async fn a_contest_starts_on_every_replica_when_the_database_clock_reaches_its_start() {
    let a = TestApp::spawn().await;
    let b = a.spawn_replica(SpawnOptions::default()).await;
    let admin = a.create_user_with_role("admin1", "pass1234", "admin").await;
    let user = a
        .create_user_with_role("user1", "pass1234", "contestant")
        .await;

    let start_ms = a.clock().now_ms().await + 3_600_000;
    let contest = a
        .post_with_token(
            routes::CONTESTS,
            &json!({
                "title": "Later",
                "description": "Starts in an hour.",
                "activate_time": "2020-01-01T00:00:00Z",
                "start_time": rfc3339(start_ms),
                "end_time": rfc3339(start_ms + 3_600_000),
                "is_public": true,
                "contest_type": "standard",
            }),
            &admin,
        )
        .await;
    assert_eq!(contest.status, 201, "{}", contest.text);
    let contest_id = contest.id();
    let problem_id = a.create_problem(&admin, "P1").await;
    let added = a
        .post_with_token(
            &routes::contest_problems(contest_id),
            &json!({"problem_id": problem_id, "label": "A"}),
            &admin,
        )
        .await;
    assert_eq!(added.status, 201, "{}", added.text);
    let joined = a
        .post_with_token(&routes::contest_register(contest_id), &json!({}), &user)
        .await;
    assert!(joined.status < 300, "{}", joined.text);

    let problem = routes::contest_problem(contest_id, problem_id);
    for replica in [&a, &b] {
        let res = replica.get_with_token(&problem, &user).await;
        assert_ne!(res.status, 200, "visible before the start: {}", res.text);
    }

    a.clock().advance(Duration::from_secs(3_601)).await;
    // The hour also expired the access token.
    let login = a
        .post_without_token(
            routes::LOGIN,
            &json!({"username": "user1", "password": "pass1234"}),
        )
        .await;
    assert_eq!(login.status, 200, "{}", login.text);
    let user = login.body["token"].as_str().expect("a token").to_owned();

    for replica in [&a, &b] {
        let res = replica.get_with_token(&problem, &user).await;
        assert_eq!(res.status, 200, "hidden after the start: {}", res.text);
    }
}

#[tokio::test]
async fn an_access_token_expires_when_the_database_clock_passes_its_expiry() {
    let app = TestApp::spawn().await;
    let token = app.create_authenticated_user("alice", "password123").await;
    assert_eq!(app.get_with_token(routes::ME, &token).await.status, 200);

    app.clock().advance(Duration::from_secs(4 * 60)).await;
    assert_eq!(app.get_with_token(routes::ME, &token).await.status, 200);

    app.clock().advance(Duration::from_secs(61)).await;
    let res = app.get_with_token(routes::ME, &token).await;
    assert_eq!(res.status, 401, "{}", res.text);
}

#[tokio::test]
async fn the_date_header_is_the_database_clock() {
    let app = TestApp::spawn().await;
    app.clock().advance(Duration::from_secs(86_400)).await;

    let db_ms = app.clock().now_ms().await;
    let res = app.get_without_token("/livez").await;
    let date = res
        .headers
        .get(reqwest::header::DATE)
        .and_then(|v| v.to_str().ok())
        .expect("a Date header");
    let served_ms = chrono::DateTime::parse_from_rfc2822(date)
        .expect("an HTTP-date")
        .timestamp_millis();
    // Whole-second resolution, read within a moment of each other.
    assert!(
        (served_ms - db_ms).abs() < 2_000,
        "Date {date} is not the database clock ({db_ms} ms)"
    );
}
