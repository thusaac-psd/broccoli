use common::Verdict;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "test_case_result")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,

    pub submission_id: i32,
    /// Versioned-judgement FK. Nullable during the rolling migration; the
    /// `seed::backfill_submission_judgements` step populates it for every
    /// pre-existing row by attaching them to a synthetic v1 judgement.
    /// New rows written by plugins always set this.
    pub judgement_id: Option<i32>,
    #[sea_orm(default_value = 0)]
    pub judge_epoch: i32,
    pub test_case_id: Option<i32>,
    /// 0-based ordinal for custom run test cases. NULL for DB-backed test cases.
    pub run_index: Option<i32>,

    #[sea_orm(column_type = "Text")]
    pub verdict: Verdict,
    #[sea_orm(column_type = "Double")]
    pub score: f64,

    pub time_used: Option<i32>,
    pub memory_used: Option<i32>,

    #[sea_orm(column_type = "Text", nullable)]
    pub stdout: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub stderr: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub checker_output: Option<String>,

    #[sea_orm(belongs_to, from = "submission_id", to = "id")]
    pub submission: HasOne<super::submission::Entity>,
    #[sea_orm(belongs_to, from = "judgement_id", to = "id")]
    pub judgement: HasOne<super::submission_judgement::Entity>,
    #[sea_orm(belongs_to, from = "test_case_id", to = "id")]
    pub test_case: HasOne<super::test_case::Entity>,

    pub created_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}
