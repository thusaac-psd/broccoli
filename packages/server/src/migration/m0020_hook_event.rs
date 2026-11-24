use sea_orm_migration::prelude::*;

/// The background hook events, one row per event id
/// (`hooks::dispatch_hooks_once`): the event as dispatched (topic, payload,
/// enablements), recorded before its hooks run, with the lease of the
/// dispatch running it (`claimed_at`, `attempts`) and when it completed. A
/// duplicate dispatch skips a recorded event; the re-scan
/// (`hooks::redeliver_once`) runs again every event not completed whose
/// lease is free or has lapsed, and deletes completed events past a horizon.
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
    topic character varying NOT NULL,
    payload jsonb NOT NULL,
    enabled_plugins jsonb,
    created_at timestamp with time zone NOT NULL DEFAULT now(),
    claimed_at timestamp with time zone,
    attempts integer NOT NULL DEFAULT 1,
    completed_at timestamp with time zone,
    CONSTRAINT hook_event_pkey PRIMARY KEY (event_id)
);
CREATE INDEX idx_hook_event_unfinished ON public.hook_event (created_at)
    WHERE completed_at IS NULL;
CREATE INDEX idx_hook_event_completed ON public.hook_event (completed_at)
    WHERE completed_at IS NOT NULL;
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
