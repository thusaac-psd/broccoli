use crate::common::{E2eTestApp, require_real_sandbox};

const CPP_ACCEPTED: &str = r#"
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

const CPP_WRONG_ANSWER: &str = r#"
#include <iostream>
int main() {
    std::cout << 0 << std::endl;
    return 0;
}
"#;

const CPP_TLE: &str = r#"
int main() {
    volatile int x = 0;
    while (true) { x++; }
    return 0;
}
"#;

const CPP_MLE: &str = r#"
#include <cstdlib>
#include <cstring>
int main() {
    while (true) {
        volatile char* p = (char*)malloc(1024 * 1024);
        if (p) memset((char*)p, 1, 1024 * 1024);
    }
    return 0;
}
"#;

const CPP_RUNTIME_ERROR: &str = r#"
int main() {
    int* p = nullptr;
    *p = 42;
    return 0;
}
"#;

const CPP_COMPILE_ERROR: &str = "this is definitely not valid c++ code {{{";

const PY_ACCEPTED: &str = r#"
n = int(input())
nums = list(map(int, input().split()))
print(sum(nums))
"#;

const PY_WRONG_ANSWER: &str = r#"
print(0)
"#;

const PY_TLE: &str = r#"
while True:
    pass
"#;

const JAVA_ACCEPTED: &str = r#"
import java.util.Scanner;
public class Main {
    public static void main(String[] args) {
        Scanner sc = new Scanner(System.in);
        int n = sc.nextInt();
        long sum = 0;
        for (int i = 0; i < n; i++) sum += sc.nextInt();
        System.out.println(sum);
    }
}
"#;

