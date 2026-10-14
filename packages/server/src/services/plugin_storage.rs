//! Atomic writes of opaque plugin storage values.
use crate::entity::plugin_storage;
use crate::utils::text::plugin_storage_text;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, DatabaseConnection, DbErr, EntityTrait, QuerySelect, Set, TransactionTrait,
};
use serde_json::Value;

pub async fn compare_and_set(
    db: &DatabaseConnection,
    plugin_id: &str,
    key: &str,
    expected: Option<&str>,
    new: &str,
    expected_revision: Option<i64>,
) -> Result<bool, DbErr> {
    let Some(expected) = expected else {
        let result = plugin_storage::Entity::insert(plugin_storage::ActiveModel {
            plugin_id: Set(plugin_id.into()),
            collection: Set("default".into()),
            key: Set(key.into()),
            data: Set(Value::String(new.into())),
            revision: Set(0),
            created_at: Set(chrono::Utc::now()),
        })
        .on_conflict(
            OnConflict::columns([
                plugin_storage::Column::PluginId,
                plugin_storage::Column::Collection,
                plugin_storage::Column::Key,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec_without_returning(db)
        .await?;
        return Ok(result > 0);
    };
    let txn = db.begin().await?;
    let Some(row) = plugin_storage::Entity::find_by_id((
        plugin_id.to_owned(),
        "default".to_owned(),
        key.to_owned(),
    ))
    .lock_exclusive()
    .one(&txn)
    .await?
    else {
        return Ok(false);
    };
    // Match exactly the text exposed by store_get. An arbitrary expected
    // value is a mismatch, never something to cast into a JSON document.
    if plugin_storage_text(&row.data) != expected
        || expected_revision.is_some_and(|revision| revision != row.revision)
    {
        return Ok(false);
    }
    let revision = row
        .revision
        .checked_add(1)
        .ok_or_else(|| DbErr::Custom("Storage revision overflow".into()))?;
    let mut updated: plugin_storage::ActiveModel = row.into();
    updated.data = Set(Value::String(new.into()));
    updated.revision = Set(revision);
    updated.update(&txn).await?;
    txn.commit().await?;
    Ok(true)
}
