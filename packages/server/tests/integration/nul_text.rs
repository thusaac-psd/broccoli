//! Text a person submits through the API never reaches Postgres with a NUL
//! (U+0000) in it: TEXT rejects it with 22021 and JSONB with 22P05. Every
//! entry point answers such text with a 400 `VALIDATION_ERROR` and stores
//! nothing.

use serde_json::{Value, json};

use crate::common::{TestApp, TestResponse, routes};

fn assert_rejected(res: &TestResponse, entry: &str) {
    assert_eq!(res.status, 400, "{entry}: {}", res.text);
    assert_eq!(
        res.body["code"], "VALIDATION_ERROR",
        "{entry}: {}",
        res.text
    );
}

async fn admin(app: &TestApp) -> String {
    app.create_user_with_role("admin1", "pass1234", "admin")
        .await
}

#[tokio::test]
async fn login_with_a_nul_username_is_rejected() {
    let app = TestApp::spawn().await;
    let res = app
        .post_without_token(
            routes::LOGIN,
            &json!({ "username": "ad\u{0}min", "password": "pass1234" }),
        )
        .await;
    assert_rejected(&res, "login");
}

#[tokio::test]
async fn clarification_with_nul_content_is_rejected() {
    let app = TestApp::spawn().await;
    let admin = admin(&app).await;
    let cid = app.create_contest(&admin, "C1", true, false).await;
    let res = app
        .post_with_token(
            &routes::contest_clarifications(cid),
            &json!({ "content": "a\u{0}b", "clarification_type": "announcement" }),
            &admin,
        )
        .await;
    assert_rejected(&res, "clarification");
}

#[tokio::test]
async fn contest_with_a_nul_title_is_rejected() {
    let app = TestApp::spawn().await;
    let admin = admin(&app).await;
    let res = app
        .post_with_token(
            routes::CONTESTS,
            &json!({
                "title": "C\u{0}1",
                "description": "d",
                "activate_time": "2020-01-01T00:00:00Z",
                "start_time": "2020-01-01T00:00:00Z",
                "end_time": "2099-01-02T00:00:00Z",
                "is_public": true,
                "submissions_visible": false,
                "contest_type": "standard",
            }),
            &admin,
        )
        .await;
    assert_rejected(&res, "contest");
}

#[tokio::test]
async fn contest_problem_with_a_nul_label_is_rejected() {
    let app = TestApp::spawn().await;
    let admin = admin(&app).await;
    let cid = app.create_contest(&admin, "C1", true, false).await;
    let pid = app.create_problem(&admin, "P1").await;
    let res = app
        .post_with_token(
            &routes::contest_problems(cid),
            &json!({ "problem_id": pid, "label": "A\u{0}" }),
            &admin,
        )
        .await;
    assert_rejected(&res, "contest problem label");
}

#[tokio::test]
async fn problem_with_a_nul_statement_is_rejected() {
    let app = TestApp::spawn().await;
    let admin = admin(&app).await;
    let res = app
        .post_with_token(
            routes::PROBLEMS,
            &json!({
                "title": "P1",
                "content": "Solve\u{0} this.",
                "time_limit": 1000,
                "memory_limit": 262144,
                "problem_type": "standard",
                "default_contest_type": "standard",
                "checker_format": "exact",
                "is_public": true,
            }),
            &admin,
        )
        .await;
    assert_rejected(&res, "problem statement");
}

#[tokio::test]
async fn test_case_with_a_nul_body_is_rejected() {
    let app = TestApp::spawn().await;
    let admin = admin(&app).await;
    let pid = app.create_problem(&admin, "P1").await;
    let res = app
        .post_with_token(
            &routes::test_cases(pid),
            &json!({ "input": "1\u{0}2", "expected_output": "3", "score": 10 }),
            &admin,
        )
        .await;
    assert_rejected(&res, "test case body");
}

#[tokio::test]
async fn zipped_test_case_with_a_nul_body_is_rejected() {
    use std::io::Write;

    let app = TestApp::spawn().await;
    let admin = admin(&app).await;
    let pid = app.create_problem(&admin, "P1").await;
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, content) in [("01.in", "1\u{0}2\n"), ("01.ans", "3\n")] {
        zip.start_file(name, options).unwrap();
        zip.write_all(content.as_bytes()).unwrap();
    }
    let bytes = zip.finish().unwrap().into_inner();
    let res = app
        .upload_with_token(
            &routes::test_cases_upload(pid),
            "tests.zip",
            bytes,
            Some("*.in"),
            Some("*.ans"),
            Some("replace"),
            &admin,
        )
        .await;
    assert_rejected(&res, "zipped test case body");
    let listed = app.get_with_token(&routes::test_cases(pid), &admin).await;
    assert_eq!(
        listed.body.as_array().map(Vec::len),
        Some(0),
        "{}",
        listed.text
    );
}

#[tokio::test]
async fn submission_with_nul_source_is_rejected() {
    let app = TestApp::spawn().await;
    let admin = admin(&app).await;
    let pid = app.create_problem(&admin, "P1").await;
    let res = app
        .post_with_token(
            &routes::problem_submissions(pid),
            &json!({
                "files": [{ "filename": "main.cpp", "content": "int main()\u{0} {}" }],
                "language": "cpp",
            }),
            &admin,
        )
        .await;
    assert_rejected(&res, "submission source");
}

#[tokio::test]
async fn nul_in_a_query_string_or_path_is_rejected() {
    let app = TestApp::spawn().await;
    let admin = admin(&app).await;
    for path in [
        format!("{}?search=a%00b", routes::CONTESTS),
        format!("{}?search=a%00b", routes::PROBLEMS),
        routes::role_permissions("a%00b"),
    ] {
        let res = app.get_with_token(&path, &admin).await;
        assert_rejected(&res, &path);
    }
}

#[tokio::test]
async fn an_escaped_backslash_before_u0000_is_not_a_nul() {
    let app = TestApp::spawn().await;
    let admin = admin(&app).await;
    let cid = app.create_contest(&admin, "C1", true, false).await;
    let content = r"path\u0000 is literal text";
    let res = app
        .post_with_token(
            &routes::contest_clarifications(cid),
            &json!({ "content": content, "clarification_type": "announcement" }),
            &admin,
        )
        .await;
    assert_eq!(res.status, 201, "{}", res.text);
    assert_eq!(res.body["content"], Value::from(content));
}
