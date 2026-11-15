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
    pub problem_type: String,

    /// Always set on insert, so the column has no default of its own: a
    /// problem that names none gets the one its checker plugins declare
    /// (`models::problem::default_checker_format`).
    #[sea_orm(column_type = "Text")]
    pub checker_format: String,
    pub default_contest_type: String,

    #[sea_orm(default_value = false)]
    pub show_test_details: bool,

    #[sea_orm(default_value = false)]
    pub is_public: bool,

    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub submission_format: Option<serde_json::Value>,

    /// Whether some stored config of the problem, at the problem scope or in
    /// a contest, was refused by its plugin when last checked: set by every
    /// test-data change (`services::test_data`), cleared by the config write
    /// that resolves the last issue. Judging copies it onto the judgement it
    /// claims.
    #[sea_orm(default_value = false)]
    pub config_issues: bool,

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
