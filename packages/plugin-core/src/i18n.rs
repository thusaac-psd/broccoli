use std::{collections::HashMap, sync::Arc};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone, utoipa::ToSchema, Default)]
#[schema(example = json!({
    "sidebar.problems": "题目",
    "sidebar.plugins": "插件"
}))]
pub struct TranslationMap(HashMap<String, String>);

impl TranslationMap {
    pub fn new() -> Self {
        Self(HashMap::new())
    }
}

impl From<HashMap<String, String>> for TranslationMap {
    fn from(value: HashMap<String, String>) -> Self {
        Self(value)
    }
}

pub type I18nData = HashMap<String, TranslationMap>;

/// The kinds of identifier a plugin registers and names in its pack
/// (`identifier.<kind>.<id>`); the host names the other kinds itself.
const PLUGIN_IDENTIFIER_KINDS: [&str; 3] = ["contestType", "problemType", "checker"];

/// Whether `key` in `plugin_id`'s pack names what its manifest declares (the
/// key shapes `i18n/labels.ts` in the web SDK reads): the plugin's own name,
/// description and config text, and the identifiers it registers.
fn is_manifest_text(plugin_id: &str, key: &str) -> bool {
    key.strip_prefix("identifier.")
        .and_then(|rest| rest.split_once('.'))
        .is_some_and(|(kind, _)| PLUGIN_IDENTIFIER_KINDS.contains(&kind))
        || [("plugin.", plugin_id), ("config.", plugin_id)]
            .iter()
            .any(|(kind, id)| under(key.strip_prefix(kind), id))
}

/// Whether `key` (with its prefix stripped) is `id` followed by a `.`.
fn under(key: Option<&str>, id: &str) -> bool {
    key.and_then(|rest| rest.strip_prefix(id))
        .is_some_and(|rest| rest.starts_with('.'))
}

/// The keys of `plugin_id`'s pack that it may not provide, sorted. A plugin
/// speaks only for itself: its keys are its own (`<plugin_id>.…`) or its
/// manifest text (`plugin.<id>.…`, `config.<id>.…` and the names of the
/// identifiers it registers), so it can never reword a host string
/// or another plugin's. A language pack carries the host's strings, so every
/// key is its own.
pub fn foreign_keys<'a>(
    plugin_id: &str,
    manifest: &crate::manifest::PluginManifest,
    map: &'a TranslationMap,
) -> Vec<&'a str> {
    if manifest.is_language_pack() {
        return Vec::new();
    }
    let mut foreign: Vec<&str> = map
        .0
        .keys()
        .map(String::as_str)
        .filter(|key| !under(Some(key), plugin_id) && !is_manifest_text(plugin_id, key))
        .collect();
    foreign.sort_unstable();
    foreign
}

/// One installed plugin's packs, as [`served_translations`] reads them.
pub struct InstalledPacks<'a> {
    pub plugin_id: &'a str,
    pub loaded: bool,
    pub packs: &'a [(String, TranslationMap)],
}

/// The translations the host serves: every loaded plugin's packs, and the
/// manifest text of the plugins installed but not loaded (disabled, or failed
/// to load), so the UI names a plugin the same way whether it runs or not. A
/// plugin that is not loaded adds no locale and overrides no loaded key.
///
/// Plugins are merged in id order and a key two plugins provide is the first
/// one's, so every replica serves the same text whatever order it loaded its
/// plugins in. Only language packs and identifier names can overlap: every
/// other key is under its plugin's id ([`foreign_keys`]).
pub fn served_translations<'a>(plugins: impl IntoIterator<Item = InstalledPacks<'a>>) -> I18nData {
    let mut plugins: Vec<InstalledPacks<'a>> = plugins.into_iter().collect();
    plugins.sort_by_key(|plugin| plugin.plugin_id);
    let mut merged = I18nData::new();
    for plugin in plugins.iter().filter(|plugin| plugin.loaded) {
        for (lang, map) in plugin.packs {
            let served = merged.entry(lang.clone()).or_default();
            for (key, text) in &map.0 {
                served.0.entry(key.clone()).or_insert_with(|| text.clone());
            }
        }
    }
    for plugin in plugins.iter().filter(|plugin| !plugin.loaded) {
        for (lang, map) in plugin.packs {
            let Some(served) = merged.get_mut(lang) else {
                continue;
            };
            for (key, text) in &map.0 {
                if is_manifest_text(plugin.plugin_id, key) {
                    served.0.entry(key.clone()).or_insert_with(|| text.clone());
                }
            }
        }
    }
    merged
}

#[derive(Default)]
pub struct I18nRegistry {
    data: Arc<RwLock<I18nData>>,
}

