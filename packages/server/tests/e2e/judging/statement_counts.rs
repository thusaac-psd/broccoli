//! Measures the SQL a judged test case and a submission poll cost, from
//! `pg_stat_statements`. A measurement, not a check: run it with
//! `E2E_SANDBOX_BACKEND=mock cargo test -p server --test e2e statement_counts -- --ignored --nocapture`.

use sea_orm::{ConnectionTrait, DbBackend, Statement};

use crate::common::E2eTestApp;

/// Calls per normalized statement in this test's database since the last
/// reset.
async fn statement_calls(app: &E2eTestApp) -> Vec<(String, i64)> {
    app.db
        .query_all_raw(Statement::from_string(
            DbBackend::Postgres,
            "SELECT query, calls::bigint AS calls FROM pg_stat_statements \
             WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database()) \
             AND query NOT LIKE '%pg_stat_statements%' ORDER BY calls DESC",
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

async fn reset(app: &E2eTestApp) {
    app.db
        .execute_unprepared("SELECT pg_stat_statements_reset()")
        .await
        .expect("reset pg_stat_statements");
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

/// Judges two submissions over `cases` test cases and returns the statements
/// the second cost, from submit to the terminal poll.
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
    for round in 0..2 {
        reset(app).await;
        let sub = app
            .create_submission(problem_id, admin, "cpp", super::CPP_SUM)
            .await;
        let res = app.wait_for_submission_terminal(sub, admin, 120).await;
        assert_eq!(res.body["status"], "Judged", "{}", res.text);
        if round == 1 {
            return statement_calls(app).await;
        }
    }
    unreachable!()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement; prints statement counts"]
async fn reports_statements_per_test_case_and_per_poll() {
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
        delta.sort_by_key(|(_, n)| -n);
        print_top(&format!("per test case ({checker} checker)"), &delta, 20.0);
        print_top(
            &format!("whole submission of 25 ({checker} checker)"),
            &large,
            1.0,
        );
    }

    let problem_id = app.create_problem(&admin, "Poll").await;
    app.create_test_case(problem_id, &admin).await;
    let sub = app
        .create_submission(problem_id, &admin, "cpp", super::CPP_SUM)
        .await;
    app.wait_for_submission_terminal(sub, &admin, 60).await;
    const POLLS: i64 = 50;
    reset(&app).await;
    for _ in 0..POLLS {
        let res = app
            .get_with_token(&format!("/api/v1/submissions/{sub}"), &admin)
            .await;
        assert_eq!(res.status, 200, "{}", res.text);
    }
    print_top("per poll", &statement_calls(&app).await, POLLS as f64);
}
