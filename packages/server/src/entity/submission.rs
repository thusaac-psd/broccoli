use common::{SubmissionStatus, Verdict};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "submission")]
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

    #[sea_orm(belongs_to, from = "user_id", to = "id")]
    pub user: HasOne<super::user::Entity>,
    #[sea_orm(belongs_to, from = "problem_id", to = "id")]
    pub problem: HasOne<super::problem::Entity>,
    #[sea_orm(belongs_to, from = "contest_id", to = "id")]
    pub contest: HasOne<super::contest::Entity>,
    #[sea_orm(has_many)]
    pub test_case_results: HasMany<super::test_case_result::Entity>,

    /// The current judgement's epoch, mirrored from it (see
    /// `services::judging`).
    #[sea_orm(default_value = 0)]
    pub judge_epoch: i32,
    /// Highest epoch ever handed out for this submission, to any of its
    /// judgements: the per-submission epoch allocator. New epochs come only
    /// from `services::judging`, which bumps it in SQL, so every judgement
    /// gets a unique epoch even when a deferred rejudge runs beside the
    /// current one.
    #[sea_orm(default_value = 0)]
    pub max_judge_epoch: i32,

    /// When set by an admin, every operation produced for this submission is
    /// pinned to the named worker via the worker's private queue. Mirrored
    /// from the current judgement.
    #[sea_orm(nullable)]
    pub target_worker_id: Option<String>,

    /// System-error retries spent since the last rejudge; the budget is
    /// `max_system_error_retries` (see `services::judging::RetryBudget`).
    #[sea_orm(default_value = 0)]
    pub system_error_retries: i32,

    pub created_at: DateTimeUtc,
    pub judged_at: Option<DateTimeUtc>,
}

impl ActiveModelBehavior for ActiveModel {}
