use sea_orm_migration::prelude::*;

/// Whether a problem's stored configs have issues
/// (`entity::problem::Model::config_issues`), and whether a judgement was
/// claimed while they did (`entity::submission_judgement::Model::config_issues`).
///
/// Rolling-safe: defaulted columns older replicas never read or write.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0008_config_issues"
    }
}

const UP: &str = r#"
ALTER TABLE public.problem ADD COLUMN config_issues boolean NOT NULL DEFAULT false;
ALTER TABLE public.submission_judgement ADD COLUMN config_issues boolean NOT NULL DEFAULT false;
"#;

const DOWN: &str = r#"
ALTER TABLE public.submission_judgement DROP COLUMN config_issues;
ALTER TABLE public.problem DROP COLUMN config_issues;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}
