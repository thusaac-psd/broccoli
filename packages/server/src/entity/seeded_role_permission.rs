use sea_orm::entity::prelude::*;

/// Ledger of the built-in `(role, permission)` mappings that seeding has
/// already applied once. Seeding inserts a built-in mapping into
/// `role_permission` only the first time it appears here, so an admin's
/// deliberate revoke survives restarts while a mapping added in a later
/// release is still applied on its first boot.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "seeded_role_permission")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub role: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub permission: String,
}

impl ActiveModelBehavior for ActiveModel {}
