use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "clarification")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,

    pub contest_id: i32,
    pub author_id: i32,
    #[sea_orm(column_type = "Text")]
    pub content: String,

    pub clarification_type: String,
    pub recipient_id: Option<i32>,
    #[sea_orm(default_value = false)]
    pub is_public: bool,

    #[sea_orm(default_value = false)]
    pub resolved: bool,
    pub resolved_at: Option<DateTimeUtc>,
    pub resolved_by: Option<i32>,

    #[sea_orm(belongs_to, from = "contest_id", to = "id")]
    pub contest: HasOne<super::contest::Entity>,
    #[sea_orm(belongs_to, from = "author_id", to = "id")]
    pub author: HasOne<super::user::Entity>,

    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}
