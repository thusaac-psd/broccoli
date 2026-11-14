//! Plugin config reads are cached per server, and a change written through
//! any replica reaches every other replica's cache.

use std::time::Duration;

use sea_orm::ConnectionTrait;

use crate::common::{SpawnOptions, TestApp};

async fn write_config(app: &TestApp, value: i64) {
    app.db
        .execute_unprepared(&format!(
            "INSERT INTO plugin_config (scope, ref_id, namespace, config, position) \
             VALUES ('plugin', 'cache-probe', 'settings', '{{\"v\": {value}}}', 0) \
             ON CONFLICT (scope, ref_id, namespace) DO UPDATE SET config = EXCLUDED.config"
        ))
        .await
        .expect("write plugin_config");
}

async fn cached_value(app: &TestApp) -> Option<i64> {
    app.state
        .plugin_config_cache
        .get(&app.db, "plugin", "cache-probe", "settings")
        .await
        .expect("read plugin_config")
        .and_then(|row| row.config["v"].as_i64())
}

/// Queues one submission on `app`'s database, ready for any replica to claim.
async fn queue_submission(app: &TestApp, user: &str) {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};
    let token = app
        .create_user_with_role(user, "password123", "admin")
        .await;
    let user_id = server::entity::user::Entity::find()
        .filter(server::entity::user::Column::Username.eq(user))
        .one(&app.db)
        .await
        .unwrap()
        .unwrap()
        .id;
    let problem_id = app.create_problem(&token, "Cache probe").await;
    app.insert_submission(server::entity::submission::ActiveModel {
        files: Set(serde_json::json!([{ "filename": "main.cpp", "content": "int main() {}" }])),
        language: Set("cpp".into()),
        user_id: Set(user_id),
        problem_id: Set(problem_id),
        contest_type: Set("standard".into()),
        ..Default::default()
    })
    .await;
}

/// Claims on `replica` without any listener: only the claim statement can
/// tell it about config changes.
async fn claim(replica: &TestApp) -> u32 {
    let server_id = replica.state.config.server.id.clone();
    server::dispatcher::claim::claim_once(&replica.state, &server_id, 32, 256)
        .await
        .expect("claim pass")
        .judgements
        .0
}

/// The exact interleaving the cache must survive: a config change commits,
/// then a submission commits, then a replica that has cached the old config,
/// and has heard no notification, claims that submission.
#[tokio::test]
async fn config_committed_before_a_submission_is_seen_when_it_is_claimed() {
    let a = TestApp::spawn().await;
    let b = a.spawn_replica(SpawnOptions::default()).await;
    write_config(&a, 1).await;
    assert_eq!(cached_value(&b).await, Some(1));
    // Settle the counter B has seen, as a claim with nothing new would.
    queue_submission(&a, "warmup").await;
    assert_eq!(claim(&b).await, 1);
    assert_eq!(cached_value(&b).await, Some(1));

    write_config(&a, 2).await;
    queue_submission(&a, "after_change").await;
    assert_eq!(cached_value(&b).await, Some(1), "nothing told B yet");
    assert_eq!(claim(&b).await, 1);

    assert_eq!(cached_value(&b).await, Some(2));
}

/// With its fallback tick an hour away and nothing queued, only the change
/// notification can reach the listening replica.
#[tokio::test]
async fn a_change_reaches_a_listening_replica_by_notification() {
    let a = TestApp::spawn().await;
    let b = a.spawn_replica(SpawnOptions::default()).await;
    let (cancel, cancelled) = tokio::sync::watch::channel(false);
    let fiber = tokio::spawn(server::dispatcher::claim::run_with_fallback_tick(
        b.state.clone(),
        b.state.config.server.id.clone(),
        Duration::from_secs(3_600),
        cancelled,
    ));
    b.wait_until_claim_listening().await;
    write_config(&a, 1).await;
    assert_eq!(cached_value(&b).await, Some(1));

    write_config(&a, 2).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while cached_value(&b).await != Some(2) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the change notification reaches the replica");
    cancel.send(true).unwrap();
    fiber.await.unwrap();
}