mod cpp_verdicts {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "judges real programs in isolate; the sandbox CI job runs it (see common::require_real_sandbox)"]
    async fn correct_cpp_gets_accepted() {
        require_real_sandbox();

        let app = E2eTestApp::spawn().await;
        let admin = app
            .create_user_with_role("v_cpp_ac", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin, "Verdict AC").await;
        app.create_test_case(problem_id, &admin).await;

        let sub_id = app
            .create_submission(problem_id, &admin, "cpp", CPP_ACCEPTED)
            .await;
        let res = app.wait_for_submission_terminal(sub_id, &admin, 60).await;

        assert_eq!(
            res.body["status"], "Judged",
            "Should be Judged: {}",
            res.text
        );
        assert_eq!(
            res.body["result"]["verdict"], "Accepted",
            "Correct program should get Accepted: {}",
            res.text
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "judges real programs in isolate; the sandbox CI job runs it (see common::require_real_sandbox)"]
    async fn wrong_output_gets_wrong_answer() {
        require_real_sandbox();

        let app = E2eTestApp::spawn().await;
        let admin = app
            .create_user_with_role("v_cpp_wa", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin, "Verdict WA").await;
        app.create_test_case(problem_id, &admin).await;

        let sub_id = app
            .create_submission(problem_id, &admin, "cpp", CPP_WRONG_ANSWER)
            .await;
        let res = app.wait_for_submission_terminal(sub_id, &admin, 60).await;

        assert_eq!(res.body["status"], "Judged");
        assert_eq!(
            res.body["result"]["verdict"], "WrongAnswer",
            "Wrong output should get WrongAnswer: {}",
            res.text
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "judges real programs in isolate; the sandbox CI job runs it (see common::require_real_sandbox)"]
    async fn infinite_loop_gets_time_limit_exceeded() {
        require_real_sandbox();

        let app = E2eTestApp::spawn().await;
        let admin = app
            .create_user_with_role("v_cpp_tle", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin, "Verdict TLE").await;
        app.create_test_case(problem_id, &admin).await;

        let sub_id = app
            .create_submission(problem_id, &admin, "cpp", CPP_TLE)
            .await;
        let res = app.wait_for_submission_terminal(sub_id, &admin, 120).await;

        assert_eq!(res.body["status"], "Judged");
        assert_eq!(
            res.body["result"]["verdict"], "TimeLimitExceeded",
            "Infinite loop should get TLE: {}",
            res.text
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "judges real programs in isolate; the sandbox CI job runs it (see common::require_real_sandbox)"]
    async fn memory_hog_gets_memory_limit_exceeded() {
        require_real_sandbox();

        let app = E2eTestApp::spawn().await;
        let admin = app
            .create_user_with_role("v_cpp_mle", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin, "Verdict MLE").await;
        app.create_test_case(problem_id, &admin).await;

        let sub_id = app
            .create_submission(problem_id, &admin, "cpp", CPP_MLE)
            .await;
        let res = app.wait_for_submission_terminal(sub_id, &admin, 120).await;

        assert_eq!(res.body["status"], "Judged");
        let verdict = res.body["result"]["verdict"].as_str().unwrap_or("");
        assert!(
            verdict == "MemoryLimitExceeded" || verdict == "RuntimeError",
            "Memory hog should get MLE or RE (OOM kill): got {verdict}, body: {}",
            res.text
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "judges real programs in isolate; the sandbox CI job runs it (see common::require_real_sandbox)"]
    async fn segfault_gets_runtime_error() {
        require_real_sandbox();

        let app = E2eTestApp::spawn().await;
        let admin = app
            .create_user_with_role("v_cpp_re", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin, "Verdict RE").await;
        app.create_test_case(problem_id, &admin).await;

        let sub_id = app
            .create_submission(problem_id, &admin, "cpp", CPP_RUNTIME_ERROR)
            .await;
        let res = app.wait_for_submission_terminal(sub_id, &admin, 60).await;

        assert_eq!(res.body["status"], "Judged");
        assert_eq!(
            res.body["result"]["verdict"], "RuntimeError",
            "Segfault should get RuntimeError: {}",
            res.text
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "judges real programs in isolate; the sandbox CI job runs it (see common::require_real_sandbox)"]
    async fn invalid_syntax_gets_compilation_error() {
        require_real_sandbox();

        let app = E2eTestApp::spawn().await;
        let admin = app
            .create_user_with_role("v_cpp_ce", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin, "Verdict CE").await;
        app.create_test_case(problem_id, &admin).await;

        let sub_id = app
            .create_submission(problem_id, &admin, "cpp", CPP_COMPILE_ERROR)
            .await;
        let res = app.wait_for_submission_terminal(sub_id, &admin, 60).await;

        assert_eq!(
            res.body["status"], "CompilationError",
            "Invalid syntax should get CompilationError: {}",
            res.text
        );
    }
}

mod multi_language {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "judges real programs in isolate; the sandbox CI job runs it (see common::require_real_sandbox)"]
    async fn python_correct_gets_accepted() {
        require_real_sandbox();

        let app = E2eTestApp::spawn().await;
        let admin = app
            .create_user_with_role("v_py_ac", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin, "Verdict Py AC").await;
        app.create_test_case(problem_id, &admin).await;

        let sub_id = app
            .create_submission(problem_id, &admin, "python3", PY_ACCEPTED)
            .await;
        let res = app.wait_for_submission_terminal(sub_id, &admin, 60).await;

        assert_eq!(res.body["status"], "Judged");
        assert_eq!(
            res.body["result"]["verdict"], "Accepted",
            "Correct Python should get Accepted: {}",
            res.text
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "judges real programs in isolate; the sandbox CI job runs it (see common::require_real_sandbox)"]
    async fn python_wrong_gets_wrong_answer() {
        require_real_sandbox();

        let app = E2eTestApp::spawn().await;
        let admin = app
            .create_user_with_role("v_py_wa", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin, "Verdict Py WA").await;
        app.create_test_case(problem_id, &admin).await;

        let sub_id = app
            .create_submission(problem_id, &admin, "python3", PY_WRONG_ANSWER)
            .await;
        let res = app.wait_for_submission_terminal(sub_id, &admin, 60).await;

        assert_eq!(res.body["status"], "Judged");
        assert_eq!(res.body["result"]["verdict"], "WrongAnswer");
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "judges real programs in isolate; the sandbox CI job runs it (see common::require_real_sandbox)"]
    async fn python_tle_gets_time_limit_exceeded() {
        require_real_sandbox();

        let app = E2eTestApp::spawn().await;
        let admin = app
            .create_user_with_role("v_py_tle", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin, "Verdict Py TLE").await;
        app.create_test_case(problem_id, &admin).await;

        let sub_id = app
            .create_submission(problem_id, &admin, "python3", PY_TLE)
            .await;
        let res = app.wait_for_submission_terminal(sub_id, &admin, 120).await;

        assert_eq!(res.body["status"], "Judged");
        assert_eq!(res.body["result"]["verdict"], "TimeLimitExceeded");
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "judges real programs in isolate; the sandbox CI job runs it (see common::require_real_sandbox)"]
    async fn java_correct_gets_accepted() {
        require_real_sandbox();

        let app = E2eTestApp::spawn().await;
        let admin = app
            .create_user_with_role("v_java_ac", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin, "Verdict Java AC").await;
        app.create_test_case(problem_id, &admin).await;

        let sub_id = app
            .create_submission(problem_id, &admin, "java", JAVA_ACCEPTED)
            .await;
        let res = app.wait_for_submission_terminal(sub_id, &admin, 120).await;

        assert_eq!(res.body["status"], "Judged");
        assert_eq!(
            res.body["result"]["verdict"], "Accepted",
            "Correct Java should get Accepted: {}",
            res.text
        );
    }
}

mod stress {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "judges real programs in isolate; the sandbox CI job runs it (see common::require_real_sandbox)"]
    async fn twenty_parallel_submissions_all_complete() {
        require_real_sandbox();

        let app = E2eTestApp::spawn().await;
        let admin = app
            .create_user_with_role("v_stress_admin", "pass1234", "admin")
            .await;
        // Public: twenty plain contestants submit outside any contest.
        let problem_id = app.create_public_problem(&admin, "Stress Problem").await;
        app.create_test_case(problem_id, &admin).await;
        app.create_test_case_with(problem_id, "3\n10 20 30", "60", 10, false, &admin)
            .await;
        app.create_test_case_with(problem_id, "1\n42", "42", 10, false, &admin)
            .await;

        let mut tokens = Vec::new();
        for i in 0..20 {
            let token = app
                .create_authenticated_user(&format!("v_stress_u{i}"), "pass1234")
                .await;
            tokens.push(token);
        }

        let mut sub_ids = Vec::new();
        for (i, token) in tokens.iter().enumerate() {
            let code = if i % 2 == 0 {
                CPP_ACCEPTED
            } else {
                CPP_WRONG_ANSWER
            };
            let sub_id = app.create_submission(problem_id, token, "cpp", code).await;
            sub_ids.push((sub_id, token.clone(), i % 2 == 0));
        }

        let mut handles = Vec::new();
        for (sub_id, token, _expects_ac) in &sub_ids {
            let sub_id = *sub_id;
            let token = token.clone();
            let client = app.client.clone();
            let base_url = app.base_url.clone();
            handles.push(tokio::spawn(async move {
                let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
                loop {
                    let res = client
                        .get(format!("{base_url}/api/v1/submissions/{sub_id}"))
                        .header("Authorization", format!("Bearer {token}"))
                        .send()
                        .await
                        .expect("HTTP request failed");
                    let body: serde_json::Value = res.json().await.expect("JSON parse failed");
                    let status = body["status"].as_str().unwrap_or("");
                    if matches!(status, "Judged" | "CompilationError" | "SystemError") {
                        return (sub_id, body);
                    }
                    assert!(
                        tokio::time::Instant::now() < deadline,
                        "Submission {sub_id} timed out at status: {status}"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                }
            }));
        }

        let mut completed = 0;
        let mut accepted = 0;
        for handle in handles {
            let (sub_id, body) = handle.await.expect("Task panicked");
            let status = body["status"].as_str().unwrap_or("unknown");
            assert!(
                ["Judged", "CompilationError", "SystemError"].contains(&status),
                "Submission {sub_id} not terminal: {status}"
            );
            completed += 1;
            if body["result"]["verdict"].as_str() == Some("Accepted") {
                accepted += 1;
            }
        }

        assert_eq!(completed, 20, "All 20 submissions should complete");

        assert_eq!(accepted, 10, "10 correct submissions should be Accepted");
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "judges real programs in isolate; the sandbox CI job runs it (see common::require_real_sandbox)"]
    async fn mixed_languages_parallel() {
        require_real_sandbox();

        let app = E2eTestApp::spawn().await;
        let admin = app
            .create_user_with_role("v_mixed_admin", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin, "Mixed Lang").await;
        app.create_test_case(problem_id, &admin).await;

        let sub_cpp = app
            .create_submission(problem_id, &admin, "cpp", CPP_ACCEPTED)
            .await;
        let sub_py = app
            .create_submission(problem_id, &admin, "python3", PY_ACCEPTED)
            .await;
        let sub_java = app
            .create_submission(problem_id, &admin, "java", JAVA_ACCEPTED)
            .await;

        let res_cpp = app.wait_for_submission_terminal(sub_cpp, &admin, 120).await;
        let res_py = app.wait_for_submission_terminal(sub_py, &admin, 120).await;
        let res_java = app
            .wait_for_submission_terminal(sub_java, &admin, 120)
            .await;

        for (lang, res) in [("cpp", &res_cpp), ("python3", &res_py), ("java", &res_java)] {
            let status = res.body["status"].as_str().unwrap_or("");
            assert!(
                ["Judged", "CompilationError", "SystemError"].contains(&status),
                "{lang} submission should be terminal, got: {status}"
            );
        }

        assert_eq!(res_cpp.body["result"]["verdict"], "Accepted");
        assert_eq!(res_py.body["result"]["verdict"], "Accepted");
        assert_eq!(res_java.body["result"]["verdict"], "Accepted");
    }
}
