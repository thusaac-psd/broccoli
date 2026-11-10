//! A problem held by two contests at once: a codelink-bracket contest that
//! has not started (or has ended), and an ordinary contest that is running.
//!
//! Every read of the problem's parts (statement, samples, attachments) and
//! every submission through the running contest carries that contest's
//! scope, so only that contest's rules and plugins decide it. The bracket's
//! own rules still apply to reads and submissions outside every contest,
//! until the bracket has ended.

use crate::codelink_bracket::{Player, player, spawn_bracket_app};
use crate::common::{TestApp, routes};
use broccoli_server_sdk::permissions as perm;
use sea_orm::{ActiveModelTrait, EntityTrait, Set};
use serde_json::json;
use server::entity::contest;

const ATTACHMENT_BYTES: &[u8] = b"shared attachment";

struct Fixture {
    app: TestApp,
    _plugins_tmp: tempfile::TempDir,
    bracket_id: i32,
    standard_id: i32,
    problem_id: i32,
    sample_id: i32,
    attachment_id: String,
    contestant: Player,
}

async fn setup_fixture() -> Fixture {
    let (app, plugins_tmp) = spawn_bracket_app().await;
    // The loaded plugins register only the bracket's contest type, so the
    // harness's stub "standard" type is not there; add it for the ordinary
    // contest.
    app.state
        .registries
        .contest_type_registry
        .write()
        .await
        .insert(
            "standard".into(),
            server::registry::ContestTypeHandlers {
                plugin_id: "__test__".into(),
                submission_fn: "noop".into(),
                code_run_fn: "noop".into(),
                submission_admission: false,
            },
        );

    let staff_token = app
        .create_user_with_permissions(
            "shared_staff",
            "pass1234",
            &[
                perm::CONTEST_CREATE,
                perm::CONTEST_MANAGE,
                perm::PROBLEM_CREATE,
                perm::PROBLEM_EDIT,
            ],
        )
        .await;
    let contestant = player(&app, "shared_contestant").await;

    let res = app
        .post_with_token(
            routes::PROBLEMS,
            &json!({
                "title": "Shared Problem",
                "content": "## Description\nSubmit `ACCEPT` to solve.",
                "time_limit": 1000,
                "memory_limit": 262144,
                "problem_type": "codelink-bracket-judge-fixture",
                "checker_format": "none",
                "default_contest_type": "standard",
                "is_public": false,
            }),
            &staff_token,
        )
        .await;
    assert_eq!(res.status, 201, "create problem failed: {}", res.text);
    let problem_id = res.id();
    let sample_id = app.create_test_case(problem_id, &staff_token).await;
    let upload = app
        .upload_attachment(
            problem_id,
            "notes.txt",
            ATTACHMENT_BYTES.to_vec(),
            None,
            &staff_token,
        )
        .await;
    assert_eq!(upload.status, 201, "upload failed: {}", upload.text);
    let attachment_id = upload.body["id"]
        .as_str()
        .expect("attachment response should carry an id")
        .to_string();

    let res = app
        .post_with_token(
            routes::CONTESTS,
            &json!({
                "title": "Pending Bracket",
                "description": "A bracket that has not started",
                "activate_time": "2020-01-01T00:00:00Z",
                "start_time": "2099-01-01T00:00:00Z",
                "end_time": "2099-01-02T00:00:00Z",
                "is_public": true,
                "submissions_visible": false,
                "contest_type": "codelink-bracket",
            }),
            &staff_token,
        )
        .await;
    assert_eq!(res.status, 201, "create bracket failed: {}", res.text);
    let bracket_id = res.id();
    app.add_problem_to_contest(bracket_id, problem_id, &staff_token)
        .await;

    let standard_id = app
        .create_contest(&staff_token, "Running Qualifier", false, false)
        .await;
    app.add_problem_to_contest(standard_id, problem_id, &staff_token)
        .await;
    let res = app
        .post_with_token(
            &routes::contest_participants(standard_id),
            &json!({ "user_id": contestant.id }),
            &staff_token,
        )
        .await;
    assert_eq!(res.status, 201, "add participant failed: {}", res.text);

    Fixture {
        app,
        _plugins_tmp: plugins_tmp,
        bracket_id,
        standard_id,
        problem_id,
        sample_id,
        attachment_id,
        contestant,
    }
}

