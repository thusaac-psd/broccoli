use crate::entity::plugin_config;
use chrono::Utc;
use common::db::DatabaseConnection;
use extism::{Function, UserData, Val, ValType};
use plugin_core::registry::PluginRegistry;
use sea_orm::{EntityTrait, Set, sea_query::OnConflict};
use serde::Deserialize;
use std::sync::Arc;

use crate::utils::text::sanitize_db_json;

type ConfigGetUserData = (String, DatabaseConnection, PluginRegistry);
type ConfigSetUserData = (String, DatabaseConnection);

const VALID_SCOPES: &[&str] = &["problem", "contest_problem", "contest", "plugin"];

fn validate_raw_namespace(namespace: &str) -> Result<(), extism::Error> {
    if namespace.is_empty() || namespace.len() > 128 {
        return Err(extism::Error::msg("namespace must be 1-128 characters"));
    }
    if !namespace
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(extism::Error::msg(
            "namespace must contain only alphanumeric, hyphen, or underscore characters",
        ));
    }
    Ok(())
}

fn validate_config_input(scope: &str, ref_id: &str, namespace: &str) -> Result<(), extism::Error> {
    if !VALID_SCOPES.contains(&scope) {
        return Err(extism::Error::msg(format!(
            "Invalid scope '{}': must be one of {}",
            scope,
            VALID_SCOPES.join(", ")
        )));
    }
    if ref_id.is_empty() {
        return Err(extism::Error::msg("ref_id must not be empty"));
    }
    if namespace.is_empty() || namespace.len() > 256 {
        return Err(extism::Error::msg("namespace must be 1-256 characters"));
    }
    if !namespace
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ':')
    {
        return Err(extism::Error::msg(
            "namespace must contain only alphanumeric, hyphen, underscore, or colon characters",
        ));
    }
    Ok(())
}

pub fn resolve_namespace(scope: &str, plugin_id: &str, namespace: &str) -> String {
    if scope == "plugin" {
        namespace.to_string()
    } else {
        format!("{}:{}", plugin_id, namespace)
    }
}

pub fn strip_namespace_prefix(composite: &str) -> &str {
    composite
        .split_once(':')
        .map(|(_, raw)| raw)
        .unwrap_or(composite)
}

pub fn extract_plugin_id(composite: &str) -> &str {
    composite
        .split_once(':')
        .map(|(prefix, _)| prefix)
        .unwrap_or(composite)
}

#[derive(Deserialize)]
struct ConfigGetInput {
    scope: String,
    ref_id: String,
    namespace: String,
}

#[derive(Deserialize)]
struct ConfigSetInput {
    scope: String,
    ref_id: String,
    namespace: String,
    config: serde_json::Value,
}

pub fn create_config_get_function(
    plugin_id: String,
    db: DatabaseConnection,
    registry: PluginRegistry,
) -> Function {
    Function::new(
        "config_get",
        [ValType::I64],
        [ValType::I64],
        super::host_data((plugin_id, db, registry)),
        config_get_fn,
    )
}

pub fn create_config_set_function(plugin_id: String, db: DatabaseConnection) -> Function {
    Function::new(
        "config_set",
        [ValType::I64],
        [],
        super::host_data((plugin_id, db)),
        config_set_fn,
    )
}

