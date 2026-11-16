//! Checks a plugin's own tests run against its manifest and packs.

use std::path::Path;

use broccoli_types::manifest::PluginManifest;
use broccoli_types::types::{RefusalCode, SdkRefusal};

/// Assert that every refusal code of `C`, and every code the SDK refuses the
/// plugin's routes with ([`SdkRefusal`]), has a text under
/// `<plugin id>.apiError.<CODE>` in every pack of the plugin whose tests run:
/// the packs its `plugin.toml` declares under `[translations]`, read from the
/// crate's directory (`CARGO_MANIFEST_DIR`).
pub fn assert_refusal_codes_translated<C: RefusalCode>() {
    let dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("cargo runs tests with CARGO_MANIFEST_DIR");
    let dir = Path::new(&dir);
    let manifest_path = dir.join("plugin.toml");
    let manifest: PluginManifest = toml::from_str(
        &std::fs::read_to_string(&manifest_path)
            .unwrap_or_else(|e| panic!("{}: {e}", manifest_path.display())),
    )
    .unwrap_or_else(|e| panic!("{} does not parse: {e}", manifest_path.display()));
    assert!(
        !manifest.translations.is_empty(),
        "{} declares no packs for its refusal codes",
        manifest_path.display()
    );
    let codes = C::ALL
        .iter()
        .map(|code| code.as_str())
        .chain(SdkRefusal::ALL.iter().map(|code| code.as_str()));
    let codes: Vec<&str> = codes.collect();
    let mut missing = Vec::new();
    for (locale, file) in &manifest.translations {
        let path = dir.join(file);
        let pack: toml::Table = toml::from_str(
            &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
        )
        .unwrap_or_else(|e| panic!("{} does not parse: {e}", path.display()));
        for code in &codes {
            let key = format!("{}.apiError.{code}", manifest.name);
            if !pack.get(&key).is_some_and(|text| text.is_str()) {
                missing.push(format!("{locale}: {key}"));
            }
        }
    }
    missing.sort();
    assert!(
        missing.is_empty(),
        "refusal codes without a text: {missing:#?}"
    );
}
