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

#[tokio::test]
async fn a_change_committed_before_a_claim_pass_is_seen_by_the_claiming_replica() {
    let a = TestApp::spawn().await;
    let b = a.spawn_replica(SpawnOptions::default()).await;
    write_config(&a, 1).await;
    assert_eq!(cached_value(&b).await, Some(1));

    write_config(&a, 2).await;
    let server_id = b.state.config.server.id.clone();
    server::dispatcher::claim::claim_once(&b.state, &server_id, 32, 256)
        .await
        .expect("claim pass");

    assert_eq!(cached_value(&b).await, Some(2));
}

#[tokio::test]
async fn a_change_reaches_a_listening_replica_without_a_claim() {
    let a = TestApp::spawn().await;
    let b = a
        .spawn_replica(SpawnOptions {
            start_dispatcher: true,
            ..Default::default()
        })
        .await;
    write_config(&a, 1).await;
    assert_eq!(cached_value(&b).await, Some(1));

    write_config(&a, 2).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while cached_value(&b).await != Some(2) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the change notification reaches the replica");
}

#[tokio::test]
async fn a_deleted_row_stops_being_served() {
    let a = TestApp::spawn().await;
    write_config(&a, 1).await;
    assert_eq!(cached_value(&a).await, Some(1));
    a.db.execute_unprepared("DELETE FROM plugin_config WHERE ref_id = 'cache-probe'")
        .await
        .expect("delete");
    let server_id = a.state.config.server.id.clone();
    server::dispatcher::claim::claim_once(&a.state, &server_id, 32, 256)
        .await
        .expect("claim pass");
    assert_eq!(cached_value(&a).await, None);
}

async fn epoch(db: &sea_orm::DatabaseConnection) -> i64 {
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

#[tokio::test]
async fn the_plugin_role_cannot_move_the_counter() {
    use sea_orm::TransactionTrait;
    let app = TestApp::spawn().await;
    for stmt in [
        "SELECT setval('plugin_config_epoch', 1)",
        "SELECT nextval('plugin_config_epoch')",
    ] {
        let txn = app.db.begin().await.expect("begin");
        txn.execute_unprepared("SET LOCAL ROLE broccoli_plugin")
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
    // A plugin's own config write still advances it, through the trigger.
    let before = epoch(&app.db).await;
    let txn = app.db.begin().await.expect("begin");
    txn.execute_unprepared("SET LOCAL ROLE broccoli_plugin")
        .await
        .unwrap();
    txn.execute_unprepared(
        "INSERT INTO plugin_config (scope, ref_id, namespace, config, position) \
         VALUES ('plugin', 'cache-probe', 'by-plugin', '{}', 0)",
    )
    .await
    .expect("a plugin may write plugin_config");
    txn.commit().await.expect("commit");
    assert!(epoch(&app.db).await > before);
}
