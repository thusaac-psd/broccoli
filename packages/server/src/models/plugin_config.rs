use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Serialize, ToSchema)]
pub struct PluginConfigResponse {
    #[schema(example = "cooldown")]
    pub plugin_id: String,
    #[schema(example = "checker")]
    pub namespace: String,
    /// The stored config, each secret field (`writeOnly` in `json_schema`)
    /// `null`: a secret never leaves the server.
    pub config: serde_json::Value,
    /// The paths of the secret fields that hold a value (`stations[0].token`).
    /// Writing `null` at one keeps the stored secret.
    pub secrets: Vec<String>,
    /// Whether `config` is withheld whole (`null`): this server knows no
    /// schema for the namespace (its plugin is not installed here, or no
    /// longer declares it), so it cannot tell which fields are secret.
    pub config_withheld: bool,
    pub enabled: Option<bool>,
    pub position: i32,
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A namespace's config at a problem, contest or contest problem as its
/// plugin reads it, and what the scopes above give it: what the admin form
/// shows as inherited. Both come from the cascade rule the plugin SDK reads
/// configs by, shown as far as the caller may read the scopes involved.
#[derive(Serialize, ToSchema)]
pub struct ConfigCascadeResponse {
    #[schema(example = "standard-languages")]
    pub plugin_id: String,
    #[schema(example = "compilation")]
    pub namespace: String,
    /// The config a problem reads that stores nothing narrower than this
    /// target, this target's own row included.
    pub effective: ConfigCascadeView,
    /// The same without this target's own row: what it inherits, and from
    /// which scope each field comes (`fields`).
    pub inherited: ConfigCascadeView,
}

/// An effective config as a caller may see it. A scope's row is shown only to
/// a caller who may read it directly: the plugin-wide row needs
/// `plugin:manage`, a problem's `problem:edit`, a contest's or contest
/// problem's `contest:manage`. A top-level field a hidden row sets is named in
/// `withheld` and left out of `config`.
#[derive(Serialize, ToSchema)]
pub struct ConfigCascadeView {
    pub config: serde_json::Value,
    pub source: broccoli_server_sdk::types::ConfigSource,
    pub is_enabled: bool,
    /// Where each top-level field comes from: the scope whose row sets it,
    /// else the plugin-wide row, else the schema's defaults.
    pub fields: std::collections::BTreeMap<String, broccoli_server_sdk::types::ConfigSource>,
    /// The top-level fields left out of `config` because a scope the caller
    /// may not read sets them.
    pub withheld: Vec<String>,
    /// Each scope's row the caller may read; `null` for a scope that does not
    /// apply here or that the caller may not read.
    pub levels: CascadeLevelsView,
}

/// [`ConfigCascadeView::levels`].
#[derive(Serialize, ToSchema)]
pub struct CascadeLevelsView {
    pub contest_problem: Option<broccoli_server_sdk::types::CascadeLevel>,
    pub contest: Option<broccoli_server_sdk::types::CascadeLevel>,
    pub problem: Option<broccoli_server_sdk::types::CascadeLevel>,
    pub plugin: Option<broccoli_server_sdk::types::CascadeLevel>,
}

#[derive(Deserialize, ToSchema)]
pub struct UpsertPluginConfigRequest {
    pub config: serde_json::Value,
    /// The row's toggle: `true` or `false` decides whether the plugin runs
    /// here, `null` leaves it to the scope above. Left out, a stored row
    /// keeps its toggle.
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<bool>)]
    pub enabled: Option<Option<bool>>,
    /// The row's position. Left out, a stored row keeps its position and a
    /// new row gets 0.
    #[serde(default)]
    pub position: Option<i32>,
}

impl UpsertPluginConfigRequest {
    pub(crate) fn row_settings(&self) -> crate::services::plugin_config::RowSettings {
        crate::services::plugin_config::RowSettings {
            enabled: self.enabled,
            position: self.position,
        }
    }
}

/// A field the body names, `null` included: `Some(None)` is an explicit
/// `null`, and `#[serde(default)]` makes a left-out field `None`.
fn present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    serde::Deserialize::deserialize(deserializer).map(Some)
}

pub mod config_key {
    pub fn problem(problem_id: i32) -> String {
        problem_id.to_string()
    }

    pub fn contest_problem(contest_id: i32, problem_id: i32) -> String {
        format!("{contest_id}:{problem_id}")
    }

    pub fn contest(contest_id: i32) -> String {
        contest_id.to_string()
    }

    pub fn plugin(plugin_id: &str) -> String {
        plugin_id.to_string()
    }

    pub fn contest_problem_by_problem_like(problem_id: i32) -> String {
        format!("%:{problem_id}")
    }

    pub fn contest_problem_by_contest_like(contest_id: i32) -> String {
        format!("{contest_id}:%")
    }
}
