use crate::error::SdkError;
use crate::types::{ConfigResult, EffectiveConfig, resolve_cascade};
use serde_json::Value;

pub struct Config {
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) inner: ConfigMock,
}

impl Config {
    /// The config `ns` has at `problem_id`, in `contest_id` when given, as
    /// the shared cascade rule decides it ([`resolve_cascade`]); see
    /// [`crate::types::ConfigSource`] for where it comes from.
    pub fn get_effective(
        &self,
        ns: &str,
        problem_id: i32,
        contest_id: Option<i32>,
    ) -> Result<EffectiveConfig, SdkError> {
        let (cp, c) = match contest_id {
            Some(cid) => (
                Some(self.get_contest_problem(cid, problem_id, ns)?),
                Some(self.get_contest(cid, ns)?),
            ),
            None => (None, None),
        };
        let p = self.get_problem(problem_id, ns)?;
        Ok(resolve_cascade(cp, c, p, self.get_global(ns)?))
    }
}

#[cfg(target_arch = "wasm32")]
impl Config {
    pub fn get(&self, scope: &str, ref_id: &str, ns: &str) -> Result<ConfigResult, SdkError> {
        let input = crate::types::host_io::ConfigGetInput {
            scope: scope.to_string(),
            ref_id: ref_id.to_string(),
            namespace: ns.to_string(),
        };
        let result_json = unsafe { crate::host::raw::config_get(serde_json::to_string(&input)?)? };
        Ok(serde_json::from_str(&result_json)?)
    }

    pub fn set(&self, scope: &str, ref_id: &str, ns: &str, value: &Value) -> Result<(), SdkError> {
        let input = crate::types::host_io::ConfigSetInput {
            scope: scope.to_string(),
            ref_id: ref_id.to_string(),
            namespace: ns.to_string(),
            config: value.clone(),
        };
        unsafe { crate::host::raw::config_set(serde_json::to_string(&input)?)? };
        Ok(())
    }

    pub fn get_global(&self, ns: &str) -> Result<ConfigResult, SdkError> {
        self.get("plugin", "", ns)
    }

    pub fn set_global(&self, ns: &str, value: &Value) -> Result<(), SdkError> {
        self.set("plugin", "", ns, value)
    }

    pub fn get_problem(&self, problem_id: i32, ns: &str) -> Result<ConfigResult, SdkError> {
        self.get("problem", &problem_id.to_string(), ns)
    }

    pub fn set_problem(&self, problem_id: i32, ns: &str, value: &Value) -> Result<(), SdkError> {
        self.set("problem", &problem_id.to_string(), ns, value)
    }

    pub fn get_contest(&self, contest_id: i32, ns: &str) -> Result<ConfigResult, SdkError> {
        self.get("contest", &contest_id.to_string(), ns)
    }

    pub fn set_contest(&self, contest_id: i32, ns: &str, value: &Value) -> Result<(), SdkError> {
        self.set("contest", &contest_id.to_string(), ns, value)
    }

    pub fn get_contest_problem(
        &self,
        contest_id: i32,
        problem_id: i32,
        ns: &str,
    ) -> Result<ConfigResult, SdkError> {
        self.get("contest_problem", &format!("{contest_id}:{problem_id}"), ns)
    }

    pub fn set_contest_problem(
        &self,
        contest_id: i32,
        problem_id: i32,
        ns: &str,
        value: &Value,
    ) -> Result<(), SdkError> {
        self.set(
            "contest_problem",
            &format!("{contest_id}:{problem_id}"),
            ns,
            value,
        )
    }
}

/// The host's config store for native tests. Like the host, it hands out
/// each config with the defaults of the namespace's schema filled in, read
/// from the `plugin.toml` of the crate under test.
#[cfg(not(target_arch = "wasm32"))]
pub(super) struct ConfigMock {
    data: std::cell::RefCell<std::collections::HashMap<String, (Value, Option<bool>)>>,
    schemas: std::collections::HashMap<String, broccoli_types::manifest::ConfigNamespace>,
}

#[cfg(not(target_arch = "wasm32"))]
impl ConfigMock {
    pub fn new() -> Self {
        Self {
            data: std::cell::RefCell::new(std::collections::HashMap::new()),
            schemas: Self::crate_schemas(),
        }
    }

