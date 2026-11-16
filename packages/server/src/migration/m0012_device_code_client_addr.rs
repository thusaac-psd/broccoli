use sea_orm_migration::prelude::*;

/// The client address that requested each pending device code
/// (`entity::device_authorization::Model::client_addr`), so pending codes are
/// bounded per address rather than fleet-wide (`services::device_auth`).
///
/// Pending codes requested before it have no address, as a code from an
/// address the server cannot tell has; they expire within a code's lifetime.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0012_device_code_client_addr"
    }
}

const UP: &str = r#"
ALTER TABLE public.device_authorization ADD COLUMN client_addr character varying;
CREATE INDEX "idx-device_authorization-client_addr"
    ON public.device_authorization USING btree (client_addr);
"#;

const DOWN: &str = r#"
ALTER TABLE public.device_authorization DROP COLUMN client_addr;
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
