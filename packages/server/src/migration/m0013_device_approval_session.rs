use sea_orm_migration::prelude::*;

/// A device-code approval belongs to the sign-in session that approved it
/// (`entity::device_authorization::Model::approved_by`), and ends with it: a
/// logout, a password reset, a role change or the account's deletion ends
/// the session and deletes the approval before the CLI collects it.
///
/// Approvals recorded by user alone (`authorized_user_id`) cannot be bound to
/// a session, so they are deleted, and the column goes with them; their CLIs
/// see the code expire and start again.
///
/// Forward-only like every migration here.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0013_device_approval_session"
    }
}

const UP: &str = r#"
DELETE FROM public.device_authorization WHERE authorized_user_id IS NOT NULL;
ALTER TABLE public.device_authorization DROP COLUMN authorized_user_id;
ALTER TABLE public.device_authorization ADD COLUMN approved_by bigint;
ALTER TABLE public.device_authorization
    ADD CONSTRAINT "fk-device_authorization-approved_by" FOREIGN KEY (approved_by)
    REFERENCES public.auth_session(id) ON DELETE CASCADE;
CREATE INDEX "idx-device_authorization-approved_by"
    ON public.device_authorization USING btree (approved_by);
"#;

const DOWN: &str = r#"
ALTER TABLE public.device_authorization DROP COLUMN approved_by;
ALTER TABLE public.device_authorization ADD COLUMN authorized_user_id integer;
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
