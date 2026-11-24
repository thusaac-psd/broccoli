use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::SdkError;

/// One stored config as the host hands it to a plugin: the stored value with
/// every default its schema declares filled in, so it deserializes into the
/// plugin's config type without defaults of the plugin's own.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigResult {
    pub config: serde_json::Value,
    /// The row as stored, without defaults; `None` when nothing is stored
    /// and `config` is the schema's defaults. The cascade reads it to tell
    /// the fields a scope sets from the ones it leaves to the next scope up.
    pub stored: Option<serde_json::Value>,
    pub enabled: Option<bool>,
}

impl ConfigResult {
    /// Nothing is stored: `config` is the schema's defaults.
    pub fn is_default(&self) -> bool {
        self.stored.is_none()
    }

    /// The config as the plugin's config type. A config that does not
    /// deserialize is an error, never a silent fallback to some default.
    pub fn parse<T: serde::de::DeserializeOwned>(&self) -> Result<T, SdkError> {
        parse(&self.config)
    }
}

fn parse<T: serde::de::DeserializeOwned>(config: &serde_json::Value) -> Result<T, SdkError> {
    T::deserialize(config).map_err(|e| {
        SdkError::Serialization(format!(
            "config does not match the plugin's config type: {e}"
        ))
    })
}

/// The most specific scope whose row an effective config takes fields from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ConfigSource {
    ContestProblem,
    Contest,
    Problem,
    /// The deployment-wide row of a namespace that declares plugin scope.
    Plugin,
    /// No scope stores the namespace: the schema's defaults.
    Default,
    /// The most specific override is switched off: the config is what it
    /// overrode (the plugin-scope row, else the schema's defaults).
    Disabled,
}

/// One scope's row of a namespace, as the cascade read it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct CascadeLevel {
    /// The row with its schema's defaults filled in; the defaults alone when
    /// nothing is stored.
    pub config: serde_json::Value,
    /// The row as stored, without defaults; `None` when nothing is stored.
    pub stored: Option<serde_json::Value>,
    pub is_default: bool,
    pub enabled: Option<bool>,
}

