use sea_orm_migration::prelude::*;

/// How often in a row a timer's delivery went unanswered: for the plugin's
/// own reason (`unanswered`: no instance free, between loads, an instance
/// that would not build) or for the platform's (`outages`: the host database
/// lost or saturated, the server draining); and when such a timer may be
/// delivered next (`retry_at`, backed off after each; `NULL` once scheduled
/// anew). A delivery the platform kept from answering always refunds its
/// attempt, however long the outage lasts; one the plugin left unanswered
/// refunds it only while `unanswered` is under its cap
/// (`dispatcher::plugin_timer`), so a plugin that never answers spends the
/// timer's budget and the timer is dropped loudly, as one that keeps failing
/// is.
///
/// Additive (release/docs/upgrade.md): defaulted and nullable columns
/// servers of the previous release keep working against.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0021_plugin_timer_backoff"
    }
}

const UP: &str = r#"
ALTER TABLE public.plugin_timer
    ADD COLUMN unanswered integer NOT NULL DEFAULT 0,
    ADD COLUMN outages integer NOT NULL DEFAULT 0,
    ADD COLUMN retry_at timestamp with time zone;
"#;

const DOWN: &str = r#"
ALTER TABLE public.plugin_timer
    DROP COLUMN unanswered, DROP COLUMN outages, DROP COLUMN retry_at;
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
