//! End-to-end proof that testlib (special-judge) judging still works after the
//! checker source moved off `problem.checker_source` onto the checker plugin's
//! problem-scoped config (`standard-checkers:checker_source`).
//!
//! The chain under test:
//!   1. `PUT /problems/{id}/checker-source` writes plugin_config(scope=problem,
//!      ns="standard-checkers:checker_source") -- the ONLY place the source now
//!      lives.
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
//! Judges in a real isolate sandbox with a C++ toolchain, like the other
//! judging verdict tests; `common::require_real_sandbox` says how to run it.

use crate::common::{E2eTestApp, require_real_sandbox};

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
const TESTLIB_H: &str = include_str!("../../fixtures/testlib/testlib.h");

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
    let res = app
        .put_with_token(
            &format!("/api/v1/problems/{problem_id}/checker-source"),
            &serde_json::json!({
                "files": [
                    { "filename": "checker.cpp", "content": WCMP_CHECKER },
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

/// One problem with a real testlib checker judges both an accepting and a
/// rejecting solution correctly, end to end, entirely through the new
/// problem-scoped `standard-checkers:checker_source` config path.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges real programs in isolate; the sandbox CI job runs it (see common::require_real_sandbox)"]
async fn testlib_checker_judges_via_problem_config() {
    require_real_sandbox();

    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("v_testlib_admin", "pass1234", "admin")
        .await;

    // Problem uses the testlib special judge; the exact comparator is NOT used.
    let problem_id = app
        .create_problem_with_checker_format(&admin, "Testlib Sum", "testlib")
        .await;

    // Write the checker source into standard-checkers:checker_source (problem
    // scope). This is the write half of the path under test.
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
