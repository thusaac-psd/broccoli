use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Serialize, ToSchema)]
pub struct PluginConfigResponse {
    #[schema(example = "cooldown")]
    pub plugin_id: String,
    #[schema(example = "checker")]
    pub namespace: String,
    pub config: serde_json::Value,
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
/// configs by.
#[derive(Serialize, ToSchema)]
pub struct ConfigCascadeResponse {
    #[schema(example = "standard-languages")]
    pub plugin_id: String,
    #[schema(example = "compilation")]
    pub namespace: String,
    /// The config a problem reads that stores nothing narrower than this
    /// target, this target's own row included.
    pub effective: broccoli_server_sdk::types::EffectiveConfig,
    /// The same without this target's own row: what it inherits, and from
    /// which scope each field comes (`fields`).
    pub inherited: broccoli_server_sdk::types::EffectiveConfig,
}

#[derive(Deserialize, ToSchema)]
pub struct UpsertPluginConfigRequest {
    pub config: serde_json::Value,
    #[schema(value_type = Option<bool>)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub position: i32,
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