    /// The config namespaces of the plugin whose tests are running: cargo
    /// runs a test binary with `CARGO_MANIFEST_DIR` set to its crate.
    fn crate_schemas()
    -> std::collections::HashMap<String, broccoli_types::manifest::ConfigNamespace> {
        let Ok(dir) = std::env::var("CARGO_MANIFEST_DIR") else {
            return Default::default();
        };
        let path = std::path::Path::new(&dir).join("plugin.toml");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Default::default();
        };
        let manifest: broccoli_types::manifest::PluginManifest = toml::from_str(&text)
            .unwrap_or_else(|e| panic!("{} does not parse: {e}", path.display()));
        manifest.config
    }

    fn resolve(&self, ns: &str, stored: Option<&Value>) -> Value {
        match self.schemas.get(ns) {
            Some(schema) => schema.resolve(stored),
            None => stored
                .cloned()
                .unwrap_or_else(|| Value::Object(serde_json::Map::new())),
        }
    }

    fn key(scope: &str, ref_id: &str, ns: &str) -> String {
        format!("{scope}:{ref_id}:{ns}")
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Config {
    pub fn get(&self, scope: &str, ref_id: &str, ns: &str) -> Result<ConfigResult, SdkError> {
        let key = ConfigMock::key(scope, ref_id, ns);
        match self.inner.data.borrow().get(&key) {
            Some((config, enabled)) => Ok(ConfigResult {
                config: self.inner.resolve(ns, Some(config)),
                stored: Some(config.clone()),
                enabled: *enabled,
            }),
            None => Ok(ConfigResult {
                config: self.inner.resolve(ns, None),
                stored: None,
                enabled: None,
            }),
        }
    }

    pub fn set(&self, scope: &str, ref_id: &str, ns: &str, value: &Value) -> Result<(), SdkError> {
        let key = ConfigMock::key(scope, ref_id, ns);
        self.inner
            .data
            .borrow_mut()
            .insert(key, (value.clone(), Some(true)));
        Ok(())
    }

    pub fn get_global(&self, ns: &str) -> Result<ConfigResult, SdkError> {
        self.get("plugin", "", ns)
    }

    pub fn set_global(&self, ns: &str, value: &Value) -> Result<(), SdkError> {
        self.set("plugin", "", ns, value)
    }

    pub fn get_problem(&self, problem_id: i32, ns: &str) -> Result<ConfigResult, SdkError> {
        self.get("problem", &problem_id.to_string(), ns)
    }

    pub fn set_problem(&self, problem_id: i32, ns: &str, value: &Value) -> Result<(), SdkError> {
        self.set("problem", &problem_id.to_string(), ns, value)
    }

    pub fn get_contest(&self, contest_id: i32, ns: &str) -> Result<ConfigResult, SdkError> {
        self.get("contest", &contest_id.to_string(), ns)
    }

    pub fn set_contest(&self, contest_id: i32, ns: &str, value: &Value) -> Result<(), SdkError> {
        self.set("contest", &contest_id.to_string(), ns, value)
    }

    pub fn get_contest_problem(
        &self,
        contest_id: i32,
        problem_id: i32,
        ns: &str,
    ) -> Result<ConfigResult, SdkError> {
        self.get("contest_problem", &format!("{contest_id}:{problem_id}"), ns)
    }

    pub fn set_contest_problem(
        &self,
        contest_id: i32,
        problem_id: i32,
        ns: &str,
        value: &Value,
    ) -> Result<(), SdkError> {
        self.set(
            "contest_problem",
            &format!("{contest_id}:{problem_id}"),
            ns,
            value,
        )
    }

    pub fn seed(&self, scope: &str, ref_id: &str, ns: &str, value: Value) {
        let key = ConfigMock::key(scope, ref_id, ns);
        self.inner
            .data
            .borrow_mut()
            .insert(key, (value, Some(true)));
    }

    pub fn seed_with_enabled(
        &self,
        scope: &str,
        ref_id: &str,
        ns: &str,
        value: Value,
        enabled: Option<bool>,
    ) {
        let key = ConfigMock::key(scope, ref_id, ns);
        self.inner.data.borrow_mut().insert(key, (value, enabled));
    }
}

#[cfg(test)]
mod tests {
    use crate::types::ConfigSource;
    use serde_json::json;

    fn make_host() -> crate::sdk::Host {
        crate::sdk::Host::mock()
    }

    #[test]
    fn no_config_anywhere_returns_disabled() {
        let host = make_host();
        let eff = host.config.get_effective("ns", 1, Some(10)).unwrap();
        assert!(!eff.is_enabled);
        assert_eq!(eff.source, ConfigSource::Default);
    }

    #[test]
    fn problem_only_returns_problem() {
        let host = make_host();
        host.config.seed("problem", "1", "ns", json!({"v": 42}));

        let eff = host.config.get_effective("ns", 1, Some(10)).unwrap();
        assert!(eff.is_enabled);
        assert_eq!(eff.source, ConfigSource::Problem);
        assert_eq!(eff.config["v"], 42);
    }

    #[test]
    fn contest_overrides_problem() {
        let host = make_host();
        host.config.seed("problem", "1", "ns", json!({"v": 10}));
        host.config.seed("contest", "10", "ns", json!({"v": 20}));

        let eff = host.config.get_effective("ns", 1, Some(10)).unwrap();
        assert!(eff.is_enabled);
        assert_eq!(eff.source, ConfigSource::Contest);
        assert_eq!(eff.config["v"], 20);
    }

    #[test]
    fn contest_problem_overrides_contest() {
        let host = make_host();
        host.config.seed("contest", "10", "ns", json!({"v": 20}));
        host.config
            .seed("contest_problem", "10:1", "ns", json!({"v": 30}));

        let eff = host.config.get_effective("ns", 1, Some(10)).unwrap();
        assert!(eff.is_enabled);
        assert_eq!(eff.source, ConfigSource::ContestProblem);
        assert_eq!(eff.config["v"], 30);
    }

    #[test]
    fn disabled_at_most_specific_overrides_enabled_parent() {
        let host = make_host();
        host.config.seed("contest", "10", "ns", json!({"v": 20}));
        host.config.seed_with_enabled(
            "contest_problem",
            "10:1",
            "ns",
            json!({"v": 30}),
            Some(false),
        );

        let eff = host.config.get_effective("ns", 1, Some(10)).unwrap();
        assert!(!eff.is_enabled);
        assert_eq!(eff.source, ConfigSource::Disabled);
    }

    #[test]
    fn enabled_at_most_specific_overrides_disabled_parent() {
        let host = make_host();
        host.config
            .seed_with_enabled("contest", "10", "ns", json!({"v": 20}), Some(false));
        host.config
            .seed("contest_problem", "10:1", "ns", json!({"v": 30}));

        let eff = host.config.get_effective("ns", 1, Some(10)).unwrap();
        assert!(eff.is_enabled);
        assert_eq!(eff.source, ConfigSource::ContestProblem);
        assert_eq!(eff.config["v"], 30);
    }

    #[test]
    fn no_contest_context_only_checks_problem() {
        let host = make_host();
        host.config.seed("problem", "1", "ns", json!({"v": 42}));

        let eff = host.config.get_effective("ns", 1, None).unwrap();
        assert!(eff.is_enabled);
        assert_eq!(eff.source, ConfigSource::Problem);
        assert!(eff.levels.contest_problem.is_none());
        assert!(eff.levels.contest.is_none());
    }

    #[test]
    fn no_contest_context_no_config_returns_disabled() {
        let host = make_host();
        let eff = host.config.get_effective("ns", 1, None).unwrap();
        assert!(!eff.is_enabled);
        assert_eq!(eff.source, ConfigSource::Default);
    }

    #[test]
    fn levels_populated_for_all_scopes() {
        let host = make_host();
        host.config.seed("problem", "1", "ns", json!({"v": 10}));
        host.config.seed("contest", "10", "ns", json!({"v": 20}));

        let eff = host.config.get_effective("ns", 1, Some(10)).unwrap();
        assert!(eff.levels.contest_problem.as_ref().unwrap().is_default);
        assert!(!eff.levels.contest.as_ref().unwrap().is_default);
        assert!(!eff.levels.problem.is_default);
    }

    #[test]
    fn disabled_contest_with_enabled_problem() {
        let host = make_host();
        host.config
            .seed_with_enabled("contest", "10", "ns", json!({"v": 20}), Some(false));
        host.config.seed("problem", "1", "ns", json!({"v": 10}));

        let eff = host.config.get_effective("ns", 1, Some(10)).unwrap();
        assert!(!eff.is_enabled);
    }

    #[test]
    fn none_enabled_inherits_from_parent() {
        let host = make_host();
        host.config.seed("contest", "10", "ns", json!({"v": 60}));
        host.config
            .seed_with_enabled("contest_problem", "10:1", "ns", json!({"v": 30}), None);

        let eff = host.config.get_effective("ns", 1, Some(10)).unwrap();
        assert_eq!(eff.config["v"], 30);
        assert_eq!(eff.source, ConfigSource::ContestProblem);
        assert!(eff.is_enabled);
    }

    #[test]
    fn none_enabled_everywhere_means_not_enabled() {
        let host = make_host();
        host.config
            .seed_with_enabled("problem", "1", "ns", json!({"v": 10}), None);

        let eff = host.config.get_effective("ns", 1, None).unwrap();
        assert_eq!(eff.config["v"], 10);
        assert!(!eff.is_enabled);
    }

    #[test]
    fn the_plugin_scope_decides_when_no_narrower_scope_sets_the_namespace() {
        let host = make_host();
        host.config.seed("plugin", "", "ns", json!({"v": 99}));

        let eff = host.config.get_effective("ns", 1, Some(10)).unwrap();
        assert_eq!(eff.config["v"], 99);
        assert_eq!(eff.source, ConfigSource::Plugin);
        let eff = host.config.get_effective("ns", 1, None).unwrap();
        assert_eq!(eff.config["v"], 99);
    }

    #[test]
    fn a_narrower_scope_overrides_the_plugin_scope() {
        let host = make_host();
        host.config.seed("plugin", "", "ns", json!({"v": 99}));
        host.config.seed("problem", "1", "ns", json!({"v": 10}));

        let eff = host.config.get_effective("ns", 1, Some(10)).unwrap();
        assert_eq!(eff.config["v"], 10);
    }

    #[test]
    fn a_disabled_override_leaves_the_config_it_overrode() {
        let host = make_host();
        host.config
            .seed_with_enabled("problem", "1", "ns", json!({"v": 10}), Some(false));
        let eff = host.config.get_effective("ns", 1, None).unwrap();
        assert_eq!(eff.config, json!({}));

        host.config.seed("plugin", "", "ns", json!({"v": 99}));
        let eff = host.config.get_effective("ns", 1, None).unwrap();
        assert_eq!(eff.config["v"], 99);
        assert!(!eff.is_enabled);
    }

    /// A field a scope does not store comes from the next scope up, not from
    /// the schema's defaults: a problem that sets only Python's interpreter
    /// keeps the plugin-wide C++ compiler.
    #[test]
    fn a_scope_takes_the_fields_it_does_not_store_from_the_next_scope_up() {
        let host = make_host();
        host.config.seed(
            "plugin",
            "",
            "ns",
            json!({"cpp": {"compiler": "gcc-14", "flags": ["-std=c++20"]}, "python3": {"interpreter": "python3"}}),
        );
        host.config.seed(
            "problem",
            "1",
            "ns",
            json!({"python3": {"interpreter": "pypy3"}}),
        );
        host.config
            .seed("contest", "10", "ns", json!({"cpp": {"flags": ["-O2"]}}));

        let eff = host.config.get_effective("ns", 1, Some(10)).unwrap();
        assert_eq!(eff.source, ConfigSource::Contest);
        assert_eq!(
            eff.config,
            json!({"cpp": {"compiler": "gcc-14", "flags": ["-O2"]}, "python3": {"interpreter": "pypy3"}})
        );
    }

    #[test]
    fn none_enabled_at_cp_with_disabled_contest() {
        let host = make_host();
        host.config
            .seed_with_enabled("contest", "10", "ns", json!({"v": 60}), Some(false));
        host.config
            .seed_with_enabled("contest_problem", "10:1", "ns", json!({"v": 30}), None);

        let eff = host.config.get_effective("ns", 1, Some(10)).unwrap();
        assert_eq!(eff.config, json!({}));
        assert!(!eff.is_enabled);
        assert_eq!(eff.source, ConfigSource::Disabled);
    }
}
