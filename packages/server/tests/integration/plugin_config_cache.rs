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
