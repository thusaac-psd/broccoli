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

    /// The `attempt_id` of the `before_submission` event that admitted this
    /// submission. Hooks that reserve something per attempt look it up to see
    /// whether the attempt has committed.
    #[sea_orm(unique, nullable)]
    pub attempt_id: Option<Uuid>,

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
    pub contest: Option<super::contest::Entity>,
    #[sea_orm(has_many)]
    pub test_case_results: HasMany<super::test_case_result::Entity>,

    /// Fencing token of the current judgement's dispatch. A worker result or
    /// a dispatch failure carrying any other epoch is ignored.
    #[sea_orm(default_value = 0)]
    pub judge_epoch: i32,
    /// Highest epoch ever handed out for this submission, to itself or to
    /// any of its judgements. This is the per-submission epoch allocator:
    /// new epochs come only from
    /// `services::judgement_lifecycle::AllocateJudgeEpoch`, which takes
    /// `GREATEST(max_judge_epoch, judge_epoch) + 1`, so every judgement gets
    /// a unique epoch even when a deferred rejudge runs beside the current
    /// one and `judge_epoch` itself does not move.
    #[sea_orm(default_value = 0)]
    pub max_judge_epoch: i32,

    /// When set by an admin, every operation produced for this submission is
    /// pinned to the named worker via the worker's private queue.
    #[sea_orm(nullable)]
    pub target_worker_id: Option<String>,

    #[sea_orm(column_type = "String(StringLen::N(128))", nullable)]
    pub owner_server_id: Option<String>,
    pub lease_heartbeat_at: Option<DateTimeUtc>,
    /// Immutable dispatch anchor: stamped once when the row is (re-)dispatched
    /// and NEVER refreshed (unlike `lease_heartbeat_at`, which the lease fiber
    /// bumps every pass). Lets the stuck-job detector bound a job's in-flight
    /// dispatch age independent of a still-live server's heartbeat refresh.
    #[sea_orm(nullable)]
    pub leased_at: Option<DateTimeUtc>,
    /// Dispatch ATTEMPTS of the current judging attempt, despite the name:
    /// bumped on every claim, steal and re-dispatch, so it is 1 after a normal
    /// first dispatch and the number of retries is `retry_count - 1`.
    /// `max_dispatch_retries` / `max_stuck_retries` are budgets on this
    /// count. A rejudge or DLQ retry starts a new attempt and resets it to 0
    /// (`services::judgement_lifecycle::OpenJudgement::Rejudge`). (Column name
    /// kept for schema compatibility.)
    #[sea_orm(default_value = 0)]
    pub retry_count: i32,

    /// Stamped by the database clock on insert, so plugins compare it with
    /// `NOW()` on one clock.
    #[sea_orm(default_expr = "Expr::current_timestamp()")]
    pub created_at: DateTimeUtc,
    pub judged_at: Option<DateTimeUtc>,
}

impl ActiveModelBehavior for ActiveModel {}
