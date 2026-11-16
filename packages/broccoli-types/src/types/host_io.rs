//! The JSON a guest and the host exchange through the config, timer and
//! storage host functions. The guest SDK serializes these and the host
//! deserializes them (and the reverse for outputs), so the two sides read
//! one definition. The `config_get` output is [`super::ConfigResult`].

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// `config_get` input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigGetInput {
    pub scope: String,
    pub ref_id: String,
    pub namespace: String,
}

/// `config_set` input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigSetInput {
    pub scope: String,
    pub ref_id: String,
    pub namespace: String,
    pub config: serde_json::Value,
}

/// `timer_schedule` input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimerScheduleInput {
    /// Unix epoch milliseconds.
    pub fire_at_ms: i64,
    pub key: String,
    pub payload: String,
}

/// `timer_cancel` input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimerCancelInput {
    pub key: String,
}

/// `store_get` input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreGetInput {
    pub keys: Vec<String>,
}

/// `store_get` output: each key found, with its value and its revision.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreGetOutput {
    pub values: HashMap<String, String>,
    pub versions: HashMap<String, i64>,
}

/// One `store_set` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreSetEntry {
    pub key: String,
    pub value: String,
}

/// `store_set` input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreSetInput {
    pub entries: Vec<StoreSetEntry>,
}

/// `store_delete` input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreDeleteInput {
    pub keys: Vec<String>,
}

/// `store_compare_and_set` input: replace `key`'s value with `new` if it is
/// `expected` (`None`: absent), and, when `expected_revision` is set, still
/// at that revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreCasInput {
    pub key: String,
    pub expected: Option<String>,
    pub new: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<i64>,
}

/// `store_compare_and_set` output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreCasOutput {
    pub swapped: bool,
}

/// `store_scan` input: keys starting with `prefix`, after `after`. Its
/// output is a key-to-value map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreScanInput {
    pub prefix: String,
    pub after: Option<String>,
}