#[tokio::test]
async fn a_deleted_row_stops_being_served() {
    let a = TestApp::spawn().await;
    let b = a.spawn_replica(SpawnOptions::default()).await;
    write_config(&a, 1).await;
    assert_eq!(cached_value(&b).await, Some(1));
    a.db.execute_unprepared("DELETE FROM plugin_config WHERE ref_id = 'cache-probe'")
        .await
        .expect("delete");
    queue_submission(&a, "after_delete").await;
    assert_eq!(claim(&b).await, 1);
    assert_eq!(cached_value(&b).await, None);
}

async fn epoch(db: &common::db::DatabaseConnection) -> i64 {
    use sea_orm::{DbBackend, Statement};
    db.query_one_raw(Statement::from_string(
        DbBackend::Postgres,
        "SELECT last_value FROM plugin_config_epoch",
    ))
    .await
    .expect("read the counter")
    .expect("one row")
    .try_get("", "last_value")
    .expect("last_value")
}

#[tokio::test]
async fn the_counter_advances_when_a_change_commits_and_not_before() {
    use sea_orm::TransactionTrait;
    let app = TestApp::spawn().await;
    let before = epoch(&app.db).await;
    let txn = app.db.begin().await.expect("begin");
    txn.execute_unprepared(
        "INSERT INTO plugin_config (scope, ref_id, namespace, config, position) \
         VALUES ('plugin', 'cache-probe', 'settings', '{}', 0)",
    )
    .await
    .expect("write");
    assert_eq!(epoch(&app.db).await, before, "advanced before the commit");
    txn.commit().await.expect("commit");
    assert!(
        epoch(&app.db).await > before,
        "a committed change advances it"
    );
}

#[tokio::test]
async fn a_statement_that_changes_no_rows_leaves_the_counter_alone() {
    let app = TestApp::spawn().await;
    let before = epoch(&app.db).await;
    app.db
        .execute_unprepared("DELETE FROM plugin_config WHERE ref_id = 'no-such-row'")
        .await
        .expect("delete nothing");
    assert_eq!(epoch(&app.db).await, before);
}

/// `SET LOCAL ROLE` to the test database's plugin group role.
async fn set_plugin_group(app: &TestApp) -> String {
    let group: String = app
        .db
        .query_one_raw(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT name FROM plugin_group_role",
        ))
        .await
        .expect("query")
        .expect("a group role")
        .try_get("", "name")
        .expect("name");
    format!("SET LOCAL ROLE \"{group}\"")
}

#[tokio::test]
async fn the_plugin_role_cannot_move_the_counter() {
    use sea_orm::TransactionTrait;
    let app = TestApp::spawn().await;
    for stmt in [
        "SELECT setval('plugin_config_epoch', 1)",
        "SELECT nextval('plugin_config_epoch')",
    ] {
        let txn = app.db.begin().await.expect("begin");
        txn.execute_unprepared(&set_plugin_group(&app).await)
            .await
            .expect("become the plugin role");
        let err = txn
            .execute_unprepared(stmt)
            .await
            .expect_err("the plugin role must be refused");
        assert!(
            err.to_string().contains("permission denied"),
            "{stmt}: {err}"
        );
        txn.rollback().await.expect("rollback");
    }
    // Nor can it write the config the counter tracks: only host functions do.
    let txn = app.db.begin().await.expect("begin");
    txn.execute_unprepared(&set_plugin_group(&app).await)
        .await
        .unwrap();
    let err = txn
        .execute_unprepared(
            "INSERT INTO plugin_config (scope, ref_id, namespace, config, position) \
             VALUES ('plugin', 'cache-probe', 'by-plugin', '{}', 0)",
        )
        .await
        .expect_err("the plugin role must not write plugin_config");
    assert!(err.to_string().contains("permission denied"), "{err}");
    txn.rollback().await.expect("rollback");
}
