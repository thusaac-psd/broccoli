//! Binary test data is judged byte for byte end to end: a ZIP upload keeps a
//! test case's non-UTF-8 input and expected output as blobs, the worker hands
//! the input to the program and the comparator reads its output as bytes.
//! Runs under the mock sandbox, which runs the program on the host.

use std::io::Write;

use crate::common::E2eTestApp;

/// Copies standard input to standard output, byte for byte.
const CPP_COPY: &str = r#"
#include <cstdio>
int main() {
    char buf[4096];
    size_t n;
    while ((n = fread(buf, 1, sizeof buf, stdin)) > 0) fwrite(buf, 1, n, stdout);
    return 0;
}
"#;

/// Copies standard input but flips the first byte.
const CPP_FLIP_FIRST: &str = r#"
#include <cstdio>
#include <vector>
int main() {
    std::vector<unsigned char> data;
    int c;
    while ((c = getchar()) != EOF) data.push_back((unsigned char)c);
    if (!data.empty()) data.front() ^= 1;
    fwrite(data.data(), 1, data.size(), stdout);
    return 0;
}
"#;

/// Bytes that are not UTF-8, with a NUL, around whitespace and newlines.
const BINARY: &[u8] = b"\x00\xff\xfe tok\x80en\n\xc3\x28 \x01\x02\n";

fn zip_of(input: &[u8], answer: &[u8]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, content) in [("01.in", input), ("01.ans", answer)] {
        zip.start_file(name, options).unwrap();
        zip.write_all(content).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

async fn verdict_of(app: &E2eTestApp, admin: &str, format: &str, program: &str) -> String {
    let problem_id = app
        .create_problem_with_checker_format(admin, &format!("Binary {format}"), format)
        .await;
    let res = app
        .upload_with_token(
            &format!("/api/v1/problems/{problem_id}/test-cases/upload"),
            "tests.zip",
            zip_of(BINARY, BINARY),
            Some("*.in"),
            Some("*.ans"),
            Some("replace"),
            admin,
        )
        .await;
    assert_eq!(res.status, 201, "upload: {}", res.text);

    let sub_id = app
        .create_submission(problem_id, admin, "cpp", program)
        .await;
    let res = app.wait_for_submission_terminal(sub_id, admin, 120).await;
    assert_eq!(res.body["status"], "Judged", "{format}: {}", res.text);
    let verdict = res.body["result"]["verdict"].as_str().unwrap_or_default();
    if verdict == "SystemError" {
        panic!("{format}: {}", res.text);
    }
    verdict.to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn binary_test_data_is_judged_byte_for_byte() {
    let app = E2eTestApp::spawn().await;
    let admin = app
        .create_user_with_role("binary_data_admin", "pass1234", "admin")
        .await;
    for format in ["exact", "lines", "tokens"] {
        assert_eq!(
            verdict_of(&app, &admin, format, CPP_COPY).await,
            "Accepted",
            "{format}: the same bytes"
        );
        assert_eq!(
            verdict_of(&app, &admin, format, CPP_FLIP_FIRST).await,
            "WrongAnswer",
            "{format}: one byte differs"
        );
    }
}
