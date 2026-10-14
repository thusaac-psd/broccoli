use std::collections::HashMap;

use extism::host_fn;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait, QueryFilter, Set,
    Statement, TransactionTrait,
};
use serde::Deserialize;
use serde_json::Value;
use tracing::error;

use crate::entity::plugin_storage;
use crate::utils::text::{plugin_storage_text as extract_str, sanitize_db_text};

const DEFAULT_COLLECTION: &str = "default";

#[derive(Deserialize)]
struct StoreGetInput {
    keys: Vec<String>,
}

host_fn!(pub store_get(user_data: (String, DatabaseConnection); input: String) -> String {
    let user_data_guard = user_data.get()?;
    let user_data = super::lock_or_poison(&user_data_guard)?;
    let (plugin_id, db) = &*user_data;
    let span = super::host_fn_span("store_get", plugin_id);
    let _enter = span.enter();

    let mut parsed: StoreGetInput = serde_json::from_str(&input)
        .map_err(|e| extism::Error::msg(format!("Invalid store_get input: {e}")))?;
    parsed.keys = parsed.keys.into_iter().map(sanitize_db_text).collect();

    if parsed.keys.is_empty() {
        return Ok(serde_json::json!({ "values": {} }).to_string());
    }

    let results = tokio::runtime::Handle::current()
        .block_on(async {
            plugin_storage::Entity::find()
                .filter(plugin_storage::Column::PluginId.eq(plugin_id))
                .filter(plugin_storage::Column::Collection.eq(DEFAULT_COLLECTION))
                .filter(plugin_storage::Column::Key.is_in(&parsed.keys))
                .all(db)
                .await
        })
        .map_err(|e| {
            error!("DB store_get error: {e}");
            extism::Error::msg("Database error")
        })?;

    let values: HashMap<&str, String> = results
        .iter()
        .map(|r| (r.key.as_str(), extract_str(&r.data)))
        .collect();

    let versions: HashMap<&str, i64> = results.iter().map(|r| (r.key.as_str(), r.revision)).collect();
    Ok(serde_json::json!({ "values": values, "versions": versions }).to_string())
});

#[derive(Deserialize)]
struct StoreSetEntry {
    key: String,
    value: String,
}

#[derive(Deserialize)]
struct StoreSetInput {
    entries: Vec<StoreSetEntry>,
}

host_fn!(pub store_set(user_data: (String, DatabaseConnection); input: String) -> () {
    let user_data_guard = user_data.get()?;
    let user_data = super::lock_or_poison(&user_data_guard)?;
    let (plugin_id, db) = &*user_data;
    let span = super::host_fn_span("store_set", plugin_id);
    let _enter = span.enter();

    let mut parsed: StoreSetInput = serde_json::from_str(&input)
        .map_err(|e| extism::Error::msg(format!("Invalid store_set input: {e}")))?;
    for entry in &mut parsed.entries {
        entry.key = sanitize_db_text(&entry.key);
        entry.value = sanitize_db_text(&entry.value);
    }

    if parsed.entries.is_empty() {
        return Ok(());
    }

    tokio::runtime::Handle::current()
        .block_on(async {
            let txn = db.begin().await?;

            for entry in parsed.entries {
                let model = plugin_storage::ActiveModel {
                    plugin_id: Set(plugin_id.clone()),
                    collection: Set(DEFAULT_COLLECTION.to_string()),
                    key: Set(entry.key),
                    data: Set(Value::String(entry.value)),
                    revision: Set(0),
                    created_at: Set(chrono::Utc::now()),
                };

                plugin_storage::Entity::insert(model)
                    .on_conflict(
                        OnConflict::columns([
                            plugin_storage::Column::PluginId,
                            plugin_storage::Column::Collection,
                            plugin_storage::Column::Key,
                        ])
                        .update_columns([plugin_storage::Column::Data])
                        .value(plugin_storage::Column::Revision, sea_orm::sea_query::Expr::cust("plugin_storage.revision + 1"))
                        .to_owned(),
                    )
                    .exec(&txn)
                    .await?;
            }

            txn.commit().await?;
            Ok::<(), sea_orm::DbErr>(())
        })
        .map_err(|e| {
            error!("DB store_set error: {e}");
            extism::Error::msg("Database error")
        })?;

    Ok(())
});

