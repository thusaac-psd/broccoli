use common::db::DatabaseConnection;
use std::collections::HashMap;
use std::sync::Arc;

use broccoli_server_sdk::types::host_io::{
    StoreCasInput, StoreCasOutput, StoreDeleteInput, StoreGetInput, StoreGetOutput, StoreScanInput,
    StoreSetInput,
};
use extism::host_fn;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter, Set, Statement,
    TransactionTrait,
};
use serde_json::Value;
use tracing::error;

use crate::entity::plugin_storage;
use crate::utils::text::plugin_storage_text as extract_str;

const DEFAULT_COLLECTION: &str = "default";

host_fn!(pub store_get(user_data: Arc<(String, DatabaseConnection)>; input: String) -> String {
    let call = super::HostCall::enter(&user_data, "store_get")?;
    let (plugin_id, db) = &*call;

    let parsed: StoreGetInput = serde_json::from_str(&input)
        .map_err(|e| extism::Error::msg(format!("Invalid store_get input: {e}")))?;

    if parsed.keys.is_empty() {
        return Ok(serde_json::to_string(&StoreGetOutput::default())?);
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

    let output = StoreGetOutput {
        values: results.iter().map(|r| (r.key.clone(), extract_str(&r.data))).collect(),
        versions: results.iter().map(|r| (r.key.clone(), r.revision)).collect(),
    };
    Ok(serde_json::to_string(&output)?)
});

host_fn!(pub store_set(user_data: Arc<(String, DatabaseConnection)>; input: String) -> () {
    let call = super::HostCall::enter(&user_data, "store_set")?;
    let (plugin_id, db) = &*call;

    let parsed: StoreSetInput = serde_json::from_str(&input)
        .map_err(|e| extism::Error::msg(format!("Invalid store_set input: {e}")))?;

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
                    created_at: sea_orm::NotSet,
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

host_fn!(pub store_delete(user_data: Arc<(String, DatabaseConnection)>; input: String) -> () {
    let call = super::HostCall::enter(&user_data, "store_delete")?;
    let (plugin_id, db) = &*call;

    let parsed: StoreDeleteInput = serde_json::from_str(&input)
        .map_err(|e| extism::Error::msg(format!("Invalid store_delete input: {e}")))?;

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

host_fn!(pub store_compare_and_set(user_data: Arc<(String, DatabaseConnection)>; input: String) -> String {
    let call = super::HostCall::enter(&user_data, "store_compare_and_set")?;
    let (plugin_id, db) = &*call;

    let parsed: StoreCasInput = serde_json::from_str(&input)
        .map_err(|e| extism::Error::msg(format!("Invalid store_compare_and_set input: {e}")))?;

    let swapped = tokio::runtime::Handle::current()
        .block_on(crate::services::plugin_storage::compare_and_set(
            db, plugin_id, &parsed.key, parsed.expected.as_deref(),
            &parsed.new, parsed.expected_revision,
        ))
        .map_err(|e| {
            error!("DB store_compare_and_set error: {e}");
            extism::Error::msg("Database error")
        })?;

    Ok(serde_json::to_string(&StoreCasOutput { swapped })?)
});

host_fn!(pub store_scan(user_data: Arc<(String, DatabaseConnection)>; input: String) -> String {
    let call = super::HostCall::enter(&user_data, "store_scan")?;
    let (plugin_id, db) = &*call;
    let input: StoreScanInput = serde_json::from_str(&input)?;
    let rows = tokio::runtime::Handle::current().block_on(db.query_all_raw(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT key, data FROM plugin_storage WHERE plugin_id = $1 AND collection = $2
         AND starts_with(key, $3) AND ($4::text IS NULL OR key > $4) ORDER BY key LIMIT 100",
        [plugin_id.as_str().into(), DEFAULT_COLLECTION.into(), input.prefix.into(), input.after.into()])))?;
    let mut entries = HashMap::new();
    for row in rows {
        let key: String = row.try_get("", "key")?;
        let value: Value = row.try_get("", "data")?;
        entries.insert(key, extract_str(&value));
    }
    Ok(serde_json::to_string(&entries)?)
});
