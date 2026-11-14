use std::collections::HashMap;

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::utils::soft_delete::SoftDeletable;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "problem")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,

    pub title: String,
    #[sea_orm(column_type = "Text")]
    pub content: String,
    pub time_limit: i32,
    pub memory_limit: i32,

    #[sea_orm(default_value = "batch")]
    pub problem_type: String,

    /// Always set on insert, so the column has no default of its own beside
    /// `broccoli_api_types::problem::DEFAULT_CHECKER_FORMAT`.
    #[sea_orm(column_type = "Text")]
    pub checker_format: String,

    #[sea_orm(default_value = "ioi")]
    pub default_contest_type: String,

    #[sea_orm(default_value = false)]
    pub show_test_details: bool,

    #[sea_orm(default_value = false)]
    pub is_public: bool,

    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub submission_format: Option<serde_json::Value>,

    #[sea_orm(has_many)]
    pub submissions: HasMany<super::submission::Entity>,

    #[sea_orm(has_many)]
    pub test_cases: HasMany<super::test_case::Entity>,

    #[sea_orm(has_many, via = "contest_problem")]
    pub contests: HasMany<super::contest::Entity>,

    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub deleted_at: Option<DateTimeUtc>,
}

impl ActiveModelBehavior for ActiveModel {}

impl SoftDeletable for Entity {
    type DeletedAtColumn = Column;
    fn deleted_at() -> Self::DeletedAtColumn {
        Column::DeletedAt
    }
}

impl Model {
    pub fn get_submission_format(&self) -> Option<HashMap<String, Vec<String>>> {
        self.submission_format
            .as_ref()
            .and_then(|value| serde_json::from_value(value.clone()).ok())
    }
}
