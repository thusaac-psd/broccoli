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

    pub(crate) fn extend(&mut self, other: TranslationMap) {
        self.0.extend(other.0);
    }
}

impl From<HashMap<String, String>> for TranslationMap {
    fn from(value: HashMap<String, String>) -> Self {
        Self(value)
    }
}

pub type I18nData = HashMap<String, TranslationMap>;

/// Whether `key` in `plugin_id`'s pack names what its manifest declares (the
/// key shapes `i18n/labels.ts` in the web SDK reads): the plugin's own name,
/// description and config text, and the identifiers it registers.
fn is_manifest_text(plugin_id: &str, key: &str) -> bool {
    key.starts_with("identifier.")
        || [("plugin.", plugin_id), ("config.", plugin_id)]
            .iter()
            .any(|(kind, id)| {
                key.strip_prefix(kind)
                    .and_then(|rest| rest.strip_prefix(id))
                    .is_some_and(|rest| rest.starts_with('.'))
            })
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
pub fn served_translations<'a>(plugins: impl IntoIterator<Item = InstalledPacks<'a>>) -> I18nData {
    let mut merged = I18nData::new();
    let mut idle = Vec::new();
    for plugin in plugins {
        if plugin.loaded {
            for (lang, map) in plugin.packs {
                merged.entry(lang.clone()).or_default().extend(map.clone());
            }
        } else {
            idle.push(plugin);
        }
    }
    for plugin in idle {
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

    pub fn merge(&self, lang: String, translation: TranslationMap) {
        let mut data = self.data.write();
        let entry = data.entry(lang).or_default();
        entry.extend(translation);
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

    use super::{I18nRegistry, InstalledPacks, TranslationMap, served_translations};

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
    fn merge_preserves_existing_locale_entries() {
        let registry = I18nRegistry::new();

        registry.merge(
            "en".to_string(),
            TranslationMap::from(HashMap::from([(
                "limit.result".to_string(),
                "Result".to_string(),
            )])),
        );
        registry.merge(
            "en".to_string(),
            TranslationMap::from(HashMap::from([(
                "ioi.tokenPanel.title".to_string(),
                "Tokens".to_string(),
            )])),
        );

        let translations = registry.get_translations("en").unwrap();
        let expected = HashMap::from([
            ("limit.result".to_string(), "Result".to_string()),
            ("ioi.tokenPanel.title".to_string(), "Tokens".to_string()),
        ]);

        assert_eq!(translations.0, expected);
    }
}