impl I18nRegistry {
    pub fn new() -> Self {
        Self {
            data: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn clear(&self) {
        self.data.write().clear();
    }

    /// Serve `data` from now on, in place of everything before.
    pub fn replace(&self, data: I18nData) {
        *self.data.write() = data;
    }

    pub fn get_locales(&self) -> Vec<String> {
        self.data.read().keys().cloned().collect::<Vec<String>>()
    }

    pub fn get_translations(&self, lang: &str) -> Option<TranslationMap> {
        self.data.read().get(lang).cloned()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{InstalledPacks, TranslationMap, foreign_keys, served_translations};

    fn pack(lang: &str, entries: &[(&str, &str)]) -> (String, TranslationMap) {
        (
            lang.to_string(),
            TranslationMap::from(
                entries
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect::<HashMap<_, _>>(),
            ),
        )
    }

    #[test]
    fn a_plugin_that_is_not_loaded_keeps_its_manifest_text_in_every_served_locale() {
        let host = [pack("zh-CN", &[("sidebar.plugins", "插件")])];
        let disabled = [
            pack(
                "zh-CN",
                &[
                    ("plugin.ioi.name", "IOI 赛制"),
                    ("plugin.ioi.description", "子任务计分"),
                    ("config.ioi.task.description", "子任务"),
                    ("identifier.contestType.ioi", "IOI"),
                    ("ioi.tokenPanel.title", "令牌"),
                    ("plugin.ioix.name", "冒名"),
                    ("sidebar.plugins", "改写"),
                ],
            ),
            pack("fr", &[("plugin.ioi.name", "IOI")]),
        ];
        let served = served_translations([
            InstalledPacks {
                plugin_id: "ioi",
                loaded: false,
                packs: &disabled,
            },
            InstalledPacks {
                plugin_id: "broccoli-zh-cn",
                loaded: true,
                packs: &host,
            },
        ]);
        assert_eq!(served.keys().collect::<Vec<_>>(), vec!["zh-CN"]);
        let expected = HashMap::from([
            ("sidebar.plugins".to_string(), "插件".to_string()),
            ("plugin.ioi.name".to_string(), "IOI 赛制".to_string()),
            (
                "plugin.ioi.description".to_string(),
                "子任务计分".to_string(),
            ),
            (
                "config.ioi.task.description".to_string(),
                "子任务".to_string(),
            ),
            ("identifier.contestType.ioi".to_string(), "IOI".to_string()),
        ]);
        assert_eq!(served["zh-CN"].0, expected);
    }

    #[test]
    fn a_key_two_plugins_provide_is_the_same_whatever_order_they_load_in() {
        let a = [pack("en", &[("identifier.checker.lines", "Lines (a)")])];
        let b = [pack("en", &[("identifier.checker.lines", "Lines (b)")])];
        let served = |order: [(&'static str, &[(String, TranslationMap)]); 2]| {
            served_translations(order.map(|(plugin_id, packs)| InstalledPacks {
                plugin_id,
                loaded: true,
                packs,
            }))
        };
        let forward = served([("a", &a), ("b", &b)]);
        let reverse = served([("b", &b), ("a", &a)]);
        assert_eq!(forward["en"].0, reverse["en"].0);
        assert_eq!(forward["en"].0["identifier.checker.lines"], "Lines (a)");
    }

    #[test]
    fn a_plugin_provides_only_its_own_keys() {
        let manifest: crate::manifest::PluginManifest = toml::from_str(
            r#"
name = "cooldown"
version = "0.1.0"

[web]
root = "frontend/dist"
entry = "index.js"

[translations]
en = "i18n/en.toml"
"#,
        )
        .unwrap();
        let (_, map) = pack(
            "en",
            &[
                ("cooldown.ready", "Ready"),
                ("cooldown.apiError.COOLDOWN_ACTIVE", "Wait"),
                ("plugin.cooldown.name", "Cooldown"),
                ("config.cooldown.contest.description", "Gap"),
                ("identifier.contestType.cooldown", "Cooldown"),
                ("apiError.COOLDOWN_ACTIVE", "Wait"),
                ("apiError.TOKEN_INVALID", "Signed out"),
                ("sidebar.plugins", "Hijacked"),
                ("ioi.tokenPanel.title", "Hijacked"),
                ("cooldownx.ready", "Hijacked"),
                ("plugin.ioi.name", "Hijacked"),
                ("identifier.role.admin", "Hijacked"),
            ],
        );
        assert_eq!(
            foreign_keys("cooldown", &manifest, &map),
            [
                "apiError.COOLDOWN_ACTIVE",
                "apiError.TOKEN_INVALID",
                "cooldownx.ready",
                "identifier.role.admin",
                "ioi.tokenPanel.title",
                "plugin.ioi.name",
                "sidebar.plugins",
            ]
        );
    }

    #[test]
    fn a_language_pack_provides_the_hosts_strings() {
        let manifest: crate::manifest::PluginManifest = toml::from_str(
            r#"
name = "ja-language"
version = "0.1.0"

[translations]
ja = "locales/ja.toml"
"#,
        )
        .unwrap();
        let (_, map) = pack("ja", &[("sidebar.plugins", "プラグイン")]);
        assert!(foreign_keys("broccoli-ja", &manifest, &map).is_empty());
    }
}