fn submission_body() -> serde_json::Value {
    json!({
        "files": [{"filename": "main.cpp", "content": "ACCEPT"}],
        "language": "cpp",
    })
}

#[tokio::test]
async fn the_running_contest_reads_and_submits_a_problem_a_pending_bracket_also_holds() {
    let fx = setup_fixture().await;
    let (cid, pid, token) = (fx.standard_id, fx.problem_id, &fx.contestant.token);

    let res = fx
        .app
        .get_with_token(&routes::contest_problem(cid, pid), token)
        .await;
    assert_eq!(res.status, 200, "statement: {}", res.text);
    assert_eq!(res.body["id"], pid);

    let res = fx
        .app
        .get_with_token(&routes::contest_problem_samples(cid, pid), token)
        .await;
    assert_eq!(res.status, 200, "samples: {}", res.text);
    assert_eq!(res.body["samples"].as_array().map(Vec::len), Some(1));

    let res = fx
        .app
        .get_with_token(
            &routes::contest_problem_sample(cid, pid, fx.sample_id),
            token,
        )
        .await;
    assert_eq!(res.status, 200, "one sample: {}", res.text);
    assert_eq!(res.body["id"], fx.sample_id);

    let res = fx
        .app
        .get_with_token(&routes::contest_problem_attachments(cid, pid), token)
        .await;
    assert_eq!(res.status, 200, "attachment list: {}", res.text);
    assert_eq!(res.body["total"], 1);
    assert_eq!(res.body["attachments"][0]["id"], fx.attachment_id.as_str());

    let res = fx
        .app
        .download_raw(
            &routes::contest_problem_attachment(cid, pid, &fx.attachment_id),
            token,
        )
        .await;
    assert_eq!(res.status().as_u16(), 200);
    assert_eq!(res.bytes().await.unwrap().as_ref(), ATTACHMENT_BYTES);

    let res = fx
        .app
        .post_with_token(
            &routes::contest_problem_submissions(cid, pid),
            &submission_body(),
            token,
        )
        .await;
    assert_eq!(res.status, 201, "contest submission: {}", res.text);
}

#[tokio::test]
async fn a_pending_bracket_still_hides_its_problem_outside_every_contest() {
    let fx = setup_fixture().await;
    let (pid, token) = (fx.problem_id, &fx.contestant.token);

    let res = fx.app.get_with_token(&routes::problem(pid), token).await;
    assert_eq!(res.status, 404, "standalone statement: {}", res.text);

    let res = fx
        .app
        .download_raw(&routes::attachment(pid, &fx.attachment_id), token)
        .await;
    assert_eq!(res.status().as_u16(), 404, "standalone attachment");

    let res = fx
        .app
        .post_with_token(&routes::problem_submissions(pid), &submission_body(), token)
        .await;
    assert_eq!(res.status, 404, "standalone submission: {}", res.text);

    // The contestant is not in the bracket, so its own route stays closed.
    let res = fx
        .app
        .get_with_token(&routes::contest_problem(fx.bracket_id, pid), token)
        .await;
    assert_eq!(res.status, 404, "bracket statement: {}", res.text);
}

#[tokio::test]
async fn an_ended_bracket_no_longer_hides_its_problem_outside_every_contest() {
    let fx = setup_fixture().await;
    let ended: chrono::DateTime<chrono::Utc> = "2020-01-02T00:00:00Z".parse().unwrap();
    let mut bracket: contest::ActiveModel = contest::Entity::find_by_id(fx.bracket_id)
        .one(&fx.app.db)
        .await
        .unwrap()
        .expect("bracket contest exists")
        .into();
    bracket.start_time = Set(ended - chrono::Duration::days(1));
    bracket.end_time = Set(ended);
    bracket.update(&fx.app.db).await.unwrap();

    let (pid, token) = (fx.problem_id, &fx.contestant.token);
    let res = fx.app.get_with_token(&routes::problem(pid), token).await;
    assert_eq!(res.status, 200, "standalone statement: {}", res.text);

    let res = fx
        .app
        .post_with_token(&routes::problem_submissions(pid), &submission_body(), token)
        .await;
    assert_eq!(res.status, 201, "standalone submission: {}", res.text);
}
