//! Single source of truth for "clear all judged output before re-dispatch".
//!
//! Every re-dispatch path - opening a new judgement for a rejudge, DLQ retry,
//! stuck retry or lease steal (`services/judgement_lifecycle.rs`), the in-place
//! steal and stuck recovery of a judgement (`dispatcher/steal.rs`,
//! `dlq/stuck/recovery.rs`) and the SystemError retry
//! (`services/submission_dispatch.rs`) - must NULL the same set of judged-output
//! columns when a submission, code run, or deferred judgement is re-queued for
//! judging; a column present in one list but not the other leaks stale
//! results into the re-judged row. The lists were previously hand-maintained
//! per call site. Add any new judged-output column HERE and every reset path
//! picks it up.

use sea_orm::sea_query::Expr;

/// Chainable on `Entity::update_many()`: NULLs every judged-output column
/// for the entity (verdict, compile output, error code/message, score,
/// time/memory used, and the judged/finalized timestamp).
pub trait ClearJudgementColumns: Sized {
    fn clear_judgement_columns(self) -> Self;
}

/// In-memory twin of [`ClearJudgementColumns`] for `Model` values that are
/// re-used as re-dispatch payloads after the DB update.
pub trait ClearJudgementFields {
    fn clear_judgement_fields(&mut self);
}

macro_rules! impl_judgement_reset {
    ($entity:ident, $ts_col:ident, $ts_field:ident) => {
        impl ClearJudgementColumns for sea_orm::UpdateMany<super::$entity::Entity> {
            fn clear_judgement_columns(self) -> Self {
                use super::$entity::Column;
                self.col_expr(Column::Verdict, Expr::value(None::<String>).into())
                    .col_expr(Column::CompileOutput, Expr::value(None::<String>).into())
                    .col_expr(Column::ErrorCode, Expr::value(None::<String>).into())
                    .col_expr(Column::ErrorMessage, Expr::value(None::<String>).into())
                    .col_expr(Column::Score, Expr::value(None::<f64>).into())
                    .col_expr(Column::TimeUsed, Expr::value(None::<i32>).into())
                    .col_expr(Column::MemoryUsed, Expr::value(None::<i32>).into())
                    .col_expr(
                        Column::$ts_col,
                        Expr::value(None::<chrono::DateTime<chrono::Utc>>).into(),
                    )
            }
        }

        impl ClearJudgementFields for super::$entity::Model {
            fn clear_judgement_fields(&mut self) {
                self.verdict = None;
                self.compile_output = None;
                self.error_code = None;
                self.error_message = None;
                self.score = None;
                self.time_used = None;
                self.memory_used = None;
                self.$ts_field = None;
            }
        }
    };
}

impl_judgement_reset!(submission, JudgedAt, judged_at);
impl_judgement_reset!(code_run, JudgedAt, judged_at);
impl_judgement_reset!(submission_judgement, FinalizedAt, finalized_at);
