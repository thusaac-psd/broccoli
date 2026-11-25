//! The installation's identity: one row, `id = 1`, whose `installation_id`
//! the migration that creates the table generates once per database. A
//! bundle records it, so an import can tell a re-import of its own export
//! from a bundle another installation wrote (see `crate::bundle`).
use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "installation")]
pub struct Model {
    /// Always [`ROW_ID`].
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i16,
    #[sea_orm(column_type = "Text")]
    pub installation_id: String,
}

/// The one row's id.
pub const ROW_ID: i16 = 1;

impl ActiveModelBehavior for ActiveModel {}
