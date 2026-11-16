use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::SdkError;

/// One stored config as the host hands it to a plugin: the stored value with
/// every default its schema declares filled in, so it deserializes into the
/// plugin's config type without defaults of the plugin's own.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigResult {
    pub config: serde_json::Value,
    /// Nothing is stored: `config` is the schema's defaults.
    #[serde(default)]
    pub is_default: bool,
    pub enabled: Option<bool>,
}

impl ConfigResult {
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

/// The scope whose row an effective config comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CascadeLevel {
    pub config: serde_json::Value,
    pub is_default: bool,
    pub enabled: Option<bool>,
}

impl From<&ConfigResult> for CascadeLevel {
    fn from(r: &ConfigResult) -> Self {
        Self {
            config: r.config.clone(),
            is_default: r.is_default,
            enabled: r.enabled,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CascadeLevels {
    pub contest_problem: Option<CascadeLevel>,
    pub contest: Option<CascadeLevel>,
    pub problem: CascadeLevel,
    /// Read only when no narrower scope decides the config alone: when none
    /// stores the namespace, or the one that does is switched off.
    pub plugin: Option<CascadeLevel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectiveConfig {
    pub config: serde_json::Value,
    pub source: ConfigSource,
    pub is_enabled: bool,
    pub levels: CascadeLevels,
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
