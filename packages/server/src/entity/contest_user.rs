use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "contest_user")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub contest_id: i32,
    #[sea_orm(primary_key)]
    pub user_id: i32,
    #[sea_orm(belongs_to, from = "contest_id", to = "id")]
    pub contest: HasOne<super::contest::Entity>,
    #[sea_orm(belongs_to, from = "user_id", to = "id")]
    pub user: HasOne<super::user::Entity>,

    pub registered_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}
