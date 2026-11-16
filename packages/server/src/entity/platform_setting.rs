//! The settings an admin changes at runtime, for the whole fleet: one row,
//! `id = 1`, read where it applies so a change takes effect on every replica
//! at once (see `crate::services::registration`).
use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "platform_setting")]
pub struct Model {
    /// Always [`ROW_ID`].
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i16,
    /// Whether `POST /auth/register` creates accounts. Closed on a fresh
    /// install: admins create or import accounts, and open self-registration
    /// when a contest wants it.
    pub registration_open: bool,
}

/// The one row's id.
pub const ROW_ID: i16 = 1;

impl ActiveModelBehavior for ActiveModel {}