fn config_get_fn(
    plugin: &mut extism::CurrentPlugin,
    inputs: &[Val],
    outputs: &mut [Val],
    user_data: UserData<Arc<ConfigGetUserData>>,
) -> Result<(), extism::Error> {
    let input: ConfigGetInput = super::read_json(plugin, inputs, "config_get input")?;
    let call = super::HostCall::enter(&user_data, "config_get")?;
    let (plugin_id, db, registry) = &*call;

    let raw_namespace = input.namespace.clone();

    let ref_id = if input.scope == "plugin" {
        plugin_id.clone()
    } else {
        input.ref_id
    };

    validate_raw_namespace(&raw_namespace)?;
    let namespace = resolve_namespace(&input.scope, plugin_id, &raw_namespace);
    validate_config_input(&input.scope, &ref_id, &namespace)?;

    let result = tokio::runtime::Handle::current()
        .block_on(async {
            plugin_config::Entity::find_by_id((input.scope, ref_id, namespace))
                .one(db)
                .await
        })
        .map_err(|e| extism::Error::msg(format!("DB error in config_get: {}", e)))?;

    let output_value = match result {
        Some(row) => {
            serde_json::json!({ "config": row.config, "is_default": false, "enabled": row.enabled })
        }
        None => {
            let defaults = registry
                .read()
                .ok()
                .and_then(|reg| {
                    reg.get(plugin_id)
                        .and_then(|entry| entry.manifest.config.get(&raw_namespace))
                        .map(|ns| ns.defaults())
                })
                .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));

            serde_json::json!({ "config": defaults, "is_default": true, "enabled": null })
        }
    };

    super::write_json(plugin, outputs, &output_value, "config_get output")
}

fn config_set_fn(
    plugin: &mut extism::CurrentPlugin,
    inputs: &[Val],
    _outputs: &mut [Val],
    user_data: UserData<Arc<ConfigSetUserData>>,
) -> Result<(), extism::Error> {
    let input: ConfigSetInput = super::read_json(plugin, inputs, "config_set input")?;
    let call = super::HostCall::enter(&user_data, "config_set")?;
    let (plugin_id, db) = &*call;

    let ref_id = if input.scope == "plugin" {
        plugin_id.clone()
    } else {
        input.ref_id
    };

    validate_raw_namespace(&input.namespace)?;
    let namespace = resolve_namespace(&input.scope, plugin_id, &input.namespace);
    validate_config_input(&input.scope, &ref_id, &namespace)?;

    tokio::runtime::Handle::current()
        .block_on(async {
            let active = plugin_config::ActiveModel {
                scope: Set(input.scope),
                ref_id: Set(ref_id),
                namespace: Set(namespace),
                config: Set(sanitize_db_json(input.config)),
                enabled: Set(None),
                position: Set(0),
                updated_at: Set(Utc::now()),
            };

            plugin_config::Entity::insert(active)
                .on_conflict(
                    OnConflict::columns([
                        plugin_config::Column::Scope,
                        plugin_config::Column::RefId,
                        plugin_config::Column::Namespace,
                    ])
                    .update_columns([
                        plugin_config::Column::Config,
                        plugin_config::Column::UpdatedAt,
                    ])
                    .to_owned(),
                )
                .exec(db)
                .await?;

            Ok::<_, sea_orm::DbErr>(())
        })
        .map_err(|e| extism::Error::msg(format!("DB error in config_set: {}", e)))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_namespace_plugin_scope_returns_raw() {
        assert_eq!(
            resolve_namespace("plugin", "my-plugin", "settings"),
            "settings"
        );
    }

    #[test]
    fn resolve_namespace_non_plugin_scope_prefixes() {
        assert_eq!(
            resolve_namespace("contest", "cooldown", "cooldown"),
            "cooldown:cooldown"
        );
        assert_eq!(resolve_namespace("problem", "ioi", "task"), "ioi:task");
        assert_eq!(
            resolve_namespace("contest_problem", "submission-limit", "limits"),
            "submission-limit:limits"
        );
    }

    #[test]
    fn strip_namespace_prefix_with_colon() {
        assert_eq!(strip_namespace_prefix("cooldown:cooldown"), "cooldown");
        assert_eq!(strip_namespace_prefix("ioi:task"), "task");
    }

    #[test]
    fn extract_plugin_id_with_colon() {
        assert_eq!(extract_plugin_id("cooldown:cooldown"), "cooldown");
        assert_eq!(extract_plugin_id("ioi:task"), "ioi");
    }
}
