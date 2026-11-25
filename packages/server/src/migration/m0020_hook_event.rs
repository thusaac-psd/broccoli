use sea_orm_migration::prelude::*;

/// The background hook events, one row per event id
/// (`hooks::dispatch_hooks_once`): the event as dispatched (topic, payload,
/// enablements), recorded before its hooks run; the lease of the dispatch
/// running it (`claimed_at`, fenced by `claim_token`); how many attempts it
/// had, when the next may start (`next_attempt_at`, backed off after each
/// unfinished one), why the last did not finish and why each did not
/// (`history`, the dead letter's retry history); and when it completed or was
/// dead-lettered (`dead_at`). `hook_event_delivery` records, per hook plugin,
/// when the event was delivered to it (`delivered_at`), so a later attempt
/// calls only the others, and the last error its hooks answered with
/// (`last_error`, `failed_at`). A duplicate dispatch skips a recorded event;
/// the re-scan
/// (`hooks::redeliver_once`) runs again every event that is due, and
/// deletes settled events past a horizon.
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
    claim_token uuid,
    attempts integer NOT NULL DEFAULT 1,
    next_attempt_at timestamp with time zone NOT NULL DEFAULT now(),
    last_error text,
    history jsonb NOT NULL DEFAULT '[]'::jsonb,
    completed_at timestamp with time zone,
    dead_at timestamp with time zone,
    CONSTRAINT hook_event_pkey PRIMARY KEY (event_id)
);
CREATE INDEX idx_hook_event_due ON public.hook_event (next_attempt_at)
    WHERE completed_at IS NULL AND dead_at IS NULL;
CREATE INDEX idx_hook_event_settled ON public.hook_event ((COALESCE(completed_at, dead_at)))
    WHERE completed_at IS NOT NULL OR dead_at IS NOT NULL;
CREATE TABLE public.hook_event_delivery (
    event_id character varying NOT NULL,
    plugin_id character varying NOT NULL,
    delivered_at timestamp with time zone,
    last_error text,
    failed_at timestamp with time zone,
    CONSTRAINT hook_event_delivery_pkey PRIMARY KEY (event_id, plugin_id),
    CONSTRAINT hook_event_delivery_event_fkey FOREIGN KEY (event_id)
        REFERENCES public.hook_event (event_id) ON DELETE CASCADE
);
"#;

const DOWN: &str = r#"
DROP TABLE public.hook_event_delivery;
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
