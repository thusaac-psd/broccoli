use crate::error::SdkError;
use crate::types::{CascadeLevel, CascadeLevels, ConfigResult, ConfigSource, EffectiveConfig};
use serde_json::Value;

pub struct Config {
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) inner: ConfigMock,
}

fn resolve_cascade(
    cp: Option<ConfigResult>,
    c: Option<ConfigResult>,
    p: ConfigResult,
) -> EffectiveConfig {
    let levels = CascadeLevels {
        contest_problem: cp.as_ref().map(CascadeLevel::from),
        contest: c.as_ref().map(CascadeLevel::from),
        problem: CascadeLevel::from(&p),
    };

    let ordered: [(Option<&ConfigResult>, ConfigSource); 3] = [
        (cp.as_ref(), ConfigSource::ContestProblem),
        (c.as_ref(), ConfigSource::Contest),
        (Some(&p), ConfigSource::Problem),
    ];

    let first_explicit = ordered
        .iter()
        .find(|(r, _)| r.is_some_and(|r| !r.is_default));

    let Some((Some(most_specific), source)) = first_explicit else {
        return EffectiveConfig {
            config: p.config,
            source: ConfigSource::Default,
            is_enabled: false,
            levels,
        };
    };

    let resolved_enabled = ordered
        .iter()
        .filter_map(|(r, _)| r.and_then(|r| if !r.is_default { r.enabled } else { None }))
        .next();

    match resolved_enabled {
        Some(true) => EffectiveConfig {
            config: most_specific.config.clone(),
            source: source.clone(),
            is_enabled: true,
            levels,
        },
        Some(false) => EffectiveConfig {
            config: most_specific.config.clone(),
            source: ConfigSource::Disabled,
            is_enabled: false,
            levels,
        },
        None => EffectiveConfig {
            config: most_specific.config.clone(),
            source: source.clone(),
            is_enabled: false,
            levels,
        },
    }
}

#[cfg(target_arch = "wasm32")]
impl Config {
    pub fn get(&self, scope: &str, ref_id: &str, ns: &str) -> Result<ConfigResult, SdkError> {
        let input = serde_json::json!({
            "scope": scope,
            "ref_id": ref_id,
            "namespace": ns,
        });
        let result_json = unsafe { crate::host::raw::config_get(serde_json::to_string(&input)?)? };
        let result: Value = serde_json::from_str(&result_json)?;
        Ok(ConfigResult {
            config: result
                .get("config")
                .cloned()
                .unwrap_or(Value::Object(serde_json::Map::new())),
            is_default: result
                .get("is_default")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            enabled: result.get("enabled").and_then(|v| v.as_bool()),
        })
    }

    pub fn set(&self, scope: &str, ref_id: &str, ns: &str, value: &Value) -> Result<(), SdkError> {
        let input = serde_json::json!({
            "scope": scope,
            "ref_id": ref_id,
            "namespace": ns,
            "config": value,
        });
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
        Ok(resolve_cascade(cp, c, p))
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) struct ConfigMock {
    data: std::cell::RefCell<std::collections::HashMap<String, (Value, Option<bool>)>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl ConfigMock {
    pub fn new() -> Self {
        Self {
            data: std::cell::RefCell::new(std::collections::HashMap::new()),
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
                config: config.clone(),
                is_default: false,
                enabled: *enabled,
            }),
            None => Ok(ConfigResult {
                config: Value::Object(serde_json::Map::new()),
                is_default: true,
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
        Ok(resolve_cascade(cp, c, p))
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
    fn none_enabled_at_cp_with_disabled_contest() {
        let host = make_host();
        host.config
            .seed_with_enabled("contest", "10", "ns", json!({"v": 60}), Some(false));
        host.config
            .seed_with_enabled("contest_problem", "10:1", "ns", json!({"v": 30}), None);

        let eff = host.config.get_effective("ns", 1, Some(10)).unwrap();
        assert_eq!(eff.config["v"], 30);
        assert!(!eff.is_enabled);
        assert_eq!(eff.source, ConfigSource::Disabled);
    }
}
