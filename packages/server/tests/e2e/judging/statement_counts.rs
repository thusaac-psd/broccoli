//! The SQL a judged test case and a submission poll cost, counted with
//! `pg_stat_statements` in this test's own database and pinned exactly, so a
//! change that adds per-case or per-poll work fails here.
//!
//! The counts are of work, not of statement texts: each statement is
//! classified by its verb and the table it acts on ([`class_of`]), a write
//! counts the rows it affected and a read counts its calls. Results that
//! arrive together are written by one multi-row statement, whose text differs
//! from the single-row one, so statement texts and calls vary with batching;
//! the rows written do not. The app's periodic loops ([`PERIODIC`]) tick with
//! how long judging takes, not with what it does, and are left out.
//! `--nocapture` prints the classes behind each count.

use std::collections::{BTreeMap, HashMap};

use sea_orm::{ConnectionTrait, DbBackend, Statement};

use crate::common::E2eTestApp;

/// Marks the statements this test issues to wait for a submission, which the
/// counts leave out: waiting by polling the API would count reads that grow
/// with how long judging takes, not with what it does.
const WAIT_MARK: &str = "statement_counts:wait";

/// The statements the app's periodic loops issue whether or not anything is
/// being judged, by how their normalized text starts: the claim fiber's
/// judgement and code-run claims, the recovery scans that reclaim and retry
/// work, this server's replica heartbeat, and transaction control. They run
/// at a rate set by time, so they are
/// no part of what a case or a poll costs. The platform clock's sample
/// (`common::clock::SAMPLE_QUERY`, on a connection of its own) is one such
/// loop too, left out by that constant.
const PERIODIC: &[&str] = &[
    "WITH room AS (SELECT",
    "SELECT u.id, u.submission_id, u.is_current, u.dispatch_retries",
    "UPDATE plugin_replica SET heartbeat_at",
    "BEGIN",
    "COMMIT",
    "ROLLBACK",
];

/// Calls and affected rows so far per normalized statement in this test's
/// database. Other databases on the shared test server are never read or
/// reset.
async fn snapshot(app: &E2eTestApp) -> HashMap<String, (i64, i64)> {
    app.db
        .query_all_raw(Statement::from_string(
            DbBackend::Postgres,
            "SELECT query, sum(calls)::bigint AS calls, sum(rows)::bigint AS rows \
             FROM pg_stat_statements \
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
                (
                    row.try_get::<i64>("", "calls").unwrap(),
                    row.try_get::<i64>("", "rows").unwrap(),
                ),
            )
        })
        .collect()
}

