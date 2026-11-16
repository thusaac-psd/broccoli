//! Every host code reads in the user's language: each language pack under
//! `plugins/` (a plugin whose manifest declares `[language_pack] locale`) has an
//! `apiError.<CODE>` text for every [`HostCode`] that has its own text.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use broccoli_api_types::error::HostCode;

/// Each language pack's locale and catalog file.
fn language_packs(plugins: &Path) -> BTreeMap<String, PathBuf> {
    let mut packs = BTreeMap::new();
    for entry in std::fs::read_dir(plugins).expect("read plugins/") {
        let dir = entry.expect("plugin dir").path();
        let Ok(text) = std::fs::read_to_string(dir.join("plugin.toml")) else {
            continue;
        };
        let manifest: toml::Table = text.parse().expect("plugin.toml parses");
        let Some(locale) = manifest
            .get("language_pack")
            .and_then(|pack| pack.get("locale"))
            .and_then(|locale| locale.as_str())
        else {
            continue;
        };
        let file = manifest
            .get("translations")
            .and_then(|t| t.get(locale))
            .and_then(|file| file.as_str())
            .expect("a language pack has a catalog for its locale");
        packs.insert(locale.to_string(), dir.join(file));
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
