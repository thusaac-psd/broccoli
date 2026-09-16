//! Self-test for `TestApp::create_user_with_permissions`.
//!
//! The integration harness historically only had `create_user_with_role`,
//! whose seeded roles (`admin`, `problem_setter`, `contestant`) each bundle
//! several permissions together. That made it structurally impossible for an
//! integration-level test to prove which single permission actually gates a
//! given code path -- a test asserting `admin` can do X can never rule out
//! that some OTHER permission `admin` also happens to carry is the real
//! gate. `create_user_with_permissions` (mirrored from the equivalent e2e
//! fixture in `tests/e2e/common/mod.rs`) closes that gap by minting a
//! brand-new role carrying exactly the requested permissions.
//!
//! This test pins the fixture's core guarantee: a user created with exactly
//! one named permission does not also hold unrelated permissions that a
//! stock role would normally bundle alongside it.

use broccoli_server_sdk::permissions as perm;

use crate::common::{TestApp, routes};

#[tokio::test]
async fn single_permission_fixture_grants_exactly_one_permission() {
    let app = TestApp::spawn().await;

    let token = app
        .create_user_with_permissions("rejudge_only", "password123", &[perm::SUBMISSION_REJUDGE])
        .await;

    let me = app.get_with_token(routes::ME, &token).await;
    assert_eq!(me.status, 200, "GET /auth/me failed: {}", me.text);

    let permissions: Vec<String> = me.body["permissions"]
        .as_array()
        .expect("permissions should be an array")
        .iter()
        .map(|v| {
            v.as_str()
                .expect("permission should be a string")
                .to_string()
        })
        .collect();

    assert_eq!(
        permissions,
        vec![perm::SUBMISSION_REJUDGE.to_string()],
        "fixture should grant exactly the requested permission and nothing else"
    );
    assert!(
        !permissions.contains(&perm::SUBMISSION_VIEW_ALL.to_string()),
        "fixture leaked submission:view_all onto a submission:rejudge-only user"
    );
    assert!(
        !permissions.contains(&perm::CONTEST_MANAGE.to_string()),
        "fixture leaked contest:manage onto a submission:rejudge-only user"
    );
}
