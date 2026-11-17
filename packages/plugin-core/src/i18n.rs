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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IdentifierKind {
    ContestType,
    ProblemType,
    Checker,
}

impl IdentifierKind {
    const ALL: [IdentifierKind; 3] = [Self::ContestType, Self::ProblemType, Self::Checker];

    /// The kind as its keys name it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ContestType => "contestType",
            Self::ProblemType => "problemType",
            Self::Checker => "checker",
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/host_namespaces.rs"));

/// The namespace of identifier names (`identifier.<kind>.<id>`).
const IDENTIFIER: &str = "identifier";
/// The namespaces of a plugin's manifest text (`plugin.<id>.…`,
/// `config.<id>.…`).
const MANIFEST_TEXT: [&str; 2] = ["plugin", "config"];
/// The namespace of the host's error texts (`apiError.<CODE>`).
const API_ERROR: &str = "apiError";

/// Whether `id` is a namespace the host speaks in: the first segment of a key
/// of the host's catalog, or a key family the platform reads for every plugin
/// (identifier names, manifest text, error texts). A plugin's own keys are the
/// ones under its id, so a plugin with such an id would speak as the host; no
/// plugin may take one (compared ignoring ASCII case).
pub fn is_host_namespace(id: &str) -> bool {
    HOST_CATALOG_NAMESPACES
        .iter()
        .chain(&MANIFEST_TEXT)
        .chain(&[IDENTIFIER, API_ERROR])
        .any(|ns| ns.eq_ignore_ascii_case(id))
}

/// The identifier `key` names, when it is the name of an identifier a
/// plugin registers.
fn plugin_identifier(key: &str) -> Option<(IdentifierKind, &str)> {
    let (kind, id) = key
        .strip_prefix(IDENTIFIER)?
        .strip_prefix('.')?
        .split_once('.')?;
    IdentifierKind::ALL
        .into_iter()
        .find(|k| k.as_str() == kind)
        .map(|kind| (kind, id))
}

/// Whether `key` in `plugin_id`'s pack is its manifest text (the key shapes
/// `i18n/labels.ts` in the web SDK reads): the plugin's own name,
/// description and config text, which the admin pages show for a plugin
/// whether it runs or not.
fn is_manifest_text(plugin_id: &str, key: &str) -> bool {
    MANIFEST_TEXT.iter().any(|ns| {
        key.strip_prefix(ns)
            .and_then(|rest| rest.strip_prefix('.'))
            .is_some_and(|rest| under(Some(rest), plugin_id))
    })
}

/// Whether `key` (with its prefix stripped) is `id` followed by a `.`.
fn under(key: Option<&str>, id: &str) -> bool {
    key.and_then(|rest| rest.strip_prefix(id))
        .is_some_and(|rest| rest.starts_with('.'))
}

/// The keys of `plugin_id`'s pack for `locale` that it may not provide,
/// sorted. A plugin speaks only for itself: its keys are its own
/// (`<plugin_id>.…`), its manifest text (`plugin.<id>.…`, `config.<id>.…`)
/// or the name of an identifier (`identifier.<kind>.…`, served only while it
/// registers that identifier, see [`served_translations`]), so it can never
/// reword a host string or another plugin's. A language pack's file for its
/// own locale carries the host's text, so every key of it is its own.
pub fn foreign_keys<'a>(
    plugin_id: &str,
    manifest: &crate::manifest::PluginManifest,
    locale: &str,
    map: &'a TranslationMap,
) -> Vec<&'a str> {
    if manifest.host_locale() == Some(locale) {
        return Vec::new();
    }
    let mut foreign: Vec<&str> = map
        .0
        .keys()
        .map(String::as_str)
        .filter(|key| {
            !under(Some(key), plugin_id)
                && !is_manifest_text(plugin_id, key)
                && plugin_identifier(key).is_none()
        })
        .collect();
    foreign.sort_unstable();
    foreign
}

/// One installed plugin's packs, as [`served_translations`] reads them.
pub struct InstalledPacks<'a> {
    pub plugin_id: &'a str,
    pub loaded: bool,
    /// The locale whose host text it provides, as a language pack.
    pub host_locale: Option<&'a str>,
    pub packs: &'a [(String, TranslationMap)],
}

/// Which plugin each registered identifier resolves to.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct IdentifierOwners(HashMap<(IdentifierKind, String), String>);

