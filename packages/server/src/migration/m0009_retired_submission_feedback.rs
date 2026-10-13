use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE submission_feedback DROP CONSTRAINT IF EXISTS submission_feedback_outcome_check;
             ALTER TABLE submission_feedback ADD CONSTRAINT submission_feedback_outcome_check
                 CHECK (outcome IN ('pending', 'retired', 'released', 'too_late'))",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE submission_feedback SET outcome = 'released' WHERE outcome = 'retired';
             ALTER TABLE submission_feedback DROP CONSTRAINT IF EXISTS submission_feedback_outcome_check;
             ALTER TABLE submission_feedback ADD CONSTRAINT submission_feedback_outcome_check
                 CHECK (outcome IN ('pending', 'released', 'too_late'))",
            )
            .await?;
        Ok(())
    }
}