impl From<&ConfigResult> for CascadeLevel {
    fn from(r: &ConfigResult) -> Self {
        Self {
            config: r.config.clone(),
            stored: r.stored.clone(),
            is_default: r.is_default(),
            enabled: r.enabled,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct CascadeLevels {
    pub contest_problem: Option<CascadeLevel>,
    pub contest: Option<CascadeLevel>,
    pub problem: CascadeLevel,
    pub plugin: CascadeLevel,
}

/// The config a plugin reads for a namespace at one problem, as
/// [`resolve_cascade`] decides it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct EffectiveConfig {
    pub config: serde_json::Value,
    pub source: ConfigSource,
    pub is_enabled: bool,
    /// Where each top-level field of `config` comes from: the scope whose
    /// row sets it, else the plugin-wide row, else the schema's defaults.
    #[serde(default)]
    pub fields: BTreeMap<String, ConfigSource>,
    pub levels: CascadeLevels,
}

/// Whether a namespace is on, from the `enabled` each scope's row says, most
/// specific first (contest problem, contest, problem, the plugin-wide row):
/// the first scope that says decides, and a row that says nothing leaves it
/// to the next scope up. `None` when none says, which is off. The one rule
/// for every reader: the cascade below, and the host's hook enablement.
pub fn decides_enabled(scopes: impl IntoIterator<Item = Option<bool>>) -> Option<bool> {
    scopes.into_iter().flatten().next()
}

/// The effective config of a namespace at one problem. Each scope that
/// stores the namespace sets the fields it stores and leaves the rest to the
/// next scope up: contest problem, contest, problem, the plugin-wide row, then
/// the schema's defaults. Whether it is on is
/// [`decides_enabled`] over the rows. A switched-off override contributes
/// nothing and leaves what it overrode, so when the narrower scope that
/// decides is switched off the config is the plugin-wide one.
///
/// The guest SDK reads configs through it, and the host answers the admin
/// forms with it, so both show and use the same config.
pub fn resolve_cascade(
    cp: Option<ConfigResult>,
    c: Option<ConfigResult>,
    p: ConfigResult,
    plugin: ConfigResult,
) -> EffectiveConfig {
    // Most specific first, only the scopes that store the namespace.
    let stored: Vec<(&ConfigResult, &serde_json::Value, ConfigSource)> = [
        (cp.as_ref(), ConfigSource::ContestProblem),
        (c.as_ref(), ConfigSource::Contest),
        (Some(&p), ConfigSource::Problem),
    ]
    .into_iter()
    .filter_map(|(r, source)| {
        let r = r?;
        Some((r, r.stored.as_ref()?, source))
    })
    .collect();
    let is_enabled = decides_enabled(
        [cp.as_ref(), c.as_ref(), Some(&p), Some(&plugin)]
            .into_iter()
            .map(|r| r.and_then(|r| r.enabled)),
    ) == Some(true);
    let narrower_decides = decides_enabled(stored.iter().map(|(r, _, _)| r.enabled));

    let base = if plugin.is_default() {
        ConfigSource::Default
    } else {
        ConfigSource::Plugin
    };
    let mut config = plugin.config.clone();
    let mut fields: BTreeMap<String, ConfigSource> = match &config {
        serde_json::Value::Object(map) => map
            .keys()
            .map(|key| {
                let set = plugin.stored.as_ref().and_then(|s| s.get(key)).is_some();
                (
                    key.clone(),
                    if set {
                        ConfigSource::Plugin
                    } else {
                        ConfigSource::Default
                    },
                )
            })
            .collect(),
        _ => BTreeMap::new(),
    };
    let source = match (stored.first(), narrower_decides) {
        (None, _) => base,
        (Some(_), Some(false)) => ConfigSource::Disabled,
        (Some((_, _, source)), _) => {
            for (row, raw, row_source) in stored.iter().rev() {
                if row.enabled == Some(false) {
                    continue;
                }
                overlay(&mut config, raw, Some(&row.config));
                if let serde_json::Value::Object(raw) = raw {
                    for key in raw.keys() {
                        fields.insert(key.clone(), *row_source);
                    }
                }
            }
            *source
        }
    };
    EffectiveConfig {
        config,
        source,
        is_enabled,
        fields,
        levels: CascadeLevels {
            contest_problem: cp.as_ref().map(CascadeLevel::from),
            contest: c.as_ref().map(CascadeLevel::from),
            problem: CascadeLevel::from(&p),
            plugin: CascadeLevel::from(&plugin),
        },
    }
}

/// Lay a narrower scope's stored row over the config of the scopes above it:
/// a field the row stores replaces the one above, except that two objects
/// merge field by field, and a field it does not store keeps the one above.
/// A value the row brings takes its schema defaults from `filled`, the row
/// as the host resolved it.
fn overlay(
    above: &mut serde_json::Value,
    stored: &serde_json::Value,
    filled: Option<&serde_json::Value>,
) {
    use serde_json::Value;
    match (above, stored) {
        (Value::Object(above), Value::Object(stored)) => {
            for (key, value) in stored {
                let filled = filled.and_then(|f| f.get(key));
                match above.get_mut(key) {
                    Some(slot) => overlay(slot, value, filled),
                    None => {
                        above.insert(key.clone(), filled.unwrap_or(value).clone());
                    }
                }
            }
        }
        (above, stored) => *above = filled.unwrap_or(stored).clone(),
    }
}

impl EffectiveConfig {
    /// See [`ConfigResult::parse`].
    pub fn parse_config<T: serde::de::DeserializeOwned>(&self) -> Result<T, SdkError> {
        parse(&self.config)
    }
}

/// What the host asks a namespace's `validate` function: may an admin store
/// `config` at this target? The host calls it after the value passed the
/// namespace's schema, so the function checks only what a schema cannot say,
/// such as whether the test labels a config names exist. It reads anything
/// else it needs through its usual host functions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigValidateInput {
    pub namespace: String,
    pub scope: crate::manifest::ConfigScope,
    /// The contest the config belongs to, at the `contest` and
    /// `contest_problem` scopes.
    pub contest_id: Option<i32>,
    /// The problem the config belongs to, at the `problem` and
    /// `contest_problem` scopes.
    pub problem_id: Option<i32>,
    /// The proposed value with every default its schema declares filled in,
    /// as the plugin would read it once stored.
    pub config: serde_json::Value,
}

impl ConfigValidateInput {
    /// The proposed config as the plugin's config type.
    pub fn parse<T: serde::de::DeserializeOwned>(&self) -> Result<T, SdkError> {
        parse(&self.config)
    }
}

/// A `validate` function's answer. `errors` is what is wrong with the config
/// itself, such as a subtask naming a test case the problem does not have:
/// the host refuses to store a config with any. `issues` is what the
/// target's data does not meet yet, such as a problem with no test cases:
/// the host stores the config and lists each issue on the target's config
/// issues until the data changes. Both are listed there when a config the
/// host already holds stops passing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConfigValidateOutput {
    #[serde(default)]
    pub errors: Vec<ConfigViolation>,
    #[serde(default)]
    pub issues: Vec<ConfigViolation>,
}

