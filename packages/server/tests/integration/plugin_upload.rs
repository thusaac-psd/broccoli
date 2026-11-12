//! Uploaded plugins across server replicas: two `TestApp`s share one
//! database and therefore one blob store (the database backend), each with
//! its own plugin cache. Each test drives the peer's reconcile directly
//! (`plugin_lifecycle::reconcile`) instead of waiting for the poll, so it is
//! deterministic.

use std::path::PathBuf;

use plugin_core::registry::PluginStatus;
use sea_orm::{ConnectionTrait, DbBackend, EntityTrait, Statement};
use serde_json::{Value, json};
use server::entity::plugin as plugin_entity;
use server::plugin_lifecycle;

use crate::common::{SpawnOptions, TestApp, routes};

/// Bundled in `tests/fixtures`, at version 0.1.0.
const BUNDLED: &str = "server-plugin";
/// Not bundled anywhere: only ever uploaded.
const UPLOAD_ONLY: &str = "uploaded-plugin";

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// A plugin archive of the `server-plugin` fixture's manifest and WASM,
/// published as `id` at `version`.
fn archive(id: &str, version: &str) -> Vec<u8> {
    let manifest = std::fs::read_to_string(fixture(BUNDLED).join("plugin.toml"))
        .unwrap()
        .replace("version = \"0.1.0\"", &format!("version = \"{version}\""));
    assert!(manifest.contains(version));
    let wasm = std::fs::read(fixture(BUNDLED).join("server_plugin.wasm")).unwrap();
    tar_gz(&[
        (&format!("{id}/plugin.toml"), manifest.as_bytes()),
        (&format!("{id}/server_plugin.wasm"), &wasm),
    ])
}

/// For requests refused before the archive is read: small enough to be
/// sent whole before the server answers.
fn tiny_archive() -> Vec<u8> {
    tar_gz(&[(&format!("{UPLOAD_ONLY}/plugin.toml"), b"name = \"x\"")])
}

fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    for (path, data) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        builder.append_data(&mut header, path, *data).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

async fn admin(app: &TestApp) -> String {
    app.create_user_with_role("upload_admin", "securepass", "admin")
        .await
}

fn status(app: &TestApp, plugin_id: &str) -> Option<PluginStatus> {
    app.state
        .plugins
        .get_registry()
        .read()
        .unwrap()
        .get(plugin_id)
        .map(|e| e.status.clone())
}

/// Version and directory of what `app` has registered for `plugin_id`.
fn registered(app: &TestApp, plugin_id: &str) -> (String, PathBuf) {
    let registry = app.state.plugins.get_registry().read().unwrap();
    let entry = registry.get(plugin_id).expect("plugin registered");
    (entry.manifest.version.clone(), entry.root_dir.clone())
}

async fn row(app: &TestApp, plugin_id: &str) -> Option<plugin_entity::Model> {
    plugin_entity::Entity::find_by_id(plugin_id)
        .one(&app.db)
        .await
        .unwrap()
}

fn cache_entries(app: &TestApp) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(app.plugin_cache.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn replica<'a>(fleet: &'a Value, app: &TestApp) -> &'a Value {
    fleet["replicas"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["server_id"] == app.state.config.server.id.as_str())
        .unwrap_or_else(|| panic!("no report from {}: {fleet}", app.state.config.server.id))
}

async fn blob(app: &TestApp, hash: &str) -> Vec<u8> {
    app.db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT data FROM blob_data WHERE content_hash = $1",
            [hash.into()],
        ))
        .await
        .unwrap()
        .expect("blob stored")
        .try_get("", "data")
        .unwrap()
}

/// Overwrite a stored blob's bytes, as a damaged or tampered store would.
async fn set_blob(app: &TestApp, hash: &str, data: Vec<u8>) {
    app.db
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE blob_data SET data = $2 WHERE content_hash = $1",
            [hash.into(), data.into()],
        ))
        .await
        .unwrap();
}

async fn upload(app: &TestApp, token: &str, archive: Vec<u8>) -> Value {
    let res = app
        .post_plugin_archive(routes::ADMIN_PLUGIN_UPLOAD, archive, token)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    res.body
}

