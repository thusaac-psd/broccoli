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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigSource {
    ContestProblem,
    Contest,
    Problem,
    Default,
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