/// One reason a `validate` function refuses a config: where, the plugin's
/// English words (`message`, for logs and for a UI that finds no
/// translation), and optionally a key of the plugin's own language pack with
/// its parameters, which a client shows in the user's language.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigViolation {
    /// Where the offending value is, as `field.nested[2].leaf`; empty for the
    /// config as a whole.
    pub path: String,
    pub message: String,
    /// A key of the plugin's pack, under the plugin's id (`ioi.config.…`).
    /// The host drops a key outside it, so the message shows instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The values the key's message names.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, serde_json::Value>,
}

impl ConfigViolation {
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
            key: None,
            params: BTreeMap::new(),
        }
    }

    /// The same, said in the user's language by `key` of the plugin's pack.
    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// A value the key's message names.
    pub fn param(mut self, name: impl Into<String>, value: impl Into<serde_json::Value>) -> Self {
        self.params.insert(name.into(), value.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn row(stored: Option<Value>, config: Value, enabled: Option<bool>) -> ConfigResult {
        ConfigResult {
            config,
            stored,
            enabled,
        }
    }

    fn unset() -> ConfigResult {
        row(None, json!({}), None)
    }

    /// The defaults the host fills into a narrower row never hide what a
    /// wider scope stores; they reach the effective config only where no
    /// scope stores the field.
    #[test]
    fn a_narrower_rows_filled_defaults_never_hide_a_wider_scopes_field() {
        let row = |stored: Option<Value>, config: Value| row(stored, config, Some(true));
        let plugin = row(
            Some(json!({"cpp": {"compiler": "gcc-14"}})),
            json!({"cpp": {"compiler": "gcc-14", "flags": ["-O2"]}, "java": {"runner": "java"}}),
        );
        let problem = row(
            Some(json!({"cpp": {"flags": ["-O3"]}, "kotlin": {"compiler": "kotlinc-2"}})),
            json!({
                "cpp": {"compiler": "g++", "flags": ["-O3"]},
                "java": {"runner": "java"},
                "kotlin": {"compiler": "kotlinc-2", "flags": ["-Werror"]},
            }),
        );
        let eff = resolve_cascade(None, None, problem, plugin);
        assert_eq!(
            eff.config,
            json!({
                "cpp": {"compiler": "gcc-14", "flags": ["-O3"]},
                "java": {"runner": "java"},
                "kotlin": {"compiler": "kotlinc-2", "flags": ["-Werror"]},
            })
        );
    }

    /// The first scope whose row says decides, the plugin-wide row last; a
    /// row that stores fields but says nothing leaves it to the next scope.
    #[test]
    fn the_most_specific_scope_that_says_decides_whether_it_is_on() {
        assert_eq!(
            decides_enabled([None, Some(false), Some(true)]),
            Some(false)
        );
        assert_eq!(decides_enabled([None, None]), None);

        let plugin = row(Some(json!({"v": 1})), json!({"v": 1}), Some(true));
        let problem = row(Some(json!({"v": 2})), json!({"v": 2}), None);
        let eff = resolve_cascade(None, None, problem, plugin);
        assert!(eff.is_enabled, "the plugin-wide row decides");
        assert_eq!(eff.source, ConfigSource::Problem);
        assert_eq!(eff.config, json!({"v": 2}));
    }

    /// Each field names the scope it comes from, so a form can say where an
    /// inherited value is set.
    #[test]
    fn each_field_names_the_scope_that_sets_it() {
        let plugin = row(
            Some(json!({"compiler": "gcc-14"})),
            json!({"compiler": "gcc-14", "flags": [], "timeout": 5}),
            None,
        );
        let contest = row(
            Some(json!({"flags": ["-O2"]})),
            json!({"compiler": "g++", "flags": ["-O2"], "timeout": 5}),
            None,
        );
        let eff = resolve_cascade(None, Some(contest), unset(), plugin);
        assert_eq!(eff.config["compiler"], "gcc-14");
        assert_eq!(
            eff.fields,
            BTreeMap::from([
                ("compiler".to_string(), ConfigSource::Plugin),
                ("flags".to_string(), ConfigSource::Contest),
                ("timeout".to_string(), ConfigSource::Default),
            ])
        );
    }

    /// A switched-off narrower scope leaves the plugin-wide config, with its
    /// fields' sources, in place.
    #[test]
    fn a_switched_off_override_keeps_the_plugin_wide_fields() {
        let plugin = row(Some(json!({"v": 99})), json!({"v": 99}), None);
        let problem = row(Some(json!({"v": 1})), json!({"v": 1}), Some(false));
        let eff = resolve_cascade(None, None, problem, plugin);
        assert_eq!(eff.source, ConfigSource::Disabled);
        assert_eq!(eff.config, json!({"v": 99}));
        assert_eq!(eff.fields["v"], ConfigSource::Plugin);
        assert!(!eff.is_enabled);
    }
}
