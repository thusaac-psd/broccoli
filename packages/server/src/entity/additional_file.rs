use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "additional_file")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,

    /// `(problem_id, language, path)` is unique. Declared on the entity because
    /// the per-boot schema `sync()` drops every unique index whose column set
    /// no entity declares an index on.
    #[sea_orm(unique_key = "problem_lang_path")]
    pub problem_id: i32,

    #[sea_orm(unique_key = "problem_lang_path")]
    pub language: String,

    #[sea_orm(column_type = "Text", unique_key = "problem_lang_path")]
    pub path: String,

    pub content_hash: String,

    pub filename: String,

    pub content_type: Option<String>,

    pub size: i64,

    pub created_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}
