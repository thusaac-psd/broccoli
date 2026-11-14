//! `server::bundle`: a contest exported from one server restores onto another
//! with every row and blob, accounts merge by username without touching the
//! target's credentials or roles, and an export never writes a partial
//! archive.

use std::sync::Arc;

use common::storage::BlobStore;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use server::bundle::{ExportOptions, ImportOptions, Selection};

use crate::common::TestApp;

async fn text(app: &TestApp, sql: &str) -> String {
    app.db
        .query_one_raw(Statement::from_string(DbBackend::Postgres, sql))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
        .expect("one row")
        .try_get::<String>("", "v")
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

async fn exec(app: &TestApp, sql: &str) {
    app.db
        .execute_unprepared(sql)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

/// Every row of `table`, as JSON, in key order.
async fn rows(app: &TestApp, table: &str, order: &str) -> String {
    text(
        app,
        &format!("SELECT coalesce(json_agg(t ORDER BY {order}), '[]')::text AS v FROM {table} t"),
    )
    .await
}

async fn export(app: &TestApp, options: ExportOptions) -> anyhow::Result<Vec<u8>> {
    let mut archive = Vec::new();
    server::bundle::export(&app.db, &*app.state.blob_store, &options, &mut archive).await?;
    Ok(archive)
}

async fn import(app: &TestApp, archive: &[u8], options: ImportOptions) -> anyhow::Result<()> {
    let store: Arc<dyn BlobStore> = app.state.blob_store.clone();
    server::bundle::import(&app.db, store, options, archive)
        .await
        .map(|_| ())
}

/// A source server holding one contest: a problem with an inline and a
/// blob-backed test case, an attachment, an additional file, problem and
/// contest plugin config, and a roster of two accounts, one with a custom
/// role. Returns the contest id and the blob's hash.
async fn seed_contest(app: &TestApp) -> (i32, String) {
    let admin = app
        .create_user_with_role("bundle_admin", "pass1234", "admin")
        .await;
    let problem = app.create_problem(&admin, "A + B").await;
    app.create_test_case(problem, &admin).await;
    let attachment = app
        .upload_attachment(problem, "notes.txt", b"read me".to_vec(), None, &admin)
        .await;
    assert_eq!(attachment.status, 201, "{}", attachment.text);
    let extra = app
        .upload_additional_file(
            problem,
            "grader.cpp",
            b"int main(){}".to_vec(),
            "cpp",
            &admin,
        )
        .await;
    assert_eq!(extra.status, 201, "{}", extra.text);

    let blob = app
        .state
        .blob_store
        .put(b"1 2\n3 4\n")
        .await
        .expect("store a test case body")
        .to_hex();
    exec(
        app,
        &format!(
            "INSERT INTO test_case (input, expected_output, input_blob_hash, input_size, \
               score, label, is_sample, position, problem_id, created_at) \
             VALUES ('', '3', '{blob}', 8, 10, 'big', false, 1, {problem}, now())"
        ),
    )
    .await;

    let contest = app.create_contest(&admin, "Bundle Cup", true, false).await;
    app.add_problem_to_contest(contest, problem, &admin).await;
    exec(
        app,
        &format!(
            "INSERT INTO plugin_config (scope, ref_id, namespace, config, position, updated_at) VALUES \
             ('problem', '{problem}', 'checker', '{{\"tolerance\": 1}}', 0, now()), \
             ('contest', '{contest}', 'scoring', '{{\"freeze\": 60}}', 0, now())"
        ),
    )
    .await;

    for name in ["carol", "dave"] {
        let token = app
            .create_user_with_role(name, "source-password", "contestant")
            .await;
        app.register_for_contest(contest, &token).await;
    }
    exec(
        app,
        "UPDATE \"user\" SET display_name = 'Carol C.' WHERE username = 'carol'; \
         INSERT INTO role (name) VALUES ('volunteer'); \
         INSERT INTO role_permission (role, permission) VALUES \
           ('volunteer', 'contest:manage'), ('contestant', 'problem:manage'); \
         INSERT INTO user_role (user_id, role) SELECT id, 'volunteer' FROM \"user\" \
           WHERE username = 'dave'",
    )
    .await;
    (contest, blob)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_contest_bundle_restores_every_row_and_blob_onto_another_server() {
    let source = TestApp::spawn().await;
    let (contest, blob) = seed_contest(&source).await;
    let archive = export(
        &source,
        ExportOptions {
            selection: Selection::Contest(contest),
            with_secrets: false,
        },
    )
    .await
    .expect("export");

    let target = TestApp::spawn().await;
    // The target already has an active `carol`, with its own password and id.
    target
        .create_user_with_role("someone_first", "pass1234", "contestant")
        .await;
    target
        .create_user_with_role("carol", "target-password", "contestant")
        .await;
    let carol_before = text(
        &target,
        "SELECT id::text || ' ' || password AS v FROM \"user\" WHERE username = 'carol'",
    )
    .await;

    import(&target, &archive, ImportOptions::default())
        .await
        .expect("import");

    for (table, order) in [
        ("problem", "id"),
        ("test_case", "id"),
        ("problem_attachment", "id"),
        ("additional_file", "id"),
        ("contest", "id"),
        ("contest_problem", "contest_id, problem_id"),
        ("plugin_config", "scope, ref_id, namespace"),
    ] {
        assert_eq!(
            rows(&target, table, order).await,
            rows(&source, table, order).await,
            "{table} differs after the round trip"
        );
    }

    // The roster follows the accounts by username.
    let roster = "SELECT string_agg(u.username, ',' ORDER BY u.username) AS v \
                  FROM contest_user cu JOIN \"user\" u ON u.id = cu.user_id";
    assert_eq!(text(&target, roster).await, "carol,dave");
    // An existing account keeps its id and credentials.
    assert_eq!(
        text(
            &target,
            "SELECT id::text || ' ' || password AS v FROM \"user\" WHERE username = 'carol'"
        )
        .await,
        carol_before
    );
    // A new account carries its profile and no password.
    assert_eq!(
        text(
            &target,
            "SELECT display_name || '|' || password AS v FROM \"user\" WHERE username = 'dave'"
        )
        .await,
        "dave|"
    );
    // A role the target lacked comes with its permissions; a role it had
    // gains none.
    assert_eq!(
        text(
            &target,
            "SELECT string_agg(r.role || ':' || r.permission, ',') AS v FROM role_permission r \
             WHERE r.role = 'volunteer' OR r.permission = 'problem:manage'"
        )
        .await,
        "volunteer:contest:manage"
    );
    assert_eq!(
        text(
            &target,
            "SELECT string_agg(ur.role, ',' ORDER BY ur.role) AS v FROM user_role ur \
             JOIN \"user\" u ON u.id = ur.user_id WHERE u.username = 'dave'"
        )
        .await,
        "contestant,volunteer"
    );

    let hash = common::storage::ContentHash::from_hex(&blob).unwrap();
    assert_eq!(
        target.state.blob_store.get(&hash).await.expect("blob"),
        b"1 2\n3 4\n"
    );

    // New rows on the target do not collide with the restored ids.
    let after = target
        .create_user_with_role("admin2", "pass1234", "admin")
        .await;
    let problem = target.create_problem(&after, "Next").await;
    assert!(problem > 0);

    // The target now has problems that contest links reference.
    let refused = import(&target, &archive, ImportOptions::default())
        .await
        .expect_err("problems exist");
    assert!(
        refused.to_string().contains("--replace-problems"),
        "{refused}"
    );
    let refused = import(
        &target,
        &archive,
        ImportOptions {
            replace_problems: true,
            wipe_dependents: false,
        },
    )
    .await
    .expect_err("contest links reference the problems");
    assert!(refused.to_string().contains("contest_problem"), "{refused}");
    import(
        &target,
        &archive,
        ImportOptions {
            replace_problems: true,
            wipe_dependents: true,
        },
    )
    .await
    .expect("replace");
    assert_eq!(
        rows(&target, "problem", "id").await,
        rows(&source, "problem", "id").await
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_export_with_a_missing_blob_writes_nothing() {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_role("bundle_admin", "pass1234", "admin")
        .await;
    let problem = app.create_problem(&admin, "Lost").await;
    let absent = "ab".repeat(32);
    exec(
        &app,
        &format!(
            "INSERT INTO test_case (input, expected_output, input_blob_hash, input_size, \
               score, label, is_sample, position, problem_id, created_at) \
             VALUES ('', '', '{absent}', 1, 0, 'gone', false, 0, {problem}, now())"
        ),
    )
    .await;
    let mut archive = Vec::new();
    let err = server::bundle::export(
        &app.db,
        &*app.state.blob_store,
        &ExportOptions {
            selection: Selection::Problems(vec![problem]),
            with_secrets: false,
        },
        &mut archive,
    )
    .await
    .expect_err("a missing blob fails the export");
    assert!(err.to_string().contains(&absent), "{err}");
    assert!(
        archive.is_empty(),
        "a failed export wrote {} bytes",
        archive.len()
    );
}
