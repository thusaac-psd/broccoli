//! Derives the host's message key namespaces from its source catalog: the
//! translation file of the language pack under `plugins/` that declares
//! `[language_pack] locale = "en"`, the catalog every other language
//! translates. Every first segment of one of its keys is a namespace the host
//! speaks in, so no plugin may take it as its id (see
//! `plugin_id::is_host_namespace`). Writes `$OUT_DIR/host_namespaces.rs` and
//! names the catalog in `HOST_CATALOG`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const SOURCE_LOCALE: &str = "en";

fn main() {
    let plugins = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    println!("cargo:rerun-if-changed={}", plugins.display());
    let catalog = source_catalog(&plugins);
    println!("cargo:rerun-if-changed={}", catalog.display());
    println!("cargo:rustc-env=HOST_CATALOG={}", catalog.display());
    let text = std::fs::read_to_string(&catalog)
        .unwrap_or_else(|e| panic!("read the host catalog {}: {e}", catalog.display()));
    let keys: toml::Table = text
        .parse()
        .unwrap_or_else(|e| panic!("parse the host catalog {}: {e}", catalog.display()));
    let namespaces: BTreeSet<&str> = keys
        .keys()
        .map(|key| key.split('.').next().unwrap_or(key))
        .collect();
    let entries: String = namespaces
        .iter()
        .map(|ns| format!("    {ns:?},\n"))
        .collect();
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    std::fs::write(
        out.join("host_namespaces.rs"),
        format!(
            "/// The first segments of the host catalog's keys, sorted.\n\
             pub(crate) const HOST_CATALOG_NAMESPACES: &[&str] = &[\n{entries}];\n"
        ),
    )
    .expect("write host_namespaces.rs");
}

/// The translation file of the language pack that provides [`SOURCE_LOCALE`].
fn source_catalog(plugins: &Path) -> PathBuf {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(plugins).expect("read plugins/") {
        let dir = entry.expect("plugin directory").path();
        let manifest_path = dir.join("plugin.toml");
        let Ok(text) = std::fs::read_to_string(&manifest_path) else {
            continue;
        };
        println!("cargo:rerun-if-changed={}", manifest_path.display());
        let manifest: toml::Table = text
            .parse()
            .unwrap_or_else(|e| panic!("parse {}: {e}", manifest_path.display()));
        let locale = manifest
            .get("language_pack")
            .and_then(|pack| pack.get("locale"))
            .and_then(|locale| locale.as_str());
        if locale != Some(SOURCE_LOCALE) {
            continue;
        }
        let file = manifest
            .get("translations")
            .and_then(|t| t.get(SOURCE_LOCALE))
            .and_then(|file| file.as_str())
            .unwrap_or_else(|| {
                panic!(
                    "{} declares no translation file for {SOURCE_LOCALE}",
                    manifest_path.display()
                )
            });
        found.push(dir.join(file));
    }
    match found.as_slice() {
        [catalog] => catalog.clone(),
        _ => {
            panic!("expected one language pack for {SOURCE_LOCALE} under plugins/, found {found:?}")
        }
    }
}
