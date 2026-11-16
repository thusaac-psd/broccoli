//! End-to-end proof that testlib (special-judge) judging works with the
//! checker source in the checker plugin's problem-scoped config: the
//! namespace the plugin providing the `testlib` format declares
//! `checker_source` in its manifest.
//!
//! The chain under test:
//!   1. `PUT /problems/{id}/checker-source` writes plugin_config(scope=problem)
//!      in that namespace -- the ONLY place the source lives.
//!   2. At judge time, standard-checkers `resolve_standard_checker` reads it back
//!      via `host.config.get_problem(id, "checker_source")`, compiles the testlib
//!      checker in the sandbox, runs it, and maps its exit code to a verdict.
//!
//! A real `Accepted` proves the WHOLE chain: the upload wrote the config, the
//! plugin read it, a real C++ testlib checker compiled + ran, and exit 0 mapped
//! to Accepted. A real `WrongAnswer` proves exit 1 mapped correctly. If the new
//! config read were broken, `load_checker_source` would error and the verdict
//! would be `SystemError`, not a clean Accepted/WrongAnswer -- so these
//! assertions genuinely fail if the path regresses.
//!
//! The checker and the solutions compile with the host's C++ toolchain, so the
//! verdicts are real under the mock sandbox as well as under isolate.

use crate::common::E2eTestApp;

/// The canonical testlib `wcmp` checker: compare the jury answer (`ans`) and the
/// contestant output (`ouf`) as sequences of whitespace-separated tokens; AC iff
/// the token streams are identical, WA on the first differing token. A trailing
/// newline is tolerated (`seekEof` skips whitespace). This is a REAL testlib
/// checker -- it `#include`s the real testlib.h uploaded alongside it.
const WCMP_CHECKER: &str = r#"#include "testlib.h"
using namespace std;

int main(int argc, char* argv[]) {
    setName("compare sequences of tokens");
    registerTestlibCmd(argc, argv);

    int n = 0;
    string j, p;
    while (!ans.seekEof() && !ouf.seekEof()) {
        n++;
        j = ans.readWord();
        p = ouf.readWord();
        if (j != p)
            quitf(_wa, "%d%s words differ - expected: '%s', found: '%s'",
                  n, englishEnding(n).c_str(), compress(j).c_str(), compress(p).c_str());
    }

    if (ans.seekEof() && ouf.seekEof()) {
        if (n == 1)
            quitf(_ok, "\"%s\"", compress(j).c_str());
        else
            quitf(_ok, "%d tokens", n);
    } else {
        if (ans.seekEof())
            quitf(_wa, "Participant output contains extra tokens");
        else
            quitf(_wa, "Unexpected EOF in the participants output");
    }
}
"#;

/// Real, unmodified `testlib.h` (github.com/MikeMirzayanov/testlib), vendored as
/// a fixture so the checker genuinely compiles in the sandbox. `include_str!`
/// keeps the test hermetic (no network at judge time).
pub(super) const TESTLIB_H: &str = include_str!("../../fixtures/testlib/testlib.h");

/// Sums `n` integers -> for input "5\n1 2 3 4 5" prints "15": the checker
/// accepts it (tokens equal the answer "15").
const SOLUTION_ACCEPTED: &str = r#"
#include <iostream>
int main() {
    int n;
    std::cin >> n;
    long long sum = 0;
    for (int i = 0; i < n; i++) {
        int x;
        std::cin >> x;
        sum += x;
    }
    std::cout << sum << std::endl;
    return 0;
}
"#;

/// Always prints "0": the checker's first token differs from "15" -> WrongAnswer.
const SOLUTION_WRONG: &str = r#"
#include <iostream>
int main() {
    std::cout << 0 << std::endl;
    return 0;
}
"#;

/// Upload the checker bundle (checker.cpp + testlib.h) to the problem via the
/// dedicated checker-source endpoint, which writes the problem-scoped plugin
/// config the resolver reads back at judge time.
async fn upload_checker_source(app: &E2eTestApp, problem_id: i32, token: &str) {
    upload_checker(app, problem_id, token, WCMP_CHECKER).await;
}

