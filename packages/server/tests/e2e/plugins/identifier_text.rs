//! Every key a shipped plugin provides is named in the user's language.

use broccoli_api_types::visibility::Provided;
use server::providers::Loaded;

use crate::common::E2eTestApp;

/// Each kind of key a plugin names in its own packs, with the kind as its
/// identifier keys write it (`identifier.<kind>.<key>`). A language and a
/// checker language carry the display name their resolver registers, so no
/// pack names them.
const NAMED_BY_PACKS: [(Provided, &str); 3] = [
    (Provided::ContestType, "contestType"),
    (Provided::ProblemType, "problemType"),
    (Provided::CheckerFormat, "checker"),
];

/// Every contest type, problem type and checker format a shipped plugin
/// registers has its `identifier.<kind>.<key>` text in every pack the plugin
/// ships, so no picker or page shows a raw key in any language.
#[tokio::test(flavor = "multi_thread")]
async fn every_key_a_shipped_plugin_provides_is_named_in_every_pack_it_ships() {
    let app = E2eTestApp::spawn().await;
    let state = app.state();
    let loaded = Loaded::here(&state.registries).await;
    let registry = state.plugins.get_registry().read().unwrap();

    let mut checked = 0;
    let mut missing = Vec::new();
    for (kind, kind_key) in NAMED_BY_PACKS {
        for (key, plugin_ids) in loaded.of(kind) {
            for plugin_id in plugin_ids {
                let entry = registry
                    .get(plugin_id)
                    .unwrap_or_else(|| panic!("{plugin_id} registered {key} but is not loaded"));
                let text = format!("identifier.{kind_key}.{key}");
                if entry.translations.is_empty() {
                    missing.push(format!("{plugin_id} ships no pack to name {text}"));
                }
                for (locale, pack) in &entry.translations {
                    checked += 1;
                    if pack.get(&text).is_none() {
                        missing.push(format!("{plugin_id} ({locale}) has no {text}"));
                    }
                }
            }
        }
    }
    assert!(checked > 0, "the shipped plugins register keys");
    assert!(missing.is_empty(), "{}", missing.join("\n"));
}