/// The statement's text without its leading `/* … */` comment, on one line.
fn text_of(query: &str) -> String {
    let mut query = query.trim_start();
    while let Some(rest) = query.strip_prefix("/*") {
        query = rest
            .split_once("*/")
            .map_or("", |(_, after)| after)
            .trim_start();
    }
    query.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The first word at parenthesis depth 0 after `keyword` (also at depth 0):
/// the table a statement acts on, not one a subquery reads.
fn top_level_after(text: &str, keyword: &str) -> Option<String> {
    let words: Vec<&str> = text.split(' ').collect();
    let mut depth = 0i32;
    for (i, word) in words.iter().enumerate() {
        let at_top = depth == 0;
        depth += word.matches('(').count() as i32 - word.matches(')').count() as i32;
        if at_top && word.eq_ignore_ascii_case(keyword) {
            let table = words.get(i + 1)?;
            let table = table.trim_matches(|c: char| c == '"' || c == '(' || c == ',');
            return Some(table.split('.').next_back()?.trim_matches('"').to_string());
        }
    }
    None
}

/// What a statement does: its verb and the table it acts on. A `WITH`
/// statement is classed by the write it ends in, a plugin's own SQL (whose
/// text this role may not read) as `PLUGIN`.
fn class_of(query: &str) -> String {
    let text = text_of(query);
    if text.starts_with('<') {
        return "PLUGIN ?".into();
    }
    let verb_of = |text: &str| -> Option<String> {
        let upper = text.to_ascii_uppercase();
        let (verb, keyword) = if upper.starts_with("INSERT") {
            ("INSERT", "INTO")
        } else if upper.starts_with("UPDATE") {
            return Some(format!(
                "UPDATE {}",
                text.split(' ').nth(1)?.trim_matches('"')
            ));
        } else if upper.starts_with("DELETE") {
            ("DELETE", "FROM")
        } else if upper.starts_with("SELECT") {
            ("SELECT", "FROM")
        } else {
            return None;
        };
        Some(format!("{verb} {}", top_level_after(text, keyword)?))
    };
    if text.to_ascii_uppercase().starts_with("WITH") {
        // The statement after the last CTE: `WITH a AS (…), b AS (…) <main>`.
        let words: Vec<&str> = text.split(' ').collect();
        let mut depth = 0i32;
        for (i, word) in words.iter().enumerate() {
            if depth == 0
                && i > 0
                && ["SELECT", "INSERT", "UPDATE", "DELETE"]
                    .contains(&word.to_ascii_uppercase().as_str())
                && let Some(class) = verb_of(&words[i..].join(" "))
            {
                return class;
            }
            depth += word.matches('(').count() as i32 - word.matches(')').count() as i32;
        }
    }
    verb_of(&text).unwrap_or(text)
}

/// The work between two snapshots, per class: rows a write affected, calls a
/// read made. Periodic statements are left out.
fn work_since(
    before: &HashMap<String, (i64, i64)>,
    after: &HashMap<String, (i64, i64)>,
) -> BTreeMap<String, i64> {
    let mut work = BTreeMap::new();
    for (query, (calls, rows)) in after {
        let (calls_before, rows_before) = before.get(query).copied().unwrap_or((0, 0));
        let (calls, rows) = (calls - calls_before, rows - rows_before);
        let text = text_of(query);
        if calls == 0
            || PERIODIC.iter().any(|p| text.starts_with(p))
            || text == common::clock::SAMPLE_QUERY
        {
            continue;
        }
        let class = class_of(query);
        let amount = if class.starts_with("SELECT") || class.starts_with("PLUGIN") {
            calls
        } else {
            rows
        };
        *work.entry(class).or_insert(0) += amount;
    }
    work.retain(|_, n| *n != 0);
    work
}

/// `large` minus `small`, per class.
fn difference(
    large: &BTreeMap<String, i64>,
    small: &BTreeMap<String, i64>,
) -> BTreeMap<String, i64> {
    let mut diff = large.clone();
    for (class, n) in small {
        *diff.entry(class.clone()).or_insert(0) -= n;
    }
    diff.retain(|_, n| *n != 0);
    diff
}

fn print_work(label: &str, work: &BTreeMap<String, i64>) {
    println!("--- {label}");
    for (class, n) in work {
        println!("{n:>7}  {class}");
    }
}

/// A testlib checker that accepts everything, so both checkers accept
/// [`super::CPP_SUM`] on every case.
/// It reads the output to its end: testlib refuses an `_ok` that leaves
/// output unread.
const CHECKER: &str = "#include \"testlib.h\"\nint main(int c, char* v[]) { registerTestlibCmd(c, v); while (!ouf.seekEof()) ouf.readWord(); quitf(_ok, \"ok\"); }\n";

/// Waits until submission `id` is judged and every judging session has
/// ended, reading its status with a statement the counts leave out
/// ([`WAIT_MARK`]). A submission reads `Judged` once its judgement is
/// finalized, before the session runs its after-judging hooks, so the
/// session's end is what bounds the work a count covers.
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
            app.judging_idle().await;
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "submission {id} still {status}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Judges two submissions over `cases` test cases and returns the work the
/// second did, from submit to its last verdict. Every case must be accepted,
/// so the count is of cases judged, not of cases a first wrong answer
/// skipped.
async fn judge(
    app: &E2eTestApp,
    admin: &str,
    checker: &str,
    cases: usize,
) -> BTreeMap<String, i64> {
    let problem_id = app
        .create_problem_with_checker_format(admin, &format!("Count {checker} {cases}"), checker)
        .await;
    if checker == "testlib" {
        let res = app
            .put_with_token(
                &format!("/api/v1/problems/{problem_id}/checker-source"),
                &serde_json::json!({
                    "files": [
                        { "filename": "checker.cpp", "content": CHECKER },
                        { "filename": "testlib.h", "content": super::testlib_checker::TESTLIB_H },
                    ]
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
    let mut measured = BTreeMap::new();
    for _ in 0..2 {
        let before = snapshot(app).await;
        let sub = app
            .create_submission(problem_id, admin, "cpp", super::CPP_SUM)
            .await;
        wait_judged_unmeasured(app, sub).await;
        measured = work_since(&before, &snapshot(app).await);
        let res = app
            .get_with_token(&format!("/api/v1/submissions/{sub}"), admin)
            .await;
        assert_eq!(res.body["status"], "Judged", "{}", res.text);
        // ICPC accepts a submission only when every case is accepted.
        assert_eq!(res.body["result"]["verdict"], "Accepted", "{}", res.text);
        assert_eq!(
            res.body["result"]["test_case_pagination"]["total"], cases,
            "{}",
            res.text
        );
    }
    measured
}

/// The platform clock resamples on a timer of its own, so a burst can land
/// inside any measurement; what it runs is left out by construction.
#[tokio::test(flavor = "multi_thread")]
async fn a_clock_resample_costs_no_measured_work() {
    if std::env::var("E2E_SERVER_URL").is_ok() {
        return;
    }
    let app = E2eTestApp::spawn().await;
    let before = snapshot(&app).await;
    app.resample_clock().await;
    app.resample_clock().await;
    let after = snapshot(&app).await;
    let sampled = after
        .iter()
        .filter(|(query, _)| text_of(query) == common::clock::SAMPLE_QUERY)
        .map(|(_, (calls, _))| calls)
        .sum::<i64>();
    assert!(sampled > 0, "the resample ran its sample query");
    assert_eq!(work_since(&before, &after), BTreeMap::new());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_test_case_and_a_poll_cost_exactly_their_work() {
    if std::env::var("E2E_SERVER_URL").is_ok() {
        // A remote stack's database is shared with whatever else runs on it.
        return;
    }
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("counter", "pass1234", "admin")
        .await;

    // Twenty more judged cases cost twenty result rows, and nothing else:
    // the problem and its additional files are read once per judging
    // session, and the worker's compile cache serves every case after the
    // first.
    let per_twenty_cases = BTreeMap::from([("INSERT test_case_result".to_string(), 20)]);
    for checker in ["exact", "testlib"] {
        let small = judge(&app, &admin, checker, 5).await;
        let large = judge(&app, &admin, checker, 25).await;
        print_work(
            &format!("submission of 5 cases ({checker} checker)"),
            &small,
        );
        print_work(
            &format!("submission of 25 cases ({checker} checker)"),
            &large,
        );
        assert_eq!(
            difference(&large, &small),
            per_twenty_cases,
            "the work 20 more cases cost ({checker} checker)"
        );
    }

    let problem_id = app.create_problem(&admin, "Poll").await;
    app.create_test_case(problem_id, &admin).await;
    let sub = app
        .create_submission(problem_id, &admin, "cpp", super::CPP_SUM)
        .await;
    wait_judged_unmeasured(&app, sub).await;
    const POLLS: i64 = 50;
    let before = snapshot(&app).await;
    for _ in 0..POLLS {
        let res = app
            .get_with_token(&format!("/api/v1/submissions/{sub}"), &admin)
            .await;
        assert_eq!(res.status, 200, "{}", res.text);
    }
    let polls = work_since(&before, &snapshot(&app).await);
    print_work(&format!("{POLLS} polls"), &polls);
    // A poll reads the submission, its results and the view context (its
    // author, problem and contest, in one statement).
    assert_eq!(
        polls,
        BTreeMap::from([
            ("SELECT submission".to_string(), POLLS),
            ("SELECT test_case_result".to_string(), POLLS),
            ("SELECT user".to_string(), POLLS),
        ]),
        "the work {POLLS} polls cost"
    );
}
