//! Every host code reads in the user's language: each language pack under
//! `plugins/` (a plugin whose manifest declares only translations) has an
//! `apiError.<CODE>` text for every [`HostCode`] that has its own text.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use broccoli_api_types::error::HostCode;

/// Manifest tables that do not stop a plugin being a language pack.
const PACK_TABLES: [&str; 2] = ["translations", "bundle"];

/// Each language pack's locale and catalog file.
fn language_packs(plugins: &Path) -> BTreeMap<String, PathBuf> {
    let mut packs = BTreeMap::new();
    for entry in std::fs::read_dir(plugins).expect("read plugins/") {
        let dir = entry.expect("plugin dir").path();
        let Ok(text) = std::fs::read_to_string(dir.join("plugin.toml")) else {
            continue;
        };
        let manifest: toml::Table = text.parse().expect("plugin.toml parses");
        let is_pack = manifest
            .iter()
            .filter(|(_, value)| value.is_table())
            .all(|(table, _)| PACK_TABLES.contains(&table.as_str()));
        let Some(translations) = manifest.get("translations").and_then(|t| t.as_table()) else {
            continue;
        };
        if !is_pack {
            continue;
        }
        for (locale, file) in translations {
            let file = file.as_str().expect("catalog path is a string");
            packs.insert(locale.clone(), dir.join(file));
        }
    }
    packs
}

#[test]
fn every_language_pack_translates_every_host_code() {
    let plugins = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    let packs = language_packs(&plugins);
    assert!(packs.contains_key("en"), "no language pack translates en");
    for (locale, file) in &packs {
        let catalog: toml::Table = std::fs::read_to_string(file)
            .expect("read catalog")
            .parse()
            .expect("catalog parses");
        let missing: Vec<String> = HostCode::ALL
            .iter()
            .filter(|code| code.has_own_text())
            .map(|code| format!("apiError.{}", code.as_str()))
            .filter(|key| !catalog.contains_key(key))
            .collect();
        assert!(
            missing.is_empty(),
            "the {locale} pack ({}) has no text for: {missing:?}",
            file.display()
        );
    }
}

#[test]
fn a_host_code_serializes_as_its_wire_string() {
    for code in HostCode::ALL {
        assert_eq!(
            serde_json::to_value(code).unwrap(),
            serde_json::Value::String(code.as_str().to_owned())
        );
    }
}
