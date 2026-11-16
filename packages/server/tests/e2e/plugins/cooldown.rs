use chrono::Utc;
use common::{SubmissionStatus, Verdict};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use serde_json::json;
use server::entity::{submission, user};

use crate::common::E2eTestApp;

async fn seed_recent_submission(
    app: &E2eTestApp,
    username: &str,
    problem_id: i32,
    contest_id: Option<i32>,
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
        contest_id: Set(contest_id),
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
    .expect("insert recent submission")
    .id
}

#[tokio::test(flavor = "multi_thread")]
async fn cooldown_rejects_rapid_contest_submissions() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("cd_admin1", "password", "admin")
        .await;
    let contestant = app.create_authenticated_user("cd_user1", "password").await;

    let problem_id = app.create_problem(&admin, "Cooldown Problem 1").await;

    let contest_id = app
        .create_typed_contest(&admin, "Cooldown Contest 1", "icpc", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    app.register_for_contest(contest_id, &contestant).await;

    let config_path =
        format!("/api/v1/contests/{contest_id}/problems/{problem_id}/config/cooldown/cooldown");
    let put_res = app
        .put_with_token(
            &config_path,
            &json!({ "config": { "cooldown_seconds": 60 }, "enabled": true }),
            &admin,
        )
        .await;
    assert_eq!(
        put_res.status, 200,
        "Failed to set cooldown config: {}",
        put_res.text
    );

    let sub1 = seed_recent_submission(&app, "cd_user1", problem_id, Some(contest_id)).await;
    assert!(sub1 > 0, "First submission should succeed");

    let sub2_path = format!("/api/v1/contests/{contest_id}/problems/{problem_id}/submissions");
    let sub2_res = app
        .post_with_token(
            &sub2_path,
            &json!({
                "files": [{"filename": "main.cpp", "content": "int main() { return 0; }"}],
                "language": "cpp",
            }),
            &contestant,
        )
        .await;
    assert_eq!(
        sub2_res.status, 429,
        "Second rapid submission should be rejected: {}",
        sub2_res.text
    );
    assert_eq!(
        sub2_res.body["code"].as_str().unwrap_or(""),
        "COOLDOWN_ACTIVE",
        "Error code should be COOLDOWN_ACTIVE"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cooldown_allows_submission_when_disabled() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("cd_admin2", "password", "admin")
        .await;
    let contestant = app.create_authenticated_user("cd_user2", "password").await;

    let problem_id = app.create_problem(&admin, "Cooldown Problem 2").await;

    let contest_id = app
        .create_typed_contest(&admin, "Cooldown Contest 2", "icpc", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    app.register_for_contest(contest_id, &contestant).await;

    let config_path =
        format!("/api/v1/contests/{contest_id}/problems/{problem_id}/config/cooldown/cooldown");
    app.put_with_token(
        &config_path,
        &json!({ "config": { "cooldown_seconds": 0 }, "enabled": true }),
        &admin,
    )
    .await;

    seed_recent_submission(&app, "cd_user2", problem_id, Some(contest_id)).await;
    let status_path = format!(
        "/api/v1/p/cooldown/api/plugins/cooldown/contests/{contest_id}/problems/{problem_id}/status"
    );
    let res = app.get_with_token(&status_path, &contestant).await;
    assert_eq!(
        res.status, 200,
        "Cooldown status endpoint failed: {}",
        res.text
    );
    assert_eq!(res.body["enabled"].as_bool(), Some(true));
    assert_eq!(res.body["cooldown_seconds"].as_u64(), Some(0));
    assert_eq!(res.body["can_submit"].as_bool(), Some(true));
}

#[tokio::test(flavor = "multi_thread")]
async fn cooldown_status_endpoint_returns_data() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("cd_admin3", "password", "admin")
        .await;
    let contestant = app.create_authenticated_user("cd_user3", "password").await;

    let problem_id = app.create_problem(&admin, "Cooldown Problem 3").await;

    let contest_id = app
        .create_typed_contest(&admin, "Cooldown Contest 3", "icpc", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    app.register_for_contest(contest_id, &contestant).await;

    let config_path =
        format!("/api/v1/contests/{contest_id}/problems/{problem_id}/config/cooldown/cooldown");
    app.put_with_token(
        &config_path,
        &json!({ "config": { "cooldown_seconds": 60 }, "enabled": true }),
        &admin,
    )
    .await;

    seed_recent_submission(&app, "cd_user3", problem_id, Some(contest_id)).await;

    let status_path = format!(
        "/api/v1/p/cooldown/api/plugins/cooldown/contests/{contest_id}/problems/{problem_id}/status"
    );
    let res = app.get_with_token(&status_path, &contestant).await;
    assert_eq!(
        res.status, 200,
        "Cooldown status endpoint failed: {}",
        res.text
    );
    assert_eq!(res.body["enabled"].as_bool(), Some(true));
    assert_eq!(res.body["cooldown_seconds"].as_u64(), Some(60));
    assert_eq!(res.body["can_submit"].as_bool(), Some(false));
    assert!(
        res.body["seconds_since_last"].is_number(),
        "seconds_since_last should be a number"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cooldown_config_at_problem_scope() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("cd_admin4", "password", "admin")
        .await;
    let user = app.create_authenticated_user("cd_user4", "password").await;

    // Public: the plain user must get past the problem-visibility gate so the
    // submission reaches the cooldown hook.
    let problem_id = app
        .create_public_problem(&admin, "Cooldown Problem 4")
        .await;

    let config_path = format!("/api/v1/problems/{problem_id}/config/cooldown/cooldown");
    let put_res = app
        .put_with_token(
            &config_path,
            &json!({ "config": { "cooldown_seconds": 60 }, "enabled": true }),
            &admin,
        )
        .await;
    assert_eq!(
        put_res.status, 200,
        "Failed to set problem-scope config: {}",
        put_res.text
    );

    seed_recent_submission(&app, "cd_user4", problem_id, None).await;

    let sub2_path = format!("/api/v1/problems/{problem_id}/submissions");
    let sub2_res = app
        .post_with_token(
            &sub2_path,
            &json!({
                "files": [{"filename": "main.cpp", "content": "int main() { return 0; }"}],
                "language": "cpp",
            }),
            &user,
        )
        .await;
    assert_eq!(
        sub2_res.status, 429,
        "Second rapid standalone submission should be rejected: {}",
        sub2_res.text
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn different_users_have_independent_cooldowns() {
    let app = E2eTestApp::spawn().await;

    let admin = app
        .create_user_with_role("cd_admin5", "password", "admin")
        .await;
    let user_a = app.create_authenticated_user("cd_userA5", "password").await;
    let user_b = app.create_authenticated_user("cd_userB5", "password").await;

    let problem_id = app.create_problem(&admin, "Cooldown Problem 5").await;

    let contest_id = app
        .create_typed_contest(&admin, "Cooldown Contest 5", "icpc", true, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &admin)
        .await;
    app.register_for_contest(contest_id, &user_a).await;
    app.register_for_contest(contest_id, &user_b).await;

    let config_path =
        format!("/api/v1/contests/{contest_id}/problems/{problem_id}/config/cooldown/cooldown");
    app.put_with_token(
        &config_path,
        &json!({ "config": { "cooldown_seconds": 60 }, "enabled": true }),
        &admin,
    )
    .await;

    seed_recent_submission(&app, "cd_userA5", problem_id, Some(contest_id)).await;

    let status_path = format!(
        "/api/v1/p/cooldown/api/plugins/cooldown/contests/{contest_id}/problems/{problem_id}/status"
    );
    let res = app.get_with_token(&status_path, &user_b).await;
    assert_eq!(res.status, 200, "User B status failed: {}", res.text);
    assert_eq!(res.body["can_submit"].as_bool(), Some(true));
}
