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
    /// True exactly when `status` is terminal (Judged, CompilationError,
    /// SystemError). A finalized judgement accepts no plugin writes.
    #[sea_orm(default_value = false)]
    pub is_finalized: bool,

    /// User who triggered this regrade. NULL on the initial submission's v1.
    pub triggered_by_user_id: Option<i32>,
    pub target_worker_id: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub note: Option<String>,

    #[sea_orm(indexed)]
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

    /// Fencing token of this judgement's current dispatch, allocated from
    /// `submission.max_judge_epoch` whenever the judgement is opened or
    /// requeued. Plugin writes carrying any other epoch are rejected.
    #[sea_orm(default_value = 0)]
    pub judge_epoch: i32,

    #[sea_orm(column_type = "String(StringLen::N(128))", nullable)]
    pub owner_server_id: Option<String>,
    pub lease_heartbeat_at: Option<DateTimeUtc>,
    /// When the current dispatch was claimed, by the database clock. Unlike
    /// `lease_heartbeat_at` it is never refreshed, so the stuck detector can
    /// bound a dispatch's age even while a live server keeps its lease.
    #[sea_orm(nullable)]
    pub leased_at: Option<DateTimeUtc>,
    /// Times this judgement was requeued after its owner was lost or it got
    /// stuck; the budget is `max_dispatch_retries`.
    #[sea_orm(default_value = 0)]
    pub dispatch_retries: i32,

    #[sea_orm(default_expr = "Expr::current_timestamp()")]
    pub created_at: DateTimeUtc,
    /// When the unit last became `Queued` (created, rejudged or requeued), by
    /// the database clock: what a queue backlog's age is measured from.
    #[sea_orm(default_expr = "Expr::current_timestamp()")]
    pub queued_at: DateTimeUtc,
    pub finalized_at: Option<DateTimeUtc>,

    #[sea_orm(belongs_to, from = "submission_id", to = "id")]
    pub submission: HasOne<super::submission::Entity>,
    #[sea_orm(belongs_to, from = "triggered_by_user_id", to = "id")]
    pub triggered_by: Option<super::user::Entity>,
    #[sea_orm(has_many)]
    pub test_case_results: HasMany<super::test_case_result::Entity>,
}

impl ActiveModelBehavior for ActiveModel {}
