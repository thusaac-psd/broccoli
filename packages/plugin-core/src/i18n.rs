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

    fn extend(&mut self, other: TranslationMap) {
        self.0.extend(other.0);
    }
}

impl From<HashMap<String, String>> for TranslationMap {
    fn from(value: HashMap<String, String>) -> Self {
        Self(value)
    }
}

pub type I18nData = HashMap<String, TranslationMap>;

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

    use super::{I18nRegistry, TranslationMap};

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
