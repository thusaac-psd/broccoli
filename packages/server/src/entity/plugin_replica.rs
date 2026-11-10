//! One row per live server replica: the plugin load state that replica
//! reports (see `crate::plugin_lifecycle`). Admin reads aggregate these rows
//! into a fleet-wide view; a replica that stops touching `heartbeat_at` drops
//! out of that view without anyone deleting its row.
use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "plugin_replica")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub server_id: String,
    /// `{ plugin_id: ReplicaPluginState }`, as last reported.
    #[sea_orm(column_type = "JsonBinary")]
    pub plugins: Json,
    /// Database clock: when `plugins` last changed.
    pub state_at: DateTimeUtc,
    /// Database clock: last write or periodic touch by the replica.
    pub heartbeat_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}
