use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "problem_attachment")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,

    #[sea_orm(unique_key = "problem_path")]
    pub problem_id: i32,

    #[sea_orm(column_type = "Text", unique_key = "problem_path")]
    pub path: String,

    pub content_hash: String,

    pub filename: String,

    pub content_type: Option<String>,

    pub size: i64,

    pub created_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}
