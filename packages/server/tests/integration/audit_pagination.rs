//! Pagination arithmetic on list endpoints (kernel audit, Nov 2026).
//!
//! Every admin/contestant list handler computes `OFFSET` as
//! `(page - 1) * per_page` on a caller-supplied `page: u64`. Only
//! `handlers/submission/response.rs::result_selection` guards it with
//! `checked_mul(..).filter(|n| *n <= i64::MAX as u64)` and maps overflow to
//! `400 VALIDATION_ERROR`; the five sibling sites below were never brought in
//! line:
//!
//! - `handlers/problem/mod.rs`        `list_problems`
//! - `handlers/contest/mod.rs`        `list_contests`
//! - `handlers/submission/mod.rs`     `list_submissions`
//! - `handlers/submission/mod.rs`     `list_contest_submissions`
//! - `dlq/service.rs`                 `DlqService::list` (uses
//!   `saturating_sub` but still an unchecked `*`)
//!
//! Two failure modes, both reachable with one query parameter:
//!
//! 1. `page * per_page` overflows `u64`. With debug assertions (every test
//!    build) the handler task panics and hyper drops the connection with no
//!    response; in release it silently wraps, so e.g. `page=2^63+1,
//!    per_page=2` yields OFFSET 0 and the server returns page-1 rows labelled
//!    as page 9e18.
//! 2. No overflow, but the offset exceeds `i64::MAX`. Postgres `OFFSET` is a
//!    `bigint`, so the bound value is rejected and the caller gets a 500.
//!
//! The tests assert the contract `result_selection` already established: an
//! out-of-range page is a client error (4xx), never a dropped connection or a
//! 5xx. They are `#[ignore]`d until the handlers are fixed; run them with
//! `cargo test -p server --test integration audit_pagination -- --ignored`.

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

/// Control: the one guarded site already behaves. Not ignored - this pins the
/// contract the other sites should converge on.
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
#[ignore = "BUG: list_problems computes (page - 1) * per_page unchecked on u64"]
async fn list_problems_rejects_overflowing_page() {
    let app = TestApp::spawn().await;
    let token = admin(&app).await;
    for page in [OVERFLOWING_PAGE, BIGINT_OVERFLOW_PAGE] {
        let path = format!("{}?page={page}&per_page=2", routes::PROBLEMS);
        assert_client_error(&path, status_of(&app, &path, &token).await);
    }
}

#[tokio::test]
#[ignore = "BUG: list_contests computes (page - 1) * per_page unchecked on u64"]
async fn list_contests_rejects_overflowing_page() {
    let app = TestApp::spawn().await;
    let token = admin(&app).await;
    for page in [OVERFLOWING_PAGE, BIGINT_OVERFLOW_PAGE] {
        let path = format!("{}?page={page}&per_page=2", routes::CONTESTS);
        assert_client_error(&path, status_of(&app, &path, &token).await);
    }
}

#[tokio::test]
#[ignore = "BUG: list_submissions computes (page - 1) * per_page unchecked on u64"]
async fn list_submissions_rejects_overflowing_page() {
    let app = TestApp::spawn().await;
    let token = admin(&app).await;
    for page in [OVERFLOWING_PAGE, BIGINT_OVERFLOW_PAGE] {
        let path = format!("{}?page={page}&per_page=2", routes::SUBMISSIONS);
        assert_client_error(&path, status_of(&app, &path, &token).await);
    }
}

#[tokio::test]
#[ignore = "BUG: list_contest_submissions computes (page - 1) * per_page unchecked on u64"]
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
#[ignore = "BUG: DlqService::list computes page.saturating_sub(1) * per_page unchecked on u64"]
async fn list_dlq_rejects_overflowing_page() {
    let app = TestApp::spawn().await;
    let token = admin(&app).await;
    for page in [OVERFLOWING_PAGE, BIGINT_OVERFLOW_PAGE] {
        let path = format!("{}?page={page}&per_page=2", routes::DLQ);
        assert_client_error(&path, status_of(&app, &path, &token).await);
    }
}