impl IdentifierOwners {
    /// Record that identifier `id` of `kind` resolves to `plugin_id`.
    pub fn insert(&mut self, kind: IdentifierKind, id: &str, plugin_id: &str) {
        self.0.insert((kind, id.to_string()), plugin_id.to_string());
    }

    fn owns(&self, plugin_id: &str, (kind, id): (IdentifierKind, &str)) -> bool {
        self.0
            .get(&(kind, id.to_string()))
            .is_some_and(|owner| owner == plugin_id)
    }
}

/// The translations the host serves, in every locale a loaded plugin
/// provides:
///
/// - a locale's host text is the language pack's that provides it. One
///   enabled pack provides a locale (enabling a second is refused); should
///   two loaded packs provide one anyway, neither does, so no pack ever
///   wins a locale by how its id sorts;
/// - every loaded plugin's own keys (`<plugin_id>.…`) and manifest text,
///   over a pack's text for the same key: a plugin speaks for itself;
/// - an identifier's name only from the plugin it resolves to (`owners`):
///   a plugin never renames another's contest type, problem type or checker;
/// - a plugin installed but not loaded (disabled, or failed to load) adds
///   only its manifest text, which the admin pages show for it, to the
///   locales the loaded plugins serve.
pub fn served_translations<'a>(
    plugins: impl IntoIterator<Item = InstalledPacks<'a>>,
    owners: &IdentifierOwners,
) -> I18nData {
    let plugins: Vec<InstalledPacks<'a>> = plugins.into_iter().collect();
    let mut merged = I18nData::new();
    let mut providers: HashMap<&str, Vec<&str>> = HashMap::new();
    for plugin in plugins.iter().filter(|plugin| plugin.loaded) {
        if let Some(locale) = plugin.host_locale {
            providers.entry(locale).or_default().push(plugin.plugin_id);
        }
        for (lang, _) in plugin.packs {
            merged.entry(lang.clone()).or_default();
        }
    }
    for (locale, packs) in &providers {
        let [pack] = packs.as_slice() else {
            tracing::error!(
                locale,
                packs = ?packs,
                "Several enabled language packs provide this locale; serving the host text of none"
            );
            continue;
        };
        let Some(served) = merged.get_mut(*locale) else {
            continue;
        };
        let texts = plugins
            .iter()
            .filter(|plugin| plugin.plugin_id == *pack)
            .flat_map(|plugin| plugin.packs)
            .filter(|(lang, _)| lang == locale)
            .flat_map(|(_, map)| &map.0);
        served.0.extend(texts.map(|(k, v)| (k.clone(), v.clone())));
    }
    for plugin in &plugins {
        let speaks = |key: &str| {
            is_manifest_text(plugin.plugin_id, key)
                || (plugin.loaded
                    && (under(Some(key), plugin.plugin_id)
                        || plugin_identifier(key)
                            .is_some_and(|identifier| owners.owns(plugin.plugin_id, identifier))))
        };
        for (lang, map) in plugin.packs {
            let Some(served) = merged.get_mut(lang) else {
                continue;
            };
            for (key, text) in &map.0 {
                if speaks(key) {
                    served.0.insert(key.clone(), text.clone());
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

    use super::{
        IdentifierKind, IdentifierOwners, InstalledPacks, TranslationMap, foreign_keys,
        served_translations,
    };

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

    fn loaded<'a>(plugin_id: &'a str, packs: &'a [(String, TranslationMap)]) -> InstalledPacks<'a> {
        InstalledPacks {
            plugin_id,
            loaded: true,
            host_locale: None,
            packs,
        }
    }

    fn language_pack<'a>(
        plugin_id: &'a str,
        locale: &'a str,
        packs: &'a [(String, TranslationMap)],
    ) -> InstalledPacks<'a> {
        InstalledPacks {
            host_locale: Some(locale),
            ..loaded(plugin_id, packs)
        }
    }

    fn manifest(source: &str) -> crate::manifest::PluginManifest {
        toml::from_str(source).unwrap()
    }

    #[test]
    fn a_plugin_that_is_not_loaded_serves_only_its_manifest_text() {
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
        let mut owners = IdentifierOwners::default();
        owners.insert(IdentifierKind::ContestType, "ioi", "ioi");
        let served = served_translations(
            [
                InstalledPacks {
                    plugin_id: "ioi",
                    loaded: false,
                    host_locale: None,
                    packs: &disabled,
                },
                language_pack("broccoli-zh-cn", "zh-CN", &host),
            ],
            &owners,
        );
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
        ]);
        assert_eq!(served["zh-CN"].0, expected);
    }

    #[test]
    fn an_identifier_is_named_only_by_the_plugin_it_resolves_to() {
        let a = [pack(
            "en",
            &[
                ("identifier.checker.lines", "Lines (a)"),
                ("identifier.contestType.icpc", "Renamed by a"),
            ],
        )];
        let b = [pack("en", &[("identifier.checker.lines", "Lines (b)")])];
        let mut owners = IdentifierOwners::default();
        owners.insert(IdentifierKind::Checker, "lines", "b");
        owners.insert(IdentifierKind::ContestType, "icpc", "icpc");
        let forward = served_translations([loaded("a", &a), loaded("b", &b)], &owners);
        let reverse = served_translations([loaded("b", &b), loaded("a", &a)], &owners);
        assert_eq!(forward["en"].0, reverse["en"].0);
        assert_eq!(
            forward["en"].0,
            HashMap::from([(
                "identifier.checker.lines".to_string(),
                "Lines (b)".to_string()
            )])
        );
    }

    /// One pack provides a locale's host text. Two loaded packs for one
    /// locale serve the host text of neither, whatever their ids: a pack
    /// that sorts first never takes the locale over.
    #[test]
    fn a_locale_two_language_packs_provide_serves_the_host_text_of_neither() {
        let real = [pack("en", &[("nav.signIn", "Sign in")])];
        let hijack = [pack("en", &[("nav.signIn", "HIJACKED SIGN IN")])];
        let owners = IdentifierOwners::default();
        let one = served_translations([language_pack("broccoli-en", "en", &real)], &owners);
        assert_eq!(one["en"].0["nav.signIn"], "Sign in");
        let two = served_translations(
            [
                language_pack("aaa-qa", "en", &hijack),
                language_pack("broccoli-en", "en", &real),
            ],
            &owners,
        );
        assert!(two["en"].0.is_empty(), "{:?}", two["en"].0);
    }

    /// Every namespace the host's catalog speaks in, and every key family
    /// the platform reads for all plugins, is refused as a plugin id.
    #[test]
    fn no_plugin_id_names_a_host_namespace() {
        let catalog: toml::Table = include_str!(env!("HOST_CATALOG")).parse().unwrap();
        assert!(!catalog.is_empty());
        for key in catalog.keys() {
            let namespace = key.split('.').next().unwrap();
            assert!(super::is_host_namespace(namespace), "{key}");
            assert!(
                super::is_host_namespace(&namespace.to_ascii_uppercase()),
                "{key}"
            );
        }
        for family in ["plugin", "config", "identifier", "apiError"] {
            assert!(super::is_host_namespace(family), "{family}");
        }
        for plugin in ["ioi", "icpc", "batch-evaluator", "apiErrors"] {
            assert!(!super::is_host_namespace(plugin), "{plugin}");
        }
    }

    #[test]
    fn a_plugin_provides_only_its_own_keys() {
        let manifest = manifest(
            r#"
name = "cooldown"
version = "0.1.0"

[web]
root = "frontend/dist"
entry = "index.js"

[translations]
en = "i18n/en.toml"
"#,
        );
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
            foreign_keys("cooldown", &manifest, "en", &map),
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

    /// A language pack's file for the locale it declares carries the host's
    /// text; a plugin that only ships translations, without declaring a
    /// locale, is confined to its own keys like any other.
    #[test]
    fn only_a_declared_language_pack_provides_the_hosts_strings() {
        let (_, map) = pack("ja", &[("sidebar.plugins", "プラグイン")]);
        let declared = manifest(
            r#"
name = "ja-language"
version = "0.1.0"

[language_pack]
locale = "ja"

[translations]
ja = "locales/ja.toml"
"#,
        );
        assert!(foreign_keys("broccoli-ja", &declared, "ja", &map).is_empty());
        assert_eq!(
            foreign_keys("broccoli-ja", &declared, "en", &map),
            ["sidebar.plugins"]
        );
        let undeclared = manifest(
            r#"
name = "aaa-qa"
version = "0.1.0"

[translations]
ja = "locales/ja.toml"
"#,
        );
        assert_eq!(
            foreign_keys("aaa-qa", &undeclared, "ja", &map),
            ["sidebar.plugins"]
        );
    }
}
