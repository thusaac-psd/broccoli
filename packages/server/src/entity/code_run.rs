use common::{SubmissionStatus, Verdict};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "code_run")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,

    #[sea_orm(column_type = "JsonBinary")]
    pub files: serde_json::Value,
    pub language: String,

    pub user_id: i32,
    pub problem_id: i32,
    pub contest_id: Option<i32>,

    #[sea_orm(default_value = "ioi")]
    pub contest_type: String,

    pub status: SubmissionStatus,
    #[sea_orm(column_type = "Text", nullable)]
    pub verdict: Option<Verdict>,

    #[sea_orm(column_type = "Text", nullable)]
    pub compile_output: Option<String>,
    pub error_code: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub error_message: Option<String>,

    #[sea_orm(column_type = "Double", nullable)]
    pub score: Option<f64>,
    pub time_used: Option<i32>,
    pub memory_used: Option<i32>,

    #[sea_orm(column_type = "JsonBinary")]
    pub custom_test_cases: serde_json::Value,

    #[sea_orm(belongs_to, from = "user_id", to = "id")]
    pub user: HasOne<super::user::Entity>,
    #[sea_orm(belongs_to, from = "problem_id", to = "id")]
    pub problem: HasOne<super::problem::Entity>,
    #[sea_orm(belongs_to, from = "contest_id", to = "id")]
    pub contest: Option<super::contest::Entity>,
    #[sea_orm(has_many)]
    pub results: HasMany<super::code_run_result::Entity>,

    #[sea_orm(column_type = "String(StringLen::N(128))", nullable)]
    pub owner_server_id: Option<String>,
    pub lease_heartbeat_at: Option<DateTimeUtc>,
    /// Immutable dispatch anchor; see `submission::Model::leased_at`.
    #[sea_orm(nullable)]
    pub leased_at: Option<DateTimeUtc>,
    /// Dispatch attempts so far (1 after the first); see
    /// `submission::Model::retry_count`.
    #[sea_orm(default_value = 0)]
    pub retry_count: i32,
    #[sea_orm(default_value = 0)]
    pub judge_epoch: i32,

    pub created_at: DateTimeUtc,
    pub judged_at: Option<DateTimeUtc>,
}

impl ActiveModelBehavior for ActiveModel {}
