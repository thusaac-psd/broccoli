//! The SQL a judged test case and a submission poll cost, counted with
//! `pg_stat_statements` in this test's own database and held under fixed
//! bounds, so a change that adds per-case or per-poll statements fails here.
//! `--nocapture` prints the statements behind each count.

use std::collections::HashMap;

use sea_orm::{ConnectionTrait, DbBackend, Statement};

use crate::common::E2eTestApp;

/// Statements one more judged test case may cost a submission: its result
/// row; the problem and its additional files, which the evaluate window that
/// judges it reads (ICPC judges one case per window); and the worker's
/// existence probe before it uploads the case's output, a statement here
/// because the test's blob store is the database. A fifth fails.
///
/// Only statements that run at least once per added case count
/// ([`per_case_statements`]): the app's background loops (claim ticks,
/// leases) run meanwhile, but they tick with how long judging takes, not with
/// what it does, so they show up at a fraction per case that depends on the
/// machine.
const MAX_PER_TEST_CASE: f64 = 4.0;

/// Marks the statements this test issues to wait for a submission, which the
/// counts leave out: waiting by polling the API would count statements that
/// grow with how long judging takes, not with what it does.
const WAIT_MARK: &str = "statement_counts:wait";

/// Statements one `GET /submissions/{id}` of a judged submission may cost:
/// the submission, its results and the view context. Half a statement of
/// room for background loops; a fourth statement per poll fails.
const MAX_PER_POLL: f64 = 3.5;

/// Calls so far per normalized statement in this test's database. Other
/// databases on the shared test server are never read or reset.
async fn snapshot(app: &E2eTestApp) -> HashMap<String, i64> {
    app.db
        .query_all_raw(Statement::from_string(
            DbBackend::Postgres,
            "SELECT query, sum(calls)::bigint AS calls FROM pg_stat_statements \
             WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database()) \
             AND query NOT LIKE '%pg_stat_statements%' \
             AND query NOT LIKE '%statement_counts:wait%' GROUP BY query",
        ))
        .await
        .expect("pg_stat_statements is loaded in the test database server")
        .into_iter()
        .map(|row| {
            (
                row.try_get::<String>("", "query").unwrap(),
                row.try_get::<i64>("", "calls").unwrap(),
            )
        })
        .collect()
}

/// Calls per statement between two snapshots, most frequent first.
fn since(before: &HashMap<String, i64>, after: &HashMap<String, i64>) -> Vec<(String, i64)> {
    let mut delta: Vec<(String, i64)> = after
        .iter()
        .map(|(q, n)| (q.clone(), n - before.get(q).copied().unwrap_or(0)))
        .filter(|(_, n)| *n > 0)
        .collect();
    delta.sort_by_key(|(q, n)| (-n, q.clone()));
    delta
}

/// The statements of `delta` (calls the `added` more cases caused) that ran
/// at least once per added case, and their calls per case.
fn per_case_statements(delta: &[(String, i64)], added: i64) -> Vec<(String, i64)> {
    delta.iter().filter(|(_, n)| *n >= added).cloned().collect()
}

fn total(calls: &[(String, i64)]) -> i64 {
    calls.iter().map(|(_, n)| n).sum()
}

fn print_top(label: &str, calls: &[(String, i64)], per: f64) {
    println!("--- {label}: {:.1} statements", total(calls) as f64 / per);
    for (query, n) in calls.iter().take(25) {
        let query: String = query.split_whitespace().collect::<Vec<_>>().join(" ");
        let query: String = query.chars().take(160).collect();
        println!("{:>7.2}  {query}", *n as f64 / per);
    }
}

/// A testlib checker that accepts everything; with the mock sandbox only its
/// config reads matter.
const CHECKER: &str = "#include \"testlib.h\"\nint main(int c, char* v[]) { registerTestlibCmd(c, v); quitf(_ok, \"ok\"); }\n";

