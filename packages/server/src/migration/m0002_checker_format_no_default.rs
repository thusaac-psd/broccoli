use sea_orm_migration::prelude::*;

/// `problem.checker_format` has no column default: every insert sets it, and
/// the one default lives in `broccoli_api_types::problem::DEFAULT_CHECKER_FORMAT`.
///
/// Rolling-safe: every build that writes `problem` already sets the column on
/// insert, so older replicas keep working against the migrated schema.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0002_checker_format_no_default"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE problem ALTER COLUMN checker_format DROP DEFAULT")
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE problem ALTER COLUMN checker_format SET DEFAULT 'exact'::text",
            )
            .await?;
        Ok(())
    }
}
