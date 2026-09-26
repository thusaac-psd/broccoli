use common::{SubmissionStatus, Verdict};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// One row per attempted judging of a submission.
///
/// The first judgement is created at submit time and becomes
/// `is_current = true`. Each subsequent rejudge inserts a new row;
/// whether it becomes current immediately or has to be applied by an
/// admin is decided at rejudge time. The denormalized result columns
/// on `submission` (`score`, `verdict`, `time_used`, `memory_used`,
/// `compile_output`, `error_*`) are a cache of the current judgement
/// for cheap list reads.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "submission_judgement")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,

    pub submission_id: i32,
    /// 1-based per-submission ordinal. Holes are not allowed.
    pub version: i32,
    /// Exactly one current judgement per submission, enforced by the
    /// partial unique index `idx_submission_judgement_one_current`.
    #[sea_orm(default_value = false)]
    pub is_current: bool,
    /// Set to true when the worker pipeline reaches a terminal status
    /// (Judged, CompilationError, SystemError). Pending and in-progress
    /// judgements stay false.
    #[sea_orm(default_value = false)]
    pub is_finalized: bool,

    /// User who triggered this regrade. NULL on the initial submission's v1.
    pub triggered_by_user_id: Option<i32>,
    pub target_worker_id: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub note: Option<String>,

    pub status: SubmissionStatus,
    #[sea_orm(column_type = "Text", nullable)]
    pub verdict: Option<Verdict>,
    #[sea_orm(column_type = "Double", nullable)]
    pub score: Option<f64>,
    pub time_used: Option<i32>,
    pub memory_used: Option<i32>,
    #[sea_orm(column_type = "Text", nullable)]
    pub compile_output: Option<String>,
    pub error_code: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub error_message: Option<String>,

    /// Bumped each time the judgement is sent to the worker so stale
    /// workers can be rejected. Mirrors `submission.judge_epoch`.
    #[sea_orm(default_value = 0)]
    pub judge_epoch: i32,

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

    pub created_at: DateTimeUtc,
    pub finalized_at: Option<DateTimeUtc>,

    #[sea_orm(belongs_to, from = "submission_id", to = "id")]
    pub submission: HasOne<super::submission::Entity>,
    #[sea_orm(belongs_to, from = "triggered_by_user_id", to = "id")]
    pub triggered_by: Option<super::user::Entity>,
    #[sea_orm(has_many)]
    pub test_case_results: HasMany<super::test_case_result::Entity>,
}

impl ActiveModelBehavior for ActiveModel {}
