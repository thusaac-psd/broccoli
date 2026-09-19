//! Integration coverage for the plugin timer feature, exercised end-to-end
//! through the real `server-plugin` fixture (built as an actual `.wasm`
//! artefact, not mocked) and the dispatcher's real background delivery loop
//! (`server::dispatcher::plugin_timer::run`, spawned unconditionally by
//! `Dispatcher::spawn` - see `dispatcher/mod.rs`).
//!
//! `SpawnOptions { start_dispatcher: true, .. }` is required here: the
//! fixture default (`TestApp::spawn_with_plugins()`) leaves the dispatcher
//! (and therefore the plugin timer loop) unstarted, since most integration
//! tests don't need any background poller hammering the shared test DB.

use std::time::Duration;

use serde_json::{Value, json};

use crate::common::{SpawnOptions, TestApp, routes};

fn fixtures_timer_negative_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures_timer_negative")
}

async fn spawn_timer_app() -> TestApp {
    TestApp::spawn_with_plugins_and_options(SpawnOptions {
        start_dispatcher: true,
        ..Default::default()
    })
    .await
}

async fn schedule_timer_raw(
    app: &TestApp,
    plugin_id: &str,
    key: &str,
    delay_ms: i64,
    payload: &str,
) -> crate::common::TestResponse {
    let fire_at_ms = chrono::Utc::now().timestamp_millis() + delay_ms;
    app.post_without_token(
        &routes::plugin_proxy(plugin_id, "timer/schedule"),
        &json!({ "key": key, "fire_at_ms": fire_at_ms, "payload": payload }),
    )
    .await
}

async fn schedule_timer(app: &TestApp, key: &str, delay_ms: i64, payload: &str) {
    let res = schedule_timer_raw(app, "server-plugin", key, delay_ms, payload).await;
    assert_eq!(res.status, 200, "schedule_timer failed: {}", res.text);
}

async fn cancel_timer(app: &TestApp, key: &str) {
    let res = app
        .post_without_token(
            &routes::plugin_proxy("server-plugin", "timer/cancel"),
            &json!({ "key": key }),
        )
        .await;
    assert_eq!(res.status, 200, "cancel_timer failed: {}", res.text);
}

async fn read_deliveries(app: &TestApp) -> Vec<Value> {
    let res = app
        .get_without_token(&routes::plugin_proxy("server-plugin", "timer/deliveries"))
        .await;
    assert_eq!(res.status, 200, "read_deliveries failed: {}", res.text);
    res.body.as_array().cloned().unwrap_or_default()
}