/// Upload `checker` (with testlib.h) as the problem's checker source.
async fn upload_checker(app: &E2eTestApp, problem_id: i32, token: &str, checker: &str) {
    let res = app
        .put_with_token(
            &format!("/api/v1/problems/{problem_id}/checker-source"),
            &serde_json::json!({
                "files": [
                    { "filename": "checker.cpp", "content": checker },
                    { "filename": "testlib.h", "content": TESTLIB_H },
                ]
            }),
            token,
        )
        .await;
    assert_eq!(
        res.status, 200,
        "checker-source upload should succeed: {}",
        res.text
    );
}

/// The host stores a checker source in the namespace the checker format's
/// plugin declares `checker_source`, as `{ "files": [...] }`, serves it back,
/// keeps it out of the plugin's config schemas, and clears it.
#[tokio::test(flavor = "multi_thread")]
async fn a_checker_source_lives_in_the_namespace_its_checker_plugin_declares() {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};

    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("v_checker_source_admin", "pass1234", "admin")
        .await;
    let problem_id = app
        .create_problem_with_checker_format(&admin, "Checker Source Home", "testlib")
        .await;
    upload_checker_source(&app, problem_id, &admin).await;

    let row = app
        .db
        .query_one_raw(Statement::from_string(
            DbBackend::Postgres,
            format!(
                "SELECT namespace, config FROM plugin_config \
                 WHERE scope = 'problem' AND ref_id = '{problem_id}'"
            ),
        ))
        .await
        .unwrap()
        .expect("the checker source is stored");
    let namespace: String = row.try_get("", "namespace").unwrap();
    let config: serde_json::Value = row.try_get("", "config").unwrap();
    let (plugin_id, raw) = namespace.split_once(':').expect("plugin:namespace");
    assert_eq!(raw, "checker_source", "{namespace}");
    assert_eq!(
        config["files"].as_array().map(Vec::len),
        Some(2),
        "{config}"
    );

    let plugins = app.get_with_token("/api/v1/admin/plugins", &admin).await;
    assert_eq!(plugins.status, 200, "{}", plugins.text);
    let owner = plugins
        .body
        .as_array()
        .and_then(|list| list.iter().find(|p| p["id"] == plugin_id))
        .expect("the owning plugin is listed");
    let schemas = owner["config_schemas"].as_array().expect("config schemas");
    assert!(
        schemas.iter().all(|s| s["namespace"] != "checker_source"),
        "the checker source is not offered as a config: {owner}"
    );

    let cleared = app
        .delete_with_token(
            &format!("/api/v1/problems/{problem_id}/checker-source"),
            &admin,
        )
        .await;
    assert_eq!(cleared.status, 204, "{}", cleared.text);
    let got = app
        .get_with_token(
            &format!("/api/v1/problems/{problem_id}/checker-source"),
            &admin,
        )
        .await;
    assert_eq!(got.status, 200, "{}", got.text);
    assert!(got.body["files"].is_null(), "{}", got.text);
}

/// One problem with a real testlib checker judges both an accepting and a
/// rejecting solution correctly, end to end, entirely through the checker
/// plugin's problem-scoped config.
#[tokio::test(flavor = "multi_thread")]
async fn testlib_checker_judges_via_problem_config() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("v_testlib_admin", "pass1234", "admin")
        .await;

    // Problem uses the testlib special judge; the exact comparator is NOT used.
    let problem_id = app
        .create_problem_with_checker_format(&admin, "Testlib Sum", "testlib")
        .await;

    // Write the checker source into the checker plugin's checker_source
    // namespace (problem scope). This is the write half of the path under test.
    upload_checker_source(&app, problem_id, &admin).await;

    // Sanity: the upload actually persisted the config the resolver will read.
    let got = app
        .get_with_token(
            &format!("/api/v1/problems/{problem_id}/checker-source"),
            &admin,
        )
        .await;
    assert_eq!(got.status, 200, "get checker-source: {}", got.text);
    let files = got.body["files"].as_array().expect("files array");
    assert_eq!(files.len(), 2, "both bundle files persisted: {}", got.text);
    assert!(
        files
            .iter()
            .any(|f| f["filename"] == "checker.cpp" && f["content"] == WCMP_CHECKER),
        "checker.cpp round-trips: {}",
        got.text
    );

    // Answer is "15"; the testlib checker (not an exact match) decides the verdict.
    app.create_test_case_with(problem_id, "5\n1 2 3 4 5", "15", 100, true, &admin)
        .await;

    // (d) A solution the checker ACCEPTS -> Accepted (exit 0 from the compiled
    // testlib checker, read from the problem-scoped config).
    let ac_id = app
        .create_submission(problem_id, &admin, "cpp", SOLUTION_ACCEPTED)
        .await;
    let ac = app.wait_for_submission_terminal(ac_id, &admin, 120).await;
    eprintln!(
        "[testlib e2e] AC submission -> status={} verdict={}",
        ac.body["status"], ac.body["result"]["verdict"]
    );
    assert_eq!(
        ac.body["status"], "Judged",
        "AC submission should reach Judged (not SystemError, which would mean the \
         checker_source config read failed): {}",
        ac.text
    );
    assert_eq!(
        ac.body["result"]["verdict"], "Accepted",
        "testlib checker should accept the correct solution: {}",
        ac.text
    );

    // (e) A solution the checker REJECTS -> WrongAnswer (exit 1). Reuses the same
    // problem-scoped checker source (and the cached compiled checker).
    let wa_id = app
        .create_submission(problem_id, &admin, "cpp", SOLUTION_WRONG)
        .await;
    let wa = app.wait_for_submission_terminal(wa_id, &admin, 120).await;
    eprintln!(
        "[testlib e2e] WA submission -> status={} verdict={} checker_output={}",
        wa.body["status"],
        wa.body["result"]["verdict"],
        wa.body["result"]["test_case_results"][0]["checker_output"]
    );
    assert_eq!(
        wa.body["status"], "Judged",
        "WA submission should reach Judged: {}",
        wa.text
    );
    assert_eq!(
        wa.body["result"]["verdict"], "WrongAnswer",
        "testlib checker should reject the wrong solution: {}",
        wa.text
    );
}

