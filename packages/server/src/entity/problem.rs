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

    /// Whether a plugin found something wrong with a config of the problem,
    /// at the problem scope or in a contest, when last checked
    /// (`services::config_issues`). Judging copies it onto the judgement it
    /// claims.
    #[sea_orm(default_value = false)]
    pub config_issues: bool,

    /// Whether the problem's config issues wait for a replica to decide
    /// them, and which plugins it must load to: a JSON array of plugin ids
    /// (empty: any replica can), `None` once decided. Set by a bundle import,
    /// which restores the problem without the plugins that check it, and by
    /// a check a plugin not loaded on the checking replica could not run;
    /// cleared by the replica that decides
    /// (`services::config_issues::recheck_marked`). While it is set,
    /// `config_issues` is set too: unknown reads as having issues.
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub config_recheck: Option<Json>,

    /// How many times anything judging reads of the problem changed (its
    /// test cases, limits, type, checker format, additional files and
    /// problem-scoped configs), counted by database triggers
    /// (`services::judging::inputs`). Judging stamps it on each judgement.
    #[sea_orm(default_value = 0)]
    pub inputs_version: i32,

    /// The language the problem's checker source compiles in, kept by the
    /// `broccoli_checker_language` trigger (migration m0018) from the
    /// extension of its first stored file; `None` without one. Work on the
    /// problem needs this language's provider when its checker format's
    /// provider declares checker languages.
    #[sea_orm(nullable)]
    pub checker_language: Option<String>,

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
