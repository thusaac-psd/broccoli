use sea_orm_migration::prelude::*;

/// Adds `submission.max_judge_epoch`, the per-submission judge-epoch
/// allocator (see `services::judgement_lifecycle`), and backfills it to the
/// highest epoch the submission or any of its judgements already holds.
///
/// Idempotent: on a fresh DB the entity `sync()` has already created the
/// column, so `ADD COLUMN IF NOT EXISTS` is a no-op, and the backfill only
/// ever raises the value. The allocator also takes `GREATEST` with
/// `judge_epoch`, so a row the backfill has not reached yet still allocates
/// above its own epoch.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE IF EXISTS submission
                    ADD COLUMN IF NOT EXISTS max_judge_epoch INTEGER NOT NULL DEFAULT 0;
                UPDATE submission s
                SET max_judge_epoch = GREATEST(
                    s.judge_epoch,
                    (SELECT MAX(j.judge_epoch) FROM submission_judgement j
                     WHERE j.submission_id = s.id)
                )
                WHERE s.max_judge_epoch < GREATEST(
                    s.judge_epoch,
                    (SELECT MAX(j.judge_epoch) FROM submission_judgement j
                     WHERE j.submission_id = s.id)
                );
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"ALTER TABLE IF EXISTS submission DROP COLUMN IF EXISTS max_judge_epoch"#,
            )
            .await?;
        Ok(())
    }
}
