use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "role_permission")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub role: String,
    #[sea_orm(primary_key)]
    pub permission: String,
    #[sea_orm(belongs_to, from = "role", to = "name")]
    pub role_ref: HasOne<super::role::Entity>,
}

impl ActiveModelBehavior for ActiveModel {}