#[tokio::test(flavor = "multi_thread")]
async fn an_upload_through_one_replica_reaches_the_other() {
    let a = TestApp::spawn_with_plugins().await;
    let b = a.spawn_replica(SpawnOptions::default()).await;
    let token = admin(&a).await;

    let body = upload(&a, &token, archive(UPLOAD_ONLY, "1.0.0")).await;
    assert_eq!(body["plugin_id"], UPLOAD_ONLY);
    assert_eq!(body["version"], "1.0.0");
    let hash = body["archive_hash"].as_str().unwrap().to_string();

    let published = row(&a, UPLOAD_ONLY).await.expect("row published");
    assert!(published.is_enabled);
    assert_eq!(published.archive_hash.as_deref(), Some(hash.as_str()));
    assert_eq!(published.version.as_deref(), Some("1.0.0"));
    assert_eq!(status(&a, UPLOAD_ONLY), Some(PluginStatus::Loaded));
    assert!(
        registered(&a, UPLOAD_ONLY)
            .1
            .starts_with(a.plugin_cache.path())
    );

    assert_eq!(status(&b, UPLOAD_ONLY), None, "B has not polled yet");
    let failures = plugin_lifecycle::reconcile(&b.state).await.unwrap();
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(status(&b, UPLOAD_ONLY), Some(PluginStatus::Loaded));
    let (version, root) = registered(&b, UPLOAD_ONLY);
    assert_eq!(version, "1.0.0");
    assert!(
        root.starts_with(b.plugin_cache.path()),
        "B loads its own unpacked copy, fetched from the blob store: {}",
        root.display()
    );
    assert_eq!(cache_entries(&b), vec![hash.clone()]);

    // B describes it the same way A does.
    let res = b
        .get_with_token(&routes::admin_plugin_details(UPLOAD_ONLY), &token)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert_eq!(res.body["source"]["kind"], "upload", "{}", res.text);
    assert_eq!(res.body["source"]["archive_hash"], hash.as_str());
    assert_eq!(res.body["source"]["bundled"], false);
    assert_eq!(res.body["fleet"]["state"], "loaded", "{}", res.text);
    for app in [&a, &b] {
        assert_eq!(replica(&res.body["fleet"], app)["version"], "1.0.0");
    }

    // A new version replaces the old one everywhere, and B drops the old
    // copy from its cache once nothing uses it.
    let body = upload(&a, &token, archive(UPLOAD_ONLY, "1.1.0")).await;
    let next = body["archive_hash"].as_str().unwrap().to_string();
    assert_ne!(next, hash);
    plugin_lifecycle::reconcile(&b.state).await.unwrap();
    assert_eq!(registered(&b, UPLOAD_ONLY).0, "1.1.0");
    assert_eq!(cache_entries(&b), vec![next.clone()]);
    assert_eq!(cache_entries(&a), vec![next]);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_upload_takes_precedence_over_the_bundled_copy() {
    let a = TestApp::spawn_with_plugins().await;
    let b = a.spawn_replica(SpawnOptions::default()).await;
    let token = admin(&a).await;
    assert_eq!(registered(&b, BUNDLED).0, "0.1.0");

    upload(&a, &token, archive(BUNDLED, "0.2.0")).await;
    plugin_lifecycle::reconcile(&b.state).await.unwrap();
    for app in [&a, &b] {
        let (version, root) = registered(app, BUNDLED);
        assert_eq!(version, "0.2.0");
        assert!(root.starts_with(app.plugin_cache.path()));
        assert_eq!(status(app, BUNDLED), Some(PluginStatus::Loaded));
    }

    let res = a
        .get_with_token(&routes::admin_plugin_details(BUNDLED), &token)
        .await;
    assert_eq!(res.body["source"]["kind"], "upload", "{}", res.text);
    assert_eq!(res.body["source"]["bundled"], true, "{}", res.text);

    // Reload keeps the upload: it re-applies the published source.
    let res = a
        .post_with_token(&routes::admin_plugin_reload(BUNDLED), &json!({}), &token)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    plugin_lifecycle::reconcile(&b.state).await.unwrap();
    assert_eq!(registered(&b, BUNDLED).0, "0.2.0");
}

#[tokio::test(flavor = "multi_thread")]
async fn inspecting_an_archive_reports_what_it_would_publish() {
    let a = TestApp::spawn_with_plugins().await;
    let token = admin(&a).await;

    let res = a
        .post_plugin_archive(
            routes::ADMIN_PLUGIN_UPLOAD_INSPECT,
            archive(BUNDLED, "0.2.0"),
            &token,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert_eq!(res.body["id"], BUNDLED);
    assert_eq!(res.body["version"], "0.2.0");
    assert_eq!(res.body["permissions"], json!(["storage", "sql", "timer"]));
    assert_eq!(res.body["current"]["source"], "bundle", "{}", res.text);
    assert_eq!(res.body["current"]["enabled"], true);

    // Nothing was published.
    let published = row(&a, BUNDLED).await.unwrap();
    assert_eq!(published.archive_hash, None);
    assert_eq!(registered(&a, BUNDLED).0, "0.1.0");

    let res = a
        .post_plugin_archive(
            routes::ADMIN_PLUGIN_UPLOAD_INSPECT,
            archive(UPLOAD_ONLY, "1.0.0"),
            &token,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert!(res.body["current"].is_null(), "{}", res.text);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_corrupt_archive_is_rejected_and_publishes_nothing() {
    let a = TestApp::spawn_with_plugins().await;
    let token = admin(&a).await;
    let manifest = std::fs::read(fixture(BUNDLED).join("plugin.toml")).unwrap();

    for (bytes, expected) in [
        (
            b"definitely not gzip".to_vec(),
            "Not a valid gzip-compressed tar",
        ),
        (
            archive(UPLOAD_ONLY, "1.0.0")[..200].to_vec(),
            "Not a valid gzip-compressed tar",
        ),
        (
            tar_gz(&[(&format!("{UPLOAD_ONLY}/plugin.toml"), &manifest[..])]),
            "not in the archive",
        ),
        (
            tar_gz(&[
                (&format!("{UPLOAD_ONLY}/plugin.toml"), &manifest[..]),
                ("other/plugin.toml", &manifest[..]),
            ]),
            "exactly one top-level directory",
        ),
    ] {
        for path in [
            routes::ADMIN_PLUGIN_UPLOAD_INSPECT,
            routes::ADMIN_PLUGIN_UPLOAD,
        ] {
            let res = a.post_plugin_archive(path, bytes.clone(), &token).await;
            assert_eq!(res.status, 400, "{path}: {}", res.text);
            assert_eq!(res.body["code"], "VALIDATION_ERROR", "{}", res.text);
            assert!(
                res.body["message"].as_str().unwrap().contains(expected),
                "{path}: expected '{expected}' in {}",
                res.text
            );
        }
    }
    assert!(row(&a, UPLOAD_ONLY).await.is_none());
    let blobs =
        a.db.query_one_raw(Statement::from_string(
            DbBackend::Postgres,
            "SELECT COUNT(*)::bigint AS n FROM blob_data",
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(blobs.try_get::<i64>("", "n").unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_that_fails_to_load_here_is_not_published() {
    let a = TestApp::spawn_with_plugins().await;
    let token = admin(&a).await;
    let manifest = std::fs::read_to_string(fixture(BUNDLED).join("plugin.toml")).unwrap();
    let broken = tar_gz(&[
        (&format!("{BUNDLED}/plugin.toml"), manifest.as_bytes()),
        (&format!("{BUNDLED}/server_plugin.wasm"), b"not wasm"),
    ]);

    let res = a
        .post_plugin_archive(routes::ADMIN_PLUGIN_UPLOAD, broken, &token)
        .await;
    assert_eq!(res.status, 422, "{}", res.text);
    assert_eq!(res.body["code"], "PLUGIN_LOAD_FAILED", "{}", res.text);

    let published = row(&a, BUNDLED).await.unwrap();
    assert_eq!(published.archive_hash, None);
    assert_eq!(status(&a, BUNDLED), Some(PluginStatus::Loaded));
    let (version, root) = registered(&a, BUNDLED);
    assert_eq!(version, "0.1.0");
    assert_eq!(root, fixture(BUNDLED), "the bundled copy keeps serving");
    assert!(cache_entries(&a).is_empty(), "{:?}", cache_entries(&a));
}

/// A new plugin that cannot load here leaves no trace: no row, nothing in
/// the registry, nothing in the cache.
#[tokio::test(flavor = "multi_thread")]
async fn a_new_plugin_that_fails_to_load_leaves_nothing_behind() {
    let a = TestApp::spawn_with_plugins().await;
    let token = admin(&a).await;
    let manifest = std::fs::read_to_string(fixture(BUNDLED).join("plugin.toml")).unwrap();
    let broken = tar_gz(&[
        (&format!("{UPLOAD_ONLY}/plugin.toml"), manifest.as_bytes()),
        (&format!("{UPLOAD_ONLY}/server_plugin.wasm"), b"not wasm"),
    ]);

    let res = a
        .post_plugin_archive(routes::ADMIN_PLUGIN_UPLOAD, broken, &token)
        .await;
    assert_eq!(res.status, 422, "{}", res.text);
    assert!(row(&a, UPLOAD_ONLY).await.is_none());
    assert_eq!(status(&a, UPLOAD_ONLY), None);
    assert!(cache_entries(&a).is_empty(), "{:?}", cache_entries(&a));
    let res = a.get_with_token(routes::ADMIN_PLUGINS, &token).await;
    assert!(
        !res.body
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == UPLOAD_ONLY),
        "{}",
        res.text
    );
}

/// B cannot use what A published (the blob store hands it bytes that do not
/// match the published hash). B must keep serving the version it had, say
/// so, and converge once the archive is intact and someone reloads.
#[tokio::test(flavor = "multi_thread")]
async fn a_replica_that_cannot_load_the_upload_keeps_serving_the_previous_version() {
    let a = TestApp::spawn_with_plugins().await;
    let b = a
        .spawn_replica(SpawnOptions {
            allow_plugin_activation_failures: true,
            ..Default::default()
        })
        .await;
    let token = admin(&a).await;

    let body = upload(&a, &token, archive(BUNDLED, "0.2.0")).await;
    let hash = body["archive_hash"].as_str().unwrap().to_string();
    let original = blob(&a, &hash).await;
    set_blob(&a, &hash, b"tampered".to_vec()).await;

    let failures = plugin_lifecycle::reconcile(&b.state).await.unwrap();
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].error.to_string().contains("hashing to"),
        "{}",
        failures[0].error
    );
    assert_eq!(status(&b, BUNDLED), Some(PluginStatus::Loaded));
    let (version, root) = registered(&b, BUNDLED);
    assert_eq!(version, "0.1.0", "B keeps its previous version");
    assert_eq!(root, fixture(BUNDLED));
    assert!(
        cache_entries(&b).is_empty(),
        "nothing unpacked from bad bytes"
    );
    assert!(
        plugin_lifecycle::not_ready(&b.state)
            .iter()
            .any(|p| p.id == BUNDLED && p.reason.contains("previous version")),
        "{:?}",
        plugin_lifecycle::not_ready(&b.state)
    );
    let res = a
        .get_with_token(&routes::admin_plugin_details(BUNDLED), &token)
        .await;
    assert_eq!(res.body["fleet"]["state"], "failed", "{}", res.text);
    let b_report = replica(&res.body["fleet"], &b);
    assert_eq!(b_report["status"], "outdated", "{b_report}");
    assert_eq!(b_report["version"], "0.1.0", "{b_report}");
    assert_eq!(replica(&res.body["fleet"], &a)["version"], "0.2.0");

    set_blob(&a, &hash, original).await;
    let res = a
        .post_with_token(&routes::admin_plugin_reload(BUNDLED), &json!({}), &token)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    let failures = plugin_lifecycle::reconcile(&b.state).await.unwrap();
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(registered(&b, BUNDLED).0, "0.2.0");
    assert_eq!(cache_entries(&b), vec![hash]);
}

/// A cache entry damaged after it was unpacked (a power loss, a disk
/// fault) no longer matches the published fingerprint: the server drops it
/// and fetches the archive again instead of trusting it forever.
#[tokio::test(flavor = "multi_thread")]
async fn a_damaged_cache_entry_is_fetched_again() {
    let a = TestApp::spawn_with_plugins().await;
    let b = a.spawn_replica(SpawnOptions::default()).await;
    let token = admin(&a).await;
    let body = upload(&a, &token, archive(UPLOAD_ONLY, "1.0.0")).await;
    let hash = body["archive_hash"].as_str().unwrap().to_string();
    plugin_lifecycle::reconcile(&b.state).await.unwrap();
    let wasm = b
        .plugin_cache
        .path()
        .join(&hash)
        .join(UPLOAD_ONLY)
        .join("server_plugin.wasm");
    let intact = std::fs::read(&wasm).unwrap();
    std::fs::write(&wasm, &intact[..intact.len() / 2]).unwrap();

    // Any re-apply checks the copy: here, a reload through A.
    let res = a
        .post_with_token(
            &routes::admin_plugin_reload(UPLOAD_ONLY),
            &json!({}),
            &token,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    let failures = plugin_lifecycle::reconcile(&b.state).await.unwrap();
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(std::fs::read(&wasm).unwrap(), intact, "B fetched it again");
    assert_eq!(status(&b, UPLOAD_ONLY), Some(PluginStatus::Loaded));
    let res = a
        .get_with_token(&routes::admin_plugin_details(UPLOAD_ONLY), &token)
        .await;
    assert_eq!(
        replica(&res.body["fleet"], &b)["status"],
        "loaded",
        "{}",
        res.text
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_an_upload_falls_back_to_the_bundled_copy() {
    let a = TestApp::spawn_with_plugins().await;
    let b = a.spawn_replica(SpawnOptions::default()).await;
    let token = admin(&a).await;
    upload(&a, &token, archive(BUNDLED, "0.2.0")).await;
    plugin_lifecycle::reconcile(&b.state).await.unwrap();
    assert_eq!(registered(&b, BUNDLED).0, "0.2.0");

    let res = a
        .delete_with_token(&routes::admin_plugin_upload(BUNDLED), &token)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert_eq!(res.body["outcome"], "bundle_restored", "{}", res.text);
    let published = row(&a, BUNDLED).await.unwrap();
    assert_eq!(published.archive_hash, None);
    assert_eq!(published.version.as_deref(), Some("0.1.0"));
    assert!(published.is_enabled);

    plugin_lifecycle::reconcile(&b.state).await.unwrap();
    for app in [&a, &b] {
        assert_eq!(registered(app, BUNDLED), ("0.1.0".into(), fixture(BUNDLED)));
        assert_eq!(status(app, BUNDLED), Some(PluginStatus::Loaded));
        assert!(cache_entries(app).is_empty(), "{:?}", cache_entries(app));
    }

    let res = a
        .delete_with_token(&routes::admin_plugin_upload(BUNDLED), &token)
        .await;
    assert_eq!(res.status, 409, "nothing left to remove: {}", res.text);
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_a_disabled_upload_restores_the_bundled_copy_disabled() {
    let a = TestApp::spawn_with_plugins().await;
    let token = admin(&a).await;
    upload(&a, &token, archive(BUNDLED, "0.2.0")).await;
    let res = a
        .post_with_token(&routes::admin_plugin_disable(BUNDLED), &json!({}), &token)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);

    let res = a
        .delete_with_token(&routes::admin_plugin_upload(BUNDLED), &token)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    let published = row(&a, BUNDLED).await.unwrap();
    assert!(!published.is_enabled);
    assert_eq!(published.archive_hash, None);
    assert_eq!(registered(&a, BUNDLED), ("0.1.0".into(), fixture(BUNDLED)));
    assert_eq!(status(&a, BUNDLED), Some(PluginStatus::Unloaded));
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_an_upload_with_no_bundled_copy_removes_the_plugin_everywhere() {
    let a = TestApp::spawn_with_plugins().await;
    let b = a.spawn_replica(SpawnOptions::default()).await;
    let token = admin(&a).await;
    upload(&a, &token, archive(UPLOAD_ONLY, "1.0.0")).await;
    plugin_lifecycle::reconcile(&b.state).await.unwrap();
    assert_eq!(status(&b, UPLOAD_ONLY), Some(PluginStatus::Loaded));

    let res = a
        .delete_with_token(&routes::admin_plugin_upload(UPLOAD_ONLY), &token)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert_eq!(res.body["outcome"], "removed", "{}", res.text);
    assert!(row(&a, UPLOAD_ONLY).await.is_none());
    assert_eq!(status(&a, UPLOAD_ONLY), None);

    plugin_lifecycle::reconcile(&b.state).await.unwrap();
    assert_eq!(status(&b, UPLOAD_ONLY), None, "B unloads and forgets it");
    assert!(cache_entries(&b).is_empty(), "{:?}", cache_entries(&b));
    let res = b.get_with_token(routes::ADMIN_PLUGINS, &token).await;
    assert!(
        !res.body
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == UPLOAD_ONLY),
        "{}",
        res.text
    );
    assert!(
        !plugin_lifecycle::not_ready(&b.state)
            .iter()
            .any(|p| p.id == UPLOAD_ONLY)
    );
}

/// A server with uploads switched off refuses archives and refuses to run
/// an uploaded version the rest of the fleet published, failing closed.
#[tokio::test(flavor = "multi_thread")]
async fn a_server_with_uploads_disabled_runs_only_bundled_code() {
    let a = TestApp::spawn_with_plugins().await;
    let locked = a
        .spawn_replica(SpawnOptions {
            plugin_uploads_disabled: true,
            allow_plugin_activation_failures: true,
            ..Default::default()
        })
        .await;
    let token = admin(&a).await;

    let res = locked
        .get_with_token(routes::ADMIN_PLUGIN_UPLOAD, &token)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert_eq!(res.body["enabled"], false, "{}", res.text);
    // Refused before the body is read, so the body is kept tiny: a large
    // one could still be in flight when the server answers.
    for path in [
        routes::ADMIN_PLUGIN_UPLOAD_INSPECT,
        routes::ADMIN_PLUGIN_UPLOAD,
    ] {
        let res = locked
            .post_plugin_archive(path, tiny_archive(), &token)
            .await;
        assert_eq!(res.status, 403, "{path}: {}", res.text);
        assert_eq!(res.body["code"], "PLUGIN_UPLOAD_DISABLED", "{}", res.text);
    }

    upload(&a, &token, archive(UPLOAD_ONLY, "1.0.0")).await;
    let failures = plugin_lifecycle::reconcile(&locked.state).await.unwrap();
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0]
            .error
            .to_string()
            .contains("uploads are disabled")
    );
    assert_ne!(status(&locked, UPLOAD_ONLY), Some(PluginStatus::Loaded));
    assert!(cache_entries(&locked).is_empty());
    let res = a
        .get_with_token(&routes::admin_plugin_details(UPLOAD_ONLY), &token)
        .await;
    let report = replica(&res.body["fleet"], &locked);
    assert_eq!(report["status"], "not_installed", "{report}");
    assert!(
        report["error"]
            .as_str()
            .is_some_and(|e| e.contains("uploads are disabled")),
        "the report says why: {report}"
    );
    assert!(
        plugin_lifecycle::not_ready(&locked.state)
            .iter()
            .any(|p| p.id == UPLOAD_ONLY),
        "an enabled plugin it cannot run makes it not ready"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn contestants_cannot_upload_or_remove_plugins() {
    let a = TestApp::spawn_with_plugins().await;
    let contestant = a
        .create_user_with_role("upload_contestant", "securepass", "contestant")
        .await;
    for path in [
        routes::ADMIN_PLUGIN_UPLOAD_INSPECT,
        routes::ADMIN_PLUGIN_UPLOAD,
    ] {
        let res = a
            .post_plugin_archive(path, tiny_archive(), &contestant)
            .await;
        assert_eq!(res.status, 403, "{path}: {}", res.text);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }
    let res = a
        .get_with_token(routes::ADMIN_PLUGIN_UPLOAD, &contestant)
        .await;
    assert_eq!(res.status, 403);
    let res = a
        .delete_with_token(&routes::admin_plugin_upload(BUNDLED), &contestant)
        .await;
    assert_eq!(res.status, 403);
    assert!(row(&a, UPLOAD_ONLY).await.is_none());
}

/// An id from the URL is a path segment only once it is a valid plugin id:
/// a decoded `../` can never point the lifecycle at another directory, such
/// as this server's upload cache.
#[tokio::test(flavor = "multi_thread")]
async fn plugin_ids_from_the_url_must_be_valid() {
    let a = TestApp::spawn_with_plugins().await;
    let token = admin(&a).await;
    let body = upload(&a, &token, archive(UPLOAD_ONLY, "1.0.0")).await;
    let hash = body["archive_hash"].as_str().unwrap();
    let cache = a.plugin_cache.path().display().to_string();
    let escape = format!("..{cache}/{hash}/{UPLOAD_ONLY}").replace('/', "%2F");

    for path in [
        routes::admin_plugin_enable(&escape),
        routes::admin_plugin_reload(&escape),
        routes::admin_plugin_disable(&escape),
    ] {
        let res = a.post_with_token(&path, &json!({}), &token).await;
        assert_eq!(res.status, 400, "{path}: {}", res.text);
        assert_eq!(res.body["code"], "VALIDATION_ERROR", "{}", res.text);
    }
    let res = a
        .delete_with_token(&routes::admin_plugin_upload(&escape), &token)
        .await;
    assert_eq!(res.status, 400, "{}", res.text);
    let res = a
        .get_with_token(&routes::admin_plugin_details(&escape), &token)
        .await;
    assert_eq!(res.status, 400, "{}", res.text);

    let rows = plugin_entity::Entity::find().all(&a.db).await.unwrap();
    assert!(
        rows.iter()
            .all(|r| server::plugin_id::PluginId::parse(r.id.as_str()).is_ok()),
        "{rows:?}"
    );
}