#[derive(Deserialize)]
struct StoreDeleteInput {
    keys: Vec<String>,
}

host_fn!(pub store_delete(user_data: (String, DatabaseConnection); input: String) -> () {
    let user_data_guard = user_data.get()?;
    let user_data = super::lock_or_poison(&user_data_guard)?;
    let (plugin_id, db) = &*user_data;
    let span = super::host_fn_span("store_delete", plugin_id);
    let _enter = span.enter();

    let mut parsed: StoreDeleteInput = serde_json::from_str(&input)
        .map_err(|e| extism::Error::msg(format!("Invalid store_delete input: {e}")))?;
    parsed.keys = parsed.keys.into_iter().map(sanitize_db_text).collect();

    if parsed.keys.is_empty() {
        return Ok(());
    }

    tokio::runtime::Handle::current()
        .block_on(async {
            plugin_storage::Entity::delete_many()
                .filter(plugin_storage::Column::PluginId.eq(plugin_id))
                .filter(plugin_storage::Column::Collection.eq(DEFAULT_COLLECTION))
                .filter(plugin_storage::Column::Key.is_in(&parsed.keys))
                .exec(db)
                .await
        })
        .map_err(|e| {
            error!("DB store_delete error: {e}");
            extism::Error::msg("Database error")
        })?;

    Ok(())
});

#[derive(Deserialize)]
struct StoreCasInput {
    key: String,
    expected: Option<String>,
    new: String,
    expected_revision: Option<i64>,
}

host_fn!(pub store_compare_and_set(user_data: (String, DatabaseConnection); input: String) -> String {
    let user_data_guard = user_data.get()?;
    let user_data = super::lock_or_poison(&user_data_guard)?;
    let (plugin_id, db) = &*user_data;
    let span = super::host_fn_span("store_compare_and_set", plugin_id);
    let _enter = span.enter();

    let mut parsed: StoreCasInput = serde_json::from_str(&input)
        .map_err(|e| extism::Error::msg(format!("Invalid store_compare_and_set input: {e}")))?;
    parsed.key = sanitize_db_text(&parsed.key);
    parsed.expected = parsed.expected.map(sanitize_db_text);
    parsed.new = sanitize_db_text(&parsed.new);

    let swapped = tokio::runtime::Handle::current()
        .block_on(crate::services::plugin_storage::compare_and_set(
            db, plugin_id, &parsed.key, parsed.expected.as_deref(),
            &parsed.new, parsed.expected_revision,
        ))
        .map_err(|e| {
            error!("DB store_compare_and_set error: {e}");
            extism::Error::msg("Database error")
        })?;

    Ok(serde_json::json!({ "swapped": swapped }).to_string())
});

#[derive(Deserialize)]
struct StoreScanInput {
    prefix: String,
    after: Option<String>,
}

host_fn!(pub store_scan(user_data: (String, DatabaseConnection); input: String) -> String {
    let (plugin_id, db) = {
        let guard = user_data.get()?;
        let data = super::lock_or_poison(&guard)?;
        data.clone()
    };
    let input: StoreScanInput = serde_json::from_str(&input)?;
    let rows = tokio::runtime::Handle::current().block_on(db.query_all_raw(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT key, data FROM plugin_storage WHERE plugin_id = $1 AND collection = $2
         AND starts_with(key, $3) AND ($4::text IS NULL OR key > $4) ORDER BY key LIMIT 100",
        [plugin_id.into(), DEFAULT_COLLECTION.into(), input.prefix.into(), input.after.into()])))?;
    let mut entries = HashMap::new();
    for row in rows {
        let key: String = row.try_get("", "key")?;
        let value: Value = row.try_get("", "data")?;
        entries.insert(key, extract_str(&value));
    }
    Ok(serde_json::to_string(&entries)?)
});