/// Polls the plugin's own `/timer/deliveries` route until `expected`
/// deliveries have arrived or `timeout` elapses. Delivery happens on the
/// REAL background loop (1s tick interval by default), not a manually
/// driven tick - this proves `Dispatcher::spawn`'s unconditional
/// `plugin_timer::run` wiring actually delivers, not just `tick_once` in
/// isolation (already covered by `dispatcher::plugin_timer`'s unit tests).
async fn await_deliveries(app: &TestApp, expected: usize, timeout: Duration) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let deliveries = read_deliveries(app).await;
        if deliveries.len() >= expected {
            return deliveries;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {expected} deliveries, got {}: {:?}",
            deliveries.len(),
            deliveries
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn a_scheduled_timer_is_delivered_to_the_plugin() {
    let app = spawn_timer_app().await;
    schedule_timer(&app, "k1", 0, r#"{"hello":"world"}"#).await;

    let deliveries = await_deliveries(&app, 1, Duration::from_secs(10)).await;

    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0]["key"], "k1");
    assert_eq!(deliveries[0]["payload"], r#"{"hello":"world"}"#);
}

#[tokio::test]
async fn rescheduling_the_same_key_replaces_rather_than_duplicates() {
    let app = spawn_timer_app().await;
    // Far future, then immediate: the second call must REPLACE the first, so
    // exactly one delivery arrives - not two, and not zero.
    schedule_timer(&app, "k2", 3_600_000, "first").await;
    schedule_timer(&app, "k2", 0, "second").await;

    let deliveries = await_deliveries(&app, 1, Duration::from_secs(10)).await;

    assert_eq!(deliveries.len(), 1, "replace, not duplicate");
    assert_eq!(
        deliveries[0]["payload"], "second",
        "the later schedule wins"
    );
}

#[tokio::test]
async fn a_cancelled_timer_never_fires() {
    let app = spawn_timer_app().await;
    schedule_timer(&app, "k3", 1_500, "doomed").await;
    cancel_timer(&app, "k3").await;

    // Sleep past both the fire_at and at least one real background tick.
    tokio::time::sleep(Duration::from_secs(5)).await;

    assert!(
        read_deliveries(&app).await.is_empty(),
        "cancelled timers must not fire"
    );
}

#[tokio::test]
async fn a_plugin_without_the_timer_permission_cannot_schedule() {
    // The host function is registered under the "timer" permission key, so a
    // plugin lacking it should not even see the import. Pins that the gate is
    // the registration, not a runtime check that could be forgotten.
    //
    // Isolated into its own `plugins_dir`/`allow_plugin_activation_failures`:
    // `no-timer-perm-plugin` imports `timer_schedule` without declaring the
    // permission, so its WASM instance can never link - activation itself
    // fails, which would trip the shared fixture's hard "all plugins
    // activated" assertion if it lived alongside `server-plugin`.
    let app = TestApp::spawn_with_plugins_and_options(SpawnOptions {
        plugins_dir: Some(fixtures_timer_negative_dir()),
        allow_plugin_activation_failures: true,
        ..Default::default()
    })
    .await;

    let res = schedule_timer_raw(&app, "no-timer-perm-plugin", "k", 0, "{}").await;
    assert_ne!(
        res.status, 200,
        "scheduling without the permission must fail: {}",
        res.text
    );
}

/// Two concurrent ticks racing for one due timer must deliver it once.
///
/// This is the integration-level counterpart to the unit tests on `claim_due`
/// in `dispatcher::plugin_timer`. It goes through `tick_once`, so it covers
/// the whole claim-and-deliver path -- including `deliver`, which the unit
/// tests do not reach -- and therefore proves the plugin's handler is invoked
/// once rather than merely that one row was claimed.
///
/// The dispatcher is deliberately NOT started here: its own loop would race
/// these explicit ticks and make the counts meaningless.
#[tokio::test]
async fn two_concurrent_ticks_deliver_a_timer_exactly_once() {
    let app = TestApp::spawn_with_plugins().await;
    schedule_timer(&app, "raced", 0, r#"{"once":true}"#).await;

    let config = server::dispatcher::plugin_timer::TimerConfig::default();
    let (a, b) = tokio::join!(
        server::dispatcher::plugin_timer::tick_once(&app.state, &config),
        server::dispatcher::plugin_timer::tick_once(&app.state, &config),
    );
    let claimed = a.unwrap().claimed + b.unwrap().claimed;

    assert_eq!(claimed, 1, "exactly one tick claims the row");

    let deliveries = read_deliveries(&app).await;
    assert_eq!(
        deliveries.len(),
        1,
        "the plugin handler runs once, not once per tick: {deliveries:?}"
    );
    assert_eq!(deliveries[0]["key"], "raced");
}

/// Seed a fixture-plugin KV key through its `kv_write` route. Mirrors
/// `visibility_plugin.rs`'s helper of the same name.
async fn seed_kv(app: &TestApp, key: &str, value: &str) {
    let route = routes::plugin_proxy("server-plugin", &format!("kv/{key}"));
    let res = app
        .post_without_token(&route, &json!({ "value": value }))
        .await;
    assert_eq!(res.status, 200, "seeding KV `{key}` failed: {}", res.text);
}

async fn pending_timer_count(app: &TestApp, key: &str) -> u64 {
    use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};
    server::entity::plugin_timer::Entity::find()
        .filter(server::entity::plugin_timer::Column::Key.eq(key))
        .count(&app.state.db)
        .await
        .expect("count pending timers")
}

/// A handler that traps must be retried a bounded number of times and then
/// dropped -- never retried forever, and never allowed to stall delivery for
/// other timers.
///
/// Delivery is documented as at-least-once with bounded retries THEN DROP,
/// precisely because a timer has no safe fail-closed outcome: "never fires"
/// is the failure the capability exists to prevent, so the loop cannot simply
/// refuse to move past a poisonous row. This test is what makes that
/// documented promise checkable.
///
/// `lease_secs: 0` makes each explicit tick immediately eligible to reclaim
/// the row, so the retry budget can be exhausted without waiting out a real
/// 30-second lease.
#[tokio::test]
async fn a_trapping_handler_is_retried_then_dropped() {
    let app = TestApp::spawn_with_plugins().await;
    seed_kv(&app, "trap_on_timer", "1").await;
    schedule_timer(&app, "poison", 0, "{}").await;

    let config = server::dispatcher::plugin_timer::TimerConfig {
        lease_secs: 0,
        max_attempts: 3,
        ..Default::default()
    };

    for tick in 1..=3 {
        let stats = server::dispatcher::plugin_timer::tick_once(&app.state, &config)
            .await
            .expect("a trapping plugin must not fail the tick itself");
        assert_eq!(
            stats.claimed, 1,
            "tick {tick} should still claim the poisonous row"
        );
    }

    assert_eq!(
        pending_timer_count(&app, "poison").await,
        0,
        "dropped once the retry budget is spent, not retried forever"
    );

    // The loop is still healthy: a well-behaved timer scheduled afterwards is
    // delivered normally. Without this, a fix that simply wedged the loop
    // after a trap would pass everything above.
    seed_kv(&app, "trap_on_timer", "0").await;
    schedule_timer(&app, "healthy", 0, r#"{"ok":true}"#).await;
    server::dispatcher::plugin_timer::tick_once(&app.state, &config)
        .await
        .unwrap();

    let deliveries = read_deliveries(&app).await;
    assert!(
        deliveries.iter().any(|d| d["key"] == "healthy"),
        "a poisonous timer must not starve later ones: {deliveries:?}"
    );
}
