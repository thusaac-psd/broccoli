use sea_orm_migration::prelude::*;

/// The background hook events dispatched fleet-wide, one row per event id
/// (`hooks::dispatch_hooks_once`): a replica inserts the row before it runs
/// the event's hooks, so no other replica, and no later restart, runs them
/// again.
///
/// Additive (release/docs/upgrade.md): servers of the previous release keep
/// working against it, so a release that adds only migrations like it rolls
/// server by server.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0020_hook_event"
    }
}

const UP: &str = r#"
CREATE TABLE public.hook_event (
    event_id character varying NOT NULL,
    claimed_at timestamp with time zone NOT NULL DEFAULT now(),
    CONSTRAINT hook_event_pkey PRIMARY KEY (event_id)
);
"#;

const DOWN: &str = r#"
DROP TABLE public.hook_event;
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
