//! Pagination arithmetic on list endpoints (kernel audit, Nov 2026).
//!
//! List handlers used to compute `OFFSET` as `(page - 1) * per_page` on a
//! caller-supplied `page: u64`. That overflowed `u64` (a dropped connection in
//! debug builds, a wrapped offset in release) or produced an offset above
//! `i64::MAX`, which Postgres' `bigint` OFFSET rejects with a 500. Every list
//! endpoint now derives its offset through `models::shared::PageWindow`, which
//! maps both cases to `400 VALIDATION_ERROR`. These tests pin that contract
//! for each paginated endpoint.

use crate::common::{TestApp, routes};

/// `page` large enough that `(page - 1) * 20` overflows `u64`.
const OVERFLOWING_PAGE: &str = "18446744073709551615";
/// `page` whose offset fits in `u64` but not in Postgres' `bigint`:
/// `(page - 1) * 2 = 2^63`, one past `i64::MAX`.
const BIGINT_OVERFLOW_PAGE: &str = "4611686018427387905";

/// Send a GET and return the status, or `None` if the server closed the
/// connection without answering (the panic-in-handler symptom).
async fn status_of(app: &TestApp, path: &str, token: &str) -> Option<u16> {
    app.client
        .get(format!("http://{}{}", app.addr, path))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .ok()
        .map(|r| r.status().as_u16())
}

fn assert_client_error(path: &str, status: Option<u16>) {
    match status {
        None => panic!(
            "{path}: server dropped the connection without a response \
             (handler panicked on u64 overflow in `(page - 1) * per_page`)"
        ),
        Some(s) => assert!(
            (400..500).contains(&s),
            "{path}: expected a 4xx for an out-of-range page, got {s}"
        ),
    }
}

async fn admin(app: &TestApp) -> String {
    app.create_user_with_role("pg_admin", "password123", "admin")
        .await
}

/// Testcase-result pages (`result_selection`) go through the same window.
#[tokio::test]
async fn submission_result_page_overflow_is_a_validation_error() {
    let app = TestApp::spawn().await;
    let token = admin(&app).await;
    let pid = app.create_problem(&token, "P").await;
    let sid = app
        .create_submission(pid, &token, "cpp", "int main(){}")
        .await;

    let path = format!(
        "{}?page={OVERFLOWING_PAGE}",
        routes::submission_judgements(sid)
    );
    let status = status_of(&app, &path, &token).await;
    assert_ne!(status, None, "{path}: connection dropped");
    // 400 (bad page) or 404 (no judgement yet) are both fine; what matters is
    // that the guarded arithmetic answers instead of panicking.
    assert!(
        status.unwrap() < 500,
        "{path}: got {status:?}, expected a non-5xx"
    );
}

#[tokio::test]
async fn list_problems_rejects_overflowing_page() {
    let app = TestApp::spawn().await;
    let token = admin(&app).await;
    for page in [OVERFLOWING_PAGE, BIGINT_OVERFLOW_PAGE] {
        let path = format!("{}?page={page}&per_page=2", routes::PROBLEMS);
        assert_client_error(&path, status_of(&app, &path, &token).await);
    }
}

#[tokio::test]
async fn list_contests_rejects_overflowing_page() {
    let app = TestApp::spawn().await;
    let token = admin(&app).await;
    for page in [OVERFLOWING_PAGE, BIGINT_OVERFLOW_PAGE] {
        let path = format!("{}?page={page}&per_page=2", routes::CONTESTS);
        assert_client_error(&path, status_of(&app, &path, &token).await);
    }
}

#[tokio::test]
async fn list_submissions_rejects_overflowing_page() {
    let app = TestApp::spawn().await;
    let token = admin(&app).await;
    for page in [OVERFLOWING_PAGE, BIGINT_OVERFLOW_PAGE] {
        let path = format!("{}?page={page}&per_page=2", routes::SUBMISSIONS);
        assert_client_error(&path, status_of(&app, &path, &token).await);
    }
}

#[tokio::test]
async fn list_contest_submissions_rejects_overflowing_page() {
    let app = TestApp::spawn().await;
    let token = admin(&app).await;
    let cid = app.create_contest(&token, "C", true, true).await;
    for page in [OVERFLOWING_PAGE, BIGINT_OVERFLOW_PAGE] {
        let path = format!(
            "{}?page={page}&per_page=2",
            routes::contest_submissions(cid)
        );
        assert_client_error(&path, status_of(&app, &path, &token).await);
    }
}

#[tokio::test]
async fn list_dlq_rejects_overflowing_page() {
    let app = TestApp::spawn().await;
    let token = admin(&app).await;
    for page in [OVERFLOWING_PAGE, BIGINT_OVERFLOW_PAGE] {
        let path = format!("{}?page={page}&per_page=2", routes::DLQ);
        assert_client_error(&path, status_of(&app, &path, &token).await);
    }
}
