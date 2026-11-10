//! Plugin lifecycle across server replicas: two `TestApp`s on one database
//! stand in for two replicas behind the gateway. Each test drives the peer's
//! reconcile directly (`plugin_lifecycle::reconcile`) instead of waiting for
//! the 5s poll, so it is deterministic.

use std::path::{Path, PathBuf};

use plugin_core::registry::PluginStatus;
use serde_json::json;
use server::plugin_lifecycle;

use crate::common::{SpawnOptions, TestApp, routes};

const PLUGIN: &str = "server-plugin";

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn status(app: &TestApp, plugin_id: &str) -> Option<PluginStatus> {
    app.state
        .plugins
        .list_plugins()
        .unwrap()
        .into_iter()
        .find(|p| p.id == plugin_id)
        .map(|p| p.status)
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}

async fn admin(app: &TestApp) -> String {
    app.create_user_with_role("fleet_admin", "securepass", "admin")
        .await
}

#[tokio::test(flavor = "multi_thread")]
async fn disable_and_enable_through_one_replica_reach_the_other() {
    let a = TestApp::spawn_with_plugins().await;
    let b = a.spawn_replica(SpawnOptions::default()).await;
    let token = admin(&a).await;
    assert_eq!(status(&b, PLUGIN), Some(PluginStatus::Loaded));

    let res = a
        .post_with_token(&routes::admin_plugin_disable(PLUGIN), &json!({}), &token)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    plugin_lifecycle::reconcile(&b.state).await.unwrap();
    assert_eq!(
        status(&b, PLUGIN),
        Some(PluginStatus::Unloaded),
        "a disable served by A must unload the plugin on B"
    );

    let res = a
        .post_with_token(&routes::admin_plugin_enable(PLUGIN), &json!({}), &token)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    let failures = plugin_lifecycle::reconcile(&b.state).await.unwrap();
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(
        status(&b, PLUGIN),
        Some(PluginStatus::Loaded),
        "an enable served by A must load the plugin on B"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn replicas_booting_together_both_come_up() {
    let a = TestApp::spawn_with_plugins().await;
    // Wipe the rows so both replicas discover the plugins as new at once.
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    a.db.execute_raw(Statement::from_string(
        DbBackend::Postgres,
        "DELETE FROM plugin",
    ))
    .await
    .unwrap();
    let (b, c) = tokio::join!(
        a.spawn_replica(SpawnOptions::default()),
        a.spawn_replica(SpawnOptions::default())
    );
    assert_eq!(status(&b, PLUGIN), Some(PluginStatus::Loaded));
    assert_eq!(status(&c, PLUGIN), Some(PluginStatus::Loaded));
}

/// B was deployed without the plugin's files. It must report itself not
/// ready, must not touch fleet-wide state, and must pick the plugin up once
/// the files arrive and someone clicks Reload on any replica.
#[tokio::test(flavor = "multi_thread")]
async fn a_replica_missing_a_plugin_reports_it_and_converges_after_reload() {
    let a = TestApp::spawn_with_plugins().await;
    let b_dir = tempfile::tempdir().unwrap();
    let b = a
        .spawn_replica(SpawnOptions {
            plugins_dir: Some(b_dir.path().to_path_buf()),
            allow_plugin_activation_failures: true,
            ..Default::default()
        })
        .await;
    let token = admin(&a).await;

    let res = b.get_without_token("/healthz").await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert_eq!(res.body["plugins"], "degraded", "{}", res.text);
    let not_ready = res.body["plugins_not_ready"].as_array().unwrap();
    assert!(
        not_ready
            .iter()
            .any(|p| p["id"] == PLUGIN && p["reason"] == "not installed on this replica"),
        "{}",
        res.text
    );
    let res = a.get_without_token("/healthz").await;
    assert_eq!(res.body["plugins"], "ok", "{}", res.text);

    copy_dir(&fixtures_dir().join(PLUGIN), &b_dir.path().join(PLUGIN));
    let res = a
        .post_with_token(&routes::admin_plugin_reload(PLUGIN), &json!({}), &token)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    plugin_lifecycle::reconcile(&b.state).await.unwrap();
    assert_eq!(status(&b, PLUGIN), Some(PluginStatus::Loaded));
    assert!(
        !plugin_lifecycle::not_ready(&b.state)
            .iter()
            .any(|p| p.id == PLUGIN)
    );
}

/// A plugin that fails to load on one replica must not cost the fleet its
/// pending timers: a healthy peer is still delivering them.
#[tokio::test(flavor = "multi_thread")]
async fn a_local_load_failure_keeps_fleet_timers() {
    let a = TestApp::spawn_with_plugins().await;
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    a.db.execute_raw(Statement::from_string(
        DbBackend::Postgres,
        format!(
            "INSERT INTO plugin_timer (plugin_id, key, fire_at, payload, attempts, created_at) \
             VALUES ('{PLUGIN}', 'k', NOW() + interval '1 hour', '{{}}', 0, NOW())"
        ),
    ))
    .await
    .unwrap();

    // B's copy of the plugin has a manifest but no WASM: loading fails there.
    let b_dir = tempfile::tempdir().unwrap();
    let broken = b_dir.path().join(PLUGIN);
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::copy(
        fixtures_dir().join(PLUGIN).join("plugin.toml"),
        broken.join("plugin.toml"),
    )
    .unwrap();
    let b = a
        .spawn_replica(SpawnOptions {
            plugins_dir: Some(b_dir.path().to_path_buf()),
            allow_plugin_activation_failures: true,
            ..Default::default()
        })
        .await;

    assert!(matches!(status(&b, PLUGIN), Some(PluginStatus::Failed(_))));
    let row =
        a.db.query_one_raw(Statement::from_string(
            DbBackend::Postgres,
            format!("SELECT COUNT(*)::bigint AS n FROM plugin_timer WHERE plugin_id = '{PLUGIN}'"),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.try_get::<i64>("", "n").unwrap(),
        1,
        "B's local load failure must not delete the fleet's timers"
    );
    assert!(
        plugin_lifecycle::not_ready(&b.state)
            .iter()
            .any(|p| p.id == PLUGIN),
        "the failure must be visible in readiness"
    );
}

/// Any replica may claim a due timer. One that does not have the plugin
/// loaded must leave it for one that does, instead of dropping it.
#[tokio::test(flavor = "multi_thread")]
async fn a_replica_without_the_plugin_leaves_due_timers_for_one_that_has_it() {
    use server::dispatcher::plugin_timer::{TimerConfig, tick_once};

    let a = TestApp::spawn_with_plugins().await;
    let b_dir = tempfile::tempdir().unwrap();
    let b = a
        .spawn_replica(SpawnOptions {
            plugins_dir: Some(b_dir.path().to_path_buf()),
            allow_plugin_activation_failures: true,
            ..Default::default()
        })
        .await;
    let fire_at_ms = chrono::Utc::now().timestamp_millis();
    let res = a
        .post_without_token(
            &routes::plugin_proxy(PLUGIN, "timer/schedule"),
            &json!({ "key": "k", "fire_at_ms": fire_at_ms, "payload": "{}" }),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);

    let config = TimerConfig::default();
    let on_b = tick_once(&b.state, &config).await.unwrap();
    assert_eq!(on_b.claimed, 0, "B cannot deliver it, so must not claim it");
    let on_a = tick_once(&a.state, &config).await.unwrap();
    assert_eq!(on_a.claimed, 1, "A still finds the timer and delivers it");
}

/// The claim fiber of a replica that has no handler for a submission's
/// contest type leaves the submission `Queued` for a replica that has one,
/// instead of claiming it only to fail it with `NO_HANDLER_REGISTERED`.
#[tokio::test(flavor = "multi_thread")]
async fn a_replica_without_the_contest_type_leaves_queued_work_for_one_that_has_it() {
    use common::SubmissionStatus;
    use sea_orm::{ActiveModelTrait, EntityTrait, Set};
    use server::entity::submission;

    let a = TestApp::spawn().await;
    let b = a.spawn_replica(SpawnOptions::default()).await;
    let token = admin(&a).await;
    let problem_id = a.create_problem(&token, "Fleet claim").await;
    let user_id = {
        use sea_orm::{ColumnTrait, QueryFilter};
        server::entity::user::Entity::find()
            .filter(server::entity::user::Column::Username.eq("fleet_admin"))
            .one(&a.db)
            .await
            .unwrap()
            .unwrap()
            .id
    };
    // Only A can judge this contest type (B's plugin set lacks it).
    a.state
        .registries
        .contest_type_registry
        .write()
        .await
        .insert(
            "fleet-only".into(),
            server::registry::ContestTypeHandlers {
                plugin_id: "__test__".into(),
                submission_fn: "noop".into(),
                code_run_fn: "noop".into(),
                submission_admission: false,
            },
        );
    let row = submission::ActiveModel {
        files: Set(json!([{ "filename": "main.cpp", "content": "int main() {}" }])),
        language: Set("cpp".into()),
        user_id: Set(user_id),
        problem_id: Set(problem_id),
        contest_id: Set(None),
        contest_type: Set("fleet-only".into()),
        status: Set(SubmissionStatus::Queued),
        judge_epoch: Set(0),
        created_at: Set(chrono::Utc::now()),
        ..Default::default()
    }
    .insert(&a.db)
    .await
    .unwrap();

    server::dispatcher::claim::claim_once(&b.state, "replica-b", 32, 0)
        .await
        .unwrap();
    let after_b = submission::Entity::find_by_id(row.id)
        .one(&a.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after_b.status, SubmissionStatus::Queued);
    assert_eq!(after_b.owner_server_id, None, "B must not claim it");

    server::dispatcher::claim::claim_once(&a.state, "replica-a", 32, 0)
        .await
        .unwrap();
    let after_a = submission::Entity::find_by_id(row.id)
        .one(&a.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after_a.owner_server_id.as_deref(), Some("replica-a"));
}

fn find<'a>(list: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    list.as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == id)
        .unwrap_or_else(|| panic!("{id} listed: {list}"))
}

/// The admin plugin list reports a plugin failing on one replica as failing
/// fleet-wide, with that replica's error, whichever replica serves the
/// request; a reload through any replica is the retry, and a plugin broken on
/// the serving replica can still be turned off.
#[tokio::test(flavor = "multi_thread")]
async fn admin_list_aggregates_each_replicas_load_state() {
    let a = TestApp::spawn_with_plugins().await;
    let b_dir = tempfile::tempdir().unwrap();
    let broken = b_dir.path().join(PLUGIN);
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::copy(
        fixtures_dir().join(PLUGIN).join("plugin.toml"),
        broken.join("plugin.toml"),
    )
    .unwrap();
    let b = a
        .spawn_replica(SpawnOptions {
            plugins_dir: Some(b_dir.path().to_path_buf()),
            allow_plugin_activation_failures: true,
            ..Default::default()
        })
        .await;
    let token = admin(&a).await;
    // A reports before B's failure existed; let it re-report.
    plugin_lifecycle::reconcile(&a.state).await.unwrap();

    for (via, app) in [("A", &a), ("B", &b)] {
        let res = app.get_with_token(routes::ADMIN_PLUGINS, &token).await;
        assert_eq!(res.status, 200, "{}", res.text);
        let p = find(&res.body, PLUGIN);
        assert_eq!(p["enabled"], true, "via {via}: {p}");
        let fleet = &p["fleet"];
        assert_eq!(fleet["state"], "failed", "via {via}: {fleet}");
        assert_eq!(fleet["replicas_total"], 2, "via {via}: {fleet}");
        assert_eq!(fleet["replicas_failed"], 1, "via {via}: {fleet}");
        assert_eq!(fleet["replicas_loaded"], 1, "via {via}: {fleet}");
        let b_entry = fleet["replicas"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["status"] == "failed")
            .unwrap_or_else(|| panic!("a failed replica: {fleet}"));
        assert_eq!(b_entry["server_id"], b.state.config.server.id.as_str());
        assert!(
            b_entry["error"].as_str().is_some_and(|e| !e.is_empty()),
            "{b_entry}"
        );
        assert!(b_entry["retry_at"].is_string(), "{b_entry}");
    }
    let detail = a
        .get_with_token(&routes::admin_plugin_details(PLUGIN), &token)
        .await;
    assert_eq!(detail.body["fleet"]["state"], "failed", "{}", detail.text);

    // Fix B's files, then retry through A: the reload bumps the generation,
    // so B retries at once instead of waiting out its backoff.
    copy_dir(&fixtures_dir().join(PLUGIN), &broken);
    let res = a
        .post_with_token(&routes::admin_plugin_reload(PLUGIN), &json!({}), &token)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    plugin_lifecycle::reconcile(&b.state).await.unwrap();
    let res = a.get_with_token(routes::ADMIN_PLUGINS, &token).await;
    let fleet = &find(&res.body, PLUGIN)["fleet"];
    assert_eq!(fleet["state"], "loaded", "{fleet}");
    assert_eq!(fleet["replicas_loaded"], 2, "{fleet}");
}

/// Disable is keyed on the desired state, so it works through a replica
/// where the plugin failed to load.
#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_broken_on_the_serving_replica_can_still_be_disabled() {
    let a = TestApp::spawn_with_plugins().await;
    let b_dir = tempfile::tempdir().unwrap();
    let b = a
        .spawn_replica(SpawnOptions {
            plugins_dir: Some(b_dir.path().to_path_buf()),
            allow_plugin_activation_failures: true,
            ..Default::default()
        })
        .await;
    let token = admin(&a).await;
    let res = b
        .post_with_token(&routes::admin_plugin_disable(PLUGIN), &json!({}), &token)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    plugin_lifecycle::reconcile(&a.state).await.unwrap();
    assert_eq!(status(&a, PLUGIN), Some(PluginStatus::Unloaded));
}
