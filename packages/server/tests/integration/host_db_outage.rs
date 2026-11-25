//! A host function that runs the host's own SQL meets a database outage the
//! way plugin SQL does: the plugin's call fails as the database's, so a route
//! answers a retryable `503 DATABASE_UNAVAILABLE` with `Retry-After` and a
//! timer delivery is handed back with its attempt refunded.

use serde_json::{Value, json};

use crate::common::{SpawnOptions, TestApp, routes};

async fn spawn() -> TestApp {
    TestApp::spawn_with_plugins_and_options(SpawnOptions {
        own_host_db: true,
        ..Default::default()
    })
    .await
}

fn kv(key: &str) -> String {
    routes::plugin_proxy("server-plugin", &format!("kv/{key}"))
}

#[tokio::test]
async fn a_route_whose_storage_meets_an_outage_answers_the_databases_503() {
    let app = spawn().await;
    let wrote = app
        .post_without_token(&kv("k"), &json!({ "value": "v" }))
        .await;
    assert_eq!(wrote.status, 200, "{}", wrote.text);

    app.host_db
        .clone()
        .close()
        .await
        .expect("close the host pool");

    for (what, res) in [
        ("store_get", app.get_without_token(&kv("k")).await),
        (
            "store_set",
            app.post_without_token(&kv("k"), &json!({ "value": "w" }))
                .await,
        ),
    ] {
        assert_eq!(res.status, 503, "{what}: {}", res.text);
        assert_eq!(
            res.body["code"], "DATABASE_UNAVAILABLE",
            "{what}: {}",
            res.text
        );
        assert_eq!(
            res.headers.get("retry-after").and_then(|v| v.to_str().ok()),
            Some(
                server::error::DATABASE_RETRY_AFTER_SECS
                    .to_string()
                    .as_str()
            ),
            "{what}"
        );
    }
}

#[tokio::test]
async fn a_timer_whose_storage_meets_an_outage_is_handed_back_unspent() {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    let app = spawn().await;
    let fire_at_ms = app.clock().now_ms().await;
    let scheduled = app
        .post_without_token(
            &routes::plugin_proxy("server-plugin", "timer/schedule"),
            &json!({ "key": "t", "fire_at_ms": fire_at_ms, "payload": "{}" }),
        )
        .await;
    assert_eq!(scheduled.status, 200, "{}", scheduled.text);

    app.host_db
        .clone()
        .close()
        .await
        .expect("close the host pool");

    let config = server::dispatcher::plugin_timer::TimerConfig::default();
    let stats = server::dispatcher::plugin_timer::tick_once(&app.state, &config)
        .await
        .expect("the tick runs on the app's own pool");
    assert_eq!(stats.claimed, 1);

    // `on_timer` reads its storage first, so the delivery could not run: the
    // row is back, unclaimed, with no attempt spent.
    let row = server::entity::plugin_timer::Entity::find()
        .filter(server::entity::plugin_timer::Column::Key.eq("t"))
        .one(&app.db)
        .await
        .unwrap()
        .expect("the timer is kept");
    assert_eq!(
        (row.attempts, row.claimed_at),
        (0, None),
        "{:?}",
        Value::from(row.attempts)
    );
}