/// A testlib checker that reads its files and then crashes.
const ABORTING_CHECKER: &str = r#"#include "testlib.h"
#include <cstdlib>

int main(int argc, char* argv[]) {
    registerTestlibCmd(argc, argv);
    ans.readWord();
    std::abort();
}
"#;

/// A checker that crashes is the problem's fault, which every attempt
/// repeats: the submission's system error stands at once under
/// `CHECKER_FAILED`, judged once and never retried. Staff see how the checker
/// failed (the signal); the contestant sees only the system error, with the
/// judgement id to quote.
#[tokio::test(flavor = "multi_thread")]
async fn a_crashing_checker_stands_as_checker_failed_once_with_its_signal_for_staff_only() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("crash_checker_admin", "pass1234", "admin")
        .await;
    let problem_id = app
        .create_problem_with_checker_format(&admin, "Crashing checker", "testlib")
        .await;
    upload_checker(&app, problem_id, &admin, ABORTING_CHECKER).await;
    app.create_test_case_with(problem_id, "5\n1 2 3 4 5", "15", 100, true, &admin)
        .await;
    let res = app
        .patch_with_token(
            &format!("/api/v1/problems/{problem_id}"),
            &serde_json::json!({"is_public": true}),
            &admin,
        )
        .await;
    assert!(res.status < 300, "{}", res.text);
    let contestant = app
        .create_authenticated_user("crash_checker_user", "pass1234")
        .await;

    let id = app
        .create_submission(problem_id, &contestant, "cpp", SOLUTION_ACCEPTED)
        .await;
    let seen = app.wait_for_submission_terminal(id, &contestant, 120).await;
    app.judging_idle().await;

    let staff = app
        .get_with_token(&format!("/api/v1/submissions/{id}"), &admin)
        .await;
    assert_eq!(staff.status, 200, "{}", staff.text);
    let result = &staff.body["result"];
    assert_eq!(result["verdict"], "SystemError", "{}", staff.text);
    assert_eq!(result["error_code"], "CHECKER_FAILED", "{}", staff.text);
    let diagnostic = result["error_message"].as_str().unwrap_or_default();
    assert!(
        diagnostic.contains("signal 6 (SIGABRT)"),
        "staff see how the checker failed: {}",
        staff.text
    );

    let result = &seen.body["result"];
    assert_eq!(result["verdict"], "SystemError", "{}", seen.text);
    assert!(result["error_code"].is_null(), "{}", seen.text);
    assert!(result["error_message"].is_null(), "{}", seen.text);
    assert!(result["judgement_id"].is_i64(), "{}", seen.text);

    use sea_orm::EntityTrait;
    let row = server::entity::submission::Entity::find_by_id(id)
        .one(&app.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.system_error_retries, 0, "never retried");
    assert_eq!(row.judge_epoch, 0, "judged once");
}
