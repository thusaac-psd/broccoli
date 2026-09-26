//! Pending one-shot plugin timer callbacks. Rows are created by the
//! `timer_schedule` host function, claimed by the `dispatcher::plugin_timer`
//! loop, and deleted on successful delivery.
use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "plugin_timer")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,

    /// `(plugin_id, key)` is unique: `timer_schedule` upserts with
    /// `ON CONFLICT (plugin_id, key)`. Declared HERE, not only in a migration,
    /// because SeaORM's schema `sync()` runs on every boot and drops any
    /// existing unique index whose column set the entity does not declare - a
    /// migration-only index survives the first boot and vanishes on the
    /// second, after which every `timer_schedule` fails. See
    /// `host_funcs::timer::tests::reschedule_still_upserts_after_a_server_restart`.
    #[sea_orm(unique_key = "plugin_key")]
    pub plugin_id: String,
    #[sea_orm(unique_key = "plugin_key")]
    pub key: String,

    pub fire_at: DateTimeUtc,

    #[sea_orm(column_type = "Text")]
    pub payload: String,

    #[sea_orm(nullable)]
    pub claimed_at: Option<DateTimeUtc>,

    #[sea_orm(default_value = 0)]
    pub attempts: i32,

    pub created_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}
