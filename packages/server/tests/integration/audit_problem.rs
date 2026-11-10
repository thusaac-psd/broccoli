//! Problem-management defects found by the kernel audit (Nov 2026).
//!
//! Each `#[ignore]`d test reproduces a bug and asserts the CORRECT behaviour,
//! so it fails today. Run the whole set with:
//!
//! `cargo test -p server --test integration audit_problem -- --ignored`
//!
//! When fixing a bug, delete its `#[ignore]` so the test becomes a regression
//! guard.

use common::{SubmissionStatus, Verdict};
use sea_orm::{ActiveModelTrait, Set};
use serde_json::json;
use server::entity::{submission, test_case_result};

use crate::common::{TestApp, routes};
use crate::problem::build_zip;

/// Insert a judged submission with one result row pointing at `tc_id`, the
/// state every problem with real traffic is in.
async fn insert_judged_result(app: &TestApp, token: &str, pid: i32, tc_id: i32) {
    let user_id = app.get_with_token(routes::ME, token).await.id();
    let now = chrono::Utc::now();
    let sub = submission::ActiveModel {
        files: Set(json!([{"filename": "main.cpp", "content": "int main(){}"}])),
        language: Set("cpp".into()),
        status: Set(SubmissionStatus::Judged),
        verdict: Set(Some(Verdict::Accepted)),
        score: Set(Some(100.0)),
        user_id: Set(user_id),
        problem_id: Set(pid),
        created_at: Set(now),
        judged_at: Set(Some(now)),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert submission");
    test_case_result::ActiveModel {
        submission_id: Set(sub.id),
        test_case_id: Set(Some(tc_id)),
        verdict: Set(Verdict::Accepted),
        score: Set(10.0),
        created_at: Set(now),
        ..Default::default()
    }
    .insert(&app.db)
    .await
    .expect("insert test_case_result");
}

/// BUG (med-high): `require_permission(PROBLEM_EDIT)` runs in the handler
/// body, after `AppJson`/`BaseMultipart` have buffered and parsed the whole
/// request (route limit: 1 GiB). Any authenticated user can make the server
/// allocate ~1 GiB per request before being told 403. Observable proxy: a
/// malformed body from an unauthorised caller gets 400 (body was parsed)
/// instead of 403.
/// Location: handlers/problem/test_cases.rs (create/update/reorder/bulk) and
/// handlers/problem/upload.rs. `checker_source.rs` documents the same class.
#[tokio::test]
async fn test_case_writes_check_permission_before_reading_body() {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_role("ap_admin", "password123", "admin")
        .await;
    let contestant = app
        .create_user_with_role("ap_user", "password123", "contestant")
        .await;
    let pid = app.create_problem(&admin, "P").await;

    let res = app
        .post_with_token(&routes::test_cases(pid), &json!({}), &contestant)
        .await;
    assert_eq!(
        res.status, 403,
        "contestant got {} (body parsed before permission check): {}",
        res.status, res.text
    );

    let res = app
        .upload_with_token(
            &routes::test_cases_upload(pid),
            "t.zip",
            b"not a zip".to_vec(),
            None,
            None,
            None,
            &contestant,
        )
        .await;
    assert_eq!(
        res.status, 403,
        "contestant upload got {} (body parsed before permission check): {}",
        res.status, res.text
    );
}

/// BUG (med): ZIP upload with `strategy=replace` deletes every test case with
/// no `test_case_result` check. Single and bulk delete both refuse with 409
/// when results exist; replace instead hits the FK and returns 500, so a
/// problem that has ever been judged can never be re-uploaded with replace.
/// Location: handlers/problem/upload.rs (replace branch).
#[tokio::test]
async fn replace_upload_on_judged_problem_is_a_conflict_not_500() {
    let app = TestApp::spawn().await;
    let token = app
        .create_user_with_role("ap_admin", "password123", "admin")
        .await;
    let pid = app.create_problem(&token, "P").await;
    let tc_id = app.create_test_case(pid, &token).await;
    insert_judged_result(&app, &token, pid, tc_id).await;

    let res = app
        .upload_with_token(
            &routes::test_cases_upload(pid),
            "t.zip",
            build_zip(&[("1.in", "a"), ("1.ans", "b")]),
            Some("*.in"),
            Some("*.ans"),
            Some("replace"),
            &token,
        )
        .await;
    assert_eq!(
        res.status, 409,
        "expected 409 CONFLICT like delete/bulk-delete, got {}: {}",
        res.status, res.text
    );
}

/// BUG (med): creating a test case without `label` auto-assigns
/// `label = position.to_string()` and then rejects it with 409 if that label
/// is taken. After the usual 1-based ZIP upload (`1.in`.. -> positions 0..N-1,
/// labels "1".."N") the next auto label is always taken.
/// Location: handlers/problem/test_cases.rs (`create_test_case`).
#[tokio::test]
async fn create_test_case_after_zip_upload_gets_a_free_label() {
    let app = TestApp::spawn().await;
    let token = app
        .create_user_with_role("ap_admin", "password123", "admin")
        .await;
    let pid = app.create_problem(&token, "P").await;

    let up = app
        .upload_with_token(
            &routes::test_cases_upload(pid),
            "t.zip",
            build_zip(&[("1.in", "a"), ("1.ans", "b")]),
            Some("*.in"),
            Some("*.ans"),
            Some("abort"),
            &token,
        )
        .await;
    assert!(up.status < 300, "upload failed: {}", up.text);

    let res = app
        .post_with_token(
            &routes::test_cases(pid),
            &json!({"input": "x", "expected_output": "y", "score": 1, "is_sample": false}),
            &token,
        )
        .await;
    assert_eq!(
        res.status, 201,
        "server rejected a label it generated itself: {}",
        res.text
    );
}

/// BUG (med): `validate_submission_format` validates the TRIMMED language id
/// and filenames but create/update store the raw map, while submission-time
/// matching trims the submitted values and compares exactly. A format that
/// passed validation makes the problem impossible to submit to.
/// Location: models/problem.rs `validate_submission_format`,
/// handlers/problem/mod.rs (stores `serde_json::to_value(sf)` raw),
/// utils/judging.rs (`validate_submission_contract`).
#[tokio::test]
#[ignore = "BUG: submission_format validated trimmed but stored raw -> problem cannot accept submissions"]
async fn submission_format_with_padded_filename_is_normalised_or_rejected() {
    let app = TestApp::spawn().await;
    let token = app
        .create_user_with_role("ap_admin", "password123", "admin")
        .await;
    let res = app
        .post_with_token(
            routes::PROBLEMS,
            &json!({
                "title": "Padded",
                "content": "x",
                "time_limit": 1000,
                "memory_limit": 262144,
                "problem_type": "standard",
                "default_contest_type": "standard",
                "checker_format": "exact",
                "is_public": true,
                "submission_format": {"cpp": [" solution.cpp"]},
            }),
            &token,
        )
        .await;
    if res.status == 400 {
        // Rejecting the padded name up front is an acceptable fix.
        return;
    }
    assert_eq!(res.status, 201, "{}", res.text);
    let pid = res.id();

    let sub = app
        .post_with_token(
            &routes::problem_submissions(pid),
            &json!({"language": "cpp", "files": [{"filename": "solution.cpp", "content": "int main(){}"}]}),
            &token,
        )
        .await;
    assert_eq!(
        sub.status, 201,
        "a submission matching the (validated) format was refused: {}",
        sub.text
    );
}

/// BUG (low-med): in ZIP parsing a filename matching BOTH formats is always
/// taken as input, so overlapping patterns such as Polygon's `*` / `*.a`
/// (`tests/01` + `tests/01.a`) fail with "Input files without matching
/// output". Both patterns pass format validation.
/// Location: handlers/problem/upload.rs (`label_as_input` checked first).
#[tokio::test]
#[ignore = "BUG: overlapping input/output formats (`*` and `*.a`) misclassify answer files as inputs"]
async fn zip_upload_with_overlapping_formats_pairs_files() {
    let app = TestApp::spawn().await;
    let token = app
        .create_user_with_role("ap_admin", "password123", "admin")
        .await;
    let pid = app.create_problem(&token, "P").await;
    let res = app
        .upload_with_token(
            &routes::test_cases_upload(pid),
            "t.zip",
            build_zip(&[("01", "in"), ("01.a", "out")]),
            Some("*"),
            Some("*.a"),
            Some("abort"),
            &token,
        )
        .await;
    assert!(
        res.status < 300,
        "expected one test case `01`, got {}: {}",
        res.status,
        res.text
    );
}

/// BUG (low): child resources of a soft-deleted problem remain readable.
/// `list_test_cases` 404s for a deleted problem, but `get_test_case` (via
/// `find_test_case_for_problem`, which never checks `problem.deleted_at`)
/// still returns the hidden test data to editors.
/// Location: handlers/problem/test_cases.rs `find_test_case_for_problem`.
#[tokio::test]
#[ignore = "BUG: get_test_case still serves a soft-deleted problem's test data"]
async fn soft_deleted_problem_hides_individual_test_cases() {
    let app = TestApp::spawn().await;
    let token = app
        .create_user_with_role("ap_admin", "password123", "admin")
        .await;
    let pid = app.create_problem(&token, "P").await;
    let tc_id = app.create_test_case(pid, &token).await;

    let del = app.delete_with_token(&routes::problem(pid), &token).await;
    assert_eq!(del.status, 204, "{}", del.text);

    let list = app.get_with_token(&routes::test_cases(pid), &token).await;
    assert_eq!(list.status, 404, "control: list already 404s");

    let one = app
        .get_with_token(&routes::test_case(pid, tc_id), &token)
        .await;
    assert_eq!(
        one.status, 404,
        "sibling endpoints disagree: list=404 but get={}",
        one.status
    );
}
