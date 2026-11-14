use sea_orm_migration::prelude::*;

/// The fleet-wide runtime settings row (`entity::platform_setting`). A
/// database without the row has self-registration closed.
///
/// Rolling-safe: a new table that older replicas never read.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0004_platform_setting"
    }
}

const UP: &str = r#"
CREATE TABLE public.platform_setting (
    id smallint NOT NULL,
    registration_open boolean NOT NULL,
    CONSTRAINT platform_setting_pkey PRIMARY KEY (id)
);
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE public.platform_setting")
            .await?;
        Ok(())
    }
}