/// Waits until submission `id` is judged, reading its status with a
/// statement the counts leave out ([`WAIT_MARK`]).
async fn wait_judged_unmeasured(app: &E2eTestApp, id: i32) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let row = app
            .db
            .query_one_raw(Statement::from_string(
                DbBackend::Postgres,
                format!("SELECT /* {WAIT_MARK} */ status::text AS status FROM submission WHERE id = {id}"),
            ))
            .await
            .unwrap()
            .expect("the submission");
        let status: String = row.try_get("", "status").unwrap();
        if status == "Judged" {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "submission {id} still {status}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Judges two submissions over `cases` test cases and returns the statements
/// the second cost, from submit to its last verdict. With the `exact`
/// checker every case must be accepted, so the count is of cases judged, not
/// of cases a first wrong answer skipped.
async fn judge(app: &E2eTestApp, admin: &str, checker: &str, cases: usize) -> Vec<(String, i64)> {
    let problem_id = app
        .create_problem_with_checker_format(admin, &format!("Count {checker} {cases}"), checker)
        .await;
    if checker == "testlib" {
        let res = app
            .put_with_token(
                &format!("/api/v1/problems/{problem_id}/checker-source"),
                &serde_json::json!({
                    "files": [{ "filename": "checker.cpp", "content": CHECKER }]
                }),
                admin,
            )
            .await;
        assert_eq!(res.status, 200, "{}", res.text);
    }
    for _ in 0..cases {
        app.create_test_case(problem_id, admin).await;
    }
    // The second submission shows the steady state: config read once, after
    // the checker upload, is cached by then.
    let mut measured = Vec::new();
    for _ in 0..2 {
        let before = snapshot(app).await;
        let sub = app
            .create_submission(problem_id, admin, "cpp", super::CPP_SUM)
            .await;
        wait_judged_unmeasured(app, sub).await;
        measured = since(&before, &snapshot(app).await);
        let res = app
            .get_with_token(&format!("/api/v1/submissions/{sub}"), admin)
            .await;
        assert_eq!(res.body["status"], "Judged", "{}", res.text);
        if checker == "exact" {
            // ICPC accepts a submission only when every case is accepted.
            assert_eq!(res.body["result"]["verdict"], "Accepted", "{}", res.text);
            assert_eq!(
                res.body["result"]["test_case_pagination"]["total"], cases,
                "{}",
                res.text
            );
        }
    }
    measured
}

#[tokio::test(flavor = "multi_thread")]
async fn a_test_case_and_a_poll_stay_within_their_statement_budgets() {
    if std::env::var("E2E_SERVER_URL").is_ok() {
        // A remote stack's database is shared with whatever else runs on it.
        return;
    }
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("counter", "pass1234", "admin")
        .await;

    for checker in ["exact", "testlib"] {
        let small = judge(&app, &admin, checker, 5).await;
        let large = judge(&app, &admin, checker, 25).await;
        let mut delta: Vec<(String, i64)> = large
            .iter()
            .map(|(q, n)| {
                let before = small.iter().find(|(s, _)| s == q).map_or(0, |(_, m)| *m);
                (q.clone(), n - before)
            })
            .filter(|(_, n)| *n > 0)
            .collect();
        delta.sort_by_key(|(q, n)| (-n, q.clone()));
        let label = format!("per test case ({checker} checker)");
        print_top(&label, &delta, 20.0);
        print_top(
            &format!("whole submission of 25 ({checker} checker)"),
            &large,
            1.0,
        );
        let per_case = total(&per_case_statements(&delta, 20)) as f64 / 20.0;
        assert!(
            per_case <= MAX_PER_TEST_CASE,
            "{label}: {per_case:.1} statements, budget {MAX_PER_TEST_CASE} (run with --nocapture for the list)"
        );
    }

    let problem_id = app.create_problem(&admin, "Poll").await;
    app.create_test_case(problem_id, &admin).await;
    let sub = app
        .create_submission(problem_id, &admin, "cpp", super::CPP_SUM)
        .await;
    app.wait_for_submission_terminal(sub, &admin, 60).await;
    const POLLS: i64 = 50;
    let before = snapshot(&app).await;
    for _ in 0..POLLS {
        let res = app
            .get_with_token(&format!("/api/v1/submissions/{sub}"), &admin)
            .await;
        assert_eq!(res.status, 200, "{}", res.text);
    }
    let polls = since(&before, &snapshot(&app).await);
    print_top("per poll", &polls, POLLS as f64);
    let per_poll = total(&polls) as f64 / POLLS as f64;
    assert!(
        per_poll <= MAX_PER_POLL,
        "per poll: {per_poll:.1} statements, budget {MAX_PER_POLL} (run with --nocapture for the list)"
    );
}
